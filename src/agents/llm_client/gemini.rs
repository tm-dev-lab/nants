//! Adaptador nativo da Gemini Interactions API (spec 132 T06), com o streaming SSE e o
//! `google_search` (T07).
//!
//! `POST {endpoint}/v1beta/interactions`, com `x-goog-api-key` ([`super::auth_headers`]) e
//! `store: false` sempre (sem estado: o histórico inteiro vai em todo pedido). O corpo sai do
//! [`ChatRequest`]:
//! - as mensagens `system` viram o `system_instruction`;
//! - as do usuário viram passos `user_input`, com o texto e as imagens em base64;
//! - as do assistente voltam como os passos que o modelo devolveu ([`ChatMessage::provider_items`],
//!   inalterados) ou, sem eles, remontadas em `model_output` e `function_call`;
//! - os resultados de ferramenta viram passos `function_result` com o `call_id` e o `name` da
//!   chamada (a API recusa o nome vazio); o resultado vazio vai como texto vazio (aceito ao vivo em
//!   2026-10-28);
//! - o JSON vai por `response_format` (`application/json`, sem schema);
//! - `max_output_tokens` só vai quando o limite do modelo é conhecido ([`remember_max_tokens`],
//!   preenchido pela lista de modelos), reduzido uma vez se o 400 citar o campo.
//!
//! Esforço: `generation_config.thinking_level` com `thinking_summaries: auto` (o texto do raciocínio
//! para a tela). A escada de relaxamento por 400, sem tabela de versões, tira primeiro o
//! `thinking_summaries` recusado e depois o `thinking_level` (o padrão do modelo). A Interactions não
//! tem `thinking_budget`.
//!
//! Assinaturas: sem estado, os passos `thought` (com a `signature`) voltam exatamente como vieram, e
//! a API recusa um `function_call` sem o `thought` assinado antes dele (`invalid_request`, conferido
//! ao vivo em 2026-10-27). O turno remontado ou compactado (os `thought` descartados por
//! [`rewrite_function_call`]) leva antes da primeira chamada o `thought` com a assinatura de
//! contorno documentada pelo Google ([`SKIP_SIGNATURE`]).
//!
//! Uso (conferido ao vivo): `total_input_tokens` já inclui o `total_cached_tokens`, e
//! `total_output_tokens` não inclui o `total_thought_tokens`; o raciocínio é cobrado como saída,
//! então a saída é a soma dos dois.
//!
//! Streaming (T07): o caminho do agente pede `stream: true`; o [`StreamAcc`] remonta dos eventos
//! (`step.start`, `step.delta`, `step.stop`, `interaction.completed`) os mesmos passos da versão sem
//! stream, que passam pelo mesmo [`parse`]. O texto e o resumo do raciocínio chegam à tela como
//! deltas. Sem o `interaction.completed`, a resposta foi cortada (o diagnóstico da spec 114).
//!
//! Busca (T07): com `web_search`, `{"type":"google_search"}` vai em `tools`. A Interactions não tem
//! `max_uses`: o limite do Arquiteto é respeitado pela contagem do lado do Nants (o decorador do
//! Arquiteto desliga a busca quando o limite acaba, como na OpenAI). Os passos `google_search_call`
//! e `google_search_result` (com a `signature`) voltam inalterados. As fontes são as `annotations`
//! `url_citation` do texto. Sem nenhuma citação (o turno que termina numa chamada de função depois
//! da busca, spec 132 T12), as fontes são os itens do `google_search_result` que trazem `url`, na
//! ordem, sem repetir. Ao vivo, em 2026-10-28 (`gemini-3.8-flash`), o resultado só trouxe o widget
//! `search_suggestions`: um HTML com links de pesquisa do próprio Google para as consultas, que não
//! são fontes. Nesse caso o turno fica sem fontes, nada é inventado. A contagem é o número de `queries` dos `google_search_call` da resposta
//! completa (a busca é cobrada por consulta); o `grounding_tool_count` do uso não é somado, para não
//! contar em dobro. A resposta cortada não devolve passos: sobe como [`NO_FINAL`], com o diagnóstico
//! da spec 114, e as consultas dela não entram na contagem. Um 400 que cite a busca relaxa uma vez,
//! para sem busca.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};

