//! Testes da janela de execução nunca vazia (spec 130 T04): o conteúdo (com e sem relatório, e o
//! "não roda no sandbox"), o próximo passo de todo erro e o "Pedir ao Arquiteto para corrigir" com
//! um Arquiteto falso. Compose falso; nada chama o Docker nem a rede. Incluídos por `app_guide.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{env, process};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::agents::architect::testing::{calls, text, Scripted};
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::Role as ChatRole;
use crate::config::{AgentConfig, Config, Connection, Effort, Project};
use crate::server::envs::tests::{docker_project, provider, Rec, COMPOSE_YAML};
use crate::server::{router, AppState};

const PROJECT: &str = "proj-guide";
const SPEC: &str = "001-guia";
const HOST: &str = "127.0.0.1:3000";
/// Valor da chave da conexão: a redação o troca onde aparecer.
const KEY: &str = "valor-da-conexao-redigido-4242";

fn root_for(tag: &str, frontmatter_extra: &str) -> PathBuf {
    let root = env::temp_dir().join(format!("nants-guide-{}-{tag}", process::id()));
    let _ = fs::remove_dir_all(&root);
    let dir = root.join(".sdd/specs").join(SPEC);
    fs::create_dir_all(dir.join("tasks")).expect("dir");
    fs::write(
        dir.join("spec.md"),
        format!(
            "---\nid: \"{SPEC}\"\ntitle: \"Guia\"\nstatus: \"COMPLETED\"\nbase_branch: \"main\"\n{frontmatter_extra}created_at: \"2026-10-27T00:00:00Z\"\n---\nCorpo.\n"
        ),
    )
    .expect("spec");
    docker_project(&root);
    root
}

fn task(root: &Path, id: &str, status: &str) {
    let md = format!(
        "---\nid: \"{id}\"\nspec_id: \"{SPEC}\"\ntitle: \"Tarefa {id}\"\nstatus: \"{status}\"\ndepends_on: []\nbranch: \"\"\nattempts: 0\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope: [\"src/**\"]\n---\nCorpo.\n"
    );
    fs::write(root.join(".sdd/specs").join(SPEC).join("tasks").join(format!("{id}.md")), md).expect("task");
}

fn agent() -> AgentConfig {
    AgentConfig { connection_id: "literal".into(), model: "m".into(), effort: Effort::Low, max_steps: None }
}

fn state_with(root: &Path, rec: &Arc<Rec>, backend: Option<Arc<Scripted>>) -> AppState {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "literal".into(),
        name: "c".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: KEY.into(),
        wire_api: Default::default(),
        kind: Default::default(),
    });
    cfg.agents.architect = agent();
    cfg.agents.developer = agent();
    cfg.agents.reviewer = agent();
    cfg.projects.push(Project {
        id: PROJECT.into(),
        name: "Guia".into(),
        path: root.display().to_string(),
        created_at: "2026-10-27T00:00:00Z".into(),
    });
    let state = AppState::new(root.join("config.toml"), cfg, 3000)
        .with_compose_provider(provider(rec))
        .with_app_observe_window(Duration::ZERO)
        .with_app_probe_timeout(Duration::from_millis(200))
        .with_app_watch_interval(Duration::ZERO);
    match backend {
        Some(b) => {
            let dynamic: Arc<dyn ChatBackend> = b;
            state.with_llm_backend(dynamic)
        }
        None => state,
    }
}

async fn call(state: &AppState, method: Method, uri: &str, body: &str) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("requisição");
    let peer: std::net::SocketAddr = ([127, 0, 0, 1], 50000).into();
    request.extensions_mut().insert(axum::extract::ConnectInfo(peer));
    let resp = router(state.clone()).oneshot(request).await.expect("resposta");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024).await.expect("corpo");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn uri(suffix: &str) -> String {
    format!("/api/projects/{PROJECT}/app{suffix}")
}

async fn guide(state: &AppState) -> (StatusCode, Value) {
    call(state, Method::GET, &uri(&format!("/guide/{SPEC}")), "").await
}

const REPORT: &str = "# Relatório\n\n## O que foi pedido e o que foi entregue\nUm CLI.\n\n## Como executar\n1. `python -m ledger resumo dados/exemplo.csv`\n\nCHAVE\n\n## Como testar\n`pytest`\n\n## Primeiros passos\nRode o resumo e veja o total.\n\n## Como parar\nNada a parar.\n";

