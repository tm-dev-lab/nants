//! Cliente HTTP para endpoints compatíveis com OpenAI (autenticação `Bearer`).
//!
//! Dois protocolos, escolhidos por [`WireApi`] da conexão:
//! - `chat`: `POST {base_url}/chat/completions`;
//! - `responses`: `POST {base_url}/responses`, sem estado (`store:false`). Os itens de saída
//!   (raciocínio, chamadas, mensagens) são preservados em [`ChatMessage::provider_items`] e
//!   reenviados na volta seguinte.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use super::LlmError;
use crate::config::{Connection, Effort, WireApi};
use crate::i18n::Lang;

mod bad_reason;
pub use bad_reason::{BadKind, BadReason, ProviderCode, THROTTLE_CODES};
mod cut;
use cut::StreamDiag;
pub use cut::{cut_reason, StreamCut, CUT_SNIPPET_BYTES, CUT_TOP_EVENTS};
mod image;
pub use image::{
    is_image_unreadable, is_image_unsupported, GeneratedImage, ImageBackground, ImageFormat, ImageOptions, ImageQuality,
    ImageSize, IMAGE_OUTPUT_TOKENS, IMAGE_UNREADABLE, IMAGE_UNSUPPORTED,
};
mod plan;
pub use plan::{Billing, PlanError, PlanFuture, PlanTokenError, PlanTokens, PLAN_FORBIDDEN_FIELDS, PLAN_UNSUPPORTED_TOOLS, PLAN_USAGE_URL};
mod pace;
pub use pace::{pace_notice, pace_notice_secs, parse_reset, PaceClock, Pacer, Quota, PACE_MAX_WAIT, PACE_NOTICE_AFTER, PACE_WINDOW};

/// Timeout padrão de cada requisição. No streaming vale como timeout de ociosidade (até os
/// cabeçalhos e entre pedaços do corpo), não como duração total (spec 080 T01).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// Teto da duração total de uma requisição em streaming. Uma resposta viva (que segue entregando
/// bytes) não é cortada por `DEFAULT_TIMEOUT`; só a ociosidade ou este teto encerram a leitura.
pub const STREAM_MAX_DURATION: Duration = Duration::from_secs(30 * 60);
/// Teto do corpo de resposta (sucesso ou erro); acima disso a leitura é abortada.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Motivo de `LlmError::Parse` quando a resposta excede o teto.
const TOO_LARGE: BadReason = BadReason::new(BadKind::TooLarge);
/// Limite de caracteres do trecho do corpo de erro guardado em `LlmError::Http`.
const SNIPPET_MAX_CHARS: usize = 300;
/// Máximo de novas tentativas de uma resposta ruim que vale repetir (spec 088).
pub const BAD_RESPONSE_RETRIES: usize = 2;
/// Esperas entre as novas tentativas de uma falha transitória do provedor (spec 079).
pub const RETRY_DELAYS: [Duration; 3] =
    [Duration::from_secs(2), Duration::from_secs(8), Duration::from_secs(30)];
/// Teto do `Retry-After` respeitado entre tentativas.
pub const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);
/// Esperas entre as novas tentativas quando o provedor está no limite (HTTP 429 ou `throttled`,
/// spec 089): ~4 min no total.
pub const THROTTLE_DELAYS: [Duration; 6] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(60),
    Duration::from_secs(60),
];
/// Teto do `Retry-After` respeitado quando o provedor está no limite.
pub const THROTTLE_RETRY_AFTER_CAP: Duration = Duration::from_secs(120);
/// Intervalo máximo entre avisos durante uma espera de limite (a tela não parece travada).
pub const THROTTLE_NOTICE_EVERY: Duration = Duration::from_secs(15);
/// Intervalo de consulta do sinal de cancelamento durante as esperas (corte em ≤ 1 s).
const CANCEL_POLL: Duration = Duration::from_millis(200);
/// Fragmento que identifica a rejeição do parâmetro `reasoning_effort`.
const REASONING_EFFORT_FIELD: &str = "reasoning_effort";

/// Papel de uma mensagem no chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// Chamada de ferramenta solicitada pelo modelo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Argumentos em JSON, como texto (conforme devolvido pelo provedor).
    pub arguments: String,
}

/// Definição de ferramenta enviada ao modelo.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema dos parâmetros.
    pub parameters: Value,
}

/// Mensagem do histórico de chat.
#[derive(Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Obrigatório para `Role::Tool`: id da chamada que esta mensagem responde.
    pub tool_call_id: Option<String>,
    /// Chamadas de ferramenta feitas pelo assistente.
    pub tool_calls: Vec<ToolCall>,
    /// Itens de saída brutos da Responses (raciocínio, `function_call`, `message`), reenviados
    /// exatamente na volta seguinte. Opacos: podem conter `encrypted_content`, por isso nunca
    /// aparecem em `Debug` (ver impl manual) e não são serializados.
    pub provider_items: Option<Vec<Value>>,
    /// Imagens anexadas (só `Role::User`). Serializadas como partes de imagem apenas quando não
    /// vazias; nunca aparecem em `Debug` (só quantidade e tamanho).
    pub images: Vec<ImageInput>,
}

/// Imagem enviada ao modelo (PNG/JPEG). Barata de clonar: os bytes ficam em `Arc`.
#[derive(Clone, PartialEq, Eq)]
pub struct ImageInput {
    /// Tipo MIME (`image/png`, `image/jpeg`).
    pub media_type: String,
    pub data: Arc<[u8]>,
}

impl ImageInput {
    pub fn new(media_type: impl Into<String>, data: impl Into<Arc<[u8]>>) -> Self {
        Self { media_type: media_type.into(), data: data.into() }
    }

    /// `data:<mime>;base64,<…>`.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, base64_encode(&self.data))
    }
}

/// `Debug` manual: só o tipo e o tamanho, nunca os bytes.
impl fmt::Debug for ImageInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ImageInput({}, {} bytes)", self.media_type, self.data.len())
    }
}

/// Base64 padrão (RFC 4648, com `=`).
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let sextets = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, s) in sextets.iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[*s as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl ChatMessage {
    /// Mensagem `user` com texto e imagens.
    pub fn with_images(content: impl Into<String>, images: Vec<ImageInput>) -> Self {
        Self { images, ..Self::text(Role::User, content) }
    }

    /// Resumo das imagens para `Debug`: quantidade e bytes, sem conteúdo.
    fn images_summary(&self) -> Option<String> {
        if self.images.is_empty() {
            return None;
        }
        let bytes: usize = self.images.iter().map(|i| i.data.len()).sum();
        Some(format!("{} imagem(ns), {bytes} bytes", self.images.len()))
    }

    /// Mensagem somente de texto, sem chamadas de ferramenta.
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
            provider_items: None,
            images: Vec::new(),
        }
    }
}

/// `Debug` manual: os itens brutos são opacos e podem conter `encrypted_content`.
impl fmt::Debug for ChatMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let items = self
            .provider_items
            .as_ref()
            .map(|items| format!("{} item(s) opacos", items.len()));
        f.debug_struct("ChatMessage")
            .field("role", &self.role)
            .field("content", &self.content)
            .field("tool_call_id", &self.tool_call_id)
            .field("tool_calls", &self.tool_calls)
            .field("provider_items", &items)
            .field("images", &self.images_summary())
            .finish()
    }
}

/// Requisição de chat completion.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    /// Quando `Some`, envia `reasoning_effort` (com fallback automático se rejeitado).
    pub effort: Option<Effort>,
    /// Quando `true`, pede saída JSON (`response_format: json_object`).
    pub response_json: bool,
    /// Quando `true` em `wire_api = responses`, acrescenta a ferramenta hospedada `web_search`
    /// (com fallback automático se o provedor a recusar). Ignorado em `chat`.
    pub web_search: bool,
    /// Spec 122 T05: `prompt_cache_key` do corpo em `wire_api = responses` (inclusive o plano),
    /// estável dentro de um histórico ([`crate::agents::usage::cache_key`]). Retirada uma vez se o
    /// provedor recusar o campo. Ignorada em `chat`.
    pub cache_key: Option<String>,
}

/// Fonte citada pela busca na web (anotação `url_citation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub url: String,
    pub title: String,
}

/// Contagem de tokens reportada pelo provedor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub prompt: u32,
    pub completion: u32,
    /// Tokens de entrada servidos do cache do provedor, quando informado.
    pub cached: Option<u32>,
    /// Tokens de raciocínio (já contidos em `completion`), quando informado.
    pub reasoning: Option<u32>,
}

impl Usage {
    /// Uso sem detalhes de cache/reasoning.
    pub fn new(prompt: u32, completion: u32) -> Self {
        Self { prompt, completion, cached: None, reasoning: None }
    }
}

/// Lê `u32` saturado de `v[a][b]`, quando presente.
fn nested_tokens(v: &Value, a: &str, b: &str) -> Option<u32> {
    v[a][b].as_u64().map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

/// Resposta de chat completion (primeira escolha).
#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    /// `true` quando a requisição só foi aceita após remover `reasoning_effort`.
    pub fallback_used: bool,
    /// Itens `web_search_call` da resposta (cobrados por chamada).
    pub web_search_calls: u32,
    /// Fontes `url_citation` do texto, sem duplicatas.
    pub citations: Vec<Citation>,
    /// `true` quando a busca na web foi recusada (HTTP 400) e a requisição refeita sem ela.
    pub web_search_rejected: bool,
    /// Como a chamada é cobrada (spec 116 T02): pelo plano ChatGPT, os tokens vão ao registro sem US$.
    pub billing: Billing,
}

/// Causa fixa de uma falha do provedor. Vocabulário estável exibido ao usuário; não carrega
/// corpo, URL, chave nem texto remoto, só o status HTTP ou o motivo fechado quando houver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailure {
    /// Status HTTP devolvido pelo provedor (100..=599), com o `error.code`/`error.type` saneado
    /// quando houver (só o caminho de imagem o preenche, spec 091); o `Display` continua só o status.
    Http(u16, Option<ProviderCode>),
    Timeout,
    ConnectFailed,
    /// Resposta ruim, com o motivo fechado (spec 088); o `Display` continua só o código fixo.
    BadResponse(BadReason),
    InvalidConfig,
    /// Falha do plano ChatGPT que pausa a execução (spec 116 T02).
    Plan(PlanError),
}

impl ProviderFailure {
    /// Motivo neutro de idioma (spec 102): o tipo estável (`BadKind::as_str`, ou `http_code` para o
    /// código saneado de um `Http`) e o código saneado do provedor, quando houver. A interface monta
    /// o rótulo pelo dicionário (`provider.kind.<kind>`).
    pub fn reason_kind(&self) -> Option<(&'static str, Option<String>)> {
        match self {
            ProviderFailure::BadResponse(reason) => Some((reason.kind.as_str(), reason.code.map(|c| c.as_str().to_string()))),
            ProviderFailure::Http(_, Some(code)) => Some(("http_code", Some(code.as_str().to_string()))),
            _ => None,
        }
    }

    /// Detalhe curto do motivo no idioma `lang`, pelo dicionário (spec 119): o de `BadResponse`
    /// (ex.: `incompleta: content_filter`) ou o código saneado de `Http` (ex.: `código:
    /// invalid_value`).
    pub fn detail_in(&self, lang: Lang) -> Option<String> {
        match self {
            ProviderFailure::BadResponse(reason) => Some(reason.detail_in(lang)),
            ProviderFailure::Http(_, Some(code)) => {
                Some(format!("{}: {}", crate::i18n::text(lang, &format!("{}http_code", bad_reason::LABEL_PREFIX)), code.as_str()))
            }
            _ => None,
        }
    }

    /// Detalhe do texto de reserva e do log técnico, em pt-BR ([`Self::detail_in`]). A tela monta o
    /// motivo pelos parâmetros neutros ([`Self::reason_kind`]), no idioma dela.
    pub fn detail(&self) -> Option<String> {
        self.detail_in(Lang::PtBr)
    }

    /// Código fixo seguido do detalhe entre parênteses, quando houver:
    /// `provider_bad_response (incompleta: content_filter)`, `provider_http_400 (código:
    /// invalid_value)`; os demais ficam só com o código.
    pub fn describe(&self) -> String {
        match self.detail() {
            Some(detail) => format!("{self} ({detail})"),
            None => self.to_string(),
        }
    }
}

/// Linha no stderr do servidor para a falha final de um papel (spec 088): papel, modelo e código
/// com detalhe. Nunca texto remoto: só o vocabulário fixo e o código saneado.
pub fn log_provider_failure(role: &str, model: &str, failure: &ProviderFailure) {
    eprintln!("nants: falha do provedor: papel={role} modelo={} motivo={}", log_safe(model), failure.describe());
}

/// Linha no stderr do servidor para a falha final de `generate_image` (spec 091), inclusive a
/// recusa de capacidade: papel, conexão, modelo e motivo. Só configuração local filtrada e o
/// vocabulário fixo com o código saneado.
pub fn log_image_failure(role: &str, connection: &str, model: &str, failure: &ProviderFailure) {
    eprintln!("{}", image_failure_line(role, connection, model, failure));
}

/// Texto de [`log_image_failure`].
pub fn image_failure_line(role: &str, connection: &str, model: &str, failure: &ProviderFailure) -> String {
    format!(
        "nants: imagem falhou papel={} conexão={} modelo={} motivo={}",
        log_safe(role),
        log_safe(connection),
        log_safe(model),
        failure.describe()
    )
}

/// Linha no stderr quando o freio de custo suspende a geração na execução (spec 094 T01): papel,
/// conexão, modelo e o número de falhas cobradas. Só configuração local filtrada.
pub fn log_image_suspended(role: &str, connection: &str, model: &str, charged: usize) {
    eprintln!("{}", image_suspended_line(role, connection, model, charged));
}

/// Texto de [`log_image_suspended`].
pub fn image_suspended_line(role: &str, connection: &str, model: &str, charged: usize) -> String {
    format!(
        "nants: imagem suspensa na execução papel={} conexão={} modelo={} falhas_cobradas={charged}",
        log_safe(role),
        log_safe(connection),
        log_safe(model)
    )
}

/// Modelo vem da configuração local; mesmo assim só passa `[A-Za-z0-9_.:/-]`, até 80 caracteres.
fn log_safe(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-'))
        .take(80)
        .collect()
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderFailure::Http(status, _) => write!(f, "provider_http_{status}"),
            ProviderFailure::Timeout => f.write_str("provider_timeout"),
            ProviderFailure::ConnectFailed => f.write_str("provider_connect_failed"),
            ProviderFailure::BadResponse(_) => f.write_str("provider_bad_response"),
            ProviderFailure::InvalidConfig => f.write_str("provider_invalid_config"),
            ProviderFailure::Plan(plan) => write!(f, "provider_{}", plan.as_str()),
        }
    }
}

/// Mapeia `LlmError` para a causa fixa. Único ponto de conversão: descarta corpo, URL e chave.
pub fn provider_failure(err: &LlmError) -> ProviderFailure {
    match err {
        LlmError::Http { status, code, .. } if (100..=599).contains(status) => {
            ProviderFailure::Http(*status, *code)
        }
        // Status fora da faixa HTTP: o próprio número vai como código do status não suportado.
        LlmError::Http { status, .. } => ProviderFailure::BadResponse(BadReason::with_code(
            BadKind::UnsupportedStatus,
            Some(&status.to_string()),
        )),
        LlmError::Parse(reason) => ProviderFailure::BadResponse(*reason),
        LlmError::Timeout => ProviderFailure::Timeout,
        LlmError::Transport(_) => ProviderFailure::ConnectFailed,
        LlmError::Config(_) => ProviderFailure::InvalidConfig,
        LlmError::Plan(plan) => ProviderFailure::Plan(*plan),
    }
}

/// Espera entre tentativas (injetável; padrão `tokio::time::sleep`, cancelável ao descartar o
/// futuro).
pub type RetrySleep = Arc<dyn Fn(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Espera padrão: `tokio::time::sleep`. Nos testes do crate a espera é zero (não dorme).
fn default_sleep() -> RetrySleep {
    if cfg!(test) {
        Arc::new(|_| Box::pin(async {}))
    } else {
        Arc::new(|d| Box::pin(tokio::time::sleep(d)))
    }
}

tokio::task_local! {
    /// Sinal de cancelamento do laço do agente em curso (spec 089): passa pelos `ChatBackend`
    /// intermediários (medição, progresso, imagens) sem mudar a assinatura do trait.
    static CANCEL_SCOPE: Option<Arc<AtomicBool>>;
}

/// Roda `fut` com o sinal de cancelamento `cancel` visível para todo [`LlmClient`] chamado dentro
/// dela (na mesma task): as esperas entre tentativas terminam em ≤ 1 s depois do pedido de parada.
pub async fn with_cancel_scope<F: Future>(cancel: Option<Arc<AtomicBool>>, fut: F) -> F::Output {
    CANCEL_SCOPE.scope(cancel, fut).await
}

/// Falha de uma tentativa, com o `Retry-After` do provedor quando houver.
struct Failed {
    err: LlmError,
    retry_after: Option<Duration>,
}

impl From<LlmError> for Failed {
    fn from(err: LlmError) -> Self {
        Self { err, retry_after: None }
    }
}

/// Falha transitória que vale repetir: transporte (conexão, DNS), timeout e HTTP 429/5xx de
/// sobrecarga. Recusas de configuração (400, 401, 403, 404) e respostas inválidas não se repetem.
fn is_transient(err: &LlmError) -> bool {
    match err {
        LlmError::Transport(_) | LlmError::Timeout => true,
        LlmError::Http { status, .. } => matches!(status, 429 | 500 | 502 | 503 | 504),
        LlmError::Parse(_) | LlmError::Config(_) | LlmError::Plan(_) => false,
    }
}

/// `Retry-After` da resposta: segundos inteiros ou data HTTP ([`parse_retry_after`]).
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| parse_retry_after(v, SystemTime::now()))
}

