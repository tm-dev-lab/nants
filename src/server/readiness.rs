//! Prontidão do Nants (spec 093 T02, decisão 15): `GET /api/readiness`,
//! `POST /api/readiness/refresh` e o bloqueio 412 `prereq_missing` das ações.
//!
//! Níveis (cada um `{ok, missing}`):
//! - `chat`: ≥ 1 conexão + Arquiteto com conexão existente e modelo, e endereço e chave dessa conexão
//!   utilizáveis (T12: `connection_url:<id>`, `connection_key:<id>`);
//! - `create_project`: `chat` + Git;
//! - `execute`: Developer e Revisor configurados (conexão utilizável), Git, Docker (CLI, daemon,
//!   permissão) e Compose v2;
//! - `recommended`: preço dos modelos em uso (não bloqueia).
//!
//! Cada pendência é `{id, label, fix?, settings_tab}` com ids fixos. Os pré-requisitos da máquina
//! vêm do [`PrereqChecker`] (cache de 30 s, renovado no `refresh`). As ações que executam (aprovar,
//! retomar, `recovery-choice`, "Continuar" e iniciar o app) conferem a máquina na hora, sem cache,
//! em `spawn_blocking` e com teto total ([`PrereqChecker::fresh_ceiling`]). Nenhum texto remoto,
//! detalhe de comando ou segredo entra na resposta: só ids, rótulos e correções fixas.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::json;

use super::events::{message_params, SecretSet};
use super::prereqs::{fix_for, label_for, PrereqChecker, PrereqItem};
use super::projects::scrub_text;
use super::state::AppState;
use crate::config::{AgentConfig, Config, Connection, ConnectionProblem};
use crate::i18n::{Lang, Msg};

/// Código do erro 412.
pub const PREREQ_MISSING: &str = "prereq_missing";

/// Pré-requisitos da máquina, na ordem de exibição.
const MACHINE_IDS: [&str; 5] = super::prereqs::REQUIRED_IDS;
const DOCKER_IDS: [&str; 4] = ["docker_cli", "docker_daemon", "docker_permission", "docker_compose"];

const TAB_MACHINE: &str = "machine";
const TAB_CONNECTIONS: &str = "connections";
const TAB_AGENTS: &str = "agents";
const TAB_PRICING: &str = "pricing";

/// Pendência de um nível. O `label` é o texto em pt-BR (reserva); a interface monta o texto no
/// idioma ativo pelo `code` e pelos `params` neutros (spec 102 T05).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Missing {
    pub id: String,
    pub label: String,
    /// Código estável do rótulo: o `id` sem o parâmetro (`connection`, `agent_architect`, `git`,
    /// `connection_key`, `connection_url`, `pricing`...).
    pub code: &'static str,
    /// Parâmetros neutros do rótulo (`name` da conexão, `model`), saneados como os de
    /// `message_params`; vazio fica fora do JSON.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub params: serde_json::Map<String, serde_json::Value>,
    /// Comando ou instrução de correção (só texto; o Nants nunca executa).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Aba de destino nas Configurações.
    pub settings_tab: &'static str,
}

/// Um nível de prontidão.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Level {
    pub ok: bool,
    pub missing: Vec<Missing>,
}

impl Level {
    fn of(missing: Vec<Missing>) -> Self {
        Self { ok: missing.is_empty(), missing }
    }

    /// Ids das pendências (corpo do 412).
    pub fn ids(&self) -> Vec<String> {
        self.missing.iter().map(|m| m.id.clone()).collect()
    }
}

/// Corpo de `GET /api/readiness`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Readiness {
    pub chat: Level,
    pub create_project: Level,
    pub execute: Level,
    pub recommended: Level,
}

impl Readiness {
    /// Setup global incompleto (selo e `/api/attention`).
    pub fn setup_incomplete(&self) -> bool {
        !self.chat.ok || !self.execute.ok
    }
}

/// Nível exigido por uma ação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Turno do chat.
    Chat,
    /// Criar projeto.
    CreateProject,
    /// Aprovar, retomar, `recovery-choice`, "Continuar" e iniciar o app (máquina conferida na hora).
    Execute,
}

/// Agente com conexão existente e modelo.
fn agent_ready(cfg: &Config, agent: &AgentConfig) -> bool {
    !agent.model.trim().is_empty() && cfg.connections.iter().any(|c| c.id == agent.connection_id)
}

