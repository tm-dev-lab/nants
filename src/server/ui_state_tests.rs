//! Testes do estado da interface (`/api/ui-state` e `/api/projects/:id/ui-state`). Incluídos por
//! `ui_state.rs`. O diretório de dados é sempre injetado pelo `AppState` (sem variável de ambiente).

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{env, process};

use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::ServiceExt;

use super::*;
use crate::config::{Config, Project};
use crate::server::state::PauseGate;
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-uistate-{}-{}", process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Estado com o projeto `p1` cadastrado e o diretório de dados em `<dir>/data`.
fn state_in(dir: &Path) -> AppState {
    let proj = dir.join("p1");
    fs::create_dir_all(&proj).expect("mkdir projeto");
    let mut cfg = Config::default();
    cfg.projects.push(Project {
        id: "p1".into(),
        name: "P1".into(),
        path: proj.to_str().expect("utf-8").into(),
        created_at: "2026-10-19T00:00:00Z".into(),
    });
    AppState::new(dir.join("config.toml"), cfg, 3000).with_data_dir(dir.join("data"))
}

fn req(method: Method, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("requisição")
}

async fn call(state: &AppState, r: Request<Body>) -> (StatusCode, String) {
    let resp: Response<Body> = router(state.clone()).oneshot(r).await.expect("resposta");
    let status = resp.status();
    let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).expect("json")
}

async fn put(state: &AppState, uri: &str, base_rev: u64, section: &str, value: Value) -> (StatusCode, String) {
    let body = json!({"base_rev": base_rev, "section": section, "value": value}).to_string();
    call(state, req(Method::PUT, uri, &body)).await
}

fn global_file(dir: &Path) -> PathBuf {
    dir.join("data/ui/global.json")
}

fn project_file(dir: &Path) -> PathBuf {
    dir.join("data/ui/projects/p1.json")
}

#[tokio::test]
async fn global_get_without_file_is_empty_rev_zero() {
    let dir = temp_dir("empty");
    let state = state_in(&dir);
    let (status, text) = call(&state, req(Method::GET, "/api/ui-state", "")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json_of(&text), json!({"version": 1, "rev": 0, "data": {}}));
    assert!(!dir.join("data").exists(), "GET não cria nada");
}

#[tokio::test]
async fn global_put_writes_only_its_section_and_increments_rev() {
    let dir = temp_dir("global-put");
    let state = state_in(&dir);
    let (status, text) = put(&state, "/api/ui-state", 0, "appearance", json!({"theme": "dark"})).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json_of(&text), json!({"version": 1, "rev": 1, "data": {"appearance": {"theme": "dark"}}}));
    let (status, text) = put(&state, "/api/ui-state", 1, "hints", json!(["drag"])).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let env = json_of(&text);
    assert_eq!(env["rev"], 2);
    assert_eq!(env["data"], json!({"appearance": {"theme": "dark"}, "hints": ["drag"]}));
    // Persistido no arquivo, que é o que o GET devolve.
    let on_disk: Value = serde_json::from_slice(&fs::read(global_file(&dir)).expect("arquivo")).expect("json");
    assert_eq!(on_disk, env);
    let (_, text) = call(&state, req(Method::GET, "/api/ui-state", "")).await;
    assert_eq!(json_of(&text), env);
    // Gravação atômica: arquivo 0600 e nenhum temporário sobrando.
    let mode = fs::metadata(global_file(&dir)).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let names: Vec<String> = fs::read_dir(dir.join("data/ui"))
        .expect("listar")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
}

#[tokio::test]
async fn project_nested_sections_keep_siblings() {
    let dir = temp_dir("nested");
    let state = state_in(&dir);
    let uri = "/api/projects/p1/ui-state";
    let (status, text) = put(&state, uri, 0, "layouts.executing.current", json!({"w": [1]})).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, text) = put(&state, uri, 1, "layouts.executing.mine", json!({"w": [2]})).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, text) = put(&state, uri, 2, "mobile", json!({"tab": "chat"})).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let env = json_of(&text);
    assert_eq!(env["rev"], 3);
    assert_eq!(
        env["data"],
        json!({"layouts": {"executing": {"current": {"w": [1]}, "mine": {"w": [2]}}}, "mobile": {"tab": "chat"}})
    );
    assert!(project_file(&dir).is_file());
    // O global é outro documento.
    let (_, text) = call(&state, req(Method::GET, "/api/ui-state", "")).await;
    assert_eq!(json_of(&text)["rev"], 0);
    let (_, text) = call(&state, req(Method::GET, uri, "")).await;
    assert_eq!(json_of(&text), env);
}

