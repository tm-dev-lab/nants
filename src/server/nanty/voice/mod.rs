//! Voz do Nanty (spec 138 T09, G5): a fala com legenda, a nota de voz transcrita e os áudios da
//! conversa para a janela.
//!
//! - **Medição primeiro (C-14):** o codificador `opus-pure` (Rust puro) foi aprovado pela medição
//!   registrada na T09. Por isso a ordem é a da decisão do usuário: Gemini (WAV de 24 kHz que vira
//!   Opus em OGG, [`encode`]), depois OpenAI (que já entrega Opus em OGG), depois só texto.
//! - **A ordem ([`chain`]):** a lista `[nanty.voice] order`, ou, vazia, as conexões do Google Gemini
//!   com chave e depois as da OpenAI (`api.openai.com`). O modelo e a voz de cada passo vêm da
//!   configuração; vazios, os iniciais provisórios de `config::nanty` (nunca aqui).
//! - **Quando fala:** com a voz ligada, o Nanty responde em voz a quem falou com ele por nota de voz
//!   no Telegram ([`attach_speech`]). A fala é a do texto da resposta, já redigido; a nota sai com a
//!   legenda curta (até 1024) e, se a legenda cortou, o texto completo numa mensagem à parte, pela
//!   mesma fila de saída da T07 ([`super::telegram::outbox`]).
//! - **Ouvir ([`hear`]):** só a nota de voz do dono (a escuta já filtra estranhos e grupos, T06).
//!   A duração e o tamanho são conferidos antes de baixar (até [`MAX_INPUT_BYTES`] e
//!   `max_input_secs`), e de novo no `getFile` e no download. A transcrição é do Gemini; o texto
//!   entra na conversa redigido, com a marca `voice`, e segue pela fila única da T08.
//! - **Orçamento:** cada fala e cada transcrição entram no gasto do dia do Nanty (os tokens
//!   informados ou estimados e o US$ pelo preço do modelo, quando há) e contam em `voice_calls`.
//!   Sem orçamento, ou com a falha de um passo, passa ao próximo e por fim ao texto: o aviso nunca
//!   se perde. O registro guarda só códigos (o provedor, o motivo), nunca o texto nem o áudio.
//! - **Os áudios ([`get_audio`]):** ficam em `<pasta do Nanty>/audio/<id>.ogg` (0600) e só são
//!   servidos pelo id de uma mensagem que ainda está na conversa: quando a mensagem sai na rotação,
//!   o áudio deixa de ser servido e o arquivo é apagado na próxima limpeza ([`prune`]).

pub mod encode;
pub mod provider;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};

use super::journal::{JournalEntry, JournalKind};
use super::jsonl::{ensure_private_dir, read_private_bytes, write_atomic};
use super::store::{AudioRef, Channel, ConversationMessage};
use super::telegram::client::{TelegramClient, VoiceNote};
use super::telegram::format;
use super::{nanty_lang, render, turn, Avatar, Nanty};
use crate::agents::llm_client::{Billing, Usage};
use crate::config::{
    Config, Connection, ConnectionKind, DEFAULT_GEMINI_TTS_MODEL, DEFAULT_GEMINI_VOICE, DEFAULT_OPENAI_TTS_MODEL,
    DEFAULT_OPENAI_VOICE, DEFAULT_TRANSCRIBE_MODEL,
};
use crate::server::error::ApiError;
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Pasta dos áudios dentro da pasta do Nanty.
pub const AUDIO_DIR: &str = "audio";
/// Maior nota de voz baixada (o limite do `getFile` do Telegram).
pub const MAX_INPUT_BYTES: u64 = 20 * 1024 * 1024;
/// Maior áudio guardado ou servido.
pub const MAX_AUDIO_BYTES: u64 = 20 * 1024 * 1024;
/// Maior texto mandado à fala (a resposta do modelo tem até 600; os textos fixos são curtos).
pub const MAX_SPEECH_CHARS: usize = 2000;
/// Tokens por segundo de áudio, para estimar a saída quando o provedor não informa (a taxa
/// publicada do áudio do Gemini; provisória para a OpenAI).
pub const AUDIO_TOKENS_PER_SEC: u64 = 25;
/// Um arquivo de áudio sem mensagem só é apagado depois disto (a mensagem dele pode estar sendo
/// gravada agora).
const PRUNE_GRACE: Duration = Duration::from_secs(10 * 60);
/// Cache privado: o id do áudio nunca muda de conteúdo.
const CACHE_CONTROL: &str = "private, max-age=3600";

