//! `GET /api/config/connections/:id/models`: lista os ids de modelos do endpoint
//! salvo da conexão.
//!
//! Contrato: sucesso é `200 {connection_id, models: [{id}], truncated, fetched_at}`.
//! Validação local (id, conexão, base_url, chave) responde 422/404 estáticos.
//! Falhas da consulta ao provedor são `200 {models: [], error: "<código fixo>"}`
//! (mesmo padrão do teste de conexão); `status` acompanha só `http_error`.
//! Nenhum texto remoto (corpo, URL, reqwest) é ecoado.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::config_api::{validate_base_url, validate_connection_id, valid_env_name};
use super::error::ApiError;
use super::state::AppState;
use crate::agents::llm_client::LlmClient;
use crate::agents::LlmError;
use crate::config::Connection;

/// Tempo de vida de uma listagem em cache.
pub const MODELS_CACHE_TTL: Duration = Duration::from_secs(300);
/// Número máximo de listagens em cache (evicção da mais antiga).
pub const MODELS_CACHE_MAX_ENTRIES: usize = 32;
/// Teto de modelos devolvidos por consulta.
pub const MODELS_MAX: usize = 500;
/// Tamanho máximo (caracteres) de um id de modelo.
pub const MODEL_ID_MAX_CHARS: usize = 128;
/// Timeout total da consulta de modelos.
pub const MODELS_TIMEOUT: Duration = Duration::from_secs(15);
/// Número de entradas de lock por chave a partir do qual as ociosas são limpas.
const GATES_PRUNE_AT: usize = 64;

/// Modelo listado (o esforço é decidido pela UI).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ModelInfo {
    /// O que vai no pedido (no plano ChatGPT, o `slug`).
    pub id: String,
    /// Nome mostrado (no plano ChatGPT, o `display_name`); ausente nas conexões por chave.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Janela de entrada em tokens, quando o provedor a informa (Anthropic e Gemini, spec 132 T08).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Limite de saída em tokens, quando o provedor o informa (spec 132 T08).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// O modelo raciocina (thinking), quando o provedor diz (spec 132 T08).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    /// O modelo aceita imagem na entrada, quando o provedor diz (Anthropic, spec 132 T08).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
}

/// Resultado de uma consulta de modelos: ids ordenados, sem duplicatas, até o teto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelList {
    pub models: Vec<ModelInfo>,
    pub truncated: bool,
}

/// Query de `GET .../models`: `?refresh=true` ignora o cache.
#[derive(Debug, Default, Deserialize)]
pub struct ModelsQuery {
    #[serde(default)]
    refresh: bool,
}

/// Corpo da resposta (sucesso ou falha).
#[derive(Serialize)]
struct ModelsBody<'a> {
    connection_id: &'a str,
    models: &'a [ModelInfo],
    truncated: bool,
    fetched_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
}

/// Falha da consulta, sempre com código fixo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Failure {
    pub(super) code: &'static str,
    pub(super) status: Option<u16>,
}

pub(super) const INVALID_CONFIG: Failure = Failure {
    code: "invalid_config",
    status: None,
};
pub(super) const BAD_RESPONSE: Failure = Failure {
    code: "bad_response",
    status: None,
};
/// Tipo de conexão ainda sem adaptador (spec 132 T01): Anthropic e Google Gemini.
pub(super) const KIND_NOT_SUPPORTED: Failure = Failure {
    code: "connection_kind_not_supported_yet",
    status: None,
};

