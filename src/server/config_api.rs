//! Endpoints REST de configuração global: `/api/config`.
//!
//! Chaves de API nunca saem nas respostas: são sempre mascaradas.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use serde_json::{json, Map};
use super::error::ApiError;
use super::models_api;
use super::price_catalog;
use super::state::AppState;
use crate::agents::llm_client::{ChatMessage, ChatRequest, LlmClient, Role};
use crate::agents::LlmError;
use crate::config::{
    AgentConfig, Agents, Config, ConfigError, Connection, ConnectionKind, Effort, Pricing, WireApi, MAX_STEPS_RANGE,
};

/// Timeout do teste de conexão.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Máscara exibida no lugar da chave de API.
const MASK: &str = "********";
/// Quantidade de caracteres finais da chave visíveis na máscara.
const MASK_VISIBLE_CHARS: usize = 4;
/// Chaves com mais caracteres que isso mostram os últimos `MASK_VISIBLE_CHARS`.
const MASK_MIN_LEN: usize = 8;
/// Prefixo de referência a variável de ambiente (não é segredo).
const ENV_PREFIX: &str = "env:";

#[derive(Serialize)]
struct ConfigView {
    server: ServerView,
    connections: Vec<ConnectionView>,
    agents: AgentsView,
    projects: Vec<ProjectView>,
    pricing: std::collections::BTreeMap<String, Pricing>,
    /// Raiz da pasta de projetos como configurada (`null` = padrão) e a efetiva, expandida.
    projects_root: Option<String>,
    projects_root_effective: Option<String>,
}

/// Campos validados antes de sair na leitura; inválidos viram `INVALID_VIEW`.
#[derive(Serialize)]
struct ServerView {
    host: String,
    port: u16,
}

#[derive(Serialize)]
struct AgentView {
    connection_id: String,
    model: String,
    effort: Effort,
    /// Teto de passos configurado (spec 081 T01); omitido quando usa o padrão do papel.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_steps: Option<u32>,
}

#[derive(Serialize)]
struct AgentsView {
    architect: AgentView,
    developer: AgentView,
    reviewer: AgentView,
}

#[derive(Serialize)]
struct ProjectView {
    id: String,
    name: String,
    path: String,
    created_at: String,
}

#[derive(Serialize)]
struct ConnectionView {
    id: String,
    name: String,
    base_url: String,
    api_key_masked: String,
    wire_api: &'static str,
    /// Como a conexão se autentica (spec 116): `api_key` ou `chatgpt_plan`; e, desde a spec 132,
    /// `anthropic` ou `google_gemini`.
    kind: &'static str,
    /// Endereço avançado das conexões nativas (spec 132), quando houver.
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url_override: Option<String>,
    /// Estado do login da conexão pelo plano ChatGPT (spec 116), sem segredos.
    #[serde(skip_serializing_if = "Option::is_none")]
    chatgpt: Option<super::chatgpt::PlanStatus>,
}

#[derive(Deserialize)]
struct ConnectionInput {
    id: String,
    name: String,
    /// Obrigatória na conexão por chave; ignorada na conexão pelo plano ChatGPT (fixa).
    #[serde(default)]
    base_url: String,
    /// Vazia em atualização mantém a chave já salva.
    #[serde(default)]
    api_key: String,
    /// Ausente em atualização mantém o valor salvo (`chat` em conexão nova).
    #[serde(default)]
    wire_api: Option<WireApi>,
    /// Ausente = o tipo salvo (por chave em conexão nova). A conexão pelo plano ChatGPT só nasce do
    /// login (spec 116); por aqui, ela só troca de nome. Texto livre para que um tipo desconhecido
    /// dê o código próprio (`connection_kind_invalid`).
    #[serde(default)]
    kind: Option<String>,
    /// Endereço avançado das conexões nativas (spec 132): vazio volta ao oficial; ausente mantém o
    /// salvo. Recusado nos outros tipos.
    #[serde(default)]
    base_url_override: Option<String>,
}

/// Corpo de `POST /api/config/connections/test`: `{id}` testa uma conexão salva;
/// os demais campos definem uma conexão a testar (chave vazia reutiliza a salva).
#[derive(Deserialize)]
struct TestInput {
    id: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    /// Sem modelo, o teste consulta a listagem; com modelo, faz uma chat completion.
    model: Option<String>,
    /// Tipo da conexão ainda não salva (spec 132 T08): `anthropic` ou `google_gemini` testam pelo
    /// endereço oficial (ou pelo avançado), sem `base_url`. Ausente = o tipo salvo, ou por chave.
    #[serde(default)]
    kind: Option<String>,
    /// Endereço avançado da conexão nativa a testar (spec 132 T08); ausente mantém o salvo.
    #[serde(default)]
    base_url_override: Option<String>,
}

/// Resultado do teste de conexão. `error` é sempre um código fixo; nenhum texto
/// livre (de reqwest, URL ou corpo remoto) sai por aqui.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TestResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Quantidade de modelos da listagem (só no teste sem modelo).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_count: Option<usize>,
}

/// Máscara da chave: `env:NOME` é exibido como está; literais mostram só os
/// últimos 4 caracteres (se houver mais de 8). Nunca devolve a chave completa.
pub fn mask_api_key(key: &str) -> String {
    // Só `env:NOME` com nome de variável válido é passthrough; o resto é tratado como literal.
    if let Some(name) = key.strip_prefix(ENV_PREFIX) {
        if valid_env_name(name) {
            return key.to_string();
        }
    }
    let chars: Vec<char> = key.chars().collect();
    if chars.len() > MASK_MIN_LEN {
        let tail: String = chars[chars.len() - MASK_VISIBLE_CHARS..].iter().collect();
        format!("{MASK}{tail}")
    } else {
        MASK.to_string()
    }
}

/// Marcador exibido no lugar de qualquer campo inválido da leitura.
pub const INVALID_VIEW: &str = "[inválido]";

/// Devolve `value` se `validate` aceitar; senão `INVALID_VIEW`. Nunca devolve o valor cru inválido.
pub fn sanitize_for_view(value: &str, validate: impl Fn(&str) -> Result<(), ApiError>) -> String {
    match validate(value) {
        Ok(()) => value.to_string(),
        Err(_) => INVALID_VIEW.to_string(),
    }
}

/// Tamanho máximo de `path` de projeto na leitura (caminhos Linux).
const MAX_PATH_BYTES: usize = 4096;

fn validate_path(value: &str) -> Result<(), ApiError> {
    if value.len() > MAX_PATH_BYTES || value.chars().any(char::is_control) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "field_path_invalid", Map::new()));
    }
    Ok(())
}

fn connection_view(conn: &Connection) -> ConnectionView {
    ConnectionView {
        id: sanitize_for_view(&conn.id, validate_connection_id),
        name: sanitize_for_view(&conn.name, |v| validate_text_field("name", v)),
        base_url: display_base_url(&conn.base_url),
        api_key_masked: if conn.is_chatgpt_plan() { String::new() } else { mask_api_key(&conn.api_key) },
        wire_api: conn.wire_api.as_str(),
        kind: conn.kind.as_str(),
        base_url_override: conn.base_url_override.as_deref().map(display_base_url),
        chatgpt: None,
    }
}

fn agent_view(agent: &AgentConfig) -> AgentView {
    AgentView {
        connection_id: sanitize_for_view(&agent.connection_id, |v| {
            if v.is_empty() {
                Ok(())
            } else {
                validate_connection_id(v)
            }
        }),
        model: sanitize_for_view(&agent.model, |v| validate_text_field("model", v)),
        effort: agent.effort,
        max_steps: agent.max_steps,
    }
}

fn agents_view(agents: &Agents) -> AgentsView {
    AgentsView {
        architect: agent_view(&agents.architect),
        developer: agent_view(&agents.developer),
        reviewer: agent_view(&agents.reviewer),
    }
}

fn config_view(cfg: &Config, data_dir: Option<&Path>, chatgpt: &super::chatgpt::ChatgptAuth) -> ConfigView {
    let connection_view_with_status = |conn: &Connection| {
        let mut view = connection_view(conn);
        if conn.is_chatgpt_plan() {
            view.chatgpt = Some(chatgpt.status(data_dir, &conn.id));
        }
        view
    };
    ConfigView {
        server: ServerView {
            host: sanitize_for_view(&cfg.server.host, |v| validate_text_field("host", v)),
            port: cfg.server.port,
        },
        connections: cfg.connections.iter().map(connection_view_with_status).collect(),
        agents: agents_view(&cfg.agents),
        pricing: cfg.pricing.clone(),
        projects_root: cfg.projects_root.as_ref().map(|r| sanitize_for_view(r, validate_path_like)),
        projects_root_effective: cfg
            .projects_root_path()
            .and_then(|p| p.to_str().map(|t| sanitize_for_view(t, validate_path_like))),
        projects: cfg
            .projects
            .iter()
            .map(|p| ProjectView {
                id: sanitize_for_view(&p.id, validate_connection_id),
                name: sanitize_for_view(&p.name, |v| validate_text_field("name", v)),
                path: sanitize_for_view(&p.path, validate_path),
                created_at: sanitize_for_view(&p.created_at, |v| {
                    validate_text_field("created_at", v)
                }),
            })
            .collect(),
    }
}

/// Valida `id`: `[a-zA-Z0-9_-]{1,64}`.
pub fn validate_connection_id(id: &str) -> Result<(), ApiError> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "id_invalid", Map::new()))
    }
}