use super::anthropic::{known_max_tokens as known_limit, learn_max_tokens as learn_limit, remember_max_tokens as remember_limit};
use super::cut::StreamDiag;
use super::{
    emit, join_message, looks_like_sse, retry_after, transport_error, Failed, LlmClient, SseAcc, StreamDelta, NO_FINAL,
    STREAM_MAX_DURATION, TOOL_RESULT_WITHOUT_ID,
};
use super::{BadKind, BadReason, ChatMessage, ChatRequest, ChatResponse, Citation, Role, ToolCall, ToolSpec, Usage};
use crate::agents::LlmError;
use crate::config::Effort;

/// Caminho da Interactions API, a partir do endereço da conexão.
pub const INTERACTIONS_PATH: &str = "/v1beta/interactions";
/// Assinatura de contorno para o `function_call` sem o `thought` original (turno remontado ou
/// compactado). Marcador do protocolo, documentado pelo Google; aceito ao vivo em 2026-10-27.
pub const SKIP_SIGNATURE: &str = "skip_thought_signature_validator";
/// Nome do `function_result` quando a chamada respondida não está no histórico (a API recusa o nome
/// vazio). Marcador do protocolo, não texto de pessoa.
const UNKNOWN_FUNCTION: &str = "function";
/// Tipo da ferramenta de busca e do passo da chamada dela.
const GOOGLE_SEARCH: &str = "google_search";
const GOOGLE_SEARCH_CALL: &str = "google_search_call";
const GOOGLE_SEARCH_RESULT: &str = "google_search_result";

/// Tipos de passo que o modelo devolve e que o adaptador reenvia de `provider_items`, além das
/// chamadas e dos resultados das ferramentas internas (`google_search_call`, ..., terminados em
/// `_call` e `_result` e com a `signature`, o que os separa dos itens da Responses).
const STEP_TYPES: [&str; 3] = ["thought", "model_output", "function_call"];

/// Passo da Interactions (o que o adaptador guarda em `provider_items`)? O `function_call` da
/// Responses tem `call_id`; o da Interactions, só `id`.
pub fn is_step(item: &Value) -> bool {
    let Some(kind) = item.get("type").and_then(Value::as_str) else {
        return false;
    };
    if kind == "function_call" {
        return item.get("call_id").is_none();
    }
    STEP_TYPES.contains(&kind)
        || (kind != "function_result" && (kind.ends_with("_call") || kind.ends_with("_result")) && item.get("signature").is_some())
}

/// Os itens são um turno da Interactions: todos passos e ao menos um `thought` ou `model_output`
/// (o `function_call` sozinho, sem `call_id`, também pode ser da Responses).
pub fn is_turn(items: &[Value]) -> bool {
    items.iter().all(is_step) && items.iter().any(|i| matches!(i["type"].as_str(), Some("thought" | "model_output")))
}

/// Passo de raciocínio (`thought`), que só volta inalterado.
fn is_thought(item: &Value) -> bool {
    item["type"] == "thought"
}

/// Guarda o `max_output_tokens` do `model` no endereço `base_url` (o da lista de modelos, T08, ou o
/// aprendido por um 400). O mesmo registro por (endereço, modelo) da Anthropic.
pub fn remember_max_tokens(base_url: &str, model: &str, max: u32) {
    remember_limit(base_url, model, max);
}

/// Guarda o `max_output_tokens` do `model` aprendido por um 400 que disse o limite: a lista de
/// modelos não o aumenta no mesmo processo (spec 132 T09).
pub fn learn_max_tokens(base_url: &str, model: &str, max: u32) {
    learn_limit(base_url, model, max);
}

/// `max_output_tokens` conhecido do `model` no endereço `base_url`.
pub fn known_max_tokens(base_url: &str, model: &str) -> Option<u32> {
    known_limit(base_url, model)
}

/// Campos relaxáveis de um pedido. Cada recusa relaxa uma vez.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Opts {
    /// `thinking_level` no corpo (só com esforço).
    pub thinking: bool,
    /// `thinking_summaries: auto` junto do `thinking_level`.
    pub summaries: bool,
    /// `max_output_tokens`, quando conhecido.
    pub max_tokens: Option<u32>,
    /// O `max_output_tokens` já foi reduzido por um 400.
    pub reduced: bool,
    /// A redução veio do limite lido no 400 (só esse é memorizado).
    pub learned: bool,
    /// `{"type":"google_search"}` em `tools`.
    pub web: bool,
    /// O pedido pediu a busca (com limite restante).
    web_requested: bool,
    effort: Option<Effort>,
}