/// Valor do `Retry-After`: segundos inteiros ou data HTTP no formato IMF-fixdate
/// (`Sun, 06 Nov 1994 08:49:37 GMT`), convertida no tempo que falta até ela. Data já passada,
/// inválida ou fora de faixa = `None` (vale a espera do cronograma). O valor vem do provedor:
/// nenhuma conta pode entrar em pânico (o teto é aplicado por quem usa).
fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = UNIX_EPOCH.checked_add(Duration::from_secs(http_date_secs(value)?))?;
    at.duration_since(now).ok().filter(|d| !d.is_zero())
}

/// Segundos desde a época Unix de uma data IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`).
fn http_date_secs(value: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let parts: Vec<&str> = value.split_whitespace().collect();
    let [_, day, month, year, time, "GMT"] = parts.as_slice() else {
        return None;
    };
    let day: u64 = day.parse().ok().filter(|d| (1..=31).contains(d))?;
    let month = u64::try_from(MONTHS.iter().position(|m| m == month)? + 1).ok()?;
    // Ano com exatamente 4 dígitos (1970..=9999): as contas abaixo ficam longe de estourar.
    if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: u64 = year.parse().ok().filter(|y| *y >= 1970)?;
    let mut hms = time.split(':').map(|p| p.parse::<u64>().ok());
    let (h, m, sec) = (hms.next()??, hms.next()??, hms.next()??);
    if hms.next().is_some() || h > 23 || m > 59 || sec > 60 {
        return None;
    }
    // Dias desde a época (algoritmo `days_from_civil`, calendário gregoriano proléptico).
    let (y, mo) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * mo + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era.checked_mul(146_097)?.checked_add(doe)?.checked_sub(719_468)?;
    days.checked_mul(86_400)?.checked_add(h * 3600 + m * 60 + sec)
}

/// Segundos inteiros para exibição, arredondando para cima.
fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

/// Rótulo do limite no aviso: o código saneado do provedor ou `HTTP 429`.
fn throttle_label(err: &LlmError) -> String {
    match err {
        // Sem código, o nome estável do motivo (spec 119: nunca o rótulo em pt-BR).
        LlmError::Parse(reason) => reason.code.map_or_else(|| reason.kind.as_str().to_string(), |c| c.as_str().to_string()),
        _ => "HTTP 429".to_string(),
    }
}

/// Provedor no limite (spec 089): HTTP 429 ou resposta ruim `throttled`.
fn is_throttle(err: &LlmError) -> bool {
    match err {
        LlmError::Http { status, .. } => *status == 429,
        LlmError::Parse(reason) => reason.is_throttled(),
        _ => false,
    }
}

/// Cliente LLM para uma conexão compatível com OpenAI.
#[derive(Clone)]
pub struct LlmClient {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
    /// Timeout de ociosidade do streaming (o mesmo valor do timeout total do modo comum).
    timeout: Duration,
    max_response_bytes: usize,
    wire_api: WireApi,
    /// Esperas antes de cada nova tentativa (uma por tentativa extra).
    retry_delays: Vec<Duration>,
    /// Esperas antes de cada nova tentativa quando o provedor está no limite (spec 089).
    throttle_delays: Vec<Duration>,
    retry_sleep: RetrySleep,
    /// Sinal de cancelamento das esperas; sem ele, vale o do escopo ([`with_cancel_scope`]).
    cancel: Option<Arc<AtomicBool>>,
    /// Limitador de ritmo da conexão (spec 089 T02): só nos clientes de [`Self::from_connection`].
    pace: Option<PaceLink>,
    /// Modo plano ChatGPT (spec 116 T02): a fonte do access token da conexão.
    plan: Option<Arc<dyn PlanTokens>>,
    /// Último access token usado no modo plano, para redigir trechos de erro e de corte.
    plan_secret: Arc<std::sync::Mutex<String>>,
    /// Só nos testes (spec 120, teste real): `tool_choice` acrescentado ao corpo, para exigir a
    /// ferramenta. O produto não envia `tool_choice`.
    #[cfg(test)]
    test_tool_choice: Option<Value>,
    /// Só nos testes (spec 120, teste real): aplica o ajuste preparado
    /// [`plan::replay_without_ids`] ao corpo do modo plano.
    #[cfg(test)]
    test_plan_replay_fix: bool,
}

/// Limitador compartilhado e o id da conexão (a chave é completada pelo modelo do pedido).
#[derive(Clone)]
struct PaceLink {
    pacer: Arc<Pacer>,
    conn: String,
}

/// Custo fixo estimado de uma imagem anexada, em tokens (a imagem não é codificada para estimar).
const IMAGE_TOKENS: u64 = 1500;
/// Sobrecarga estimada de cada mensagem (papel, ids, estrutura), em caracteres.
const MESSAGE_OVERHEAD_CHARS: usize = 16;

/// Soma dos tamanhos dos textos (chaves e valores) de um JSON, sem serializá-lo.
fn text_len(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Array(items) => items.iter().fold(0usize, |n, i| n.saturating_add(text_len(i))),
        Value::Object(map) => map
            .iter()
            .fold(0usize, |n, (k, v)| n.saturating_add(k.len()).saturating_add(text_len(v))),
        Value::Number(_) | Value::Bool(_) | Value::Null => 4,
    }
}

/// Motivo da falha quando a parada chega durante a espera do limitador (o pedido não saiu). Não se
/// repete; o laço do agente vê a parada antes e a trata como cancelamento.
const PACE_STOPPED: &str = "espera de ritmo interrompida";

/// `Debug` manual: redige a chave de API.
impl fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"***")
            .finish_non_exhaustive()
    }
}