/// Valida `base_url` (http ou https com host, sem userinfo) e devolve a forma
/// normalizada (esquema e host em minúsculas, sem barra final).
pub fn validate_base_url(raw: &str) -> Result<String, ApiError> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|_| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_invalid", Map::new()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_scheme", Map::new()));
    }
    if url.host_str().is_none() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_no_host", Map::new()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_credentials", Map::new()));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_query", Map::new()));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// Forma de `base_url` segura para resposta: sem userinfo, query nem fragmento,
/// mesmo em configs antigas editadas à mão.
fn display_base_url(raw: &str) -> String {
    match reqwest::Url::parse(raw) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.as_str().trim_end_matches('/').to_string()
        }
        Err(_) => "[url inválida]".to_string(),
    }
}

/// Limite de tamanho (caracteres) para campos de texto livre devolvidos ao cliente.
pub const MAX_TEXT_CHARS: usize = 128;

/// `name` e `model`: até 128 caracteres, sem caracteres de controle.
pub fn validate_text_field(field: &str, value: &str) -> Result<(), ApiError> {
    if value.chars().count() > MAX_TEXT_CHARS || value.chars().any(char::is_control) {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "text_field_invalid",
            super::error::params([("field", json!(field)), ("max", json!(MAX_TEXT_CHARS))]),
        ));
    }
    Ok(())
}

/// Nome de variável de ambiente: `^[A-Za-z_][A-Za-z0-9_]{0,63}$`.
pub fn valid_env_name(name: &str) -> bool {
    let first_ok = matches!(name.as_bytes().first(), Some(b) if b.is_ascii_alphabetic() || *b == b'_');
    first_ok
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Chave com prefixo `env:` deve referenciar um nome de variável válido.
fn validate_api_key(key: &str) -> Result<(), ApiError> {
    match key.strip_prefix(ENV_PREFIX) {
        Some(name) if !valid_env_name(name) => Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_env_invalid", Map::new())),
        _ => Ok(()),
    }
}

/// Decodifica o corpo. JSON malformado vira 400; campos com tipo/valor inválido, 422.
/// Mensagens de tipo não ecoam valores, para não vazar chaves informadas por engano.
pub(super) fn parse_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields", Map::new())
        } else {
            ApiError::coded(StatusCode::BAD_REQUEST, "json_malformed", Map::new())
        }
    })
}

/// Grava a configuração de forma atômica: arquivo temporário com modo 0600 no
/// mesmo diretório e `rename` final. Em falha, remove o temporário.
pub fn save_atomic(path: &Path, cfg: &Config) -> Result<(), ConfigError> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "caminho de configuração sem nome de arquivo",
        )
    })?;
    let mut tmp_name = OsString::from(".");
    tmp_name.push(file_name);
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);

    if let Err(e) = cfg.save_to(&tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

pub(super) async fn persist(state: &AppState, cfg: &Config) -> Result<(), ApiError> {
    let path = state.config_path().to_path_buf();
    let snapshot = cfg.clone();
    tokio::task::spawn_blocking(move || save_atomic(&path, &snapshot))
        .await
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "config_save_failed", Map::new()))?
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "config_save_failed", Map::new()))
}

/// `GET /api/config`
pub async fn get_config(State(state): State<AppState>) -> Response {
    let cfg = state.config().read().await;
    let mut view = serde_json::to_value(config_view(&cfg, state.data_dir(), state.chatgpt())).unwrap_or_else(|_| serde_json::json!({}));
    // Acesso pela rede (spec 097): porta do Nants, faixa de visualização e WSL.
    // Versão do Nants (spec 103 T03): vem só do `Cargo.toml`, mostrada em Configurações.
    if let Some(obj) = view.as_object_mut() {
        obj.insert("network".into(), super::app::network_info(&state));
        obj.insert("version".into(), serde_json::Value::from(crate::version()));
    }
    Json(view).into_response()
}

/// `PUT /api/config/connections`: cria ou atualiza (upsert por `id`).
pub async fn put_connection(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input: ConnectionInput = parse_body(&body)?;
    validate_connection_id(&input.id)?;
    validate_text_field("name", &input.name)?;

    let requested_kind = match input.kind.as_deref() {
        Some(raw) => Some(ConnectionKind::parse(raw).ok_or_else(|| {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "connection_kind_invalid", Map::new())
        })?),
        None => None,
    };

    let mut guard = state.config().write().await;
    let mut next = guard.clone();
    let position = next.connections.iter().position(|c| c.id == input.id);
    let saved_kind = position.and_then(|i| next.connections.get(i)).map(|c| c.kind);
    let kind = requested_kind.or(saved_kind).unwrap_or_default();
    // Spec 116: a conexão pelo plano ChatGPT nasce do login e, por aqui, só troca de nome; o tipo
    // de uma conexão salva não muda (spec 132: vale também para a Anthropic e o Gemini).
    if kind == ConnectionKind::ChatgptPlan && saved_kind != Some(ConnectionKind::ChatgptPlan) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "chatgpt_plan_needs_login", Map::new()));
    }
    if saved_kind.is_some_and(|saved| saved != kind) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "connection_kind_fixed", Map::new()));
    }
    if !kind.is_native() && input.base_url_override.as_deref().is_some_and(|u| !u.trim().is_empty()) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_override_not_allowed", Map::new()));
    }
    match kind {
        ConnectionKind::ChatgptPlan => {
            let Some(conn) = position.and_then(|i| next.connections.get_mut(i)) else {
                return Err(ApiError::coded(StatusCode::NOT_FOUND, "connection_not_found", Map::new()));
            };
            conn.name = input.name;
            conn.normalize();
            let view = connection_view(conn);
            persist(&state, &next).await?;
            *guard = next;
            return Ok((StatusCode::OK, Json(view)).into_response());
        }
        ConnectionKind::Anthropic | ConnectionKind::GoogleGemini => {
            let saved = position.and_then(|i| next.connections.get(i));
            let connection = native_connection(input, kind, saved)?;
            return save_connection(&state, guard, next, position, connection).await;
        }
        ConnectionKind::ApiKey => {}
    }
    let base_url = validate_base_url(&input.base_url)?;
    let provided_key = input.api_key.trim();
    let saved_wire_api = position
        .and_then(|i| next.connections.get(i))
        .map(|c| c.wire_api)
        .unwrap_or_default();
    let wire_api = input.wire_api.unwrap_or(saved_wire_api);

    let api_key = if provided_key.is_empty() {
        // Chave salva só é mantida para o mesmo endpoint (mesma normalização do teste de conexão).
        let saved = position.and_then(|i| next.connections.get(i)).ok_or_else(|| {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_required_new", Map::new())
        })?;
        let same_endpoint = validate_base_url(&saved.base_url)
            .is_ok_and(|saved_url| saved_url == base_url);
        if !same_endpoint {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_required_base_url", Map::new()));
        }
        saved.api_key.clone()
    } else {
        validate_api_key(provided_key)?;
        provided_key.to_string()
    };

    let connection = Connection {
        id: input.id,
        name: input.name,
        base_url,
        api_key,
        wire_api,
        kind: Default::default(),
        base_url_override: None,
    };
    save_connection(&state, guard, next, position, connection).await
}

/// Conexão nativa (Anthropic ou Google Gemini, spec 132) a partir do corpo do `PUT`: endereço
/// oficial fixo, protocolo do tipo e o endereço avançado opcional validado. A chave salva só é
/// mantida para o mesmo endereço efetivo, como na conexão por chave.
fn native_connection(input: ConnectionInput, kind: ConnectionKind, saved: Option<&Connection>) -> Result<Connection, ApiError> {
    let base_url_override = match input.base_url_override.as_deref().map(str::trim) {
        Some("") => None,
        Some(raw) => Some(validate_base_url(raw).map_err(|_| {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_override_invalid", Map::new())
        })?),
        None => saved.and_then(|c| c.base_url_override.clone()),
    };
    let mut connection = Connection {
        id: input.id,
        name: input.name,
        base_url: String::new(),
        api_key: String::new(),
        wire_api: WireApi::Chat,
        kind,
        base_url_override,
    };
    connection.normalize();
    let provided_key = input.api_key.trim();
    connection.api_key = if provided_key.is_empty() {
        let saved = saved.ok_or_else(|| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_required_new", Map::new()))?;
        if saved.endpoint() != connection.endpoint() {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_required_base_url", Map::new()));
        }
        saved.api_key.clone()
    } else {
        validate_api_key(provided_key)?;
        provided_key.to_string()
    };
    Ok(connection)
}

/// Grava a conexão criada ou atualizada (na posição `position`, ou no fim) e responde com a vista
/// dela, sem segredo.
async fn save_connection(
    state: &AppState,
    mut guard: tokio::sync::RwLockWriteGuard<'_, Config>,
    mut next: Config,
    position: Option<usize>,
    connection: Connection,
) -> Result<Response, ApiError> {
    let connection_id = connection.id.clone();
    let view = connection_view(&connection);
    match position {
        Some(i) if i < next.connections.len() => next.connections[i] = connection,
        _ => next.connections.push(connection),
    }

    persist(state, &next).await?;
    *guard = next;
    // Listagens em cache da conexão deixam de valer após a troca de endpoint ou chave.
    state.models_cache().invalidate_connection(&connection_id);
    Ok((StatusCode::OK, Json(view)).into_response())
}

fn validate_agent(
    role: &str,
    agent: &AgentConfig,
    connections: &[Connection],
) -> Result<(), ApiError> {
    if !connections.iter().any(|c| c.id == agent.connection_id) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "connection_missing", super::error::params([("role", json!(role))])));
    }
    if agent.model.trim().is_empty() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "model_required", super::error::params([("role", json!(role))])));
    }
    validate_text_field("model", &agent.model)?;
    validate_max_steps(role, agent.max_steps)
}