impl Opts {
    pub fn new(req: &ChatRequest, max_tokens: Option<u32>) -> Self {
        let thinking = req.effort.is_some();
        let web_requested = req.web_search && req.web_search_max_uses != Some(0);
        Self {
            thinking,
            summaries: thinking,
            max_tokens,
            reduced: false,
            learned: false,
            web: web_requested,
            web_requested,
            effort: req.effort,
        }
    }

    /// Relaxa o campo que o 400 `error_body` recusou. `false` quando não há o que relaxar (o erro
    /// sobe). Ordem: o `thinking_summaries`, o `max_output_tokens` e o `thinking_level` (visto ao
    /// vivo: "Thinking level THINKING_LEVEL_MINIMAL is not supported for this model"). O 400 que
    /// cita o contexto é estouro, não limite de saída: sobe, para o resumo do histórico (spec 122).
    /// Antes de tudo, o 400 que cita a busca a tira (T07).
    pub fn relax(&mut self, error_body: &str) -> bool {
        let lower = error_body.to_ascii_lowercase();
        if self.web && ["google_search", "google search", "grounding"].iter().any(|k| lower.contains(k)) {
            self.web = false;
            return true;
        }
        if self.thinking && self.summaries && (lower.contains("thinking_summaries") || lower.contains("thinking summaries")) {
            self.summaries = false;
            return true;
        }
        if lower.contains("context") {
            return false;
        }
        if let Some(current) = self.max_tokens.filter(|_| !self.reduced && lower.contains("max_output_tokens")) {
            if let Some((max, learned)) = super::anthropic::reduced_max_tokens(&lower, current) {
                self.max_tokens = Some(max);
                self.reduced = true;
                self.learned = learned;
                return true;
            }
        }
        if self.thinking && lower.contains("thinking") {
            self.thinking = false;
            self.summaries = false;
            return true;
        }
        false
    }

    /// O pedido queria raciocínio e a escada desceu até sem ele.
    pub fn fallback_used(&self) -> bool {
        self.effort.is_some() && !self.thinking
    }

    /// O pedido queria a busca e a escada desceu até sem ela.
    pub fn web_rejected(&self) -> bool {
        self.web_requested && !self.web
    }
}

/// Parte de texto.
fn text_part(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// Conteúdo de uma mensagem do usuário: o texto (quando houver) e as imagens em base64.
fn user_content(msg: &ChatMessage) -> Vec<Value> {
    let mut parts = Vec::new();
    if !msg.content.is_empty() {
        parts.push(text_part(&msg.content));
    }
    for image in &msg.images {
        parts.push(json!({ "type": "image", "mime_type": image.media_type, "data": super::base64_encode(&image.data) }));
    }
    parts
}

/// Acrescenta `parts` ao último passo se ele for `user_input`; senão abre um passo novo.
fn push_user(input: &mut Vec<Value>, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = input.last_mut().filter(|s| s["type"] == "user_input") {
        if let Some(content) = last["content"].as_array_mut() {
            content.extend(parts);
            return;
        }
    }
    input.push(json!({ "type": "user_input", "content": parts }));
}

/// `arguments` de um `function_call` remontado: os argumentos quando são um objeto JSON; senão,
/// objeto vazio.
fn call_arguments(call: &ToolCall) -> Value {
    match serde_json::from_str::<Value>(&call.arguments) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({}),
    }
}

/// `model_output` sem nenhum texto (a API não precisa dele de volta).
fn is_empty_output(step: &Value) -> bool {
    step["type"] == "model_output"
        && step["content"].as_array().is_none_or(|parts| parts.iter().all(|p| p["text"].as_str().is_none_or(str::is_empty)))
}

