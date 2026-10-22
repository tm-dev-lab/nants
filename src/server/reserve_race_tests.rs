//! Testes de corrida entre busca e reserva de projeto, e entre resolução de permissão e remoção.
//! Determinísticos: a pausa de teste (`AppState::install_gate`) segura a requisição entre a
//! entrada e a reserva (ou a resolução), a remoção é commitada, e só então a requisição segue.
//! Incluídos por `projects.rs`.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tokio::sync::Notify;

use super::remove_project_tests::{
    call, create, delete, get, ids_in_memory, json_of, real_repo, request, snapshot, state_at,
    temp_dir, HOST, RUN_MSG,
};
use crate::harness::tools::{ApprovalRequest, Approver, Role};
use crate::server::approvals::ProjectApprover;
use crate::server::events::SecretSet;
use crate::server::state::{
    PauseGate, PAUSE_AFTER_LOOKUP, PAUSE_AFTER_PERMISSION_LOOKUP, PAUSE_BEFORE_PERMISSION,
    PAUSE_BEFORE_RESERVE, SETTINGS_RESERVATION,
};
use crate::server::AppState;

const SPEC: &str = "001-demo";
const WAIT: Duration = Duration::from_secs(5);

/// Instala uma pausa no ponto e devolve `(reached, release)`.
fn pause_at(state: &AppState, point: &'static str) -> (Arc<Notify>, Arc<Notify>) {
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    state.install_gate(PauseGate {
        point,
        reached: Arc::clone(&reached),
        release: Arc::clone(&release),
    });
    (reached, release)
}

/// Dispara `req` em task própria e espera ela ser pausada no ponto.
async fn spawn_until_paused(
    state: &AppState,
    req: Request<axum::body::Body>,
    reached: &Notify,
) -> tokio::task::JoinHandle<(StatusCode, String)> {
    let st = state.clone();
    let task = tokio::spawn(async move { call(&st, req).await });
    tokio::time::timeout(WAIT, reached.notified())
        .await
        .expect("requisição pausada no ponto");
    task
}

/// Coleta todos os eventos já publicados num receptor.
fn drain(rx: &mut tokio::sync::broadcast::Receiver<crate::server::events::ApiEvent>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev.event_type().to_string());
    }
    out
}

fn post(uri: &str, body: &str) -> Request<axum::body::Body> {
    request(Method::POST, uri, HOST, body)
}

fn put(uri: &str, body: &str) -> Request<axum::body::Body> {
    request(Method::PUT, uri, HOST, body)
}