/// Quem fala.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Gemini,
    OpenAi,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Gemini => "gemini",
            Provider::OpenAi => "openai",
        }
    }
}

/// Um passo da fala: a conexão, o modelo e a voz já resolvidos.
#[derive(Debug, Clone)]
pub struct Step {
    pub provider: Provider,
    pub connection: Connection,
    pub model: String,
    pub voice: String,
}

/// Conexão da OpenAI: por chave, no endereço `api.openai.com` (a escolha automática; na lista
/// explícita, qualquer conexão por chave serve).
fn is_openai(conn: &Connection) -> bool {
    conn.kind == ConnectionKind::ApiKey
        && reqwest::Url::parse(conn.endpoint().trim()).ok().and_then(|u| u.host_str().map(str::to_string)).as_deref()
            == Some("api.openai.com")
}

/// O tipo de conexão serve para a fala? Só o Google Gemini e as conexões por chave (a fala da
/// OpenAI). A mesma regra vale no [`chain`] e na gravação dos Ajustes (`nanty::settings`, T11).
pub fn speaks(kind: ConnectionKind) -> bool {
    matches!(kind, ConnectionKind::GoogleGemini | ConnectionKind::ApiKey)
}

/// O tipo de conexão serve para a transcrição? Só o Google Gemini. A mesma regra vale no
/// [`transcriber`] e na gravação dos Ajustes (`nanty::settings`, T11).
pub fn transcribes(kind: ConnectionKind) -> bool {
    kind == ConnectionKind::GoogleGemini
}

fn step_of(conn: &Connection, model: &str, voice: &str) -> Result<Step, &'static str> {
    if !speaks(conn.kind) {
        return Err("unsupported");
    }
    let (provider, default_model, default_voice) = if conn.kind == ConnectionKind::GoogleGemini {
        (Provider::Gemini, DEFAULT_GEMINI_TTS_MODEL, DEFAULT_GEMINI_VOICE)
    } else {
        (Provider::OpenAi, DEFAULT_OPENAI_TTS_MODEL, DEFAULT_OPENAI_VOICE)
    };
    let pick = |v: &str, d: &str| if v.trim().is_empty() { d.to_string() } else { v.trim().to_string() };
    Ok(Step { provider, connection: conn.clone(), model: pick(model, default_model), voice: pick(voice, default_voice) })
}

/// A ordem da fala: os passos utilizáveis e os códigos dos que ficaram de fora (conexão que não
/// existe mais ou tipo sem voz).
pub fn chain(cfg: &Config) -> (Vec<Step>, Vec<&'static str>) {
    let voice = &cfg.nanty.voice;
    let mut steps = Vec::new();
    let mut skipped = Vec::new();
    if voice.order.is_empty() {
        let usable = |c: &&Connection| c.has_usable_key();
        let gemini = cfg.connections.iter().filter(|c| c.kind == ConnectionKind::GoogleGemini).filter(usable);
        let openai = cfg.connections.iter().filter(|c| is_openai(c)).filter(usable);
        for conn in gemini.chain(openai) {
            if let Ok(step) = step_of(conn, "", "") {
                steps.push(step);
            }
        }
        return (steps, skipped);
    }
    for route in &voice.order {
        match cfg.connections.iter().find(|c| c.id == route.connection_id) {
            None => skipped.push("missing_connection"),
            Some(conn) => match step_of(conn, &route.model, &route.voice) {
                Ok(step) => steps.push(step),
                Err(code) => skipped.push(code),
            },
        }
    }
    (steps, skipped)
}

/// A conexão e o modelo da transcrição: o `[nanty.voice] transcribe` ou a primeira conexão do
/// Google Gemini com chave, com o modelo inicial.
pub fn transcriber(cfg: &Config) -> Option<(Connection, String)> {
    let route = &cfg.nanty.voice.transcribe;
    let conn = if route.connection_id.trim().is_empty() {
        cfg.connections.iter().find(|c| transcribes(c.kind) && c.has_usable_key())?
    } else {
        cfg.connections.iter().find(|c| c.id == route.connection_id && transcribes(c.kind))?
    };
    let model = if route.model.trim().is_empty() { DEFAULT_TRANSCRIBE_MODEL.to_string() } else { route.model.trim().to_string() };
    Some((conn.clone(), model))
}

// ---- Registro e gasto ----