/// `max_steps` fora de [`MAX_STEPS_RANGE`] é rejeitado com 422 (spec 081 T01; decisão do Arquiteto:
/// segue a convenção deste módulo para valor semanticamente inválido, no lugar do 400 citado no
/// critério). Sem valor, o papel usa o padrão. No uso, o valor ainda é ajustado à faixa (config
/// editada à mão).
fn validate_max_steps(role: &str, max_steps: Option<u32>) -> Result<(), ApiError> {
    match max_steps {
        Some(n) if !MAX_STEPS_RANGE.contains(&n) => Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "max_steps_range",
            super::error::params([
                ("role", json!(role)),
                ("min", json!(MAX_STEPS_RANGE.start())),
                ("max", json!(MAX_STEPS_RANGE.end())),
            ]),
        )),
        _ => Ok(()),
    }
}

/// Texto de caminho seguro para exibição: até 4096 bytes e sem controle.
fn validate_path_like(raw: &str) -> Result<(), ApiError> {
    if raw.len() > super::projects::MAX_PATH_BYTES || raw.chars().any(char::is_control) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "path_invalid", Map::new()));
    }
    Ok(())
}

/// Corpo de `PUT /api/config/projects-root`: `null` volta ao padrão (`~/NantsProjects`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectsRootInput {
    projects_root: Option<String>,
}

/// `PUT /api/config/projects-root` (spec 093 T02): define a raiz da pasta de projetos.
/// Responde `{projects_root, projects_root_effective}`; raiz inválida é 422 com mensagem estática.
pub async fn put_projects_root(State(state): State<AppState>, body: Bytes) -> Result<Response, ApiError> {
    let input: ProjectsRootInput = parse_body(&body)?;
    let raw = input.projects_root.map(|r| r.trim().to_string());
    let mut guard = state.config().write().await;
    let effective = super::projects_root::validate_root(
        raw.as_deref().unwrap_or(crate::config::DEFAULT_PROJECTS_ROOT),
        &guard,
        std::env::var_os("HOME"),
    )?;
    let mut next = guard.clone();
    next.projects_root = raw;
    persist(&state, &next).await?;
    let response = Json(serde_json::json!({
        "projects_root": next.projects_root,
        "projects_root_effective": effective.display().to_string(),
    }))
    .into_response();
    *guard = next;
    Ok(response)
}

/// `PUT /api/config/agents`: substitui a configuração dos três papéis.
pub async fn put_agents(State(state): State<AppState>, body: Bytes) -> Result<Response, ApiError> {
    let agents: Agents = parse_body(&body)?;

    let mut guard = state.config().write().await;
    validate_agent("architect", &agents.architect, &guard.connections)?;
    validate_agent("developer", &agents.developer, &guard.connections)?;
    validate_agent("reviewer", &agents.reviewer, &guard.connections)?;

    let mut next = guard.clone();
    next.agents = agents;
    persist(&state, &next).await?;
    let response = Json(agents_view(&next.agents)).into_response();
    *guard = next;
    drop(guard);
    // Segundo plano: pré-cadastra a estimativa dos modelos em uso sem atrasar a resposta.
    let bg = state.clone();
    tokio::spawn(async move {
        let _ = autofill_pricing(&bg, false).await;
    });
    Ok(response)
}

/// Modelos em uso pelos três agentes (sem repetição, sem vazios), com o tipo da conexão de cada um
/// (spec 132 T02: o preço do catálogo segue o provedor). O mesmo modelo em conexões de tipos
/// diferentes fica com o tipo nativo, quando houver.
fn models_in_use(cfg: &Config) -> Vec<(String, ConnectionKind)> {
    let mut out: Vec<(String, ConnectionKind)> = Vec::new();
    for agent in [&cfg.agents.architect, &cfg.agents.developer, &cfg.agents.reviewer] {
        if agent.model.trim().is_empty() {
            continue;
        }
        let kind = cfg
            .connections
            .iter()
            .find(|c| c.id == agent.connection_id)
            .map_or(ConnectionKind::ApiKey, |c| c.kind);
        match out.iter_mut().find(|(m, _)| *m == agent.model) {
            Some(entry) if kind.is_native() && !entry.1.is_native() => entry.1 = kind,
            Some(_) => {}
            None => out.push((agent.model.clone(), kind)),
        }
    }
    out
}

/// Grava a estimativa dos modelos em uso sem preço manual (e, com `refresh`, renova as já
/// estimadas). Rede/catálogo indisponível: não muda nada. Devolve o mapa de preços vigente.
async fn autofill_pricing(
    state: &AppState,
    refresh: bool,
) -> Result<std::collections::BTreeMap<String, Pricing>, ApiError> {
    let models = models_in_use(&*state.config().read().await);
    let dir = state.config_path().parent().map(Path::to_path_buf).unwrap_or_default();
    let catalog = price_catalog::current_catalog(state.price_fetcher(), &dir, refresh).await;
    let mut guard = state.config().write().await;
    if let Some(catalog) = catalog {
        let mut next = guard.clone();
        if price_catalog::apply_estimates(&mut next.pricing, &models, &catalog, refresh) {
            persist(state, &next).await?;
            *guard = next;
        }
    }
    Ok(guard.pricing.clone())
}

#[derive(Deserialize)]
pub struct EstimateQuery {
    #[serde(default)]
    models: String,
    #[serde(default)]
    refresh: bool,
    /// Tipo da conexão dos modelos pedidos (spec 132 T10: a sugestão de modelo de uma conexão nova,
    /// cujos modelos ainda não estão em uso). Ausente = o tipo da conexão do agente que usa o modelo.
    #[serde(default)]
    kind: Option<String>,
}

/// `GET /api/config/pricing/estimate?models=a,b`: estimativas do catálogo para os modelos
/// encontrados. Não grava nada. Modelo não encontrado fica de fora.
pub async fn get_estimate(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<EstimateQuery>,
) -> Result<Response, ApiError> {
    let mut out = std::collections::BTreeMap::new();
    let names: Vec<&str> = q.models.split(',').map(str::trim).filter(|m| !m.is_empty()).take(64).collect();
    for name in &names {
        validate_text_field("model", name)?;
    }
    let requested_kind = match q.kind.as_deref() {
        Some(raw) => Some(ConnectionKind::parse(raw).ok_or_else(|| {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "connection_kind_invalid", Map::new())
        })?),
        None => None,
    };
    let in_use = models_in_use(&*state.config().read().await);
    let dir = state.config_path().parent().map(Path::to_path_buf).unwrap_or_default();
    if let Some(catalog) = price_catalog::current_catalog(state.price_fetcher(), &dir, q.refresh).await {
        for name in names {
            // Spec 132 T02: o modelo em uso por um agente segue o tipo da conexão dele.
            // Com `kind` no pedido, ele vale para todos os modelos (spec 132 T10).
            let kind = requested_kind.unwrap_or_else(|| in_use.iter().find(|(m, _)| m == name).map_or(ConnectionKind::ApiKey, |(_, k)| *k));
            if let Some(p) = catalog.lookup_for(name, kind) {
                out.insert(name.to_string(), p);
            }
        }
    }
    Ok(Json(out).into_response())
}

/// `POST /api/config/pricing/autofill[?refresh=true]`: aplica as estimativas aos modelos em uso
/// (preço manual nunca é sobrescrito) e devolve o mapa de preços.
pub async fn post_autofill(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<EstimateQuery>,
) -> Result<Response, ApiError> {
    Ok(Json(autofill_pricing(&state, q.refresh).await?).into_response())
}

/// `PUT /api/config/pricing`: substitui o mapa `modelo -> preço` (US$ por 1M tokens).
/// Valores precisam ser finitos e >= 0; o nome do modelo segue a validação de texto comum.
pub async fn put_pricing(State(state): State<AppState>, body: Bytes) -> Result<Response, ApiError> {
    let pricing: std::collections::BTreeMap<String, Pricing> = parse_body(&body)?;
    for (model, price) in &pricing {
        validate_text_field("model", model)?;
        if model.trim().is_empty() || !price.is_valid() {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "price_invalid", Map::new()));
        }
    }
    let mut guard = state.config().write().await;
    let mut next = guard.clone();
    next.pricing = pricing;
    persist(&state, &next).await?;
    let response = Json(&next.pricing).into_response();
    *guard = next;
    Ok(response)
}

