//! Lista de modelos e códigos do teste de conexão das conexões nativas (spec 132 T08).
//!
//! - **Anthropic:** `GET {endpoint}/v1/models?limit=1000`, seguindo `has_more` com `after_id`. Lê
//!   `data[].id`, `display_name`, `max_input_tokens`, `max_tokens` e as `capabilities` (thinking e
//!   imagem). Todos os modelos entram.
//! - **Google Gemini:** `GET {endpoint}/v1beta/models?pageSize=1000`, seguindo `nextPageToken`. Só
//!   os modelos de texto: com `generateContent` em `supportedGenerationMethods` (o que já tira
//!   embeddings e AQA) e fora das famílias de mídia pelo nome (imagem, voz, música e áudio). O nome
//!   vai sem o prefixo `models/`.
//!
//! O limite de saída de cada modelo vai para a memória de `max_tokens` dos adaptadores
//! ([`anthropic::remember_max_tokens`], [`gemini::remember_max_tokens`]) e a janela de entrada para
//! [`remember_input_window`], que a execução usa antes do catálogo de preços. As duas chaves são o
//! endereço da conexão ([`Connection::endpoint`]) e o modelo, as mesmas dos adaptadores.
//!
//! Falhas viram códigos fixos ([`classify`]); nenhum texto do provedor sai daqui.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use serde_json::Value;

use super::models_api::{self, valid_model_id, Failure, ModelInfo, ModelList, BAD_RESPONSE, INVALID_CONFIG, MODELS_MAX};
use crate::agents::llm_client::{anthropic, gemini, LlmClient};
use crate::agents::LlmError;
use crate::config::{Connection, ConnectionKind};

/// Lista de modelos da Anthropic, a partir do endereço da conexão.
pub const ANTHROPIC_MODELS_PATH: &str = "/v1/models";
/// Lista de modelos do Gemini, a partir do endereço da conexão.
pub const GEMINI_MODELS_PATH: &str = "/v1beta/models";
/// Modelos pedidos por página (o máximo das duas APIs).
const PAGE_SIZE: u32 = 1000;
/// Teto de páginas por consulta (defesa contra um cursor que nunca acaba).
const MAX_PAGES: usize = 20;
/// Método de geração que marca um modelo de texto no Gemini.
const GEMINI_TEXT_METHOD: &str = "generateContent";
/// Partes do nome (separadas por `-`) das famílias do Gemini que não são de texto, embora aceitem
/// `generateContent`: imagem, voz, música, áudio e transcrição. Embeddings e AQA já saem pelo
/// método; ficam aqui também, por segurança.
const GEMINI_NON_TEXT_PARTS: [&str; 8] = ["image", "banana", "tts", "lyria", "audio", "transcribe", "embedding", "aqa"];
/// Prefixo dos nomes de modelo do Gemini.
const GEMINI_NAME_PREFIX: &str = "models/";

/// Chave inválida (401; no Gemini, também o 400 `API_KEY_INVALID`).
pub(super) const AUTH_INVALID: Failure = Failure { code: "auth_invalid", status: None };
/// Chave sem permissão (403).
pub(super) const FORBIDDEN: Failure = Failure { code: "forbidden", status: None };
/// Modelo inexistente no provedor (404 do teste com modelo).
pub(super) const MODEL_NOT_FOUND: Failure = Failure { code: "model_not_found", status: None };
/// Limite de taxa do provedor (429).
pub(super) const RATE_LIMITED: Failure = Failure { code: "rate_limited", status: None };
/// Teto de gasto da conta atingido (429 da Anthropic com `enforced_spend_limit_reached`).
pub(super) const SPEND_LIMIT: Failure = Failure { code: "spend_limit", status: None };
/// Cobrança do provedor: crédito esgotado ou pagamento pendente (402, spec 133 T02).
pub(super) const BILLING: Failure = Failure { code: "billing", status: None };

/// Janelas de entrada conhecidas por (endereço, modelo), da lista de modelos.
fn input_windows() -> &'static Mutex<HashMap<(String, String), u64>> {
    static WINDOWS: OnceLock<Mutex<HashMap<(String, String), u64>>> = OnceLock::new();
    WINDOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn window_key(endpoint: &str, model: &str) -> (String, String) {
    (endpoint.trim_end_matches('/').to_string(), model.to_string())
}

