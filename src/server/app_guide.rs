//! Janela de execução nunca vazia nem só com erro (spec 130 T04, G2 e G3).
//!
//! - **Conteúdo** (`GET /api/projects/:id/app/guide/:spec_id`): o "Como usar" do relatório final (as
//!   seções "Como executar" e "Primeiros passos", em qualquer idioma do relatório) ou o README quando
//!   não houver relatório, o resumo das provas (tarefas DONE, a verificação final na base e as
//!   capturas) e o aviso do que não roda no sandbox (`not_in_sandbox` do frontmatter da spec). Spec
//!   131 T05: `run` diz se a spec declara `run_command` e traz a decisão `nothing_to_run`; sem
//!   `run_command`, a janela abre no Console (`open_console`). Tudo redigido e limitado, em leitura
//!   somente.
//! - **Próximo passo de todo erro** ([`attach_action`]): toda resposta de erro das rotas da janela
//!   (`/app`, `/app/start`, `/app/stop`, `/app/logs`, `/app/exec*`, `/app/guide`, `/app/fix`) ganha o
//!   campo `action`, pelo `code` ([`error_action`]). Os avisos do resultado do Console levam o mesmo
//!   campo.
//! - **Falha do ambiente** ([`FailureLog`]): a falha que pede o Arquiteto (`app_docker_up_failed`,
//!   `project_not_docker`, `app_exec_docker_failed`) fica guardada por projeto, com o fim do log
//!   redigido e limitado, e aparece em `GET /app` (`failure`). A ação `ask_architect_fix`
//!   (`POST /api/projects/:id/app/fix`, corpo `{confirm: true, spec_id}`) abre um turno interno do
//!   Arquiteto com esse diagnóstico (`chat::internal_turn`). O log nunca vem do corpo: só do que o
//!   servidor guardou. Se o Arquiteto propuser mudar o ambiente, é por um plano em rascunho, que passa
//!   pela aprovação de sempre (030 e 074).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::error::ApiError;
use super::events::{validate_project_id, SecretSet};
use super::projects::{find_project, format_utc, load_tasks, scrub_text};
use super::safe_fs::{self, MAX_READ_BYTES};
use super::spec_result::validate_spec_id;
use super::AppState;
use crate::i18n::Lang;
use crate::sdd::{Spec, TaskStatus};

/// Próximos passos (a interface dá o rótulo de cada um).
pub const ACTION_ASK_ARCHITECT_FIX: &str = "ask_architect_fix";
/// Tentar de novo (a mesma ação).
pub const ACTION_RETRY: &str = "retry";
/// Abrir o Console do projeto (o padrão quando não há como executar).
pub const ACTION_OPEN_CONSOLE: &str = "open_console";
/// Esperar o que ocupa o projeto (run, relatório, spec em andamento) e tentar de novo.
pub const ACTION_WAIT: &str = "wait";
/// Confirmar a ação (o clique humano).
pub const ACTION_CONFIRM: &str = "confirm";
/// Recarregar a janela (projeto, spec ou pedido inválidos).
pub const ACTION_RELOAD: &str = "reload";
/// Abrir a tela de preparação (pré-requisito faltando).
pub const ACTION_OPEN_SETUP: &str = "open_setup";
/// Mostrar o app que já está rodando.
pub const ACTION_SHOW_APP: &str = "show_app";
/// Parar o comando em curso do Console.
pub const ACTION_STOP_COMMAND: &str = "stop_command";
/// Corrigir o comando digitado no Console.
pub const ACTION_EDIT_COMMAND: &str = "edit_command";

/// Não há falha guardada para a spec: nada a pedir ao Arquiteto.
pub const CODE_FIX_NO_FAILURE: &str = "app_fix_no_failure";

/// Todos os códigos de erro que a janela de execução recebe (das rotas e dos avisos do Console).
/// O teste confere que cada um tem um próximo passo explícito e que a lista cobre os códigos
/// declarados em `app.rs`, `console.rs` e aqui.
pub const APP_ERROR_CODES: &[&str] = &[
    "app_confirm_required",
    "body_invalid",
    "project_not_found",
    "project_id_invalid",
    "spec_not_found",
    "spec_id_invalid",
    "file_invalid",
    "run_active",
    super::state::REPORT_BUSY_CODE,
    super::readiness::PREREQ_MISSING,
    "app_spec_not_completed",
    "app_no_run_command",
    "app_already_running",
    "internal",
    "app_run_command_docker",
    super::app::CODE_PORT_IN_USE,
    super::app::CODE_VIEW_PORTS_EXHAUSTED,
    "app_docker_up_failed",
    "app_docker_down_failed",
    super::runs::PROJECT_NOT_DOCKER_CODE,
    super::console::CODE_EXEC_BUSY,
    super::console::CODE_EXEC_COMMAND_INVALID,
    super::console::CODE_EXEC_COMMAND_DOCKER,
    super::console::CODE_EXEC_TIMEOUT,
    super::console::CODE_EXEC_DOCKER_FAILED,
    CODE_FIX_NO_FAILURE,
];