impl LlmClient {
    /// Cria o cliente para uma API OpenAI-compatível (autenticação `Bearer`).
    pub fn new(base_url: impl Into<String>, api_key: String) -> Result<Self, LlmError> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(LlmError::Config("base_url vazia".into()));
        }
        Ok(Self {
            base_url,
            api_key,
            http: build_http(DEFAULT_TIMEOUT)?,
            timeout: DEFAULT_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
            wire_api: WireApi::Chat,
            retry_delays: RETRY_DELAYS.to_vec(),
            throttle_delays: THROTTLE_DELAYS.to_vec(),
            retry_sleep: default_sleep(),
            cancel: None,
            pace: None,
            plan: None,
            plan_secret: Arc::new(std::sync::Mutex::new(String::new())),
            #[cfg(test)]
            test_tool_choice: None,
            #[cfg(test)]
            test_plan_replay_fix: false,
        })
    }

    /// Cria o cliente a partir de uma `Connection`, resolvendo `env:NOME` na chave e usando
    /// o protocolo `wire_api` da conexão. O ritmo segue o limitador do processo para o par
    /// (id da conexão, modelo) ([`Pacer::global`], spec 089 T02).
    ///
    /// A conexão pelo plano ChatGPT não tem chave: monta-se por [`Self::from_plan_connection`], com a
    /// fonte do access token; aqui ela é recusada como configuração inválida.
    pub fn from_connection(conn: &Connection) -> Result<Self, LlmError> {
        if conn.is_chatgpt_plan() {
            return Err(LlmError::Config("conexão pelo plano ChatGPT sem fonte de token".into()));
        }
        let api_key = conn
            .resolve_api_key()
            .map_err(|e| LlmError::Config(e.to_string()))?;
        let client = Self::new(conn.base_url.clone(), api_key)?.with_wire_api(conn.wire_api);
        Ok(Self { pace: Some(PaceLink { pacer: Pacer::global(), conn: conn.id.clone() }), ..client })
    }

    /// Cliente da conexão pelo plano ChatGPT (spec 116 T02): Responses no modo plano, com o access
    /// token de `tokens` e o ritmo do processo para o par (id da conexão, modelo).
    pub fn from_plan_connection(conn: &Connection, tokens: Arc<dyn PlanTokens>) -> Result<Self, LlmError> {
        let client = Self::new(conn.base_url.clone(), String::new())?.with_plan_tokens(tokens);
        Ok(Self { pace: Some(PaceLink { pacer: Pacer::global(), conn: conn.id.clone() }), ..client })
    }

    /// Liga o modo plano ChatGPT (spec 116 T02): Responses, `store:false` e `stream:true` sempre,
    /// sem os campos proibidos, com o access token de `tokens`.
    pub fn with_plan_tokens(mut self, tokens: Arc<dyn PlanTokens>) -> Self {
        self.plan = Some(tokens);
        self.wire_api = WireApi::Responses;
        self
    }

    /// Como as chamadas deste cliente são cobradas.
    pub fn billing(&self) -> Billing {
        if self.plan.is_some() {
            Billing::ChatgptPlan
        } else {
            Billing::Metered
        }
    }

    /// `true` no modo plano ChatGPT.
    pub fn is_plan(&self) -> bool {
        self.plan.is_some()
    }

    /// Desliga o limitador: nem espera nem alimenta o ritmo do par (teste de conexão).
    pub fn without_pacing(mut self) -> Self {
        self.pace = None;
        self
    }

    /// Usa o limitador `pacer` para a conexão `conn` (testes, com relógio injetável).
    #[cfg(test)]
    pub(crate) fn with_pacer(mut self, pacer: Arc<Pacer>, conn: impl Into<String>) -> Self {
        self.pace = Some(PaceLink { pacer, conn: conn.into() });
        self
    }

    /// `true` com o limitador ligado (testes).
    #[cfg(test)]
    pub(crate) fn is_paced(&self) -> bool {
        self.pace.is_some()
    }

    /// Define o protocolo usado por `chat` (padrão: `WireApi::Chat`).
    pub fn with_wire_api(mut self, wire_api: WireApi) -> Self {
        self.wire_api = wire_api;
        self
    }

    /// Define o timeout de cada requisição (padrão: 120s).
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, LlmError> {
        self.http = build_http(timeout)?;
        self.timeout = timeout;
        Ok(self)
    }

    /// Define o teto do corpo de resposta (padrão: `MAX_RESPONSE_BYTES`).
    pub fn with_max_response_bytes(mut self, max: usize) -> Self {
        self.max_response_bytes = max;
        self
    }

    /// Define as esperas entre tentativas (padrão: `RETRY_DELAYS`); vazio desliga a repetição.
    pub fn with_retry_delays(mut self, delays: Vec<Duration>) -> Self {
        self.retry_delays = delays;
        self
    }

    /// Define as esperas quando o provedor está no limite (padrão: `THROTTLE_DELAYS`); vazio
    /// desliga a repetição por limite.
    pub fn with_throttle_delays(mut self, delays: Vec<Duration>) -> Self {
        self.throttle_delays = delays;
        self
    }

    /// Define a função de espera entre tentativas (padrão: `tokio::time::sleep`).
    pub fn with_retry_sleep(mut self, sleep: RetrySleep) -> Self {
        self.retry_sleep = sleep;
        self
    }

    /// Liga um sinal de cancelamento: com ele `true`, a espera em curso termina em ≤ 1 s e não há
    /// nova tentativa (devolve o erro da falha que motivou a espera). Sem ele, vale o sinal de
    /// [`with_cancel_scope`], se houver.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Sinal de cancelamento efetivo: o do cliente ou o do escopo da task.
    fn cancel_signal(&self) -> Option<Arc<AtomicBool>> {
        self.cancel
            .clone()
            .or_else(|| CANCEL_SCOPE.try_with(Clone::clone).ok().flatten())
    }

    /// Espera `d` com a função de espera; `true` quando o cancelamento foi pedido (antes, durante
    /// ou ao fim da espera). O sinal é consultado a cada [`CANCEL_POLL`].
    async fn pause(&self, d: Duration, cancel: Option<&AtomicBool>) -> bool {
        let Some(flag) = cancel else {
            (self.retry_sleep)(d).await;
            return false;
        };
        let stopped = || flag.load(Ordering::SeqCst);
        if stopped() {
            return true;
        }
        let watch = async {
            while !stopped() {
                tokio::time::sleep(CANCEL_POLL).await;
            }
        };
        tokio::select! {
            () = (self.retry_sleep)(d) => stopped(),
            () = watch => true,
        }
    }

    /// Espera de limite (spec 089): avisa antes e renova o aviso, com o tempo restante, a cada
    /// [`THROTTLE_NOTICE_EVERY`]. `true` quando o cancelamento foi pedido.
    async fn throttle_pause(
        &self,
        label: &str,
        total: Duration,
        attempt: usize,
        on_notice: &impl Fn(String),
        cancel: Option<&AtomicBool>,
    ) -> bool {
        let notice = |left| Some(throttle_notice(label, left, attempt, self.throttle_delays.len()));
        self.noticed_pause(total, notice, on_notice, cancel).await
    }

    /// Espera `total` em passos de até [`THROTTLE_NOTICE_EVERY`], entregando antes de cada passo o
    /// aviso de `notice(tempo restante)`, quando houver. `true` quando o cancelamento foi pedido.
    async fn noticed_pause(
        &self,
        total: Duration,
        notice: impl Fn(Duration) -> Option<String>,
        on_notice: &impl Fn(String),
        cancel: Option<&AtomicBool>,
    ) -> bool {
        let mut left = total;
        loop {
            if let Some(text) = notice(left) {
                on_notice(text);
            }
            let step = left.min(THROTTLE_NOTICE_EVERY);
            if self.pause(step, cancel).await {
                return true;
            }
            left = left.saturating_sub(step);
            if left.is_zero() {
                return false;
            }
        }
    }

    /// Repete `once` em falha transitória ([`is_transient`]) até `retry_delays.len()` vezes,
    /// esperando o atraso da vez ou o `Retry-After` (teto `RETRY_AFTER_CAP`). `final_now()`
    /// verdadeiro impede nova tentativa (ex.: delta já entregue no streaming). Esgotadas as
    /// tentativas, devolve o erro original (o da primeira falha; se houve limite antes, o do limite).
    ///
    /// Resposta ruim que vale repetir ([`BadReason::is_retryable`], spec 088) segue outra regra:
    /// até [`BAD_RESPONSE_RETRIES`] novas tentativas com as esperas iniciais de `retry_delays`,
    /// mesmo com `final_now()` verdadeiro; `on_notice` recebe o aviso antes de cada uma.
    /// Esgotadas, devolve o erro da última falha.
    ///
    /// Provedor no limite (HTTP 429 ou `throttled`, spec 089) segue o cronograma `throttle_delays`,
    /// também com `final_now()` verdadeiro: o `Retry-After` (teto [`THROTTLE_RETRY_AFTER_CAP`])
    /// substitui a espera da vez, e `on_notice` recebe o aviso antes e durante a espera
    /// ([`Self::throttle_pause`]). Esgotado, devolve o erro da última falha.
    ///
    /// Com o cancelamento pedido ([`Self::cancel_signal`]), nenhuma espera continua nem há nova
    /// tentativa: devolve o erro da falha atual.
    async fn with_retry<T, F, Fut>(
        &self,
        mut once: F,
        final_now: impl Fn() -> bool,
        on_notice: impl Fn(String),
    ) -> Result<T, LlmError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, Failed>>,
    {
        let cancel = self.cancel_signal();
        let cancel = cancel.as_deref();
        let mut first: Option<LlmError> = None;
        let mut attempt = 0usize;
        let mut bad_retries = 0usize;
        let mut throttles = 0usize;
        loop {
            let failed = match once().await {
                Ok(v) => return Ok(v),
                Err(f) => f,
            };
            if is_throttle(&failed.err) {
                let Some(delay) = self.throttle_delays.get(throttles).copied() else {
                    return Err(failed.err);
                };
                throttles += 1;
                let wait = failed.retry_after.map_or(delay, |r| r.min(THROTTLE_RETRY_AFTER_CAP));
                let label = throttle_label(&failed.err);
                let cancelled = self.throttle_pause(&label, wait, throttles, &on_notice, cancel).await;
                if cancelled {
                    return Err(failed.err);
                }
                // O limite é a causa mais informativa: vale como erro final se os transitórios
                // seguintes se esgotarem.
                first = Some(failed.err);
                continue;
            }
            if let LlmError::Parse(reason) = &failed.err {
                let delay = self.retry_delays.get(bad_retries).copied();
                let Some(delay) = delay.filter(|_| reason.is_retryable() && bad_retries < BAD_RESPONSE_RETRIES) else {
                    return Err(failed.err);
                };
                bad_retries += 1;
                on_notice(retry_notice(reason, bad_retries));
                if self.pause(delay, cancel).await {
                    return Err(failed.err);
                }
                continue;
            }
            if !is_transient(&failed.err) || final_now() {
                return Err(failed.err);
            }
            let Some(delay) = self.retry_delays.get(attempt).copied() else {
                return Err(first.unwrap_or(failed.err));
            };
            let wait = failed.retry_after.map_or(delay, |r| r.min(RETRY_AFTER_CAP));
            if self.pause(wait, cancel).await {
                return Err(failed.err);
            }
            if first.is_none() {
                first = Some(failed.err);
            }
            attempt += 1;
        }
    }

    /// Uma tentativa no ritmo do par (spec 089 T02): reserva a vaga no limitador, espera por ela
    /// (avisando em `on_notice` se passar de [`PACE_NOTICE_AFTER`], com o aviso renovado; a parada
    /// corta em ≤ 1 s), envia `send` e realimenta o limitador: o `usage` real corrige o custo e um
    /// limite (429 ou `throttled`) ensina os tetos. Sem limitador, só `send`.
    async fn paced<Fut>(
        &self,
        req: &ChatRequest,
        on_notice: &impl Fn(String),
        send: Fut,
    ) -> Result<ChatResponse, Failed>
    where
        Fut: Future<Output = Result<ChatResponse, Failed>>,
    {
        self.paced_call(&req.model, || self.estimate_tokens(req), on_notice, send, |r: &ChatResponse| r.usage)
            .await
    }

    /// Núcleo de [`Self::paced`] para qualquer pedido do `model` (texto ou imagem, spec 090):
    /// `estimate` dá o custo previsto (só calculado com o limitador ligado) e `usage` lê o uso real
    /// do resultado.
    async fn paced_call<T, Fut>(
        &self,
        model: &str,
        estimate: impl FnOnce() -> u64,
        on_notice: &impl Fn(String),
        send: Fut,
        usage: impl Fn(&T) -> Option<Usage>,
    ) -> Result<T, Failed>
    where
        Fut: Future<Output = Result<T, Failed>>,
    {
        let Some(link) = &self.pace else {
            return send.await;
        };
        let (mut ticket, wait) = link.pacer.reserve(&link.conn, model, estimate());
        if !wait.is_zero() {
            let cancel = self.cancel_signal();
            let notice = |left| (wait > PACE_NOTICE_AFTER).then(|| pace_notice(left));
            if self.noticed_pause(wait, notice, on_notice, cancel.as_deref()).await {
                // O bilhete descartado devolve a vaga: o pedido não saiu.
                return Err(LlmError::Config(PACE_STOPPED.into()).into());
            }
        }
        ticket.sent();
        let out = send.await;
        match &out {
            Ok(resp) => {
                if let Some(u) = usage(resp) {
                    ticket.settle(u64::from(u.prompt).saturating_add(u64::from(u.completion)));
                }
            }
            Err(f) if is_throttle(&f.err) => link.pacer.on_limit(&link.conn, model),
            Err(_) => {}
        }
        out
    }

    /// Custo estimado do pedido em tokens (corrigido depois pelo `usage`): o texto que vai no corpo
    /// (conteúdo, chamadas e resultados de ferramentas, itens da Responses, schema das tools) / 4,
    /// mais [`IMAGE_TOKENS`] por imagem. Não serializa o corpo nem codifica as imagens: só soma
    /// tamanhos, barato o bastante para rodar em todo pedido (o aprendizado precisa do custo de cada
    /// um, mesmo antes de haver sinal).
    fn estimate_tokens(&self, req: &ChatRequest) -> u64 {
        let mut chars = 0usize;
        let mut images = 0usize;
        for msg in &req.messages {
            chars = chars.saturating_add(MESSAGE_OVERHEAD_CHARS).saturating_add(msg.content.len());
            for call in &msg.tool_calls {
                chars = chars.saturating_add(call.name.len()).saturating_add(call.arguments.len());
            }
            for item in msg.provider_items.iter().flatten() {
                chars = chars.saturating_add(text_len(item));
            }
            images = images.saturating_add(msg.images.len());
        }
        for tool in &req.tools {
            chars = chars
                .saturating_add(tool.name.len())
                .saturating_add(tool.description.len())
                .saturating_add(text_len(&tool.parameters));
        }
        let text = u64::try_from(chars).unwrap_or(u64::MAX) / 4;
        let images = u64::try_from(images).unwrap_or(u64::MAX).saturating_mul(IMAGE_TOKENS);
        text.saturating_add(images)
    }

    /// Reenvio interno do mesmo pedido (recusa de um campo opcional com HTTP 400): conta mais uma
    /// requisição na janela do par, sem espera e sem custo em tokens (o `usage` fica com a vaga da
    /// tentativa).
    fn note_resend(&self, model: &str) {
        if let Some(link) = &self.pace {
            link.pacer.record(&link.conn, model, 0);
        }
    }

    /// Cabeçalhos de cota de uma resposta do `model` para o limitador, quando ligado.
    fn observe_quota(&self, model: &str, headers: &reqwest::header::HeaderMap) {
        if let Some(link) = &self.pace {
            if let Some(quota) = Quota::from_headers(headers) {
                link.pacer.observe(&link.conn, model, &quota);
            }
        }
    }

    /// Envia a requisição pelo protocolo da conexão e devolve a primeira escolha. Falhas
    /// transitórias do provedor são repetidas ([`Self::with_retry`]); cada tentativa segue o ritmo
    /// do par ([`Self::paced`]).
    ///
    /// Sem consumidor de deltas, a espera do limitador aqui não é exibida (o reparo do JSON do
    /// Revisor, o advisor e o juiz de comandos usam este caminho): ela tem o teto de
    /// [`PACE_MAX_WAIT`] por requisição, que cabe nos prazos externos desses chamadores.
    ///
    /// No modo plano ChatGPT toda chamada é em streaming (spec 116): vai por [`Self::chat_stream`],
    /// sem consumidor de deltas.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        if self.plan.is_some() {
            return self.chat_stream(req, &|_| {}).await;
        }
        let quiet = |_: String| {};
        self.with_retry(
            || {
                self.paced(req, &quiet, async {
                    match self.wire_api {
                        WireApi::Chat => self.chat_completions(req).await,
                        WireApi::Responses => self.responses(req).await,
                    }
                })
            },
            || false,
            |_| {},
        )
        .await
    }

    async fn chat_completions(&self, req: &ChatRequest) -> Result<ChatResponse, Failed> {
        let (body, fallback_used, _) = self
            .post_with_effort_fallback(
                "/chat/completions",
                req,
                chat_body,
                chat_rejects_effort,
                false,
            )
            .await?;
        Ok(parse_response(&body, fallback_used)?)
    }

    async fn responses(&self, req: &ChatRequest) -> Result<ChatResponse, Failed> {
        let (body, fallback_used, web_rejected) = self
            .post_with_effort_fallback(
                "/responses",
                req,
                build_responses_body,
                responses_rejects_effort,
                true,
            )
            .await?;
        let mut resp = parse_responses(&body, fallback_used)?;
        resp.web_search_rejected = web_rejected;
        Ok(resp)
    }

    /// Envia `build(req, effort)`. Se a API recusar o esforço (HTTP 400 que `rejected` reconhece),
    /// repete sem o campo e devolve `fallback_used = true`. Com `supports_web`, um 400 que cita a
    /// busca na web repete sem a ferramenta (`web_rejected = true`). Cada recusa relaxa uma vez.
    async fn post_with_effort_fallback(
        &self,
        path: &str,
        req: &ChatRequest,
        build: fn(&ChatRequest, Option<Effort>) -> Result<Value, LlmError>,
        rejected: fn(&str) -> bool,
        supports_web: bool,
    ) -> Result<(String, bool, bool), Failed> {
        let mut effort = req.effort;
        let mut web = supports_web && req.web_search;
        // Spec 122 T05: a chave de cache recusada sai uma vez (só existe no corpo Responses).
        let mut cache = req.cache_key.is_some();
        loop {
            let cache_key = if cache { req.cache_key.clone() } else { None };
            let attempt = ChatRequest { web_search: web, cache_key, ..req.clone() };
            let builder = self
                .http
                .post(format!("{}{path}", self.base_url))
                .json(&build(&attempt, effort)?);
            let (status, body, after) = self.send_capped_with_headers(builder, Some(&req.model)).await?;
            if status.is_success() {
                let web_rejected = supports_web && req.web_search && !web;
                return Ok((body, req.effort.is_some() && effort.is_none(), web_rejected));
            }
            if status.as_u16() == 400 && cache && rejects_cache_key(&body) {
                cache = false;
            } else if status.as_u16() == 400 && web && rejects_web_search(&body) {
                web = false;
            } else if status.as_u16() == 400 && effort.is_some() && rejected(&body) {
                effort = None;
            } else {
                return Err(Failed { err: self.http_error(status.as_u16(), &body), retry_after: after });
            }
            self.note_resend(&req.model);
        }
    }

    /// `GET {base_url}{path}` com a mesma autenticação, timeout, sem redirects e teto
    /// de resposta do `chat`. Devolve o corpo como JSON.
    ///
    /// No modo plano, um 401 sem motivo descarta o access token e tenta uma vez com um renovado.
    pub async fn get_json(&self, path: &str) -> Result<serde_json::Value, LlmError> {
        let mut renewed = false;
        loop {
            let builder = self.http.get(format!("{}{path}", self.base_url));
            let (status, body) = self.send_capped(builder).await?;
            if !status.is_success() {
                if self.renew_after_401(status.as_u16(), &body, &mut renewed).await {
                    continue;
                }
                return Err(self.fail(status.as_u16(), &body).await);
            }
            return serde_json::from_str(&body).map_err(|_| LlmError::Parse(BadReason::new(BadKind::InvalidJson)));
        }
    }

    /// Valor do `Authorization: Bearer`: a chave da conexão ou, no modo plano, o access token da
    /// fonte (renovado por ela). Sem login utilizável, [`PlanError::NeedsLogin`]; autenticação
    /// indisponível é transitória.
    async fn bearer(&self) -> Result<String, LlmError> {
        let Some(tokens) = &self.plan else {
            return Ok(self.api_key.clone());
        };
        match tokens.access_token().await {
            Ok(token) => {
                *self.plan_secret.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = token.clone();
                Ok(token)
            }
            Err(PlanTokenError::NeedsLogin) => Err(LlmError::Plan(PlanError::NeedsLogin)),
            Err(PlanTokenError::Unavailable) => Err(LlmError::Transport("autenticação do plano indisponível".into())),
            Err(PlanTokenError::Failed) => Err(LlmError::Config("credenciais do plano indisponíveis".into())),
        }
    }

    /// Segredo a redigir dos trechos: a chave ou o último access token do plano.
    fn secret(&self) -> String {
        if self.plan.is_some() {
            self.plan_secret.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
        } else {
            self.api_key.clone()
        }
    }

    /// Modo plano: um 401 sem código do plano descarta o access token (a próxima chamada renova) e
    /// pede uma nova tentativa, uma vez por pedido. `true` quando vale repetir.
    async fn renew_after_401(&self, status: u16, body: &str, renewed: &mut bool) -> bool {
        let Some(tokens) = &self.plan else {
            return false;
        };
        if status != 401 || *renewed {
            return false;
        }
        let (code, _) = plan::body_error_parts(body);
        if code.as_deref().and_then(plan::classify).is_some() {
            return false;
        }
        *renewed = true;
        tokens.expire().await;
        true
    }

    /// Erro final de uma resposta HTTP de falha ([`Self::http_error`]); no modo plano, "precisa
    /// entrar de novo" também marca a conexão.
    async fn fail(&self, status: u16, body: &str) -> LlmError {
        let err = self.http_error(status, body);
        self.note_plan_error(&err).await;
        err
    }

    /// Modo plano: [`PlanError::NeedsLogin`] marca a conexão como "precisa entrar de novo".
    async fn note_plan_error(&self, err: &LlmError) {
        if let (Some(tokens), LlmError::Plan(PlanError::NeedsLogin)) = (&self.plan, err) {
            tokens.needs_login().await;
        }
    }

    /// Aplica a autenticação `Authorization: Bearer` em toda requisição.
    fn authorize(&self, builder: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
        builder.bearer_auth(token)
    }

    /// Envia, lê o corpo respeitando `max_response_bytes` e devolve status e texto.
    async fn send_capped(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<(reqwest::StatusCode, String), LlmError> {
        let (status, text, _) = self.send_capped_with_headers(builder, None).await?;
        Ok((status, text))
    }

    /// Como `send_capped`, devolvendo também o `Retry-After` da resposta. Com `model`, os
    /// cabeçalhos de cota alimentam o limitador ([`Self::observe_quota`]).
    async fn send_capped_with_headers(
        &self,
        builder: reqwest::RequestBuilder,
        model: Option<&str>,
    ) -> Result<(reqwest::StatusCode, String, Option<Duration>), LlmError> {
        let token = self.bearer().await?;
        let mut resp = self
            .authorize(builder, &token)
            .send()
            .await
            .map_err(transport_error)?;
        let status = resp.status();
        if let Some(model) = model {
            self.observe_quota(model, resp.headers());
        }
        let after = retry_after(resp.headers());
        let text = self.read_capped(&mut resp).await?;
        Ok((status, text, after))
    }

    /// Lê o corpo restante respeitando `max_response_bytes`.
    async fn read_capped(&self, resp: &mut reqwest::Response) -> Result<String, LlmError> {
        self.read_capped_from(resp, Vec::new()).await
    }

    /// Como [`Self::read_capped`], continuando a partir de `buf` (o começo já lido).
    async fn read_capped_from(&self, resp: &mut reqwest::Response, mut buf: Vec<u8>) -> Result<String, LlmError> {
        let too_large = || LlmError::Parse(TOO_LARGE);
        if resp
            .content_length()
            .is_some_and(|len| len > self.max_response_bytes as u64)
        {
            return Err(too_large());
        }
        while let Some(chunk) = self.next_chunk(resp).await? {
            if buf.len().saturating_add(chunk.len()) > self.max_response_bytes {
                return Err(too_large());
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Como `chat`, mas com streaming SSE: entrega deltas de reasoning/texto em `on_delta` e
    /// devolve o mesmo objeto final do modo não-streaming. Recusas do provedor a `stream`,
    /// `stream_options` ou `summary` (HTTP 400) são repetidas sem o campo (cada um uma vez);
    /// resposta JSON comum (não-SSE) é aceita. Falhas transitórias são repetidas como em `chat`,
    /// mas só enquanto nenhum delta foi entregue; depois do primeiro delta o erro sobe. Resposta
    /// ruim que vale repetir (spec 088) e provedor no limite (spec 089) repetem mesmo depois de
    /// deltas: antes (e, no limite, durante a espera), `on_delta` recebe [`StreamDelta::Retry`]
    /// com o aviso. A espera do limitador de ritmo ([`Self::paced`]) avisa pelo mesmo caminho.
    pub async fn chat_stream(
        &self,
        req: &ChatRequest,
        on_delta: &(dyn Fn(StreamDelta) + Send + Sync),
    ) -> Result<ChatResponse, LlmError> {
        let delivered = AtomicBool::new(false);
        // O tamanho recebido (`Received`) não conta como entrega: a nova tentativa de uma falha
        // transitória continua valendo enquanto nenhum texto chegou.
        let tracked = |d: StreamDelta| {
            if !matches!(d, StreamDelta::Received(_) | StreamDelta::Cut(_)) {
                delivered.store(true, Ordering::SeqCst);
            }
            on_delta(d);
        };
        let notify = |notice| on_delta(StreamDelta::Retry(notice));
        // Modo plano (spec 116 T02): o item retirado por `unsupported_capability` continua fora nas
        // novas tentativas da mesma chamada (ex.: depois de um 503).
        let removed: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        self.with_retry(
            || self.paced(req, &notify, self.chat_stream_once(req, &tracked, &removed)),
            || delivered.load(Ordering::SeqCst),
            notify,
        )
        .await
    }

    async fn chat_stream_once(
        &self,
        req: &ChatRequest,
        on_delta: &(dyn Fn(StreamDelta) + Send + Sync),
        removed_slot: &std::sync::Mutex<Option<String>>,
    ) -> Result<ChatResponse, Failed> {
        let plan = self.plan.is_some();
        let responses = plan || matches!(self.wire_api, WireApi::Responses);
        let path = if responses { "/responses" } else { "/chat/completions" };
        let mut opts = StreamOpts {
            stream: true,
            usage: true,
            summary: true,
            effort: req.effort,
            web: responses && req.web_search,
            plan,
            cache: responses && req.cache_key.is_some(),
        };
        let prompt_estimate = self.estimate_tokens(req);
        // Modo plano (spec 116 T02): item retirado por `unsupported_capability` (uma vez por chamada,
        // guardado entre as tentativas) e a renovação do token depois de um 401 sem motivo (uma vez
        // por tentativa).
        let mut removed: Option<String> = removed_slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let mut renewed = false;
        loop {
            let mut body = if responses {
                let cache_key = if opts.cache { req.cache_key.clone() } else { None };
                build_responses_body(&ChatRequest { web_search: opts.web, cache_key, ..req.clone() }, opts.effort)?
            } else {
                build_body(req, opts.effort)
            };
            opts.apply(&mut body, responses);
            if plan {
                plan::plan_body(&mut body);
                if let Some(param) = &removed {
                    plan::remove_param(&mut body, param);
                }
            }
            #[cfg(test)]
            if let Some(choice) = &self.test_tool_choice {
                body["tool_choice"] = choice.clone();
            }
            #[cfg(test)]
            if plan && self.test_plan_replay_fix {
                plan::replay_without_ids(&mut body);
            }
            let token = self.bearer().await.map_err(Failed::from)?;
            // Streaming (spec 080 T01): o timeout total do cliente cortaria uma resposta viva no
            // meio (com deltas já entregues, sem nova tentativa). Aqui ele é trocado pelo teto
            // `STREAM_MAX_DURATION`, e `self.timeout` vale como ociosidade: até os cabeçalhos e
            // entre pedaços do corpo.
            let builder = self
                .http
                .post(format!("{}{path}", self.base_url))
                .timeout(STREAM_MAX_DURATION.max(self.timeout))
                .json(&body);
            // Spec 114 T05: o diagnóstico conta o tempo desde o envio do pedido.
            let mut diag = StreamDiag::new(prompt_estimate);
            let mut resp = tokio::time::timeout(self.timeout, self.authorize(builder, &token).send())
                .await
                .map_err(|_| LlmError::Timeout)?
                .map_err(transport_error)?;
            let status = resp.status();
            self.observe_quota(&req.model, resp.headers());
            if !status.is_success() {
                let after = retry_after(resp.headers());
                let text = self.read_capped(&mut resp).await?;
                // `unsupported_capability`: só a retirada do item de `error.param`, uma vez; nunca
                // o mesmo corpo de novo nem outro relaxamento.
                if plan && status.as_u16() == 400 && is_unsupported_capability(&text) {
                    if let Some(param) = unsupported_param(&body, &text).filter(|_| removed.is_none()) {
                        *removed_slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(param.clone());
                        removed = Some(param);
                        self.note_resend(&req.model);
                        continue;
                    }
                    return Err(Failed { err: self.fail(400, &text).await, retry_after: after });
                }
                if self.renew_after_401(status.as_u16(), &text, &mut renewed).await {
                    continue;
                }
                if status.as_u16() == 400 && opts.relax(&text, responses) {
                    self.note_resend(&req.model);
                    continue;
                }
                return Err(Failed { err: self.fail(status.as_u16(), &text).await, retry_after: after });
            }
            let fallback_used = req.effort.is_some() && opts.effort.is_none();
            let web_rejected = responses && req.web_search && !opts.web;
            // Spec 119: o stream se reconhece pelo pedido e pelo corpo, não só pelo cabeçalho. A API
            // do plano devolve o SSE sem `Content-Type`; no plano o pedido é sempre `stream:true`,
            // então a resposta 200 é sempre lida como stream (só `response.completed` conta como
            // sucesso). Nos outros modos, sem o cabeçalho e com o pedido em stream, decide o começo
            // do corpo ([`looks_like_sse`]).
            let ctype = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_ascii_lowercase);
            let mut first: Vec<u8> = Vec::new();
            let is_sse = if plan || ctype.as_deref().is_some_and(|v| v.contains("text/event-stream")) {
                true
            } else if ctype.is_none() && opts.stream {
                first = self.read_until_content(&mut resp).await?;
                looks_like_sse(&first)
            } else {
                false
            };
            if !is_sse {
                let text = self.read_capped_from(&mut resp, first).await?;
                return if responses {
                    parse_responses(&text, fallback_used).map(|mut r| {
                        r.web_search_rejected = web_rejected;
                        r
                    })
                } else {
                    parse_response(&text, fallback_used)
                }
                .map_err(Failed::from);
            }
            let out = self.read_sse(resp, first, responses, fallback_used, on_delta, &mut diag).await;
            // Spec 114 T05: resposta cortada (tamanho, tempo, transmissão ou formato) vira um
            // diagnóstico com o custo, entregue antes do erro subir. Nunca a resposta inteira.
            if let Err(e) = &out {
                if let Some(mut cut) = diag.finish(e, &self.secret()) {
                    cut.billing = self.billing();
                    on_delta(StreamDelta::Cut(Arc::new(cut)));
                }
                self.note_plan_error(e).await;
            }
            return out
                .map(|mut r| {
                    r.web_search_rejected = web_rejected;
                    r.billing = self.billing();
                    r
                })
                .map_err(Failed::from);
        }
    }

    /// `first`: bytes do começo do corpo já lidos para reconhecer o stream (spec 119), tratados
    /// como o primeiro pedaço.
    async fn read_sse(
        &self,
        mut resp: reqwest::Response,
        first: Vec<u8>,
        responses: bool,
        fallback_used: bool,
        on_delta: &(dyn Fn(StreamDelta) + Send + Sync),
        diag: &mut StreamDiag,
    ) -> Result<ChatResponse, LlmError> {
        let mut parser = SseParser::default();
        let plan = self.plan.is_some();
        let mut acc = if responses {
            SseAcc::Responses(RespAcc::default())
        } else {
            SseAcc::Chat(ChatAcc::default())
        };
        let mut total = 0usize;
        let mut pending = (!first.is_empty()).then(|| axum::body::Bytes::from(first));
        'read: loop {
            let chunk = match pending.take() {
                Some(c) => c,
                None => match self.next_chunk(&mut resp).await? {
                    Some(c) => c,
                    None => break,
                },
            };
            total = total.saturating_add(chunk.len());
            diag.received(total);
            if total > self.max_response_bytes {
                return Err(LlmError::Parse(TOO_LARGE));
            }
            // Spec 114 T04: o tamanho da resposta em curso, para a Atividade (quem consome limita o ritmo).
            on_delta(StreamDelta::Received(u64::try_from(total).unwrap_or(u64::MAX)));
            for (event, data) in parser.feed(&chunk) {
                diag.observe(&event, &data);
                if plan {
                    if let Some(err) = plan::stream_failure(&event, &data) {
                        return Err(err);
                    }
                }
                if acc.event(&event, &data, fallback_used, on_delta)? {
                    break 'read;
                }
            }
        }
        if !acc.is_done() {
            if let Some((event, data)) = parser.finish() {
                diag.observe(&event, &data);
                if plan {
                    if let Some(err) = plan::stream_failure(&event, &data) {
                        return Err(err);
                    }
                }
                acc.event(&event, &data, fallback_used, on_delta)?;
            }
        }
        acc.finish(fallback_used)
    }

    /// Spec 119: lê o começo do corpo até o primeiro byte que não seja espaço (ou o fim), para
    /// reconhecer um stream sem `Content-Type`. Respeita o teto de bytes.
    async fn read_until_content(&self, resp: &mut reqwest::Response) -> Result<Vec<u8>, LlmError> {
        let mut buf: Vec<u8> = Vec::new();
        while buf.iter().all(u8::is_ascii_whitespace) {
            let Some(chunk) = self.next_chunk(resp).await? else {
                break;
            };
            if buf.len().saturating_add(chunk.len()) > self.max_response_bytes {
                return Err(LlmError::Parse(TOO_LARGE));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }

    /// Próximo pedaço do corpo, com timeout de ociosidade `self.timeout`: sem bytes nesse
    /// intervalo, `LlmError::Timeout` (transitório; repetido se nenhum delta foi entregue).
    /// `axum::body::Bytes` é o mesmo `bytes::Bytes` devolvido pelo reqwest.
    async fn next_chunk(
        &self,
        resp: &mut reqwest::Response,
    ) -> Result<Option<axum::body::Bytes>, LlmError> {
        tokio::time::timeout(self.timeout, resp.chunk())
            .await
            .map_err(|_| LlmError::Timeout)?
            .map_err(transport_error)
    }

    /// Erro de uma resposta HTTP de falha. No modo plano (spec 116 T02), os códigos do plano viram
    /// o erro próprio ([`plan::class_error`]) e um 401 é sempre "precisa entrar de novo".
    fn http_error(&self, status: u16, body: &str) -> LlmError {
        if self.plan.is_some() {
            let (code, _) = plan::body_error_parts(body);
            if let Some(err) = code.as_deref().and_then(|c| plan::class_error(&plan::classify(c)?, c)) {
                return err;
            }
            if status == 401 {
                return LlmError::Plan(PlanError::NeedsLogin);
            }
        }
        LlmError::Http {
            status,
            body_snippet: snippet(body, &self.secret()),
            code: None,
        }
    }
}

/// Classifica erro de transporte: timeout vira `LlmError::Timeout`; demais viram
/// `Transport`. A mensagem pode conter a URL completa, então quem expõe erros
/// ao cliente HTTP deve usar apenas a variante, não o texto.
fn transport_error(e: reqwest::Error) -> LlmError {
    if e.is_timeout() {
        LlmError::Timeout
    } else {
        LlmError::Transport(e.to_string())
    }
}

/// Spec 119: o começo do corpo é de um stream SSE (`event:`, `data:`, `id:`, `retry:` ou
/// comentário `:`), descontados os espaços iniciais e um BOM.
fn looks_like_sse(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(head);
    let start = head.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(head.len());
    let head = &head[start..];
    [b"event:".as_slice(), b"data:", b"id:", b"retry:", b":"].iter().any(|p| head.starts_with(p))
}

/// Modo plano: o corpo de erro traz o código `unsupported_capability`.
fn is_unsupported_capability(error_body: &str) -> bool {
    let (code, _) = plan::body_error_parts(error_body);
    code.as_deref().and_then(plan::classify) == Some(plan::PlanClass::Unsupported)
}

/// Modo plano: `param` de um 400 `unsupported_capability` cuja retirada muda o corpo `body` (o
/// mesmo corpo não se repete). `None` para outro erro ou item que não está no corpo.
fn unsupported_param(body: &Value, error_body: &str) -> Option<String> {
    if !is_unsupported_capability(error_body) {
        return None;
    }
    let (_, param) = plan::body_error_parts(error_body);
    let param = param?;
    let mut probe = body.clone();
    plan::remove_param(&mut probe, &param).then_some(param)
}

fn build_http(timeout: Duration) -> Result<reqwest::Client, LlmError> {
    reqwest::Client::builder()
        .timeout(timeout)
        // Não segue redirects: o cabeçalho de autenticação não pode vazar
        // para outro host indicado em `Location`.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| LlmError::Config(format!("cliente HTTP: {e}")))
}

/// Trecho do corpo de erro, com a chave redigida e limitado a `SNIPPET_MAX_CHARS`.
fn snippet(body: &str, secret: &str) -> String {
    let redacted = if secret.is_empty() {
        body.to_string()
    } else {
        body.replace(secret, "***")
    };
    redacted.chars().take(SNIPPET_MAX_CHARS).collect()
}

fn effort_str(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    }
}

fn build_body(req: &ChatRequest, effort: Option<Effort>) -> Value {
    let messages: Vec<Value> = req.messages.iter().map(message_json).collect();
    let mut body = json!({ "model": req.model, "messages": messages });
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req.tools.iter().map(tool_json).collect();
        body["tools"] = Value::Array(tools);
    }
    if let Some(effort) = effort {
        body["reasoning_effort"] = json!(effort_str(effort));
    }
    if req.response_json {
        body["response_format"] = json!({ "type": "json_object" });
    }
    body
}

fn message_json(msg: &ChatMessage) -> Value {
    let mut obj = json!({ "role": msg.role.as_str(), "content": msg.content });
    if msg.role == Role::User && !msg.images.is_empty() {
        let mut parts = vec![json!({ "type": "text", "text": msg.content })];
        parts.extend(
            msg.images
                .iter()
                .map(|i| json!({ "type": "image_url", "image_url": { "url": i.data_url() } })),
        );
        obj["content"] = Value::Array(parts);
    }
    if let Some(id) = &msg.tool_call_id {
        obj["tool_call_id"] = json!(id);
    }
    if !msg.tool_calls.is_empty() {
        let calls: Vec<Value> = msg
            .tool_calls
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "type": "function",
                    "function": { "name": c.name, "arguments": c.arguments },
                })
            })
            .collect();
        obj["tool_calls"] = Value::Array(calls);
    }
    obj
}