/// Conexão do agente, quando existe.
fn connection_of<'a>(cfg: &'a Config, agent: &AgentConfig) -> Option<&'a Connection> {
    cfg.connections.iter().find(|c| c.id == agent.connection_id)
}

/// Id da conexão de teste de [`chat_ready`]/[`execute_ready`].
#[cfg(test)]
const TEST_READY_CONN: &str = "nants-test-ready";

/// Problemas da conexão ([`Connection::usable_problems`]).
///
/// Exceção só em `cfg(test)`, restrita ao id [`TEST_READY_CONN`]: a conexão dos helpers de teste
/// conta como utilizável sem montar cliente. Com uma chave resolvível, cadastrar um projeto
/// existente nos testes dispararia o levantamento inicial do Arquiteto, que grava na conversa do
/// projeto (`.sdd/specs/.chat.jsonl`) e ocupa as reservas por até ~40 s de novas tentativas, mudando
/// o comportamento de testes existentes. Fora dos testes este desvio não existe.
fn connection_problems(conn: &Connection, plan_ready: &dyn Fn(&str) -> bool) -> Vec<ConnectionProblem> {
    #[cfg(test)]
    if conn.id == TEST_READY_CONN {
        return Vec::new();
    }
    let mut out = conn.usable_problems();
    // Spec 116 T02: a conexão pelo plano ChatGPT precisa do login utilizável.
    if conn.is_chatgpt_plan() && !plan_ready(&conn.id) {
        out.push(ConnectionProblem::PlanLogin);
    }
    out
}

/// Acrescenta as pendências `connection_url:<id>` e `connection_key:<id>` de cada agente pronto
/// cuja conexão não monta cliente ([`Connection::usable_problems`], as mesmas recusas de
/// `LlmClient::new`/`from_connection`), sem repetir a mesma conexão. O rótulo usa o nome da
/// conexão (filtrado pelos segredos); nunca o valor da chave, o nome da variável ou a URL.
fn push_connection_items(
    out: &mut Vec<Missing>,
    cfg: &Config,
    agents: &[&AgentConfig],
    secrets: &SecretSet,
    plan_ready: &dyn Fn(&str) -> bool,
) {
    for agent in agents {
        if !agent_ready(cfg, agent) {
            continue;
        }
        let Some(conn) = connection_of(cfg, agent) else { continue };
        for problem in connection_problems(conn, plan_ready) {
            let (prefix, what) = match problem {
                ConnectionProblem::BaseUrl => ("connection_url", "Endereço"),
                ConnectionProblem::ApiKey => ("connection_key", "Chave"),
                ConnectionProblem::PlanLogin => ("connection_plan", "Acesso ao plano ChatGPT"),
            };
            let id = format!("{prefix}:{}", scrub_text(secrets, &conn.id));
            if out.iter().any(|m| m.id == id) {
                continue;
            }
            let name = scrub_text(secrets, conn.name.trim());
            let name = if name.trim().is_empty() { scrub_text(secrets, &conn.id) } else { name };
            out.push(Missing {
                id,
                label: format!("{what} da conexão {name}"),
                code: prefix,
                params: neutral_params(secrets, "name", &name),
                fix: None,
                settings_tab: TAB_CONNECTIONS,
            });
        }
    }
}

/// `{<name>: <value>}` saneado como os `params` das mensagens com `code` (spec 102 T05).
fn neutral_params(secrets: &SecretSet, name: &str, value: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert(name.to_string(), serde_json::Value::from(value));
    message_params(secrets, &map)
}

fn config_item(id: &'static str, label: &str, tab: &'static str) -> Missing {
    Missing {
        id: id.to_string(),
        label: label.to_string(),
        code: id,
        params: serde_json::Map::new(),
        fix: None,
        settings_tab: tab,
    }
}

/// Pendência de um item da máquina (ausente na lista conta como pendente).
fn machine_missing(items: &[PrereqItem], id: &'static str) -> Option<Missing> {
    match items.iter().find(|i| i.id == id) {
        Some(item) if item.ok => None,
        Some(item) => Some(Missing { label: item.label.to_string(), fix: item.fix.clone(), ..config_item(id, id, TAB_MACHINE) }),
        None => Some(config_item(id, id, TAB_MACHINE)),
    }
}

/// Modelos em uso pelos três papéis (sem repetição, sem vazios). Os papéis pela conexão do plano
/// ChatGPT ficam de fora: lá não há US$ (spec 116 T02).
fn models_in_use(cfg: &Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for agent in [&cfg.agents.architect, &cfg.agents.developer, &cfg.agents.reviewer] {
        if connection_of(cfg, agent).is_some_and(Connection::is_chatgpt_plan) {
            continue;
        }
        let model = agent.model.trim();
        if !model.is_empty() && !out.iter().any(|m| m == model) {
            out.push(model.to_string());
        }
    }
    out
}