/// Substitui `scheme://usuario:senha@` por `scheme://***@` em todo o texto.
///
/// Uma região de userinfo+host termina no primeiro `/` ou espaço (ou fim do texto),
/// ou no início de outro esquema (`https://a:b@h1https://c:d@h2`, JSON com várias URLs).
/// Dentro da região, tudo até o último `@` é mascarado. Caracteres como `( ) ' " < > ? #`
/// são válidos em userinfo (RFC 3986), por isso não encerram a região.
///
/// Esta função é melhor esforço para texto livre. Limitações aceitas (fixadas em teste):
/// - F-03: `/` não codificado dentro de userinfo encerra a região, e barras invertidas
///   ou aspas simples não são tratadas como delimitadores. A barreira principal é
///   `SecretSet::scrub_exact`, que troca os valores secretos conhecidos.
/// - F-04: texto que segue a URL e contém `@` (ex.: e-mail numa query) é mascarado
///   até o último `@` da região (mascara-se a mais).
pub fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("://") {
        let (head, tail) = rest.split_at(pos + 3);
        out.push_str(head);
        let delim = tail
            .find(|c: char| c == '/' || c.is_whitespace())
            .unwrap_or(tail.len());
        let end = match tail.find("://") {
            Some(next) if next < delim => scheme_start(tail, next),
            _ => delim,
        };
        let (authority, after) = tail.split_at(end);
        match authority.rfind('@') {
            Some(at) => {
                out.push_str("***@");
                out.push_str(&authority[at + 1..]);
            }
            None => out.push_str(authority),
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Início do esquema (`[A-Za-z][A-Za-z0-9+.-]*`) que termina em `sep`; sem esquema válido,
/// devolve `sep`. Garante que cada iteração de `redact_urls` consome o `://` encontrado.
fn scheme_start(tail: &str, sep: usize) -> usize {
    let prefix = &tail[..sep];
    let cut = prefix
        .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        .len();
    if prefix[cut..].starts_with(|c: char| c.is_ascii_alphabetic()) {
        cut
    } else {
        sep
    }
}

/// Resolve a conexão a testar e o modelo opcional (`None` = teste pela listagem).
/// Lê a configuração sem segurar o lock durante a chamada de rede.
async fn resolve_test_target(
    state: &AppState,
    input: &TestInput,
) -> Result<(Connection, Option<String>), ApiError> {
    if let Some(model) = input.model.as_deref() {
        validate_text_field("model", model)?;
    }
    let requested_kind = match input.kind.as_deref() {
        Some(raw) => Some(ConnectionKind::parse(raw).ok_or_else(|| {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "connection_kind_invalid", Map::new())
        })?),
        None => None,
    };
    let guard = state.config().read().await;
    let saved = input
        .id
        .as_deref()
        .and_then(|id| guard.connections.iter().find(|c| c.id == id));
    if let (Some(kind), Some(saved)) = (requested_kind, saved) {
        if kind != saved.kind {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "connection_kind_fixed", Map::new()));
        }
    }
    if !requested_kind.or(saved.map(|c| c.kind)).is_some_and(ConnectionKind::is_native)
        && input.base_url_override.as_deref().is_some_and(|u| !u.trim().is_empty())
    {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_override_not_allowed", Map::new()));
    }

    let conn = match (&input.base_url, saved) {
        // Plano ChatGPT (spec 116 T02): endereço fixo e sem chave; testa a conexão salva, com o
        // token do login.
        (_, Some(saved)) if saved.is_chatgpt_plan() => {
            let model = input.model.as_deref().map(str::trim).filter(|m| !m.is_empty()).map(str::to_string);
            return Ok((saved.clone(), model));
        }
        // Anthropic e Google Gemini (spec 132): o endereço é o oficial ou o avançado, nunca o
        // `base_url` do corpo; a chave do corpo, quando houver, substitui a salva.
        (_, Some(saved)) if saved.kind.is_native() => native_test_target(&input, saved.kind, Some(saved))?,
        // Conexão nativa ainda não salva (spec 132 T08): o tipo vem no corpo, com a chave.
        (_, None) if requested_kind.is_some_and(ConnectionKind::is_native) => {
            native_test_target(&input, requested_kind.unwrap_or_default(), None)?
        }
        (None, Some(saved)) => {
            // Conexão salva antes da validação atual (ex.: editada à mão): não usar.
            validate_base_url(&saved.base_url).map_err(|_| {
                ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_saved_invalid", Map::new())
            })?;
            saved.clone()
        }
        (None, None) => {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_or_connection", Map::new()))
        }
        (Some(url), _) => {
            let normalized_url = validate_base_url(url)?;
            // A chave salva só é reutilizada para o mesmo endpoint, nunca para outro host.
            let same_endpoint_as_saved = saved.is_some_and(|c| {
                validate_base_url(&c.base_url).is_ok_and(|saved_url| saved_url == normalized_url)
            });
            let api_key = match input.api_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
                Some(key) => {
                    validate_api_key(key)?;
                    key.to_string()
                }
                None => match saved {
                    Some(c) if same_endpoint_as_saved => c.api_key.clone(),
                    Some(_) => {
                        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_required_other_url", Map::new()))
                    }
                    None => return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_required", Map::new())),
                },
            };
            Connection {
                id: input.id.clone().unwrap_or_default(),
                name: String::new(),
                base_url: normalized_url,
                api_key,
                wire_api: saved.map(|c| c.wire_api).unwrap_or_default(),
                kind: Default::default(),
                base_url_override: None,
            }
        }
    };

    // Sem modelo explícito o teste usa a listagem; o modelo do Arquiteto não é herdado.
    let model = input
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string);
    drop(guard);
    // Vale também para conexões salvas à mão: a referência env: precisa ser válida.
    validate_api_key(&conn.api_key)?;
    Ok((conn, model))
}

/// Conexão nativa a testar (spec 132 T08): o tipo `kind`, o endereço avançado do corpo (vazio volta
/// ao oficial; ausente mantém o salvo) e a chave do corpo ou a salva. A chave salva só vale para o
/// mesmo endereço.
fn native_test_target(input: &TestInput, kind: ConnectionKind, saved: Option<&Connection>) -> Result<Connection, ApiError> {
    let base_url_override = match input.base_url_override.as_deref().map(str::trim) {
        Some("") => None,
        Some(raw) => Some(validate_base_url(raw).map_err(|_| {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "base_url_override_invalid", Map::new())
        })?),
        None => saved.and_then(|c| c.base_url_override.clone()),
    };
    let mut conn = Connection::native(input.id.clone().unwrap_or_default(), String::new(), kind, String::new());
    conn.base_url_override = base_url_override;
    conn.normalize();
    conn.api_key = match (input.api_key.as_deref().map(str::trim).filter(|k| !k.is_empty()), saved) {
        (Some(key), _) => {
            validate_api_key(key)?;
            key.to_string()
        }
        (None, Some(saved)) if saved.endpoint() == conn.endpoint() => saved.api_key.clone(),
        (None, Some(_)) => {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_required_other_url", Map::new()))
        }
        (None, None) => return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "api_key_required", Map::new())),
    };
    if let Some(saved) = saved {
        conn.name = saved.name.clone();
    }
    Ok(conn)
}

/// Teste pela listagem (sem modelo) com timeout `TEST_TIMEOUT`.
async fn run_models_probe(conn: &Connection) -> TestResult {
    run_models_probe_with(conn, TEST_TIMEOUT).await
}

/// Teste sem modelo: a listagem pelo mesmo caminho e parser de `GET .../models`, no endereço da
/// conexão ([`Connection::endpoint`], que respeita o avançado das nativas). Não usa nem grava o
/// cache de modelos. Códigos fixos, sem eco de corpo ou URL.
pub async fn run_models_probe_with(conn: &Connection, timeout: Duration) -> TestResult {
    models_probe_listing(conn, timeout).await.0
}

/// O teste pela listagem e, no sucesso, os modelos listados (na ordem do provedor nas conexões
/// nativas).
async fn models_probe_listing(conn: &Connection, timeout: Duration) -> (TestResult, Option<Vec<models_api::ModelInfo>>) {
    let started = Instant::now();
    match models_api::fetch(conn, conn.endpoint(), timeout).await {
        Ok(list) => (
            TestResult {
                ok: true,
                latency_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
                error: None,
                status: None,
                models_count: Some(list.models.len()),
            },
            Some(list.models),
        ),
        Err(failure) => (
            TestResult {
                ok: false,
                latency_ms: None,
                error: Some(failure.code),
                status: failure.status,
                models_count: None,
            },
            None,
        ),
    }
}

/// Resposta do teste sem modelo das conexões nativas (spec 132 T10): o resultado de sempre e a
/// lista de modelos, para a tela de nova conexão mostrar o que a chave alcança antes de salvar.
/// Os ids passam pelo mesmo filtro da listagem (`valid_model_id`), que recusa o segredo.
#[derive(Serialize)]
struct NativeProbeBody {
    #[serde(flatten)]
    result: TestResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    models: Option<Vec<models_api::ModelInfo>>,
}

/// Handshake mínimo com timeout `TEST_TIMEOUT`.
async fn run_handshake(conn: &Connection, model: String) -> TestResult {
    run_handshake_with(conn, model, TEST_TIMEOUT).await
}

/// Teste da conexão pelo plano ChatGPT (spec 116 T02), com o token do login: com modelo, o mesmo
/// handshake (em streaming, como toda chamada do plano); sem modelo, a lista de modelos da conta.
pub async fn run_plan_test(state: &AppState, conn: &Connection, model: Option<String>, timeout: Duration) -> TestResult {
    let started = Instant::now();
    let elapsed = || Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    let Some(model) = model else {
        return match models_api::fetch_plan(state, conn, timeout).await {
            Ok(list) => TestResult { ok: true, latency_ms: elapsed(), error: None, status: None, models_count: Some(list.models.len()) },
            Err(failure) => TestResult { ok: false, latency_ms: None, error: Some(failure.code), status: failure.status, models_count: None },
        };
    };
    let client = match super::chatgpt::plan_client::llm_client(state, conn).and_then(|c| c.with_timeout(timeout)) {
        Ok(client) => client.with_throttle_delays(Vec::new()).without_pacing(),
        Err(e) => return classify_error(&e),
    };
    handshake(&client, model).await
}