fn tool_json(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        },
    })
}

#[derive(Deserialize)]
struct WireResponse {
    /// Ausente quando o corpo traz só `error` (spec 088: vira `provider_error`).
    #[serde(default)]
    choices: Vec<WireChoice>,
    usage: Option<WireUsage>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    id: String,
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    name: String,
    /// Alguns servidores compatíveis enviam objeto JSON em vez de string.
    arguments: Value,
}

/// Normaliza `arguments` para texto: string é mantida; outro JSON é serializado.
fn arguments_text(value: Value) -> String {
    match value {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<Value>,
    #[serde(default)]
    completion_tokens_details: Option<Value>,
}

fn parse_response(body: &str, fallback_used: bool) -> Result<ChatResponse, LlmError> {
    let wire: WireResponse =
        serde_json::from_str(body).map_err(|_| LlmError::Parse(BadReason::new(BadKind::InvalidJson)))?;
    if let Some(error) = wire.error.as_ref().filter(|e| !e.is_null()) {
        return Err(LlmError::Parse(BadReason::provider_error(error)));
    }
    let choice = wire
        .choices
        .into_iter()
        .next()
        .ok_or(LlmError::Parse(BadReason::new(BadKind::Empty)))?;
    let tool_calls = choice
        .message
        .tool_calls
        .into_iter()
        .map(|c| ToolCall {
            id: c.id,
            name: c.function.name,
            arguments: arguments_text(c.function.arguments),
        })
        .collect();
    Ok(ChatResponse {
        message: ChatMessage {
            role: Role::Assistant,
            content: choice.message.content.unwrap_or_default(),
            tool_call_id: None,
            tool_calls,
            provider_items: None,
            images: Vec::new(),
        },
        finish_reason: choice.finish_reason,
        usage: wire.usage.map(|u| Usage {
            prompt: u.prompt_tokens,
            completion: u.completion_tokens,
            cached: u.prompt_tokens_details.as_ref().and_then(|d| d["cached_tokens"].as_u64()).map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
            reasoning: u.completion_tokens_details.as_ref().and_then(|d| d["reasoning_tokens"].as_u64()).map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
        }),
        fallback_used,
        web_search_calls: 0,
        citations: Vec::new(),
        web_search_rejected: false,
        billing: Default::default(),
    })
}

/// Mensagem de erro estática: resultado de ferramenta sem `tool_call_id`.
const TOOL_RESULT_WITHOUT_ID: &str = "resultado de ferramenta sem tool_call_id";

/// Corpo do Chat com o contrato de `build` (sempre válido).
fn chat_body(req: &ChatRequest, effort: Option<Effort>) -> Result<Value, LlmError> {
    Ok(build_body(req, effort))
}

/// Chat: a API recusou `reasoning_effort`?
fn chat_rejects_effort(body: &str) -> bool {
    body.contains(REASONING_EFFORT_FIELD)
}

/// Responses: a API recusou `reasoning.effort`?
fn responses_rejects_effort(body: &str) -> bool {
    body.contains("effort")
}

/// A API recusou a ferramenta `web_search` (ou o tipo de ferramenta)?
fn rejects_web_search(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("web_search")
        || (lower.contains("tool") && (lower.contains("not supported") || lower.contains("unsupported")))
            && !lower.contains("effort")
}

/// Corpo de `POST /responses`. `instructions` recebe as mensagens `system` (na ordem); as demais
/// viram `input`. Sem `previous_response_id`: a conversa inteira é reenviada (`store:false`).
fn build_responses_body(req: &ChatRequest, effort: Option<Effort>) -> Result<Value, LlmError> {
    let instructions: Vec<&str> = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect();
    let mut input: Vec<Value> = Vec::new();
    for msg in req.messages.iter().filter(|m| m.role != Role::System) {
        responses_input(msg, &mut input)?;
    }
    let mut body = json!({ "model": req.model, "input": input, "store": false });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions.join("\n\n"));
    }
    let mut tools: Vec<Value> = req.tools.iter().map(responses_tool_json).collect();
    if req.web_search {
        tools.push(json!({ "type": "web_search" }));
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if let Some(effort) = effort {
        body["reasoning"] = json!({ "effort": effort_str(effort) });
    }
    if req.response_json {
        body["text"] = json!({ "format": { "type": "json_object" } });
    }
    if let Some(key) = req.cache_key.as_deref().filter(|k| !k.is_empty()) {
        body[PROMPT_CACHE_KEY_FIELD] = json!(key);
    }
    Ok(body)
}

/// Campo da chave de cache no corpo Responses (spec 122 T05).
const PROMPT_CACHE_KEY_FIELD: &str = "prompt_cache_key";

/// O 400 recusou a `prompt_cache_key`?
fn rejects_cache_key(body: &str) -> bool {
    body.contains(PROMPT_CACHE_KEY_FIELD)
}

/// Acrescenta a `input` os itens correspondentes a uma mensagem (exceto `system`).
fn responses_input(msg: &ChatMessage, input: &mut Vec<Value>) -> Result<(), LlmError> {
    match msg.role {
        Role::System => {}
        Role::User if !msg.images.is_empty() => {
            let mut parts = vec![json!({ "type": "input_text", "text": msg.content })];
            parts.extend(
                msg.images
                    .iter()
                    .map(|i| json!({ "type": "input_image", "image_url": i.data_url() })),
            );
            input.push(json!({ "role": "user", "content": parts }));
        }
        Role::User => input.push(json!({ "role": "user", "content": msg.content })),
        Role::Tool => {
            let call_id = msg
                .tool_call_id
                .as_deref()
                .ok_or_else(|| LlmError::Config(TOOL_RESULT_WITHOUT_ID.into()))?;
            input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": msg.content,
            }));
        }
        Role::Assistant => {
            if let Some(items) = &msg.provider_items {
                // Já normalizados na leitura (`parse_responses`) e com `call_id` sincronizado.
                // `web_search_call` não é reenviável com `store:false` (a API rejeita o item).
                input.extend(
                    items
                        .iter()
                        .filter(|i| item_str(i, "type") != "web_search_call")
                        .cloned(),
                );
                return Ok(());
            }
            if msg.tool_calls.is_empty() || !msg.content.is_empty() {
                input.push(json!({ "role": "assistant", "content": msg.content }));
            }
            for call in &msg.tool_calls {
                input.push(json!({
                    "type": "function_call",
                    "call_id": call.id,
                    "name": call.name,
                    "arguments": call.arguments,
                }));
            }
        }
    }
    Ok(())
}

/// Alinha o `call_id` dos itens `function_call` de `provider_items` ao da `ToolCall` de mesma
/// posição. Necessário quando o id foi substituído localmente (ex.: `nants-synth-*`), para que o
/// `function_call_output` correspondente continue válido.
pub(crate) fn sync_provider_call_ids(msg: &mut ChatMessage) {
    let Some(items) = msg.provider_items.as_mut() else {
        return;
    };
    let mut calls = msg.tool_calls.iter();
    for item in items.iter_mut() {
        if item_str(item, "type") != "function_call" {
            continue;
        }
        let Some(call) = calls.next() else {
            break;
        };
        if let Some(obj) = item.as_object_mut() {
            obj.insert("call_id".into(), json!(call.id));
        }
    }
}