/// Passos de uma mensagem do assistente: os de `provider_items` inalterados quando todos são passos
/// da Interactions (sem os `model_output` vazios); senão, remontados do texto e das chamadas. A
/// primeira chamada sem `thought` antes dela recebe o de contorno ([`SKIP_SIGNATURE`]).
fn assistant_steps(msg: &ChatMessage) -> Vec<Value> {
    let mut steps: Vec<Value> = match msg.provider_items.as_ref().filter(|i| !i.is_empty() && i.iter().all(is_step)) {
        Some(items) => items.iter().filter(|s| !is_empty_output(s)).cloned().collect(),
        None => {
            let mut steps = Vec::new();
            if !msg.content.is_empty() {
                steps.push(json!({ "type": "model_output", "content": [text_part(&msg.content)] }));
            }
            for call in &msg.tool_calls {
                steps.push(json!({ "type": "function_call", "id": call.id, "name": call.name, "arguments": call_arguments(call) }));
            }
            steps
        }
    };
    if let Some(first_call) = steps.iter().position(|s| s["type"] == "function_call") {
        if !steps[..first_call].iter().any(is_thought) {
            steps.insert(first_call, json!({ "type": "thought", "signature": SKIP_SIGNATURE }));
        }
    }
    steps
}

fn tool_json(tool: &ToolSpec) -> Value {
    json!({ "type": "function", "name": tool.name, "description": tool.description, "parameters": tool.parameters })
}

fn effort_str(effort: Effort) -> &'static str {
    super::effort_str(effort)
}