#[tokio::test]
async fn guide_with_report_brings_how_to_use_and_the_proofs() {
    let root = root_for("report", "");
    task(&root, "T01", "DONE");
    task(&root, "T02", "DONE");
    task(&root, "T03", "IN_PROGRESS");
    let spec_dir = root.join(".sdd/specs").join(SPEC);
    fs::write(spec_dir.join("report.md"), REPORT.replace("CHAVE", KEY)).expect("report");
    fs::write(root.join("README.md"), "# Não deve aparecer\n").expect("readme");
    let dialog = [
        json!({"seq": 1, "ts": 1, "agent": "tests", "task_id": null, "attempt": null, "text": "verificação final na base: testes ✓ · checagens: build ✓"}),
    ];
    fs::write(spec_dir.join("dialog.jsonl"), format!("{}\n", dialog[0])).expect("dialog");
    fs::create_dir_all(spec_dir.join("captures")).expect("captures");
    fs::write(spec_dir.join("captures/t01-a1-inicio.png"), b"png").expect("png");
    fs::write(spec_dir.join("captures/NOME-RUIM.png"), b"png").expect("png");
    fs::write(spec_dir.join("captures/notas.txt"), b"x").expect("txt");
    let state = state_with(&root, &Rec::with_ports(), None);
    let (status, v) = guide(&state).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let how = &v["how_to_use"];
    assert_eq!(how["source"], "report");
    let md = how["markdown"].as_str().expect("markdown");
    assert!(md.starts_with("## Como executar\n") && md.contains("## Primeiros passos\nRode o resumo"), "{md}");
    assert!(!md.contains("Como testar") && !md.contains("Como parar") && !md.contains("Um CLI"), "{md}");
    assert!(!md.contains(KEY), "a chave sai redigida: {md}");
    assert!(!v.to_string().contains("Não deve aparecer"), "com relatório, o README não entra");
    assert_eq!(v["proofs"]["tasks_done"], 2);
    assert_eq!(v["proofs"]["tasks_total"], 3);
    assert_eq!(v["proofs"]["final_verification"]["passed"], true);
    assert!(v["proofs"]["final_verification"]["text"].as_str().unwrap_or("").contains("testes ✓"));
    assert_eq!(v["proofs"]["captures_total"], 1);
    assert_eq!(v["proofs"]["captures"][0]["name"], "t01-a1-inicio.png");
    assert_eq!(
        v["proofs"]["captures"][0]["url"],
        format!("/api/projects/{PROJECT}/specs/{SPEC}/captures/t01-a1-inicio.png")
    );
    assert_eq!(v["sandbox"], json!({"runs_here": true, "note": null}));
}

#[tokio::test]
async fn guide_in_english_report_uses_the_english_sections() {
    let root = root_for("report-en", "");
    let report = "# Report\n\n## How to run\n1. `npm start`\n\n## How to test\n`npm test`\n\n## Getting started\nOpen the page.\n";
    fs::write(root.join(".sdd/specs").join(SPEC).join("report.md"), report).expect("report");
    let (status, v) = guide(&state_with(&root, &Rec::with_ports(), None)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["how_to_use"]["markdown"], "## How to run\n1. `npm start`\n\n## Getting started\nOpen the page.");
}

#[tokio::test]
async fn guide_without_report_falls_back_to_the_readme_and_then_to_nothing() {
    let root = root_for("readme", "");
    task(&root, "T01", "DONE");
    fs::write(root.join("README.md"), format!("# Ledger\n\nUse `python -m ledger`.\n{KEY}\n")).expect("readme");
    let state = state_with(&root, &Rec::with_ports(), None);
    let (status, v) = guide(&state).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["how_to_use"]["source"], "readme");
    let md = v["how_to_use"]["markdown"].as_str().expect("markdown");
    assert!(md.contains("python -m ledger") && !md.contains(KEY), "{md}");
    assert_eq!(v["proofs"]["tasks_done"], 1);
    assert_eq!(v["proofs"]["final_verification"], Value::Null);
    assert_eq!(v["proofs"]["captures"], json!([]));

    fs::remove_file(root.join("README.md")).expect("rm");
    let (_, v) = guide(&state).await;
    assert_eq!(v["how_to_use"], Value::Null, "sem relatório nem README: nada inventado");
}