/// Texto dos `arguments` de um item `function_call`: ausente/null vira vazio.
fn responses_arguments(item: &Value) -> String {
    match &item["arguments"] {
        Value::Null => String::new(),
        other => arguments_text(other.clone()),
    }
}

fn responses_tool_json(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
        "strict": false,
    })
}

#[derive(Deserialize)]
struct WireResponsesReply {
    status: Option<String>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    incomplete_details: Option<Value>,
    #[serde(default)]
    output: Vec<Value>,
    usage: Option<WireResponsesUsage>,
}

#[derive(Deserialize)]
struct WireResponsesUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    input_tokens_details: Option<Value>,
    #[serde(default)]
    output_tokens_details: Option<Value>,
}

/// Campo textual de um item; ausente vira vazio.
fn item_str<'a>(item: &'a Value, key: &str) -> &'a str {
    item[key].as_str().unwrap_or_default()
}

/// Interpreta a resposta de `POST /responses`. Mensagens de erro são fixas: nunca ecoam o corpo.
fn parse_responses(body: &str, fallback_used: bool) -> Result<ChatResponse, LlmError> {
    let wire: WireResponsesReply = serde_json::from_str(body)
        .map_err(|_| LlmError::Parse(BadReason::new(BadKind::InvalidJson)))?;
    if let Some(error) = wire.error.as_ref().filter(|e| !e.is_null()) {
        return Err(LlmError::Parse(BadReason::provider_error(error)));
    }
    let truncated = match wire.status.as_deref() {
        Some("completed") => false,
        Some("incomplete") => {
            let reason = wire
                .incomplete_details
                .as_ref()
                .map(|d| item_str(d, "reason"));
            if reason == Some("max_output_tokens") {
                true
            } else {
                return Err(LlmError::Parse(BadReason::with_code(BadKind::Incomplete, reason)));
            }
        }
        status => return Err(LlmError::Parse(BadReason::with_code(BadKind::UnsupportedStatus, status))),
    };

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut web_search_calls = 0u32;
    let mut citations: Vec<Citation> = Vec::new();
    for item in &wire.output {
        match item_str(item, "type") {
            "web_search_call" => web_search_calls = web_search_calls.saturating_add(1),
            "message" => {
                for part in item["content"].as_array().into_iter().flatten() {
                    match item_str(part, "type") {
                        "output_text" => {
                            text.push_str(item_str(part, "text"));
                            for ann in part["annotations"].as_array().into_iter().flatten() {
                                if item_str(ann, "type") != "url_citation" {
                                    continue;
                                }
                                let c = Citation {
                                    url: item_str(ann, "url").to_string(),
                                    title: item_str(ann, "title").to_string(),
                                };
                                if !c.url.is_empty() && !citations.iter().any(|x| x.url == c.url) {
                                    citations.push(c);
                                }
                            }
                        }
                        "refusal" => text.push_str(item_str(part, "refusal")),
                        _ => {}
                    }
                }
            }
            "function_call" => tool_calls.push(ToolCall {
                id: item_str(item, "call_id").to_string(),
                name: item_str(item, "name").to_string(),
                arguments: responses_arguments(item),
            }),
            _ => {}
        }
    }
    if text.is_empty() && tool_calls.is_empty() {
        return Err(LlmError::Parse(BadReason::new(BadKind::Empty)));
    }

    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if truncated {
        "length"
    } else {
        "stop"
    };
    // Itens guardados com `arguments` já normalizado para texto (mesma regra das `ToolCall`).
    let mut items = wire.output;
    items.retain(|i| item_str(i, "type") != "web_search_call");
    for item in items.iter_mut() {
        if item_str(item, "type") == "function_call" {
            let args = responses_arguments(item);
            if let Some(obj) = item.as_object_mut() {
                obj.insert("arguments".into(), json!(args));
            }
        }
    }
    let usage = wire.usage.map(|u| Usage {
        prompt: u.input_tokens,
        completion: u.output_tokens,
        cached: u.input_tokens_details.as_ref().and_then(|d| d["cached_tokens"].as_u64()).map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
        reasoning: u.output_tokens_details.as_ref().and_then(|d| d["reasoning_tokens"].as_u64()).map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
    });
    Ok(ChatResponse {
        message: ChatMessage {
            role: Role::Assistant,
            content: text,
            tool_call_id: None,
            tool_calls,
            provider_items: Some(items),
            images: Vec::new(),
        },
        finish_reason: Some(finish_reason.into()),
        usage,
        fallback_used,
        web_search_calls,
        citations,
        web_search_rejected: false,
        billing: Default::default(),
    })
}

/// Trecho de conversa entregue durante o streaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamDelta {
    Reasoning(String),
    Text(String),
    /// A resposta em curso falhou e será repetida (spec 088): o que veio antes deve ser
    /// descartado. Texto fixo de [`retry_notice`], sem texto remoto.
    Retry(String),
    /// Bytes recebidos até agora na resposta em curso (spec 114 T04), a cada pedaço do corpo.
    /// Não é texto: não marca a resposta como entregue e quem exibe limita o ritmo.
    Received(u64),
    /// A resposta em curso foi cortada (spec 114 T05): o diagnóstico e o uso conhecido ou estimado,
    /// para o registro de uso. Não é texto nem marca a resposta como entregue.
    Cut(Arc<StreamCut>),
}

/// Aviso de espera de limite (spec 089):
/// `provedor no limite (<código ou HTTP 429>); aguardando N s (tentativa k/total)`.
pub fn throttle_notice(label: &str, wait: Duration, attempt: usize, total: usize) -> String {
    format!(
        "provedor no limite ({label}); aguardando {} s (tentativa {attempt}/{total})",
        ceil_secs(wait)
    )
}

/// Partes de um aviso de [`throttle_notice`] (spec 102: a atividade o publica com `code`):
/// rótulo, segundos, tentativa e total. Outro texto devolve `None`.
pub fn throttle_notice_parts(text: &str) -> Option<(String, u64, u64, u64)> {
    let rest = text.strip_prefix("provedor no limite (")?;
    let at = rest.rfind("); aguardando ")?;
    let label = &rest[..at];
    let rest = rest[at + "); aguardando ".len()..].strip_suffix(')')?;
    let (secs, count) = rest.split_once(" s (tentativa ")?;
    let (attempt, total) = count.split_once('/')?;
    Some((label.to_string(), secs.parse().ok()?, attempt.parse().ok()?, total.parse().ok()?))
}

/// Partes de um aviso de [`retry_notice`] (spec 119: a atividade o publica com `code`): o motivo
/// lido de volta do detalhe, a tentativa e o total. Outro texto, ou detalhe fora do vocabulário,
/// devolve `None`.
pub fn retry_notice_parts(text: &str) -> Option<(BadReason, u64, u64)> {
    let rest = text.strip_prefix(RETRY_NOTICE_HEAD)?;
    let at = rest.rfind(RETRY_NOTICE_MID)?;
    let reason = BadReason::parse_detail(&rest[..at])?;
    let (attempt, total) = rest[at + RETRY_NOTICE_MID.len()..].strip_suffix(')')?.split_once('/')?;
    Some((reason, attempt.parse().ok()?, total.parse().ok()?))
}

/// Parâmetros neutros (`provider`, `kind`, `provider_code`) de um texto de
/// [`ProviderFailure::describe`] já montado (`provider_bad_response (incompleta: x)`,
/// `provider_http_400 (código: y)`, `provider_timeout`), para a tela montar o motivo no idioma dela
/// (spec 119). Outro texto devolve `None`.
pub fn described_params(text: &str) -> Option<serde_json::Map<String, Value>> {
    let (provider, detail) = match text.split_once(" (") {
        Some((provider, rest)) => (provider, Some(rest.strip_suffix(')')?)),
        None => (text, None),
    };
    let valid = provider.strip_prefix("provider_").is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    });
    if !valid {
        return None;
    }
    let mut out = serde_json::Map::new();
    out.insert("provider".into(), Value::from(provider));
    let Some(detail) = detail else {
        return Some(out);
    };
    let (kind, code) = match BadReason::parse_detail(detail) {
        Some(reason) => (reason.kind.as_str(), reason.code),
        None => {
            let (label, code) = detail.split_once(": ")?;
            let http = format!("{}http_code", bad_reason::LABEL_PREFIX);
            let known = [Lang::PtBr, Lang::En].into_iter().any(|lang| crate::i18n::text(lang, &http) == label);
            ("http_code", Some(ProviderCode::parse(code).filter(|_| known)?))
        }
    };
    out.insert("kind".into(), Value::from(kind));
    if let Some(code) = code {
        out.insert("provider_code".into(), Value::from(code.as_str()));
    }
    Some(out)
}

/// Começo e meio do aviso de [`retry_notice`] (marcadores estruturais lidos por
/// [`retry_notice_parts`]; a tela mostra o aviso pelo `code`).
const RETRY_NOTICE_HEAD: &str = "resposta do provedor falhou (";
const RETRY_NOTICE_MID: &str = "); repetindo (";

/// Aviso de nova tentativa: `resposta do provedor falhou (<detalhe>); repetindo (n/2)`.
pub fn retry_notice(reason: &BadReason, attempt: usize) -> String {
    format!("{RETRY_NOTICE_HEAD}{}{RETRY_NOTICE_MID}{attempt}/{BAD_RESPONSE_RETRIES})", reason.detail())
}

/// Campos opcionais do pedido em streaming; cada um é removido no máximo uma vez.
struct StreamOpts {
    stream: bool,
    usage: bool,
    summary: bool,
    effort: Option<Effort>,
    /// Ferramenta `web_search` ainda incluída no pedido.
    web: bool,
    /// Modo plano ChatGPT (spec 116): `stream` nunca sai do pedido.
    plan: bool,
    /// `prompt_cache_key` ainda incluída no pedido (spec 122 T05).
    cache: bool,
}

impl StreamOpts {
    fn apply(&self, body: &mut Value, responses: bool) {
        if self.stream {
            body["stream"] = json!(true);
            if !responses && self.usage {
                body["stream_options"] = json!({ "include_usage": true });
            }
        }
        if responses && self.summary && self.effort.is_some() {
            body["reasoning"]["summary"] = json!("auto");
        }
    }

    /// Desliga o campo recusado por um 400; `false` quando nada resta a relaxar.
    fn relax(&mut self, body: &str, responses: bool) -> bool {
        if self.cache && rejects_cache_key(body) {
            self.cache = false;
            return true;
        }
        if self.web && rejects_web_search(body) {
            self.web = false;
            return true;
        }
        let effort_rejected = self.effort.is_some()
            && if responses {
                responses_rejects_effort(body)
            } else {
                chat_rejects_effort(body)
            };
        if responses && self.summary && self.effort.is_some() && body.contains("summary") {
            self.summary = false;
        } else if !responses && self.usage && body.contains("stream_options") {
            self.usage = false;
        } else if self.stream && !self.plan && body.contains("stream") {
            self.stream = false;
        } else if effort_rejected {
            self.effort = None;
        } else {
            return false;
        }
        true
    }
}

/// Parser SSE incremental (linhas `event:`/`data:`, CRLF, comentários, eventos multi-linha).
#[derive(Default)]
struct SseParser {
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl SseParser {
    fn feed(&mut self, bytes: &[u8]) -> Vec<(String, String)> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if let Some(ev) = self.take() {
                    out.push(ev);
                }
            } else if line.starts_with(':') {
                // comentário
            } else {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => self.event = value.to_string(),
                    "data" => self.data.push(value.to_string()),
                    _ => {}
                }
            }
        }
        out
    }

    fn take(&mut self) -> Option<(String, String)> {
        let event = std::mem::take(&mut self.event);
        if self.data.is_empty() {
            return None;
        }
        Some((event, std::mem::take(&mut self.data).join("\n")))
    }

    /// Evento pendente no fim do fluxo (sem linha em branco final).
    fn finish(&mut self) -> Option<(String, String)> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest).trim_end_matches('\r').to_string();
            if let Some(v) = line.strip_prefix("data:") {
                self.data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
            }
        }
        self.take()
    }
}

#[derive(Default)]
struct ChatAcc {
    content: String,
    /// index -> (id, name, arguments)
    calls: std::collections::BTreeMap<u64, (String, String, String)>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    done: bool,
}

/// Acumulador do stream Responses.
#[derive(Default)]
struct RespAcc {
    /// A resposta final, montada no `response.completed` (ou `response.incomplete`).
    done: Option<ChatResponse>,
    /// Spec 120: os itens completos de `response.output_item.done`, por `output_index`. A API do
    /// plano devolve `response.completed` com `output: []`; a saída só vem nesses eventos.
    items: std::collections::BTreeMap<u64, Value>,
}

impl RespAcc {
    /// Spec 120: o objeto `response` do evento final, com o `output` montado pelos itens recebidos
    /// quando o provedor o mandou vazio. Com `output` preenchido, ele continua sendo a fonte (sem
    /// juntar os itens, para não duplicar). `usage` e `status` vêm sempre do evento final.
    fn final_response(&mut self, event: &Value) -> Value {
        let mut response = event["response"].clone();
        let empty = response.get("output").is_none_or(|o| o.is_null() || o.as_array().is_some_and(Vec::is_empty));
        if empty && !self.items.is_empty() {
            if let Some(obj) = response.as_object_mut() {
                let items: Vec<Value> = std::mem::take(&mut self.items).into_values().collect();
                obj.insert("output".into(), Value::Array(items));
            }
        }
        response
    }
}

enum SseAcc {
    Responses(RespAcc),
    Chat(ChatAcc),
}

fn emit(on: &(dyn Fn(StreamDelta) + Send + Sync), delta: StreamDelta) {
    let empty = match &delta {
        StreamDelta::Reasoning(s) | StreamDelta::Text(s) | StreamDelta::Retry(s) => s.is_empty(),
        StreamDelta::Received(_) | StreamDelta::Cut(_) => false,
    };
    if !empty {
        on(delta);
    }
}

const NO_FINAL: BadReason = BadReason::new(BadKind::NoFinal);

impl SseAcc {
    fn is_done(&self) -> bool {
        match self {
            SseAcc::Responses(r) => r.done.is_some(),
            SseAcc::Chat(c) => c.done,
        }
    }

    /// Processa um evento; `true` quando o fluxo terminou.
    fn event(
        &mut self,
        name: &str,
        data: &str,
        fallback_used: bool,
        on: &(dyn Fn(StreamDelta) + Send + Sync),
    ) -> Result<bool, LlmError> {
        match self {
            SseAcc::Responses(acc) => {
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    return Ok(false);
                };
                let kind = v["type"].as_str().unwrap_or(name);
                let delta = v["delta"].as_str().unwrap_or_default().to_string();
                match kind {
                    "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                        emit(on, StreamDelta::Reasoning(delta));
                    }
                    "response.output_text.delta" => emit(on, StreamDelta::Text(delta)),
                    "response.output_item.done" => {
                        if v["item"].is_object() {
                            let next = acc.items.keys().next_back().map_or(0, |k| k.saturating_add(1));
                            let idx = v["output_index"].as_u64().unwrap_or(next);
                            acc.items.insert(idx, v["item"].clone());
                        }
                    }
                    "response.completed" | "response.incomplete" => {
                        let response = acc.final_response(&v);
                        acc.done = Some(parse_responses(&response.to_string(), fallback_used)?);
                        return Ok(true);
                    }
                    "response.failed" => {
                        return Err(LlmError::Parse(BadReason::provider_error(&v["response"]["error"])));
                    }
                    // `error` traz `code` no topo (ou, em alguns provedores, dentro de `error`).
                    "error" => {
                        let error = if v["error"].is_object() { &v["error"] } else { &v };
                        return Err(LlmError::Parse(BadReason::provider_error(error)));
                    }
                    _ => {}
                }
                Ok(false)
            }
            SseAcc::Chat(acc) => {
                if data.trim() == "[DONE]" {
                    acc.done = true;
                    return Ok(true);
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    return Ok(false);
                };
                if let Some(error) = v.get("error").filter(|e| !e.is_null()) {
                    return Err(LlmError::Parse(BadReason::provider_error(error)));
                }
                if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                    let n = |k: &str| u32::try_from(u[k].as_u64().unwrap_or(0)).unwrap_or(u32::MAX);
                    acc.usage = Some(Usage {
                        prompt: n("prompt_tokens"),
                        completion: n("completion_tokens"),
                        cached: nested_tokens(u, "prompt_tokens_details", "cached_tokens"),
                        reasoning: nested_tokens(u, "completion_tokens_details", "reasoning_tokens"),
                    });
                }
                let Some(choice) = v["choices"].as_array().and_then(|c| c.first()) else {
                    return Ok(false);
                };
                if let Some(f) = choice["finish_reason"].as_str() {
                    acc.finish_reason = Some(f.to_string());
                }
                let delta = &choice["delta"];
                for key in ["reasoning_content", "reasoning"] {
                    if let Some(t) = delta[key].as_str() {
                        emit(on, StreamDelta::Reasoning(t.to_string()));
                    }
                }
                if let Some(t) = delta["content"].as_str() {
                    acc.content.push_str(t);
                    emit(on, StreamDelta::Text(t.to_string()));
                }
                for tc in delta["tool_calls"].as_array().into_iter().flatten() {
                    let idx = tc["index"].as_u64().unwrap_or(0);
                    let entry = acc.calls.entry(idx).or_default();
                    if let Some(id) = tc["id"].as_str() {
                        entry.0 = id.to_string();
                    }
                    if let Some(n) = tc["function"]["name"].as_str() {
                        entry.1.push_str(n);
                    }
                    match &tc["function"]["arguments"] {
                        Value::String(a) => entry.2.push_str(a),
                        Value::Null => {}
                        other => entry.2.push_str(&other.to_string()),
                    }
                }
                Ok(false)
            }
        }
    }

    fn finish(self, fallback_used: bool) -> Result<ChatResponse, LlmError> {
        match self {
            SseAcc::Responses(RespAcc { done: Some(r), .. }) => Ok(r),
            SseAcc::Responses(RespAcc { done: None, .. }) => Err(LlmError::Parse(NO_FINAL)),
            SseAcc::Chat(acc) => {
                if !acc.done && acc.finish_reason.is_none() {
                    return Err(LlmError::Parse(NO_FINAL));
                }
                Ok(ChatResponse {
                    message: ChatMessage {
                        role: Role::Assistant,
                        content: acc.content,
                        tool_call_id: None,
                        tool_calls: acc
                            .calls
                            .into_values()
                            .map(|(id, name, arguments)| ToolCall { id, name, arguments })
                            .collect(),
                        provider_items: None,
                        images: Vec::new(),
                    },
                    finish_reason: acc.finish_reason,
                    usage: acc.usage,
                    fallback_used,
                    web_search_calls: 0,
                    citations: Vec::new(),
                    web_search_rejected: false,
                    billing: Default::default(),
                })
            }
        }
    }
}

