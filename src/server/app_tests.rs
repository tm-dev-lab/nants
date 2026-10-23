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

/// Saída de `compose ps` com uma porta publicada (`web`, alvo 8080).
fn ps_with_port(published: u16) -> String {
    format!(
        "{}\n",
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
    assert_eq!(view["mode"], "docker");
    assert_eq!(view["ports"][0]["url"], "http://192.168.1.50:18080");
    assert_eq!(view["ports"][0]["service"], "web");
    assert!(view["ports"][0].get("hint").is_none());
    assert_eq!(view["ports"].as_array().map(Vec::len), Some(1), "porta duplicada IPv6 e interna não entram");
    let calls = rec.compose_calls();
    assert_eq!(calls[0].0, "nants-proj-app-run");
    assert_eq!(calls[0].1, ["up", "-d", "--build", "--wait"]);
    assert_eq!(calls[1].1, ["ps", "--format", "json"]);

    // Cada consulta usa o Host da própria requisição.
    let (_, text) = call(&state, req_host(Method::GET, &uri(""), "localhost:3000", "")).await;
    assert!(text.contains("http://localhost:18080"), "{text}");
    let (_, text) = call(&state, req_host(Method::GET, &uri(""), "127.0.0.1:3000", "")).await;
    assert!(text.contains("http://127.0.0.1:18080"), "{text}");

    let started = drain(&mut rx);
    assert!(started.iter().any(|(t, p)| t == "app_started" && p["ports"][0]["published"] == 18080), "{started:?}");
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
    assert!(events.iter().any(|(t, p)| t == "app_exited" && p["reason"] == "stopped"), "{events:?}");
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
    assert_eq!(body, json!({"error": "prereq_missing", "missing": ["agent_reviewer"], "code": "prereq_missing", "params": {"count": 1}}));
    assert!(rec.compose_calls().is_empty());
    assert!(!state.apps().is_running(PROJECT));
}