/// Avalia os quatro níveis a partir da configuração e do diagnóstico da máquina (função pura). As
/// conexões pelo plano ChatGPT contam como sem login ([`evaluate_with`]).
pub fn evaluate(cfg: &Config, machine: &[PrereqItem], secrets: &SecretSet) -> Readiness {
    evaluate_with(cfg, machine, secrets, &|_| false)
}

/// Como [`evaluate`], com `plan_ready(id)` dizendo se a conexão pelo plano ChatGPT `id` tem login
/// utilizável (spec 116 T02).
pub fn evaluate_with(cfg: &Config, machine: &[PrereqItem], secrets: &SecretSet, plan_ready: &dyn Fn(&str) -> bool) -> Readiness {
    let mut chat = Vec::new();
    if cfg.connections.is_empty() {
        chat.push(config_item("connection", "Conexão com um provedor de modelos", TAB_CONNECTIONS));
    }
    if !agent_ready(cfg, &cfg.agents.architect) {
        chat.push(config_item("agent_architect", "Arquiteto: conexão e modelo", TAB_AGENTS));
    }
    push_connection_items(&mut chat, cfg, &[&cfg.agents.architect], secrets, plan_ready);
    let git = machine_missing(machine, "git");
    let mut create_project = chat.clone();
    create_project.extend(git.clone());

    let mut execute = Vec::new();
    if !agent_ready(cfg, &cfg.agents.developer) {
        execute.push(config_item("agent_developer", "Desenvolvedor: conexão e modelo", TAB_AGENTS));
    }
    if !agent_ready(cfg, &cfg.agents.reviewer) {
        execute.push(config_item("agent_reviewer", "Revisor: conexão e modelo", TAB_AGENTS));
    }
    push_connection_items(&mut execute, cfg, &[&cfg.agents.developer, &cfg.agents.reviewer], secrets, plan_ready);
    execute.extend(git);
    execute.extend(DOCKER_IDS.iter().filter_map(|id| machine_missing(machine, id)));

    let recommended = models_in_use(cfg)
        .into_iter()
        .filter(|model| !cfg.pricing.contains_key(model))
        .map(|model| {
            let model = scrub_text(secrets, &model);
            Missing {
                id: format!("pricing:{model}"),
                label: format!("Preço do modelo {model}"),
                code: "pricing",
                params: neutral_params(secrets, "model", &model),
                fix: None,
                settings_tab: TAB_PRICING,
            }
        })
        .collect();
    Readiness {
        chat: Level::of(chat),
        create_project: Level::of(create_project),
        execute: Level::of(execute),
        recommended: Level::of(recommended),
    }
}

/// Diagnóstico da máquina com cache (em `spawn_blocking`).
async fn machine_cached(checker: &PrereqChecker) -> Vec<PrereqItem> {
    let checker = checker.clone();
    tokio::task::spawn_blocking(move || checker.check()).await.unwrap_or_default()
}

/// Diagnóstico da máquina na hora, sem cache, em `spawn_blocking` e com o teto do checker.
/// Estourado o teto, o daemon conta como pendente (é o que trava) e os demais itens como ok:
/// a ação é recusada com `docker_daemon` sem esperar o comando terminar.
pub async fn machine_fresh(checker: &PrereqChecker) -> Vec<PrereqItem> {
    let ceiling = checker.fresh_ceiling();
    let job = {
        let checker = checker.clone();
        tokio::task::spawn_blocking(move || checker.check_fresh())
    };
    match tokio::time::timeout(ceiling, job).await {
        Ok(Ok(items)) => items,
        _ => timed_out_items(),
    }
}

/// Itens sintéticos do teto estourado: só o daemon pendente.
fn timed_out_items() -> Vec<PrereqItem> {
    MACHINE_IDS
        .iter()
        .map(|id| {
            let pending = *id == "docker_daemon";
            let detail = pending.then(|| Msg::new("doctor.detail.daemon_timeout"));
            PrereqItem {
                id,
                label: label_for(id),
                ok: !pending,
                version: None,
                detail: detail.as_ref().map(|m| m.render(Lang::PtBr)),
                fix: pending.then(|| fix_for("docker_daemon")),
                info: false,
                warn: false,
                detail_msg: detail,
            }
        })
        .collect()
}