/// Mapeia `LlmError` para código fixo (mesmo vocabulário do teste de conexão,
/// sem usar a mensagem do erro). 404/405/501 em `/models` = provedor sem listagem.
pub(super) fn classify(err: &LlmError) -> Failure {
    match err {
        LlmError::Config(_) => INVALID_CONFIG,
        LlmError::Timeout => Failure {
            code: "timeout",
            status: None,
        },
        LlmError::Transport(_) => Failure {
            code: "connect_failed",
            status: None,
        },
        LlmError::Http { status, .. } if matches!(status, 404 | 405 | 501) => Failure {
            code: "unsupported",
            status: None,
        },
        LlmError::Http { status, .. } => Failure {
            code: "http_error",
            status: Some(*status),
        },
        LlmError::Parse(_) => BAD_RESPONSE,
        // Plano ChatGPT (spec 116 T02): o motivo próprio (`plan_limit`, `plan_not_eligible`,
        // `plan_needs_login`).
        LlmError::Plan(plan) => Failure {
            code: plan.as_str(),
            status: None,
        },
        // Spec 132 T09: o teto de gasto (`spend_limit`) ou a cota diária (`daily_quota`).
        LlmError::Limit(limit) => Failure {
            code: limit.as_str(),
            status: None,
        },
        LlmError::KindNotSupported(_) => KIND_NOT_SUPPORTED,
    }
}

/// Chave do cache: id da conexão + `base_url` normalizado (já validado pela escrita).
/// A chave de API nunca entra aqui.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    connection_id: String,
    base_url: String,
}

impl CacheKey {
    pub fn new(connection_id: &str, base_url: &str) -> Self {
        Self {
            connection_id: connection_id.to_string(),
            base_url: base_url.to_string(),
        }
    }
}

#[derive(Debug)]
struct CacheEntry {
    list: ModelList,
    fetched_at: u64,
    stored_at: Instant,
}

/// Cache em memória das listagens (TTL e teto) e lock por chave, para que no máximo
/// uma consulta por conexão esteja em curso. Nenhum I/O é feito sob os locks std.
#[derive(Debug, Default)]
pub struct ModelsCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
    gates: Mutex<HashMap<CacheKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl ModelsCache {
    /// Listagem fresca (idade < TTL) e seu `fetched_at`.
    pub fn get(&self, key: &CacheKey, now: Instant) -> Option<(ModelList, u64)> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = entries.get(key)?;
        let fresh = now
            .checked_duration_since(entry.stored_at)
            .is_some_and(|age| age < MODELS_CACHE_TTL);
        fresh.then(|| (entry.list.clone(), entry.fetched_at))
    }

    /// Grava a listagem; remove expiradas e, no teto, a mais antiga.
    pub fn insert(&self, key: CacheKey, list: ModelList, fetched_at: u64, now: Instant) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, e| {
            now.checked_duration_since(e.stored_at)
                .is_some_and(|age| age < MODELS_CACHE_TTL)
        });
        if !entries.contains_key(&key) && entries.len() >= MODELS_CACHE_MAX_ENTRIES {
            let oldest = entries
                .iter()
                .min_by_key(|(_, e)| e.stored_at)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            key,
            CacheEntry {
                list,
                fetched_at,
                stored_at: now,
            },
        );
    }

    /// Descarta todas as listagens da conexão (após troca de `base_url` ou `api_key`).
    pub fn invalidate_connection(&self, connection_id: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|k, _| k.connection_id != connection_id);
    }

    /// Lock assíncrono da chave: quem chega junto espera a consulta em curso e depois
    /// lê o cache.
    fn gate(&self, key: &CacheKey) -> Arc<tokio::sync::Mutex<()>> {
        let mut gates = self.gates.lock().unwrap_or_else(PoisonError::into_inner);
        if gates.len() >= GATES_PRUNE_AT && !gates.contains_key(key) {
            gates.retain(|_, g| Arc::strong_count(g) > 1);
        }
        Arc::clone(
            gates
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }
}

/// Chave salva com `env:` precisa referenciar um nome de variável válido.
fn validate_saved_key(key: &str) -> Result<(), ApiError> {
    match key.strip_prefix("env:") {
        Some(name) if !valid_env_name(name) => Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_env_invalid", Map::new())),
        _ => Ok(()),
    }
}

