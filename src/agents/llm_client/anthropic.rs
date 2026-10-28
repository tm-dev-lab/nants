//! Adaptador nativo da Anthropic Messages API (spec 132 T03), com o streaming SSE (T04).
//!
//! `POST {endpoint}/v1/messages`, com `x-api-key` e `anthropic-version` ([`super::auth_headers`]).
//! O corpo sai do [`ChatRequest`]:
//! - as mensagens `system` viram o `system` em blocos de texto;
//! - os resultados de ferramenta viram blocos `tool_result` numa mensagem `user` (mensagens seguidas
//!   do mesmo papel são juntadas, com os `tool_result` primeiro);
//! - as imagens vão em base64;
//! - `max_tokens` é obrigatório: o conhecido do modelo ([`remember_max_tokens`], preenchido pela
//!   lista de modelos), senão [`DEFAULT_MAX_TOKENS`], reduzido uma vez se o 400 citar o campo. Ele
//!   não passa do limite de tokens de saída por minuto lido no cabeçalho, nem do teto aprendido
//!   com um 429 que citou os tokens de saída (spec 132 T09, [`LlmClient::capped_max_tokens`]).
//!
//! Esforço: `thinking: {type: adaptive}` com `output_config.effort`. A escada de relaxamento por 400
//! é a de sempre, sem tabela de versões: `adaptive` recusado vira `enabled` com `budget_tokens`, e
//! depois sem thinking. O thinking pede `display: summarized` (T04): os modelos novos omitem o
//! texto do raciocínio por padrão, e é ele que chega à tela; o campo recusado sai antes de tudo.
//!
//! A resposta guarda os blocos de conteúdo inalterados em [`ChatMessage::provider_items`]: os
//! `thinking`/`redacted_thinking` (com a `signature`) e os `tool_use` voltam assim na volta seguinte.
//!
//! Streaming (T04): o caminho do agente pede `stream: true`; o [`StreamAcc`] remonta dos eventos
//! (`message_start`, `content_block_*`, `message_delta`, `message_stop`) a mesma mensagem da versão
//! sem stream, que passa pelo mesmo [`parse`]. O texto e o thinking chegam à tela como deltas.
//!
//! Busca web (T05): com `web_search`, a ferramenta de servidor vai no pedido com o `max_uses` do
//! limite do Arquiteto. A escada desce `web_search_20260209` (filtragem dinâmica) para
//! `web_search_20250305` e depois para sem busca, uma vez por degrau, quando o 400 cita a busca. A
//! dinâmica também desce quando o modelo a chama com a entrada recusada (`invalid_tool_input`, HTTP
//! 200, sem busca com resultado); a queda da dinâmica fica lembrada por modelo ([`uses_basic_web`]).
//! O histórico com os blocos da busca vai sem a ferramenta quando o pedido não a pede (a API aceita;
//! só um 400 que cite a busca a declara, a básica com uma busca). Os
//! blocos `server_tool_use` e `*_tool_result` voltam inalterados; a contagem vem de
//! `usage.server_tool_use.web_search_requests` e as fontes das `citations` do texto. O `pause_turn`
//! (o laço do servidor parou no limite) continua reenviando o conteúdo como mensagem `assistant`, até
//! [`MAX_CONTINUATIONS`] vezes, com o conteúdo e o uso somados.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde_json::{json, Value};

use super::cut::StreamDiag;
use super::{
    emit, join_message, looks_like_sse, retry_after, transport_error, Failed, LlmClient, SseAcc, StreamDelta,
    NO_FINAL, RESPONSES_JSON_HINT, STREAM_MAX_DURATION, TOOL_RESULT_WITHOUT_ID,
};
use super::{BadKind, BadReason, ChatMessage, ChatRequest, ChatResponse, Citation, Role, ToolCall, ToolSpec, Usage};
use crate::agents::LlmError;
use crate::config::Effort;

/// Caminho da Messages API, a partir do endereço da conexão.
pub const MESSAGES_PATH: &str = "/v1/messages";
/// `max_tokens` quando o limite do modelo não é conhecido: conservador e aceito pelos modelos atuais.
pub const DEFAULT_MAX_TOKENS: u32 = 32_000;
/// Redução usada quando o 400 cita `max_tokens` sem dizer o limite.
const FALLBACK_MAX_TOKENS: u32 = 8_192;
/// Menor `budget_tokens` aceito pelo thinking `enabled` (e o menor limite lido de um 400).
pub const MIN_THINKING_BUDGET: u32 = 1_024;
/// Tetos de `cache_control` por pedido impostos pela API.
const MAX_CACHE_BREAKPOINTS: usize = 4;
/// Descrição das ferramentas declaradas só porque o histórico as usa (pedido sem ferramentas, como
/// o resumo do histórico). Texto para o modelo, fora do dicionário: a API recusa `tool_use` e
/// `tool_result` sem as ferramentas declaradas.
const PAST_TOOL_DESCRIPTION: &str = "Ferramenta usada antes nesta conversa; não chame agora.";

/// Tipos de bloco de conteúdo da Anthropic que o adaptador reenvia de `provider_items`, além dos
/// resultados das ferramentas de servidor (`web_search_tool_result`, `code_execution_tool_result`,
/// ..., que terminam em [`SERVER_RESULT_SUFFIX`]).
const BLOCK_TYPES: [&str; 5] = ["text", "thinking", "redacted_thinking", "tool_use", "server_tool_use"];
/// Fim do tipo dos blocos de resultado das ferramentas de servidor (spec 132 T05).
const SERVER_RESULT_SUFFIX: &str = "_tool_result";
/// Busca web com filtragem dinâmica (usa a execução de código por baixo; não declarar junto).
pub const WEB_SEARCH_DYNAMIC: &str = "web_search_20260209";
/// Busca web básica, aceita pelos modelos sem a filtragem dinâmica.
pub const WEB_SEARCH_BASIC: &str = "web_search_20250305";
/// Nome da ferramenta de busca web.
const WEB_SEARCH_NAME: &str = "web_search";
/// Continuações de um `pause_turn` por chamada.
pub const MAX_CONTINUATIONS: usize = 5;
/// `stop_reason` do laço do servidor parado no limite (e o `finish_reason` interno até continuar).
const PAUSE_TURN: &str = "pause_turn";

/// Bloco de conteúdo da Anthropic (o que o adaptador guarda em `provider_items`)?
pub fn is_block(item: &Value) -> bool {
    item.get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| BLOCK_TYPES.contains(&t) || t.ends_with(SERVER_RESULT_SUFFIX))
}