/// Próximo passo explícito de um código de erro da janela; `None` fora da lista.
pub fn explicit_action(code: &str) -> Option<&'static str> {
    let action = match code {
        "app_docker_up_failed" | "app_exec_docker_failed" | "project_not_docker" => ACTION_ASK_ARCHITECT_FIX,
        // Sem como executar: o padrão é o Console do projeto (G2).
        "app_no_run_command" | "app_run_command_docker" => ACTION_OPEN_CONSOLE,
        "run_active" | "report_in_progress" | "app_spec_not_completed" => ACTION_WAIT,
        "app_confirm_required" => ACTION_CONFIRM,
        "body_invalid" | "project_not_found" | "project_id_invalid" | "spec_not_found" | "spec_id_invalid" | "file_invalid" => {
            ACTION_RELOAD
        }
        "prereq_missing" => ACTION_OPEN_SETUP,
        "app_already_running" => ACTION_SHOW_APP,
        "app_exec_busy" => ACTION_STOP_COMMAND,
        "app_exec_command_invalid" | "app_exec_command_docker" | "app_exec_timeout" => ACTION_EDIT_COMMAND,
        // Porta ocupada, faixa esgotada, falha ao derrubar, erro interno e "nada a corrigir": de novo.
        "internal" | "app_port_in_use" | "view_ports_exhausted" | "app_docker_down_failed" | "app_fix_no_failure" => ACTION_RETRY,
        _ => return None,
    };
    Some(action)
}

/// Próximo passo de qualquer código: o explícito ou, para um código de outra área que chegue pela
/// janela (ex.: o turno do Arquiteto ocupado), tentar de novo. Nunca uma falha "seca".
pub fn error_action(code: &str) -> &'static str {
    explicit_action(code).unwrap_or(ACTION_RETRY)
}

/// Teto do corpo de erro lido pela camada [`attach_action`].
const ACTION_BODY_MAX: usize = 256 * 1024;

/// Camada das rotas da janela: acrescenta `action` a toda resposta de erro JSON que traga `code`.
pub async fn attach_action(req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    let status = resp.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return resp;
    }
    let json_body = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !json_body {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, ACTION_BODY_MAX).await else {
        parts.headers.remove(header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(Value::Object(mut obj)) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let Some(code) = obj.get("code").and_then(Value::as_str).map(str::to_string) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    obj.entry("action").or_insert_with(|| Value::String(error_action(&code).into()));
    let Ok(out) = serde_json::to_vec(&Value::Object(obj)) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(out))
}

// --- falha guardada ---

/// Limite (caracteres, contados do FIM) do log guardado e enviado ao Arquiteto.
pub const FIX_LOG_CHARS: usize = 1600;

/// Falha da janela que pede o Arquiteto, com o fim do log já redigido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppFailure {
    pub spec_id: String,
    pub code: &'static str,
    pub detail: String,
    pub at: String,
}

/// Última falha por projeto (só em memória). Clones compartilham o mapa.
#[derive(Debug, Clone, Default)]
pub struct FailureLog(Arc<Mutex<HashMap<String, AppFailure>>>);

/// Fim do texto com no máximo `max` caracteres, sem caractere de controle (fora quebra e tab).
fn clean_tail(text: &str, max: usize) -> String {
    let skip = text.chars().count().saturating_sub(max);
    text.chars()
        .skip(skip)
        .map(|c| if c.is_control() && c != '\n' && c != '\t' { ' ' } else { c })
        .collect()
}

impl FailureLog {
    /// Guarda a falha do projeto (o `detail` já vem redigido; é cortado pelo fim).
    pub fn record(&self, project_id: &str, spec_id: &str, code: &'static str, detail: &str) {
        let at = format_utc(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
        let failure = AppFailure { spec_id: spec_id.to_string(), code, detail: clean_tail(detail, FIX_LOG_CHARS), at };
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(project_id.to_string(), failure);
    }

    pub fn get(&self, project_id: &str) -> Option<AppFailure> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(project_id).cloned()
    }

    pub fn clear(&self, project_id: &str) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(project_id);
    }

    /// Some com a falha do projeto só se ela tem o `code` dado (um comando do Console que rodou
    /// limpa a falha antiga do Console, sem apagar a da partida do app).
    pub fn clear_code(&self, project_id: &str, code: &str) {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if map.get(project_id).is_some_and(|f| f.code == code) {
            map.remove(project_id);
        }
    }
}