/// Handshake mínimo: uma chat completion com "ping". O resultado usa apenas
/// códigos fixos; erros de transporte, URL e corpo remoto nunca são devolvidos.
pub async fn run_handshake_with(conn: &Connection, model: String, timeout: Duration) -> TestResult {
    // O teste de conexão relata o limite na hora (`provider_http_429`), sem o cronograma longo de
    // espera da spec 089 (que deixaria o botão "Testar" parado por minutos), e fica fora do
    // limitador de ritmo (089 T02): nem espera nem ensina o ritmo do par.
    let client = match LlmClient::from_connection(conn).and_then(|c| c.with_timeout(timeout)) {
        Ok(client) => client.with_throttle_delays(Vec::new()).without_pacing(),
        Err(e) => return classify_error(&e),
    };
    // Spec 132 T08: nos nativos, a chave recusada, a falta de permissão, o modelo inexistente, o
    // limite de taxa e o teto de gasto têm códigos próprios.
    match ping(&client, model).await {
        Ok(result) => result,
        Err(e) if conn.kind.is_native() => native_result(&e),
        Err(e) => classify_error(&e),
    }
}

/// Resultado do teste nativo pelo código de [`super::models_native::classify`] (com modelo).
fn native_result(err: &LlmError) -> TestResult {
    let failure = super::models_native::classify(err, true);
    TestResult { ok: false, latency_ms: None, error: Some(failure.code), status: failure.status, models_count: None }
}

/// Uma chamada "ping" pelo cliente pronto; só códigos fixos no resultado.
async fn handshake(client: &LlmClient, model: String) -> TestResult {
    ping(client, model).await.unwrap_or_else(|e| classify_error(&e))
}