/// Prontidão atual (máquina com cache).
pub async fn current(state: &AppState) -> Readiness {
    let machine = machine_cached(state.prereqs()).await;
    build(state, &machine).await
}

async fn build(state: &AppState, machine: &[PrereqItem]) -> Readiness {
    let secrets: Arc<SecretSet> = state.secret_set().await;
    let cfg = state.config().read().await;
    let auth = state.chatgpt();
    let plan_ready = |id: &str| {
        matches!(auth.status(state.data_dir(), id), super::chatgpt::PlanStatus::Connected { .. })
    };
    evaluate_with(&cfg, machine, &secrets, &plan_ready)
}

/// Resposta 412 `{error: "prereq_missing", missing: [ids], code: "prereq_missing", params: {count}}`
/// (`code`/`params` da spec 102; os ids seguem em `missing`).
pub fn prereq_missing(ids: Vec<String>) -> Response {
    let body = json!({"error": PREREQ_MISSING, "missing": ids, "code": PREREQ_MISSING, "params": {"count": ids.len()}});
    (StatusCode::PRECONDITION_FAILED, Json(body)).into_response()
}

/// Confere o nível exigido pela ação. `Err` traz o 412 pronto.
///
/// - `Chat` não consulta a máquina;
/// - `CreateProject` usa o diagnóstico com cache (Git);
/// - `Execute` confere a máquina na hora ([`machine_fresh`]).
pub async fn require(state: &AppState, gate: Gate) -> Result<(), Response> {
    let machine = match gate {
        Gate::Chat => Vec::new(),
        Gate::CreateProject => machine_cached(state.prereqs()).await,
        Gate::Execute => machine_fresh(state.prereqs()).await,
    };
    let readiness = build(state, &machine).await;
    let level = match gate {
        Gate::Chat => &readiness.chat,
        Gate::CreateProject => &readiness.create_project,
        Gate::Execute => &readiness.execute,
    };
    if level.ok {
        Ok(())
    } else {
        Err(prereq_missing(level.ids()))
    }
}

/// `GET /api/readiness`
pub async fn get_readiness(State(state): State<AppState>) -> Response {
    Json(current(&state).await).into_response()
}

/// `POST /api/readiness/refresh` ("verificar de novo"): renova o diagnóstico da máquina (com o
/// mesmo teto da conferência na hora) e devolve a prontidão.
pub async fn post_refresh(State(state): State<AppState>) -> Response {
    let machine = machine_fresh(state.prereqs()).await;
    Json(build(&state, &machine).await).into_response()
}

/// Testes: completa a prontidão `chat` (conexão + Arquiteto) sem tocar num Arquiteto já pronto. A
/// chave é uma referência a variável de ambiente que não existe: o Arquiteto nunca monta cliente
/// (nenhum turno, levantamento ou rede). Só esta conexão ([`TEST_READY_CONN`]) conta como pronta
/// na prontidão apesar disso (exceção restrita a `cfg(test)`, em [`connection_problems`]).
#[cfg(test)]
pub(crate) fn chat_ready(mut cfg: Config) -> Config {
    const CONN: &str = TEST_READY_CONN;
    if !agent_ready(&cfg, &cfg.agents.architect) {
        if !cfg.connections.iter().any(|c| c.id == CONN) {
            cfg.connections.push(crate::config::Connection {
                id: CONN.into(),
                name: "Prontidão (teste)".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key: "env:NANTS_TEST_READY_KEY_NEVER_SET".into(),
                wire_api: crate::config::WireApi::default(),
                kind: Default::default(),
                base_url_override: None,
            });
        }
        cfg.agents.architect.connection_id = CONN.into();
        cfg.agents.architect.model = "modelo-teste".into();
    }
    cfg
}

/// Testes: como [`chat_ready`], completando também Developer e Revisor (prontidão `execute`) com a
/// mesma conexão sem chave: contam como configurados, mas nunca montam cliente.
#[cfg(test)]
pub(crate) fn execute_ready(cfg: Config) -> Config {
    let mut cfg = chat_ready(cfg);
    let conn = cfg.agents.architect.connection_id.clone();
    for role in [&mut cfg.agents.developer, &mut cfg.agents.reviewer] {
        if role.model.trim().is_empty() || role.connection_id.is_empty() {
            role.connection_id = conn.clone();
            role.model = "modelo-teste".into();
        }
    }
    cfg
}

#[cfg(test)]
#[path = "readiness_tests.rs"]
mod tests;