/// Nos testes: só os deltas de conteúdo (sem o tamanho recebido, spec 114 T04).
#[cfg(test)]
fn not_size(d: StreamDelta) -> Option<StreamDelta> {
    (!matches!(d, StreamDelta::Received(_) | StreamDelta::Cut(_))).then_some(d)
}

#[cfg(test)]
pub(crate) mod stream_tests;

#[cfg(test)]
mod retry_tests;

#[cfg(test)]
mod pace_client_tests;

#[cfg(test)]
mod plan_client_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    pub(crate) const KEY: &str = "sk-test-SEGREDO-987";
    const OK_BODY: &str = r#"{"choices":[{"message":{"role":"assistant","content":"olá"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#;
    /// Responses: reasoning + function_call + message com `reasoning_tokens` em usage.
    pub(crate) const RESP_TOOL: &str = r#"{"id":"resp_1","object":"response","status":"completed","error":null,"output":[{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"ENC-SEGREDO-123"},{"type":"function_call","id":"fc_1","call_id":"call_abc","name":"read_file","arguments":"{\"path\":\"a.txt\"}","status":"completed"},{"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"vou ler","annotations":[]}]}],"usage":{"input_tokens":11,"output_tokens":5,"total_tokens":16,"output_tokens_details":{"reasoning_tokens":2}}}"#;
    /// Responses: somente texto.
    pub(crate) const RESP_TEXT: &str = r#"{"id":"resp_2","object":"response","status":"completed","error":null,"output":[{"type":"message","id":"msg_2","role":"assistant","status":"completed","content":[{"type":"output_text","text":"olá","annotations":[]}]}],"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10}}"#;
    /// Responses: truncada por `max_output_tokens` com texto parcial.
    const RESP_MAX: &str = r#"{"id":"resp_3","object":"response","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"error":null,"output":[{"type":"reasoning","id":"rs_2","summary":[],"encrypted_content":"ENC-SEGREDO-123"},{"type":"message","id":"msg_3","role":"assistant","status":"incomplete","content":[{"type":"output_text","text":"parci","annotations":[]}]}]}"#;

    fn request(effort: Option<Effort>) -> ChatRequest {
        ChatRequest {
            model: "o3-mini".into(),
            messages: vec![
                ChatMessage::text(Role::System, "seja breve"),
                ChatMessage::text(Role::User, "oi"),
            ],
            tools: Vec::new(),
            effort,
            response_json: false,
            web_search: false,
            cache_key: None,
        }
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn content_length(head: &str) -> usize {
        head.lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0)
    }

    /// Lê uma requisição HTTP completa (cabeçalhos + corpo por Content-Length).
    async fn read_request(sock: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            if let Some(pos) = find_header_end(&buf) {
                let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                let total = pos + 4 + content_length(&head);
                if buf.len() >= total {
                    return String::from_utf8_lossy(&buf[..total]).to_string();
                }
            }
            let n = sock.read(&mut tmp).await.expect("leitura do socket");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    /// Servidor HTTP local: responde, em ordem, cada par (status, corpo) e
    /// captura a requisição bruta recebida. Retorna a base URL e a captura.
    pub(crate) async fn spawn_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("endereço");
        let captured = Arc::new(Mutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        tokio::spawn(async move {
            for (status, body) in responses {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let raw = read_request(&mut sock).await;
                cap.lock().expect("lock").push(raw);
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}/v1"), captured)
    }

    pub(crate) fn head_of(raw: &str) -> String {
        raw.split("\r\n\r\n").next().unwrap_or("").to_string()
    }

    pub(crate) fn body_of(raw: &str) -> Value {
        let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");
        serde_json::from_str(body).expect("corpo JSON")
    }

    #[tokio::test]
    async fn openai_uses_bearer_auth() {
        let (base, cap) = spawn_server(vec![(200, OK_BODY)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let resp = client.chat(&request(None)).await.expect("chat");
        assert_eq!(resp.message.content, "olá");
        assert!(!resp.fallback_used);

        let raw = cap.lock().expect("lock")[0].clone();
        let head = head_of(&raw).to_ascii_lowercase();
        assert!(head.starts_with("post /v1/chat/completions http/1.1"), "{head}");
        assert!(head.contains(&format!("authorization: bearer {}", KEY.to_ascii_lowercase())));
        assert!(!head.contains("api-key:"));
        assert!(!head.contains("api-version"));
    }

    #[tokio::test]
    async fn sends_reasoning_effort_when_set() {
        let (base, cap) = spawn_server(vec![(200, OK_BODY)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let resp = client
            .chat(&request(Some(Effort::High)))
            .await
            .expect("chat");
        assert!(!resp.fallback_used);

        let raw = cap.lock().expect("lock")[0].clone();
        let body = body_of(&raw);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["model"], "o3-mini");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(cap.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn omits_reasoning_effort_when_none() {
        let (base, cap) = spawn_server(vec![(200, OK_BODY)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        client.chat(&request(None)).await.expect("chat");
        let raw = cap.lock().expect("lock")[0].clone();
        assert!(body_of(&raw).get("reasoning_effort").is_none());
    }

    #[tokio::test]
    async fn falls_back_once_without_reasoning_effort_on_400() {
        let rejection = r#"{"error":{"message":"Unsupported parameter: 'reasoning_effort' is not supported with this model."}}"#;
        let (base, cap) = spawn_server(vec![(400, rejection), (200, OK_BODY)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let resp = client
            .chat(&request(Some(Effort::Low)))
            .await
            .expect("chat com fallback");
        assert!(resp.fallback_used);
        assert_eq!(resp.message.content, "olá");

        let captured = cap.lock().expect("lock").clone();
        assert_eq!(captured.len(), 2, "deve haver exatamente duas requisições");
        assert_eq!(body_of(&captured[0])["reasoning_effort"], "low");
        assert!(body_of(&captured[1]).get("reasoning_effort").is_none());
    }

    #[tokio::test]
    async fn fallback_failure_returns_http_error_without_third_request() {
        let rejection = r#"{"error":"reasoning_effort not supported"}"#;
        let (base, cap) = spawn_server(vec![(400, rejection), (400, rejection)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let err = client
            .chat(&request(Some(Effort::Low)))
            .await
            .expect_err("deve falhar");
        assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
        assert_eq!(cap.lock().expect("lock").len(), 2);
    }

    #[tokio::test]
    async fn bad_request_without_effort_mention_is_not_retried() {
        let (base, cap) = spawn_server(vec![(400, r#"{"error":"model not found"}"#)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let err = client
            .chat(&request(Some(Effort::Medium)))
            .await
            .expect_err("deve falhar");
        match err {
            LlmError::Http { status, body_snippet, .. } => {
                assert_eq!(status, 400);
                assert!(body_snippet.contains("model not found"));
            }
            other => panic!("esperado Http, obtido {other:?}"),
        }
        assert_eq!(cap.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn parses_tool_calls_usage_and_finish_reason() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":11,"completion_tokens":5}}"#;
        let (base, _cap) = spawn_server(vec![(200, body)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let resp = client.chat(&request(None)).await.expect("chat");

        assert_eq!(resp.message.role, Role::Assistant);
        assert_eq!(resp.message.content, "");
        assert_eq!(resp.message.tool_calls.len(), 1);
        let call = &resp.message.tool_calls[0];
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments, r#"{"path":"a.txt"}"#);
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(resp.usage, Some(Usage::new(11, 5)));
    }

    #[test]
    fn usage_details_cached_and_reasoning_are_parsed() {
        let chat = r#"{"choices":[{"message":{"role":"assistant","content":"x"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":4},"completion_tokens_details":{"reasoning_tokens":2}}}"#;
        let u = parse_response(chat, false).expect("chat").usage.expect("usage");
        assert_eq!((u.prompt, u.completion, u.cached, u.reasoning), (10, 5, Some(4), Some(2)));
        let resp = r#"{"status":"completed","output":[{"type":"message","id":"m","role":"assistant","status":"completed","content":[{"type":"output_text","text":"x","annotations":[]}]}],"usage":{"input_tokens":9,"output_tokens":3,"input_tokens_details":{"cached_tokens":6},"output_tokens_details":{"reasoning_tokens":1}}}"#;
        let u = parse_responses(resp, false).expect("responses").usage.expect("usage");
        assert_eq!((u.prompt, u.completion, u.cached, u.reasoning), (9, 3, Some(6), Some(1)));
        let plain = parse_response(OK_BODY, false).expect("plain").usage.expect("usage");
        assert_eq!((plain.cached, plain.reasoning), (None, None));
    }

    #[tokio::test]
    async fn request_body_includes_tools_tool_results_and_json_mode() {
        let (base, cap) = spawn_server(vec![(200, OK_BODY)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let mut req = request(None);
        req.tools = vec![ToolSpec {
            name: "read_file".into(),
            description: "Lê um arquivo".into(),
            parameters: json!({"type": "object"}),
        }];
        req.response_json = true;
        req.messages.push(ChatMessage {
            role: Role::Tool,
            content: "conteúdo".into(),
            tool_call_id: Some("call_1".into()),
            tool_calls: Vec::new(),
            provider_items: None,
            images: Vec::new(),
        });
        client.chat(&req).await.expect("chat");

        let body = body_of(&cap.lock().expect("lock")[0].clone());
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
    }

    #[tokio::test]
    async fn http_500_is_http_error_without_secret() {
        let (base, _cap) = spawn_server(vec![(500, "erro interno, chave echo: sk-test-SEGREDO-987")]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let err = client.chat(&request(None)).await.expect_err("deve falhar");
        match err {
            LlmError::Http { status, body_snippet, .. } => {
                assert_eq!(status, 500);
                assert!(body_snippet.contains("erro interno"));
                assert!(!body_snippet.contains(KEY), "{body_snippet}");
            }
            other => panic!("esperado Http, obtido {other:?}"),
        }
    }

    /// Corpo remoto de erro com chave, URL e texto livre: nada disso pode vazar na causa fixa.
    const REMOTE_ERROR_BODY: &str = r#"{"error":{"message":"chave sk-test-SEGREDO-987 em https://api.exemplo.test/v1/chat/completions"}}"#;

    #[tokio::test]
    async fn provider_http_errors_map_to_fixed_codes_without_remote_text() {
        for status in [400u16, 401, 404, 429, 500] {
            let (base, _cap) = spawn_server(vec![(status, REMOTE_ERROR_BODY)]).await;
            let client = LlmClient::new(base, KEY.into()).expect("client");
            let err = client.chat(&request(None)).await.expect_err("deve falhar");
            let failure = provider_failure(&err);
            assert_eq!(failure, ProviderFailure::Http(status, None));
            let text = failure.to_string();
            assert_eq!(text, format!("provider_http_{status}"));
            assert!(!text.contains(KEY), "{text}");
            assert!(!text.contains("api.exemplo"), "{text}");
            assert!(!text.contains("chat/completions"), "{text}");
        }
    }

    #[test]
    fn provider_failure_maps_every_llm_error_kind() {
        assert_eq!(provider_failure(&LlmError::Timeout), ProviderFailure::Timeout);
        assert_eq!(
            provider_failure(&LlmError::Transport("sem rota".into())),
            ProviderFailure::ConnectFailed
        );
        let reason = BadReason::with_code(BadKind::Incomplete, Some("content_filter"));
        assert_eq!(provider_failure(&LlmError::Parse(reason)), ProviderFailure::BadResponse(reason));
        assert_eq!(
            provider_failure(&LlmError::Config("base_url vazia".into())),
            ProviderFailure::InvalidConfig
        );
        let http = |status| LlmError::Http {
            status,
            body_snippet: KEY.into(),
            code: None,
        };
        assert_eq!(provider_failure(&http(100)), ProviderFailure::Http(100, None));
        assert_eq!(provider_failure(&http(599)), ProviderFailure::Http(599, None));
        let odd = |code: &str| ProviderFailure::BadResponse(BadReason::with_code(BadKind::UnsupportedStatus, Some(code)));
        assert_eq!(provider_failure(&http(600)), odd("600"));
        assert_eq!(provider_failure(&http(99)), odd("99"));
    }

    #[test]
    fn provider_failure_display_is_the_fixed_vocabulary() {
        let texts = [
            ProviderFailure::Http(400, None),
            ProviderFailure::Timeout,
            ProviderFailure::ConnectFailed,
            ProviderFailure::BadResponse(BadReason::with_code(BadKind::Incomplete, Some("content_filter"))),
            ProviderFailure::InvalidConfig,
        ]
        .map(|f| f.to_string());
        assert_eq!(
            texts,
            [
                "provider_http_400",
                "provider_timeout",
                "provider_connect_failed",
                "provider_bad_response",
                "provider_invalid_config",
            ]
        );
    }

    #[test]
    fn snippet_is_limited_to_300_chars() {
        let long = "x".repeat(1000);
        assert_eq!(snippet(&long, KEY).chars().count(), SNIPPET_MAX_CHARS);
    }

    #[tokio::test]
    async fn invalid_json_is_parse_error() {
        // Spec 088: JSON inválido repete até 2 vezes; esgotadas, o erro sobe.
        let (base, cap) = spawn_server(vec![(200, "não é json"); 3]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let err = client.chat(&request(None)).await.expect_err("deve falhar");
        assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::InvalidJson), "{err:?}");
        assert_eq!(cap.lock().expect("lock").len(), 3);
    }

    #[tokio::test]
    async fn connection_unreachable_is_transport_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("endereço");
        drop(listener);
        let client = LlmClient::new(format!("http://{addr}/v1"), KEY.into())
            .expect("client");
        let err = client.chat(&request(None)).await.expect_err("deve falhar");
        assert!(matches!(err, LlmError::Transport(_)), "{err:?}");
    }

    #[tokio::test]
    async fn redirect_is_not_followed_and_key_is_not_sent() {
        let listener_a = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr_a = listener_a.local_addr().expect("endereço");
        let listener_b = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr_b = listener_b.local_addr().expect("endereço");
        let location = format!("http://{addr_b}/v1/chat/completions");

        tokio::spawn(async move {
            let Ok((mut sock, _)) = listener_a.accept().await else {
                return;
            };
            let _ = read_request(&mut sock).await;
            let resp = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
        // Servidor B: aceita e responde 200 para que um cliente que siga o
        // redirect tenha sucesso; o teste detecta a conexão pelo accept.
        let hit_b = tokio::spawn(async move {
            match tokio::time::timeout(Duration::from_millis(500), listener_b.accept()).await {
                Ok(Ok((mut sock, _))) => {
                    let _ = read_request(&mut sock).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{OK_BODY}",
                        OK_BODY.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    true
                }
                _ => false,
            }
        });

        let client = LlmClient::new(format!("http://{addr_a}/v1"), KEY.into())
        .expect("client")
        .with_timeout(Duration::from_secs(2))
        .expect("timeout");
        let err = client.chat(&request(None)).await.expect_err("deve falhar");
        assert!(!hit_b.await.expect("join"), "redirect não deve ser seguido");
        match &err {
            LlmError::Http { status, .. } => assert_eq!(*status, 307),
            other => panic!("esperado Http 307, obtido {other:?}"),
        }
        assert!(!format!("{err:?}").contains(KEY), "{err:?}");
    }

    #[tokio::test]
    async fn tool_arguments_as_json_object_are_serialized() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":{"path":"a.txt"}}}]},"finish_reason":"tool_calls"}]}"#;
        let (base, _cap) = spawn_server(vec![(200, body)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let resp = client.chat(&request(None)).await.expect("chat");
        let call = &resp.message.tool_calls[0];
        assert_eq!(call.name, "read_file");
        assert_eq!(call.arguments, r#"{"path":"a.txt"}"#);
    }

    #[tokio::test]
    async fn success_body_over_limit_is_parse_error() {
        let big: &'static str = Box::leak("x".repeat(4096).into_boxed_str());
        let (base, _cap) = spawn_server(vec![(200, big)]).await;
        let client = LlmClient::new(base, KEY.into())
            .expect("client")
            .with_max_response_bytes(1024);
        let err = client.chat(&request(None)).await.expect_err("deve falhar");
        assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::TooLarge), "{err:?}");
    }

    #[tokio::test]
    async fn error_body_over_limit_is_parse_error() {
        let big: &'static str = Box::leak("y".repeat(4096).into_boxed_str());
        let (base, _cap) = spawn_server(vec![(500, big)]).await;
        let client = LlmClient::new(base, KEY.into())
            .expect("client")
            .with_max_response_bytes(1024);
        let err = client.chat(&request(None)).await.expect_err("deve falhar");
        assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::TooLarge), "{err:?}");
    }

    #[tokio::test]
    async fn chunked_body_without_content_length_over_limit_is_parse_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("endereço");
        tokio::spawn(async move {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let _ = read_request(&mut sock).await;
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .await;
            for _ in 0..8 {
                if sock.write_all(&[b'z'; 512]).await.is_err() {
                    break;
                }
            }
            let _ = sock.shutdown().await;
        });
        let client = LlmClient::new(format!("http://{addr}/v1"), KEY.into())
            .expect("client")
            .with_max_response_bytes(1024);
        let err = client.chat(&request(None)).await.expect_err("deve falhar");
        assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::TooLarge), "{err:?}");
    }

    #[test]
    fn debug_does_not_leak_key() {
        let client = LlmClient::new("http://localhost/v1", KEY.into())
            .expect("client");
        let dbg = format!("{client:?}");
        assert!(!dbg.contains(KEY), "{dbg}");
        assert!(dbg.contains("***"));
    }

    #[test]
    fn from_connection_resolves_env_key() {
        let var = "NANTS_T01_LLM_KEY_PRESENT";
        std::env::set_var(var, KEY);
        let conn = Connection {
            id: "c".into(),
            name: "c".into(),
            base_url: "http://localhost/v1".into(),
            api_key: format!("env:{var}"),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
        };
        let client = LlmClient::from_connection(&conn).expect("client");
        std::env::remove_var(var);
        assert!(!format!("{client:?}").contains(KEY));
        assert_eq!(client.api_key, KEY);
    }

    #[test]
    fn from_connection_missing_env_is_config_error() {
        let var = "NANTS_T01_LLM_KEY_ABSENT";
        std::env::remove_var(var);
        let conn = Connection {
            id: "c".into(),
            name: "c".into(),
            base_url: "http://localhost/v1".into(),
            api_key: format!("env:{var}"),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
        };
        let err = LlmClient::from_connection(&conn).expect_err("deve falhar");
        assert!(matches!(err, LlmError::Config(_)), "{err:?}");
    }

    #[tokio::test]
    async fn get_json_uses_bearer_and_parses_body() {
        let body = r#"{"data":[{"id":"gpt-4o"}]}"#;
        let (base, cap) = spawn_server(vec![(200, body)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let value = client.get_json("/models").await.expect("get_json");
        assert_eq!(value["data"][0]["id"], "gpt-4o");

        let raw = cap.lock().expect("lock")[0].clone();
        let head = head_of(&raw).to_ascii_lowercase();
        assert!(head.starts_with("get /v1/models http/1.1"), "{head}");
        assert!(head.contains(&format!("authorization: bearer {}", KEY.to_ascii_lowercase())));
    }

    #[tokio::test]
    async fn get_json_http_error_keeps_status() {
        let (base, _cap) = spawn_server(vec![(401, r#"{"error":"bad key"}"#)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let err = client.get_json("/models").await.expect_err("deve falhar");
        assert!(matches!(err, LlmError::Http { status: 401, .. }), "{err:?}");
    }

    #[tokio::test]
    async fn get_json_invalid_json_is_parse_error() {
        let (base, _cap) = spawn_server(vec![(200, "não é json")]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        let err = client.get_json("/models").await.expect_err("deve falhar");
        assert!(matches!(err, LlmError::Parse(_)), "{err:?}");
    }

    #[tokio::test]
    async fn get_json_body_over_limit_is_parse_error() {
        let big: &'static str = Box::leak("x".repeat(4096).into_boxed_str());
        let (base, _cap) = spawn_server(vec![(200, big)]).await;
        let client = LlmClient::new(base, KEY.into())
            .expect("client")
            .with_max_response_bytes(1024);
        let err = client.get_json("/models").await.expect_err("deve falhar");
        assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::TooLarge), "{err:?}");
    }

    #[tokio::test]
    async fn get_json_silent_server_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("endereço");
        tokio::spawn(async move {
            let Ok((_sock, _)) = listener.accept().await else {
                return;
            };
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let client = LlmClient::new(format!("http://{addr}/v1"), KEY.into())
            .expect("client")
            .with_timeout(Duration::from_millis(300))
            .expect("timeout");
        let err = client.get_json("/models").await.expect_err("deve falhar");
        assert!(matches!(err, LlmError::Timeout), "{err:?}");
    }

    // ---- Backend Responses (stateless) ----

    fn read_file_tool() -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Lê um arquivo".into(),
            parameters: json!({"type": "object"}),
        }
    }

    fn responses_client(base: String) -> LlmClient {
        LlmClient::new(base, KEY.into())
            .expect("client")
            .with_wire_api(WireApi::Responses)
    }

    fn responses_request() -> ChatRequest {
        let mut req = request(Some(Effort::High));
        req.tools = vec![read_file_tool()];
        req
    }

    fn tool_result(id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(id.into()),
            tool_calls: Vec::new(),
            provider_items: None,
            images: Vec::new(),
        }
    }

    #[tokio::test]
    async fn responses_request_uses_responses_endpoint_and_exact_body() {
        let (base, cap) = spawn_server(vec![(200, RESP_TEXT)]).await;
        responses_client(base)
            .chat(&responses_request())
            .await
            .expect("chat");

        let raw = cap.lock().expect("lock")[0].clone();
        let head = head_of(&raw).to_ascii_lowercase();
        assert!(head.starts_with("post /v1/responses http/1.1"), "{head}");
        assert!(head.contains(&format!("authorization: bearer {}", KEY.to_ascii_lowercase())));
        assert_eq!(
            body_of(&raw),
            json!({
                "model": "o3-mini",
                "instructions": "seja breve",
                "input": [{"role": "user", "content": "oi"}],
                "tools": [{
                    "type": "function",
                    "name": "read_file",
                    "description": "Lê um arquivo",
                    "parameters": {"type": "object"},
                    "strict": false
                }],
                "store": false,
                "reasoning": {"effort": "high"}
            })
        );
    }

    #[tokio::test]
    async fn responses_omits_optional_fields_when_unset() {
        let (base, cap) = spawn_server(vec![(200, RESP_TEXT), (200, RESP_TEXT)]).await;
        let client = responses_client(base);
        client.chat(&request(None)).await.expect("chat");
        let mut json_req = request(None);
        json_req.response_json = true;
        client.chat(&json_req).await.expect("chat json");

        let captured = cap.lock().expect("lock").clone();
        let plain = body_of(&captured[0]);
        for field in ["reasoning", "tools", "text", "parallel_tool_calls", "messages", "reasoning_effort"] {
            assert!(plain.get(field).is_none(), "campo indevido: {field}");
        }
        assert_eq!(plain["store"], false);
        let with_json = body_of(&captured[1]);
        assert_eq!(with_json["text"]["format"]["type"], "json_object");
    }

    #[tokio::test]
    async fn responses_parses_reasoning_function_call_and_message() {
        let (base, _cap) = spawn_server(vec![(200, RESP_TOOL)]).await;
        let resp = responses_client(base)
            .chat(&responses_request())
            .await
            .expect("chat");

        assert_eq!(resp.message.role, Role::Assistant);
        assert_eq!(resp.message.content, "vou ler");
        assert_eq!(
            resp.message.tool_calls,
            vec![ToolCall {
                id: "call_abc".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"a.txt"}"#.into(),
            }]
        );
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(resp.usage, Some(Usage { prompt: 11, completion: 5, cached: None, reasoning: Some(2) }));
        assert!(!resp.fallback_used);
        // Todos os itens de saída são preservados, na ordem recebida.
        let items = resp.message.provider_items.clone().expect("itens brutos");
        let types: Vec<&str> = items.iter().filter_map(|i| i["type"].as_str()).collect();
        assert_eq!(types, ["reasoning", "function_call", "message"]);
    }

    #[tokio::test]
    async fn responses_next_turn_resends_raw_items_and_call_outputs() {
        let (base, cap) = spawn_server(vec![(200, RESP_TOOL), (200, RESP_TEXT)]).await;
        let client = responses_client(base);
        let first = client.chat(&responses_request()).await.expect("primeira volta");

        let mut req = responses_request();
        req.messages.push(first.message.clone());
        req.messages.push(tool_result("call_abc", "conteúdo"));
        client.chat(&req).await.expect("segunda volta");

        let body = body_of(&cap.lock().expect("lock")[1].clone());
        assert_eq!(
            body["input"],
            json!([
                {"role": "user", "content": "oi"},
                {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC-SEGREDO-123"},
                {"type": "function_call", "id": "fc_1", "call_id": "call_abc", "name": "read_file", "arguments": "{\"path\":\"a.txt\"}", "status": "completed"},
                {"type": "message", "id": "msg_1", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": "vou ler", "annotations": []}]},
                {"type": "function_call_output", "call_id": "call_abc", "output": "conteúdo"}
            ])
        );
    }

    #[tokio::test]
    async fn responses_rebuilds_items_without_provider_items() {
        let (base, cap) = spawn_server(vec![(200, RESP_TEXT)]).await;
        let mut req = responses_request();
        req.messages = vec![
            ChatMessage::text(Role::System, "regra 1"),
            ChatMessage::text(Role::User, "oi"),
            ChatMessage {
                role: Role::Assistant,
                content: String::new(),
                tool_call_id: None,
                tool_calls: vec![ToolCall {
                    id: "call_x".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"b.txt"}"#.into(),
                }],
                provider_items: None,
                images: Vec::new(),
            },
            tool_result("call_x", "dados"),
            ChatMessage::text(Role::Assistant, "pronto"),
            ChatMessage::text(Role::System, "regra 2"),
            ChatMessage::text(Role::User, "continue"),
        ];
        responses_client(base).chat(&req).await.expect("chat");

        let body = body_of(&cap.lock().expect("lock")[0].clone());
        assert_eq!(body["instructions"], "regra 1\n\nregra 2");
        assert_eq!(
            body["input"],
            json!([
                {"role": "user", "content": "oi"},
                {"type": "function_call", "call_id": "call_x", "name": "read_file", "arguments": r#"{"path":"b.txt"}"#},
                {"type": "function_call_output", "call_id": "call_x", "output": "dados"},
                {"role": "assistant", "content": "pronto"},
                {"role": "user", "content": "continue"}
            ])
        );
    }

    #[tokio::test]
    async fn responses_incomplete_max_output_tokens_is_length() {
        let (base, _cap) = spawn_server(vec![(200, RESP_MAX)]).await;
        let resp = responses_client(base)
            .chat(&responses_request())
            .await
            .expect("chat");
        assert_eq!(resp.finish_reason.as_deref(), Some("length"));
        assert_eq!(resp.message.content, "parci");
        assert!(resp.message.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn responses_http_errors_are_redacted_and_not_retried() {
        for status in [401u16, 500] {
            let body: &'static str = Box::leak(
                format!(r#"{{"error":{{"message":"chave {KEY} recusada"}}}}"#).into_boxed_str(),
            );
            let (base, cap) = spawn_server(vec![(status, body)]).await;
            let err = responses_client(base)
                .chat(&responses_request())
                .await
                .expect_err("deve falhar");
            match &err {
                LlmError::Http { status: s, body_snippet, .. } => {
                    assert_eq!(*s, status);
                    assert!(!body_snippet.contains(KEY), "{body_snippet}");
                }
                other => panic!("esperado Http, obtido {other:?}"),
            }
            assert!(!format!("{err:?}").contains(KEY), "{err:?}");
            assert_eq!(cap.lock().expect("lock").len(), 1);
        }
    }

    #[tokio::test]
    async fn responses_effort_rejection_retries_once_without_reasoning() {
        let rejection = r#"{"error":{"message":"Unsupported parameter: 'reasoning.effort' is not supported with this model."}}"#;
        let (base, cap) = spawn_server(vec![(400, rejection), (200, RESP_TEXT)]).await;
        let resp = responses_client(base)
            .chat(&responses_request())
            .await
            .expect("chat com fallback");
        assert!(resp.fallback_used);
        assert_eq!(resp.message.content, "olá");

        let captured = cap.lock().expect("lock").clone();
        assert_eq!(captured.len(), 2, "deve haver exatamente duas requisições");
        assert_eq!(body_of(&captured[0])["reasoning"]["effort"], "high");
        assert!(body_of(&captured[1]).get("reasoning").is_none());
    }

    #[tokio::test]
    async fn responses_unrelated_400_is_not_retried() {
        let (base, cap) = spawn_server(vec![(400, r#"{"error":{"message":"modelo inexistente"}}"#)]).await;
        let err = responses_client(base)
            .chat(&responses_request())
            .await
            .expect_err("deve falhar");
        assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
        assert_eq!(cap.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn responses_without_text_or_tool_call_is_parse_error() {
        // (corpo, detalhe esperado, requisições: 3 = repetida 2 vezes, 1 = não repete).
        let cases = [
            // Só raciocínio, sem texto nem chamada.
            (r#"{"status":"completed","error":null,"output":[{"type":"reasoning","id":"rs_9","summary":[],"encrypted_content":"ENC-SEGREDO-123"}]}"#, "sem texto nem ação", 3),
            // Erro no corpo, com mensagem que não deve ser ecoada.
            (r#"{"status":"failed","error":{"message":"segredo interno do provedor"},"output":[]}"#, "erro do provedor", 3),
            // Status não suportado.
            (r#"{"status":"in_progress","error":null,"output":[]}"#, "status não suportado: in_progress", 1),
            // Incompleta por outro motivo.
            (r#"{"status":"incomplete","incomplete_details":{"reason":"content_filter"},"error":null,"output":[]}"#, "incompleta: content_filter", 3),
            // Sem status.
            (r#"{"output":[]}"#, "status não suportado", 1),
        ];
        for (body, detail, requests) in cases {
            let (base, cap) = spawn_server(vec![(200, body); 3]).await;
            let err = responses_client(base)
                .chat(&responses_request())
                .await
                .expect_err("deve falhar");
            let LlmError::Parse(reason) = err else { panic!("{err:?}") };
            assert_eq!(reason.detail(), detail);
            assert_eq!(cap.lock().expect("lock").len(), requests, "{detail}");
            let text = format!("{err:?} {err} {}", ProviderFailure::BadResponse(reason).describe());
            assert!(!text.contains("segredo interno"), "{text}");
            assert!(!text.contains("ENC-SEGREDO-123"), "{text}");
        }
    }

    #[tokio::test]
    async fn bad_reasons_carry_sanitized_provider_codes() {
        let long = format!(r#"{{"status":"incomplete","incomplete_details":{{"reason":"{}"}},"output":[]}}"#, "a".repeat(41));
        let long: &'static str = Box::leak(long.into_boxed_str());
        let cases: [(&'static str, &str); 6] = [
            (r#"{"status":"failed","error":{"code":"Server_Error","message":"x"},"output":[]}"#, "erro do provedor: server_error"),
            (r#"{"status":"failed","error":{"type":"invalid_request_error"},"output":[]}"#, "erro do provedor: invalid_request_error"),
            (r#"{"status":"failed","error":{"code":"sk-x https://h/v1 chave"},"output":[]}"#, "erro do provedor"),
            (r#"{"status":"incomplete","incomplete_details":{"reason":"tem espaço"},"output":[]}"#, "incompleta"),
            (long, "incompleta"),
            (r#"{"status":"QUEUED","error":null,"output":[]}"#, "status não suportado: queued"),
        ];
        for (body, detail) in cases {
            let (base, _cap) = spawn_server(vec![(200, body); 3]).await;
            let err = responses_client(base).chat(&responses_request()).await.expect_err("falha");
            let failure = provider_failure(&err);
            assert_eq!(failure.to_string(), "provider_bad_response");
            assert_eq!(failure.detail().as_deref(), Some(detail), "{body}");
        }
        // Protocolo chat: `error` no corpo e corpo sem `choices`.
        let (base, _cap) = spawn_server(vec![(200, r#"{"error":{"code":"server_error","message":"x"}}"#); 3]).await;
        let err = LlmClient::new(base, KEY.into()).expect("client").chat(&request(None)).await.expect_err("falha");
        assert_eq!(provider_failure(&err).describe(), "provider_bad_response (erro do provedor: server_error)");
        let (base, _cap) = spawn_server(vec![(200, r#"{"choices":[]}"#); 3]).await;
        let err = LlmClient::new(base, KEY.into()).expect("client").chat(&request(None)).await.expect_err("falha");
        assert_eq!(provider_failure(&err).detail().as_deref(), Some("sem texto nem ação"));
    }

    #[test]
    fn describe_adds_detail_only_to_bad_response() {
        let bad = ProviderFailure::BadResponse(BadReason::with_code(BadKind::Incomplete, Some("content_filter")));
        assert_eq!(bad.describe(), "provider_bad_response (incompleta: content_filter)");
        assert_eq!(ProviderFailure::Http(429, None).describe(), "provider_http_429");
        assert_eq!(ProviderFailure::Timeout.detail(), None);
        let steps = ProviderFailure::BadResponse(BadReason::new(BadKind::StepsExhausted));
        assert_eq!(steps.describe(), "provider_bad_response (passos esgotados)");
        // Spec 091: o código saneado do HTTP entra no `describe`, não no `Display`.
        let coded = ProviderFailure::Http(400, ProviderCode::parse("invalid_value"));
        assert_eq!(coded.to_string(), "provider_http_400");
        assert_eq!(coded.describe(), "provider_http_400 (código: invalid_value)");
    }

    /// Spec 091: só o caminho de imagem lê o código do corpo; o chat (texto, nos dois protocolos)
    /// segue com o HTTP sem código.
    #[tokio::test]
    async fn chat_http_errors_do_not_carry_the_provider_code() {
        let body: &'static str = r#"{"error":{"code":"invalid_value","type":"invalid_request_error","message":"x"}}"#;
        let (base, _cap) = spawn_server(vec![(400, body)]).await;
        let err = LlmClient::new(base, KEY.into()).expect("client").chat(&request(None)).await.expect_err("chat");
        assert!(matches!(err, LlmError::Http { status: 400, code: None, .. }), "{err:?}");
        assert_eq!(provider_failure(&err).describe(), "provider_http_400");
        let (base, _cap) = spawn_server(vec![(400, body)]).await;
        let err = responses_client(base).chat(&responses_request()).await.expect_err("responses");
        assert!(matches!(err, LlmError::Http { status: 400, code: None, .. }), "{err:?}");
        assert_eq!(provider_failure(&err).describe(), "provider_http_400");
    }

    #[test]
    fn image_failure_log_line_has_only_filtered_config_and_the_fixed_reason() {
        let failure = ProviderFailure::Http(400, ProviderCode::parse("invalid_value"));
        assert_eq!(
            image_failure_line("developer", "azure-1", "gpt-image 1\n$x", &failure),
            "nants: imagem falhou papel=developer conexão=azure-1 modelo=gpt-image1x motivo=provider_http_400 (código: invalid_value)"
        );
    }

    #[tokio::test]
    async fn responses_encrypted_content_stays_out_of_debug() {
        let (base, _cap) = spawn_server(vec![(200, RESP_TOOL)]).await;
        let resp = responses_client(base)
            .chat(&responses_request())
            .await
            .expect("chat");
        let mut req = responses_request();
        req.messages.push(resp.message.clone());
        for text in [
            format!("{resp:?}"),
            format!("{:?}", resp.message),
            format!("{req:?}"),
        ] {
            assert!(!text.contains("ENC-SEGREDO-123"), "{text}");
        }
    }

    #[tokio::test]
    async fn from_connection_selects_backend_by_wire_api() {
        for (wire, path) in [
            (WireApi::Chat, "post /v1/chat/completions http/1.1"),
            (WireApi::Responses, "post /v1/responses http/1.1"),
        ] {
            let (base, cap) = spawn_server(vec![(200, RESP_TEXT)]).await;
            let conn = Connection {
                id: "c".into(),
                name: "c".into(),
                base_url: base,
                api_key: KEY.into(),
                wire_api: wire,
                kind: Default::default(),
            };
            let client = LlmClient::from_connection(&conn).expect("client");
            // Chat não entende o corpo Responses, mas o caminho basta para o teste de seleção.
            let _ = client.chat(&request(None)).await;
            let head = head_of(&cap.lock().expect("lock")[0]).to_ascii_lowercase();
            assert!(head.starts_with(path), "{head}");
        }
    }

    /// Resposta Responses com uma `function_call` cujos `arguments` são fornecidos por `extra`.
    fn call_only(extra: &str) -> &'static str {
        let body = format!(
            r#"{{"status":"completed","error":null,"output":[{{"type":"function_call","id":"fc_a","call_id":"call_obj"{extra},"name":"read_file","status":"completed"}}]}}"#
        );
        Box::leak(body.into_boxed_str())
    }

    /// Primeira volta com a chamada `extra`, segunda com o resultado; devolve o `input` da segunda.
    async fn second_turn_input(extra: &str) -> (ChatResponse, Vec<Value>) {
        let (base, cap) = spawn_server(vec![(200, call_only(extra)), (200, RESP_TEXT)]).await;
        let client = responses_client(base);
        let first = client.chat(&responses_request()).await.expect("primeira volta");
        let mut req = responses_request();
        req.messages.push(first.message.clone());
        req.messages.push(tool_result("call_obj", "ok"));
        client.chat(&req).await.expect("segunda volta");
        let body = body_of(&cap.lock().expect("lock")[1].clone());
        let input = body["input"].as_array().expect("input").clone();
        (first, input)
    }

    #[tokio::test]
    async fn arguments_object_is_normalized_to_string_on_resend() {
        let (first, input) = second_turn_input(r#","arguments":{"path":"a.txt"}"#).await;
        assert_eq!(first.message.tool_calls[0].arguments, r#"{"path":"a.txt"}"#);
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["arguments"], r#"{"path":"a.txt"}"#, "{input:?}");
        assert_eq!(input[1]["call_id"], "call_obj");
        assert_eq!(input[2]["call_id"], input[1]["call_id"]);
    }

    #[tokio::test]
    async fn missing_arguments_is_normalized_to_empty_string_on_resend() {
        let (first, input) = second_turn_input("").await;
        assert_eq!(first.message.tool_calls[0].arguments, "");
        assert_eq!(input[1]["arguments"], "", "{input:?}");
        assert_eq!(input[2]["call_id"], input[1]["call_id"]);
    }

    #[tokio::test]
    async fn effort_fallback_requires_effort_mention() {
        // Menciona `reasoning` (item malformado), mas não `effort`: não deve repetir.
        let rejection = r#"{"error":{"message":"item de reasoning malformado"}}"#;
        let (base, cap) = spawn_server(vec![(400, rejection), (200, RESP_TEXT)]).await;
        let err = responses_client(base)
            .chat(&responses_request())
            .await
            .expect_err("deve falhar");
        assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
        assert_eq!(cap.lock().expect("lock").len(), 1);
    }

    #[tokio::test]
    async fn tool_result_without_call_id_is_static_error() {
        let (base, cap) = spawn_server(vec![(200, RESP_TEXT)]).await;
        let mut req = responses_request();
        req.messages.push(ChatMessage {
            role: Role::Tool,
            content: "dados".into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
            provider_items: None,
            images: Vec::new(),
        });
        let err = responses_client(base)
            .chat(&req)
            .await
            .expect_err("deve falhar");
        assert!(matches!(err, LlmError::Config(_)), "{err:?}");
        assert!(!format!("{err:?}").contains("dados"));
        assert!(cap.lock().expect("lock").is_empty());
    }

    /// Responses com busca na web: duas chamadas, uma fonte repetida e uma anotação de outro tipo.
    pub(crate) const RESP_WEB: &str = r#"{"id":"resp_w","object":"response","status":"completed","error":null,"output":[{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"rust"}},{"type":"web_search_call","id":"ws_2","status":"completed","action":{"type":"search","query":"tokio"}},{"type":"message","id":"msg_w","role":"assistant","status":"completed","content":[{"type":"output_text","text":"resposta","annotations":[{"type":"url_citation","url":"https://a.example/x","title":"A","start_index":0,"end_index":3},{"type":"url_citation","url":"https://a.example/x","title":"A","start_index":4,"end_index":6},{"type":"url_citation","url":"https://b.example/y","title":"B","start_index":0,"end_index":1},{"type":"file_citation","file_id":"f"}]}]}],"usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14}}"#;

    fn web_request() -> ChatRequest {
        let mut req = responses_request();
        req.web_search = true;
        req
    }

    #[tokio::test]
    async fn web_search_tool_is_added_only_when_requested_on_responses() {
        let (base, cap) = spawn_server(vec![(200, RESP_TEXT), (200, RESP_TEXT)]).await;
        let client = responses_client(base);
        client.chat(&web_request()).await.expect("com busca");
        client.chat(&responses_request()).await.expect("sem busca");
        let captured = cap.lock().expect("lock").clone();
        let tools = body_of(&captured[0])["tools"].as_array().expect("tools").clone();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[1], json!({"type":"web_search"}));
        let tools = body_of(&captured[1])["tools"].as_array().expect("tools").clone();
        assert_eq!(tools.len(), 1);
    }

    #[tokio::test]
    async fn web_search_alone_creates_tools_array() {
        let (base, cap) = spawn_server(vec![(200, RESP_TEXT)]).await;
        let mut req = request(None);
        req.web_search = true;
        responses_client(base).chat(&req).await.expect("ok");
        let body = body_of(&cap.lock().expect("lock")[0]);
        assert_eq!(body["tools"], json!([{"type":"web_search"}]));
    }

    #[tokio::test]
    async fn chat_wire_ignores_web_search() {
        let (base, cap) = spawn_server(vec![(200, OK_BODY)]).await;
        let mut req = request(None);
        req.web_search = true;
        let resp = LlmClient::new(base, KEY.into()).expect("client").chat(&req).await.expect("ok");
        let raw = cap.lock().expect("lock")[0].clone();
        assert!(!raw.contains("web_search"));
        assert_eq!(resp.web_search_calls, 0);
        assert!(!resp.web_search_rejected);
    }

    #[tokio::test]
    async fn web_search_calls_counted_citations_deduped_and_items_not_resent() {
        let (base, cap) = spawn_server(vec![(200, RESP_WEB), (200, RESP_TEXT)]).await;
        let client = responses_client(base);
        let resp = client.chat(&web_request()).await.expect("ok");
        assert_eq!(resp.web_search_calls, 2);
        assert_eq!(
            resp.citations,
            vec![
                Citation { url: "https://a.example/x".into(), title: "A".into() },
                Citation { url: "https://b.example/y".into(), title: "B".into() },
            ]
        );
        assert!(!resp.web_search_rejected);
        let items = resp.message.provider_items.clone().expect("itens");
        assert!(items.iter().all(|i| i["type"] != "web_search_call"));
        assert!(items.iter().any(|i| i["type"] == "message"));

        // Mesmo que o item chegue em `provider_items`, não é reenviado.
        let mut with_call = resp.message.clone();
        if let Some(items) = with_call.provider_items.as_mut() {
            items.insert(0, json!({"type":"web_search_call","id":"ws_9","status":"completed"}));
        }
        let mut req = web_request();
        req.messages.push(with_call);
        client.chat(&req).await.expect("segundo turno");
        let input = body_of(&cap.lock().expect("lock")[1])["input"].as_array().expect("input").clone();
        assert!(input.iter().all(|i| i["type"] != "web_search_call"));
        assert!(input.iter().any(|i| i["type"] == "message"));
    }

    #[tokio::test]
    async fn web_search_400_retries_once_without_tool_and_flags_rejection() {
        let rejection = r#"{"error":{"message":"Tool type 'web_search' is not supported with this model. SEGREDO-REMOTO"}}"#;
        let (base, cap) = spawn_server(vec![(400, rejection), (200, RESP_TEXT)]).await;
        let resp = responses_client(base).chat(&web_request()).await.expect("fallback");
        assert!(resp.web_search_rejected);
        assert!(!resp.fallback_used, "esforço não foi relaxado");
        assert_eq!(resp.message.content, "olá");
        assert!(!format!("{resp:?}").contains("SEGREDO-REMOTO"));
        let captured = cap.lock().expect("lock").clone();
        assert_eq!(captured.len(), 2);
        assert!(body_of(&captured[0])["tools"].to_string().contains("web_search"));
        assert!(!body_of(&captured[1])["tools"].to_string().contains("web_search"));
        assert_eq!(body_of(&captured[1])["reasoning"]["effort"], "high");
    }

    #[tokio::test]
    async fn web_search_400_still_failing_is_http_error() {
        let rejection = r#"{"error":{"message":"web_search unsupported"}}"#;
        let (base, cap) = spawn_server(vec![(400, rejection), (400, rejection)]).await;
        let err = responses_client(base).chat(&web_request()).await.expect_err("falha");
        assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
        assert_eq!(cap.lock().expect("lock").len(), 2);
    }

    #[tokio::test]
    async fn web_search_and_effort_rejections_are_both_relaxed() {
        let web = r#"{"error":{"message":"web_search not supported"}}"#;
        let eff = r#"{"error":{"message":"reasoning.effort not supported"}}"#;
        let (base, cap) = spawn_server(vec![(400, web), (400, eff), (200, RESP_TEXT)]).await;
        let resp = responses_client(base).chat(&web_request()).await.expect("ok");
        assert!(resp.web_search_rejected && resp.fallback_used);
        assert_eq!(cap.lock().expect("lock").len(), 3);
    }

    fn keyed(mut req: ChatRequest) -> ChatRequest {
        req.cache_key = Some("nants-abc123-developer:T05:a1-0f0f".into());
        req
    }

    /// Spec 122 T05: a chave de cache vai no corpo Responses, como texto.
    #[tokio::test]
    async fn responses_body_carries_the_prompt_cache_key() {
        let (base, cap) = spawn_server(vec![(200, RESP_TEXT)]).await;
        responses_client(base).chat(&keyed(responses_request())).await.expect("chat");
        let body = body_of(&cap.lock().expect("lock")[0]);
        assert_eq!(body["prompt_cache_key"], "nants-abc123-developer:T05:a1-0f0f");
        // Sem chave, o campo não vai.
        let (base, cap) = spawn_server(vec![(200, RESP_TEXT)]).await;
        responses_client(base).chat(&responses_request()).await.expect("chat");
        assert!(body_of(&cap.lock().expect("lock")[0]).get("prompt_cache_key").is_none());
    }

    /// Spec 122 T05: o protocolo Chat não muda: o corpo com chave é igual ao corpo sem chave.
    #[tokio::test]
    async fn chat_wire_body_is_unchanged_by_the_cache_key() {
        let (base, cap) = spawn_server(vec![(200, OK_BODY), (200, OK_BODY)]).await;
        let client = LlmClient::new(base, KEY.into()).expect("client");
        client.chat(&keyed(request(Some(Effort::High)))).await.expect("ok");
        client.chat(&request(Some(Effort::High))).await.expect("ok");
        let captured = cap.lock().expect("lock").clone();
        assert!(!captured[0].contains("prompt_cache_key"));
        assert_eq!(body_of(&captured[0]), body_of(&captured[1]));
    }

    /// Spec 122 T05: o 400 que recusa a chave repete uma vez sem ela, mantendo os demais campos.
    #[tokio::test]
    async fn cache_key_refusal_retries_once_without_the_key() {
        let rejection = r#"{"error":{"message":"Unrecognized request argument supplied: prompt_cache_key"}}"#;
        let (base, cap) = spawn_server(vec![(400, rejection), (200, RESP_TEXT)]).await;
        let resp = responses_client(base).chat(&keyed(responses_request())).await.expect("sem a chave");
        assert_eq!(resp.message.content, "olá");
        assert!(!resp.fallback_used, "o esforço não foi relaxado");
        let captured = cap.lock().expect("lock").clone();
        assert_eq!(captured.len(), 2);
        assert!(body_of(&captured[0]).get("prompt_cache_key").is_some());
        let second = body_of(&captured[1]);
        assert!(second.get("prompt_cache_key").is_none());
        assert_eq!(second["reasoning"]["effort"], "high");
        // Recusada de novo, sem mais nada a relaxar: o erro sobe.
        let (base, cap) = spawn_server(vec![(400, rejection), (400, rejection)]).await;
        let err = responses_client(base).chat(&keyed(responses_request())).await.expect_err("falha");
        assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
        assert_eq!(cap.lock().expect("lock").len(), 2);
    }

    #[test]
    fn rejects_web_search_matches_only_relevant_bodies() {
        assert!(rejects_web_search(r#"{"error":"Tool 'web_search' not allowed"}"#));
        assert!(rejects_web_search(r#"{"error":"Unsupported tool type"}"#));
        assert!(!rejects_web_search(r#"{"error":"Unsupported parameter: reasoning.effort"}"#));
        assert!(!rejects_web_search(r#"{"error":"modelo inexistente"}"#));
    }

    fn image_request() -> ChatRequest {
        let mut req = request(None);
        req.messages.push(ChatMessage::with_images(
            "Capturas pedidas",
            vec![ImageInput::new("image/png", vec![1u8, 2, 3]), ImageInput::new("image/jpeg", vec![255u8])],
        ));
        req
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        let cases = [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")];
        for (raw, enc) in cases {
            assert_eq!(base64_encode(raw.as_bytes()), enc);
        }
        assert_eq!(base64_encode(&[0xff, 0xfe]), "//4=");
    }

    #[test]
    fn bodies_without_images_are_unchanged() {
        let req = request(None);
        let chat = build_body(&req, None);
        assert_eq!(
            chat["messages"],
            json!([{ "role": "system", "content": "seja breve" }, { "role": "user", "content": "oi" }])
        );
        let resp = build_responses_body(&req, None).expect("corpo");
        assert_eq!(resp["input"], json!([{ "role": "user", "content": "oi" }]));
    }

    #[test]
    fn chat_body_sends_images_as_image_url_parts() {
        let body = build_body(&image_request(), None);
        assert_eq!(
            body["messages"][2],
            json!({ "role": "user", "content": [
                { "type": "text", "text": "Capturas pedidas" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AQID" } },
                { "type": "image_url", "image_url": { "url": "data:image/jpeg;base64,/w==" } },
            ]})
        );
        assert_eq!(body["messages"][1], json!({ "role": "user", "content": "oi" }));
    }

    #[test]
    fn responses_body_sends_images_as_input_image_parts() {
        let body = build_responses_body(&image_request(), None).expect("corpo");
        assert_eq!(
            body["input"][1],
            json!({ "role": "user", "content": [
                { "type": "input_text", "text": "Capturas pedidas" },
                { "type": "input_image", "image_url": "data:image/png;base64,AQID" },
                { "type": "input_image", "image_url": "data:image/jpeg;base64,/w==" },
            ]})
        );
    }

    #[test]
    fn debug_hides_image_bytes() {
        let msg = ChatMessage::with_images("x", vec![ImageInput::new("image/png", vec![0xAB; 40])]);
        let shown = format!("{msg:?}");
        assert!(shown.contains("1 imagem(ns), 40 bytes"), "{shown}");
        assert!(!shown.contains("171") && !shown.contains("AB") && !shown.contains("base64"), "{shown}");
        assert!(ChatMessage::text(Role::User, "y").images.is_empty());
    }
}