#[tokio::test]
async fn approve_and_resume_removed_between_lookup_and_reserve_is_404_without_effect() {
    for suffix in ["approve", "resume"] {
        let dir = temp_dir(&format!("race-{suffix}"));
        let repo = dir.join("repo");
        real_repo(&repo);
        let state = state_at(dir.join("config.toml"));
        let id = create(&state, "Repo", &repo).await;
        let before = snapshot(&repo);
        let (reached, release) = pause_at(&state, PAUSE_BEFORE_RESERVE);
        let mut events = state.events().subscribe();

        let req = post(&format!("/api/projects/{id}/specs/{SPEC}/{suffix}"), "{}");
        let task = spawn_until_paused(&state, req, &reached).await;
        let (status, text) = call(&state, delete(&id)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        release.notify_one();

        let (status, text) = tokio::time::timeout(WAIT, task).await.expect("tempo").expect("task");
        assert_eq!(status, StatusCode::NOT_FOUND, "{suffix}: {text}");
        assert_eq!(json_of(&text)["error"], "projeto não encontrado");
        assert_eq!(snapshot(&repo), before, "{suffix}: arquivos mudaram");
        assert!(state.runs().active(&id).is_none(), "{suffix}: run registrado");
        assert_eq!(drain(&mut events), vec!["project_removed"], "{suffix}: evento após remoção");
    }
}

#[tokio::test]
async fn chat_removed_between_lookup_and_reserve_writes_nothing_and_publishes_nothing() {
    let dir = temp_dir("race-chat");
    let repo = dir.join("repo");
    real_repo(&repo);
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "Repo", &repo).await;
    let before = snapshot(&repo);
    let (reached, release) = pause_at(&state, PAUSE_BEFORE_RESERVE);
    let mut events = state.events().subscribe();

    let req = post(&format!("/api/projects/{id}/chat"), r#"{"message":"oi"}"#);
    let task = spawn_until_paused(&state, req, &reached).await;
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    release.notify_one();

    let (status, text) = tokio::time::timeout(WAIT, task).await.expect("tempo").expect("task");
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert_eq!(snapshot(&repo), before, "histórico de chat foi gravado");
    assert_eq!(drain(&mut events), vec!["project_removed"]);
}

#[tokio::test]
async fn settings_during_removal_is_refused_and_writes_nothing() {
    let dir = temp_dir("race-settings");
    let repo = dir.join("repo");
    real_repo(&repo);
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "Repo", &repo).await;
    let before = snapshot(&repo);
    let (reached, release) = pause_at(&state, PAUSE_BEFORE_RESERVE);
    let mut events = state.events().subscribe();

    let req = put(
        &format!("/api/projects/{id}/settings"),
        r#"{"test_command":"cargo test"}"#,
    );
    let task = spawn_until_paused(&state, req, &reached).await;
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    release.notify_one();

    let (status, text) = tokio::time::timeout(WAIT, task).await.expect("tempo").expect("task");
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert_eq!(snapshot(&repo), before, "settings.yaml foi gravado");
    assert_eq!(drain(&mut events), vec!["project_removed"]);
}