/// Id aceito: não vazio, até `MODEL_ID_MAX_CHARS` caracteres, sem controle, sem cara
/// de URL e sem a chave de API (o provedor não pode ecoar segredo na listagem).
pub(super) fn valid_model_id(id: &str, secret: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MODEL_ID_MAX_CHARS
        && !id.chars().any(char::is_control)
        && !id.contains("://")
        && (secret.is_empty() || !id.contains(secret))
}

/// Itens da listagem: `data` (OpenAI, Ollama, vLLM), senão `models`
/// (alguns servidores locais), senão o próprio array. `None` se nenhum for array.
fn items_of(body: &Value) -> Option<&Vec<Value>> {
    match body {
        Value::Object(map) => map
            .get("data")
            .or_else(|| map.get("models"))
            .and_then(Value::as_array),
        Value::Array(items) => Some(items),
        _ => None,
    }
}

/// Id de um item: `id` (string) ou, na falta dele, `name`.
fn id_of(item: &Value) -> Option<&str> {
    ["id", "name"]
        .iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str))
}

/// Normaliza a listagem. Entradas inválidas são descartadas; ids são deduplicados e
/// ordenados; o teto de `MODELS_MAX` marca `truncated`. `None` se o corpo não tem
/// uma lista ou se a lista é não vazia e nenhum item tem id válido (`bad_response`).
pub fn normalize_models(body: &Value, secret: &str) -> Option<ModelList> {
    let items = items_of(body)?;
    let unique: BTreeSet<&str> = items
        .iter()
        .filter_map(id_of)
        .filter(|id| valid_model_id(id, secret))
        .collect();
    if !items.is_empty() && unique.is_empty() {
        return None;
    }
    let truncated = unique.len() > MODELS_MAX;
    let models = unique
        .into_iter()
        .take(MODELS_MAX)
        .map(|id| ModelInfo { id: id.to_string(), ..ModelInfo::default() })
        .collect();
    Some(ModelList { models, truncated })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Modelos da conexão pelo plano ChatGPT (spec 116 T02): `GET /models` com o token do login, só os
/// visíveis, na ordem do servidor.
pub(super) async fn fetch_plan(state: &AppState, conn: &Connection, timeout: Duration) -> Result<ModelList, Failure> {
    let client = super::chatgpt::plan_client::llm_client(state, conn)
        .and_then(|c| c.with_timeout(timeout))
        .map_err(|e| classify(&e))?
        .without_pacing();
    super::chatgpt::plan_client::fetch_plan_models(&client).await.map_err(|e| classify(&e))
}

/// Consulta `{base_url}/models` da conexão salva. A chave só vai ao `base_url` salvo.
/// A Anthropic e o Google Gemini usam a Models API própria ([`super::models_native`], spec 132 T08).
pub(super) async fn fetch(
    conn: &Connection,
    base_url: &str,
    timeout: Duration,
) -> Result<ModelList, Failure> {
    if conn.kind.is_native() {
        return super::models_native::fetch(conn, base_url, timeout).await;
    }
    let api_key = conn.resolve_api_key().map_err(|_| INVALID_CONFIG)?;
    let client = LlmClient::new(base_url, api_key.clone())
        .and_then(|c| c.with_timeout(timeout))
        .map_err(|e| classify(&e))?;
    let body = client.get_json("/models").await.map_err(|e| classify(&e))?;
    normalize_models(&body, &api_key).ok_or(BAD_RESPONSE)
}

fn success_response(connection_id: &str, list: &ModelList, fetched_at: u64) -> Response {
    let body = ModelsBody {
        connection_id,
        models: &list.models,
        truncated: list.truncated,
        fetched_at,
        error: None,
        status: None,
    };
    (StatusCode::OK, Json(body)).into_response()
}

fn failure_response(connection_id: &str, failure: Failure) -> Response {
    let body = ModelsBody {
        connection_id,
        models: &[],
        truncated: false,
        fetched_at: unix_now(),
        error: Some(failure.code),
        status: failure.status,
    };
    (StatusCode::OK, Json(body)).into_response()
}

/// `GET /api/config/connections/:id/models`
pub async fn list_models(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ModelsQuery>,
) -> Result<Response, ApiError> {
    validate_connection_id(&id)?;
    let saved = {
        let guard = state.config().read().await;
        let found = guard.connections.iter().find(|c| c.id == id).cloned();
        found
    }
    .ok_or_else(|| ApiError::coded(StatusCode::NOT_FOUND, "connection_not_found", Map::new()))?;
    // Conexão salva à mão fora do padrão atual: não usar a chave nesse endpoint.
    let base_url = validate_base_url(saved.endpoint())
        .map_err(|_| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_saved_invalid", Map::new()))?;
    validate_saved_key(&saved.api_key)?;

    let key = CacheKey::new(&id, &base_url);
    let cache = state.models_cache();
    let gate = cache.gate(&key);
    let _turn = gate.lock().await;

    if !query.refresh {
        if let Some((list, fetched_at)) = cache.get(&key, Instant::now()) {
            return Ok(success_response(&id, &list, fetched_at));
        }
    }
    let fetched = if saved.is_chatgpt_plan() {
        fetch_plan(&state, &saved, state.models_timeout()).await
    } else {
        fetch(&saved, &base_url, state.models_timeout()).await
    };
    match fetched {
        Ok(list) => {
            let fetched_at = unix_now();
            cache.insert(key, list.clone(), fetched_at, Instant::now());
            Ok(success_response(&id, &list, fetched_at))
        }
        Err(failure) => Ok(failure_response(&id, failure)),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{header, Method, Request, StatusCode};
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tower::ServiceExt;

    use super::*;
    use crate::config::{Config, Connection};
    use crate::server::router;

    const PORT: u16 = 3000;
    const HOST: &str = "127.0.0.1:3000";
    const KEY: &str = "sk-MODELS-SEGREDO-4242";
    const OPENAI_BODY: &str = r#"{"object":"list","data":[{"id":"o3-mini","object":"model"},{"id":"gpt-4o","object":"model"}]}"#;
    const MODELS_URI: &str = "/api/config/connections/conn/models";

    fn conn(id: &str, base_url: &str, key: &str) -> Connection {
        Connection {
            id: id.into(),
            name: "Teste".into(),
            base_url: base_url.into(),
            api_key: key.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        }
    }

    fn state_with(connections: Vec<Connection>) -> AppState {
        let cfg = Config {
            connections,
            ..Config::default()
        };
        AppState::new(PathBuf::from("/nonexistent-nants/config.toml"), cfg, PORT)
    }

    fn raw_response(status: u16, extra_headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} X\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn json_response(status: u16, body: &str) -> String {
        raw_response(status, "Content-Type: application/json\r\n", body)
    }

    /// Lê a cabeça da requisição (até a linha vazia; GET não tem corpo).
    async fn read_head(sock: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut tmp).await {
                Ok(n) if n > 0 => buf.extend_from_slice(&tmp[..n]),
                _ => break,
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Servidor local: responde, em ordem, cada resposta bruta e captura as requisições.
    async fn spawn(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("endereço");
        let captured = Arc::new(Mutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        tokio::spawn(async move {
            for raw in responses {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let head = read_head(&mut sock).await;
                cap.lock().expect("lock").push(head);
                let _ = sock.write_all(raw.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}/v1"), captured)
    }

    /// Servidor que aceita e nunca responde.
    async fn spawn_silent() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("endereço");
        tokio::spawn(async move {
            let Ok((_sock, _)) = listener.accept().await else {
                return;
            };
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        format!("http://{addr}/v1")
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(header::HOST, HOST)
            .body(Body::empty())
            .expect("requisição")
    }

    /// Envia `GET uri` pelo roteador completo; devolve status, texto e corpo JSON.
    async fn get(state: &AppState, uri: &str) -> (StatusCode, String, Value) {
        let resp = router(state.clone())
            .oneshot(get_req(uri))
            .await
            .expect("resposta");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
            .await
            .expect("corpo");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let value = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, text, value)
    }

    fn ids(list: &ModelList) -> Vec<&str> {
        list.models.iter().map(|m| m.id.as_str()).collect()
    }

    // ---- Normalização (unitário) ----

    #[test]
    fn formato_openai_data_com_id() {
        let body = json!({"object": "list", "data": [{"id": "o3-mini"}, {"id": "gpt-4o"}]});
        let list = normalize_models(&body, KEY).expect("lista");
        assert_eq!(ids(&list), vec!["gpt-4o", "o3-mini"]);
        assert!(!list.truncated);
    }

    #[test]
    fn formato_com_metadados_extras_e_ignorado() {
        let body = json!({"data": [
            {"id": "a/r1", "supported_parameters": ["tools", "reasoning"]},
            {"id": "a/n", "supported_parameters": []}
        ]});
        let list = normalize_models(&body, KEY).expect("lista");
        assert_eq!(ids(&list), vec!["a/n", "a/r1"]);
    }

    #[test]
    fn formato_ollama_e_vllm_com_data() {
        let body = json!({"object": "list", "data": [{"id": "llama3:8b", "object": "model"}]});
        let list = normalize_models(&body, KEY).expect("lista");
        assert_eq!(ids(&list), vec!["llama3:8b"]);
    }

    #[test]
    fn formato_models_com_id_ou_name_quando_data_ausente() {
        let body = json!({"models": [{"id": "m1"}, {"name": "m2"}]});
        let list = normalize_models(&body, KEY).expect("lista");
        assert_eq!(ids(&list), vec!["m1", "m2"]);
    }

    #[test]
    fn array_de_topo_e_aceito() {
        let list = normalize_models(&json!([{"id": "x"}]), KEY).expect("lista");
        assert_eq!(ids(&list), vec!["x"]);
    }

    #[test]
    fn lista_vazia_e_valida() {
        let list = normalize_models(&json!({"data": []}), KEY).expect("lista");
        assert!(list.models.is_empty());
        assert!(!list.truncated);
    }

    #[test]
    fn corpo_sem_lista_e_resposta_ruim() {
        assert!(normalize_models(&json!({"data": {"id": "x"}}), KEY).is_none());
        assert!(normalize_models(&json!({"outro": []}), KEY).is_none());
        assert!(normalize_models(&json!("texto"), KEY).is_none());
        assert!(normalize_models(&json!(42), KEY).is_none());
    }

    #[test]
    fn entradas_invalidas_sao_descartadas() {
        let long_ok = "y".repeat(128);
        let body = json!({"data": [
            {"object": "model"},
            {"id": 5},
            {"id": ""},
            {"id": "com\u{7}controle"},
            {"id": "x".repeat(129)},
            {"id": long_ok.clone()},
            {"id": "https://u:senha@host/v1"},
            {"id": format!("vazou-{KEY}-aqui")},
            "não é objeto",
            {"id": "valido"}
        ]});
        let list = normalize_models(&body, KEY).expect("lista");
        assert_eq!(ids(&list), vec!["valido", long_ok.as_str()]);
    }

    #[test]
    fn ids_duplicados_sao_unificados_e_ordenados() {
        let body = json!({"data": [{"id": "b"}, {"id": "a"}, {"id": "b"}]});
        let list = normalize_models(&body, KEY).expect("lista");
        assert_eq!(ids(&list), vec!["a", "b"]);
    }

    #[test]
    fn teto_de_500_modelos_marca_truncated() {
        let data: Vec<Value> = (0..510).rev().map(|i| json!({"id": format!("m{i:04}")})).collect();
        let list = normalize_models(&json!({ "data": data }), KEY).expect("lista");
        assert_eq!(list.models.len(), MODELS_MAX);
        assert!(list.truncated);
        assert_eq!(list.models[0].id, "m0000");
        assert_eq!(list.models[MODELS_MAX - 1].id, "m0499");
    }

    // ---- Cache (unitário) ----

    #[test]
    fn cache_expira_apos_o_ttl() {
        let cache = ModelsCache::default();
        let key = CacheKey::new("c", "https://h/v1");
        let t0 = Instant::now();
        cache.insert(key.clone(), ModelList { models: vec![], truncated: false }, 42, t0);
        assert_eq!(cache.get(&key, t0 + Duration::from_secs(299)).map(|(_, f)| f), Some(42));
        assert!(cache.get(&key, t0 + Duration::from_secs(301)).is_none());
    }

    #[test]
    fn cache_evicta_a_entrada_mais_antiga_no_teto() {
        let cache = ModelsCache::default();
        let t0 = Instant::now();
        let keys: Vec<CacheKey> = (0..=MODELS_CACHE_MAX_ENTRIES)
            .map(|i| CacheKey::new(&format!("c{i}"), "https://h/v1"))
            .collect();
        for (i, key) in keys.iter().enumerate() {
            let now = t0 + Duration::from_secs(i as u64);
            cache.insert(key.clone(), ModelList { models: vec![], truncated: false }, 0, now);
        }
        let now = t0 + Duration::from_secs(MODELS_CACHE_MAX_ENTRIES as u64);
        assert!(cache.get(&keys[0], now).is_none(), "a mais antiga deve sair");
        assert!(cache.get(&keys[MODELS_CACHE_MAX_ENTRIES], now).is_some());
    }

    #[test]
    fn chave_de_cache_depende_da_conexao_e_do_endpoint() {
        let a = CacheKey::new("c", "https://h/v1");
        assert_ne!(a, CacheKey::new("c", "https://outro/v1"));
        assert_ne!(a, CacheKey::new("d", "https://h/v1"));
        assert_eq!(a, CacheKey::new("c", "https://h/v1"));
    }

    #[tokio::test]
    async fn gate_serializa_consultas_da_mesma_chave() {
        let cache = ModelsCache::default();
        let key = CacheKey::new("c", "https://h/v1");
        let first = cache.gate(&key);
        let _held = first.lock().await;
        let second = cache.gate(&key);
        assert!(second.try_lock().is_err(), "a segunda consulta deve esperar");
        let other = cache.gate(&CacheKey::new("d", "https://h/v1"));
        assert!(other.try_lock().is_ok(), "chaves diferentes não se bloqueiam");
    }

    // ---- Endpoint (HTTP local) ----

    #[tokio::test]
    async fn lista_openai_com_bearer_e_sem_vazar_chave() {
        let (base, cap) = spawn(vec![json_response(200, OPENAI_BODY)]).await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (status, text, v) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(!text.contains(KEY), "vazou chave: {text}");
        assert_eq!(v["connection_id"], "conn");
        assert_eq!(v["truncated"], false);
        assert!(v["fetched_at"].is_u64());
        assert!(v.get("error").is_none());
        assert_eq!(v["models"], json!([{"id": "gpt-4o"}, {"id": "o3-mini"}]));

        let head = cap.lock().expect("lock")[0].to_ascii_lowercase();
        assert!(head.starts_with("get /v1/models http/1.1"), "{head}");
        assert!(head.contains(&format!("authorization: bearer {}", KEY.to_ascii_lowercase())));
    }

    #[tokio::test]
    async fn troca_de_api_key_via_put_invalida_a_listagem_em_cache() {
        const NOVA: &str = "sk-NOVA-CHAVE-9999";
        let (base, cap) = spawn(vec![
            json_response(200, OPENAI_BODY),
            json_response(200, OPENAI_BODY),
        ])
        .await;
        let dir = std::env::temp_dir().join(format!("nants-models-put-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let state = AppState::new(dir.join("config.toml"), Config::default(), PORT);
        let put = Request::builder()
            .method(Method::PUT)
            .uri("/api/config/connections")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"id":"conn","name":"Teste","base_url":base,"api_key":KEY}).to_string(),
            ))
            .expect("put");
        let resp = router(state.clone()).oneshot(put).await.expect("put");
        assert_eq!(resp.status(), StatusCode::OK);

        let (status, _, _) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cap.lock().expect("lock").len(), 1, "segunda leitura vem do cache");

        let put = Request::builder()
            .method(Method::PUT)
            .uri("/api/config/connections")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"id":"conn","name":"Teste","base_url":base,"api_key":NOVA}).to_string(),
            ))
            .expect("put");
        let resp = router(state.clone()).oneshot(put).await.expect("put");
        assert_eq!(resp.status(), StatusCode::OK);

        let (status, _, _) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK);
        let reqs = cap.lock().expect("lock").clone();
        assert_eq!(reqs.len(), 2, "após trocar a chave a listagem é consultada de novo");
        assert!(reqs[1].to_ascii_lowercase().contains(&format!("bearer {}", NOVA.to_ascii_lowercase())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn conexao_inexistente_da_404() {
        let state = state_with(vec![conn("outra", "https://h/v1", KEY)]);
        let (status, _, v) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(v["error"].is_string());
    }

    #[tokio::test]
    async fn id_invalido_da_422() {
        let state = state_with(vec![]);
        let (status, _, _) = get(&state, "/api/config/connections/a%20b/models").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let long = "c".repeat(65);
        let (status, _, _) = get(&state, &format!("/api/config/connections/{long}/models")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn host_invalido_da_403() {
        let state = state_with(vec![conn("conn", "https://h/v1", KEY)]);
        let mut req = get_req(MODELS_URI);
        req.headers_mut()
            .insert(header::HOST, "evil.example:3000".parse().expect("hv"));
        let resp = router(state).oneshot(req).await.expect("resposta");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn base_url_salva_com_userinfo_da_422_sem_enviar_nada() {
        let state = state_with(vec![conn(
            "conn",
            "http://usuario:SEGUSER@127.0.0.1:9/v1",
            KEY,
        )]);
        let (status, text, _) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
        assert!(!text.contains("SEGUSER") && !text.contains(KEY), "{text}");
    }

    #[tokio::test]
    async fn chave_env_invalida_da_422_e_ausente_vira_invalid_config() {
        let state = state_with(vec![conn("conn", "https://h/v1", "env:A B")]);
        let (status, _, _) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let var = "NANTS_T01_MODELS_KEY_ABSENT";
        std::env::remove_var(var);
        let state = state_with(vec![conn(
            "conn",
            "https://h/v1",
            &format!("env:{var}"),
        )]);
        let (status, _, v) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["error"], "invalid_config");
    }

    #[tokio::test]
    async fn redirect_307_nao_e_seguido_e_chave_nao_vai_ao_destino() {
        let (target, target_cap) = spawn(vec![json_response(200, OPENAI_BODY)]).await;
        let (base, _cap) = spawn(vec![raw_response(
            307,
            &format!("Location: {target}/models\r\n"),
            "",
        )])
        .await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (status, text, v) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(v["error"], "http_error");
        assert_eq!(v["status"], 307);
        assert!(target_cap.lock().expect("lock").is_empty(), "redirect foi seguido");
    }

    #[tokio::test]
    async fn corpo_gigante_vira_bad_response_fixo() {
        let big = "x".repeat(17 * 1024 * 1024);
        let (base, _cap) = spawn(vec![json_response(200, &big)]).await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (status, text, v) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["error"], "bad_response");
        assert!(text.len() < 1024, "não deve ecoar o corpo");
    }

    #[tokio::test]
    async fn http_401_nao_expoe_corpo_do_provedor() {
        let body = r#"{"error":"chave recusada SECRETBODY"}"#;
        let (base, _cap) = spawn(vec![json_response(401, body)]).await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (status, text, v) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["error"], "http_error");
        assert_eq!(v["status"], 401);
        assert!(!text.contains("SECRETBODY") && !text.contains(KEY), "{text}");
    }

    #[tokio::test]
    async fn status_404_do_provedor_e_unsupported() {
        let (base, _cap) = spawn(vec![json_response(404, "{}")]).await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (_, _, v) = get(&state, MODELS_URI).await;
        assert_eq!(v["error"], "unsupported");
        assert!(v.get("status").is_none());
    }

    #[tokio::test]
    async fn json_invalido_e_bad_response() {
        let (base, _cap) = spawn(vec![json_response(200, "não é json")]).await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (_, _, v) = get(&state, MODELS_URI).await;
        assert_eq!(v["error"], "bad_response");
        assert_eq!(v["models"], json!([]));
    }

    #[tokio::test]
    async fn servidor_silencioso_vira_timeout() {
        let base = spawn_silent().await;
        let state = state_with(vec![conn("conn", &base, KEY)])
            .with_models_timeout(Duration::from_millis(300));
        let (status, _, v) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["error"], "timeout");
    }

    #[tokio::test]
    async fn conexao_recusada_vira_connect_failed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("endereço");
        drop(listener);
        let state = state_with(vec![conn("conn", &format!("http://{addr}/v1"), KEY)]);
        let (_, _, v) = get(&state, MODELS_URI).await;
        assert_eq!(v["error"], "connect_failed");
    }

    #[tokio::test]
    async fn ids_com_a_chave_ou_url_nao_aparecem_na_resposta() {
        let body = format!(r#"{{"data":[{{"id":"a-{KEY}"}},{{"id":"https://u:p@h/x"}},{{"id":"ok-model"}}]}}"#);
        let (base, _cap) = spawn(vec![json_response(200, &body)]).await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (status, text, v) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!text.contains(KEY) && !text.contains("u:p@"), "{text}");
        assert_eq!(v["models"], json!([{"id": "ok-model"}]));
    }

    #[tokio::test]
    async fn cache_hit_nao_refaz_a_requisicao() {
        let (base, cap) = spawn(vec![json_response(200, OPENAI_BODY)]).await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (_, _, first) = get(&state, MODELS_URI).await;
        let (status, _, second) = get(&state, MODELS_URI).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cap.lock().expect("lock").len(), 1, "segunda chamada deve usar o cache");
        assert_eq!(first["fetched_at"], second["fetched_at"]);
        assert_eq!(first["models"], second["models"]);
    }

    #[tokio::test]
    async fn refresh_ignora_o_cache() {
        let (base, cap) = spawn(vec![
            json_response(200, OPENAI_BODY),
            json_response(200, r#"{"data":[{"id":"novo"}]}"#),
        ])
        .await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        get(&state, MODELS_URI).await;
        let (status, _, v) = get(&state, &format!("{MODELS_URI}?refresh=true")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cap.lock().expect("lock").len(), 2);
        assert_eq!(v["models"], json!([{"id": "novo"}]));
    }

    #[tokio::test]
    async fn erros_nao_sao_cacheados() {
        let (base, cap) = spawn(vec![json_response(500, "falhou"), json_response(200, OPENAI_BODY)]).await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (_, _, first) = get(&state, MODELS_URI).await;
        assert_eq!(first["error"], "http_error");
        let (_, _, second) = get(&state, MODELS_URI).await;
        assert!(second.get("error").is_none());
        assert_eq!(cap.lock().expect("lock").len(), 2);
    }

    #[tokio::test]
    async fn consultas_concorrentes_da_mesma_conexao_viram_uma() {
        let (base, cap) = spawn(vec![json_response(200, OPENAI_BODY)]).await;
        let state = state_with(vec![conn("conn", &base, KEY)]);
        let (a, b) = tokio::join!(get(&state, MODELS_URI), get(&state, MODELS_URI));
        assert_eq!(a.0, StatusCode::OK);
        assert_eq!(b.0, StatusCode::OK);
        assert!(a.2.get("error").is_none() && b.2.get("error").is_none(), "{} / {}", a.1, b.1);
        assert_eq!(cap.lock().expect("lock").len(), 1);
    }
}