/// Corpo de `POST /v1beta/interactions` para o degrau `opts` da escada.
pub(super) fn build_body(req: &ChatRequest, opts: &Opts) -> Result<Value, LlmError> {
    let names: HashMap<&str, &str> = req
        .messages
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();
    let mut system: Vec<&str> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for msg in &req.messages {
        match msg.role {
            Role::System => {
                if !msg.content.is_empty() {
                    system.push(&msg.content);
                }
            }
            Role::User => push_user(&mut input, user_content(msg)),
            Role::Tool => {
                let id = msg.tool_call_id.as_deref().ok_or_else(|| LlmError::Config(TOOL_RESULT_WITHOUT_ID.into()))?;
                input.push(json!({
                    "type": "function_result",
                    "name": names.get(id).copied().unwrap_or(UNKNOWN_FUNCTION),
                    "call_id": id,
                    "result": { "content": [text_part(&msg.content)] },
                }));
            }
            Role::Assistant => input.extend(assistant_steps(msg)),
        }
    }
    let mut body = json!({ "model": req.model, "store": false, "input": input });
    if !system.is_empty() {
        body["system_instruction"] = Value::String(system.join("\n\n"));
    }
    let mut tools: Vec<Value> = req.tools.iter().map(tool_json).collect();
    if opts.web {
        tools.insert(0, json!({ "type": GOOGLE_SEARCH }));
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if req.response_json {
        body["response_format"] = json!({ "type": "text", "mime_type": "application/json" });
    }
    let mut config = serde_json::Map::new();
    if let Some(effort) = req.effort.filter(|_| opts.thinking) {
        config.insert("thinking_level".into(), json!(effort_str(effort)));
        if opts.summaries {
            config.insert("thinking_summaries".into(), json!("auto"));
        }
    }
    if let Some(max) = opts.max_tokens {
        config.insert("max_output_tokens".into(), json!(max));
    }
    if !config.is_empty() {
        body["generation_config"] = Value::Object(config);
    }
    Ok(body)
}

/// Lê `u32` saturado de `v[key]`.
fn tokens(v: &Value, key: &str) -> Option<u32> {
    v[key].as_u64().map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

/// Uso da Interactions: a entrada é `total_input_tokens` (já com o cache) mais o
/// `total_tool_use_tokens` (o que as ferramentas internas trazem, cobrado como entrada); a saída é
/// `total_output_tokens + total_thought_tokens` (disjuntos, conferido ao vivo pelo `total_tokens`).
pub(super) fn usage(v: &Value) -> Option<Usage> {
    if !v.is_object() {
        return None;
    }
    let thought = tokens(v, "total_thought_tokens");
    let prompt = tokens(v, "total_input_tokens").unwrap_or(0).saturating_add(tokens(v, "total_tool_use_tokens").unwrap_or(0));
    let completion = tokens(v, "total_output_tokens").unwrap_or(0).saturating_add(thought.unwrap_or(0));
    Some(Usage { prompt, completion, cached: tokens(v, "total_cached_tokens"), reasoning: thought, cache_write: None })
}

/// Texto, chamadas, fontes e consultas dos passos. Os textos de um mesmo `model_output` são um texto
/// só; os de passos separados são juntados como mensagens. As fontes são as `annotations`
/// `url_citation` do texto, sem repetir o endereço (sem nenhuma, as dos resultados da busca,
/// [`search_result_sources`]); as consultas, as `queries` de cada
/// `google_search_call` (a chamada sem a lista conta uma).
fn read_steps(steps: &[Value]) -> (String, Vec<ToolCall>, Vec<Citation>, u32) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut citations: Vec<Citation> = Vec::new();
    let mut queries = 0u32;
    let mut results: Vec<&Value> = Vec::new();
    for step in steps {
        match step["type"].as_str().unwrap_or_default() {
            "model_output" => {
                let parts: Vec<&Value> =
                    step["content"].as_array().into_iter().flatten().filter(|p| p["type"] == "text").collect();
                let piece: String = parts.iter().filter_map(|p| p["text"].as_str()).collect();
                join_message(&mut text, &piece);
                let annotations = parts.iter().flat_map(|p| p["annotations"].as_array().into_iter().flatten());
                for a in annotations.filter(|a| a["type"] == "url_citation") {
                    let url = a["url"].as_str().unwrap_or_default();
                    if !url.is_empty() && !citations.iter().any(|c| c.url == url) {
                        let title = a["title"].as_str().unwrap_or_default().to_string();
                        citations.push(Citation { url: url.to_string(), title });
                    }
                }
            }
            GOOGLE_SEARCH_RESULT => results.push(step),
            GOOGLE_SEARCH_CALL => {
                let n = step["arguments"]["queries"].as_array().map_or(1, Vec::len).max(1);
                queries = queries.saturating_add(u32::try_from(n).unwrap_or(u32::MAX));
            }
            "function_call" => tool_calls.push(ToolCall {
                id: step["id"].as_str().unwrap_or_default().to_string(),
                name: step["name"].as_str().unwrap_or_default().to_string(),
                arguments: match &step["arguments"] {
                    Value::Null => "{}".to_string(),
                    Value::String(s) => s.clone(),
                    args => args.to_string(),
                },
            }),
            _ => {}
        }
    }
    if citations.is_empty() {
        citations = search_result_sources(&results);
    }
    (text, tool_calls, citations, queries)
}

/// Fontes dos `google_search_result`, para o turno sem `url_citation`: os itens de `result` com
/// `url`, na ordem, sem repetir o endereço. O widget `search_suggestions` (só links de pesquisa do
/// Google) não tem `url` e não vira fonte.
fn search_result_sources(results: &[&Value]) -> Vec<Citation> {
    let mut citations: Vec<Citation> = Vec::new();
    let items = results.iter().flat_map(|r| r["result"].as_array().into_iter().flatten());
    for item in items {
        let url = item["url"].as_str().unwrap_or_default();
        if !url.is_empty() && !citations.iter().any(|c| c.url == url) {
            let title = item["title"].as_str().unwrap_or_default().to_string();
            citations.push(Citation { url: url.to_string(), title });
        }
    }
    citations
}

/// Interpreta a resposta de `POST /v1beta/interactions`. Erros com motivo fixo: nunca ecoam o corpo.
pub(super) fn parse(body: &str, fallback_used: bool) -> Result<ChatResponse, LlmError> {
    let v: Value = serde_json::from_str(body).map_err(|_| LlmError::Parse(BadReason::new(BadKind::InvalidJson)))?;
    parse_value(&v, fallback_used)
}

/// [`parse`] sobre o JSON já lido (a resposta sem stream ou a remontada do stream).
fn parse_value(v: &Value, fallback_used: bool) -> Result<ChatResponse, LlmError> {
    if v.get("error").is_some_and(|e| !e.is_null()) {
        return Err(LlmError::Parse(BadReason::provider_error(v)));
    }
    let status = v["status"].as_str();
    if !matches!(status, Some("completed" | "requires_action" | "incomplete")) {
        return Err(LlmError::Parse(BadReason::with_code(BadKind::UnsupportedStatus, status)));
    }
    let Some(steps) = v["steps"].as_array() else {
        return Err(LlmError::Parse(BadReason::new(BadKind::InvalidJson)));
    };
    let (text, tool_calls, citations, web_search_calls) = read_steps(steps);
    if text.is_empty() && tool_calls.is_empty() {
        return Err(LlmError::Parse(match status {
            Some("incomplete") => BadReason::with_code(BadKind::Incomplete, Some("max_output_tokens")),
            _ => BadReason::new(BadKind::Empty),
        }));
    }
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else if status == Some("incomplete") {
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
            provider_items: Some(steps.clone()),
            images: Vec::new(),
        },
        finish_reason: Some(finish_reason.into()),
        usage: usage(&v["usage"]),
        fallback_used,
        web_search_calls,
        citations,
        web_search_rejected: false,
        billing: Default::default(),
    })
}

