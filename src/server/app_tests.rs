//! Testes do app de teste por projeto (`app.rs`): sempre Docker (spec 074), com executor Compose
//! falso; a sonda `embeddable` usa um servidor HTTP falso em 127.0.0.1. Incluídos por `app.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{env, process};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;
use tower::ServiceExt;

use super::*;
use crate::config::{AgentConfig, Config, Connection, Effort, Project, WireApi};
use crate::server::envs::tests::{docker_project, provider, Rec};
use crate::server::security::HostPolicy;
use crate::server::{router, AppState};

const PROJECT: &str = "proj-app";
const SPEC: &str = "001-app";
const HOST: &str = "127.0.0.1:3000";
const SECRET: &str = "sk-SEGREDO-APP-5151";

fn spec_md(status: &str, extra: &str) -> String {
    format!(
        "---\nid: \"{SPEC}\"\ntitle: \"App\"\nstatus: \"{status}\"\nbase_branch: \"main\"\n{extra}created_at: \"2026-10-14T00:00:00Z\"\n---\nCorpo.\n"
    )
}

/// Raiz com a spec; sem ambiente docker (ver [`root_with`]).
fn plain_root(tag: &str, status: &str, extra: &str) -> PathBuf {
    let root = env::temp_dir().join(format!("nants-app-{}-{tag}", process::id()));
    let _ = fs::remove_dir_all(&root);
    let dir = root.join(".sdd/specs").join(SPEC);
    fs::create_dir_all(&dir).expect("dir");
    fs::write(dir.join("spec.md"), spec_md(status, extra)).expect("spec");
    root
}

/// Projeto docker com a spec no status dado.
fn root_with(tag: &str, status: &str, extra: &str) -> PathBuf {
    let root = plain_root(tag, status, extra);
    docker_project(&root);
    root
}

fn run_cmd(command: &str) -> String {
    format!("run_command: {}\n", serde_json::to_string(command).expect("json"))
}

fn agent(model: &str) -> AgentConfig {
    AgentConfig { connection_id: "literal".into(), model: model.into(), effort: Effort::Low, max_steps: None }
}

/// Estado com o projeto, conexão e agentes prontos (prontidão `execute`) e Compose falso.
fn state_rec(root: &Path, rec: &Arc<Rec>) -> AppState {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "literal".into(),
        name: "c".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
    cfg.agents.architect = agent("m");
    cfg.agents.developer = agent("m");
    cfg.agents.reviewer = agent("m");
    cfg.projects.push(Project {
        id: PROJECT.into(),
        name: "App".into(),
        path: root.display().to_string(),
        created_at: "2026-10-14T00:00:00Z".into(),
    });
    AppState::new(root.join("config.toml"), cfg, 3000)
        .with_compose_provider(provider(rec))
        .with_app_observe_window(Duration::ZERO)
        .with_app_probe_timeout(Duration::from_millis(300))
        .with_app_watch_interval(Duration::ZERO)
}

fn state_for(root: &Path) -> AppState {
    state_rec(root, &Rec::with_ports())
}

fn req(method: Method, uri: &str, body: &str) -> Request<Body> {
    req_host(method, uri, HOST, body)
}

fn req_host(method: Method, uri: &str, host: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("requisição")
}

async fn call(state: &AppState, mut request: Request<Body>) -> (StatusCode, String) {
    // Conexão de loopback (spec 112): estes testes não tratam de dispositivo autorizado.
    if request.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().is_none() {
        let peer: std::net::SocketAddr = ([127, 0, 0, 1], 50000).into();
        request.extensions_mut().insert(axum::extract::ConnectInfo(peer));
    }
    let resp = router(state.clone()).oneshot(request).await.expect("resposta");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn uri(suffix: &str) -> String {
    format!("/api/projects/{PROJECT}/app{suffix}")
}

async fn start(state: &AppState) -> (StatusCode, Value) {
    let body = json!({"confirm": true, "spec_id": SPEC}).to_string();
    let (status, text) = call(state, req(Method::POST, &uri("/start"), &body)).await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn stop(state: &AppState) -> Value {
    let (status, text) = call(state, req(Method::POST, &uri("/stop"), "")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    serde_json::from_str(&text).expect("json")
}

async fn logs(state: &AppState) -> String {
    call(state, req(Method::GET, &uri("/logs"), "")).await.1
}

fn drain(rx: &mut broadcast::Receiver<crate::server::events::ApiEvent>) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push((ev.event_type().to_string(), ev.payload.clone()));
    }
    out
}

/// Linha do `ps --all` do `workspace` no estado dado.
fn ps_workspace(state: &str, code: i64) -> String {
    json!({"Service": "workspace", "Name": "nants-proj-app-run-workspace-1", "State": state, "ExitCode": code, "Publishers": []}).to_string()
}

/// Saída de `compose ps` com o `workspace` no ar e uma porta publicada (`web`, alvo 8080).
fn ps_with_port(published: u16) -> String {
    format!(
        "{}\n{}\n",
        ps_workspace("running", 0),
        json!({"Service": "web", "Publishers": [{"URL": "0.0.0.0", "TargetPort": 8080, "PublishedPort": published, "Protocol": "tcp"}]})
    )
}

fn rec_with_port(published: u16) -> Arc<Rec> {
    let rec = Rec::with_ports();
    *rec.ps_output.lock().expect("lock") = ps_with_port(published);
    rec
}

#[tokio::test]
async fn state_is_stopped_before_any_start() {
    let root = root_with("idle", "COMPLETED", "");
    let state = state_for(&root);
    let (status, text) = call(&state, req(Method::GET, &uri(""), "")).await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["state"], "stopped");
    assert_eq!(v["embeddable"], Value::Null);
    assert_eq!(stop(&state).await["state"], "stopped");
}