/// Bloco da busca web (a chamada `server_tool_use` de `web_search` ou o resultado dela)?
fn is_web_block(item: &Value) -> bool {
    item["type"] == "web_search_tool_result" || (item["type"] == "server_tool_use" && item["name"] == WEB_SEARCH_NAME)
}

/// Bloco de raciocínio (`thinking` ou `redacted_thinking`), que só volta inalterado.
pub fn is_thinking(item: &Value) -> bool {
    matches!(item.get("type").and_then(Value::as_str), Some("thinking" | "redacted_thinking"))
}

/// Limite de saída conhecido de um modelo e se ele veio de uma recusa (spec 132 T09).
#[derive(Debug, Clone, Copy)]
struct Known {
    max: u32,
    /// Aprendido por um 400 que disse o limite: a lista de modelos não o aumenta.
    learned: bool,
}

/// Limites de saída conhecidos por (endereço, modelo).
fn known_limits() -> &'static Mutex<HashMap<(String, String), Known>> {
    static LIMITS: OnceLock<Mutex<HashMap<(String, String), Known>>> = OnceLock::new();
    LIMITS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Guarda o limite `max` de `model` em `base_url`, aprendido por recusa (`learned`) ou da lista.
/// Valor abaixo de [`MIN_THINKING_BUDGET`] é ignorado. O aprendido por recusa prevalece: a lista
/// não o aumenta no mesmo processo (só o baixa, se informar um menor).
fn store_max_tokens(base_url: &str, model: &str, max: u32, learned: bool) {
    if max < MIN_THINKING_BUDGET {
        return;
    }
    let key = (base_url.trim_end_matches('/').to_string(), model.to_string());
    let mut limits = known_limits().lock().unwrap_or_else(PoisonError::into_inner);
    let next = match limits.get(&key) {
        Some(old) if old.learned && !learned => Known { max: old.max.min(max), learned: true },
        _ => Known { max, learned },
    };
    limits.insert(key, next);
}

/// Guarda o `max_tokens` do `model` no endereço `base_url` vindo da lista de modelos (T08). Não
/// aumenta um limite aprendido por recusa ([`learn_max_tokens`]).
pub fn remember_max_tokens(base_url: &str, model: &str, max: u32) {
    store_max_tokens(base_url, model, max, false);
}

/// Guarda o `max_tokens` do `model` no endereço `base_url` aprendido por um 400 que disse o limite.
pub fn learn_max_tokens(base_url: &str, model: &str, max: u32) {
    store_max_tokens(base_url, model, max, true);
}

/// `max_tokens` conhecido do `model` no endereço `base_url`.
pub fn known_max_tokens(base_url: &str, model: &str) -> Option<u32> {
    let key = (base_url.trim_end_matches('/').to_string(), model.to_string());
    known_limits().lock().unwrap_or_else(PoisonError::into_inner).get(&key).map(|k| k.max)
}

/// Pares (endereço, modelo) em que a busca com filtragem dinâmica foi recusada (por um 400 ou pela
/// entrada inválida do modelo): os pedidos seguintes já começam pela básica. Aprendido, não tabela.
fn basic_web_models() -> &'static Mutex<HashSet<(String, String)>> {
    static MODELS: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    MODELS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Guarda que o `model` em `base_url` usa a busca básica.
pub fn remember_basic_web(base_url: &str, model: &str) {
    let key = (base_url.trim_end_matches('/').to_string(), model.to_string());
    basic_web_models().lock().unwrap_or_else(PoisonError::into_inner).insert(key);
}

/// O `model` em `base_url` já recusou a busca dinâmica?
pub fn uses_basic_web(base_url: &str, model: &str) -> bool {
    let key = (base_url.trim_end_matches('/').to_string(), model.to_string());
    basic_web_models().lock().unwrap_or_else(PoisonError::into_inner).contains(&key)
}

/// A busca dinâmica falhou pela entrada: o modelo chamou `web_search` com uma entrada que o servidor
/// recusa (`invalid_tool_input`, visto ao vivo com `claude-haiku-5-5` em 2026-10-27), sem nenhuma
/// busca com resultado. O HTTP é 200; o adaptador trata como a recusa da dinâmica.
pub(super) fn web_input_refused(content: &[Value]) -> bool {
    let results = || content.iter().filter(|b| b["type"] == "web_search_tool_result");
    results().all(|b| !b["content"].is_array())
        && results().any(|b| b["content"]["error_code"] == WEB_INPUT_INVALID)
}

/// `error_code` da entrada recusada pela busca.
const WEB_INPUT_INVALID: &str = "invalid_tool_input";

/// `finish_reason` interno do stream interrompido por [`web_input_refused`]: a resposta só leva o
/// uso já gasto, e o pedido volta com a busca básica.
const WEB_INPUT_STOP: &str = "web_input_refused";

/// Aviso de nova tentativa (spec 132 T11) quando a busca dinâmica é descartada no stream pelo
/// [`WEB_INPUT_STOP`]: o que já apareceu na Atividade é substituído antes de o pedido voltar com a
/// busca básica. A tela mostra o texto do código `provider.web_search_retry` no idioma dela
/// ([`crate::agents::activity::notice_code`]); este texto é só o valor de reserva.
pub const WEB_SEARCH_RETRY_NOTICE: &str = "a busca na web falhou; repetindo com a busca simples";

/// Degrau da escada do thinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Thinking {
    Adaptive,
    Enabled,
    Off,
}

/// Degrau da escada da busca web (spec 132 T05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Web {
    /// `web_search_20260209`.
    Dynamic,
    /// `web_search_20250305`.
    Basic,
    Off,
}

/// Campos relaxáveis de um pedido. Cada recusa relaxa uma vez.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Opts {
    pub thinking: Thinking,
    pub web: Web,
    /// `max_uses` da busca (o que resta do limite do Arquiteto).
    max_uses: Option<u32>,
    /// O pedido pediu a busca.
    web_requested: bool,
    /// O histórico tem blocos da busca web.
    history_web: bool,
    /// Busca declarada só porque o histórico a usa (depois de um 400 que a cite, com o pedido sem
    /// a busca). A API aceita hoje o histórico sem a ferramenta; este degrau é a reserva.
    pub history_tool: bool,
    /// `output_config.effort` no corpo.
    pub output_effort: bool,
    /// `display: summarized` no thinking (o texto do raciocínio para a tela).
    pub display: bool,
    pub max_tokens: u32,
    /// O `max_tokens` já foi reduzido por um 400.
    pub reduced: bool,
    /// A redução veio do limite de saída lido no 400 (só esse é memorizado).
    pub learned: bool,
    effort: Option<Effort>,
}

