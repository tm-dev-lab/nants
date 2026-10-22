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

use super::error::ApiError;
use super::models_api;
use super::price_catalog;
use super::state::AppState;
use crate::agents::llm_client::{ChatMessage, ChatRequest, LlmClient, Role};
use crate::agents::LlmError;
use crate::config::{
    AgentConfig, Agents, Config, ConfigError, Connection, Effort, Pricing, WireApi, MAX_STEPS_RANGE,
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
}

#[derive(Deserialize)]
struct ConnectionInput {
    id: String,
    name: String,
    base_url: String,
    /// Vazia em atualização mantém a chave já salva.
    #[serde(default)]
    api_key: String,
    /// Ausente em atualização mantém o valor salvo (`chat` em conexão nova).
    #[serde(default)]
    wire_api: Option<WireApi>,
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
        return Err(ApiError::unprocessable("path inválido"));
    }
    Ok(())
}

fn connection_view(conn: &Connection) -> ConnectionView {
    ConnectionView {
        id: sanitize_for_view(&conn.id, validate_connection_id),
        name: sanitize_for_view(&conn.name, |v| validate_text_field("name", v)),
        base_url: display_base_url(&conn.base_url),
        api_key_masked: mask_api_key(&conn.api_key),
        wire_api: conn.wire_api.as_str(),
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

fn config_view(cfg: &Config) -> ConfigView {
    ConfigView {
        server: ServerView {
            host: sanitize_for_view(&cfg.server.host, |v| validate_text_field("host", v)),
            port: cfg.server.port,
        },
        connections: cfg.connections.iter().map(connection_view).collect(),
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
        Err(ApiError::unprocessable(
            "id deve conter apenas [a-zA-Z0-9_-] e ter de 1 a 64 caracteres",
        ))
    }
}

/// Valida `base_url` (http ou https com host, sem userinfo) e devolve a forma
/// normalizada (esquema e host em minúsculas, sem barra final).
pub fn validate_base_url(raw: &str) -> Result<String, ApiError> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|_| ApiError::unprocessable("base_url inválida"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::unprocessable("base_url deve usar http ou https"));
    }
    if url.host_str().is_none() {
        return Err(ApiError::unprocessable("base_url sem host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::unprocessable(
            "base_url não pode conter usuário ou senha; use api_key",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ApiError::unprocessable(
            "base_url não pode conter query ou fragmento; use api_key",
        ));
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
        return Err(ApiError::unprocessable(format!(
            "{field} inválido: máximo de {MAX_TEXT_CHARS} caracteres, sem caracteres de controle"
        )));
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
        Some(name) if !valid_env_name(name) => Err(ApiError::unprocessable(
            "api_key com prefixo env: deve ser um nome de variável de ambiente válido",
        )),
        _ => Ok(()),
    }
}

/// Decodifica o corpo. JSON malformado vira 400; campos com tipo/valor inválido, 422.
/// Mensagens de tipo não ecoam valores, para não vazar chaves informadas por engano.
pub(super) fn parse_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            ApiError::unprocessable("corpo JSON com campos inválidos")
        } else {
            ApiError::new(StatusCode::BAD_REQUEST, "corpo JSON malformado")
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
        .map_err(|_| ApiError::internal("falha ao salvar a configuração"))?
        .map_err(|_| ApiError::internal("falha ao salvar a configuração"))
}

/// `GET /api/config`
pub async fn get_config(State(state): State<AppState>) -> Response {
    let cfg = state.config().read().await;
    let mut view = serde_json::to_value(config_view(&cfg)).unwrap_or_else(|_| serde_json::json!({}));
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
    let base_url = validate_base_url(&input.base_url)?;
    let provided_key = input.api_key.trim();

    let mut guard = state.config().write().await;
    let mut next = guard.clone();
    let position = next.connections.iter().position(|c| c.id == input.id);
    let saved_wire_api = position
        .and_then(|i| next.connections.get(i))
        .map(|c| c.wire_api)
        .unwrap_or_default();
    let wire_api = input.wire_api.unwrap_or(saved_wire_api);

    let api_key = if provided_key.is_empty() {
        // Chave salva só é mantida para o mesmo endpoint (mesma normalização do teste de conexão).
        let saved = position.and_then(|i| next.connections.get(i)).ok_or_else(|| {
            ApiError::unprocessable("api_key é obrigatória para nova conexão")
        })?;
        let same_endpoint = validate_base_url(&saved.base_url)
            .is_ok_and(|saved_url| saved_url == base_url);
        if !same_endpoint {
            return Err(ApiError::unprocessable(
                "api_key é obrigatória quando base_url muda",
            ));
        }
        saved.api_key.clone()
    } else {
        validate_api_key(provided_key)?;
        provided_key.to_string()
    };

    let connection_id = input.id.clone();
    let connection = Connection {
        id: input.id,
        name: input.name,
        base_url,
        api_key,
        wire_api,
    };
    let view = connection_view(&connection);
    match position {
        Some(i) if i < next.connections.len() => next.connections[i] = connection,
        _ => next.connections.push(connection),
    }

    persist(&state, &next).await?;
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
        return Err(ApiError::unprocessable(format!(
            "{role}: connection_id não existe"
        )));
    }
    if agent.model.trim().is_empty() {
        return Err(ApiError::unprocessable(format!("{role}: model é obrigatório")));
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
        Some(n) if !MAX_STEPS_RANGE.contains(&n) => Err(ApiError::unprocessable(
            format!(
                "{role}: limite de passos deve estar entre {} e {}",
                MAX_STEPS_RANGE.start(),
                MAX_STEPS_RANGE.end()
            ),
        )),
        _ => Ok(()),
    }
}