/// Visão da falha para `GET /app`: redigida de novo na saída, com o próximo passo.
pub fn failure_view(failure: Option<AppFailure>, secrets: &SecretSet) -> Value {
    match failure {
        Some(f) => json!({
            "spec_id": f.spec_id,
            "code": f.code,
            "detail": scrub_text(secrets, &f.detail),
            "at": f.at,
            "action": error_action(f.code),
        }),
        None => Value::Null,
    }
}

// --- conteúdo da janela ---

/// Teto do "Como usar" exposto (bytes).
const HOW_TO_USE_MAX_BYTES: usize = 8 * 1024;
/// Capturas listadas.
const MAX_CAPTURES: usize = 12;
/// Índices das seções do relatório que formam o "Como usar" (`report_titles`): "Como executar" e
/// "Primeiros passos".
const HOW_TO_USE_SECTIONS: [usize; 2] = [3, 5];

/// Corta no limite de bytes, em fronteira de caractere. `(texto, cortou)`.
fn cut_bytes(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// "Como usar" do relatório: as seções de execução e primeiros passos, no idioma em que o relatório
/// foi escrito. `None` sem relatório ou sem essas seções.
fn how_to_use_from_report(root: &Path, spec_id: &str, secrets: &SecretSet) -> Option<Value> {
    let rel = format!(".sdd/specs/{spec_id}/{}", super::report::REPORT_FILE);
    let text = safe_fs::read_regular(root, &rel, super::report::REPORT_MAX_BYTES as u64).ok()?;
    let text = scrub_text(secrets, &text);
    let parts: Vec<String> = [Lang::PtBr, Lang::En]
        .into_iter()
        .map(|lang| {
            let titles = crate::agents::prompts::report_titles(lang);
            HOW_TO_USE_SECTIONS
                .iter()
                .filter_map(|i| {
                    let title = titles.get(*i)?;
                    let body = crate::agents::runner::section_text(&text, title)?;
                    let body = body.trim();
                    (!body.is_empty()).then(|| format!("## {title}\n{body}"))
                })
                .collect::<Vec<_>>()
        })
        .find(|found| !found.is_empty())?;
    let (markdown, truncated) = cut_bytes(&parts.join("\n\n"), HOW_TO_USE_MAX_BYTES);
    Some(json!({"source": "report", "markdown": markdown, "truncated": truncated}))
}

/// "Como usar": o do relatório ou, sem ele, o trecho do README.
fn how_to_use(root: &Path, spec_id: &str, secrets: &SecretSet) -> Value {
    if let Some(view) = how_to_use_from_report(root, spec_id, secrets) {
        return view;
    }
    match super::spec_result::readme_view(root, secrets).and_then(|r| serde_json::to_value(r).ok()) {
        Some(readme) => json!({
            "source": "readme",
            "markdown": readme.get("text").cloned().unwrap_or(Value::Null),
            "truncated": readme.get("truncated").cloned().unwrap_or(Value::Bool(false)),
        }),
        None => Value::Null,
    }
}

/// Capturas copiadas da spec (`.sdd/specs/<spec>/captures/`), por nome, com o link de cada uma.
fn captures(root: &Path, project_id: &str, spec_id: &str) -> (Vec<Value>, usize) {
    let rel = format!(".sdd/specs/{spec_id}/{}", crate::sdd::screenshots::CAPTURES_DIR);
    let Ok(dir) = safe_fs::check_chain(root, &rel) else {
        return (Vec::new(), 0);
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return (Vec::new(), 0);
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| super::captures::capture_mime(n).is_some())
        .collect();
    names.sort();
    let total = names.len();
    let list = names
        .into_iter()
        .take(MAX_CAPTURES)
        .map(|name| json!({"name": name, "url": format!("/api/projects/{project_id}/specs/{spec_id}/captures/{name}")}))
        .collect();
    (list, total)
}

/// Resultado da verificação final pelo texto gravado: `✗` reprova, `✓` aprova, sem marca é `null`.
fn verification_passed(text: &str) -> Value {
    if text.contains('✗') {
        Value::Bool(false)
    } else if text.contains('✓') {
        Value::Bool(true)
    } else {
        Value::Null
    }
}

/// Monta o conteúdo da janela. Chamado dentro de `spawn_blocking`.
fn build_guide(root: &Path, project_id: &str, spec_id: &str, secrets: &SecretSet) -> Result<Value, ApiError> {
    let spec_rel = format!(".sdd/specs/{spec_id}");
    let text = safe_fs::read_regular(root, &format!("{spec_rel}/spec.md"), MAX_READ_BYTES)
        .map_err(|_| ApiError::coded(StatusCode::NOT_FOUND, "spec_not_found", Map::new()))?;
    let spec = Spec::parse(&text).map_err(|_| ApiError::coded(StatusCode::CONFLICT, "file_invalid", Map::new()))?;
    let tasks = load_tasks(root, &spec_rel, secrets);
    let done = tasks.iter().filter(|t| t.status == Some(TaskStatus::Done)).count();
    let final_verification = super::report::final_base_fact(root, spec_id, secrets)
        .map_or(Value::Null, |text| json!({"passed": verification_passed(&text), "text": text}));
    let (capture_list, captures_total) = captures(root, project_id, spec_id);
    let note = spec.frontmatter.not_in_sandbox.as_deref().map(|n| scrub_text(secrets, n));
    // Spec 131 T05: sem `run_command` (com ou sem a decisão `nothing_to_run`), as janelas abrem no
    // Console, sem "Iniciar" nem o erro de não haver como executar.
    let declared = spec.frontmatter.run_command.as_deref().is_some_and(|c| !c.trim().is_empty());
    let nothing_to_run = spec.frontmatter.nothing_to_run.as_deref().map(|n| scrub_text(secrets, n));
    Ok(json!({
        "spec_id": spec_id,
        "how_to_use": how_to_use(root, spec_id, secrets),
        "proofs": {
            "tasks_done": done,
            "tasks_total": tasks.len(),
            "final_verification": final_verification,
            "captures": capture_list,
            "captures_total": captures_total,
        },
        "sandbox": {"runs_here": note.is_none(), "note": note},
        "run": {"declared": declared, "nothing_to_run": nothing_to_run},
    }))
}

/// `GET /api/projects/:id/app/guide/:spec_id`: o conteúdo da janela de execução.
pub async fn get_guide(State(state): State<AppState>, AxPath((id, spec_id)): AxPath<(String, String)>) -> Response {
    if let Err(e) = validate_project_id(&id).and_then(|()| validate_spec_id(&spec_id)) {
        return e.into_response();
    }
    let project = match find_project(&state, &id).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let secrets = state.secret_set().await;
    let root = PathBuf::from(&project.path);
    let job = tokio::task::spawn_blocking(move || build_guide(&root, &id, &spec_id, &secrets)).await;
    match job {
        Ok(Ok(view)) => Json(view).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()).into_response(),
    }
}