fn journal(nanty: &Nanty, code: &str, params: Map<String, Value>) -> JournalEntry {
    let mut entry = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Voice, code);
    entry.params = params;
    entry
}

fn param(name: &str, value: impl Into<Value>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(name.to_string(), value.into());
    map
}

fn exhausted(cfg: &Config, nanty: &Nanty) -> bool {
    turn::budget_view(cfg, &nanty.snapshot().spend_on(&nanty.now().day_key())).exhausted
}

fn estimate_tokens(text: &str) -> u64 {
    (text.len() / 4 + 1) as u64
}

/// Soma a chamada ao gasto do dia e devolve o custo (para o registro).
fn charge(cfg: &Config, nanty: &Nanty, model: &str, input: u64, output: u64) -> Option<f64> {
    let usage = Usage::new(u32::try_from(input).unwrap_or(u32::MAX), u32::try_from(output).unwrap_or(u32::MAX));
    let usd = turn::turn_cost(cfg, model, &usage, Billing::Metered);
    let day = nanty.now().day_key();
    nanty.update(|st| {
        st.add_voice_spend(&day, input, output, usd);
        true
    });
    nanty.changed(super::Part::Status);
    usd
}

/// O avatar volta ao estado derivado em qualquer saída.
struct Speaking<'a>(&'a Nanty);

impl Drop for Speaking<'_> {
    fn drop(&mut self) {
        self.0.set_busy(None);
    }
}

// ---- A fala ----

/// Uma fala pronta: a nota de voz (Opus em OGG), a duração e quem falou.
#[derive(Debug, Clone)]
pub struct Spoken {
    pub ogg: Vec<u8>,
    pub duration_secs: u32,
    pub provider: Provider,
}

/// Um passo: chama o provedor, soma o gasto e, no Gemini, codifica o PCM em Opus.
async fn speak_step(cfg: &Config, nanty: &Nanty, step: &Step, text: &str) -> Result<Spoken, &'static str> {
    let audio = match step.provider {
        Provider::Gemini => provider::gemini_speak(&step.connection, &step.model, &step.voice, text).await?,
        Provider::OpenAi => provider::openai_speak(&step.connection, &step.model, &step.voice, text).await?,
    };
    let (ogg, duration_secs) = match step.provider {
        Provider::OpenAi => {
            let secs = provider::ogg_duration_secs(&audio.bytes).ok_or("format")?;
            (audio.bytes.clone(), secs)
        }
        Provider::Gemini => {
            let bytes = audio.bytes.clone();
            let encoded = tokio::task::spawn_blocking(move || {
                let pcm = encode::pcm_of_gemini(&bytes)?;
                let secs = pcm.duration_secs();
                encode::ogg_opus(&pcm).map(|ogg| (ogg, secs))
            })
            .await
            .map_err(|_| "encode")?;
            // O provedor já cobrou: o gasto entra mesmo se a codificação falhar.
            let secs = encoded.as_ref().map(|(_, s)| *s).unwrap_or(0);
            let output = audio.output_tokens.unwrap_or(u64::from(secs) * AUDIO_TOKENS_PER_SEC);
            charge(cfg, nanty, &step.model, audio.input_tokens.unwrap_or_else(|| estimate_tokens(text)), output);
            return encoded.map(|(ogg, secs)| Spoken { ogg, duration_secs: secs, provider: step.provider });
        }
    };
    let output = audio.output_tokens.unwrap_or(u64::from(duration_secs) * AUDIO_TOKENS_PER_SEC);
    charge(cfg, nanty, &step.model, audio.input_tokens.unwrap_or_else(|| estimate_tokens(text)), output);
    if ogg.len() as u64 > MAX_AUDIO_BYTES {
        return Err("too_big");
    }
    Ok(Spoken { ogg, duration_secs, provider: step.provider })
}