impl Opts {
    pub fn new(req: &ChatRequest, max_tokens: u32, basic_web: bool) -> Self {
        let thinking = if req.effort.is_some() { Thinking::Adaptive } else { Thinking::Off };
        let web_requested = req.web_search && req.web_search_max_uses != Some(0);
        let history_web = req
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .flat_map(|m| m.provider_items.iter().flatten())
            .any(is_web_block);
        Self {
            thinking,
            web: match (web_requested, basic_web) {
                (false, _) => Web::Off,
                (true, false) => Web::Dynamic,
                (true, true) => Web::Basic,
            },
            max_uses: req.web_search_max_uses,
            web_requested,
            history_web,
            history_tool: false,
            output_effort: req.effort.is_some(),
            display: true,
            max_tokens,
            reduced: false,
            learned: false,
            effort: req.effort,
        }
    }

    /// A dinâmica recusada pela entrada inválida do modelo ([`web_input_refused`]): desce para a
    /// básica. `false` fora da dinâmica.
    fn relax_web_input(&mut self) -> bool {
        if self.web != Web::Dynamic {
            return false;
        }
        self.web = Web::Basic;
        true
    }

    /// O degrau para a continuação de um `pause_turn` depois de `used` buscas: o `max_uses` vale para
    /// a chamada inteira, então a continuação leva só o que resta, e sem a busca quando nada resta (a
    /// API aceita o histórico com os blocos da busca sem a ferramenta). Não é uma recusa.
    fn spent(&self, used: u32) -> Self {
        let mut opts = *self;
        if used == 0 {
            return opts;
        }
        if let Some(max) = self.max_uses {
            let left = max.saturating_sub(used);
            opts.max_uses = Some(left);
            if left == 0 && self.web != Web::Off {
                opts.web = Web::Off;
            }
        }
        opts
    }

    /// `budget_tokens` do thinking `enabled`, quando cabe abaixo do `max_tokens`.
    fn budget(&self) -> Option<u32> {
        let want = match self.effort? {
            Effort::Low => 2_048,
            Effort::Medium => 8_192,
            Effort::High => 16_384,
        };
        let budget = want.min(self.max_tokens / 2).max(MIN_THINKING_BUDGET);
        (budget < self.max_tokens).then_some(budget)
    }

    /// Relaxa o campo que o 400 `error_body` recusou. `false` quando não há o que relaxar (o erro
    /// sobe). Ordem: o `budget_tokens` do thinking `enabled`, o `max_tokens`, o thinking e o
    /// `output_config`. O 400 que cita o contexto ("input length and `max_tokens` exceed context
    /// limit") é estouro do contexto, não o limite de saída do modelo: não reduz nada e sobe, para o
    /// resumo do histórico (spec 122) tratar.
    pub fn relax(&mut self, error_body: &str) -> bool {
        let lower = error_body.to_ascii_lowercase();
        if lower.contains("web_search") || lower.contains("web search") {
            match self.web {
                Web::Dynamic => {
                    self.web = Web::Basic;
                    return true;
                }
                Web::Basic => {
                    self.web = Web::Off;
                    return true;
                }
                Web::Off if !self.web_requested && self.history_web && !self.history_tool => {
                    self.history_tool = true;
                    return true;
                }
                Web::Off => {}
            }
        }
        if self.thinking == Thinking::Enabled && lower.contains("budget_tokens") {
            self.thinking = Thinking::Off;
            return true;
        }
        if self.display && self.thinking != Thinking::Off && lower.contains("display") {
            self.display = false;
            return true;
        }
        if lower.contains("context") {
            return false;
        }
        if !self.reduced && lower.contains("max_tokens") {
            if let Some((max, learned)) = reduced_max_tokens(&lower, self.max_tokens) {
                self.max_tokens = max;
                self.reduced = true;
                self.learned = learned;
                return true;
            }
        }
        if lower.contains("thinking") || lower.contains("adaptive") {
            match self.thinking {
                Thinking::Adaptive => {
                    self.thinking = if self.budget().is_some() { Thinking::Enabled } else { Thinking::Off };
                    return true;
                }
                Thinking::Enabled => {
                    self.thinking = Thinking::Off;
                    return true;
                }
                Thinking::Off => {}
            }
        }
        if self.output_effort && (lower.contains("output_config") || lower.contains("effort")) {
            self.output_effort = false;
            return true;
        }
        false
    }
}

/// O `max_tokens` menor que o 400 indica: o maior número do texto entre [`MIN_THINKING_BUDGET`] e o
/// valor atual (ex.: `max_tokens: 32000 > 8192, which is the maximum ...`), com `true` (é o limite
/// do modelo, que vale memorizar). Sem número útil, [`FALLBACK_MAX_TOKENS`] se for menor que o
/// atual, com `false` (vale só para este pedido).
pub(super) fn reduced_max_tokens(error_body: &str, current: u32) -> Option<(u32, bool)> {
    let numbers = error_body
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse::<u32>().ok())
        .filter(|n| (MIN_THINKING_BUDGET..current).contains(n));
    numbers
        .max()
        .map(|n| (n, true))
        .or_else(|| (FALLBACK_MAX_TOKENS < current).then_some((FALLBACK_MAX_TOKENS, false)))
}

fn effort_str(effort: Effort) -> &'static str {
    super::effort_str(effort)
}

/// Bloco de texto.
fn text_block(text: String) -> Value {
    json!({ "type": "text", "text": text })
}

/// Acrescenta `blocks` à última mensagem se ela for do mesmo papel; senão abre uma mensagem nova.
fn push(out: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut().filter(|m| m["role"] == role) {
        if let Some(content) = last["content"].as_array_mut() {
            content.extend(blocks);
            return;
        }
    }
    out.push(json!({ "role": role, "content": blocks }));
}

/// Blocos de uma mensagem do assistente: os de `provider_items` inalterados quando todos são
/// blocos da Anthropic (sem os textos vazios, que a API recusa); senão, remontados do texto e das
/// chamadas.
fn assistant_blocks(msg: &ChatMessage) -> Vec<Value> {
    if let Some(items) = msg.provider_items.as_ref().filter(|i| !i.is_empty() && i.iter().all(is_block)) {
        return items
            .iter()
            .filter(|b| !(b["type"] == "text" && b["text"].as_str().is_none_or(str::is_empty)))
            .cloned()
            .collect();
    }
    let mut blocks = Vec::new();
    if !msg.content.is_empty() {
        blocks.push(text_block(msg.content.clone()));
    }
    for call in &msg.tool_calls {
        blocks.push(json!({ "type": "tool_use", "id": call.id, "name": call.name, "input": tool_input(call) }));
    }
    blocks
}

/// `input` de um `tool_use` remontado: os argumentos quando são um objeto JSON; senão, objeto vazio
/// (a API exige objeto).
fn tool_input(call: &ToolCall) -> Value {
    match serde_json::from_str::<Value>(&call.arguments) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({}),
    }
}