/// Troca os argumentos da chamada `id` no passo `function_call` guardado (compactação do histórico,
/// spec 122) e, se algum mudou, descarta os passos `thought` da mensagem: a assinatura deles vale
/// para o turno original ([`assistant_steps`] põe o de contorno no reenvio). `true` quando algo
/// mudou.
pub fn rewrite_function_call(items: &mut Vec<Value>, id: &str, arguments: &str) -> bool {
    let Ok(args @ Value::Object(_)) = serde_json::from_str::<Value>(arguments) else {
        return false;
    };
    let mut changed = false;
    for item in items.iter_mut().filter(|i| is_step(i) && i["type"] == "function_call" && i["id"] == id) {
        if let Some(obj) = item.as_object_mut() {
            obj.insert("arguments".into(), args.clone());
            changed = true;
        }
    }
    if changed {
        items.retain(|i| !is_thought(i));
    }
    changed
}

impl LlmClient {
    /// Uma tentativa pela Interactions API, sem stream, com a escada de relaxamento por 400 (cada
    /// recusa relaxa uma vez, sem laço).
    pub(super) async fn gemini_interactions(&self, req: &ChatRequest) -> Result<ChatResponse, Failed> {
        let mut opts = Opts::new(req, self.capped_known(&req.model, known_max_tokens(&self.base_url, &req.model)));
        loop {
            let body = build_body(req, &opts)?;
            let builder = self.http.post(format!("{}{INTERACTIONS_PATH}", self.base_url)).json(&body);
            let (status, text, after) = self.send_capped_with_headers(builder, Some(&req.model)).await?;
            if status.is_success() {
                if opts.learned {
                    if let Some(max) = opts.max_tokens {
                        learn_max_tokens(&self.base_url, &req.model, max);
                    }
                }
                let mut resp = parse(&text, opts.fallback_used())?;
                resp.web_search_rejected = opts.web_rejected();
                resp.billing = self.billing();
                return Ok(resp);
            }
            if status.as_u16() == 400 && opts.relax(&text) {
                self.note_resend(&req.model);
                continue;
            }
            return Err(gemini_failed(self.http_error(status.as_u16(), &text), status.as_u16(), &text, after));
        }
    }

    /// Uma tentativa pela Interactions API em stream (o caminho do agente), com a mesma escada de
    /// relaxamento. O timeout comum vale como ociosidade (até os cabeçalhos e entre pedaços), com o
    /// teto [`STREAM_MAX_DURATION`] no total. A resposta cortada vira o diagnóstico da spec 114
    /// ([`StreamDelta::Cut`]), entregue antes do erro subir. Resposta 200 que não é SSE é lida como
    /// a da versão sem stream.
    pub(super) async fn gemini_stream_once(
        &self,
        req: &ChatRequest,
        on_delta: &(dyn Fn(StreamDelta) + Send + Sync),
    ) -> Result<ChatResponse, Failed> {
        let mut opts = Opts::new(req, self.capped_known(&req.model, known_max_tokens(&self.base_url, &req.model)));
        let prompt_estimate = self.estimate_tokens(req);
        loop {
            let mut body = build_body(req, &opts)?;
            body["stream"] = json!(true);
            let token = self.bearer().await.map_err(Failed::from)?;
            let builder = self
                .http
                .post(format!("{}{INTERACTIONS_PATH}", self.base_url))
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
                if status.as_u16() == 400 && opts.relax(&text) {
                    self.note_resend(&req.model);
                    continue;
                }
                return Err(gemini_failed(self.http_error(status.as_u16(), &text), status.as_u16(), &text, after));
            }
            if opts.learned {
                if let Some(max) = opts.max_tokens {
                    learn_max_tokens(&self.base_url, &req.model, max);
                }
            }
            let fallback_used = opts.fallback_used();
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
                let out = self.read_sse(resp, first, SseAcc::Gemini(StreamAcc::default()), fallback_used, on_delta, &mut diag).await;
                if let Err(e) = &out {
                    if let Some(mut cut) = diag.finish(e, &self.secret()) {
                        cut.billing = self.billing();
                        on_delta(StreamDelta::Cut(std::sync::Arc::new(cut)));
                    }
                }
                out
            } else {
                let text = self.read_capped_from(&mut resp, first).await?;
                parse(&text, fallback_used)
            };
            let mut resp = out?;
            resp.web_search_rejected = opts.web_rejected();
            resp.billing = self.billing();
            return Ok(resp);
        }
    }
}

