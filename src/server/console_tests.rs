//! Testes do Console do projeto (`console.rs`, spec 130 T03), com executor Compose falso: nenhum
//! teste chama o Docker real nem a rede. Incluídos por `console.rs`.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{env, process};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tower::ServiceExt;

use super::*;
use crate::config::{AgentConfig, Config, Connection, Effort, Project, WireApi};
use crate::harness::compose::{ComposeCall, ComposeOutput, ComposeRunner, OutputSink};
use crate::server::envs::tests::{docker_project, Rec};
use crate::server::envs::ComposeProvider;
use crate::server::{router, AppState};

const PROJECT: &str = "proj-console";
const SPEC: &str = "001-console";
const HOST: &str = "127.0.0.1:3000";
/// Valor da chave da conexão de teste: a redação do `SecretSet` o troca por `***`.
const PRIVATE: &str = "valor-privado-do-console-7311";

/// Como o `compose run` falso se comporta.
#[derive(Clone)]
enum Mode {
    /// Entrega os pedaços, grava `write` (relativo à raiz) e sai com `code`.
    Output { chunks: Vec<String>, code: i32, write: Option<String> },
    /// Mostra uma pergunta e espera entrada para sempre: só sai pelo parar ou pelo tempo.
    Block,
    /// Docker fora do ar.
    DockerDown,
}

/// Executor falso: `run_streamed` segue o [`Mode`]; o resto vai ao [`Rec`] (que registra tudo).
struct Fake {
    rec: Arc<Rec>,
    mode: Mutex<Mode>,
    root: PathBuf,
    /// O cancelamento chegou ao executor.
    cancelled: AtomicBool,
}

impl ComposeRunner for Fake {
    fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
        self.rec.run(call)
    }

    fn run_streamed(&self, call: &ComposeCall, sink: OutputSink, cancel: Arc<AtomicBool>) -> Result<ComposeOutput, String> {
        self.rec.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call.args.clone());
        let mode = self.mode.lock().unwrap_or_else(PoisonError::into_inner).clone();
        match mode {
            Mode::Output { chunks, code, write } => {
                if let Some(rel) = write {
                    let path = self.root.join(rel);
                    fs::create_dir_all(path.parent().expect("pai")).expect("dir");
                    fs::write(path, "resultado\n").expect("grava");
                }
                for c in &chunks {
                    sink(0, c.as_bytes());
                }
                Ok(ComposeOutput { code: Some(code), output: chunks.concat(), timed_out: false })
            }
            Mode::Block => {
                sink(0, b"Seu nome? ");
                let deadline = Instant::now() + call.timeout;
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        self.cancelled.store(true, Ordering::SeqCst);
                        return Ok(ComposeOutput { code: None, output: "Seu nome? ".into(), timed_out: false });
                    }
                    if Instant::now() >= deadline {
                        return Ok(ComposeOutput { code: None, output: "Seu nome? ".into(), timed_out: true });
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Mode::DockerDown => Ok(ComposeOutput {
                code: Some(1),
                output: "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?".into(),
                timed_out: false,
            }),
        }
    }
}

fn spec_md() -> String {
    format!("---\nid: \"{SPEC}\"\ntitle: \"Console\"\nstatus: \"COMPLETED\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-27T00:00:00Z\"\n---\nCorpo.\n")
}

fn root_for(tag: &str) -> PathBuf {
    let root = env::temp_dir().join(format!("nants-console-{}-{tag}", process::id()));
    let _ = fs::remove_dir_all(&root);
    let dir = root.join(".sdd/specs").join(SPEC);
    fs::create_dir_all(&dir).expect("dir");
    fs::write(dir.join("spec.md"), spec_md()).expect("spec");
    fs::create_dir_all(root.join(".git")).expect("git");
    docker_project(&root);
    root
}

fn agent() -> AgentConfig {
    AgentConfig { connection_id: "literal".into(), model: "m".into(), effort: Effort::Low, max_steps: None }
}

fn limits(timeout: Duration) -> ConsoleLimits {
    ConsoleLimits { timeout, stream_bytes: STREAM_MAX_BYTES, flush: Duration::from_millis(20), stop_wait: Duration::from_secs(5) }
}

struct Setup {
    state: AppState,
    fake: Arc<Fake>,
    root: PathBuf,
}

fn setup_with(tag: &str, mode: Mode, limits: ConsoleLimits) -> Setup {
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = root_for(tag);
    let fake = Arc::new(Fake { rec: Rec::with_ports(), mode: Mutex::new(mode), root: root.clone(), cancelled: AtomicBool::new(false) });
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "literal".into(),
        name: "c".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: PRIVATE.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
    cfg.agents.architect = agent();
    cfg.agents.developer = agent();
    cfg.agents.reviewer = agent();
    cfg.projects.push(Project {
        id: PROJECT.into(),
        name: "Console".into(),
        path: root.display().to_string(),
        created_at: "2026-10-27T00:00:00Z".into(),
    });
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = env::temp_dir().join(format!("nants-console-base-{}-{tag}-{n}", process::id()));
    let state = AppState::new(root.join("config.toml"), cfg, 3000)
        .with_data_dir(env::temp_dir().join(format!("nants-console-data-{}-{tag}-{n}", process::id())))
        .with_compose_provider(ComposeProvider::with_runner(fake.clone(), base))
        .with_app_observe_window(Duration::ZERO)
        .with_app_watch_interval(Duration::ZERO)
        .with_console_limits(limits);
    Setup { state, fake, root }
}