/// Guarda a janela de entrada do `model` no endereço `endpoint`. Zero é ignorado.
pub fn remember_input_window(endpoint: &str, model: &str, tokens: u64) {
    if tokens == 0 {
        return;
    }
    input_windows().lock().unwrap_or_else(PoisonError::into_inner).insert(window_key(endpoint, model), tokens);
}

/// Janela de entrada conhecida do `model` no endereço `endpoint` (da última lista de modelos).
pub fn known_input_window(endpoint: &str, model: &str) -> Option<u64> {
    input_windows().lock().unwrap_or_else(PoisonError::into_inner).get(&window_key(endpoint, model)).copied()
}

/// Código fixo de uma falha de conexão nativa. `with_model` é o teste com modelo, em que o 404 é o
/// modelo inexistente; na lista, o 404 segue o vocabulário de sempre ([`models_api::classify`]).
/// O corpo do erro só é lido para escolher o código; nunca sai daqui.
pub(super) fn classify(err: &LlmError, with_model: bool) -> Failure {
    let LlmError::Http { status, body_snippet, .. } = err else {
        return models_api::classify(err);
    };
    match *status {
        401 => AUTH_INVALID,
        402 => BILLING,
        400 if key_rejected(body_snippet) => AUTH_INVALID,
        403 => FORBIDDEN,
        404 if with_model => MODEL_NOT_FOUND,
        429 if spend_limit_reached(body_snippet) => SPEND_LIMIT,
        429 => RATE_LIMITED,
        _ => models_api::classify(err),
    }
}

/// O 400 do Gemini para a chave recusada (`API_KEY_INVALID`).
fn key_rejected(body: &str) -> bool {
    body.contains("API_KEY_INVALID") || body.contains("API key not valid")
}

/// O 429 da Anthropic pelo teto de gasto da conta (sem `retry-after`: esperar não resolve). O
/// cliente já o devolve como [`LlmError::Limit`]; este caminho cobre o `LlmError::Http` cru.
fn spend_limit_reached(body: &str) -> bool {
    crate::agents::llm_client::limit::spend_limit_reached(body)
}

/// Página lida: os modelos aceitos e o cursor da seguinte.
#[derive(Debug, Default, PartialEq, Eq)]
struct Page {
    models: Vec<ModelInfo>,
    next: Option<String>,
}

/// Inteiro positivo do campo, quando houver.
fn positive(v: &Value) -> Option<u64> {
    v.as_u64().filter(|n| *n > 0)
}

/// Nome mostrado, com as mesmas regras do id (sem controle, URL nem a chave); senão nenhum.
fn display_name(v: &Value, secret: &str) -> Option<String> {
    v.as_str().map(str::trim).filter(|n| valid_model_id(n, secret)).map(str::to_string)
}

/// Página da Anthropic: `{data:[...], has_more, last_id}`. `None` sem a lista, ou com itens e
/// nenhum id válido.
fn anthropic_page(body: &Value, secret: &str) -> Option<Page> {
    let items = body.get("data")?.as_array()?;
    let models: Vec<ModelInfo> = items
        .iter()
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?;
            valid_model_id(id, secret).then(|| ModelInfo {
                id: id.to_string(),
                name: display_name(&item["display_name"], secret),
                input_tokens: positive(&item["max_input_tokens"]),
                output_tokens: positive(&item["max_tokens"]),
                thinking: item["capabilities"]["thinking"]["supported"].as_bool(),
                vision: item["capabilities"]["image_input"]["supported"].as_bool(),
            })
        })
        .collect();
    if !items.is_empty() && models.is_empty() {
        return None;
    }
    let next = (body["has_more"].as_bool() == Some(true))
        .then(|| body["last_id"].as_str().map(str::to_string).or_else(|| models.last().map(|m| m.id.clone())))
        .flatten()
        .filter(|cursor| !cursor.is_empty());
    Some(Page { models, next })
}

/// Modelo de texto do Gemini: gera conteúdo e não é de uma família de mídia pelo nome.
fn gemini_text_model(item: &Value, id: &str) -> bool {
    let generates = item["supportedGenerationMethods"]
        .as_array()
        .is_some_and(|methods| methods.iter().any(|m| m.as_str() == Some(GEMINI_TEXT_METHOD)));
    let lower = id.to_ascii_lowercase();
    generates && !lower.split('-').any(|part| GEMINI_NON_TEXT_PARTS.contains(&part))
}