#[tokio::test]
async fn guide_explains_what_does_not_run_in_the_sandbox() {
    let note = "A janela nativa de desktop não abre no sandbox; o Console roda o núcleo.";
    let root = root_for("sandbox", &format!("not_in_sandbox: \"{note}\"\n"));
    let (status, v) = guide(&state_with(&root, &Rec::with_ports(), None)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["sandbox"], json!({"runs_here": false, "note": note}));
}

#[tokio::test]
async fn guide_errors_carry_the_next_step() {
    let root = root_for("guide-err", "");
    let state = state_with(&root, &Rec::with_ports(), None);
    let (status, v) = call(&state, Method::GET, &uri("/guide/999-nada"), "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    assert_eq!((v["code"].as_str(), v["action"].as_str()), (Some("spec_not_found"), Some(ACTION_RELOAD)));
    let (status, v) = call(&state, Method::GET, "/api/projects/outro/app/guide/001-guia", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["action"], ACTION_RELOAD);
}

// --- próximo passo de todo erro ---

/// Valores das constantes `const ERR_*`/`CODE_*: &str = "..."` e dos códigos literais em
/// `ApiError::coded(StatusCode::X, "...")` de um arquivo de código.
fn declared_codes(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in source.lines() {
        let t = line.trim_start();
        let t = t.strip_prefix("pub ").unwrap_or(t);
        if let Some(rest) = t.strip_prefix("const ") {
            let is_code = rest.starts_with("ERR_") || rest.starts_with("CODE_");
            if let (true, Some((_, value))) = (is_code, rest.split_once(": &str = \"")) {
                if let Some((code, _)) = value.split_once('"') {
                    out.push(code.to_string());
                }
            }
        }
    }
    let mut rest = source;
    while let Some(at) = rest.find("coded(StatusCode::") {
        rest = &rest[at + "coded(StatusCode::".len()..];
        // Só o literal logo depois do status (uma constante ali já foi lida acima).
        let Some((_, after)) = rest.split_once(", ") else { break };
        let Some(after) = after.strip_prefix('"') else { continue };
        if let Some((code, _)) = after.split_once('"') {
            if code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                out.push(code.to_string());
            }
        }
    }
    out
}