/// Blocos de uma mensagem `user`: o texto (quando houver) e as imagens em base64.
fn user_blocks(msg: &ChatMessage) -> Vec<Value> {
    let mut blocks = Vec::new();
    if !msg.content.is_empty() {
        blocks.push(text_block(msg.content.clone()));
    }
    for image in &msg.images {
        blocks.push(json!({
            "type": "image",
            "source": { "type": "base64", "media_type": image.media_type, "data": super::base64_encode(&image.data) },
        }));
    }
    blocks
}

fn tool_json(tool: &ToolSpec) -> Value {
    json!({ "name": tool.name, "description": tool.description, "input_schema": tool.parameters })
}

/// Nomes das ferramentas usadas no histórico (chamadas e blocos `tool_use`), na ordem, sem repetir.
fn past_tool_names(messages: &[Value]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for msg in messages {
        for block in msg["content"].as_array().into_iter().flatten() {
            if block["type"] == "tool_use" {
                if let Some(name) = block["name"].as_str().filter(|n| !names.iter().any(|x| x == n)) {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

/// Corpo de `POST /v1/messages` para o degrau `opts` da escada.
pub(super) fn build_body(req: &ChatRequest, opts: &Opts) -> Result<Value, LlmError> {
    let mut messages: Vec<Value> = Vec::new();
    let mut system: Vec<Value> = Vec::new();
    for msg in &req.messages {
        match msg.role {
            Role::System => {
                if !msg.content.is_empty() {
                    system.push(text_block(msg.content.clone()));
                }
            }
            Role::User => push(&mut messages, "user", user_blocks(msg)),
            Role::Tool => {
                let id = msg.tool_call_id.as_deref().ok_or_else(|| LlmError::Config(TOOL_RESULT_WITHOUT_ID.into()))?;
                let mut block = json!({ "type": "tool_result", "tool_use_id": id });
                if !msg.content.is_empty() {
                    block["content"] = Value::String(msg.content.clone());
                }
                push(&mut messages, "user", vec![block]);
            }
            Role::Assistant => push(&mut messages, "assistant", assistant_blocks(msg)),
        }
    }
    // Os `tool_result` vêm antes de qualquer texto na mensagem `user` (exigência da API).
    for msg in messages.iter_mut().filter(|m| m["role"] == "user") {
        if let Some(content) = msg["content"].as_array_mut() {
            content.sort_by_key(|b| b["type"] != "tool_result");
        }
    }
    if req.response_json && !mentions_json(req) {
        append_json_hint(&mut messages);
    }
    let mut tools: Vec<Value> = req.tools.iter().map(tool_json).collect();
    if tools.is_empty() {
        tools = past_tool_names(&messages)
            .into_iter()
            .map(|name| json!({ "name": name, "description": PAST_TOOL_DESCRIPTION, "input_schema": { "type": "object" } }))
            .collect();
    }
    if let Some(web) = web_tool(opts) {
        tools.insert(0, web);
    }
    let mut body = json!({ "model": req.model, "max_tokens": opts.max_tokens, "messages": messages });
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    match opts.thinking {
        Thinking::Adaptive => body["thinking"] = json!({ "type": "adaptive" }),
        Thinking::Enabled => {
            if let Some(budget) = opts.budget() {
                body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            }
        }
        Thinking::Off => {}
    }
    if opts.display {
        if let Some(thinking) = body.get_mut("thinking").and_then(Value::as_object_mut) {
            thinking.insert("display".into(), json!("summarized"));
        }
    }
    if let Some(effort) = req.effort.filter(|_| opts.output_effort) {
        body["output_config"] = json!({ "effort": effort_str(effort) });
    }
    add_cache_control(&mut body);
    Ok(body)
}

/// A ferramenta de busca web do degrau `opts`: a pedida, com o `max_uses`; ou, só para o histórico
/// que a usa, a básica com uma busca.
fn web_tool(opts: &Opts) -> Option<Value> {
    let (kind, max_uses) = match opts.web {
        Web::Dynamic => (WEB_SEARCH_DYNAMIC, opts.max_uses),
        Web::Basic => (WEB_SEARCH_BASIC, opts.max_uses),
        Web::Off if opts.history_tool => (WEB_SEARCH_BASIC, Some(1)),
        Web::Off => return None,
    };
    let mut tool = json!({ "type": kind, "name": WEB_SEARCH_NAME });
    if let Some(n) = max_uses {
        tool["max_uses"] = json!(n);
    }
    Some(tool)
}

/// Alguma mensagem cita "json", sem diferenciar maiúsculas?
fn mentions_json(req: &ChatRequest) -> bool {
    req.messages.iter().any(|m| m.content.to_ascii_lowercase().contains("json"))
}

/// Acrescenta [`RESPONSES_JSON_HINT`] como bloco de texto da última mensagem `user` (ou numa nova).
fn append_json_hint(messages: &mut Vec<Value>) {
    push(messages, "user", vec![text_block(RESPONSES_JSON_HINT.to_string())]);
}

/// `cache_control: ephemeral` nas âncoras estáveis: o último bloco do `system`, a última ferramenta
/// e o último bloco da última mensagem `user`. No máximo [`MAX_CACHE_BREAKPOINTS`].
fn add_cache_control(body: &mut Value) {
    let mut used = 0usize;
    let mut mark = |block: Option<&mut Value>| {
        if let Some(obj) = block.and_then(Value::as_object_mut).filter(|_| used < MAX_CACHE_BREAKPOINTS) {
            obj.insert("cache_control".into(), json!({ "type": "ephemeral" }));
            used += 1;
        }
    };
    mark(body.get_mut("system").and_then(Value::as_array_mut).and_then(|s| s.last_mut()));
    mark(body.get_mut("tools").and_then(Value::as_array_mut).and_then(|t| t.last_mut()));
    let last_user = body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .and_then(|m| m.iter_mut().rev().find(|m| m["role"] == "user"));
    mark(last_user.and_then(|m| m.get_mut("content")).and_then(Value::as_array_mut).and_then(|c| c.last_mut()));
}

/// Lê `u32` saturado de `v[key]`.
fn tokens(v: &Value, key: &str) -> Option<u32> {
    v[key].as_u64().map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

/// Uso da Anthropic: a entrada é `input_tokens + cache_read_input_tokens +
/// cache_creation_input_tokens` (os três são disjuntos); `cached` é a leitura e `cache_write` a
/// escrita do cache. O thinking vem do detalhe de saída, quando informado.
pub(super) fn usage(v: &Value) -> Option<Usage> {
    if !v.is_object() {
        return None;
    }
    let read = tokens(v, "cache_read_input_tokens");
    let write = tokens(v, "cache_creation_input_tokens");
    let prompt = tokens(v, "input_tokens")
        .unwrap_or(0)
        .saturating_add(read.unwrap_or(0))
        .saturating_add(write.unwrap_or(0));
    let details = &v["output_tokens_details"];
    let reasoning = tokens(details, "thinking_tokens").or_else(|| tokens(details, "reasoning_tokens"));
    Some(Usage { prompt, completion: tokens(v, "output_tokens").unwrap_or(0), cached: read, reasoning, cache_write: write })
}

/// Com `response_json`, o texto sem a cerca de código em volta do JSON (```` ```json ... ``` ````),
/// que a Anthropic costuma acrescentar. A validação fica com quem lê o JSON.
fn unwrap_json_fence(text: &str) -> Option<String> {
    let inner = text.trim().strip_prefix("```")?.strip_suffix("```")?;
    let (first, rest) = inner.split_once('\n')?;
    let lang = first.trim();
    (lang.is_empty() || lang.eq_ignore_ascii_case("json")).then(|| rest.trim().to_string())
}

/// Interpreta a resposta de `POST /v1/messages`. Erros com motivo fixo: nunca ecoam o corpo.
pub(super) fn parse(body: &str, fallback_used: bool, response_json: bool) -> Result<ChatResponse, LlmError> {
    let v: Value = serde_json::from_str(body).map_err(|_| LlmError::Parse(BadReason::new(BadKind::InvalidJson)))?;
    if v["type"] == "error" || v.get("error").is_some_and(|e| !e.is_null()) {
        return Err(LlmError::Parse(BadReason::provider_error(&v)));
    }
    let Some(content) = v["content"].as_array() else {
        return Err(LlmError::Parse(BadReason::new(BadKind::InvalidJson)));
    };
    let paused = v["stop_reason"] == PAUSE_TURN;
    let resp = response_of(content.clone(), v["stop_reason"].as_str(), usage(&v["usage"]), fallback_used, response_json)?;
    Ok(ChatResponse {
        web_search_calls: tokens(&v["usage"]["server_tool_use"], "web_search_requests").unwrap_or(0),
        finish_reason: if paused && resp.message.tool_calls.is_empty() { Some(PAUSE_TURN.into()) } else { resp.finish_reason },
        ..resp
    })
}

/// Texto, chamadas e fontes dos blocos de `content`. Blocos de texto seguidos são um texto só (a
/// resposta com citações vem partida neles); os separados por outros blocos são juntados como
/// mensagens. As fontes são as `citations` do texto, sem repetir o endereço.
fn read_blocks(content: &[Value]) -> (String, Vec<ToolCall>, Vec<Citation>) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut citations: Vec<Citation> = Vec::new();
    let mut after_text = false;
    for block in content {
        match block["type"].as_str().unwrap_or_default() {
            "text" => {
                let piece = block["text"].as_str().unwrap_or_default();
                if after_text {
                    text.push_str(piece);
                } else {
                    join_message(&mut text, piece);
                }
                after_text = after_text || !piece.is_empty();
                for c in block["citations"].as_array().into_iter().flatten() {
                    let url = c["url"].as_str().unwrap_or_default();
                    if !url.is_empty() && !citations.iter().any(|x| x.url == url) {
                        let title = c["title"].as_str().unwrap_or_default().to_string();
                        citations.push(Citation { url: url.to_string(), title });
                    }
                }
                continue;
            }
            "tool_use" => tool_calls.push(ToolCall {
                id: block["id"].as_str().unwrap_or_default().to_string(),
                name: block["name"].as_str().unwrap_or_default().to_string(),
                arguments: match &block["input"] {
                    Value::Null => "{}".to_string(),
                    input => input.to_string(),
                },
            }),
            _ => {}
        }
        after_text = false;
    }
    // Sem citações no texto (a filtragem dinâmica costuma não citar), as fontes são os resultados
    // das buscas, na ordem em que vieram, sem repetir.
    if citations.is_empty() {
        let results = content
            .iter()
            .filter(|b| b["type"] == "web_search_tool_result")
            .flat_map(|b| b["content"].as_array().into_iter().flatten());
        for r in results.filter(|r| r["type"] == "web_search_result") {
            let url = r["url"].as_str().unwrap_or_default();
            if !url.is_empty() && !citations.iter().any(|x| x.url == url) {
                let title = r["title"].as_str().unwrap_or_default().to_string();
                citations.push(Citation { url: url.to_string(), title });
            }
        }
    }
    (text, tool_calls, citations)
}

/// A resposta montada dos blocos `content` com o `stop` final. Sem texto nem chamadas é erro, salvo
/// no `pause_turn` (o servidor ainda vai continuar).
fn response_of(
    content: Vec<Value>,
    stop: Option<&str>,
    usage: Option<Usage>,
    fallback_used: bool,
    response_json: bool,
) -> Result<ChatResponse, LlmError> {
    let (mut text, tool_calls, citations) = read_blocks(&content);
    if text.is_empty() && tool_calls.is_empty() && stop != Some(PAUSE_TURN) {
        return Err(LlmError::Parse(match stop {
            Some("refusal") => BadReason::with_code(BadKind::Incomplete, stop),
            _ => BadReason::new(BadKind::Empty),
        }));
    }
    if response_json {
        if let Some(inner) = unwrap_json_fence(&text) {
            text = inner;
        }
    }
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if stop == Some("max_tokens") {
        "length"
    } else {
        "stop"
    };
    Ok(ChatResponse {
        message: ChatMessage {
            role: Role::Assistant,
            content: text,
            tool_call_id: None,
            tool_calls,
            provider_items: Some(content),
            images: Vec::new(),
        },
        finish_reason: Some(finish_reason.into()),
        usage,
        fallback_used,
        web_search_calls: 0,
        citations,
        web_search_rejected: false,
        billing: Default::default(),
    })
}

/// A resposta parou num `pause_turn` (sem chamadas do cliente)?
pub(super) fn is_paused(resp: &ChatResponse) -> bool {
    resp.finish_reason.as_deref() == Some(PAUSE_TURN)
}

/// Soma de dois usos (os campos opcionais somam quando algum lado informa).
fn add_usage(a: Option<Usage>, b: Option<Usage>) -> Option<Usage> {
    let opt = |x: Option<u32>, y: Option<u32>| match (x, y) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0).saturating_add(y.unwrap_or(0))),
    };
    match (a, b) {
        (Some(a), Some(b)) => Some(Usage {
            prompt: a.prompt.saturating_add(b.prompt),
            completion: a.completion.saturating_add(b.completion),
            cached: opt(a.cached, b.cached),
            reasoning: opt(a.reasoning, b.reasoning),
            cache_write: opt(a.cache_write, b.cache_write),
        }),
        (a, b) => a.or(b),
    }
}

/// Junta a continuação `next` de um `pause_turn` ao que veio antes: os blocos em ordem (o texto e as
/// fontes relidos deles), o uso e as buscas somados, o fim da última.
pub(super) fn join_paused(prev: ChatResponse, next: ChatResponse, response_json: bool) -> Result<ChatResponse, LlmError> {
    let paused = is_paused(&next);
    let mut content = prev.message.provider_items.unwrap_or_default();
    content.extend(next.message.provider_items.unwrap_or_default());
    let stop = match next.finish_reason.as_deref() {
        Some("length") => Some("max_tokens"),
        _ if paused => Some(PAUSE_TURN),
        _ => None,
    };
    let fallback_used = prev.fallback_used || next.fallback_used;
    let resp = response_of(content, stop, add_usage(prev.usage, next.usage), fallback_used, response_json)?;
    Ok(ChatResponse {
        web_search_calls: prev.web_search_calls.saturating_add(next.web_search_calls),
        finish_reason: if paused && resp.message.tool_calls.is_empty() { Some(PAUSE_TURN.into()) } else { resp.finish_reason },
        ..resp
    })
}

/// O fim da chamada: o `pause_turn` que passou de [`MAX_CONTINUATIONS`] vira o fim comum quando já
/// há texto ou chamadas; sem nada, a resposta ficou incompleta.
pub(super) fn settle_paused(mut resp: ChatResponse) -> Result<ChatResponse, LlmError> {
    if !is_paused(&resp) {
        return Ok(resp);
    }
    if resp.message.content.is_empty() && resp.message.tool_calls.is_empty() {
        return Err(LlmError::Parse(BadReason::with_code(BadKind::Incomplete, Some(PAUSE_TURN))));
    }
    resp.finish_reason = Some("stop".into());
    Ok(resp)
}

/// O corpo da continuação de um `pause_turn`: o mesmo pedido com o conteúdo já recebido como a
/// mensagem `assistant` do fim (juntado à última, se ela já for do assistente), sem mensagem nova do
/// usuário.
pub(super) fn continue_body(body: &mut Value, prev: &ChatResponse) {
    let blocks: Vec<Value> = prev
        .message
        .provider_items
        .iter()
        .flatten()
        .filter(|b| !(b["type"] == "text" && b["text"].as_str().is_none_or(str::is_empty)))
        .cloned()
        .collect();
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        push(messages, "assistant", blocks);
    }
}

/// Troca os argumentos da chamada `id` no bloco `tool_use` guardado (compactação do histórico,
/// spec 122) e, se algum mudou, descarta os blocos de raciocínio da mensagem: a assinatura deles
/// vale para o turno original. `true` quando algo mudou.
pub fn rewrite_tool_use(items: &mut Vec<Value>, id: &str, arguments: &str) -> bool {
    let Ok(input @ Value::Object(_)) = serde_json::from_str::<Value>(arguments) else {
        return false;
    };
    let mut changed = false;
    for item in items.iter_mut().filter(|i| i["type"] == "tool_use" && i["id"] == id) {
        if let Some(obj) = item.as_object_mut() {
            obj.insert("input".into(), input.clone());
            changed = true;
        }
    }
    if changed {
        items.retain(|i| !is_thinking(i));
    }
    changed
}

/// Acumulador do stream da Messages API: os blocos por `index`, o JSON parcial dos `tool_use`, o
/// uso (o de `message_start` com os totais de `message_delta`) e o `stop_reason`.
#[derive(Default)]
pub(super) struct StreamAcc {
    blocks: BTreeMap<u64, Value>,
    /// `index` -> JSON parcial do `input` do `tool_use` (`input_json_delta`).
    partial: BTreeMap<u64, String>,
    usage: serde_json::Map<String, Value>,
    stop_reason: Option<Value>,
    response_json: bool,
    done: bool,
    /// Interromper na entrada inválida da busca dinâmica ([`web_input_refused`]).
    web_guard: bool,
    /// Já chegou um resultado de busca com resultados.
    web_ok: bool,
    /// O stream foi interrompido pela entrada inválida da busca.
    web_refused: bool,
}

/// Acrescenta `piece` ao texto de `block[key]`.
fn append(block: &mut Value, key: &str, piece: &str) {
    if let Some(obj) = block.as_object_mut() {
        let mut text = obj.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        text.push_str(piece);
        obj.insert(key.into(), Value::String(text));
    }
}

/// Junta ao uso acumulado os campos informados em `v`: os não nulos substituem os anteriores (o
/// `message_delta` traz os totais).
fn merge_usage(acc: &mut serde_json::Map<String, Value>, v: &Value) {
    for (k, val) in v.as_object().into_iter().flatten().filter(|(_, val)| !val.is_null()) {
        acc.insert(k.clone(), val.clone());
    }
}

/// Uso conhecido até o evento `kind` do stream (para o diagnóstico de corte da spec 114): o de
/// `message_start` com os totais de `message_delta`, acumulados em `raw`. `None` nos outros eventos.
pub(super) fn stream_usage(raw: &mut serde_json::Map<String, Value>, kind: &str, v: &Value) -> Option<Usage> {
    match kind {
        "message_start" => merge_usage(raw, &v["message"]["usage"]),
        "message_delta" => merge_usage(raw, &v["usage"]),
        _ => return None,
    }
    usage(&Value::Object(raw.clone()))
}

impl StreamAcc {
    pub fn new(response_json: bool) -> Self {
        Self { response_json, ..Self::default() }
    }

    /// Com a busca dinâmica no pedido: o primeiro `web_search_tool_result` com `invalid_tool_input`,
    /// antes de qualquer busca com resultado, encerra o stream (o pedido volta com a básica).
    pub fn with_web_guard(mut self, on: bool) -> Self {
        self.web_guard = on;
        self
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Fecha o `input` do `tool_use` em `index` com o JSON acumulado. Sem JSON, fica o `input` do
    /// `content_block_start`; JSON inválido (resposta cortada) vira objeto vazio, como a API exige.
    fn close_input(&mut self, index: u64) {
        let Some(raw) = self.partial.remove(&index) else {
            return;
        };
        let Some(block) = self.blocks.get_mut(&index).and_then(Value::as_object_mut) else {
            return;
        };
        let input = match serde_json::from_str::<Value>(&raw) {
            Ok(v @ Value::Object(_)) => v,
            _ if raw.trim().is_empty() => return,
            _ => json!({}),
        };
        block.insert("input".into(), input);
    }

    /// Processa um evento; `true` quando o fluxo terminou (`message_stop`).
    pub fn event(&mut self, name: &str, data: &str, on: &(dyn Fn(StreamDelta) + Send + Sync)) -> Result<bool, LlmError> {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        let index = v["index"].as_u64().unwrap_or(0);
        match v["type"].as_str().unwrap_or(name) {
            "message_start" => merge_usage(&mut self.usage, &v["message"]["usage"]),
            "content_block_start" => {
                if v["content_block"].is_object() {
                    // O que chegou antes do start (delta fora de ordem) é juntado ao bloco, não
                    // apagado: o texto já foi mostrado na tela. O JSON parcial fica em `partial`.
                    let mut block = v["content_block"].clone();
                    if let Some(early) = self.blocks.remove(&index) {
                        for key in ["text", "thinking", "signature"] {
                            if let Some(piece) = early[key].as_str() {
                                append(&mut block, key, piece);
                            }
                        }
                        if let Some(early_citations) = early["citations"].as_array() {
                            if let Some(obj) = block.as_object_mut() {
                                let list = obj.entry("citations").or_insert_with(|| json!([]));
                                if !list.is_array() {
                                    *list = json!([]);
                                }
                                if let Some(list) = list.as_array_mut() {
                                    list.extend(early_citations.iter().cloned());
                                }
                            }
                        }
                    }
                    let refused = self.web_guard && !self.web_ok && block["type"] == "web_search_tool_result" && {
                        self.web_ok = block["content"].is_array();
                        block["content"]["error_code"] == WEB_INPUT_INVALID
                    };
                    self.blocks.insert(index, block);
                    if refused {
                        self.web_refused = true;
                        self.done = true;
                        return Ok(true);
                    }
                }
            }
            "content_block_delta" => {
                let delta = &v["delta"];
                let piece = |key: &str| delta[key].as_str().unwrap_or_default();
                let block = self.blocks.entry(index).or_insert_with(|| json!({}));
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        append(block, "text", piece("text"));
                        emit(on, StreamDelta::Text(piece("text").to_string()));
                    }
                    "thinking_delta" => {
                        append(block, "thinking", piece("thinking"));
                        emit(on, StreamDelta::Reasoning(piece("thinking").to_string()));
                    }
                    "signature_delta" => append(block, "signature", piece("signature")),
                    "input_json_delta" => self.partial.entry(index).or_default().push_str(piece("partial_json")),
                    "citations_delta" => {
                        if let Some(obj) = block.as_object_mut() {
                            let list = obj.entry("citations").or_insert_with(|| json!([]));
                            if !list.is_array() {
                                *list = json!([]);
                            }
                            if let Some(list) = list.as_array_mut() {
                                list.push(delta["citation"].clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => self.close_input(index),
            "message_delta" => {
                if let Some(stop) = v["delta"].get("stop_reason").filter(|s| !s.is_null()) {
                    self.stop_reason = Some(stop.clone());
                }
                merge_usage(&mut self.usage, &v["usage"]);
            }
            "message_stop" => {
                self.done = true;
                return Ok(true);
            }
            "error" => {
                let error = if v["error"].is_object() { &v["error"] } else { &v };
                return Err(LlmError::Parse(BadReason::provider_error(error)));
            }
            _ => {}
        }
        Ok(false)
    }

    /// A mensagem montada, pelo mesmo [`parse`] da versão sem stream. Sem `message_stop` nem
    /// `stop_reason`, a resposta não terminou.
    pub fn finish(mut self, fallback_used: bool) -> Result<ChatResponse, LlmError> {
        if self.web_refused {
            let spent = usage(&Value::Object(std::mem::take(&mut self.usage)));
            let mut resp = response_of(Vec::new(), Some(PAUSE_TURN), spent, fallback_used, false)?;
            resp.finish_reason = Some(WEB_INPUT_STOP.into());
            return Ok(resp);
        }
        if !self.done && self.stop_reason.is_none() {
            return Err(LlmError::Parse(NO_FINAL));
        }
        let open: Vec<u64> = self.partial.keys().copied().collect();
        for index in open {
            self.close_input(index);
        }
        // Bloco sem `type` (delta sem o start) não pode voltar à API: seria recusado no reenvio.
        let content: Vec<Value> =
            std::mem::take(&mut self.blocks).into_values().filter(|b| b["type"].is_string()).collect();
        let message = json!({
            "type": "message",
            "role": "assistant",
            "content": content,
            "stop_reason": self.stop_reason.take().unwrap_or(Value::Null),
            "usage": Value::Object(std::mem::take(&mut self.usage)),
        });
        parse(&message.to_string(), fallback_used, self.response_json)
    }
}

impl LlmClient {
    /// Uma tentativa pela Messages API, sem stream, com a escada de relaxamento por 400 (cada recusa
    /// relaxa uma vez) e as continuações do `pause_turn` (até [`MAX_CONTINUATIONS`]).
    pub(super) async fn anthropic_messages(&self, req: &ChatRequest) -> Result<ChatResponse, Failed> {
        let known = known_max_tokens(&self.base_url, &req.model).unwrap_or(DEFAULT_MAX_TOKENS);
        let max_tokens = self.capped_max_tokens(&req.model, known);
        let mut opts = Opts::new(req, max_tokens, uses_basic_web(&self.base_url, &req.model));
        let mut prior: Option<ChatResponse> = None;
        let mut continuations = 0usize;
        // Uso do pedido descartado pela entrada inválida da busca dinâmica (somado ao final).
        let mut wasted: Option<Usage> = None;
        loop {
            let mut body = build_body(req, &opts.spent(prior.as_ref().map_or(0, |p| p.web_search_calls)))?;
            if let Some(prev) = &prior {
                continue_body(&mut body, prev);
            }
            let builder = self.http.post(format!("{}{MESSAGES_PATH}", self.base_url)).json(&body);
            let (status, text, after) = self.send_capped_with_headers(builder, Some(&req.model)).await?;
            if status.is_success() {
                if opts.learned {
                    learn_max_tokens(&self.base_url, &req.model, opts.max_tokens);
                }
                let fallback_used = req.effort.is_some() && opts.thinking == Thinking::Off;
                let resp = parse(&text, fallback_used, req.response_json)?;
                if prior.is_none()
                    && web_input_refused(resp.message.provider_items.as_deref().unwrap_or_default())
                    && opts.relax_web_input()
                {
                    wasted = add_usage(wasted, resp.usage);
                    remember_basic_web(&self.base_url, &req.model);
                    self.note_resend(&req.model);
                    continue;
                }
                let resp = match prior.take() {
                    Some(prev) => join_paused(prev, resp, req.response_json)?,
                    None => resp,
                };
                if is_paused(&resp) && continuations < MAX_CONTINUATIONS {
                    continuations += 1;
                    prior = Some(resp);
                    continue;
                }
                let mut resp = finish_web(settle_paused(resp)?, req, &opts);
                resp.usage = add_usage(wasted, resp.usage);
                return Ok(resp);
            }
            if status.as_u16() == 400 && self.relax_noting(&mut opts, &text, &req.model) {
                continue;
            }
            self.note_output_limit(&req.model, status.as_u16(), &text, opts.max_tokens);
            return Err(Failed { err: self.http_error(status.as_u16(), &text), retry_after: after });
        }
    }

    /// Relaxa o degrau pelo 400 `text` e, se a busca dinâmica caiu, guarda isso para o modelo.
    fn relax_noting(&self, opts: &mut Opts, text: &str, model: &str) -> bool {
        let was_dynamic = opts.web == Web::Dynamic;
        if !opts.relax(text) {
            return false;
        }
        if was_dynamic && opts.web != Web::Dynamic {
            remember_basic_web(&self.base_url, model);
        }
        self.note_resend(model);
        true
    }

    /// Uma tentativa pela Messages API em stream (o caminho do agente), com a mesma escada de
    /// relaxamento. O timeout comum vale como ociosidade (até os cabeçalhos e entre pedaços), com o
    /// teto [`STREAM_MAX_DURATION`] no total. A resposta cortada vira o diagnóstico da spec 114
    /// ([`StreamDelta::Cut`]), entregue antes do erro subir. Resposta 200 que não é SSE é lida como
    /// a da versão sem stream.
    pub(super) async fn anthropic_stream_once(
        &self,
        req: &ChatRequest,
        on_delta: &(dyn Fn(StreamDelta) + Send + Sync),
    ) -> Result<ChatResponse, Failed> {
        let known = known_max_tokens(&self.base_url, &req.model).unwrap_or(DEFAULT_MAX_TOKENS);
        let max_tokens = self.capped_max_tokens(&req.model, known);
        let mut opts = Opts::new(req, max_tokens, uses_basic_web(&self.base_url, &req.model));
        let prompt_estimate = self.estimate_tokens(req);
        let mut prior: Option<ChatResponse> = None;
        let mut continuations = 0usize;
        // Uso do pedido descartado pela entrada inválida da busca dinâmica (somado ao final).
        let mut wasted: Option<Usage> = None;
        loop {
            let mut body = build_body(req, &opts.spent(prior.as_ref().map_or(0, |p| p.web_search_calls)))?;
            if let Some(prev) = &prior {
                continue_body(&mut body, prev);
            }
            body["stream"] = json!(true);
            let token = self.bearer().await.map_err(Failed::from)?;
            let builder = self
                .http
                .post(format!("{}{MESSAGES_PATH}", self.base_url))
                .timeout(STREAM_MAX_DURATION.max(self.timeout))
                .json(&body);
            let mut diag = StreamDiag::new(prompt_estimate);
            let authorized = self.authorize(builder, &token).map_err(Failed::from)?;
            let mut resp = tokio::time::timeout(self.timeout, authorized.send())
                .await
                .map_err(|_| LlmError::Timeout)?
                .map_err(transport_error)?;
            let status = resp.status();
            self.observe_quota(&req.model, resp.headers());
            if !status.is_success() {
                let after = retry_after(resp.headers());
                let text = self.read_capped(&mut resp).await?;
                if status.as_u16() == 400 && self.relax_noting(&mut opts, &text, &req.model) {
                    continue;
                }
                self.note_output_limit(&req.model, status.as_u16(), &text, opts.max_tokens);
                return Err(Failed { err: self.http_error(status.as_u16(), &text), retry_after: after });
            }
            if opts.learned {
                learn_max_tokens(&self.base_url, &req.model, opts.max_tokens);
            }
            let fallback_used = req.effort.is_some() && opts.thinking == Thinking::Off;
            let ctype = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_ascii_lowercase);
            let mut first: Vec<u8> = Vec::new();
            let is_sse = if ctype.as_deref().is_some_and(|v| v.contains("text/event-stream")) {
                true
            } else if ctype.is_none() {
                first = self.read_until_content(&mut resp).await?;
                looks_like_sse(&first)
            } else {
                false
            };
            let out = if is_sse {
                let guard = opts.web == Web::Dynamic && prior.is_none();
                let acc = SseAcc::Anthropic(StreamAcc::new(req.response_json).with_web_guard(guard));
                let out = self.read_sse(resp, first, acc, fallback_used, on_delta, &mut diag).await;
                if let Some(r) = out.as_ref().ok().filter(|r| r.finish_reason.as_deref() == Some(WEB_INPUT_STOP)) {
                    if opts.relax_web_input() {
                        wasted = add_usage(wasted, r.usage);
                        remember_basic_web(&self.base_url, &req.model);
                        self.note_resend(&req.model);
                        // Os deltas já mostrados desta tentativa são descartados na tela.
                        on_delta(StreamDelta::Retry(WEB_SEARCH_RETRY_NOTICE.into()));
                        continue;
                    }
                }
                if let Err(e) = &out {
                    if let Some(mut cut) = diag.finish(e, &self.secret()) {
                        cut.billing = self.billing();
                        on_delta(StreamDelta::Cut(std::sync::Arc::new(cut)));
                    }
                }
                out
            } else {
                let text = self.read_capped_from(&mut resp, first).await?;
                let parsed = parse(&text, fallback_used, req.response_json);
                let refused = parsed.as_ref().ok().filter(|r| {
                    web_input_refused(r.message.provider_items.as_deref().unwrap_or_default())
                });
                if let Some(r) = refused.filter(|_| prior.is_none() && opts.relax_web_input()) {
                    wasted = add_usage(wasted, r.usage);
                    remember_basic_web(&self.base_url, &req.model);
                    self.note_resend(&req.model);
                    continue;
                }
                parsed
            };
            let resp = match prior.take() {
                Some(prev) => join_paused(prev, out?, req.response_json)?,
                None => out?,
            };
            if is_paused(&resp) && continuations < MAX_CONTINUATIONS {
                continuations += 1;
                prior = Some(resp);
                continue;
            }
            let mut resp = finish_web(settle_paused(resp)?, req, &opts);
            resp.usage = add_usage(wasted, resp.usage);
            resp.billing = self.billing();
            return Ok(resp);
        }
    }
}

/// Marca a busca recusada: o pedido a queria e a escada desceu até sem ela.
fn finish_web(mut resp: ChatResponse, req: &ChatRequest, opts: &Opts) -> ChatResponse {
    resp.web_search_rejected = req.web_search && opts.web_requested && opts.web == Web::Off;
    resp
}