// --- "Pedir ao Arquiteto para corrigir" ---

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixInput {
    confirm: bool,
    spec_id: String,
}

fn coded(status: StatusCode, code: &'static str) -> Response {
    ApiError::coded(status, code, Map::new()).into_response()
}

/// `run_command` da spec, redigido (contexto do pedido); `None` se ausente ou ilegível.
fn run_command_of(root: &Path, spec_id: &str, secrets: &SecretSet) -> Option<String> {
    let text = safe_fs::read_regular(root, &format!(".sdd/specs/{spec_id}/spec.md"), MAX_READ_BYTES).ok()?;
    let spec = Spec::parse(&text).ok()?;
    spec.frontmatter.run_command.map(|c| scrub_text(secrets, &c))
}

/// `POST /api/projects/:id/app/fix` (`ask_architect_fix`): turno interno do Arquiteto com o
/// diagnóstico guardado da última falha da spec (o fim do log, redigido e limitado). Mesma resposta
/// do `POST /chat` (`reply`, `turn_id`, `draft_updated`). Sem falha guardada para a spec: 409
/// `app_fix_no_failure`.
pub async fn post_fix(State(state): State<AppState>, AxPath(id): AxPath<String>, body: Bytes) -> Response {
    if let Err(e) = validate_project_id(&id) {
        return e.into_response();
    }
    let Ok(input) = serde_json::from_slice::<FixInput>(&body) else {
        return coded(StatusCode::BAD_REQUEST, "body_invalid");
    };
    if !input.confirm {
        return coded(StatusCode::BAD_REQUEST, "app_confirm_required");
    }
    if let Err(e) = validate_spec_id(&input.spec_id) {
        return e.into_response();
    }
    let project = match find_project(&state, &id).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let Some(failure) = state.apps().failures().get(&id).filter(|f| f.spec_id == input.spec_id) else {
        return coded(StatusCode::CONFLICT, CODE_FIX_NO_FAILURE);
    };
    let secrets = state.secret_set().await;
    let log = clean_tail(&scrub_text(&secrets, &failure.detail), FIX_LOG_CHARS);
    let root = PathBuf::from(&project.path);
    let (spec, job_secrets) = (input.spec_id.clone(), Arc::clone(&secrets));
    let run_command = tokio::task::spawn_blocking(move || run_command_of(&root, &spec, &job_secrets)).await.ok().flatten();
    let message = crate::agents::prompts::app_fix_request(&input.spec_id, failure.code, run_command.as_deref(), &log, state.project_lang(&id).get());
    match super::chat::internal_turn(&state, &id, message).await {
        Ok(resp) => resp,
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
#[path = "app_guide_tests.rs"]
mod tests;