/// Falha HTTP do Gemini com a espera sugerida (spec 132 T09): o `Retry-After` do cabeçalho ou, num
/// 429, o `retryDelay` do corpo.
fn gemini_failed(err: LlmError, status: u16, body: &str, after: Option<std::time::Duration>) -> Failed {
    let retry_after = after.or_else(|| if status == 429 { super::limit::gemini_retry_delay(body) } else { None });
    Failed { err, retry_after }
}

/// Acumulador do stream da Interactions API: os passos por `index` (os deltas que chegam antes do
/// `step.start` ficam guardados e são aplicados nele, na ordem), os argumentos parciais das
/// chamadas, e o status e o uso do `interaction.completed`.
#[derive(Default)]
pub(super) struct StreamAcc {
    steps: BTreeMap<u64, Value>,
    /// `index` -> deltas recebidos antes do `step.start` (o texto já foi mostrado).
    early: BTreeMap<u64, Vec<Value>>,
    /// `index` -> JSON parcial dos argumentos (`arguments_delta`).
    partial: BTreeMap<u64, String>,
    status: Option<Value>,
    usage: Value,
    done: bool,
}

/// Acrescenta `piece` ao texto de `obj[key]`.
fn append_str(obj: &mut serde_json::Map<String, Value>, key: &str, piece: &str) {
    let mut text = obj.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    text.push_str(piece);
    obj.insert(key.into(), Value::String(text));
}

/// Acrescenta a parte `part` à lista `obj[key]`; uma parte de texto seguida de outra de texto é uma
/// só (a resposta sem stream traz o texto inteiro numa parte).
fn push_part(obj: &mut serde_json::Map<String, Value>, key: &str, part: &Value) {
    if !part.is_object() {
        return;
    }
    let list = obj.entry(key).or_insert_with(|| json!([]));
    if !list.is_array() {
        *list = json!([]);
    }
    let Some(list) = list.as_array_mut() else {
        return;
    };
    let text = part["text"].as_str();
    if let (Some(piece), Some(last)) = (text.filter(|_| part["type"] == "text"), list.last_mut().filter(|l| l["type"] == "text")) {
        if let Some(last) = last.as_object_mut() {
            append_str(last, "text", piece);
            return;
        }
    }
    list.push(part.clone());
}

