//! Testes da prontidão (`readiness.rs`): níveis, cache e refresh, 412 `prereq_missing` em cada
//! rota e teto da conferência na hora. Executor de comandos falso; nenhum comando real.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::{AgentConfig, Config, Connection, Effort, WireApi};
use crate::server::prereqs::{CmdOutput, CmdRunner};
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";
const SECRET: &str = "sk-SEGREDO-READY-7171";

fn out(ok: bool) -> Option<CmdOutput> {
    Some(CmdOutput { success: ok, stdout: "v1".into(), stderr: if ok { String::new() } else { "Cannot connect".into() } })
}

/// Máquina falsa: Git e Docker ligáveis em tempo de execução; conta as chamadas.
struct Machine {
    git: AtomicBool,
    docker: AtomicBool,
    calls: AtomicUsize,
}

impl Machine {
    fn new(git: bool, docker: bool) -> Arc<Self> {
        Arc::new(Self { git: AtomicBool::new(git), docker: AtomicBool::new(docker), calls: AtomicUsize::new(0) })
    }

    fn runner(self: &Arc<Self>) -> CmdRunner {
        let me = Arc::clone(self);
        Arc::new(move |prog: &str, _: &[&str]| {
            me.calls.fetch_add(1, Ordering::SeqCst);
            match prog {
                "git" => me.git.load(Ordering::SeqCst).then(|| out(true)).flatten(),
                _ if me.docker.load(Ordering::SeqCst) => out(true),
                _ => None,
            }
        })
    }

    fn checker(self: &Arc<Self>) -> PrereqChecker {
        PrereqChecker::new(self.runner())
    }
}

fn agent(model: &str) -> AgentConfig {
    AgentConfig { connection_id: "c1".into(), model: model.into(), effort: Effort::Low, max_steps: None }
}

/// Conexão e os três agentes configurados.
fn ready_config() -> Config {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "c1".into(),
        name: "c".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
    cfg.agents.architect = agent("m-arq");
    cfg.agents.developer = agent("m-dev");
    cfg.agents.reviewer = agent("m-dev");
    cfg
}

fn items(git: bool, docker: bool) -> Vec<PrereqItem> {
    let m = Machine::new(git, docker);
    m.checker().check()
}

fn ids(level: &Level) -> Vec<&str> {
    level.missing.iter().map(|m| m.id.as_str()).collect()
}

fn secrets(cfg: &Config) -> SecretSet {
    SecretSet::from_config(cfg)
}

#[test]
fn levels_follow_the_table() {
    // Nada configurado, máquina ok.
    let cfg = Config::default();
    let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
    assert_eq!(ids(&r.chat), ["connection", "agent_architect"]);
    assert_eq!(ids(&r.create_project), ["connection", "agent_architect"]);
    assert_eq!(ids(&r.execute), ["agent_developer", "agent_reviewer"]);
    assert!(r.recommended.ok);
    assert!(r.setup_incomplete());

    // Tudo configurado, sem Git: chat ok; criar projeto e executar pedem Git.
    let cfg = ready_config();
    let r = evaluate(&cfg, &items(false, true), &secrets(&cfg));
    assert!(r.chat.ok && r.chat.missing.is_empty());
    assert_eq!(ids(&r.create_project), ["git"]);
    assert_eq!(ids(&r.execute), ["git"]);

    // Sem Docker: só `execute` cai, com os quatro itens, a correção e a aba Máquina.
    let r = evaluate(&cfg, &items(true, false), &secrets(&cfg));
    assert!(r.chat.ok && r.create_project.ok);
    assert_eq!(ids(&r.execute), ["docker_cli", "docker_daemon", "docker_permission", "docker_compose"]);
    assert!(r.execute.missing.iter().all(|m| m.settings_tab == "machine" && m.fix.is_some()));
    assert!(r.setup_incomplete());

    // Tudo pronto; preço ausente só recomenda (um id por modelo em uso, sem repetir).
    let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
    assert!(r.chat.ok && r.create_project.ok && r.execute.ok);
    assert!(!r.setup_incomplete());
    assert_eq!(ids(&r.recommended), ["pricing:m-arq", "pricing:m-dev"]);
    assert!(r.recommended.missing.iter().all(|m| m.settings_tab == "pricing"));

    // Agente apontando para conexão inexistente ou sem modelo não conta.
    let mut cfg = ready_config();
    cfg.agents.developer.connection_id = "sumiu".into();
    cfg.agents.reviewer.model = "  ".into();
    cfg.agents.architect.model = String::new();
    let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
    assert_eq!(ids(&r.chat), ["agent_architect"]);
    assert_eq!(ids(&r.execute), ["agent_developer", "agent_reviewer"]);
    assert_eq!(r.chat.missing[0].settings_tab, "agents");
}