#[tokio::test]
async fn stale_base_rev_is_409_with_current_envelope_and_nothing_written() {
    let dir = temp_dir("conflict");
    let state = state_in(&dir);
    let (status, _) = put(&state, "/api/ui-state", 0, "appearance", json!({"theme": "light"})).await;
    assert_eq!(status, StatusCode::OK);
    let before = fs::read(global_file(&dir)).expect("arquivo");
    for stale in [0, 5] {
        let (status, text) = put(&state, "/api/ui-state", stale, "hints", json!(["x"])).await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        let body = json_of(&text);
        assert_eq!(body["version"], 1);
        assert_eq!(body["rev"], 1);
        assert_eq!(body["data"], json!({"appearance": {"theme": "light"}}));
    }
    assert_eq!(fs::read(global_file(&dir)).expect("arquivo"), before);
}

#[tokio::test]
async fn invalid_section_and_body_are_rejected() {
    let dir = temp_dir("invalid");
    let state = state_in(&dir);
    let long = "a".repeat(65);
    for section in ["", "Appearance", "a..b", ".a", "a.", "a/b", "a b", "ação", long.as_str()] {
        let (status, text) = put(&state, "/api/ui-state", 0, section, json!(1)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{section}: {text}");
        assert_eq!(json_of(&text)["error"], MSG_SECTION, "mensagem fixa, sem ecoar a seção");
    }
    let ok = "a".repeat(64);
    let (status, _) = put(&state, "/api/ui-state", 0, &ok, json!(1)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&state, req(Method::PUT, "/api/ui-state", r#"{"base_rev":1,"section":"x"}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "value é obrigatório");
    let (status, _) = call(&state, req(Method::PUT, "/api/ui-state", r#"{"base_rev":"1","section":"x","value":1}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(&state, req(Method::PUT, "/api/ui-state", "{nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn cap_of_64_kib_on_body_and_on_the_stored_document() {
    let dir = temp_dir("cap");
    let state = state_in(&dir);
    // Corpo acima do teto: barrado no extractor.
    let huge = "x".repeat(UI_STATE_MAX_BYTES + 1);
    let (status, _) = put(&state, "/api/ui-state", 0, "appearance", json!(huge)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!global_file(&dir).exists());
    // Cada corpo cabe, mas o documento somado passaria do teto: recusado sem gravar.
    let part = "y".repeat(40 * 1024);
    let (status, text) = put(&state, "/api/ui-state", 0, "appearance", json!(part)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let before = fs::read(global_file(&dir)).expect("arquivo");
    let (status, text) = put(&state, "/api/ui-state", 1, "hints", json!(part)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{text}");
    assert_eq!(fs::read(global_file(&dir)).expect("arquivo"), before);
    let (_, text) = call(&state, req(Method::GET, "/api/ui-state", "")).await;
    assert_eq!(json_of(&text)["rev"], 1);
}

#[tokio::test]
async fn symlinked_file_or_dir_is_never_followed() {
    let dir = temp_dir("symlink");
    let state = state_in(&dir);
    let outside = dir.join("outside.json");
    let original = r#"{"version":1,"rev":7,"data":{"segredo":"FORA"}}"#;
    fs::write(&outside, original).expect("w");
    fs::create_dir_all(dir.join("data/ui")).expect("mkdir");
    symlink(&outside, global_file(&dir)).expect("symlink");

    let (status, text) = call(&state, req(Method::GET, "/api/ui-state", "")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!text.contains("FORA") && !text.contains("outside"), "{text}");
    let (status, text) = put(&state, "/api/ui-state", 7, "appearance", json!(1)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
    assert!(!text.contains("outside"), "{text}");
    assert_eq!(fs::read_to_string(&outside).expect("r"), original, "alvo intacto");
    assert!(fs::symlink_metadata(global_file(&dir)).expect("meta").file_type().is_symlink());

    // Pasta `ui/projects` como symlink para fora: nada é escrito lá.
    let elsewhere = dir.join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("mkdir");
    symlink(&elsewhere, dir.join("data/ui/projects")).expect("symlink dir");
    let (status, _) = put(&state, "/api/projects/p1/ui-state", 0, "selection", json!({"task": "T01"})).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(fs::read_dir(&elsewhere).expect("listar").count(), 0);
}

#[tokio::test]
async fn unknown_project_is_404_and_invalid_id_is_422() {
    let dir = temp_dir("404");
    let state = state_in(&dir);
    let (status, _) = call(&state, req(Method::GET, "/api/projects/nope/ui-state", "")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = put(&state, "/api/projects/nope/ui-state", 0, "selection", json!(1)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!dir.join("data/ui/projects/nope.json").exists());
    let (status, _) = call(&state, req(Method::GET, "/api/projects/a.b/ui-state", "")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn removing_the_project_deletes_its_document() {
    let dir = temp_dir("remove");
    let state = state_in(&dir);
    let (status, _) = put(&state, "/api/projects/p1/ui-state", 0, "selection", json!({"task": "T01"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(project_file(&dir).is_file());
    let (status, text) = call(&state, req(Method::DELETE, "/api/projects/p1", "")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!project_file(&dir).exists());
    let (status, _) = call(&state, req(Method::GET, "/api/projects/p1/ui-state", "")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Instala uma pausa no ponto e devolve (chegou, liberar).
fn gate(state: &AppState, point: &'static str) -> (Arc<Notify>, Arc<Notify>) {
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    state.install_gate(PauseGate { point, reached: Arc::clone(&reached), release: Arc::clone(&release) });
    (reached, release)
}

#[tokio::test]
async fn late_put_after_removal_does_not_recreate_the_document() {
    let dir = temp_dir("late");
    let state = state_in(&dir);
    let (reached, release) = gate(&state, PAUSE_UI_BEFORE_LOOKUP);
    let st = state.clone();
    let put_task = tokio::spawn(async move { put(&st, "/api/projects/p1/ui-state", 0, "selection", json!(1)).await });
    tokio::time::timeout(Duration::from_secs(5), reached.notified()).await.expect("PUT pausado");
    let (status, text) = call(&state, req(Method::DELETE, "/api/projects/p1", "")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    release.notify_one();
    let (status, _) = tokio::time::timeout(Duration::from_secs(5), put_task).await.expect("fim").expect("tarefa");
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!project_file(&dir).exists(), "PUT tardio não recria");
}

#[tokio::test]
async fn removal_waits_for_an_in_flight_put_and_then_deletes() {
    let dir = temp_dir("inflight");
    let state = state_in(&dir);
    let (reached, release) = gate(&state, PAUSE_UI_AFTER_LOOKUP);
    let st = state.clone();
    let put_task = tokio::spawn(async move { put(&st, "/api/projects/p1/ui-state", 0, "selection", json!(1)).await });
    tokio::time::timeout(Duration::from_secs(5), reached.notified()).await.expect("PUT com o lock");
    let st = state.clone();
    let delete_task = tokio::spawn(async move { call(&st, req(Method::DELETE, "/api/projects/p1", "")).await });
    // A remoção espera o read lock do PUT.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!delete_task.is_finished(), "remoção não passa com o PUT segurando o lock");
    release.notify_one();
    let (status, text) = tokio::time::timeout(Duration::from_secs(5), put_task).await.expect("fim").expect("tarefa");
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, text) = tokio::time::timeout(Duration::from_secs(10), delete_task).await.expect("fim").expect("tarefa");
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!project_file(&dir).exists(), "a remoção apaga o que o PUT gravou");
}

#[tokio::test]
async fn corrupt_document_reads_as_empty_and_is_replaced() {
    let dir = temp_dir("corrupt");
    let state = state_in(&dir);
    fs::create_dir_all(dir.join("data/ui")).expect("mkdir");
    fs::write(global_file(&dir), "{lixo").expect("w");
    let (_, text) = call(&state, req(Method::GET, "/api/ui-state", "")).await;
    assert_eq!(json_of(&text), json!({"version": 1, "rev": 0, "data": {}}));
    let (status, text) = put(&state, "/api/ui-state", 0, "appearance", json!(1)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json_of(&text)["rev"], 1);
}

#[test]
fn section_rules_and_nested_set() {
    assert!(valid_section("layouts.executing.current"));
    assert!(valid_section("a_b-c.0"));
    assert!(!valid_section("a..b"));
    let mut data = Map::new();
    set_section(&mut data, "layouts", json!(5));
    set_section(&mut data, "layouts.x.current", json!(true));
    assert_eq!(Value::Object(data), json!({"layouts": {"x": {"current": true}}}));
}

#[tokio::test]
async fn ui_dirs_are_private() {
    let dir = temp_dir("private");
    let state = state_in(&dir);
    let (status, _) = put(&state, "/api/projects/p1/ui-state", 0, "selection", json!(1)).await;
    assert_eq!(status, StatusCode::OK);
    for rel in ["data/ui", "data/ui/projects"] {
        let mode = fs::metadata(dir.join(rel)).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{rel}");
    }
    // Pasta já existente e aberta é restringida na próxima gravação.
    fs::set_permissions(dir.join("data/ui"), fs::Permissions::from_mode(0o755)).expect("chmod");
    let (status, _) = put(&state, "/api/ui-state", 0, "hints", json!([])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fs::metadata(dir.join("data/ui")).expect("meta").permissions().mode() & 0o777, 0o700);
}