/// A fala do texto pela ordem da voz. `Err` com os motivos (códigos) quando nenhum passo falou: o
/// texto segue sem áudio.
pub async fn speak(state: &AppState, nanty: &Nanty, text: &str) -> Result<Spoken, Vec<&'static str>> {
    let cfg = state.config().read().await.clone();
    let text: String = text.chars().take(MAX_SPEECH_CHARS).collect();
    let (steps, mut reasons) = chain(&cfg);
    let mut spoken = None;
    for step in &steps {
        if exhausted(&cfg, nanty) {
            reasons.push("budget");
            break;
        }
        nanty.set_busy(Some(Avatar::Speaking));
        let speaking = Speaking(nanty);
        let result = speak_step(&cfg, nanty, step, &text).await;
        drop(speaking);
        match result {
            Ok(done) => {
                let mut p = param("provider", step.provider.as_str());
                p.insert("secs".into(), json!(done.duration_secs));
                nanty.record(&journal(nanty, "voice.ok", p));
                spoken = Some(done);
                break;
            }
            Err(code) => {
                let mut p = param("provider", step.provider.as_str());
                p.insert("reason".into(), json!(code));
                nanty.record(&journal(nanty, "voice.failed", p));
                reasons.push(code);
            }
        }
    }
    match spoken {
        Some(done) => Ok(done),
        None => {
            if steps.is_empty() && reasons.is_empty() {
                reasons.push("no_connection");
            }
            nanty.record(&journal(nanty, "voice.text", param("reasons", json!(reasons))));
            Err(reasons)
        }
    }
}

/// Com a voz ligada e a pessoa tendo falado por voz no Telegram, a resposta ganha a fala: o texto
/// da resposta (como vai ao Telegram, já redigido) vira nota de voz. Sem fala, nada muda: a
/// resposta sai em texto.
pub async fn attach_speech(state: &AppState, nanty: &Arc<Nanty>, person: &ConversationMessage, message: &mut ConversationMessage) {
    let cfg = state.config().read().await.clone();
    if !person.voice || person.channel != Channel::Telegram || !cfg.nanty.voice.enabled {
        return;
    }
    let lang = nanty_lang(&cfg.nanty);
    let secrets = SecretSet::from_config(&cfg);
    let text = format::clean(&secrets, &render::message_text(lang, message));
    if text.is_empty() {
        return;
    }
    let Ok(spoken) = speak(state, nanty, &text).await else { return };
    let n = Arc::clone(nanty);
    let audio = tokio::task::spawn_blocking(move || save_audio(&n, &spoken.ogg, spoken.duration_secs)).await.ok().flatten();
    message.audio = audio;
}

// ---- Os áudios guardados ----

/// Id de áudio aceito: 16 hex (o `new_id`).
pub fn valid_audio_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Nanty {
    /// A pasta dos áudios (`None` sem pasta de dados).
    fn audio_dir(&self) -> Option<PathBuf> {
        self.store.as_ref().map(|s| s.dir().join(AUDIO_DIR))
    }

    /// Toda a conversa guardada (as gerações que a rotação ainda guarda).
    fn conversation_all(&self) -> Vec<ConversationMessage> {
        self.store.as_ref().map(|s| s.conversation().tail(usize::MAX)).unwrap_or_default()
    }

    /// O áudio de uma mensagem que ainda está na conversa.
    pub fn read_audio(&self, id: &str) -> Option<Vec<u8>> {
        if !valid_audio_id(id) {
            return None;
        }
        let referenced = self.conversation_all().iter().any(|m| m.audio.as_ref().is_some_and(|a| a.id == id));
        if !referenced {
            return None;
        }
        read_private_bytes(&self.audio_dir()?.join(format!("{id}.ogg")), MAX_AUDIO_BYTES)
    }
}

/// Grava um áudio (só o dono da trava) e devolve a referência.
pub fn save_audio(nanty: &Nanty, ogg: &[u8], duration_secs: u32) -> Option<AudioRef> {
    if !nanty.owns() || ogg.is_empty() || ogg.len() as u64 > MAX_AUDIO_BYTES {
        return None;
    }
    let dir = nanty.audio_dir()?;
    ensure_private_dir(&dir).ok()?;
    let id = super::new_id();
    write_atomic(&dir.join(format!("{id}.ogg")), ogg).ok()?;
    prune(nanty);
    Some(AudioRef { id, duration_secs, bytes: ogg.len() as u64 })
}

/// Apaga os áudios que nenhuma mensagem da conversa cita mais (a rotação levou a mensagem, ou o
/// projeto saiu), passada a espera de [`PRUNE_GRACE`].
pub fn prune(nanty: &Nanty) {
    prune_older(nanty, PRUNE_GRACE);
}

/// Como [`prune`], com a espera dada (os testes usam zero).
fn prune_older(nanty: &Nanty, grace: Duration) {
    if !nanty.owns() {
        return;
    }
    let Some(dir) = nanty.audio_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let referenced: Vec<String> = nanty.conversation_all().into_iter().filter_map(|m| m.audio.map(|a| a.id)).collect();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".ogg")) else { continue };
        if !valid_audio_id(id) || referenced.iter().any(|r| r == id) {
            continue;
        }
        let old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= grace);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn not_found() -> ApiError {
    ApiError::coded(StatusCode::NOT_FOUND, "not_found", Map::new())
}