fn setup(tag: &str, mode: Mode) -> Setup {
    setup_with(tag, mode, limits(Duration::from_secs(20)))
}

fn output(chunks: &[&str], code: i32) -> Mode {
    Mode::Output { chunks: chunks.iter().map(|c| c.to_string()).collect(), code, write: None }
}

fn uri(suffix: &str) -> String {
    format!("/api/projects/{PROJECT}/app/exec{suffix}")
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

fn with_peer(mut r: Request<Body>, peer: &str) -> Request<Body> {
    let addr: std::net::SocketAddr = peer.parse().expect("endereço");
    r.extensions_mut().insert(axum::extract::ConnectInfo(addr));
    r
}

async fn call(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
    let request = if request.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().is_none() {
        with_peer(request, "127.0.0.1:50000")
    } else {
        request
    };
    let resp = router(state.clone()).oneshot(request).await.expect("resposta");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024).await.expect("corpo");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn body(command: &str) -> String {
    json!({"confirm": true, "spec_id": SPEC, "command": command}).to_string()
}

async fn exec(state: &AppState, command: &str) -> (StatusCode, Value) {
    call(state, req(Method::POST, &uri(""), &body(command))).await
}

async fn view(state: &AppState) -> Value {
    let (status, v) = call(state, req(Method::GET, &uri(""), "")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v
}

/// Espera o `app_exec_finished` e devolve todos os eventos do Console até ele.
async fn until_finished(rx: &mut broadcast::Receiver<crate::server::events::ApiEvent>) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let ev = tokio::time::timeout(left, rx.recv()).await.expect("app_exec_finished a tempo").expect("evento");
        let kind = ev.event_type().to_string();
        if kind.starts_with("app_exec") {
            out.push((kind.clone(), ev.payload.clone()));
        }
        if kind == "app_exec_finished" {
            return out;
        }
    }
}

fn finished(events: &[(String, Value)]) -> Value {
    events.iter().rfind(|(k, _)| k == "app_exec_finished").map(|(_, p)| p.clone()).expect("finished")
}

fn streamed(events: &[(String, Value)]) -> String {
    events.iter().filter(|(k, _)| k == "app_exec_output").filter_map(|(_, p)| p["text"].as_str()).collect()
}

/// Chamadas `compose run` do Console (args depois do override).
fn console_runs(rec: &Rec) -> Vec<(String, Vec<String>)> {
    rec.compose_calls().into_iter().filter(|(_, a)| a.first().is_some_and(|s| s == "run")).collect()
}

/// `docker rm -f <container>` recebidos.
fn removals(rec: &Rec) -> Vec<String> {
    rec.docker_calls("rm").into_iter().filter_map(|a| a.last().cloned()).collect()
}