#[tokio::test]
async fn start_requires_explicit_confirmation_and_valid_body() {
    let root = root_with("confirm", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_for(&root);
    for body in [
        "",
        "{}",
        "{not json",
        r#"{"spec_id":"001-app"}"#,
        r#"{"confirm":false,"spec_id":"001-app"}"#,
        r#"{"confirm":"true","spec_id":"001-app"}"#,
        r#"{"confirm":true}"#,
        r#"{"confirm":true,"spec_id":"001-app","command":"rm -rf /"}"#,
    ] {
        let (status, text) = call(&state, req(Method::POST, &uri("/start"), body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {text}");
    }
    assert!(!state.apps().is_running(PROJECT));
    let bad = r#"{"confirm":true,"spec_id":"../x"}"#;
    assert_eq!(call(&state, req(Method::POST, &uri("/start"), bad)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn cross_origin_start_is_forbidden() {
    let root = root_with("origin", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_for(&root);
    let body = json!({"confirm": true, "spec_id": SPEC}).to_string();
    let request = Request::builder()
        .method(Method::POST)
        .uri(uri("/start"))
        .header(header::HOST, HOST)
        .header(header::ORIGIN, "http://evil.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("req");
    assert_eq!(call(&state, request).await.0, StatusCode::FORBIDDEN);
    assert!(!state.apps().is_running(PROJECT));
}

#[tokio::test]
async fn spec_must_be_completed_and_declare_run_command() {
    for status in ["DRAFT", "AWAITING_APPROVAL", "IN_PROGRESS", "BLOCKED"] {
        let root = root_with("notdone", status, &run_cmd("npm run dev"));
        let state = state_for(&root);
        let (code, body) = start(&state).await;
        assert_eq!(code, StatusCode::CONFLICT, "{status}");
        assert_eq!(body["reason"], "spec_not_completed");
    }
    let root = root_with("nocmd", "COMPLETED", "");
    let (code, body) = start(&state_for(&root)).await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["reason"], "no_run_command");
    // run_command inválido (controle) é ignorado pelo parser: equivale a ausente.
    let root = root_with("badcmd", "COMPLETED", "run_command: \"a\\nb\"\n");
    assert_eq!(start(&state_for(&root)).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let root = root_with("nospec", "COMPLETED", "");
    let body = json!({"confirm": true, "spec_id": "999-x"}).to_string();
    let code = call(&state_for(&root), req(Method::POST, &uri("/start"), &body)).await.0;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

/// Todo projeto é Docker (spec 074): sem ambiente docker, a partida é recusada com a mensagem
/// fixa de "replaneje" e nada sobe (o antigo modo processo/host não existe mais).
#[tokio::test]
async fn non_docker_project_is_refused_with_the_replan_message() {
    let root = plain_root("plain", "COMPLETED", &run_cmd("echo oi > ran.txt; sleep 30"));
    fs::create_dir_all(root.join(".sdd")).expect("sdd");
    fs::write(root.join(".sdd/settings.yaml"), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\n")
        .expect("settings");
    let rec = Rec::with_ports();
    let state = state_rec(&root, &rec);
    let (code, body) = start(&state).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "este projeto não tem ambiente docker; replaneje a spec");
    assert_eq!(body["reason"], "environment_not_docker");
    assert!(!state.apps().is_running(PROJECT));
    assert!(rec.compose_calls().is_empty(), "nada sobe");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!root.join("ran.txt").exists(), "nenhum processo no host");
}

#[tokio::test]
async fn unknown_project_is_404() {
    let root = root_with("p404", "COMPLETED", "");
    let state = state_for(&root);
    let body = json!({"confirm": true, "spec_id": SPEC}).to_string();
    let r = req(Method::POST, "/api/projects/nao-existe/app/start", &body);
    assert_eq!(call(&state, r).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&state, req(Method::GET, "/api/projects/nao-existe/app", "")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&state, req(Method::GET, "/api/projects/nao-existe/app/logs", "")).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn active_run_or_reservation_refuses_start() {
    let root = root_with("busy", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_for(&root);
    let guard = state.runs().try_start(PROJECT, "s1").expect("run");
    let (code, body) = start(&state).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert!(!state.apps().is_running(PROJECT));
    drop(guard);
    assert_eq!(start(&state).await.0, StatusCode::OK);
    stop(&state).await;
}

#[tokio::test]
async fn concurrent_and_repeated_starts_keep_a_single_app() {
    let root = root_with("race", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_for(&root);
    let results = tokio::join!(start(&state), start(&state), start(&state));
    let codes = [results.0 .0, results.1 .0, results.2 .0];
    assert_eq!(codes.iter().filter(|c| **c == StatusCode::OK).count(), 1, "{codes:?}");
    assert!(codes.iter().all(|c| *c == StatusCode::OK || *c == StatusCode::CONFLICT), "{codes:?}");
    let (code, body) = start(&state).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["reason"], "already_running");
    stop(&state).await;
}

#[tokio::test]
async fn start_holds_the_project_reservation_against_run_and_removal() {
    let root = root_with("hold", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_for(&root);
    let (_, guard) = state.reserve_run(PROJECT, APP_RESERVATION).await.expect("reserva");
    assert!(state.runs().try_start(PROJECT, "s1").is_err(), "run exclui partida");
    let (code, _) = call(&state, req(Method::DELETE, &format!("/api/projects/{PROJECT}"), "")).await;
    assert_eq!(code, StatusCode::CONFLICT, "remoção exclui partida");
    assert!(state.runs().active(PROJECT).is_none(), "reserva interna não é run");
    drop(guard);
}

#[tokio::test]
async fn stop_during_start_never_leaves_an_orphan() {
    let root = root_with("startstop", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_for(&root);
    let (started, _) = tokio::join!(start(&state), async {
        tokio::time::sleep(Duration::from_millis(2)).await;
        stop(&state).await
    });
    assert!(started.0 == StatusCode::OK || started.0 == StatusCode::CONFLICT);
    stop(&state).await;
    assert!(!state.apps().is_running(PROJECT));
}

#[tokio::test]
async fn start_brings_up_run_project_and_links_follow_the_request_host() {
    let rec = Rec::with_ports();
    let root = root_with("d-start", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_rec(&root, &rec).with_host_policy(HostPolicy::Lan);
    let mut rx = state.events().subscribe();
    let body = json!({"confirm": true, "spec_id": SPEC}).to_string();
    let (status, text) = call(&state, req_host(Method::POST, &uri("/start"), "192.168.1.50:3000", &body)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let view: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(view["state"], "running");
    assert_eq!(view["mode"], "running", "vivo e sem porta respondendo");
    assert_eq!(view["ports"][0]["url"], "http://192.168.1.50:18080");
    assert_eq!(view["ports"][0]["service"], "web");
    assert!(view["ports"][0].get("hint").is_none());
    assert_eq!(view["ports"].as_array().map(Vec::len), Some(1), "porta duplicada IPv6 e interna não entram");
    let calls = rec.compose_calls();
    assert_eq!(calls[0].0, "nants-proj-app-run");
    assert_eq!(calls[0].1, ["up", "-d", "--build"], "sem --wait (spec 130 T02)");
    assert_eq!(calls[1].1, ["ps", "--all", "--format", "json"]);

    // Cada consulta usa o Host da própria requisição.
    let (_, text) = call(&state, req_host(Method::GET, &uri(""), "localhost:3000", "")).await;
    assert!(text.contains("http://localhost:18080"), "{text}");
    let (_, text) = call(&state, req_host(Method::GET, &uri(""), "127.0.0.1:3000", "")).await;
    assert!(text.contains("http://127.0.0.1:18080"), "{text}");

    let started = drain(&mut rx);
    assert!(
        started.iter().any(|(t, p)| t == "app_started" && p["ports"][0]["published"] == 18080 && p["mode"] == "running"),
        "{started:?}"
    );
}

#[tokio::test]
async fn host_outside_the_policy_never_leaks_into_links() {
    let rec = Rec::with_ports();
    let root = root_with("d-host", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_rec(&root, &rec);
    // Política loopback: IP de LAN é recusado pelo guard (403) antes de qualquer link.
    let body = json!({"confirm": true, "spec_id": SPEC}).to_string();
    let (status, _) = call(&state, req_host(Method::POST, &uri("/start"), "192.168.1.50:3000", &body)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(rec.compose_calls().is_empty());
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "evil.example:3000".parse().expect("host"));
    assert_eq!(request_host(&state, &headers), "127.0.0.1");
}

#[tokio::test]
async fn logs_come_from_compose_redacted_and_stop_downs_without_volumes() {
    let rec = Rec::with_ports();
    let root = root_with("d-stop", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_rec(&root, &rec);
    let mut rx = state.events().subscribe();
    assert_eq!(start(&state).await.0, StatusCode::OK);
    let text = logs(&state).await;
    assert!(text.contains("servidor no ar"), "{text}");
    assert!(!text.contains(SECRET));
    assert!(rec.compose_calls().iter().any(|(_, a)| a == &["logs", "--no-color", "--tail", "200"]));
    let after = stop(&state).await;
    assert_eq!(after["state"], "stopped");
    assert!(!state.apps().is_running(PROJECT));
    let last = rec.compose_calls().pop().expect("down");
    assert_eq!(last.1, ["down", "--timeout", "5", "--remove-orphans"], "Parar nunca usa -v");
    let events = drain(&mut rx);
    // Parado pelo usuário: sem código próprio (o processo não terminou sozinho).
    assert!(
        events.iter().any(|(t, p)| t == "app_exited" && p["reason"] == "stopped" && p["code"].is_null() && p["mode"] == "running"),
        "{events:?}"
    );
    // Idempotente.
    stop(&state).await;
}

const WRAPPED_REAL: &str = "docker compose -f .nants/compose.yaml run --rm --service-ports workspace sh -lc 'npm ci && npm run dev -- --host 0.0.0.0 --port 5173 --strictPort'";

#[tokio::test]
async fn start_unwraps_the_wrapped_run_command_into_the_override() {
    let rec = Rec::with_ports();
    let root = root_with("d-wrapped", "COMPLETED", &run_cmd(WRAPPED_REAL));
    let state = state_rec(&root, &rec);
    let (status, text) = start(&state).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let ov = rec.up_override.lock().expect("lock").clone();
    assert!(ov.contains("npm ci && npm run dev -- --host 0.0.0.0 --port 5173 --strictPort"), "{ov}");
    assert!(!ov.contains("docker compose"), "{ov}");
}

#[tokio::test]
async fn start_refuses_missing_or_docker_run_command() {
    let rec = Rec::with_ports();
    let state = state_rec(&root_with("d-norc", "COMPLETED", ""), &rec);
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(view["reason"], "no_run_command");
    let state = state_rec(&root_with("d-badrc", "COMPLETED", &run_cmd("docker compose up -d")), &rec);
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(view["reason"], "run_command_docker");
    assert!(view["error"].as_str().unwrap_or("").contains("0.0.0.0"));
    assert!(rec.compose_calls().is_empty(), "nada sobe");
}

#[tokio::test]
async fn up_failure_detail_keeps_the_tail_and_workspace_logs() {
    let rec = Rec::with_ports();
    *rec.fail_sub.lock().expect("lock") = Some("up".into());
    let state = state_rec(&root_with("d-detail", "COMPLETED", &run_cmd("npm run dev")), &rec);
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let detail = view["detail"].as_str().expect("detail");
    assert!(detail.contains("boom") && detail.contains("servidor no ar"), "{detail}");
    assert!(rec.compose_calls().iter().any(|(_, a)| a.last().map(String::as_str) == Some("workspace") && a.contains(&"logs".to_string())));
}

#[tokio::test]
async fn up_failure_is_reported_and_rolled_back() {
    let rec = Rec::with_ports();
    *rec.fail_sub.lock().expect("lock") = Some("up".into());
    let state = state_rec(&root_with("d-fail", "COMPLETED", &run_cmd("npm run dev")), &rec);
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(view["reason"], "docker_failed");
    assert!(!state.apps().is_running(PROJECT));
    assert!(rec.compose_calls().iter().any(|(_, a)| a.first().map(String::as_str) == Some("down")));
}

#[tokio::test]
async fn removing_the_project_downs_both_and_run_volumes_need_confirmation() {
    for (body, run_args) in [
        ("{}", vec!["down", "--timeout", "5", "--remove-orphans"]),
        ("", vec!["down", "--timeout", "5", "--remove-orphans"]),
        (r#"{"remove_volumes":true}"#, vec!["down", "--timeout", "5", "-v", "--remove-orphans"]),
    ] {
        let rec = Rec::with_ports();
        let state = state_rec(&root_with("d-remove", "COMPLETED", &run_cmd("npm run dev")), &rec);
        let mut rx = state.events().subscribe();
        assert_eq!(start(&state).await.0, StatusCode::OK);
        let (code, text) = call(&state, req(Method::DELETE, &format!("/api/projects/{PROJECT}"), body)).await;
        assert_eq!(code, StatusCode::OK, "{text}");
        assert!(!state.apps().is_running(PROJECT));
        let calls = rec.compose_calls();
        let downs: Vec<_> = calls.iter().filter(|(_, a)| a[0] == "down").collect();
        // Parar do app (-run), depois -test (-v) e -run (conforme a confirmação).
        let test = downs.iter().find(|(p, _)| p == "nants-proj-app-test").expect("down -test");
        assert_eq!(test.1, ["down", "-v", "--remove-orphans"]);
        let last_run = downs.iter().rfind(|(p, _)| p == "nants-proj-app-run").expect("down -run");
        assert_eq!(last_run.1, run_args, "body={body:?}");
        let events = drain(&mut rx);
        assert!(events.iter().any(|(t, p)| t == "app_exited" && p["reason"] == "project_removed"), "{events:?}");
    }
}

#[tokio::test]
async fn nants_shutdown_downs_run_and_test_without_volumes() {
    let rec = Rec::with_ports();
    let state = state_rec(&root_with("d-shutdown", "COMPLETED", &run_cmd("npm run dev")), &rec);
    let mut rx = state.events().subscribe();
    assert_eq!(start(&state).await.0, StatusCode::OK);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let (tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serve = tokio::spawn(crate::server::serve_with_shutdown(listener, state.clone(), async move {
        let _ = stop_rx.await;
    }));
    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(20), serve).await.expect("encerrou");
    assert!(!state.apps().is_running(PROJECT));
    let calls = rec.compose_calls();
    for project in ["nants-proj-app-test", "nants-proj-app-run"] {
        let d = calls.iter().find(|(p, a)| p == project && a[0] == "down").unwrap_or_else(|| panic!("{project}"));
        let expected: &[&str] = if project.ends_with("-run") { &["down", "--timeout", "5", "--remove-orphans"] } else { &["down", "--remove-orphans"] };
        assert_eq!(d.1, expected);
    }
    let events = drain(&mut rx);
    assert!(events.iter().any(|(t, p)| t == "app_exited" && p["reason"] == "shutdown"), "{events:?}");
}

// ---------- `embeddable` (spec 093 T02) ----------

/// Servidor HTTP falso em 127.0.0.1: responde com os cabeçalhos dados (ou nunca, com `None`) e
/// guarda o texto de cada requisição recebida.
struct FakeApp {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

async fn fake_app(headers: Option<&'static str>) -> FakeApp {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    fake_app_on(listener, headers)
}

fn fake_app_on(listener: tokio::net::TcpListener, headers: Option<&'static str>) -> FakeApp {
    let port = listener.local_addr().expect("addr").port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let mut got = Vec::new();
                while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => got.extend_from_slice(&buf[..n]),
                    }
                }
                log.lock().expect("lock").push(String::from_utf8_lossy(&got).to_string());
                match headers {
                    Some(extra) => {
                        let body = "<html>app</html>";
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                    }
                    // Nunca responde: a sonda estoura o timeout.
                    None => tokio::time::sleep(Duration::from_secs(10)).await,
                }
            });
        }
    });
    FakeApp { port, seen }
}

/// Sobe o app (Compose falso publicando a porta do servidor falso) e devolve a resposta do start.
async fn started_with(tag: &str, app: &FakeApp, host_policy: HostPolicy) -> (AppState, Value) {
    let rec = rec_with_port(app.port);
    let root = root_with(tag, "COMPLETED", &run_cmd("npm run dev"));
    let state = state_rec(&root, &rec).with_host_policy(host_policy);
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    (state, view)
}

#[tokio::test]
async fn embeddable_false_with_x_frame_options_deny_or_sameorigin() {
    for (i, xfo) in ["X-Frame-Options: DENY\r\n", "X-Frame-Options: sameorigin\r\n"].into_iter().enumerate() {
        let app = fake_app(Some(xfo)).await;
        let (_, view) = started_with(&format!("emb-xfo-{i}"), &app, HostPolicy::Loopback).await;
        assert_eq!(view["embeddable"], false, "{view}");
        assert_eq!(view["ports"][0]["embeddable"], false, "{view}");
    }
}

#[tokio::test]
async fn embeddable_false_with_frame_ancestors_none_or_self() {
    let cases = [
        "Content-Security-Policy: default-src 'self'; frame-ancestors 'none'\r\n",
        "Content-Security-Policy: frame-ancestors 'self'\r\n",
    ];
    for (i, csp) in cases.into_iter().enumerate() {
        let app = fake_app(Some(csp)).await;
        let (_, view) = started_with(&format!("emb-csp-{i}"), &app, HostPolicy::Loopback).await;
        assert_eq!(view["embeddable"], false, "{csp}: {view}");
    }
}

#[tokio::test]
async fn embeddable_true_without_headers_and_rechecked_on_each_query() {
    let app = fake_app(Some("")).await;
    let (state, view) = started_with("emb-none", &app, HostPolicy::Loopback).await;
    assert_eq!(view["embeddable"], true, "{view}");
    assert_eq!(app.seen.lock().expect("lock").len(), 1, "sonda ao subir");
    let (_, text) = call(&state, req(Method::GET, &uri(""), "")).await;
    let now: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(now["embeddable"], true, "{now}");
    assert_eq!(app.seen.lock().expect("lock").len(), 2, "sonda de novo ao consultar");
    // Parado: indeterminado e sem sonda.
    let after = stop(&state).await;
    assert_eq!(after["embeddable"], Value::Null);
    assert_eq!(app.seen.lock().expect("lock").len(), 2);
}

#[tokio::test]
async fn embeddable_null_on_timeout_or_refused_connection() {
    let app = fake_app(None).await;
    let started = std::time::Instant::now();
    let (_, view) = started_with("emb-timeout", &app, HostPolicy::Loopback).await;
    assert_eq!(view["embeddable"], Value::Null, "{view}");
    assert!(started.elapsed() < Duration::from_secs(3), "timeout curto: {:?}", started.elapsed());
    // Porta fechada: indeterminado.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = closed.local_addr().expect("addr").port();
    drop(closed);
    assert_eq!(probe_embeddable(port, Duration::from_millis(300)).await, None);
}

/// A sonda vai sempre a 127.0.0.1:<porta publicada>, nunca ao `Host` da requisição (SSRF em `--lan`).
#[tokio::test]
async fn probe_always_targets_loopback_never_the_request_host() {
    let app = fake_app(Some("")).await;
    let rec = rec_with_port(app.port);
    let root = root_with("emb-ssrf", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_rec(&root, &rec).with_host_policy(HostPolicy::Lan);
    let body = json!({"confirm": true, "spec_id": SPEC}).to_string();
    let (status, text) = call(&state, req_host(Method::POST, &uri("/start"), "192.168.1.50:3000", &body)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let view: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(view["ports"][0]["url"], format!("http://192.168.1.50:{}", app.port), "o link segue o host");
    assert_eq!(view["embeddable"], true);
    call(&state, req_host(Method::GET, &uri(""), "192.168.1.50:3000", "")).await;
    let seen = app.seen.lock().expect("lock").clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    for request in &seen {
        let lower = request.to_ascii_lowercase();
        assert!(lower.starts_with("get / http/1.1"), "{request}");
        assert!(lower.contains(&format!("host: 127.0.0.1:{}", app.port)), "{request}");
        assert!(!lower.contains("192.168.1.50"), "{request}");
    }
}

/// Spec 097: o estado do app expõe a porta do Nants, a faixa (só em `--lan`) e o WSL, sem IP.
#[tokio::test]
async fn app_state_exposes_view_range_nants_port_and_wsl() {
    let root = root_with("net-info", "COMPLETED", "");
    let lan = state_for(&root).with_host_policy(HostPolicy::Lan);
    let (status, text) = call(&lan, req_host(Method::GET, &uri(""), "192.168.1.50:3000", "")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let view: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(view["lan_view_ports"], json!({"start": 3001, "end": 3010}), "{view}");
    assert_eq!(view["nants_port"], 3000);
    assert_eq!(view["wsl"], json!(crate::server::netinfo::running_on_wsl()));
    for ip in crate::server::netinfo::local_ipv4_addrs() {
        assert!(!text.contains(&ip.to_string()), "IP interno exposto: {text}");
    }
    // Faixa configurada vale no lugar da padrão.
    let custom = state_for(&root).with_host_policy(HostPolicy::Lan).with_view_ports(crate::config::ViewPorts { start: 5000, end: 5004 });
    let (_, text) = call(&custom, req(Method::GET, &uri(""), "")).await;
    let view: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(view["lan_view_ports"], json!({"start": 5000, "end": 5004}), "{view}");
    // Sem `--lan`: sem faixa.
    let local = state_for(&root);
    let (_, text) = call(&local, req(Method::GET, &uri(""), "")).await;
    let view: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(view["lan_view_ports"], Value::Null, "{view}");
    assert_eq!(view["nants_port"], 3000);
    assert!(view["wsl"].is_boolean());
    // A configuração (lida pela interface em Configurações) traz o mesmo bloco.
    let (_, text) = call(&lan, req(Method::GET, "/api/config", "")).await;
    let cfg: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(cfg["network"]["lan_view_ports"], json!({"start": 3001, "end": 3010}), "{cfg}");
    assert_eq!(cfg["network"]["nants_port"], 3000);
    assert!(cfg["network"]["wsl"].is_boolean());
}

/// Spec 097, decisão 3: faixa esgotada falha no caminho do conflito de porta, com a mensagem
/// da faixa e sem subir nada.
#[tokio::test]
async fn exhausted_view_range_fails_like_a_port_conflict() {
    let root = plain_root("net-full", "COMPLETED", &run_cmd("npm run dev"));
    fs::create_dir_all(root.join(".nants")).expect("nants");
    fs::write(root.join(".nants/compose.yaml"), "services:\n  workspace:\n    image: alpine\n    ports:\n      - \"5173\"\n").expect("compose");
    let hash = crate::sdd::environment::env_hash(&root).expect("hash");
    fs::write(
        root.join(".sdd/settings.yaml"),
        format!("base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\nenvironment: docker\nenvironment_hash: \"{hash}\"\n"),
    )
    .expect("settings");
    let busy = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind");
    let port = busy.local_addr().expect("addr").port();
    let rec = Rec::with_ports();
    let state = state_rec(&root, &rec)
        .with_host_policy(HostPolicy::Lan)
        .with_view_ports(crate::config::ViewPorts { start: port, end: port });
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{view}");
    let expected = format!("Todas as portas de visualização ({port} a {port}) estão em uso. Pare outro app na Execução e tente de novo.");
    assert_eq!(view["error"], expected.as_str(), "{view}");
    assert_eq!(view["reason"], "docker_port_in_use");
    // Spec 102: `code` e `params` para a interface traduzir.
    assert_eq!(view["code"], "view_ports_exhausted");
    assert_eq!(view["params"], json!({"start": port, "end": port}));
    assert!(rec.compose_calls().iter().all(|(_, args)| args.first().map(String::as_str) != Some("up")), "nada sobe");
    assert!(!state.apps().is_running(PROJECT));
}

#[test]
fn header_rules() {
    use reqwest::header::{HeaderMap as RHeaders, HeaderValue};
    let of = |pairs: &[(&'static str, &'static str)]| {
        let mut h = RHeaders::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        embeddable_from_headers(&h)
    };
    assert_eq!(of(&[]), Some(true));
    assert_eq!(of(&[("x-frame-options", "DENY")]), Some(false));
    assert_eq!(of(&[("x-frame-options", " SameOrigin ")]), Some(false));
    assert_eq!(of(&[("content-security-policy", "frame-ancestors 'none'")]), Some(false));
    assert_eq!(of(&[("content-security-policy", "script-src 'self'; frame-ancestors 'self'")]), Some(false));
    assert_eq!(of(&[("content-security-policy", "frame-ancestors")]), Some(false));
    assert_eq!(of(&[("content-security-policy", "frame-ancestors *")]), Some(true));
    assert_eq!(of(&[("content-security-policy", "frame-ancestors 'self' http://localhost:3000")]), None);
    assert_eq!(of(&[("content-security-policy", "default-src 'self'")]), Some(true));
}

/// A porta do próprio Nants nunca vira link do app (nem é sondada).
#[tokio::test]
async fn nants_port_is_never_an_app_link() {
    let rec = rec_with_port(3000);
    let root = root_with("emb-nants", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_rec(&root, &rec);
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["ports"][0]["published"], 3000);
    assert_eq!(view["ports"][0]["url"], Value::Null, "{view}");
    assert_eq!(view["embeddable"], Value::Null);
    let (_, text) = call(&state, req(Method::GET, &uri(""), "")).await;
    assert!(!text.contains(":3000\""), "{text}");
}

#[tokio::test]
async fn start_without_execute_readiness_is_412_and_nothing_starts() {
    let rec = Rec::with_ports();
    let root = root_with("ready", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_rec(&root, &rec);
    state.config().write().await.agents.reviewer = AgentConfig::default();
    let (status, body) = start(&state).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    assert_eq!(body, json!({"error": "prereq_missing", "missing": ["agent_reviewer"], "code": "prereq_missing", "params": {"count": 1}, "action": "open_setup"}));
    assert!(rec.compose_calls().is_empty());
    assert!(!state.apps().is_running(PROJECT));
}

// ---------- Modo pelo comportamento (spec 130 T02) ----------

/// Projeto docker com `run_command` e o `ps --all` dado.
fn finished_state(tag: &str, ps: &str) -> (AppState, Arc<Rec>, PathBuf) {
    let rec = Rec::with_ports();
    *rec.ps_output.lock().expect("lock") = format!("{ps}\n");
    let root = root_with(tag, "COMPLETED", &run_cmd("python -m ledger report data/sample.csv"));
    (state_rec(&root, &rec), rec, root)
}

/// Um CLI que roda e sai com 0 não é falha: "terminou", com o código, a saída redigida e o que
/// foi gravado fora de `/work`; o container encerrado fica (sem `down`) até subir de novo.
#[tokio::test]
async fn cli_that_exits_zero_is_finished_not_a_failure() {
    let (state, rec, _root) = finished_state("m-zero", &ps_workspace("exited", 0));
    *rec.logs_output.lock().expect("lock") = Some(format!("Saldo: 42\nchave={SECRET}\n"));
    *rec.diff_output.lock().expect("lock") =
        "C /tmp\nA /tmp/cache.db\nC /work\nA /work/out.csv\nD /etc/motd\nC /usr\nC /usr/sbin\nA /usr/sbin/docker-init\nA /root/.config\n".into();
    let mut rx = state.events().subscribe();
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["state"], "exited");
    assert_eq!(view["mode"], "finished");
    assert_eq!(view["exit_code"], 0);
    let finished = &view["finished"];
    let output = finished["output"].as_str().expect("saída");
    assert!(output.contains("Saldo: 42"), "{output}");
    assert!(!output.contains(SECRET), "saída redigida: {output}");
    // Só onde o app gravou: sem as pastas ancestrais e sem o init que o Docker injeta.
    assert_eq!(finished["outside_work"], json!(["/root/.config", "/tmp/cache.db"]));
    assert_eq!(finished["outside_work_total"], 2);
    assert!(rec.calls.lock().expect("lock").iter().any(|a| a == &["diff", "nants-proj-app-run-workspace-1"]));
    let logs_call = ["logs", "--no-color", "--no-log-prefix", "--tail", "400", "workspace"];
    assert!(rec.compose_calls().iter().any(|(_, a)| a == &logs_call), "{:?}", rec.compose_calls());
    assert!(rec.compose_calls().iter().all(|(_, a)| a[0] != "down"), "o container encerrado fica");
    assert!(!state.apps().is_running(PROJECT), "terminou não é app vivo");
    let events = drain(&mut rx);
    assert!(events.iter().any(|(t, p)| t == "app_started" && p["mode"] == "finished"), "{events:?}");
    assert!(
        events.iter().any(|(t, p)| t == "app_exited" && p["code"] == 0 && p["reason"] == "exited" && p["mode"] == "finished"),
        "{events:?}"
    );
    // O estado relido continua "terminou" (sem nova sonda nem evento).
    let (_, text) = call(&state, req(Method::GET, &uri(""), "")).await;
    let again: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(again["mode"], "finished");
    assert!(!text.contains(SECRET));
    assert!(drain(&mut rx).is_empty());
    // Subir de novo derruba o container encerrado antes, sem repetir o `app_exited`.
    let (status, _) = start(&state).await;
    assert_eq!(status, StatusCode::OK);
    let calls = rec.compose_calls();
    let down = calls.iter().position(|(_, a)| a[0] == "down").expect("down antes de subir de novo");
    assert!(calls.iter().skip(down).any(|(_, a)| a[0] == "up"), "{calls:?}");
    let events = drain(&mut rx);
    assert_eq!(events.iter().filter(|(t, _)| t == "app_exited").count(), 1, "{events:?}");
    // Fechar (parar) um "terminou" derruba o container sem novo `app_exited`.
    assert_eq!(stop(&state).await["state"], "stopped");
    assert!(!state.apps().is_docker_running(PROJECT));
    assert!(drain(&mut rx).iter().all(|(t, _)| t != "app_exited"));
}

#[tokio::test]
async fn nonzero_exit_code_is_reported_as_finished() {
    let (state, _rec, _root) = finished_state("m-code", &ps_workspace("exited", 2));
    let mut rx = state.events().subscribe();
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["mode"], "finished");
    assert_eq!(view["exit_code"], 2);
    // Spec 130 T05: a janela mostra o comando que rodou ("$ ...").
    assert_eq!(view["command"], "python -m ledger report data/sample.csv");
    assert_eq!(view["finished"]["exit_code"], 2);
    let events = drain(&mut rx);
    assert!(events.iter().any(|(t, p)| t == "app_exited" && p["code"] == 2), "{events:?}");
}

/// C-08 e C-10: o app vivo que sai depois vira "terminou" na consulta seguinte, com o código real
/// e os arquivos criados ou mudados na pasta (inclusive ignorados), sem as pastas do Nants.
#[tokio::test]
async fn running_app_that_exits_turns_finished_with_code_and_files() {
    let rec = Rec::with_ports();
    let root = root_with("m-files", "COMPLETED", &run_cmd("python -m worker"));
    fs::write(root.join(".gitignore"), "out/\n").expect("w");
    fs::write(root.join("data.csv"), "a\n").expect("w");
    let state = state_rec(&root, &rec);
    let mut rx = state.events().subscribe();
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["mode"], "running");
    // O programa grava e sai.
    fs::create_dir_all(root.join("out")).expect("d");
    fs::write(root.join("out/report.txt"), "pronto").expect("w");
    fs::write(root.join("data.csv"), "a\nb\n").expect("w");
    fs::write(root.join(".nants-home/.cache-x"), "x").expect("w");
    fs::write(root.join(".sdd/extra.md"), "x").expect("w");
    *rec.ps_output.lock().expect("lock") = format!("{}\n", ps_workspace("exited", 0));
    let (_, text) = call(&state, req(Method::GET, &uri(""), "")).await;
    let now: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(now["state"], "exited", "{now}");
    assert_eq!(now["mode"], "finished");
    assert_eq!(now["exit_code"], 0);
    assert_eq!(
        now["finished"]["files"],
        json!([{"path": "data.csv", "change": "changed"}, {"path": "out/report.txt", "change": "added"}]),
        "{now}"
    );
    assert_eq!(now["finished"]["files_total"], 2);
    assert_eq!(now["finished"]["files_truncated"], false);
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|(t, p)| t == "app_mode_changed" && p["mode"] == "finished" && p["previous"] == "running" && p["exit_code"] == 0),
        "{events:?}"
    );
    assert!(events.iter().any(|(t, p)| t == "app_exited" && p["code"] == 0 && p["mode"] == "finished"), "{events:?}");
}

/// C-08: `running` vira `page` quando uma porta passa a responder HTTP.
#[tokio::test]
async fn running_app_turns_page_when_a_port_answers() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = closed.local_addr().expect("addr").port();
    drop(closed);
    let rec = rec_with_port(port);
    let root = root_with("m-page", "COMPLETED", &run_cmd("npm run dev"));
    let state = state_rec(&root, &rec);
    let mut rx = state.events().subscribe();
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["mode"], "running", "{view}");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.expect("rebind");
    let _app = fake_app_on(listener, Some(""));
    let (_, text) = call(&state, req(Method::GET, &uri(""), "")).await;
    let now: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(now["mode"], "page", "{now}");
    assert_eq!(now["state"], "running");
    let events = drain(&mut rx);
    assert!(events.iter().any(|(t, p)| t == "app_mode_changed" && p["mode"] == "page" && p["previous"] == "running"), "{events:?}");
    stop(&state).await;
}

/// C-08: a sondagem leve leva o app vivo a "terminou" sem ninguém consultar.
#[tokio::test]
async fn light_watch_detects_the_exit_without_a_query() {
    let rec = Rec::with_ports();
    let root = root_with("m-watch", "COMPLETED", &run_cmd("python -m worker"));
    let state = state_rec(&root, &rec).with_app_watch_interval(Duration::from_millis(30));
    let mut rx = state.events().subscribe();
    assert_eq!(start(&state).await.0, StatusCode::OK);
    *rec.ps_output.lock().expect("lock") = format!("{}\n", ps_workspace("exited", 7));
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Ok(ev) if ev.event_type() == "app_exited" => return ev.payload.clone(),
                Ok(_) => {}
                Err(e) => panic!("{e}"),
            }
        }
    })
    .await
    .expect("app_exited pela sondagem");
    assert_eq!(got["code"], 7);
    assert_eq!(got["mode"], "finished");
    assert!(!state.apps().is_running(PROJECT));
    stop(&state).await;
}

/// Parar durante a sondagem: o container sai (143) no meio do `down`, mas a consulta e a sondagem
/// desse intervalo não viram "terminou"; sai um único `app_exited` com o motivo da parada.
#[tokio::test]
async fn stopping_is_never_turned_into_finished_by_the_watch_or_a_query() {
    let rec = Rec::with_ports();
    let root = root_with("m-stopping", "COMPLETED", &run_cmd("python -m worker"));
    let state = state_rec(&root, &rec).with_app_watch_interval(Duration::from_millis(30));
    let mut rx = state.events().subscribe();
    assert_eq!(start(&state).await.0, StatusCode::OK);
    *rec.delay_sub.lock().expect("lock") = Some(("down".into(), Duration::from_millis(400)));
    let stopper = {
        let state = state.clone();
        tokio::spawn(async move { state.apps().stop(PROJECT, REASON_STOPPED).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    *rec.ps_output.lock().expect("lock") = format!("{}\n", ps_workspace("exited", 143));
    let (_, text) = call(&state, req(Method::GET, &uri(""), "")).await;
    let during: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(during["mode"], "running", "{during}");
    assert!(during["finished"].is_null(), "{during}");
    tokio::time::sleep(Duration::from_millis(150)).await;
    stopper.await.expect("parou");
    assert!(!state.apps().is_docker_running(PROJECT));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let events = drain(&mut rx);
    let exited: Vec<_> = events.iter().filter(|(t, _)| t == "app_exited").collect();
    assert_eq!(exited.len(), 1, "{events:?}");
    assert_eq!(exited[0].1["reason"], "stopped");
    assert!(exited[0].1["code"].is_null(), "{events:?}");
    assert!(events.iter().all(|(t, _)| t != "app_mode_changed"), "{events:?}");
}

/// `down` que falha desfaz a marca de parada: o app segue registrado e volta a ser observado.
#[tokio::test]
async fn failed_down_clears_the_stopping_mark() {
    let rec = Rec::with_ports();
    let root = root_with("m-downfail", "COMPLETED", &run_cmd("python -m worker"));
    let state = state_rec(&root, &rec);
    assert_eq!(start(&state).await.0, StatusCode::OK);
    *rec.fail_sub.lock().expect("lock") = Some("down".into());
    state.apps().stop(PROJECT, REASON_STOPPED).await;
    assert!(state.apps().is_running(PROJECT));
    *rec.fail_sub.lock().expect("lock") = None;
    *rec.ps_output.lock().expect("lock") = format!("{}\n", ps_workspace("exited", 1));
    let (_, text) = call(&state, req(Method::GET, &uri(""), "")).await;
    let now: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(now["mode"], "finished", "{now}");
    assert_eq!(now["exit_code"], 1);
}

/// Diff grande (cache de ferramenta): o filtro dos ancestrais é rápido e não confunde irmãos
/// (`/a-b`, `/a.txt`) com descendentes.
#[test]
fn outside_leaves_is_fast_and_exact() {
    let got = outside_leaves(
        ["/a", "/a-b", "/a.txt", "/a/c", "/usr", "/usr/sbin", "/usr/sbin/docker-init", "/x", "/a/c"].map(String::from).to_vec(),
    );
    assert_eq!(got, ["/a/c", "/a-b", "/a.txt", "/x"]);
    let mut many: Vec<String> = Vec::new();
    for i in 0..300 {
        many.push(format!("/root/.cache/pkg{i}"));
        for j in 0..200 {
            many.push(format!("/root/.cache/pkg{i}/f{j}"));
        }
    }
    many.push("/root".into());
    many.push("/root/.cache".into());
    let started = std::time::Instant::now();
    let leaves = outside_leaves(many);
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(leaves.len(), 300 * 200);
}

/// Teste real (spec 130 T02): os três modos com o Docker de verdade, numa pasta temporária, com o
/// rótulo `nants.project` e tudo removido no fim. Imagem por `NANTS_TEST_RUN_IMAGE` (padrão
/// `alpine:3`); a página usa `python3 -m http.server` ou o `httpd` do busybox, o que houver.
/// Rodar: `NANTS_TEST_REAL_DOCKER=1 scripts/test.sh cargo test --lib real_app_modes -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_app_modes_by_behavior() {
    use crate::harness::compose::{ComposeEnv, ComposeKind};
    let image = env::var("NANTS_TEST_RUN_IMAGE").unwrap_or_else(|_| "alpine:3".into());
    let cases = [
        ("finished", "mkdir -p out && echo resultado > out/r.txt && echo pronto-130 && exit 0", None),
        ("running", "while true; do echo tick-130; sleep 1; done", None),
        (
            "page",
            "if command -v python3 >/dev/null; then exec python3 -m http.server 8080; else exec httpd -f -p 8080 -h /work; fi",
            Some("    ports:\n      - \"127.0.0.1::8080\"\n"),
        ),
    ];
    for (expected, command, ports) in cases {
        let root = plain_root(&format!("real-{expected}"), "COMPLETED", "");
        fs::create_dir_all(root.join(".nants")).expect("nants");
        let compose = format!("services:\n  workspace:\n    image: {image}\n{}", ports.unwrap_or(""));
        fs::write(root.join(".nants/compose.yaml"), compose).expect("compose");
        let id = format!("itest130{expected}{}", process::id());
        let env = ComposeEnv::real(id.clone()).with_kind(ComposeKind::Run);
        let env = Arc::new(env.with_approved_hashes(vec![crate::sdd::environment::env_hash(&root).expect("hash")]));
        let events = crate::server::events::EventBus::new();
        let mut rx = events.subscribe();
        let apps = AppManager::new(events).with_watch_interval(Duration::ZERO);
        let view = apps.launch_docker(&id, &root, SPEC, Arc::clone(&env), command, 3000).await;
        let logs = apps.docker_logs(&id).await.unwrap_or_default();
        apps.stop(&id, REASON_STOPPED).await;
        let _ = env.down_with(&root, true);
        let view = view.expect("subiu");
        eprintln!("[{expected}] mode={} exit_code={} finished={}", view["mode"], view["exit_code"], view["finished"]);
        assert_eq!(view["mode"], expected, "{view}");
        let events = drain(&mut rx);
        assert!(events.iter().any(|(t, p)| t == "app_started" && p["mode"] == expected), "{events:?}");
        match expected {
            "finished" => {
                assert_eq!(view["exit_code"], 0);
                assert!(view["finished"]["output"].as_str().unwrap_or("").contains("pronto-130"), "{view}");
                assert_eq!(view["finished"]["files"], json!([{"path": "out/r.txt", "change": "added"}]), "{view}");
                assert!(!view["finished"]["outside_work"].to_string().contains("docker-init"), "{view}");
            }
            "running" => assert!(logs.contains("tick-130"), "{logs}"),
            _ => assert!(view["ports"][0]["published"].as_u64().is_some_and(|p| p > 0), "{view}"),
        }
        assert!(!apps.is_docker_running(&id));
        let _ = fs::remove_dir_all(&root);
    }
}

/// Spec 130 T05: o comando mostrado pela janela sai redigido.
#[tokio::test]
async fn app_view_command_is_redacted() {
    let rec = Rec::with_ports();
    *rec.ps_output.lock().expect("lock") = format!("{}\n", ps_workspace("exited", 0));
    let root = root_with("m-cmd-secret", "COMPLETED", &run_cmd(&format!("python -m app --token {SECRET}")));
    let state = state_rec(&root, &rec);
    let (status, view) = start(&state).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let command = view["command"].as_str().expect("comando");
    assert!(command.starts_with("python -m app --token"), "{command}");
    assert!(!command.contains(SECRET), "{command}");
    let (_, text) = call(&state, req(Method::GET, &uri(""), "")).await;
    assert!(!text.contains(SECRET), "{text}");
}