/// Página do Gemini: `{models:[...], nextPageToken}`. Um objeto sem `models` é uma lista vazia;
/// outro formato é `None`.
fn gemini_page(body: &Value, secret: &str) -> Option<Page> {
    let obj = body.as_object()?;
    let items = match obj.get("models") {
        Some(models) => models.as_array()?.as_slice(),
        None => &[],
    };
    let models = items
        .iter()
        .filter_map(|item| {
            let raw = item.get("name")?.as_str()?;
            let id = raw.strip_prefix(GEMINI_NAME_PREFIX).unwrap_or(raw);
            (valid_model_id(id, secret) && gemini_text_model(item, id)).then(|| ModelInfo {
                id: id.to_string(),
                name: display_name(&item["displayName"], secret),
                input_tokens: positive(&item["inputTokenLimit"]),
                output_tokens: positive(&item["outputTokenLimit"]),
                thinking: item["thinking"].as_bool(),
                vision: None,
            })
        })
        .collect();
    let next = obj.get("nextPageToken").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string);
    Some(Page { models, next })
}

/// Codifica o cursor para a query (só os caracteres não reservados passam crus).
fn encode_query(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => char::from(b).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Caminho da página com o cursor da anterior.
fn page_path(kind: ConnectionKind, cursor: Option<&str>) -> String {
    match (kind, cursor) {
        (ConnectionKind::GoogleGemini, None) => format!("{GEMINI_MODELS_PATH}?pageSize={PAGE_SIZE}"),
        (ConnectionKind::GoogleGemini, Some(c)) => format!("{GEMINI_MODELS_PATH}?pageSize={PAGE_SIZE}&pageToken={}", encode_query(c)),
        (_, None) => format!("{ANTHROPIC_MODELS_PATH}?limit={PAGE_SIZE}"),
        (_, Some(c)) => format!("{ANTHROPIC_MODELS_PATH}?limit={PAGE_SIZE}&after_id={}", encode_query(c)),
    }
}

/// Grava os limites dos modelos listados nas memórias por (endereço, modelo).
fn remember_limits(kind: ConnectionKind, endpoint: &str, models: &[ModelInfo]) {
    for m in models {
        if let Some(max) = m.output_tokens.and_then(|n| u32::try_from(n).ok()) {
            match kind {
                ConnectionKind::GoogleGemini => gemini::remember_max_tokens(endpoint, &m.id, max),
                _ => anthropic::remember_max_tokens(endpoint, &m.id, max),
            }
        }
        if let Some(window) = m.input_tokens {
            remember_input_window(endpoint, &m.id, window);
        }
    }
}

/// Lista de modelos da conexão nativa em `base_url` (o endereço dela, já validado), página a página.
/// A chave só vai a esse endereço. Os limites ficam na memória com o [`Connection::endpoint`].
pub(super) async fn fetch(conn: &Connection, base_url: &str, timeout: Duration) -> Result<ModelList, Failure> {
    let api_key = conn.resolve_api_key().map_err(|_| INVALID_CONFIG)?;
    if api_key.trim().is_empty() {
        return Err(INVALID_CONFIG);
    }
    let client = LlmClient::new(base_url, api_key.clone())
        .map(|c| c.with_kind(conn.kind))
        .and_then(|c| c.with_timeout(timeout))
        .map_err(|e| classify(&e, false))?;
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    let mut cursor: Option<String> = None;
    // A última página lida ainda indicava outra (spec 132 T09): esgotado o `MAX_PAGES` assim, a
    // lista fica truncada.
    let mut more = false;
    for _ in 0..MAX_PAGES {
        let body = client.get_json(&page_path(conn.kind, cursor.as_deref())).await.map_err(|e| classify(&e, false))?;
        let page = match conn.kind {
            ConnectionKind::GoogleGemini => gemini_page(&body, &api_key),
            _ => anthropic_page(&body, &api_key),
        }
        .ok_or(BAD_RESPONSE)?;
        models.extend(page.models.into_iter().filter(|m| seen.insert(m.id.clone())));
        match page.next {
            Some(next) if cursor.as_deref() != Some(next.as_str()) => {
                cursor = Some(next);
                more = true;
            }
            _ => {
                more = false;
                break;
            }
        }
    }
    remember_limits(conn.kind, conn.endpoint(), &models);
    let truncated = more || models.len() > MODELS_MAX;
    models.truncate(MODELS_MAX);
    Ok(ModelList { models, truncated })
}

#[cfg(test)]
#[path = "models_native_tests.rs"]
mod tests;