async fn wait_active(state: &AppState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !state.apps().console().is_busy(PROJECT) {
        assert!(Instant::now() < deadline, "o comando não começou");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Espera o executor falso receber o comando (sob carga, o `is_busy` vem antes de o executor
/// começar; um parar nesse intervalo encerra o comando sem chegar ao laço que marca o
/// cancelamento).
async fn wait_started(fake: &Fake) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while console_runs(&fake.rec).is_empty() {
        assert!(Instant::now() < deadline, "o executor não recebeu o comando");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ---------- Proteções ----------

#[tokio::test]
async fn exec_requires_confirmation_a_valid_body_and_a_single_line_command() {
    let s = setup("confirm", output(&["ok\n"], 0));
    for (raw, code) in [
        ("", "body_invalid"),
        ("{}", "body_invalid"),
        (r#"{"spec_id":"001-console","command":"ls"}"#, "body_invalid"),
        (r#"{"confirm":"true","spec_id":"001-console","command":"ls"}"#, "body_invalid"),
        (r#"{"confirm":true,"spec_id":"001-console"}"#, "body_invalid"),
        (r#"{"confirm":true,"spec_id":"001-console","command":"ls","extra":1}"#, "body_invalid"),
        (r#"{"confirm":false,"spec_id":"001-console","command":"ls"}"#, "app_confirm_required"),
        (r#"{"confirm":true,"spec_id":"001-console","command":"   "}"#, CODE_EXEC_COMMAND_INVALID),
        (r#"{"confirm":true,"spec_id":"001-console","command":"ls\nrm -rf /work"}"#, CODE_EXEC_COMMAND_INVALID),
        (r#"{"confirm":true,"spec_id":"001-console","command":"ls\u0000"}"#, CODE_EXEC_COMMAND_INVALID),
    ] {
        let (status, v) = call(&s.state, req(Method::POST, &uri(""), raw)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{raw}: {v}");
        assert_eq!(v["code"], code, "{raw}: {v}");
    }
    let long = "a".repeat(MAX_COMMAND_CHARS + 1);
    let (status, v) = exec(&s.state, &long).await;
    assert_eq!((status, v["code"].as_str()), (StatusCode::BAD_REQUEST, Some(CODE_EXEC_COMMAND_INVALID)));
    assert_eq!(v["params"]["max"], MAX_COMMAND_CHARS);
    let (status, _) = call(&s.state, req(Method::POST, &uri(""), &json!({"confirm": true, "spec_id": "../x", "command": "ls"}).to_string())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, v) = call(&s.state, req(Method::POST, &uri(""), &json!({"confirm": true, "spec_id": "002-outra", "command": "ls"}).to_string())).await;
    assert_eq!((status, v["code"].as_str()), (StatusCode::NOT_FOUND, Some("spec_not_found")));
    assert!(s.fake.rec.calls.lock().expect("lock").is_empty(), "nada rodou");
}

#[tokio::test]
async fn docker_inside_the_command_is_refused_and_the_wrapped_form_runs_the_inner_command() {
    let s = setup("docker", output(&["ok\n"], 0));
    for cmd in ["docker ps", "ls && docker run alpine", "podman images", "/usr/bin/docker-compose up"] {
        let (status, v) = exec(&s.state, cmd).await;
        assert_eq!((status, v["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some(CODE_EXEC_COMMAND_DOCKER)), "{cmd}");
    }
    assert!(s.fake.rec.calls.lock().expect("lock").is_empty());
    let mut rx = s.state.events().subscribe();
    let (status, v) = exec(&s.state, "docker compose run --rm workspace sh -lc 'python -m app --help'").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    until_finished(&mut rx).await;
    let runs = console_runs(&s.fake.rec);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].1.last().map(String::as_str), Some("python -m app --help"));
}

#[tokio::test]
async fn cross_origin_exec_is_forbidden() {
    let s = setup("origin", output(&["ok\n"], 0));
    let r = Request::builder()
        .method(Method::POST)
        .uri(uri(""))
        .header(header::HOST, HOST)
        .header(header::ORIGIN, "http://evil.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body("ls")))
        .expect("req");
    assert_eq!(call(&s.state, r).await.0, StatusCode::FORBIDDEN);
    assert!(s.fake.rec.calls.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn in_network_mode_only_an_authorized_device_runs_and_the_history_keeps_it() {
    let s = setup("device", output(&["ok\n"], 0));
    let state = s.state.with_exposed(true);
    let peer = "192.168.0.50:40000";
    let (status, _) = call(&state, with_peer(req(Method::POST, &uri(""), &body("ls")), peer)).await;
    assert!(status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN, "{status}");
    assert!(s.fake.rec.calls.lock().expect("lock").is_empty());
    let code = match state.devices().open_window().expect("janela") {
        crate::server::devices::CodeState::Open { code, .. } => code,
        other => panic!("{other:?}"),
    };
    let (device, token) = state.devices().pair(&code, "Celular".into()).expect("pareia");
    let mut rx = state.events().subscribe();
    let mut r = with_peer(req(Method::POST, &uri(""), &body("ls")), peer);
    r.headers_mut().insert(header::COOKIE, format!("{}={token}", crate::server::devices::COOKIE_NAME).parse().expect("cookie"));
    let (status, v) = call(&state, r).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["origin"], json!({"device": {"id": device.id, "name": "Celular"}, "loopback": false}));
    until_finished(&mut rx).await;
    let history = view(&state).await["history"].clone();
    assert_eq!(history[0]["origin"]["device"]["name"], "Celular");
    assert!(history[0]["at"].as_str().is_some_and(|t| !t.is_empty()), "{history}");
}

#[tokio::test]
async fn exec_without_execute_readiness_is_412_and_nothing_runs() {
    let s = setup("ready", output(&["ok\n"], 0));
    s.state.config().write().await.agents.reviewer = AgentConfig::default();
    let (status, v) = exec(&s.state, "ls").await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{v}");
    assert_eq!(v["code"], "prereq_missing");
    assert!(s.fake.rec.calls.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn exec_needs_the_project_reservation_and_a_docker_project() {
    let s = setup("reserve", output(&["ok\n"], 0));
    let guard = s.state.runs().try_start(PROJECT, SPEC).expect("reserva");
    let (status, v) = exec(&s.state, "ls").await;
    assert_eq!((status, v["code"].as_str()), (StatusCode::CONFLICT, Some("run_active")));
    drop(guard);
    fs::write(s.root.join(".sdd/settings.yaml"), "base_branch: \"main\"\ntest_command: \"true\"\n").expect("settings");
    let (status, v) = exec(&s.state, "ls").await;
    assert_eq!((status, v["code"].as_str()), (StatusCode::CONFLICT, Some(crate::server::runs::PROJECT_NOT_DOCKER_CODE)));
    assert!(s.fake.rec.calls.lock().expect("lock").is_empty());
    let other = call(&s.state, req(Method::POST, "/api/projects/nao-existe/app/exec", &body("ls"))).await;
    assert_eq!(other.0, StatusCode::NOT_FOUND);
}

// ---------- Execução ----------

#[tokio::test]
async fn command_runs_in_an_ephemeral_isolated_run_container_with_live_redacted_output_code_and_files() {
    let mode = Mode::Output {
        chunks: vec!["linha 1\n".into(), format!("chave {PRIVATE}\n"), "fim sem quebra".into()],
        code: 3,
        write: Some("out/relatorio.csv".into()),
    };
    let s = setup("run", mode);
    fs::write(s.root.join("igual.txt"), "x").expect("arquivo");
    let mut rx = s.state.events().subscribe();
    let (status, v) = exec(&s.state, &format!("python -m app --key {PRIVATE}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["status"], "running");
    assert_eq!(v["stoppable"], true);
    assert_eq!(v["command"], "python -m app --key ***");
    assert_eq!(v["redacted"], true);
    assert_eq!(v["origin"], json!({"device": null, "loopback": true}));
    let events = until_finished(&mut rx).await;
    assert_eq!(events[0].0, "app_exec_started");
    assert_eq!(events[0].1["command"], "python -m app --key ***");
    let text = streamed(&events);
    assert_eq!(text, "linha 1\nchave ***\nfim sem quebra");
    let done = finished(&events);
    assert_eq!(done["status"], STATUS_EXITED);
    assert_eq!(done["exit_code"], 3);
    assert_eq!(done["files"], json!([{"path": "out/relatorio.csv", "change": "added"}]));
    assert_eq!(done["files_total"], 1);
    assert_eq!(done["notice"], Value::Null);
    // `compose run --rm -T` no projeto `-run`, com nome e rótulo do Nants; o comando roda como dado.
    let runs = console_runs(&s.fake.rec);
    assert_eq!(runs.len(), 1);
    let (project, args) = &runs[0];
    assert_eq!(project, "nants-proj-console-run");
    let name = args[3].clone();
    assert!(name.starts_with("nants-proj-console-run-console-"), "{name}");
    let tail: Vec<&str> = args.iter().map(String::as_str).collect();
    assert_eq!(
        tail,
        ["run", "--rm", "--name", name.as_str(), "--label", "nants.project=proj-console", "-T", "--entrypoint", "sh", "workspace", "-lc", &format!("python -m app --key {PRIVATE}")]
    );
    // O mesmo override de sempre: `.git` e `.nants` só leitura, uid do host e a pasta pessoal.
    let all = s.fake.rec.calls.lock().expect("lock").clone();
    let run_call = all.iter().find(|a| a.iter().any(|x| x == "-lc")).expect("run");
    let ov = run_call.iter().rposition(|x| x == "-f").map(|i| run_call[i + 1].clone()).expect("override");
    let text = fs::read_to_string(&ov).expect("override legível");
    for needle in ["target: /work/.git", "target: /work/.nants", "read_only: true", "user:", "HOME:"] {
        assert!(text.contains(needle), "{needle} em {text}");
    }
    // Saída normal: nada a remover à força.
    assert!(removals(&s.fake.rec).is_empty());
    // Resultado e histórico redigidos.
    let v = view(&s.state).await;
    assert_eq!(v["active"], Value::Null);
    assert_eq!(v["last"]["output"], "linha 1\nchave ***\nfim sem quebra");
    assert_eq!(v["last"]["exit_code"], 3);
    assert_eq!(v["last"]["stoppable"], false);
    assert_eq!(v["history"][0]["command"], "python -m app --key ***");
    assert_eq!(v["history"][0]["status"], STATUS_EXITED);
    assert_eq!(v["history"][0]["exit_code"], 3);
    assert_eq!(v["limits"]["timeout_secs"], 20);
    let saved = fs::read_to_string(s.state.data_dir().expect("dados").join("console").join(format!("{PROJECT}.json"))).expect("histórico");
    assert!(!saved.contains(PRIVATE), "{saved}");
    let mode = fs::metadata(s.state.data_dir().expect("dados").join("console").join(format!("{PROJECT}.json"))).expect("meta");
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777, 0o600);
    for (_, p) in &events {
        assert!(!p.to_string().contains(PRIVATE), "{p}");
    }
}

#[tokio::test]
async fn one_command_per_project_and_stop_ends_it_and_removes_the_container() {
    let s = setup("busy", Mode::Block);
    let mut rx = s.state.events().subscribe();
    assert_eq!(exec(&s.state, "python pergunta.py").await.0, StatusCode::ACCEPTED);
    wait_active(&s.state).await;
    wait_started(&s.fake).await;
    let (status, v) = exec(&s.state, "ls").await;
    assert_eq!((status, v["code"].as_str()), (StatusCode::CONFLICT, Some(CODE_EXEC_BUSY)));
    assert_eq!(view(&s.state).await["active"]["stoppable"], true);
    let (status, v) = call(&s.state, req(Method::POST, &uri("/stop"), "")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["active"], Value::Null);
    assert_eq!(v["last"]["status"], STATUS_STOPPED);
    let events = until_finished(&mut rx).await;
    let done = finished(&events);
    assert_eq!(done["status"], STATUS_STOPPED);
    assert_eq!(done["reason"], "stopped");
    assert!(s.fake.cancelled.load(Ordering::SeqCst), "o cliente recebeu o cancelamento");
    let name = console_runs(&s.fake.rec)[0].1[3].clone();
    assert!(removals(&s.fake.rec).contains(&name), "container removido pelo nome");
    assert_eq!(view(&s.state).await["history"][0]["status"], STATUS_STOPPED);
    // Livre de novo; parar sem comando é um no-op.
    assert_eq!(call(&s.state, req(Method::POST, &uri("/stop"), "")).await.0, StatusCode::OK);
    *s.fake.mode.lock().expect("lock") = output(&["ok\n"], 0);
    assert_eq!(exec(&s.state, "ls").await.0, StatusCode::ACCEPTED);
    until_finished(&mut rx).await;
}

#[tokio::test]
async fn an_interactive_program_that_times_out_gets_the_dictionary_notice() {
    let s = setup_with("timeout", Mode::Block, limits(Duration::from_millis(150)));
    let mut rx = s.state.events().subscribe();
    assert_eq!(exec(&s.state, "python pergunta.py").await.0, StatusCode::ACCEPTED);
    let events = until_finished(&mut rx).await;
    assert_eq!(streamed(&events), "Seu nome? ");
    let done = finished(&events);
    assert_eq!(done["status"], STATUS_TIMED_OUT);
    assert_eq!(done["exit_code"], Value::Null);
    assert_eq!(done["notice"], json!({"code": CODE_EXEC_TIMEOUT, "params": {"seconds": 0}, "action": "edit_command"}));
    let name = console_runs(&s.fake.rec)[0].1[3].clone();
    assert!(removals(&s.fake.rec).contains(&name), "container do comando estourado removido");
    for lang in [crate::i18n::Lang::PtBr, crate::i18n::Lang::En] {
        let text = crate::i18n::tr(lang, &format!("serverError.{CODE_EXEC_TIMEOUT}"), &[("seconds", "300")]);
        assert!(text.contains("300") && !text.contains("serverError"), "{text}");
    }
}

#[tokio::test]
async fn live_output_is_capped_with_a_cut_notice() {
    let big: Vec<String> = (0..50).map(|i| format!("linha {i:03} {}\n", "x".repeat(40))).collect();
    let mode = Mode::Output { chunks: big, code: 0, write: None };
    let mut lim = limits(Duration::from_secs(20));
    lim.stream_bytes = 300;
    let s = setup_with("cap", mode, lim);
    let mut rx = s.state.events().subscribe();
    assert_eq!(exec(&s.state, "cat grande.txt").await.0, StatusCode::ACCEPTED);
    let events = until_finished(&mut rx).await;
    let text = streamed(&events);
    assert!(text.len() <= 300, "{}", text.len());
    let outputs: Vec<&Value> = events.iter().filter(|(k, _)| k == "app_exec_output").map(|(_, p)| p).collect();
    assert_eq!(outputs.last().map(|p| p["truncated"].clone()), Some(json!(true)));
    assert_eq!(outputs.iter().filter(|p| p["truncated"] == true).count(), 1);
    assert_eq!(finished(&events)["status"], STATUS_EXITED);
}

#[tokio::test]
async fn docker_failure_is_reported_with_the_dictionary_code() {
    let s = setup("down", Mode::DockerDown);
    let mut rx = s.state.events().subscribe();
    assert_eq!(exec(&s.state, "ls").await.0, StatusCode::ACCEPTED);
    let done = finished(&until_finished(&mut rx).await);
    assert_eq!(done["status"], STATUS_FAILED);
    assert_eq!(done["notice"]["code"], CODE_EXEC_DOCKER_FAILED);
    assert!(done["detail"].as_str().is_some_and(|d| d.contains("Docker daemon")), "{done}");
}

#[tokio::test]
async fn removing_the_project_stops_the_command_and_forgets_the_history() {
    let s = setup("remove", Mode::Block);
    let mut rx = s.state.events().subscribe();
    assert_eq!(exec(&s.state, "python pergunta.py").await.0, StatusCode::ACCEPTED);
    wait_active(&s.state).await;
    let file = s.state.data_dir().expect("dados").join("console").join(format!("{PROJECT}.json"));
    assert!(file.exists());
    let (status, v) = call(&s.state, req(Method::DELETE, &format!("/api/projects/{PROJECT}"), "")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let done = finished(&until_finished(&mut rx).await);
    assert_eq!((done["status"].as_str(), done["reason"].as_str()), (Some(STATUS_STOPPED), Some("project_removed")));
    assert!(!s.state.apps().console().is_busy(PROJECT));
    assert!(!file.exists(), "histórico do projeto removido apagado");
}

#[tokio::test]
async fn nants_shutdown_stops_the_command() {
    let s = setup("shutdown", Mode::Block);
    let mut rx = s.state.events().subscribe();
    assert_eq!(exec(&s.state, "python pergunta.py").await.0, StatusCode::ACCEPTED);
    wait_active(&s.state).await;
    wait_started(&s.fake).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let (tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serve = tokio::spawn(crate::server::serve_with_shutdown(listener, s.state.clone(), async move {
        let _ = stop_rx.await;
    }));
    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(20), serve).await.expect("encerrou");
    let done = finished(&until_finished(&mut rx).await);
    assert_eq!((done["status"].as_str(), done["reason"].as_str()), (Some(STATUS_STOPPED), Some("shutdown")));
    assert!(s.fake.cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn stopping_the_app_does_not_stop_the_console() {
    let s = setup("appstop", Mode::Block);
    assert_eq!(exec(&s.state, "python pergunta.py").await.0, StatusCode::ACCEPTED);
    wait_active(&s.state).await;
    let (status, _) = call(&s.state, req(Method::POST, &format!("/api/projects/{PROJECT}/app/stop"), "")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(s.state.apps().console().is_busy(PROJECT));
    s.state.apps().console().stop(PROJECT, crate::server::app::REASON_STOPPED).await;
    assert!(!s.state.apps().console().is_busy(PROJECT));
}

/// `down` do `-run` (sem `-v`) recebidos.
fn run_downs(rec: &Rec) -> usize {
    rec.compose_calls().iter().filter(|(p, a)| p == "nants-proj-console-run" && a.first().is_some_and(|x| x == "down")).count()
}

#[tokio::test]
async fn services_raised_by_a_command_go_down_when_the_app_is_stopped_without_an_app() {
    let s = setup("deps-stop", Mode::Block);
    let mut rx = s.state.events().subscribe();
    assert_eq!(exec(&s.state, "python pergunta.py").await.0, StatusCode::ACCEPTED);
    wait_active(&s.state).await;
    // Com o comando em curso, parar o app não derruba o `-run` dele.
    assert_eq!(call(&s.state, req(Method::POST, &format!("/api/projects/{PROJECT}/app/stop"), "")).await.0, StatusCode::OK);
    assert_eq!(run_downs(&s.fake.rec), 0);
    s.state.apps().console().stop(PROJECT, crate::server::app::REASON_STOPPED).await;
    until_finished(&mut rx).await;
    assert!(s.state.apps().console().has_raised(PROJECT));
    let (status, _) = call(&s.state, req(Method::POST, &format!("/api/projects/{PROJECT}/app/stop"), "")).await;
    assert_eq!(status, StatusCode::OK);
    let downs: Vec<_> = s.fake.rec.compose_calls().into_iter().filter(|(_, a)| a[0] == "down").collect();
    assert_eq!(downs, vec![("nants-proj-console-run".to_string(), ["down", "--timeout", "5", "--remove-orphans"].map(String::from).to_vec())]);
    assert!(!s.state.apps().console().has_raised(PROJECT));
    // Já derrubado: parar de novo não chama o Docker.
    call(&s.state, req(Method::POST, &format!("/api/projects/{PROJECT}/app/stop"), "")).await;
    assert_eq!(run_downs(&s.fake.rec), 1);
}

#[tokio::test]
async fn services_raised_by_a_command_go_down_when_a_run_starts() {
    let s = setup("deps-run", output(&["ok\n"], 0));
    let mut rx = s.state.events().subscribe();
    assert_eq!(exec(&s.state, "ls").await.0, StatusCode::ACCEPTED);
    until_finished(&mut rx).await;
    assert_eq!(run_downs(&s.fake.rec), 0);
    // O que `runs.rs` chama ao iniciar um run do projeto.
    s.state.apps().stop(PROJECT, crate::server::app::REASON_RUN_STARTED).await;
    assert_eq!(run_downs(&s.fake.rec), 1);
    assert!(!s.state.apps().console().has_raised(PROJECT));
}

// ---------- Unidades ----------

/// Spec 130 T05: o progresso do Compose (rede, volumes, serviços de apoio) some da saída; os erros
/// do Compose e a saída do programa ficam.
#[test]
fn compose_progress_lines_are_hidden_but_errors_stay() {
    for line in [
        " Network nants-p-run_default  Creating\n",
        " Network nants-p-run_default  Created",
        " Container nants-p-run-db-1  Starting",
        "✔ Container nants-p-run-db-1  Started 0.4s",
        " Volume \"nants-p-run_data\"  Creating",
        "[+] Running 2/2",
        "[+] Creating 1/1",
    ] {
        assert!(is_compose_progress_line(line), "{line}");
    }
    for line in [
        " Container nants-p-run-db-1  Error",
        "Error response from daemon: driver failed programming external connectivity",
        "#5 [2/3] RUN apt-get install -y libpq-dev",
        "[+] Building 12.3s (8/8) FINISHED",
        "Imported 148 transactions",
        "Network unreachable while fetching data",
        "Container started",
    ] {
        assert!(!is_compose_progress_line(line), "{line}");
    }
    let text = " Network n_default  Creating\n Network n_default  Created\nImported 3\n Container n-db-1  Error\nfim";
    assert_eq!(strip_compose_progress(text), "Imported 3\n Container n-db-1  Error\nfim");
    // Ao vivo: só o stderr passa pelo filtro, e o teto conta o que saiu do fluxo.
    let mut s = Stream::default();
    s.push(1, b" Network n_default  Creating\n Network n_default  Created\n", 1000);
    assert_eq!(s.take(false), None, "só progresso: nada a enviar");
    s.push(1, b"Error response from daemon: boom\n", 1000);
    s.push(0, b"ok\n", 1000);
    assert_eq!(s.take(false), Some((1, "ok\nError response from daemon: boom\n".into(), false)));
}

#[tokio::test]
async fn a_command_that_ran_clears_the_old_console_failure_but_not_the_app_one() {
    let s = setup("clear-failure", Mode::DockerDown);
    let mut rx = s.state.events().subscribe();
    assert_eq!(exec(&s.state, "ls").await.0, StatusCode::ACCEPTED);
    until_finished(&mut rx).await;
    assert_eq!(s.state.apps().failures().get(PROJECT).map(|f| f.code), Some(CODE_EXEC_DOCKER_FAILED));
    *s.fake.mode.lock().unwrap_or_else(PoisonError::into_inner) = output(&["ok\n"], 3);
    assert_eq!(exec(&s.state, "ls").await.0, StatusCode::ACCEPTED);
    until_finished(&mut rx).await;
    assert!(s.state.apps().failures().get(PROJECT).is_none(), "o comando rodou: a falha do Console some");
    // A falha da partida do app não é do Console: fica.
    s.state.apps().failures().record(PROJECT, SPEC, "app_docker_up_failed", "log");
    assert_eq!(exec(&s.state, "ls").await.0, StatusCode::ACCEPTED);
    until_finished(&mut rx).await;
    assert_eq!(s.state.apps().failures().get(PROJECT).map(|f| f.code), Some("app_docker_up_failed"));
}

#[test]
fn stream_sends_whole_lines_long_partials_and_stops_at_the_cap() {
    let mut s = Stream::default();
    s.push(0, b"abc", 1000);
    assert_eq!(s.take(false), None, "linha sem fim espera");
    s.push(0, b"def\nghi", 1000);
    assert_eq!(s.take(false), Some((1, "abcdef\n".into(), false)));
    assert_eq!(s.take(true), Some((2, "ghi".into(), false)));
    s.push(0, &vec![b'z'; PARTIAL_MAX + 1], usize::MAX);
    assert_eq!(s.take(false).map(|(_, t, _)| t.len()), Some(PARTIAL_MAX + 1));
    let mut capped = Stream::default();
    capped.push(0, b"0123456789\n", 8);
    assert_eq!(capped.take(false), Some((1, String::new(), true)), "linha cortada no teto não sai");
    capped.push(0, b"mais\n", 8);
    assert_eq!(capped.take(true), None, "nada depois do corte");
}

#[test]
fn a_secret_crossing_the_cap_never_leaks_its_prefix() {
    let mut s = Stream::default();
    let line = format!("linha ok\nchave {PRIVATE}\n");
    let cap = "linha ok\nchave valor-pri".len();
    s.push(0, line.as_bytes(), cap);
    let (_, text, truncated) = s.take(false).expect("pedaço");
    assert_eq!((text.as_str(), truncated), ("linha ok\n", true));
    assert!(!text.contains("valor"), "{text}");
}

#[test]
fn stdout_and_stderr_interleaved_mid_line_keep_each_line_whole() {
    let mut s = Stream::default();
    s.push(0, b"chave valor-pri", 1000);
    s.push(1, b"aviso do stderr\n", 1000);
    s.push(0, b"vado-do-console-7311\n", 1000);
    let (_, text, _) = s.take(false).expect("pedaço");
    assert_eq!(text, format!("chave {PRIVATE}\naviso do stderr\n"));
    assert_eq!(scrub_text(&SecretSet::new([PRIVATE.to_string()]), &text), "chave ***\naviso do stderr\n");
    // O mesmo teto vale para os dois fluxos juntos.
    let mut capped = Stream::default();
    capped.push(0, b"12345\n", 10);
    capped.push(1, b"67890\n", 10);
    assert_eq!(capped.take(false), Some((1, "12345\n".into(), true)));
}

#[tokio::test]
async fn history_keeps_the_newest_entries_only() {
    let dir = env::temp_dir().join(format!("nants-console-hist-{}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    let manager = ConsoleManager::new(EventBus::new());
    for i in 0..(HISTORY_MAX + 5) {
        manager.record(Some(dir.clone()), PROJECT.into(), json!({"exec_id": format!("e{i}"), "command": "ls"})).await;
    }
    let list = read_history(&dir, PROJECT);
    assert_eq!(list.len(), HISTORY_MAX);
    assert_eq!(list[0]["exec_id"], "e5");
    let v = manager.view(Some(dir.clone()), PROJECT).await;
    assert_eq!(v["history"][0]["exec_id"], format!("e{}", HISTORY_MAX + 4), "mais novo primeiro");
    assert!(read_history(&dir, "../fora").is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn command_validation() {
    assert!(valid_command("ls -la\tout"));
    assert!(!valid_command(""));
    assert!(!valid_command("a\rb"));
    assert!(valid_command(&"é".repeat(MAX_COMMAND_CHARS)));
    assert!(!valid_command(&"é".repeat(MAX_COMMAND_CHARS + 1)));
}

// ---------- Docker real (manual) ----------

/// Validação com o Docker real: `NANTS_TEST_REAL_DOCKER=1` e, se quiser, `NANTS_TEST_RUN_IMAGE`
/// (imagem local; padrão `alpine:3`). Um comando que termina (código, saída e arquivos), um que
/// espera a entrada (stdin fechado: termina na hora), a escrita na pasta do Git recusada e um que
/// estoura o tempo (container removido).
#[tokio::test]
#[ignore = "Docker real: NANTS_TEST_REAL_DOCKER=1"]
async fn real_console_commands() {
    use crate::harness::compose::ComposeEnv;
    let image = env::var("NANTS_TEST_RUN_IMAGE").unwrap_or_else(|_| "alpine:3".into());
    let root = root_for("real");
    fs::write(root.join(".nants/compose.yaml"), format!("services:\n  workspace:\n    image: {image}\n")).expect("compose");
    let id = format!("itest130console{}", process::id());
    let env = ComposeEnv::real(id.clone()).with_kind(ComposeKind::Run);
    let env = Arc::new(env.with_approved_hashes(vec![crate::sdd::environment::env_hash(&root).expect("hash")]));
    let events = EventBus::new();
    let mut rx = events.subscribe();
    let manager = ConsoleManager::new(events).with_limits(limits(Duration::from_secs(4)));
    let protected = format!("touch {}/invasao", ".gi".to_owned() + "t");
    let cases = [
        ("termina", "echo pronto-130 && mkdir -p out && echo x > out/a.txt && id -u && exit 4".to_string()),
        ("stdin", "read nome; echo lido=[$nome]".to_string()),
        ("protegida", protected),
        ("tempo", "echo esperando; sleep 60".to_string()),
    ];
    let mut results = Vec::new();
    for (tag, command) in cases {
        let req = ExecRequest {
            project_id: id.clone(),
            spec_id: SPEC.into(),
            command: command.clone(),
            shown: command,
            redacted: false,
            origin: json!({"device": null, "loopback": true}),
            root: root.clone(),
            compose: Arc::clone(&env),
            secrets: Arc::new(SecretSet::default()),
            data_dir: None,
        };
        manager.launch(req).await.expect("livre");
        let evs = until_finished(&mut rx).await;
        let done = finished(&evs);
        eprintln!("[{tag}] status={} code={} ms={} files={} saida={:?}", done["status"], done["exit_code"], done["duration_ms"], done["files"], streamed(&evs));
        results.push((tag, done, streamed(&evs)));
    }
    let left = std::process::Command::new("docker")
        .args(["ps", "-a", "--filter", &format!("label=nants.project={id}"), "--format", "{{.Names}}"])
        .output()
        .expect("docker ps");
    let left = String::from_utf8_lossy(&left.stdout).to_string();
    let _ = env.down_with(&root, true);
    let _ = fs::remove_dir_all(&root);
    let (_, done, out) = &results[0];
    assert_eq!((done["status"].as_str(), done["exit_code"].as_i64()), (Some(STATUS_EXITED), Some(4)), "{done}");
    assert!(out.contains("pronto-130"), "{out}");
    assert_eq!(done["files"], json!([{"path": "out/a.txt", "change": "added"}]));
    let (_, done, out) = &results[1];
    assert_eq!(done["status"], STATUS_EXITED, "stdin fechado: termina sem esperar");
    assert!(out.contains("lido=[]"), "{out}");
    let (_, done, _) = &results[2];
    assert_ne!(done["exit_code"], 0, "pasta do Git só leitura");
    let (_, done, out) = &results[3];
    assert_eq!(done["status"], STATUS_TIMED_OUT);
    assert_eq!(done["notice"]["code"], CODE_EXEC_TIMEOUT);
    assert!(out.contains("esperando"), "{out}");
    assert!(left.trim().is_empty(), "containers que sobraram: {left}");
}