/// Um intervalo pedido no `Range` (T10): o Safari, inclusive o do iPhone, só toca um áudio servido
/// com intervalos (ele pede `bytes=0-1` antes de tocar e desiste sem a resposta 206).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// Sem `Range`, ou um pedido que a rota não atende por partes (vários intervalos, outra
    /// unidade): o arquivo inteiro, com 200.
    Full,
    /// Do byte `start` ao `end`, inclusive: 206.
    Part { start: u64, end: u64 },
    /// Fora do arquivo: 416.
    Unsatisfiable,
}

/// Lê o `Range` de um único intervalo em bytes (`a-b`, `a-` ou `-n`) para um arquivo de `len` bytes.
pub fn parse_range(raw: Option<&str>, len: u64) -> ByteRange {
    let Some(spec) = raw.map(str::trim).and_then(|r| r.strip_prefix("bytes=")) else { return ByteRange::Full };
    if spec.contains(',') {
        return ByteRange::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else { return ByteRange::Full };
    let (a, b) = (a.trim(), b.trim());
    let parsed = |v: &str| v.parse::<u64>().ok();
    let (start, end) = match (a.is_empty(), b.is_empty()) {
        // `-n`: os últimos `n` bytes.
        (true, false) => match parsed(b) {
            Some(0) => return ByteRange::Unsatisfiable,
            Some(n) => (len.saturating_sub(n), len.saturating_sub(1)),
            None => return ByteRange::Full,
        },
        (false, true) => match parsed(a) {
            Some(start) => (start, len.saturating_sub(1)),
            None => return ByteRange::Full,
        },
        (false, false) => match (parsed(a), parsed(b)) {
            (Some(start), Some(end)) if start <= end => (start, end.min(len.saturating_sub(1))),
            (Some(_), Some(_)) => return ByteRange::Unsatisfiable,
            _ => return ByteRange::Full,
        },
        (true, true) => return ByteRange::Full,
    };
    if len == 0 || start >= len {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Part { start, end }
}

/// `GET /api/nanty/audio/:id` (T09, para a janela da T10): o áudio de uma mensagem da conversa,
/// pelo id validado (nunca um caminho vindo do cliente). Id inválido, áudio que saiu da conversa ou
/// arquivo ausente: 404, sem eco. Aceita o `Range` de um intervalo (206 e `Content-Range`; fora do
/// arquivo, 416), que o Safari exige para tocar.
pub async fn get_audio(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    if !valid_audio_id(&id) {
        return Err(not_found());
    }
    let nanty = Arc::clone(state.nanty());
    let data = tokio::task::spawn_blocking(move || nanty.read_audio(&id)).await.ok().flatten().ok_or_else(not_found)?;
    let len = data.len() as u64;
    let range = parse_range(headers.get(header::RANGE).and_then(|v| v.to_str().ok()), len);
    let mut resp = match range {
        ByteRange::Full => data.into_response(),
        ByteRange::Part { start, end } => {
            let part = data.get(start as usize..=end as usize).map(<[u8]>::to_vec).unwrap_or_default();
            let mut resp = (StatusCode::PARTIAL_CONTENT, part).into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")) {
                resp.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            resp
        }
        ByteRange::Unsatisfiable => {
            let mut resp =
                ApiError::coded(StatusCode::RANGE_NOT_SATISFIABLE, "nanty_audio_range", Map::new()).into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{len}")) {
                resp.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            resp.headers_mut().insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return Ok(resp);
        }
    };
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(provider::OGG_MIME));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    Ok(resp)
}

// ---- Ouvir ----

/// Por que a nota de voz não virou texto (o código da resposta fixa).
fn refusal(reason: &str) -> &'static str {
    match reason {
        "off" => "nanty.voice.off",
        "too_long" => "nanty.voice.too_long",
        "too_big" => "nanty.voice.too_big",
        "no_transcriber" => "nanty.voice.no_transcriber",
        "budget" => "nanty.voice.budget",
        _ => "nanty.voice.not_understood",
    }
}