/// Aplica o `delta` ao passo `step`.
fn apply_delta(step: &mut Value, delta: &Value) {
    let step_type = step["type"].as_str().unwrap_or_default().to_string();
    let Some(obj) = step.as_object_mut() else {
        return;
    };
    match delta["type"].as_str().unwrap_or_default() {
        "text" => push_part(obj, "content", &text_part(delta["text"].as_str().unwrap_or_default())),
        "text_annotation_delta" => {
            let content = obj.entry("content").or_insert_with(|| json!([]));
            if !content.as_array().is_some_and(|c| c.last().is_some_and(|p| p["type"] == "text")) {
                push_part(obj, "content", &text_part(""));
            }
            let last = obj.get_mut("content").and_then(Value::as_array_mut).and_then(|c| c.last_mut()).and_then(Value::as_object_mut);
            if let Some(last) = last {
                let list = last.entry("annotations").or_insert_with(|| json!([]));
                if !list.is_array() {
                    *list = json!([]);
                }
                if let Some(list) = list.as_array_mut() {
                    list.extend(delta["annotations"].as_array().into_iter().flatten().cloned());
                }
            }
        }
        "thought_summary" => push_part(obj, "summary", &delta["content"]),
        "thought_signature" => append_str(obj, "signature", delta["signature"].as_str().unwrap_or_default()),
        // Os argumentos parciais ficam em `StreamAcc::partial`.
        "arguments_delta" => {}
        // Delta de uma ferramenta interna (`google_search_call`, `google_search_result`, ...): os
        // campos vão para o passo; a `signature` é acrescentada (o start a traz vazia).
        kind if kind == step_type => {
            for (key, value) in delta.as_object().into_iter().flatten().filter(|(k, _)| k.as_str() != "type") {
                match (key.as_str(), value.as_str()) {
                    ("signature", Some(piece)) => append_str(obj, "signature", piece),
                    _ => {
                        obj.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        _ => {}
    }
}

impl StreamAcc {
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Fecha os `arguments` da chamada em `index` com o JSON acumulado. Sem JSON, ficam os do
    /// `step.start`; JSON inválido (resposta cortada) vira objeto vazio.
    fn close_arguments(&mut self, index: u64) {
        let Some(step) = self.steps.get_mut(&index).and_then(Value::as_object_mut) else {
            return;
        };
        let Some(raw) = self.partial.remove(&index) else {
            return;
        };
        let args = match serde_json::from_str::<Value>(&raw) {
            Ok(v @ Value::Object(_)) => v,
            _ if raw.trim().is_empty() => return,
            _ => json!({}),
        };
        step.insert("arguments".into(), args);
    }

    /// Processa um evento; `true` quando o fluxo terminou (`interaction.completed`).
    pub fn event(&mut self, name: &str, data: &str, on: &(dyn Fn(StreamDelta) + Send + Sync)) -> Result<bool, LlmError> {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        let index = v["index"].as_u64().unwrap_or(0);
        match v["event_type"].as_str().unwrap_or(name) {
            "step.start" => {
                if v["step"].is_object() {
                    let mut step = v["step"].clone();
                    for delta in self.early.remove(&index).into_iter().flatten() {
                        apply_delta(&mut step, &delta);
                    }
                    self.steps.insert(index, step);
                }
            }
            "step.delta" => {
                let delta = &v["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text" => emit(on, StreamDelta::Text(delta["text"].as_str().unwrap_or_default().to_string())),
                    "thought_summary" => {
                        emit(on, StreamDelta::Reasoning(delta["content"]["text"].as_str().unwrap_or_default().to_string()));
                    }
                    "arguments_delta" => {
                        let piece = delta["arguments"].as_str().unwrap_or_default();
                        self.partial.entry(index).or_default().push_str(piece);
                    }
                    _ => {}
                }
                match self.steps.get_mut(&index) {
                    Some(step) => apply_delta(step, delta),
                    None => self.early.entry(index).or_default().push(delta.clone()),
                }
            }
            "step.stop" => self.close_arguments(index),
            "interaction.completed" => {
                let interaction = &v["interaction"];
                self.status = Some(interaction["status"].clone());
                self.usage = interaction["usage"].clone();
                self.done = true;
                return Ok(true);
            }
            "error" => {
                let error = if v["error"].is_object() { &v["error"] } else { &v };
                // Spec 132 T09: a cota diária pausa, sem repetir, também no meio do stream.
                if super::limit::is_daily_quota_code(error) {
                    return Err(LlmError::Limit(super::ProviderLimit::DailyQuota));
                }
                return Err(LlmError::Parse(BadReason::provider_error(error)));
            }
            _ => {}
        }
        Ok(false)
    }

    /// A resposta montada, pelo mesmo [`parse`] da versão sem stream. Sem `interaction.completed`,
    /// a resposta não terminou.
    pub fn finish(mut self, fallback_used: bool) -> Result<ChatResponse, LlmError> {
        if !self.done {
            return Err(LlmError::Parse(NO_FINAL));
        }
        let open: Vec<u64> = self.partial.keys().copied().collect();
        for index in open {
            self.close_arguments(index);
        }
        // Delta sem o start não vira passo: um passo sem `type` seria recusado no reenvio.
        let steps: Vec<Value> = std::mem::take(&mut self.steps).into_values().filter(|s| s["type"].is_string()).collect();
        let interaction = json!({
            "status": self.status.take().unwrap_or(Value::Null),
            "steps": steps,
            "usage": std::mem::take(&mut self.usage),
        });
        parse_value(&interaction, fallback_used)
    }
}