/// A chamada "ping": o resultado de sucesso, ou o erro para quem escolhe o código.
async fn ping(client: &LlmClient, model: String) -> Result<TestResult, LlmError> {
    let req = ChatRequest {
        model,
        messages: vec![ChatMessage::text(Role::User, "ping")],
        tools: Vec::new(),
        effort: None,
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let started = Instant::now();
    client.chat(&req).await?;
    Ok(TestResult {
        ok: true,
        latency_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        error: None,
        status: None,
        models_count: None,
    })
}

/// Mapeia `LlmError` para código fixo. Nunca usa a mensagem do erro.
fn classify_error(err: &LlmError) -> TestResult {
    let (code, status) = match err {
        LlmError::Config(_) => ("invalid_config", None),
        LlmError::Timeout => ("timeout", None),
        LlmError::Transport(_) => ("connect_failed", None),
        LlmError::Http { status, .. } => ("http_error", Some(*status)),
        LlmError::Parse(_) => ("bad_response", None),
        // Plano ChatGPT (spec 116 T02): `plan_limit`, `plan_not_eligible` ou `plan_needs_login`.
        LlmError::Plan(plan) => (plan.as_str(), None),
        // Spec 132 T09: `spend_limit` ou `daily_quota`.
        LlmError::Limit(limit) => (limit.as_str(), None),
        // Spec 132 T01: Anthropic e Google Gemini ainda sem adaptador.
        LlmError::KindNotSupported(_) => (models_api::KIND_NOT_SUPPORTED.code, None),
    };
    TestResult {
        ok: false,
        latency_ms: None,
        error: Some(code),
        status,
        models_count: None,
    }
}

/// `POST /api/config/connections/test`: com `model`, chat completion; sem ele, listagem.
pub async fn test_connection(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input: TestInput = parse_body(&body)?;
    let (conn, model) = resolve_test_target(&state, &input).await?;
    if conn.is_chatgpt_plan() {
        let result = run_plan_test(&state, &conn, model, TEST_TIMEOUT).await;
        return Ok((StatusCode::OK, Json(result)).into_response());
    }
    let result = match model {
        Some(model) => run_handshake(&conn, model).await,
        None if conn.kind.is_native() => {
            let (result, models) = models_probe_listing(&conn, TEST_TIMEOUT).await;
            return Ok((StatusCode::OK, Json(NativeProbeBody { result, models })).into_response());
        }
        None => run_models_probe(&conn).await,
    };
    Ok((StatusCode::OK, Json(result)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod put_key_rules {
        use super::*;
        use serde_json::json;

        const KEY: &str = "sk-SALVA-1234";

        fn state_with_saved(base_url: &str) -> (AppState, std::path::PathBuf) {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!("nants-putkey-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("dir");
            let cfg = Config {
                connections: vec![Connection {
                    id: "conn".to_string(),
                    name: "Original".to_string(),
                    base_url: base_url.to_string(),
                    api_key: KEY.to_string(),
                    wire_api: WireApi::default(),
                    kind: Default::default(),
                    base_url_override: None,
                }],
                ..Config::default()
            };
            let path = dir.join("config.toml");
            (AppState::new(path.clone(), cfg, 3000), path)
        }

        /// Estado com uma conexão `conn` salva com o protocolo `wire`.
        fn state_with_wire(wire: WireApi) -> AppState {
            let dir = std::env::temp_dir().join(format!(
                "nants-wire-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).expect("dir");
            let cfg = Config {
                connections: vec![Connection {
                    id: "conn".to_string(),
                    name: "Original".to_string(),
                    base_url: "https://api.example.com/v1".to_string(),
                    api_key: KEY.to_string(),
                    wire_api: wire,
                    kind: Default::default(),
                    base_url_override: None,
                }],
                ..Config::default()
            };
            AppState::new(dir.join("config.toml"), cfg, 3000)
        }

        async fn get_view(state: &AppState) -> serde_json::Value {
            let resp = get_config(State(state.clone())).await;
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .expect("corpo");
            serde_json::from_slice(&bytes).expect("json")
        }

        #[tokio::test]
        async fn wire_api_put_responses_e_chat_sao_aceitos_e_visiveis_no_get() {
            let state = state_with_wire(WireApi::Chat);
            put(
                &state,
                json!({"id":"conn","name":"Original","base_url":"https://api.example.com/v1","api_key":"","wire_api":"responses"}),
            )
            .await
            .expect("responses aceito");
            assert_eq!(saved_conn(&state).wire_api, WireApi::Responses);
            let view = get_view(&state).await;
            assert_eq!(view["connections"][0]["wire_api"], "responses", "{view}");

            put(
                &state,
                json!({"id":"conn","name":"Original","base_url":"https://api.example.com/v1","api_key":"","wire_api":"chat"}),
            )
            .await
            .expect("chat aceito");
            assert_eq!(saved_conn(&state).wire_api, WireApi::Chat);
            let view = get_view(&state).await;
            assert_eq!(view["connections"][0]["wire_api"], "chat", "{view}");
        }

        #[tokio::test]
        async fn wire_api_put_sem_campo_mantem_o_valor_salvo() {
            let state = state_with_wire(WireApi::Responses);
            put(
                &state,
                json!({"id":"conn","name":"Novo nome","base_url":"https://api.example.com/v1","api_key":""}),
            )
            .await
            .expect("ok");
            assert_eq!(saved_conn(&state).name, "Novo nome");
            assert_eq!(saved_conn(&state).wire_api, WireApi::Responses);
            assert_eq!(get_view(&state).await["connections"][0]["wire_api"], "responses");
        }

        #[tokio::test]
        async fn wire_api_put_conexao_nova_sem_campo_e_chat() {
            let state = state_with_wire(WireApi::Responses);
            put(
                &state,
                json!({"id":"nova","name":"Nova","base_url":"https://other.example.org/v1","api_key":"sk-NOVA-9999"}),
            )
            .await
            .expect("ok");
            let nova = state.config().try_read().expect("leitura").connections[1].clone();
            assert_eq!(nova.wire_api, WireApi::Chat);
        }

        #[tokio::test]
        async fn wire_api_put_invalido_da_422_e_nao_grava() {
            let state = state_with_wire(WireApi::Chat);
            let err = put(
                &state,
                json!({"id":"conn","name":"Original","base_url":"https://api.example.com/v1","api_key":"","wire_api":"foo"}),
            )
            .await
            .expect_err("422");
            assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(saved_conn(&state).wire_api, WireApi::Chat);
        }

        #[tokio::test]
        async fn wire_api_nao_altera_segredos_expostos() {
            let state = state_with_wire(WireApi::Responses);
            let view = get_view(&state).await;
            let conn = &view["connections"][0];
            assert!(conn.get("api_key").is_none(), "{view}");
            assert_eq!(conn["api_key_masked"], "********1234", "{view}");
            assert_eq!(conn["wire_api"], "responses", "{view}");
        }

        async fn put(state: &AppState, body: serde_json::Value) -> Result<StatusCode, ApiError> {
            let bytes = Bytes::from(body.to_string());
            put_connection(State(state.clone()), bytes)
                .await
                .map(|r| r.status())
        }

        fn saved_conn(state: &AppState) -> Connection {
            state.config().try_read().expect("leitura").connections[0].clone()
        }

        #[tokio::test]
        async fn mesmo_base_url_com_chave_vazia_mantem_a_chave() {
            let (state, _) = state_with_saved("https://api.example.com/v1");
            let status = put(
                &state,
                json!({"id":"conn","name":"Novo nome","provider_type":"openai","base_url":"https://api.example.com/v1","api_key":""}),
            )
            .await
            .expect("ok");
            assert_eq!(status, StatusCode::OK);
            assert_eq!(saved_conn(&state).api_key, KEY);
            assert_eq!(saved_conn(&state).name, "Novo nome");
        }

        #[tokio::test]
        async fn normalizacao_de_barra_final_e_host_mantem_a_chave() {
            let (state, _) = state_with_saved("https://api.example.com/v1");
            put(
                &state,
                json!({"id":"conn","name":"Original","base_url":"HTTPS://API.EXAMPLE.COM/v1/","api_key":""}),
            )
            .await
            .expect("ok");
            assert_eq!(saved_conn(&state).api_key, KEY);
        }

        #[tokio::test]
        async fn base_url_diferente_com_chave_vazia_da_422_e_nao_grava() {
            let (state, path) = state_with_saved("https://api.example.com/v1");
            let err = put(
                &state,
                json!({"id":"conn","name":"Trocado","provider_type":"openai","base_url":"https://evil.example.org/v1","api_key":""}),
            )
            .await
            .expect_err("422");
            assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let saved = saved_conn(&state);
            assert_eq!(saved.base_url, "https://api.example.com/v1");
            assert_eq!(saved.api_key, KEY);
            assert_eq!(saved.name, "Original");
            assert!(!path.exists(), "nada deve ser gravado em disco");
        }

        #[tokio::test]
        async fn base_url_diferente_com_chave_nova_aceita_e_troca_a_chave() {
            let (state, _) = state_with_saved("https://api.example.com/v1");
            put(
                &state,
                json!({"id":"conn","name":"Trocado","provider_type":"openai","base_url":"https://other.example.org/v1","api_key":"sk-NOVA-9999"}),
            )
            .await
            .expect("ok");
            let saved = saved_conn(&state);
            assert_eq!(saved.base_url, "https://other.example.org/v1");
            assert_eq!(saved.api_key, "sk-NOVA-9999");
        }

        #[tokio::test]
        async fn campos_legados_do_put_sao_ignorados_e_mantem_a_chave() {
            let (state, _) = state_with_saved("https://api.example.com/v1");
            put(
                &state,
                json!({"id":"conn","name":"Original","provider_type":"azure","base_url":"https://api.example.com/v1","api_key":"","api_version":"sk-SEGREDO bad"}),
            )
            .await
            .expect("ok");
            assert_eq!(saved_conn(&state).api_key, KEY);
            let view = serde_json::to_value(connection_view(&saved_conn(&state))).expect("json");
            assert!(view.get("provider_type").is_none(), "{view}");
            assert!(view.get("api_version").is_none(), "{view}");
            let toml = toml::to_string(&Config { connections: vec![saved_conn(&state)], ..Config::default() })
                .expect("toml");
            assert!(!toml.contains("provider_type") && !toml.contains("api_version"), "{toml}");
        }
    }

    #[test]
    fn env_prefix_with_key_shaped_value_is_masked_not_echoed() {
        let raw = "env:sk-abc123segredo";
        let masked = mask_api_key(raw);
        assert_ne!(masked, raw);
        assert!(!masked.contains("segredo"), "{masked}");
        assert!(validate_api_key(raw).is_err());
    }

    #[test]
    fn valid_env_name_follows_pattern() {
        assert!(valid_env_name("OPENAI_API_KEY"));
        assert!(valid_env_name("_x1"));
        assert!(valid_env_name("sk_abc123"));
        assert!(valid_env_name(&"A".repeat(64)));
        assert!(!valid_env_name(""));
        assert!(!valid_env_name("1ABC"));
        assert!(!valid_env_name("sk-abc"));
        assert!(!valid_env_name("A B"));
        assert!(!valid_env_name(&"A".repeat(65)));
        assert!(!valid_env_name("çabc"));
    }

    #[test]
    fn env_reference_passthrough_only_for_valid_names() {
        assert_eq!(mask_api_key("env:OPENAI_API_KEY"), "env:OPENAI_API_KEY");
        assert_eq!(mask_api_key("env:sk_abc123"), "env:sk_abc123");
        assert!(validate_api_key("env:OPENAI_API_KEY").is_ok());
        assert!(validate_api_key("env:").is_err());
        assert!(validate_api_key("env:A B").is_err());
        assert!(validate_api_key(&format!("env:{}", "A".repeat(65))).is_err());
    }

    #[test]
    fn redact_urls_removes_userinfo_from_every_url() {
        let msg = "erro em https://user:SEGREDO@host/v1 e depois http://a:b@127.0.0.1:9/x?q=1 fim";
        let out = redact_urls(msg);
        assert!(!out.contains("SEGREDO"), "{out}");
        assert!(!out.contains("a:b@"), "{out}");
        assert_eq!(
            out,
            "erro em https://***@host/v1 e depois http://***@127.0.0.1:9/x?q=1 fim"
        );
    }

    #[test]
    fn redact_urls_masks_userinfo_with_reserved_or_quote_chars() {
        for input in [
            "https://u:pa(ss)@h",
            "https://u:pa'ss@h",
            "https://u:pa\"ss@h/x",
            "https://u:p<a>ss@h",
            "https://u:pa?ss@h/x",
            "https://u:pa#ss@h/x",
        ] {
            let out = redact_urls(input);
            assert!(!out.contains("pa"), "{input} -> {out}");
            assert!(!out.contains("ss"), "{input} -> {out}");
            assert!(!out.contains("u:"), "{input} -> {out}");
            assert!(out.starts_with("https://***@h"), "{input} -> {out}");
        }
    }

    #[test]
    fn redact_urls_handles_uppercase_scheme_and_several_urls() {
        assert_eq!(
            redact_urls("HTTPS://a:b@h1 e http://c:d@h2/p"),
            "HTTPS://***@h1 e http://***@h2/p"
        );
        assert_eq!(redact_urls("https://u:p@h/a@b"), "https://***@h/a@b");
    }

    #[test]
    fn redact_urls_chained_urls_are_all_masked() {
        let out = redact_urls("https://a:b@h1https://c:d@h2");
        assert!(!out.contains("a:b") && !out.contains("c:d"), "{out}");
        assert_eq!(out, "https://***@h1https://***@h2");
        let json = r#"{"a":"https://u:p@h","b":"https://c:d@h2/x"}"#;
        let out = redact_urls(json);
        assert!(!out.contains("u:p") && !out.contains("c:d"), "{out}");
        assert_eq!(out, r#"{"a":"https://***@h","b":"https://***@h2/x"}"#);
        let out = redact_urls("http://u:senha@a.com e https://c:d@b.org/p");
        assert_eq!(out, "http://***@a.com e https://***@b.org/p");
    }

    #[test]
    fn redact_urls_documented_limitations_are_pinned() {
        // F-03: '/' não codificado encerra a região (não é userinfo válido).
        assert_eq!(redact_urls("https://u:p/x@h"), "https://u:p/x@h");
        // F-04: e-mail numa query após a URL é mascarado a mais (conservador).
        assert_eq!(
            redact_urls("https://h.com?mail=fulano@x.com"),
            "https://***@x.com"
        );
    }

    #[test]
    fn redact_urls_keeps_text_without_userinfo() {
        assert_eq!(redact_urls("sem url"), "sem url");
        assert_eq!(
            redact_urls("https://api.openai.com/v1/chat"),
            "https://api.openai.com/v1/chat"
        );
        // '@' só no caminho não é userinfo.
        assert_eq!(redact_urls("https://host/v1/@algo"), "https://host/v1/@algo");
    }
}

#[cfg(test)]
mod pricing_estimate_tests {
    use super::*;
    use crate::server::price_catalog::StaticPriceFetcher;
    use std::sync::Arc;

    const FIXTURE: &str = r#"{"sample_spec":{},
      "gpt-4o":{"input_cost_per_token":0.0000025,"output_cost_per_token":0.00001,"cache_read_input_token_cost":0.00000125},
      "claude-x":{"input_cost_per_token":0.000003,"output_cost_per_token":0.000015}}"#;

    fn state(models: [&str; 3], manual: Option<(&str, f64)>, net: bool) -> AppState {
        let dir = std::env::temp_dir().join(format!(
            "nants-est-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let mut cfg = Config::default();
        cfg.agents.architect.model = models[0].to_string();
        cfg.agents.developer.model = models[1].to_string();
        cfg.agents.reviewer.model = models[2].to_string();
        if let Some((m, v)) = manual {
            cfg.pricing.insert(
                m.to_string(),
                Pricing { input_per_mtok: v, output_per_mtok: v, cached_input_per_mtok: None, cache_write_per_mtok: None, web_search_per_call: None, image_per_unit: None, source: Default::default(), estimated_at: None },
            );
        }
        let body = net.then(|| FIXTURE.as_bytes().to_vec());
        AppState::new(dir.join("config.toml"), cfg, 3000).with_price_fetcher(Arc::new(StaticPriceFetcher(body)))
    }

    async fn json_of(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("corpo");
        serde_json::from_slice(&bytes).expect("json")
    }

    fn q(models: &str) -> axum::extract::Query<EstimateQuery> {
        axum::extract::Query(EstimateQuery { models: models.to_string(), refresh: false, kind: None })
    }

    #[tokio::test]
    async fn estimate_devolve_so_os_encontrados() {
        let st = state(["a", "b", "c"], None, true);
        let v = json_of(get_estimate(State(st.clone()), q("gpt-4o, openai/claude-x,nada")).await.expect("ok")).await;
        assert_eq!(v["gpt-4o"]["input_per_mtok"], 2.5);
        assert_eq!(v["gpt-4o"]["cached_input_per_mtok"], 1.25);
        assert_eq!(v["gpt-4o"]["source"], "estimated");
        assert_eq!(v["openai/claude-x"]["output_per_mtok"], 15.0);
        assert!(v.get("nada").is_none());
        assert!(st.config().read().await.pricing.is_empty(), "estimate não grava");
    }

    #[tokio::test]
    async fn autofill_grava_estimado_sem_sobrescrever_manual() {
        let st = state(["gpt-4o", "claude-x", "desconhecido"], Some(("claude-x", 7.0)), true);
        let v = json_of(post_autofill(State(st.clone()), q("")).await.expect("ok")).await;
        assert_eq!(v["gpt-4o"]["source"], "estimated");
        assert!(v["gpt-4o"]["estimated_at"].is_string());
        assert_eq!(v["claude-x"]["input_per_mtok"], 7.0);
        assert_eq!(v["claude-x"]["source"], "manual");
        assert!(v.get("desconhecido").is_none());
        let saved = std::fs::read_to_string(st.config_path()).expect("persistido");
        assert!(saved.contains("estimated"));
    }

    /// Spec 132 T02: o modelo de um agente numa conexão nativa usa o preço do próprio provedor
    /// (`gemini/<id>`, `anthropic/<id>`), no autofill e na estimativa; fora de uso, o id exato.
    #[tokio::test]
    async fn conexao_nativa_usa_o_preco_do_proprio_provedor() {
        const NATIVOS: &str = r#"{
          "gemini-y":{"input_cost_per_token":0.000001,"output_cost_per_token":0.000004},
          "gemini/gemini-y":{"input_cost_per_token":0.0000003,"output_cost_per_token":0.0000025},
          "claude-y":{"input_cost_per_token":0.000009,"output_cost_per_token":0.00002},
          "anthropic/claude-y":{"input_cost_per_token":0.000003,"output_cost_per_token":0.000015,"cache_creation_input_token_cost":0.00000375}}"#;
        let dir = std::env::temp_dir().join(format!("nants-est-nativo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let mut cfg = Config::default();
        cfg.connections.push(Connection::native("g", "Gemini", ConnectionKind::GoogleGemini, "env:NANTS_TESTE_G"));
        cfg.connections.push(Connection::native("a", "Anthropic", ConnectionKind::Anthropic, "env:NANTS_TESTE_A"));
        cfg.agents.architect.model = "gemini-y".into();
        cfg.agents.architect.connection_id = "g".into();
        cfg.agents.developer.model = "claude-y".into();
        cfg.agents.developer.connection_id = "a".into();
        cfg.agents.reviewer.model = "gemini-y".into();
        let st = AppState::new(dir.join("config.toml"), cfg, 3000)
            .with_price_fetcher(Arc::new(StaticPriceFetcher(Some(NATIVOS.as_bytes().to_vec()))));
        let est = json_of(get_estimate(State(st.clone()), q("gemini-y,claude-y")).await.expect("ok")).await;
        assert_eq!(est["gemini-y"]["input_per_mtok"], 0.3, "o tipo nativo vence o revisor sem conexão");
        assert_eq!(est["claude-y"]["input_per_mtok"], 3.0);
        assert_eq!(est["claude-y"]["cache_write_per_mtok"], 3.75);
        // Spec 132 T10: com o `kind` no pedido, o modelo fora de uso usa o preço do provedor.
        let by_kind = |models: &str, kind: &str| axum::extract::Query(EstimateQuery { models: models.into(), refresh: false, kind: Some(kind.into()) });
        let est = json_of(get_estimate(State(st.clone()), by_kind("gemini-y", "google_gemini")).await.expect("ok")).await;
        assert_eq!(est["gemini-y"]["input_per_mtok"], 0.3);
        let est = json_of(get_estimate(State(st.clone()), by_kind("claude-y", "api_key")).await.expect("ok")).await;
        assert_eq!(est["claude-y"]["input_per_mtok"], 9.0, "o kind do pedido vence o do agente");
        // Spec 132 T11: o `kind` desconhecido é um 422 com o código, não só um erro qualquer.
        let Err(err) = get_estimate(State(st.clone()), by_kind("claude-y", "azure")).await else {
            panic!("kind inválido aceito");
        };
        assert_eq!((err.status(), err.code()), (StatusCode::UNPROCESSABLE_ENTITY, Some("connection_kind_invalid")));
        let v = json_of(post_autofill(State(st.clone()), q("")).await.expect("ok")).await;
        assert_eq!(v["gemini-y"]["input_per_mtok"], 0.3);
        assert_eq!(v["claude-y"]["output_per_mtok"], 15.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rota_autofill_com_json_vazio_responde_200_e_sem_content_type_415() {
        use axum::body::Body;
        use axum::http::{header, Method, Request, StatusCode};
        use tower::ServiceExt;
        let st = state(["gpt-4o", "x", "x"], None, true);
        let mk = |ct: bool| {
            let mut b = Request::builder().method(Method::POST).uri("/api/config/pricing/autofill").header(header::HOST, "127.0.0.1:3000");
            if ct {
                b = b.header(header::CONTENT_TYPE, "application/json");
            }
            b.body(Body::from(if ct { "{}" } else { "" })).expect("req")
        };
        let resp = crate::server::router(st.clone()).oneshot(mk(true)).await.expect("resp");
        assert_eq!(resp.status(), StatusCode::OK);
        let v = json_of(resp).await;
        assert_eq!(v["gpt-4o"]["source"], "estimated");
        assert!(st.config().read().await.pricing.contains_key("gpt-4o"));
        let resp = crate::server::router(st).oneshot(mk(false)).await.expect("resp");
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn salvar_agentes_preenche_estimativa_e_sem_rede_nao_falha() {
        let st = state(["x", "x", "x"], None, true);
        {
            let mut cfg = st.config().write().await;
            cfg.connections.push(Connection {
                id: "c".into(), name: "n".into(), base_url: "https://a.example/v1".into(),
                api_key: "k".into(), wire_api: WireApi::default(),
                kind: Default::default(),
                base_url_override: None,
            });
        }
        let body = r#"{"architect":{"connection_id":"c","model":"gpt-4o","effort":"low"},
            "developer":{"connection_id":"c","model":"gpt-4o","effort":"low"},
            "reviewer":{"connection_id":"c","model":"zzz","effort":"low"}}"#;
        put_agents(State(st.clone()), Bytes::from(body)).await.expect("ok");
        for _ in 0..200 {
            if st.config().read().await.pricing.contains_key("gpt-4o") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let cfg = st.config().read().await;
        assert!(cfg.pricing.contains_key("gpt-4o") && !cfg.pricing.contains_key("zzz"));
        drop(cfg);
        let offline = state(["gpt-4o", "gpt-4o", "gpt-4o"], None, false);
        let v = json_of(post_autofill(State(offline), q("")).await.expect("ok")).await;
        assert_eq!(v, serde_json::json!({}));
    }

    #[tokio::test]
    async fn get_config_traz_a_versao_do_cargo_toml() {
        let st = state(["x", "x", "x"], None, false);
        let got = json_of(get_config(State(st)).await).await;
        assert_eq!(got["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn max_steps_faz_ida_e_volta_no_put_e_get_e_fora_da_faixa_da_422() {
        let st = state(["x", "x", "x"], None, false);
        {
            let mut cfg = st.config().write().await;
            cfg.connections.push(Connection {
                id: "c".into(), name: "n".into(), base_url: "https://a.example/v1".into(),
                api_key: "k".into(), wire_api: WireApi::default(),
                kind: Default::default(),
                base_url_override: None,
            });
        }
        let body = |dev: &str| format!(
            r#"{{"architect":{{"connection_id":"c","model":"m","effort":"low"}},
            "developer":{{"connection_id":"c","model":"m","effort":"low"{dev}}},
            "reviewer":{{"connection_id":"c","model":"m","effort":"high","max_steps":60}}}}"#
        );
        let v = json_of(put_agents(State(st.clone()), Bytes::from(body(r#","max_steps":200"#))).await.expect("ok")).await;
        assert_eq!(v["developer"]["max_steps"], 200);
        assert_eq!(v["reviewer"]["max_steps"], 60);
        assert!(v["architect"].get("max_steps").is_none(), "sem valor, omitido: {v}");
        let got = json_of(get_config(State(st.clone())).await).await;
        assert_eq!(got["agents"]["developer"]["max_steps"], 200);
        assert_eq!(got["agents"]["reviewer"]["max_steps"], 60);
        let saved = std::fs::read_to_string(st.config_path()).expect("persistido");
        assert!(saved.contains("max_steps = 200"), "{saved}");
        let cfg = st.config().read().await.clone();
        assert_eq!(cfg.agents.developer.max_steps_or(crate::agents::llm_agents::DEV_MAX_STEPS), 200);

        for bad in [r#","max_steps":9"#, r#","max_steps":1001"#] {
            let err = put_agents(State(st.clone()), Bytes::from(body(bad))).await.expect_err("fora da faixa");
            assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert!(err.message().contains("entre 10 e 1000"), "{}", err.message());
        }
        assert_eq!(st.config().read().await.agents.developer.max_steps, Some(200), "não grava inválido");
        // Sem o campo: volta ao padrão do papel.
        put_agents(State(st.clone()), Bytes::from(body(""))).await.expect("ok");
        assert_eq!(st.config().read().await.agents.developer.max_steps, None);
    }

    /// Spec 132 T01: conexões Anthropic e Google Gemini pelas rotas de configuração.
    mod native_kinds {
        use super::*;
        use crate::config::{ANTHROPIC_BASE_URL, GOOGLE_GEMINI_BASE_URL};
        use serde_json::json;

        const ENV_KEY: &str = "env:NANTS_T132_ANTHROPIC";
        const LITERAL: &str = "valor-literal-qualquer-7788";

        fn empty_state() -> AppState {
            let dir = std::env::temp_dir().join(format!(
                "nants-t132-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).expect("dir");
            AppState::new(dir.join("config.toml"), Config::default(), 3000)
        }

        async fn put(state: &AppState, body: serde_json::Value) -> Result<serde_json::Value, ApiError> {
            let resp = put_connection(State(state.clone()), Bytes::from(body.to_string())).await?;
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("corpo");
            Ok(serde_json::from_slice(&bytes).expect("json"))
        }

        async fn put_code(state: &AppState, body: serde_json::Value) -> Option<&'static str> {
            let err = put(state, body).await.expect_err("recusa");
            assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
            err.code()
        }

        async fn view(state: &AppState) -> String {
            let resp = get_config(State(state.clone())).await;
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("corpo");
            String::from_utf8(bytes.to_vec()).expect("utf8")
        }

        fn saved(state: &AppState, id: &str) -> Option<Connection> {
            state.config().try_read().expect("leitura").connections.iter().find(|c| c.id == id).cloned()
        }

        #[tokio::test]
        async fn cria_anthropic_e_gemini_com_url_fixa_e_get_mostra_o_tipo_sem_segredo() {
            let st = empty_state();
            let v = put(&st, json!({"id":"anth","name":"Claude","kind":"anthropic","base_url":"https://outro.example.com","api_key":ENV_KEY,"wire_api":"responses"}))
                .await
                .expect("anthropic");
            assert_eq!((v["kind"].as_str(), v["base_url"].as_str()), (Some("anthropic"), Some(ANTHROPIC_BASE_URL)));
            put(&st, json!({"id":"gem","name":"Gemini","kind":"google_gemini","api_key":LITERAL})).await.expect("gemini");

            let anth = saved(&st, "anth").expect("salva");
            assert_eq!(
                (anth.kind, anth.base_url.as_str(), anth.wire_api, anth.api_key.as_str()),
                (ConnectionKind::Anthropic, ANTHROPIC_BASE_URL, WireApi::Chat, ENV_KEY)
            );
            let gem = saved(&st, "gem").expect("salva");
            assert_eq!((gem.kind, gem.base_url.as_str()), (ConnectionKind::GoogleGemini, GOOGLE_GEMINI_BASE_URL));

            let body = view(&st).await;
            let json: serde_json::Value = serde_json::from_str(&body).expect("json");
            let conns = json["connections"].as_array().expect("lista");
            assert_eq!(conns[0]["kind"], "anthropic", "{body}");
            assert_eq!(conns[0]["api_key_masked"], ENV_KEY, "{body}");
            assert_eq!(conns[1]["kind"], "google_gemini", "{body}");
            assert_eq!(conns[1]["api_key_masked"], "********7788", "{body}");
            assert!(conns.iter().all(|c| c.get("api_key").is_none() && c.get("base_url_override").is_none()), "{body}");
            assert!(!body.contains(LITERAL), "{body}");

            let toml = std::fs::read_to_string(st.config_path()).expect("gravado");
            assert!(toml.contains("kind = \"anthropic\"") && toml.contains("kind = \"google_gemini\""), "{toml}");
            let current = st.config().try_read().expect("leitura").clone();
            assert_eq!(Config::load_from(st.config_path()).expect("recarrega"), current);
        }

        #[tokio::test]
        async fn tipo_invalido_da_422_com_codigo_e_nao_grava() {
            let st = empty_state();
            for kind in ["azure", "", "Anthropic"] {
                let code = put_code(&st, json!({"id":"x","name":"x","kind":kind,"base_url":"https://h.example.com","api_key":ENV_KEY})).await;
                assert_eq!(code, Some("connection_kind_invalid"), "{kind:?}");
            }
            let code = put_code(&st, json!({"id":"x","name":"x","kind":7,"api_key":ENV_KEY})).await;
            assert_eq!(code, Some("json_invalid_fields"));
            assert!(st.config().try_read().expect("leitura").connections.is_empty());
            assert!(!st.config_path().exists());
        }

        #[tokio::test]
        async fn endereco_avancado_e_validado_e_so_vale_nas_nativas() {
            let st = empty_state();
            put(&st, json!({"id":"gem","name":"G","kind":"google_gemini","api_key":ENV_KEY,"base_url_override":"HTTPS://Proxy.Example.com/gemini/"}))
                .await
                .expect("ok");
            let gem = saved(&st, "gem").expect("salva");
            assert_eq!(gem.base_url_override.as_deref(), Some("https://proxy.example.com/gemini"));
            assert_eq!(gem.base_url, GOOGLE_GEMINI_BASE_URL);
            assert!(view(&st).await.contains("\"base_url_override\":\"https://proxy.example.com/gemini\""));

            for bad in ["ftp://h.example.com", "https://u:p@h.example.com", "https://h.example.com/?q=1", "nada"] {
                let code = put_code(&st, json!({"id":"gem","name":"G","api_key":ENV_KEY,"base_url_override":bad})).await;
                assert_eq!(code, Some("base_url_override_invalid"), "{bad}");
            }
            let code = put_code(
                &st,
                json!({"id":"k","name":"K","base_url":"https://h.example.com/v1","api_key":ENV_KEY,"base_url_override":"https://p.example.com"}),
            )
            .await;
            assert_eq!(code, Some("base_url_override_not_allowed"));

            // Sem o campo, o salvo fica; vazio volta ao oficial (com a chave, porque o endereço mudou).
            put(&st, json!({"id":"gem","name":"Outro nome","api_key":""})).await.expect("mantém");
            assert_eq!(saved(&st, "gem").expect("salva").endpoint(), "https://proxy.example.com/gemini");
            let code = put_code(&st, json!({"id":"gem","name":"G","api_key":"","base_url_override":""})).await;
            assert_eq!(code, Some("api_key_required_base_url"));
            put(&st, json!({"id":"gem","name":"G","api_key":ENV_KEY,"base_url_override":""})).await.expect("oficial");
            let gem = saved(&st, "gem").expect("salva");
            assert_eq!((gem.base_url_override.as_deref(), gem.endpoint()), (None, GOOGLE_GEMINI_BASE_URL));
        }

        #[tokio::test]
        async fn tipo_salvo_nao_muda_e_chave_vazia_mantem_a_salva() {
            let st = empty_state();
            let code = put_code(&st, json!({"id":"anth","name":"A","kind":"anthropic","api_key":""})).await;
            assert_eq!(code, Some("api_key_required_new"));
            let code = put_code(&st, json!({"id":"anth","name":"A","kind":"anthropic","api_key":"env:1INVALIDA"})).await;
            assert_eq!(code, Some("api_key_env_invalid"));
            put(&st, json!({"id":"anth","name":"A","kind":"anthropic","api_key":ENV_KEY})).await.expect("cria");
            for kind in ["google_gemini", "api_key"] {
                let code = put_code(&st, json!({"id":"anth","name":"A","kind":kind,"base_url":"https://h.example.com","api_key":ENV_KEY})).await;
                assert_eq!(code, Some("connection_kind_fixed"), "{kind}");
            }
            let code = put_code(&st, json!({"id":"anth","name":"A","kind":"chatgpt_plan"})).await;
            assert_eq!(code, Some("chatgpt_plan_needs_login"));
            put(&st, json!({"id":"anth","name":"Novo","api_key":""})).await.expect("renomeia");
            let anth = saved(&st, "anth").expect("salva");
            assert_eq!((anth.name.as_str(), anth.kind, anth.api_key.as_str()), ("Novo", ConnectionKind::Anthropic, ENV_KEY));
            // A conexão por chave também não vira nativa.
            put(&st, json!({"id":"k","name":"K","base_url":"https://h.example.com/v1","api_key":ENV_KEY})).await.expect("por chave");
            let code = put_code(&st, json!({"id":"k","name":"K","kind":"anthropic","api_key":ENV_KEY})).await;
            assert_eq!(code, Some("connection_kind_fixed"));
        }

        #[tokio::test]
        async fn teste_e_listagem_das_nativas_dao_codigo_sem_ir_a_rede() {
            let st = empty_state();
            put(&st, json!({"id":"anth","name":"A","kind":"anthropic","api_key":ENV_KEY})).await.expect("cria");
            put(&st, json!({"id":"gem","name":"G","kind":"google_gemini","api_key":ENV_KEY})).await.expect("cria");
            let cases = [
                json!({"id":"anth"}),
                json!({"id":"gem","base_url":"https://outro.example.com"}),
            ];
            for body in cases {
                let resp = test_connection(State(st.clone()), Bytes::from(body.to_string())).await.expect("ok");
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("corpo");
                let result: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
                assert_eq!(result["ok"], false, "{body}");
                // Spec 132 T08: a lista já vai pela Models API do provedor; a chave `env:` indefinida
                // para antes da rede, como configuração inválida.
                assert_eq!(result["error"], "invalid_config", "{body}");
            }
            // Spec 132: com modelo, os nativos já vão pelo adaptador (Anthropic na T03, Gemini na T06);
            // a chave `env:` indefinida para antes da rede, como configuração inválida.
            for body in [json!({"id":"anth","model":"m"}), json!({"id":"gem","model":"m","api_key":ENV_KEY})] {
                let resp = test_connection(State(st.clone()), Bytes::from(body.to_string())).await.expect("ok");
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("corpo");
                let result: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
                assert_eq!((result["ok"].clone(), result["error"].clone()), (json!(false), json!("invalid_config")), "{body}");
            }
            let anth = saved(&st, "anth").expect("salva");
            let failure = models_api::fetch(&anth, anth.endpoint(), Duration::from_secs(1)).await.expect_err("sem listagem");
            assert_eq!(failure.code, "invalid_config");
        }
    }
}