/// Baixa e transcreve a nota de voz. `Ok((texto, áudio))` ou o motivo em código.
async fn listen(state: &AppState, nanty: &Nanty, cfg: &Config, note: &VoiceNote) -> Result<(String, Vec<u8>), &'static str> {
    let voice = &cfg.nanty.voice;
    if !voice.enabled {
        return Err("off");
    }
    if note.duration > voice.max_input_secs {
        return Err("too_long");
    }
    if note.file_size.is_some_and(|s| s > MAX_INPUT_BYTES) {
        return Err("too_big");
    }
    let Some((conn, model)) = transcriber(cfg) else { return Err("no_transcriber") };
    if exhausted(cfg, nanty) {
        return Err("budget");
    }
    let token = cfg.nanty.telegram.resolve_token().filter(|t| super::telegram::valid_token(t)).ok_or("telegram")?;
    let client = TelegramClient::new(state.telegram_api(), token, Duration::ZERO).map_err(|e| e.code())?;
    let file = client.get_file(&note.file_id).await.map_err(|e| e.code())?;
    if file.file_size.is_some_and(|s| s > MAX_INPUT_BYTES) {
        return Err("too_big");
    }
    let path = file.file_path.ok_or("bad_response")?;
    let audio = client.download(&path, MAX_INPUT_BYTES).await.map_err(|e| e.code())?;
    nanty.set_busy(Some(Avatar::Thinking));
    let thinking = Speaking(nanty);
    let prompt = crate::agents::prompts::nanty_transcribe_prompt(nanty_lang(&cfg.nanty));
    let heard = provider::gemini_transcribe(&conn, &model, &prompt, &audio).await;
    drop(thinking);
    let heard = heard?;
    let output = heard.output_tokens.unwrap_or_else(|| estimate_tokens(&heard.text));
    let input = heard.input_tokens.unwrap_or(note.duration.saturating_mul(AUDIO_TOKENS_PER_SEC));
    charge(cfg, nanty, &model, input, output);
    Ok((heard.text, audio))
}

/// A nota de voz do dono (ao vivo, já filtrada pela escuta): baixa, transcreve e segue como texto
/// pela fila única da T08, com a marca `voice`. Quando não dá, a mensagem da pessoa entra na
/// conversa sem texto e o Nanty responde no Telegram com o motivo (texto fixo).
pub async fn hear(state: &AppState, note: &VoiceNote) {
    let nanty = Arc::clone(state.nanty());
    let cfg = state.config().read().await.clone();
    let secrets = SecretSet::from_config(&cfg);
    let heard = listen(state, &nanty, &cfg, note).await;
    let (text, audio_bytes, reason) = match heard {
        Ok((text, audio)) => (Some(text), Some(audio), None),
        Err(reason) => (None, None, Some(reason)),
    };
    let audio = match audio_bytes {
        Some(bytes) => {
            let n = Arc::clone(&nanty);
            let secs = u32::try_from(note.duration).unwrap_or(u32::MAX);
            tokio::task::spawn_blocking(move || save_audio(&n, &bytes, secs)).await.ok().flatten()
        }
        None => None,
    };
    let n = Arc::clone(&nanty);
    let person = tokio::task::spawn_blocking(move || {
        super::chat::person_message_with(&n, &secrets, Channel::Telegram, text.as_deref(), true, audio)
    })
    .await
    .ok()
    .flatten();
    let Some(person) = person else { return };
    match reason {
        None => {
            nanty.record(&journal(&nanty, "voice.heard", param("secs", note.duration)));
            super::chat::reply(state, &person).await;
        }
        Some(reason) => {
            nanty.record(&journal(&nanty, "voice.listen_failed", param("reason", reason)));
            let code = refusal(reason);
            let params = match code {
                "nanty.voice.too_long" => param("max", cfg.nanty.voice.max_input_secs),
                "nanty.voice.too_big" => param("max", MAX_INPUT_BYTES / (1024 * 1024)),
                _ => Map::new(),
            };
            let mut message = ConversationMessage::from_nanty(super::new_id(), nanty.now().epoch_ms, Channel::Telegram, code, params);
            message.reply_to = Some(person.id.clone());
            let _ = tokio::task::spawn_blocking(move || nanty.push_message(&message)).await;
        }
    }
}

/// Ouve numa tarefa à parte (a escuta segue enquanto baixa e transcreve).
pub fn spawn_hear(state: &AppState, note: VoiceNote) {
    let state = state.clone();
    tokio::spawn(async move {
        hear(&state, &note).await;
    });
}

#[cfg(test)]
pub(crate) mod tests;