#[tokio::test]
async fn permission_resolution_during_removal_persists_and_publishes_nothing_after_removal() {
    let dir = temp_dir("race-permission");
    let proj = dir.join("p");
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "P", &proj).await;
    let before = snapshot(&proj);

    let approver = ProjectApprover::new(
        id.clone(),
        proj.clone(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::new(SecretSet::default()),
    );
    let waiting = tokio::spawn(async move {
        let req = ApprovalRequest {
            in_container: false,
            role: Role::Developer,
            task_id: Some("T01".into()),
            command: "make deploy-prod".into(),
        };
        approver.approve(&req).await
    });
    for _ in 0..500 {
        if !state.approvals().list_pending(&id).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let request_id = state.approvals().list_pending(&id)[0].request_id.clone();
    let (reached, release) = pause_at(&state, PAUSE_BEFORE_PERMISSION);
    let mut events = state.events().subscribe();

    let req = post(
        &format!("/api/projects/{id}/permissions/{request_id}"),
        r#"{"decision":"always"}"#,
    );
    let task = spawn_until_paused(&state, req, &reached).await;
    // A remoção nega o pendente (agente liberado) e commita antes da resolução seguir.
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    release.notify_one();

    let (status, text) = tokio::time::timeout(WAIT, task).await.expect("tempo").expect("task");
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    let decision = tokio::time::timeout(WAIT, waiting).await.expect("agente").expect("tarefa");
    assert_eq!(decision, crate::harness::tools::ApprovalDecision::Deny);
    assert_eq!(snapshot(&proj), before, "persistência após remoção");
    let seen = drain(&mut events);
    let removed = seen.iter().position(|t| t == "project_removed").expect("removida");
    assert!(
        seen[removed + 1..].iter().all(|t| t != "permission_resolved"),
        "permission_resolved após project_removed: {seen:?}"
    );
}

#[tokio::test]
async fn removal_is_refused_while_settings_reservation_is_held() {
    let dir = temp_dir("race-held");
    let config_path = dir.join("config.toml");
    let state = state_at(config_path.clone());
    let id = create(&state, "P", &dir.join("p")).await;

    let (_project, settings) = state
        .reserve_run(&id, SETTINGS_RESERVATION)
        .await
        .expect("reserva de configuração");
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(json_of(&text)["error"], RUN_MSG);
    assert_eq!(ids_in_memory(&state).await, vec![id.clone()]);
    drop(settings);
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

#[tokio::test]
async fn internal_reservations_are_hidden_and_real_runs_are_shown_in_state() {
    let dir = temp_dir("race-sentinel");
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "P", &dir.join("p")).await;
    let state_uri = format!("/api/projects/{id}/state");

    let (_project, settings) = state
        .reserve_run(&id, SETTINGS_RESERVATION)
        .await
        .expect("reserva de configuração");
    let (status, text) = call(&state, get(&state_uri)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let view: Value = json_of(&text);
    assert!(view["run"].is_null(), "{text}");
    assert_eq!(view["running"], false, "{text}");
    drop(settings);

    let (_project, run) = state.reserve_run(&id, SPEC).await.expect("reserva de execução");
    let (status, text) = call(&state, get(&state_uri)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json_of(&text)["run"]["spec_id"], SPEC, "{text}");
    assert_eq!(json_of(&text)["running"], true, "{text}");
    drop(run);
}

/// Dispara a remoção e prova que ela NÃO completa enquanto a pausa segura a requisição depois
/// da busca (o write lock espera o read lock). Devolve o handle da remoção ainda pendente.
async fn removal_blocked_while_paused(
    state: &AppState,
    id: &str,
) -> tokio::task::JoinHandle<(StatusCode, String)> {
    let st = state.clone();
    let uri = format!("/api/projects/{id}");
    let mut del = tokio::spawn(async move { call(&st, request(Method::DELETE, &uri, HOST, "")).await });
    let finished = tokio::time::timeout(Duration::from_millis(300), &mut del)
        .await
        .is_ok();
    assert!(!finished, "remoção completou com a requisição pausada após a busca");
    del
}

/// Espera a tarefa (com prazo) e devolve o resultado.
async fn finish(task: tokio::task::JoinHandle<(StatusCode, String)>) -> (StatusCode, String) {
    tokio::time::timeout(WAIT, task).await.expect("tempo").expect("task")
}

#[tokio::test]
async fn removal_waits_for_run_lookup_and_refuses_or_follows_start() {
    // Discriminação: com `find; drop(read); try_start` a remoção completaria durante a pausa e este
    // teste falha em `removal_blocked_while_paused`.
    for suffix in ["approve", "resume"] {
        let dir = temp_dir(&format!("window-{suffix}"));
        let repo = dir.join("repo");
        real_repo(&repo);
        let state = state_at(dir.join("config.toml"));
        let id = create(&state, "Repo", &repo).await;
        let (reached, release) = pause_at(&state, PAUSE_AFTER_LOOKUP);

        let req = post(&format!("/api/projects/{id}/specs/{SPEC}/{suffix}"), "{}");
        let run = spawn_until_paused(&state, req, &reached).await;
        let del = removal_blocked_while_paused(&state, &id).await;
        release.notify_one();

        let _ = finish(run).await;
        let (del_status, del_text) = finish(del).await;
        // A reserva foi tomada antes da remoção ser admitida: ou a remoção vê o run (409), ou o run
        // já terminou e a remoção conclui. Nunca há run ativo em projeto removido.
        match del_status {
            StatusCode::CONFLICT => {
                assert_eq!(json_of(&del_text)["error"], RUN_MSG, "{suffix}");
                assert_eq!(ids_in_memory(&state).await, vec![id.clone()], "{suffix}");
            }
            StatusCode::OK => {
                assert!(ids_in_memory(&state).await.is_empty(), "{suffix}");
                assert!(state.runs().active(&id).is_none(), "{suffix}");
            }
            other => panic!("{suffix}: status inesperado {other}: {del_text}"),
        }
    }
}

#[tokio::test]
async fn removal_waits_for_chat_lookup_and_refuses_or_follows_turn() {
    let dir = temp_dir("window-chat");
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "P", &dir.join("p")).await;
    let (reached, release) = pause_at(&state, PAUSE_AFTER_LOOKUP);

    let req = post(&format!("/api/projects/{id}/chat"), r#"{"message":"oi"}"#);
    let chat = spawn_until_paused(&state, req, &reached).await;
    let del = removal_blocked_while_paused(&state, &id).await;
    release.notify_one();

    let _ = finish(chat).await;
    let (del_status, del_text) = finish(del).await;
    assert!(
        del_status == StatusCode::OK || del_status == StatusCode::CONFLICT,
        "{del_text}"
    );
}

#[tokio::test]
async fn permission_resolution_waits_for_lookup_and_removal_follows_it() {
    let dir = temp_dir("window-permission");
    let proj = dir.join("p");
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "P", &proj).await;
    let approver = ProjectApprover::new(
        id.clone(),
        proj.clone(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::new(SecretSet::default()),
    );
    let waiting = tokio::spawn(async move {
        let req = ApprovalRequest {
            in_container: false,
            role: Role::Developer,
            task_id: Some("T01".into()),
            command: "make deploy-prod".into(),
        };
        approver.approve(&req).await
    });
    for _ in 0..500 {
        if !state.approvals().list_pending(&id).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let request_id = state.approvals().list_pending(&id)[0].request_id.clone();
    let (reached, release) = pause_at(&state, PAUSE_AFTER_PERMISSION_LOOKUP);
    let mut events = state.events().subscribe();

    let req = post(
        &format!("/api/projects/{id}/permissions/{request_id}"),
        r#"{"decision":"once"}"#,
    );
    let resolve = spawn_until_paused(&state, req, &reached).await;
    let del = removal_blocked_while_paused(&state, &id).await;
    release.notify_one();

    let (status, text) = finish(resolve).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json_of(&text)["applied"], "once");
    let (status, text) = finish(del).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let decision = tokio::time::timeout(WAIT, waiting).await.expect("agente").expect("tarefa");
    assert_eq!(decision, crate::harness::tools::ApprovalDecision::Once);
    let seen = drain(&mut events);
    let resolved = seen.iter().position(|t| t == "permission_resolved").expect("resolvido");
    let removed = seen.iter().position(|t| t == "project_removed").expect("removida");
    assert!(resolved < removed, "{seen:?}");
}

/// Reserva tomada sob o read lock (busca + reserva) bloqueia a remoção com 409 enquanto mantida.
#[tokio::test]
async fn reservation_taken_under_lookup_lock_blocks_removal_with_409() {
    for chat in [false, true] {
        let dir = temp_dir(&format!("window-hold-{chat}"));
        let state = state_at(dir.join("config.toml"));
        let id = create(&state, "P", &dir.join("p")).await;
        let (_reached, release) = pause_at(&state, PAUSE_AFTER_LOOKUP);
        let hold = Arc::new(Notify::new());

        let st = state.clone();
        let id_owned = id.clone();
        let hold_task = Arc::clone(&hold);
        let reserve = tokio::spawn(async move {
            let guard: Box<dyn std::any::Any + Send> = if chat {
                Box::new(st.reserve_chat(&id_owned).await.map_err(|_| ())?.1)
            } else {
                Box::new(st.reserve_run(&id_owned, SPEC).await.map_err(|_| ())?.1)
            };
            hold_task.notified().await;
            drop(guard);
            Ok::<(), ()>(())
        });
        let del = removal_blocked_while_paused(&state, &id).await;
        release.notify_one();

        let (status, text) = finish(del).await;
        assert_eq!(status, StatusCode::CONFLICT, "chat={chat}: {text}");
        assert_eq!(ids_in_memory(&state).await, vec![id.clone()]);
        hold.notify_one();
        tokio::time::timeout(WAIT, reserve).await.expect("tempo").expect("task").expect("reserva");
        let (status, text) = call(&state, delete(&id)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
    }
}