/// Critério 3: todo código de erro da janela tem um próximo passo explícito, e a lista cobre os
/// códigos declarados no app, no Console e aqui.
#[test]
fn every_app_error_code_has_a_next_step() {
    for code in APP_ERROR_CODES {
        assert!(explicit_action(code).is_some(), "{code} sem próximo passo");
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server");
    for file in ["app.rs", "console.rs", "app_guide.rs"] {
        let source = fs::read_to_string(dir.join(file)).expect("fonte");
        let codes = declared_codes(&source);
        assert!(!codes.is_empty(), "{file}: a varredura achou os códigos");
        for code in codes {
            // `app_port_remapped` é aviso de sucesso (a porta do host trocou), não erro.
            if code == super::super::app::CODE_PORT_REMAPPED {
                continue;
            }
            assert!(APP_ERROR_CODES.contains(&code.as_str()), "{file}: {code} fora de APP_ERROR_CODES");
        }
    }
    assert_eq!(explicit_action("app_docker_up_failed"), Some(ACTION_ASK_ARCHITECT_FIX));
    assert_eq!(explicit_action("app_no_run_command"), Some(ACTION_OPEN_CONSOLE), "o padrão é o Console");
    assert_eq!(error_action("turn_busy"), ACTION_RETRY, "código de outra área ainda tem um passo");
}

#[tokio::test]
async fn window_errors_always_carry_an_action() {
    let root = root_for("actions", "");
    let state = state_with(&root, &Rec::with_ports(), None);
    let cases = [
        (Method::POST, uri("/start"), json!({"confirm": false, "spec_id": SPEC}).to_string(), ACTION_CONFIRM),
        (Method::POST, uri("/start"), "{".to_string(), ACTION_RELOAD),
        (Method::POST, uri("/start"), json!({"confirm": true, "spec_id": SPEC}).to_string(), ACTION_OPEN_CONSOLE),
        (Method::POST, uri("/exec"), json!({"confirm": true, "spec_id": SPEC, "command": "docker ps"}).to_string(), ACTION_EDIT_COMMAND),
        (Method::POST, uri("/fix"), json!({"confirm": true, "spec_id": SPEC}).to_string(), ACTION_RETRY),
        (Method::POST, uri("/fix"), json!({"confirm": false, "spec_id": SPEC}).to_string(), ACTION_CONFIRM),
        (Method::GET, "/api/projects/outro/app".to_string(), String::new(), ACTION_RELOAD),
    ];
    for (method, path, body, action) in cases {
        let (status, v) = call(&state, method.clone(), &path, &body).await;
        assert!(status.is_client_error() || status.is_server_error(), "{method} {path} {body}: {status} {v}");
        assert!(v["code"].is_string(), "{v}");
        assert_eq!(v["action"], action, "{method} {path} {body}: {v}");
    }
    // Sucesso não ganha `action`.
    let (status, v) = call(&state, Method::GET, &uri(""), "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(v.get("action").is_none(), "{v}");
    assert_eq!(v["failure"], Value::Null);
}

// --- falha do ambiente e o Arquiteto ---

fn run_cmd(command: &str) -> String {
    format!("run_command: {}\n", serde_json::to_string(command).expect("json"))
}

/// Partida que falha no `up`, com o log do workspace contendo a chave e uma linha longa.
fn failing_up(rec: &Arc<Rec>) {
    *rec.fail_sub.lock().expect("lock") = Some("up".into());
    let long = "x".repeat(4000);
    *rec.logs_output.lock().expect("lock") = Some(format!("{long}\nworkspace-1 | erro de import {KEY}\nworkspace-1 | ModuleNotFoundError: ledger\n"));
}

fn last_user_message(scripted: &Scripted) -> String {
    let requests = scripted.requests();
    let first = requests.first().expect("o Arquiteto foi chamado");
    first
        .messages
        .iter()
        .rev()
        .find(|m| m.role == ChatRole::User)
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn docker_up_failure_offers_the_architect_with_the_redacted_log() {
    let root = root_for("fix", &run_cmd("python -m ledger resumo dados/exemplo.csv"));
    let rec = Rec::with_ports();
    failing_up(&rec);
    let scripted = Scripted::new(vec![text("O módulo ledger não está no PYTHONPATH; proponho um plano de correção.")]);
    let state = state_with(&root, &rec, Some(Arc::clone(&scripted)));

    let (status, v) = call(&state, Method::POST, &uri("/start"), &json!({"confirm": true, "spec_id": SPEC}).to_string()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["code"], "app_docker_up_failed");
    assert_eq!(v["action"], ACTION_ASK_ARCHITECT_FIX);
    let detail = v["detail"].as_str().expect("detail");
    assert!(detail.contains("ModuleNotFoundError") && !detail.contains(KEY), "fim do log redigido: {detail}");

    // A janela reaberta ainda mostra a falha com o caminho para o Arquiteto.
    let (_, view) = call(&state, Method::GET, &uri(""), "").await;
    assert_eq!(view["failure"]["code"], "app_docker_up_failed");
    assert_eq!(view["failure"]["action"], ACTION_ASK_ARCHITECT_FIX);
    assert_eq!(view["failure"]["spec_id"], SPEC);

    let (status, reply) = call(&state, Method::POST, &uri("/fix"), &json!({"confirm": true, "spec_id": SPEC}).to_string()).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(reply["reply"].as_str().unwrap_or("").contains("PYTHONPATH"), "{reply}");
    let asked = last_user_message(&scripted);
    assert!(asked.contains("app_docker_up_failed") && asked.contains(SPEC), "{asked}");
    assert!(asked.contains("ModuleNotFoundError: ledger"), "o diagnóstico vai junto: {asked}");
    assert!(asked.contains("python -m ledger resumo dados/exemplo.csv"), "{asked}");
    assert!(asked.contains("DADO NÃO CONFIÁVEL"), "{asked}");
    assert!(!asked.contains(KEY), "log redigido: {asked}");
    let log_part = asked.split("<<<").nth(1).unwrap_or("");
    assert!(log_part.chars().count() <= FIX_LOG_CHARS + 8, "log limitado: {}", log_part.chars().count());
    // Turno interno: a mensagem não aparece no histórico da Conversa, só a resposta.
    let (_, history) = call(&state, Method::GET, &format!("/api/projects/{PROJECT}/chat"), "").await;
    let turns = history["turns"].as_array().expect("turns");
    assert_eq!(turns.len(), 1, "{history}");
    assert_eq!(turns[0]["role"], "architect");

    // Uma partida que sobe limpa a falha (sem porta publicada: nada é sondado).
    *rec.fail_sub.lock().expect("lock") = None;
    *rec.ps_output.lock().expect("lock") =
        r#"{"Service":"workspace","Name":"nants-p-run-workspace-1","State":"running","ExitCode":0,"Publishers":[]}"#.to_string();
    let (status, _) = call(&state, Method::POST, &uri("/start"), &json!({"confirm": true, "spec_id": SPEC}).to_string()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.apps().failures().get(PROJECT).is_none());
    state.apps().stop_all(super::super::app::REASON_SHUTDOWN).await;
}

#[tokio::test]
async fn fix_without_a_failure_of_that_spec_asks_to_run_again() {
    let root = root_for("fix-none", &run_cmd("npm start"));
    let scripted = Scripted::new(vec![text("não deve ser chamado")]);
    let state = state_with(&root, &Rec::with_ports(), Some(Arc::clone(&scripted)));
    state.apps().failures().record(PROJECT, "002-outra", "app_docker_up_failed", "log");
    let (status, v) = call(&state, Method::POST, &uri("/fix"), &json!({"confirm": true, "spec_id": SPEC}).to_string()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!((v["code"].as_str(), v["action"].as_str()), (Some(CODE_FIX_NO_FAILURE), Some(ACTION_RETRY)));
    let (status, _) = call(&state, Method::POST, &uri("/fix"), r#"{"confirm":true,"spec_id":"001-guia","log":"forjado"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "o log nunca vem do corpo");
    assert!(scripted.requests().is_empty());
}

/// Se o Arquiteto tentar mudar o ambiente direto no turno da correção, a mudança não passa: o
/// ambiente só muda por um plano em rascunho, com a aprovação de sempre (030 e 074).
#[tokio::test]
async fn an_environment_change_from_the_fix_turn_needs_the_usual_approval() {
    let root = root_for("fix-env", &run_cmd("python -m ledger"));
    let rec = Rec::with_ports();
    failing_up(&rec);
    let compose = "services:\n  workspace:\n    image: python:3.12\n";
    let args = json!({"path": ".nants/compose.yaml", "content": compose});
    let scripted = Scripted::new(vec![
        calls(&[("c1", "write_sdd_artifacts", &args.to_string())]),
        text("Proponho um plano de correção do ambiente para você aprovar."),
    ]);
    let state = state_with(&root, &rec, Some(Arc::clone(&scripted)));
    let (status, _) = call(&state, Method::POST, &uri("/start"), &json!({"confirm": true, "spec_id": SPEC}).to_string()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, reply) = call(&state, Method::POST, &uri("/fix"), &json!({"confirm": true, "spec_id": SPEC}).to_string()).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(fs::read_to_string(root.join(".nants/compose.yaml")).expect("compose"), COMPOSE_YAML, "ambiente intacto");
    let requests = scripted.requests();
    let refused = requests.get(1).is_some_and(|r| {
        r.messages.iter().any(|m| m.role == ChatRole::Tool && m.content.contains("rascunho"))
    });
    assert!(refused, "a escrita do ambiente foi recusada fora do rascunho");
}

#[tokio::test]
async fn console_docker_failure_is_recorded_with_its_next_step() {
    let failures = FailureLog::default();
    failures.record(PROJECT, SPEC, "app_exec_docker_failed", &format!("{}\u{1b}[31mfim", "y".repeat(3000)));
    let got = failures.get(PROJECT).expect("guardada");
    assert_eq!(got.detail.chars().count(), FIX_LOG_CHARS);
    assert!(got.detail.ends_with(" [31mfim"), "sem caractere de controle: {:?}", &got.detail[got.detail.len() - 10..]);
    let view = failure_view(Some(got), &SecretSet::new(Vec::new()));
    assert_eq!(view["action"], ACTION_ASK_ARCHITECT_FIX);
    failures.clear(PROJECT);
    assert!(failures.get(PROJECT).is_none());
}
