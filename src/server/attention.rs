//! `GET /api/attention` (spec 093 T01, decisão 7): o que exige ação do usuário em TODOS os
//! projetos, para o sino "Precisa de você".
//!
//! Resposta: `{setup_incomplete, projects: [...]}`; cada projeto só aparece com algum item:
//! - `pending_permissions`: pedidos de permissão pendentes (inclui a confirmação da correção final,
//!   que já é um pedido `approve_final_fix`; não é contada à parte);
//! - `pending_choices`: a última resposta do Arquiteto no histórico persistido tem um bloco
//!   `nants-choices` válido e não há mensagem do usuário depois (nem turno em andamento);
//! - `plan_awaiting_approval`: a spec ativa aguarda aprovação e o usuário pode aprová-la (sem
//!   execução, sem turno do Arquiteto e sem revisão do plano `pending`/`running`);
//! - `paused_or_stalled`: execução pausada (tarefa BLOCKED) ou parada, com as opções de escalada.
//!
//! Tudo vem do estado em memória e do disco, sem chamar modelo. Só ids e rótulos fixos: comandos,
//! motivos, rótulos de opção e textos do modelo nunca entram na resposta.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use super::approvals::{PendingView, FINAL_FIX_COMMAND};
use super::chat::read_visible_history;
use super::choices::parse_choices;
use super::events::{validate_project_id, SecretSet};
use super::projects::{disk_attention, id_view, text_field};
use super::state::AppState;
use crate::agents::architect::TurnRole;

/// Opção de escalada da tarefa bloqueada: id e tipo fixo (o rótulo é texto do modelo e fica fora).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EscalationOption {
    pub id: String,
    pub kind: &'static str,
}

/// Execução pausada (`kind: "paused"`, com tarefa BLOCKED) ou parada (`kind: "stalled"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PausedOrStalled {
    pub kind: &'static str,
    pub spec_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Número da análise que ofereceu as opções (enviado de volta no `recovery-choice`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_options: Option<Vec<EscalationOption>>,
}

/// Pendências lidas do disco de um projeto.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskAttention {
    pub plan_awaiting_approval: bool,
    pub paused_or_stalled: Option<PausedOrStalled>,
}

/// Pedido de permissão pendente, só com ids e rótulos fixos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionItem {
    pub request_id: String,
    /// Papel que pediu (`developer`, `reviewer`, `architect`).
    pub agent: &'static str,
    /// `command`, `environment` ou `final_fix` (confirmação da correção final).
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// "Sempre" é aceito para este pedido.
    pub always_allowed: bool,
}

/// Projeto com algo que exige ação.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectAttention {
    pub project_id: String,
    pub name: String,
    pub pending_permissions: Vec<PermissionItem>,
    pub pending_choices: bool,
    pub plan_awaiting_approval: bool,
    pub paused_or_stalled: Option<PausedOrStalled>,
}

impl ProjectAttention {
    fn has_items(&self) -> bool {
        !self.pending_permissions.is_empty()
            || self.pending_choices
            || self.plan_awaiting_approval
            || self.paused_or_stalled.is_some()
    }
}

/// Corpo de `GET /api/attention`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttentionView {
    pub setup_incomplete: bool,
    pub projects: Vec<ProjectAttention>,
}

/// Setup global incompleto: a prontidão (`/api/readiness`, spec 093 T02) sem `chat` ou sem
/// `execute`. A máquina vem do diagnóstico com cache.
async fn setup_incomplete(state: &AppState) -> bool {
    super::readiness::current(state).await.setup_incomplete()
}

fn permission_item(view: PendingView) -> PermissionItem {
    // A correção final é um pedido de ambiente com o texto fixo; só o texto não basta.
    let kind = if view.kind.is_some() && view.command == FINAL_FIX_COMMAND {
        "final_fix"
    } else if view.kind.is_some() {
        "environment"
    } else {
        "command"
    };
    PermissionItem {
        request_id: view.request_id,
        agent: view.agent,
        kind,
        task_id: view.task_id,
        always_allowed: view.always_allowed,
    }
}

/// A última resposta visível do Arquiteto tem perguntas válidas e nenhuma mensagem visível do
/// usuário veio depois dela.
pub fn choices_pending(root: &Path) -> bool {
    read_visible_history(root)
        .last()
        .is_some_and(|turn| turn.role == TurnRole::Architect && parse_choices(&turn.text).is_some())
}

/// Itens de um projeto (sem decidir se ele entra na lista).
async fn project_attention(
    state: &AppState,
    secrets: &std::sync::Arc<SecretSet>,
    review_now: Option<SystemTime>,
    project: (&str, &str, &str),
) -> ProjectAttention {
    let (id, name, path) = project;
    let pending_permissions: Vec<PermissionItem> =
        state.approvals().list_pending(id).into_iter().map(permission_item).collect();
    let running = state.runs().active(id).is_some();
    // Turno em andamento: o usuário já respondeu (a mensagem ainda não foi persistida).
    let turn_open = state.architect_turn(id).is_some();
    let root = PathBuf::from(path);
    let job_secrets = std::sync::Arc::clone(secrets);
    let (disk, choices) = tokio::task::spawn_blocking(move || {
        (disk_attention(&root, &job_secrets, running, review_now), choices_pending(&root))
    })
    .await
    .unwrap_or_default();
    ProjectAttention {
        project_id: id_view(secrets, id),
        name: text_field(secrets, "name", name),
        pending_permissions,
        pending_choices: choices && !turn_open,
        // Com o turno do Arquiteto em andamento o plano ainda pode mudar: o cliente não deixa aprovar.
        plan_awaiting_approval: disk.plan_awaiting_approval && !turn_open,
        paused_or_stalled: disk.paused_or_stalled,
    }
}

/// Monta a visão de atenção de todos os projetos cadastrados.
pub async fn build_attention(state: &AppState) -> AttentionView {
    let projects: Vec<(String, String, String)> = {
        let cfg = state.config().read().await;
        cfg.projects.iter().map(|p| (p.id.clone(), p.name.clone(), p.path.clone())).collect()
    };
    let secrets = state.secret_set().await;
    // Mesma espera da revisão do plano do estado do projeto (spec 092 T02).
    let review_now = super::plan_review::reviewer_configured(state).await.then(SystemTime::now);
    let mut out = Vec::new();
    for (id, name, path) in projects {
        if validate_project_id(&id).is_err() {
            continue;
        }
        let item = project_attention(state, &secrets, review_now, (&id, &name, &path)).await;
        if item.has_items() {
            out.push(item);
        }
    }
    AttentionView { setup_incomplete: setup_incomplete(state).await, projects: out }
}

/// `GET /api/attention`
pub async fn get_attention(State(state): State<AppState>) -> Response {
    Json(build_attention(&state).await).into_response()
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