/// Spec 102 T05: cada pendência leva `code` e `params` neutros; o `label` pt-BR fica de reserva.
#[test]
fn items_carry_a_neutral_code_and_params_with_the_pt_br_label_as_fallback() {
    let codes = |level: &Level| level.missing.iter().map(|m| m.code).collect::<Vec<_>>();
    let cfg = Config::default();
    let r = evaluate(&cfg, &items(false, false), &secrets(&cfg));
    assert_eq!(codes(&r.chat), ["connection", "agent_architect"]);
    assert_eq!(codes(&r.create_project), ["connection", "agent_architect", "git"]);
    assert_eq!(codes(&r.execute), ["agent_developer", "agent_reviewer", "git", "docker_cli", "docker_daemon", "docker_permission", "docker_compose"]);
    let all: Vec<&Missing> = r.create_project.missing.iter().chain(&r.execute.missing).collect();
    assert!(all.iter().all(|m| m.params.is_empty() && m.id == m.code));
    let label = |level: &Level, code: &str| level.missing.iter().find(|m| m.code == code).map(|m| m.label.clone());
    assert_eq!(label(&r.chat, "connection").as_deref(), Some("Conexão com um provedor de modelos"));
    assert_eq!(label(&r.execute, "docker_permission").as_deref(), Some("Permissão no socket do Docker"));

    let cfg = ready_config();
    let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
    let pricing = &r.recommended.missing[0];
    assert_eq!((pricing.code, pricing.label.as_str()), ("pricing", "Preço do modelo m-arq"));
    assert_eq!(serde_json::to_value(pricing).expect("json")["params"], json!({"model": "m-arq"}));
    // Sem parâmetro, o campo fica fora do JSON.
    let cfg = Config::default();
    let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
    let first = serde_json::to_value(&r.chat.missing[0]).expect("json");
    assert_eq!(first, json!({"id": "connection", "label": "Conexão com um provedor de modelos", "code": "connection", "settings_tab": "connections"}));

    // Teto estourado: o daemon pendente com o detalhe pelo dicionário (pt-BR de reserva e código neutro).
    let daemon = timed_out_items().into_iter().find(|i| i.id == "docker_daemon").expect("daemon");
    assert_eq!(daemon.label, "Docker daemon");
    assert_eq!(daemon.detail.as_deref(), Some("não respondeu a tempo"));
    let json = serde_json::to_value(&daemon).expect("json");
    assert_eq!(json["detail_code"], "daemon_timeout");
    assert_eq!(crate::server::doctor::format_line(&daemon, Lang::En), format!("FAIL  Docker daemon: did not answer in time{}", crate::i18n::tr(Lang::En, "doctor.action", &[("fix", &crate::i18n::text(Lang::En, "doctor.fix.docker_daemon"))])));
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

async fn call(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
    let resp = router(state.clone()).oneshot(request).await.expect("resposta");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("corpo");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn state_with(cfg: Config, machine: &Arc<Machine>) -> AppState {
    let dir = std::env::temp_dir().join(format!("nants-ready-{}", std::process::id()));
    AppState::new(dir.join("config.toml"), cfg, 3000).with_prereqs(machine.checker())
}

#[tokio::test]
async fn get_readiness_has_the_four_levels_and_the_item_shape() {
    let machine = Machine::new(true, false);
    let state = state_with(Config::default(), &machine);
    let (status, body) = call(&state, req(Method::GET, "/api/readiness", "")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for level in ["chat", "create_project", "execute", "recommended"] {
        assert!(body[level]["ok"].is_boolean() && body[level]["missing"].is_array(), "{level}: {body}");
    }
    assert_eq!(body["chat"]["ok"], false);
    let first = &body["chat"]["missing"][0];
    assert_eq!(first["id"], "connection");
    assert_eq!(first["settings_tab"], "connections");
    assert!(first["label"].is_string());
    assert!(first.get("fix").is_none(), "sem correção, o campo é omitido");
    let daemon = body["execute"]["missing"]
        .as_array()
        .expect("lista")
        .iter()
        .find(|m| m["id"] == "docker_daemon")
        .expect("daemon")
        .clone();
    assert!(daemon["fix"].as_str().is_some_and(|f| f.contains("Docker")));
    assert!(daemon.get("detail").is_none() && daemon.get("version").is_none(), "{daemon}");
    assert!(!body.to_string().contains(SECRET));
}

#[tokio::test]
async fn machine_is_cached_for_readiness_and_refresh_renews_it() {
    let machine = Machine::new(true, true);
    let state = state_with(ready_config(), &machine);
    let (_, body) = call(&state, req(Method::GET, "/api/readiness", "")).await;
    assert_eq!(body["execute"]["ok"], true);
    let after_first = machine.calls.load(Ordering::SeqCst);
    machine.docker.store(false, Ordering::SeqCst);
    let (_, body) = call(&state, req(Method::GET, "/api/readiness", "")).await;
    assert_eq!(body["execute"]["ok"], true, "dentro dos 30 s vale o cache");
    assert_eq!(machine.calls.load(Ordering::SeqCst), after_first, "nenhum comando novo");
    let (status, body) = call(&state, req(Method::POST, "/api/readiness/refresh", "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["execute"]["ok"], false, "{body}");
    assert!(machine.calls.load(Ordering::SeqCst) > after_first);
    // O refresh renovou o cache: a leitura seguinte já vê o Docker fora.
    let (_, body) = call(&state, req(Method::GET, "/api/readiness", "")).await;
    assert_eq!(body["execute"]["ok"], false);
}

#[tokio::test]
async fn attention_setup_incomplete_follows_chat_and_execute() {
    let machine = Machine::new(true, true);
    let state = state_with(ready_config(), &machine);
    let (_, body) = call(&state, req(Method::GET, "/api/attention", "")).await;
    assert_eq!(body["setup_incomplete"], false, "{body}");
    // Preço ausente (recomendado) não torna o setup incompleto; Docker fora, sim.
    machine.docker.store(false, Ordering::SeqCst);
    call(&state, req(Method::POST, "/api/readiness/refresh", "")).await;
    let (_, body) = call(&state, req(Method::GET, "/api/attention", "")).await;
    assert_eq!(body["setup_incomplete"], true, "{body}");
    let state = state_with(Config::default(), &Machine::new(true, true));
    let (_, body) = call(&state, req(Method::GET, "/api/attention", "")).await;
    assert_eq!(body["setup_incomplete"], true, "sem chat");
}

fn missing_of(body: &Value) -> Vec<String> {
    body["missing"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
}

const SPEC: &str = "001-x";

fn execute_routes() -> Vec<(String, String)> {
    vec![
        (format!("/api/projects/p1/specs/{SPEC}/approve"), "{}".into()),
        (format!("/api/projects/p1/specs/{SPEC}/resume"), "{}".into()),
        (format!("/api/projects/p1/specs/{SPEC}/tasks/T01/recovery-choice"), r#"{"guidance":"tente de novo"}"#.into()),
        ("/api/projects/p1/app/start".into(), json!({"confirm": true, "spec_id": SPEC}).to_string()),
    ]
}

#[tokio::test]
async fn chat_turn_without_chat_readiness_is_412() {
    let state = state_with(Config::default(), &Machine::new(true, true));
    let (status, body) = call(&state, req(Method::POST, "/api/projects/p1/chat", r#"{"message":"oi"}"#)).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    assert_eq!(body["error"], "prereq_missing");
    assert_eq!(missing_of(&body), ["connection", "agent_architect"]);
    // Corpo inválido continua 4xx de validação (a prontidão vem depois da forma do pedido).
    let (status, _) = call(&state, req(Method::POST, "/api/projects/p1/chat", "{x")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_project_without_git_is_412_in_both_formats() {
    let machine = Machine::new(false, true);
    let state = state_with(ready_config(), &machine);
    for body in [json!({"name": "Novo"}), json!({"name": "Novo", "path": "/tmp"})] {
        let (status, resp) = call(&state, req(Method::POST, "/api/projects", &body.to_string())).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}: {resp}");
        assert_eq!(missing_of(&resp), ["git"]);
    }
    assert!(state.config().read().await.projects.is_empty());
    let state = state_with(Config::default(), &machine);
    let (status, resp) = call(&state, req(Method::POST, "/api/projects", r#"{"name":"Novo"}"#)).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(missing_of(&resp), ["connection", "agent_architect", "git"]);
}

#[tokio::test]
async fn execute_routes_without_execute_readiness_are_412() {
    // Sem Developer/Revisor (o 422 de "agente não configurado" ficaria depois).
    let mut cfg = ready_config();
    cfg.agents.developer = AgentConfig::default();
    cfg.agents.reviewer = AgentConfig::default();
    let state = state_with(cfg, &Machine::new(true, true));
    for (uri, body) in execute_routes() {
        let (status, resp) = call(&state, req(Method::POST, &uri, &body)).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{uri}: {resp}");
        assert_eq!(resp["error"], "prereq_missing");
        assert_eq!(missing_of(&resp), ["agent_developer", "agent_reviewer"], "{uri}");
    }
    // Sem Docker na máquina.
    let state = state_with(ready_config(), &Machine::new(true, false));
    for (uri, body) in execute_routes() {
        let (status, resp) = call(&state, req(Method::POST, &uri, &body)).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{uri}: {resp}");
        assert_eq!(missing_of(&resp), ["docker_cli", "docker_daemon", "docker_permission", "docker_compose"], "{uri}");
    }
}

#[tokio::test]
async fn execute_routes_check_docker_now_ignoring_the_cache() {
    let machine = Machine::new(true, true);
    let state = state_with(ready_config(), &machine);
    // Cache aquecido com o Docker ok.
    let (_, body) = call(&state, req(Method::GET, "/api/readiness", "")).await;
    assert_eq!(body["execute"]["ok"], true);
    machine.docker.store(false, Ordering::SeqCst);
    for (uri, body) in execute_routes() {
        let (status, resp) = call(&state, req(Method::POST, &uri, &body)).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{uri}: {resp}");
        assert!(missing_of(&resp).contains(&"docker_daemon".to_string()), "{uri}: {resp}");
        machine.docker.store(false, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn fresh_check_has_a_total_ceiling() {
    assert_eq!(crate::server::prereqs::FRESH_CHECK_CEILING, Duration::from_secs(8));
    assert_eq!(PrereqChecker::real().fresh_ceiling(), Duration::from_secs(8));
    // `docker info` travado: a ação volta no teto com o daemon pendente.
    let runner: CmdRunner = Arc::new(|prog: &str, args: &[&str]| {
        if prog == "docker" && args.first() == Some(&"info") {
            std::thread::sleep(Duration::from_secs(3));
        }
        out(true)
    });
    let checker = PrereqChecker::new(runner).with_fresh_ceiling(Duration::from_millis(300));
    let dir = std::env::temp_dir().join(format!("nants-ready-ceiling-{}", std::process::id()));
    let state = AppState::new(dir.join("config.toml"), ready_config(), 3000).with_prereqs(checker);
    for (uri, body) in execute_routes() {
        let started = Instant::now();
        let (status, resp) = call(&state, req(Method::POST, &uri, &body)).await;
        assert!(started.elapsed() < Duration::from_secs(2), "{uri}: {:?}", started.elapsed());
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{uri}: {resp}");
        assert_eq!(missing_of(&resp), ["docker_daemon"], "{uri}");
    }
}

// --- T12: a chave da conexão do papel precisa ser resolvível ---

const ABSENT_VAR: &str = "NANTS_TEST_T12_READY_KEY_NEVER_SET";
const LEAK_URL: &str = "https://segredo-host-t12.example.com/v1";

fn conn(id: &str, name: &str, api_key: &str) -> Connection {
    Connection { id: id.into(), name: name.into(), base_url: LEAK_URL.into(), api_key: api_key.into(), wire_api: WireApi::default(), kind: Default::default(), base_url_override: None }
}

/// Três papéis prontos; o Arquiteto em `c1` e Developer/Revisor em `c2`, com as chaves dadas.
fn keyed_config(arch_key: &str, exec_key: &str) -> Config {
    let mut cfg = Config::default();
    cfg.connections.push(conn("c1", "Principal", arch_key));
    cfg.connections.push(conn("c2", "Execução", exec_key));
    cfg.agents.architect = agent("m-arq");
    cfg.agents.developer = AgentConfig { connection_id: "c2".into(), ..agent("m-dev") };
    cfg.agents.reviewer = AgentConfig { connection_id: "c2".into(), ..agent("m-dev") };
    cfg
}

fn key_ids(level: &Level) -> Vec<&str> {
    ids(level).into_iter().filter(|id| id.starts_with("connection_key:")).collect()
}

#[test]
fn unresolvable_key_is_pending_in_the_role_level() {
    let empty_var = "NANTS_TEST_T12_READY_KEY_EMPTY";
    let present_var = "NANTS_TEST_T12_READY_KEY_PRESENT";
    std::env::set_var(empty_var, "");
    std::env::set_var(present_var, SECRET);
    let machine = items(true, true);
    let bad = [String::new(), "  ".to_string(), format!("env:{ABSENT_VAR}"), format!("env:{empty_var}")];
    let good = [SECRET.to_string(), format!("env:{present_var}")];
    for key in &bad {
        // Arquiteto sem chave: `chat` e `create_project`; `execute` intacto.
        let cfg = keyed_config(key, SECRET);
        let r = evaluate(&cfg, &machine, &secrets(&cfg));
        assert_eq!(ids(&r.chat), ["connection_key:c1"], "{key:?}");
        assert_eq!(ids(&r.create_project), ["connection_key:c1"], "{key:?}");
        assert!(r.execute.ok, "{key:?}: {:?}", r.execute);
        let item = &r.chat.missing[0];
        assert_eq!(item.label, "Chave da conexão Principal");
        assert_eq!(item.settings_tab, "connections");
        assert!(item.fix.is_none());
        // Developer/Revisor sem chave: só `execute`, com um item só para a conexão compartilhada.
        let cfg = keyed_config(SECRET, key);
        let r = evaluate(&cfg, &machine, &secrets(&cfg));
        assert!(r.chat.ok && r.create_project.ok, "{key:?}");
        assert_eq!(ids(&r.execute), ["connection_key:c2"], "{key:?}");
        assert_eq!(r.execute.missing[0].label, "Chave da conexão Execução");
    }
    for key in &good {
        let cfg = keyed_config(key, key);
        let r = evaluate(&cfg, &machine, &secrets(&cfg));
        assert!(r.chat.ok && r.create_project.ok && r.execute.ok, "{key:?}: {r:?}");
    }
    std::env::remove_var(empty_var);
    std::env::remove_var(present_var);
}

#[test]
fn empty_base_url_is_pending_in_the_role_level() {
    let machine = items(true, true);
    for url in ["", "  ", "/"] {
        // Arquiteto sem endereço: `chat` e `create_project`.
        let mut cfg = keyed_config(SECRET, SECRET);
        cfg.connections[0].base_url = url.into();
        let r = evaluate(&cfg, &machine, &secrets(&cfg));
        assert_eq!(ids(&r.chat), ["connection_url:c1"], "{url:?}");
        assert_eq!(ids(&r.create_project), ["connection_url:c1"], "{url:?}");
        assert!(r.execute.ok, "{url:?}");
        assert_eq!(r.chat.missing[0].label, "Endereço da conexão Principal");
        assert_eq!(r.chat.missing[0].settings_tab, "connections");
        // Developer/Revisor na mesma conexão sem endereço nem chave: um item de cada, sem repetir.
        let mut cfg = keyed_config(SECRET, "");
        cfg.connections[1].base_url = url.into();
        let r = evaluate(&cfg, &machine, &secrets(&cfg));
        assert!(r.chat.ok, "{url:?}");
        assert_eq!(ids(&r.execute), ["connection_url:c2", "connection_key:c2"], "{url:?}");
    }
}

#[test]
fn key_item_is_not_repeated_for_a_shared_connection() {
    // Os três papéis na mesma conexão sem chave: um item por nível.
    let mut cfg = keyed_config(&format!("env:{ABSENT_VAR}"), SECRET);
    cfg.agents.developer.connection_id = "c1".into();
    cfg.agents.reviewer.connection_id = "c1".into();
    let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
    assert_eq!(ids(&r.chat), ["connection_key:c1"]);
    assert_eq!(ids(&r.execute), ["connection_key:c1"]);
    // Developer e Revisor em conexões distintas, ambas sem chave: um item para cada.
    let mut cfg = keyed_config(SECRET, "");
    cfg.connections.push(conn("c3", "Revisão", &format!("env:{ABSENT_VAR}")));
    cfg.agents.reviewer.connection_id = "c3".into();
    let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
    assert_eq!(key_ids(&r.execute), ["connection_key:c2", "connection_key:c3"]);
    // Agente já pendente por conexão inexistente não gera item de chave.
    let mut cfg = keyed_config(SECRET, "");
    cfg.agents.developer.connection_id = "sumiu".into();
    cfg.agents.reviewer.connection_id = "sumiu".into();
    let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
    assert_eq!(ids(&r.execute), ["agent_developer", "agent_reviewer"]);
}

#[tokio::test]
async fn key_item_json_has_no_sensitive_data() {
    // Arquiteto e execução com `env:` ausente; um segredo literal (de outra conexão) no nome de `c2`
    // prova que o rótulo passa pelo filtro de segredos.
    let mut cfg = keyed_config(&format!("env:{ABSENT_VAR}"), &format!("env:{ABSENT_VAR}"));
    cfg.connections.push(conn("lit", "Literal", SECRET));
    cfg.connections[1].name = format!("Exec {SECRET}");
    let state = state_with(cfg, &Machine::new(true, true));
    let (status, body) = call(&state, req(Method::GET, "/api/readiness", "")).await;
    assert_eq!(status, StatusCode::OK);
    let item = &body["chat"]["missing"][0];
    assert_eq!(
        item,
        &json!({"id": "connection_key:c1", "label": "Chave da conexão Principal", "code": "connection_key", "params": {"name": "Principal"}, "settings_tab": "connections"})
    );
    // O nome da conexão com o segredo vai filtrado também no `params`.
    let exec = body["execute"]["missing"].as_array().expect("lista").iter().find(|m| m["id"] == "connection_key:c2").expect("c2").clone();
    assert_eq!(exec["code"], "connection_key");
    assert_eq!(exec["params"]["name"], exec["label"].as_str().unwrap_or("").trim_start_matches("Chave da conexão "));
    let text = body.to_string();
    for leak in [SECRET, ABSENT_VAR, "env:", LEAK_URL, "segredo-host-t12"] {
        assert!(!text.contains(leak), "{leak} vazou: {text}");
    }
    assert!(text.contains("connection_key:c2"), "{text}");
    // Item de endereço: mesma forma, sem dado sensível.
    let mut cfg = keyed_config(SECRET, SECRET);
    cfg.connections[0].base_url = String::new();
    let state = state_with(cfg, &Machine::new(true, true));
    let (_, body) = call(&state, req(Method::GET, "/api/readiness", "")).await;
    assert_eq!(
        body["chat"]["missing"],
        json!([{"id": "connection_url:c1", "label": "Endereço da conexão Principal", "code": "connection_url", "params": {"name": "Principal"}, "settings_tab": "connections"}])
    );
    let text = body.to_string();
    for leak in [SECRET, LEAK_URL, "segredo-host-t12"] {
        assert!(!text.contains(leak), "{leak} vazou: {text}");
    }
}

#[tokio::test]
async fn chat_readiness_matches_the_architect_client() {
    let present_var = "NANTS_TEST_T12_CLIENT_KEY_PRESENT";
    std::env::set_var(present_var, SECRET);
    // (chave, base_url, prontidão esperada, o cliente também recusa)
    let cases = [
        (format!("env:{ABSENT_VAR}"), LEAK_URL, false, true),
        (String::new(), LEAK_URL, false, false),
        (SECRET.to_string(), "", false, true),
        (SECRET.to_string(), "/", false, true),
        (SECRET.to_string(), LEAK_URL, true, false),
        (format!("env:{present_var}"), LEAK_URL, true, false),
    ];
    for (key, url, expect_ok, client_refuses) in cases {
        // A mesma `Config` alimenta a prontidão e o cliente do Arquiteto.
        let mut cfg = keyed_config(&key, SECRET);
        cfg.connections[0].base_url = url.into();
        let state = state_with(cfg, &Machine::new(true, true));
        let readiness = current(&state).await;
        let client = crate::server::chat::architect_client(&state).await;
        assert_eq!(readiness.chat.ok, expect_ok, "{key:?} {url:?}");
        // Com `chat.ok`, o cliente nunca falha por configuração.
        if readiness.chat.ok {
            assert!(client.is_ok(), "{key:?} {url:?}");
        }
        // Variável ausente ou endereço vazio: os dois recusam.
        if client_refuses {
            assert!(client.is_err(), "{key:?} {url:?}");
        }
    }
    std::env::remove_var(present_var);
}

#[tokio::test]
async fn protected_routes_412_with_the_key_item() {
    let absent = format!("env:{ABSENT_VAR}");
    // Arquiteto sem chave: turno do chat e criar projeto.
    let state = state_with(keyed_config(&absent, SECRET), &Machine::new(true, true));
    let (status, body) = call(&state, req(Method::POST, "/api/projects/p1/chat", r#"{"message":"oi"}"#)).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    assert_eq!(missing_of(&body), ["connection_key:c1"]);
    let (status, body) = call(&state, req(Method::POST, "/api/projects", r#"{"name":"Novo"}"#)).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    assert_eq!(missing_of(&body), ["connection_key:c1"]);
    // Developer/Revisor sem chave: as rotas que executam.
    let state = state_with(keyed_config(SECRET, &absent), &Machine::new(true, true));
    for (uri, body) in execute_routes() {
        let (status, resp) = call(&state, req(Method::POST, &uri, &body)).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{uri}: {resp}");
        assert_eq!(missing_of(&resp), ["connection_key:c2"], "{uri}");
        assert!(!resp.to_string().contains(ABSENT_VAR));
    }
}

/// Spec 116 T02: a conexão pelo plano ChatGPT não pede chave; pede o login utilizável. Os modelos
/// dela não entram na recomendação de preço (lá não há US$).
#[test]
fn plan_connection_needs_login_not_key_nor_price() {
    let mut cfg = ready_config();
    cfg.connections[0] = Connection::chatgpt_plan("c1", "ChatGPT (p)");
    let r = evaluate_with(&cfg, &items(true, true), &secrets(&cfg), &|_| false);
    assert_eq!(ids(&r.chat), ["connection_plan:c1"]);
    assert_eq!(r.chat.missing[0].code, "connection_plan");
    assert_eq!(r.chat.missing[0].settings_tab, "connections");
    assert!(ids(&r.execute).contains(&"connection_plan:c1"));
    assert!(!ids(&r.execute).iter().any(|id| id.starts_with("connection_key")));
    assert!(ids(&r.recommended).is_empty(), "sem preço para os modelos do plano");
    // Com o login utilizável, nada pendente; `evaluate` sem a consulta conta como sem login.
    let r = evaluate_with(&cfg, &items(true, true), &secrets(&cfg), &|id| id == "c1");
    assert!(r.chat.ok && r.execute.ok, "{:?} {:?}", r.chat, r.execute);
    assert!(!evaluate(&cfg, &items(true, true), &secrets(&cfg)).chat.ok);
}

/// Spec 132 T08: a conexão Anthropic ou Gemini com chave resolvível conta como pronta (endereço
/// oficial ou avançado, sem `base_url` próprio); sem chave, a pendência é a da chave.
#[test]
fn native_connection_with_a_key_is_ready() {
    use crate::config::ConnectionKind;
    for kind in [ConnectionKind::Anthropic, ConnectionKind::GoogleGemini] {
        let mut cfg = ready_config();
        cfg.connections[0] = Connection::native("c1", "Nativa", kind, SECRET);
        let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
        assert!(r.chat.ok && r.create_project.ok && r.execute.ok, "{kind:?}: {r:?}");
        cfg.connections[0].base_url_override = Some("https://proxy.example.com".into());
        cfg.connections[0].normalize();
        let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
        assert!(r.chat.ok && r.execute.ok, "{kind:?} com o avançado: {r:?}");
        cfg.connections[0].api_key = format!("env:{ABSENT_VAR}");
        let r = evaluate(&cfg, &items(true, true), &secrets(&cfg));
        assert_eq!(ids(&r.chat), ["connection_key:c1"], "{kind:?}");
    }
}