/// Texto de caminho seguro para exibição: até 4096 bytes e sem controle.
fn validate_path_like(raw: &str) -> Result<(), ApiError> {
    if raw.len() > super::projects::MAX_PATH_BYTES || raw.chars().any(char::is_control) {
        return Err(ApiError::unprocessable("caminho inválido"));
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

/// Modelos em uso pelos três agentes (sem repetição, sem vazios).
fn models_in_use(cfg: &Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in [&cfg.agents.architect.model, &cfg.agents.developer.model, &cfg.agents.reviewer.model] {
        if !m.trim().is_empty() && !out.contains(m) {
            out.push(m.clone());
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
    let dir = state.config_path().parent().map(Path::to_path_buf).unwrap_or_default();
    if let Some(catalog) = price_catalog::current_catalog(state.price_fetcher(), &dir, q.refresh).await {
        for name in names {
            if let Some(p) = catalog.lookup(name) {
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
            return Err(ApiError::unprocessable(
                "preço inválido: use números finitos maiores ou iguais a zero",
            ));
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
    let guard = state.config().read().await;
    let saved = input
        .id
        .as_deref()
        .and_then(|id| guard.connections.iter().find(|c| c.id == id));

    let conn = match (&input.base_url, saved) {
        (None, Some(saved)) => {
            // Conexão salva antes da validação atual (ex.: editada à mão): não usar.
            validate_base_url(&saved.base_url).map_err(|_| {
                ApiError::unprocessable("base_url salva inválida; recrie a conexão")
            })?;
            saved.clone()
        }
        (None, None) => {
            return Err(ApiError::unprocessable(
                "informe base_url ou o id de uma conexão salva",
            ))
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
                        return Err(ApiError::unprocessable(
                            "api_key é obrigatória quando base_url difere da conexão salva",
                        ))
                    }
                    None => return Err(ApiError::unprocessable("api_key é obrigatória")),
                },
            };
            Connection {
                id: input.id.clone().unwrap_or_default(),
                name: String::new(),
                base_url: normalized_url,
                api_key,
                wire_api: saved.map(|c| c.wire_api).unwrap_or_default(),
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

/// Teste pela listagem (sem modelo) com timeout `TEST_TIMEOUT`.
async fn run_models_probe(conn: &Connection) -> TestResult {
    run_models_probe_with(conn, TEST_TIMEOUT).await
}

/// Teste sem modelo: `GET {base_url}/models` pelo mesmo caminho e parser da listagem.
/// Não usa nem grava o cache de modelos. Códigos fixos, sem eco de corpo ou URL.
pub async fn run_models_probe_with(conn: &Connection, timeout: Duration) -> TestResult {
    let started = Instant::now();
    match models_api::fetch(conn, &conn.base_url, timeout).await {
        Ok(list) => TestResult {
            ok: true,
            latency_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            error: None,
            status: None,
            models_count: Some(list.models.len()),
        },
        Err(failure) => TestResult {
            ok: false,
            latency_ms: None,
            error: Some(failure.code),
            status: failure.status,
            models_count: None,
        },
    }
}

/// Handshake mínimo com timeout `TEST_TIMEOUT`.
async fn run_handshake(conn: &Connection, model: String) -> TestResult {
    run_handshake_with(conn, model, TEST_TIMEOUT).await
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
    let req = ChatRequest {
        model,
        messages: vec![ChatMessage::text(Role::User, "ping")],
        tools: Vec::new(),
        effort: None,
        response_json: false,
        web_search: false,
    };
    let started = Instant::now();
    match client.chat(&req).await {
        Ok(_) => TestResult {
            ok: true,
            latency_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            error: None,
            status: None,
            models_count: None,
        },
        Err(e) => classify_error(&e),
    }
}

/// Mapeia `LlmError` para código fixo. Nunca usa a mensagem do erro.
fn classify_error(err: &LlmError) -> TestResult {
    let (code, status) = match err {
        LlmError::Config(_) => ("invalid_config", None),
        LlmError::Timeout => ("timeout", None),
        LlmError::Transport(_) => ("connect_failed", None),
        LlmError::Http { status, .. } => ("http_error", Some(*status)),
        LlmError::Parse(_) => ("bad_response", None),
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
    let result = match model {
        Some(model) => run_handshake(&conn, model).await,
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
                Pricing { input_per_mtok: v, output_per_mtok: v, cached_input_per_mtok: None, web_search_per_call: None, image_per_unit: None, source: Default::default(), estimated_at: None },
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
        axum::extract::Query(EstimateQuery { models: models.to_string(), refresh: false })
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
}
