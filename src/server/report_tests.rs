//! Testes do relatório final da spec (`report.rs`): geração após `integrated`, persistência local,
//! fallback com código fixo, idempotência, base limpa, redação e o GET. Incluídos por `report.rs`.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use std::{env, process};

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tower::ServiceExt;

use super::*;
use crate::agents::architect::testing::{calls, text, Reply, Scripted};
use crate::agents::architect::REPORT_FACTS_LABEL;
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::{ChatRequest, Role as ChatRole};
use crate::agents::LlmError;
use crate::config::{AgentConfig, Config, Connection, Effort, Project, WireApi};
use crate::server::events::ApiEvent;
use crate::server::{router, AppState};

const PROJECT: &str = "proj-report";
const SPEC: &str = "001-exemplo";
const SECRET: &str = "sk-SEGREDO-REPORT-4242";
const PORT: u16 = 3000;
const HOST: &str = "127.0.0.1:3000";
const GOOD_REPORT: &str = "# Relatório\n\n## Como executar\n1. `node --test`\n";

const SPEC_MD: &str = "---\nid: \"001-exemplo\"\ntitle: \"Exemplo\"\nstatus: \"COMPLETED\"\nbase_branch: \"main\"\ntest_command: \"node --test\"\ncreated_at: \"2026-10-14T00:00:00Z\"\n---\n## Contexto\nCorpo da spec.\n";
const TASK_MD: &str = "---\nid: \"T01\"\nspec_id: \"001-exemplo\"\ntitle: \"Primeira tarefa\"\nstatus: \"DONE\"\ndepends_on: []\nbranch: \"sdd/001-exemplo/T01\"\nattempts: 1\nmax_attempts: 3\nlast_review_verdict: \"APPROVED\"\nallowed_scope: [\"src/**\"]\ntest_evidence: \"node --test: 3 passed\"\n---\nCorpo da tarefa.\n";
const SETTINGS_YAML: &str = "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"node --test\"\n";

fn temp_root(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-report-{}-{tag}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("diretório temporário");
    dir
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=Teste", "-c", "user.email=teste@example.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Projeto com spec COMPLETED, uma tarefa DONE e settings. Com `git`, traz um commit base limpo.
fn project(tag: &str, git_repo: bool) -> PathBuf {
    let root = temp_root(tag);
    let spec_dir = root.join(".sdd/specs").join(SPEC);
    fs::create_dir_all(spec_dir.join("tasks")).expect("tasks");
    fs::write(root.join(".sdd/settings.yaml"), SETTINGS_YAML).expect("settings");
    fs::write(spec_dir.join("spec.md"), SPEC_MD).expect("spec");
    fs::write(spec_dir.join("tasks/T01.md"), TASK_MD).expect("task");
    if git_repo {
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "base"]);
    }
    root
}

fn report_path(root: &Path) -> PathBuf {
    root.join(".sdd/specs").join(SPEC).join(REPORT_FILE)
}

fn config(root: &Path, architect: bool) -> Config {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "literal".into(),
        name: "Conexão de teste".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
    });
    if architect {
        cfg.agents.architect = AgentConfig {
            connection_id: "literal".into(),
            model: "modelo-arq".into(),
            effort: Effort::Low,
            max_steps: None,
        };
    }
    cfg.projects.push(Project {
        id: PROJECT.into(),
        name: "Relatório".into(),
        path: root.display().to_string(),
        created_at: "2026-10-14T00:00:00Z".into(),
    });
    cfg
}

fn state(root: &Path, architect: bool, backend: Option<Arc<dyn ChatBackend>>) -> AppState {
    let state = AppState::new(root.join("config.toml"), config(root, architect), PORT);
    match backend {
        Some(backend) => state.with_report_backend(backend),
        None => state,
    }
}

/// Backend que nunca responde (para timeout e shutdown).
struct Hang;

impl ChatBackend for Hang {
    fn complete<'a>(
        &'a self,
        _req: &'a ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

fn drain(rx: &mut broadcast::Receiver<ApiEvent>) -> Vec<ApiEvent> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    out
}

/// Eventos de relatório (`report_ready` / `report_failed`) como `(tipo, payload)`.
fn report_events(events: &[ApiEvent]) -> Vec<(String, Value)> {
    events
        .iter()
        .filter(|e| e.event_type() == "report_ready" || e.event_type() == "report_failed")
        .map(|e| (e.event_type().to_string(), e.payload.clone()))
        .collect()
}

fn all_text(events: &[ApiEvent]) -> String {
    events
        .iter()
        .map(|e| e.payload.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn get(state: &AppState, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::HOST, HOST)
        .body(Body::empty())
        .expect("requisição");
    let resp = router(state.clone()).oneshot(req).await.expect("resposta");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.expect("corpo");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn report_uri(project: &str, spec: &str) -> String {
    format!("/api/projects/{project}/specs/{spec}/report")
}

/// Roda a geração como o gatilho de produção e devolve os eventos de relatório publicados.
async fn generate(state: &AppState, rx: &mut broadcast::Receiver<ApiEvent>) -> Vec<(String, Value)> {
    let root = PathBuf::from(state.config().read().await.projects[0].path.clone());
    after_integration(state, PROJECT, SPEC, root, state.shutdown_signal()).await;
    report_events(&drain(rx))
}

fn has_tool_error(req: &ChatRequest, id: &str) -> bool {
    req.messages.iter().any(|m| {
        m.role == ChatRole::Tool
            && m.tool_call_id.as_deref() == Some(id)
            && m.content.contains("erro:")
    })
}

#[tokio::test]
async fn happy_path_writes_report_and_publishes_ready() {
    let root = project("happy", true);
    let state = state(&root, true, Some(Scripted::new(vec![text(GOOD_REPORT)])));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events, vec![("report_ready".into(), json!({"spec_id": SPEC}))]);
    assert_eq!(fs::read_to_string(report_path(&root)).expect("relatório"), GOOD_REPORT);
    let names: Vec<String> = fs::read_dir(root.join(".sdd/specs").join(SPEC))
        .expect("spec")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.starts_with(".report.md.")), "{names:?}");
    assert!(state.runs().active(PROJECT).is_none(), "reserva liberada");
}

#[tokio::test]
async fn get_returns_stored_report_with_source_and_timestamp() {
    let root = project("get-ok", false);
    let state = state(&root, true, Some(Scripted::new(vec![text(GOOD_REPORT)])));
    let mut rx = state.events().subscribe();
    generate(&state, &mut rx).await;
    let (status, body) = get(&state, &report_uri(PROJECT, SPEC)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["markdown"], GOOD_REPORT);
    assert_eq!(body["source"], "architect");
    let at = body["generated_at"].as_str().expect("generated_at");
    assert!(at.ends_with('Z') && at.len() == 20, "{at}");
}

#[tokio::test]
async fn report_keeps_base_clean_and_is_locally_excluded() {
    let root = project("clean", true);
    let state = state(&root, true, Some(Scripted::new(vec![text(GOOD_REPORT)])));
    let mut rx = state.events().subscribe();
    generate(&state, &mut rx).await;
    assert!(report_path(&root).exists());
    assert_eq!(git(&root, &["status", "--porcelain"]), "", "base suja após o relatório");
    let exclude = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
    assert!(exclude.lines().any(|l| l == REPORT_EXCLUDE_LINE), "{exclude}");
    assert!(exclude.lines().any(|l| l == REPORT_TMP_EXCLUDE_LINE), "{exclude}");
}

#[tokio::test]
async fn provider_failure_publishes_fixed_code_without_secret_or_file() {
    let root = project("provider", true);
    let remote = format!("chave {SECRET} em https://api.example.com/v1/chat");
    let backend = Scripted::new(vec![Err(LlmError::Http { status: 400, body_snippet: remote, code: None })]);
    let state = state(&root, true, Some(backend));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(
        events,
        vec![("report_failed".into(), json!({"spec_id": SPEC, "code": "provider_http_400"}))]
    );
    assert!(!report_path(&root).exists());
    let raw = drain(&mut rx);
    assert!(!all_text(&raw).contains(SECRET));
    assert_eq!(fs::read_to_string(root.join(".sdd/specs").join(SPEC).join("spec.md")).expect("spec"), SPEC_MD);
}

#[tokio::test]
async fn empty_answer_is_fixed_bad_response_code() {
    let root = project("empty", false);
    let state = state(&root, true, Some(Scripted::new(vec![text("   ")])));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(
        events,
        vec![("report_failed".into(), json!({"spec_id": SPEC, "code": "provider_bad_response"}))]
    );
    assert!(!report_path(&root).exists());
}

#[tokio::test]
async fn silent_provider_hits_report_timeout_and_writes_nothing() {
    let root = project("timeout", false);
    let state = state(&root, true, Some(Arc::new(Hang)))
        .with_report_timeout(Duration::from_millis(50));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events, vec![("report_failed".into(), json!({"spec_id": SPEC, "code": "timeout"}))]);
    assert!(!report_path(&root).exists());
    assert!(state.runs().active(PROJECT).is_none(), "reserva liberada no timeout");
}

#[tokio::test]
async fn shutdown_cancels_generation_with_fixed_code() {
    let root = project("shutdown", false);
    let state = state(&root, true, Some(Arc::new(Hang)));
    let mut rx = state.events().subscribe();
    let task_state = state.clone();
    let task_root = root.clone();
    let shutdown = state.shutdown_signal();
    let handle = tokio::spawn(async move {
        after_integration(&task_state, PROJECT, SPEC, task_root, shutdown).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    state.trigger_shutdown();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("geração encerra com o shutdown")
        .expect("task");
    let events = report_events(&drain(&mut rx));
    assert_eq!(events, vec![("report_failed".into(), json!({"spec_id": SPEC, "code": "shutdown"}))]);
    assert!(!report_path(&root).exists());
}

#[tokio::test]
async fn missing_architect_is_not_configured_and_calls_nothing() {
    let root = project("noarch", false);
    let state = state(&root, false, None);
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(
        events,
        vec![("report_failed".into(), json!({"spec_id": SPEC, "code": "not_configured"}))]
    );
    assert!(!report_path(&root).exists());
}

#[tokio::test]
async fn unreadable_spec_fails_before_any_model_call() {
    let root = project("nospec", false);
    fs::remove_file(root.join(".sdd/specs").join(SPEC).join("spec.md")).expect("rm");
    let backend = Scripted::new(vec![text(GOOD_REPORT)]);
    let state = state(&root, true, Some(backend.clone()));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(
        events,
        vec![("report_failed".into(), json!({"spec_id": SPEC, "code": "spec_unreadable"}))]
    );
    assert!(backend.requests().is_empty());
}

#[tokio::test]
async fn existing_report_is_not_regenerated_nor_overwritten() {
    let root = project("existing", true);
    fs::write(report_path(&root), "relatório antigo\n").expect("antigo");
    let backend = Scripted::new(Vec::new());
    let state = state(&root, true, Some(backend.clone()));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events, vec![("report_ready".into(), json!({"spec_id": SPEC}))]);
    assert!(backend.requests().is_empty());
    assert_eq!(fs::read_to_string(report_path(&root)).expect("r"), "relatório antigo\n");
}

#[tokio::test]
async fn second_generation_is_idempotent() {
    let root = project("twice", true);
    let first = state(&root, true, Some(Scripted::new(vec![text(GOOD_REPORT)])));
    let mut first_rx = first.events().subscribe();
    generate(&first, &mut first_rx).await;
    let second_backend = Scripted::new(vec![text("outro texto")]);
    let second = state(&root, true, Some(second_backend.clone()));
    let mut second_rx = second.events().subscribe();
    let events = generate(&second, &mut second_rx).await;
    assert_eq!(events, vec![("report_ready".into(), json!({"spec_id": SPEC}))]);
    assert!(second_backend.requests().is_empty());
    assert_eq!(fs::read_to_string(report_path(&root)).expect("r"), GOOD_REPORT);
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn write_attempt_is_refused_and_project_files_stay_unchanged() {
    let root = project("readonly", false);
    let backend = Scripted::new(vec![
        calls(&[(
            "c1",
            "write_sdd_artifacts",
            &json!({"path": ".sdd/specs/001-exemplo/spec.md", "content": "x"}).to_string(),
        )]),
        text(GOOD_REPORT),
    ]);
    let state = state(&root, true, Some(backend.clone()));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events[0].0, "report_ready");
    assert!(has_tool_error(&backend.requests()[1], "c1"));
    assert_eq!(fs::read_to_string(root.join(".sdd/specs").join(SPEC).join("spec.md")).expect("s"), SPEC_MD);
    assert!(!root.join("src").exists());
}

#[tokio::test]
async fn facts_are_sent_as_labelled_data_without_secrets() {
    let root = project("facts", false);
    let backend = Scripted::new(vec![text(GOOD_REPORT)]);
    let state = state(&root, true, Some(backend.clone()));
    let mut rx = state.events().subscribe();
    generate(&state, &mut rx).await;
    let req = &backend.requests()[0];
    let facts = req
        .messages
        .iter()
        .find(|m| m.content.starts_with(REPORT_FACTS_LABEL))
        .expect("bloco de fatos");
    for needle in ["Spec: 001-exemplo", "node --test", "T01", "Primeira tarefa", "node --test: 3 passed"] {
        assert!(facts.content.contains(needle), "faltou {needle}: {}", facts.content);
    }
    assert!(!format!("{:?}", req.messages).contains(SECRET));
}

#[test]
fn facts_list_manual_items_with_task_id_redacted_and_omit_header_without_them() {
    let secrets = SecretSet::new(vec![SECRET.to_string()]);
    let root = project("manual-facts", false);
    let none = build_facts(&root, SPEC, &secrets).expect("fatos");
    assert!(!none.contains("Verificação manual sugerida"), "{none}");
    let task = TASK_MD.replace(
        "Corpo da tarefa.",
        &format!("## Critérios de Aceitação\n1. node --test verde\n2. - [ ] (Manual) abrir no navegador com {SECRET}\n"),
    );
    fs::write(root.join(".sdd/specs").join(SPEC).join("tasks/T01.md"), task).expect("task");
    let facts = build_facts(&root, SPEC, &secrets).expect("fatos");
    assert!(facts.contains("Verificação manual sugerida"), "{facts}");
    assert!(facts.contains("- T01: "), "{facts}");
    assert!(facts.contains("abrir no navegador"), "{facts}");
    assert!(!facts.contains("node --test verde"), "{facts}");
    assert!(!facts.contains(SECRET), "{facts}");
}

#[test]
fn facts_record_whether_profiles_were_on_and_their_updated_at() {
    let secrets = SecretSet::new(vec![SECRET.to_string()]);
    let root = project("skills-facts", false);
    let none = build_facts(&root, SPEC, &secrets).expect("fatos");
    assert!(none.contains("Perfis de especialista (skills): ligados"), "{none}");
    assert!(none.contains("- developer: sem perfil"), "{none}");
    let dir = root.join(".sdd/skills");
    fs::create_dir_all(&dir).expect("dir");
    fs::write(
        dir.join("developer.md"),
        format!("---\nrole: developer\nstack: [clojure]\nupdated_at: 2026-10-16\nsource_spec: {SPEC}\n---\n## Papel\n- usa {SECRET}\n"),
    )
    .expect("perfil");
    let facts = build_facts(&root, SPEC, &secrets).expect("fatos");
    assert!(facts.contains("- developer: updated_at=2026-10-16"), "{facts}");
    assert!(facts.contains("- reviewer: sem perfil"), "{facts}");
    assert!(!facts.contains(SECRET), "{facts}");
    let settings = root.join(".sdd/settings.yaml");
    let raw = fs::read_to_string(&settings).expect("settings");
    fs::write(&settings, format!("{raw}\nskills: false\n")).expect("settings");
    let off = build_facts(&root, SPEC, &secrets).expect("fatos");
    assert!(off.contains("desligados (skills: false)"), "{off}");
}

#[tokio::test]
async fn secret_in_answer_is_redacted_in_file_and_get() {
    let root = project("redact", false);
    let answer = format!("{GOOD_REPORT}\nUse a chave {SECRET} para autenticar.\n");
    let state = state(&root, true, Some(Scripted::new(vec![text(&answer)])));
    let mut rx = state.events().subscribe();
    generate(&state, &mut rx).await;
    let stored = fs::read_to_string(report_path(&root)).expect("relatório");
    assert!(!stored.contains(SECRET), "{stored}");
    let (_, body) = get(&state, &report_uri(PROJECT, SPEC)).await;
    assert!(!body.to_string().contains(SECRET), "{body}");
}

#[tokio::test]
async fn get_redacts_secrets_already_stored_in_file() {
    let root = project("get-redact", false);
    fs::write(report_path(&root), format!("antigo {SECRET}\n")).expect("w");
    let state = state(&root, true, None);
    let (status, body) = get(&state, &report_uri(PROJECT, SPEC)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.to_string().contains(SECRET), "{body}");
}

#[tokio::test]
async fn oversized_answer_is_truncated_to_the_limit() {
    let root = project("big", false);
    let big = format!("# Relatório\n{}", "linha longa de texto\n".repeat(10_000));
    let state = state(&root, true, Some(Scripted::new(vec![text(&big)])));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events[0].0, "report_ready");
    let len = fs::metadata(report_path(&root)).expect("meta").len() as usize;
    assert!(len <= REPORT_MAX_BYTES, "{len}");
}

#[tokio::test]
async fn get_is_404_when_missing_and_422_for_invalid_spec_id() {
    let root = project("get-404", false);
    let state = state(&root, true, None);
    let (status, _) = get(&state, &report_uri(PROJECT, SPEC)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(&state, &report_uri(PROJECT, "bad.id")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = get(&state, &report_uri("nao-existe", SPEC)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_refuses_symlinked_report() {
    let root = project("get-link", false);
    let outside = temp_root("get-link-outside");
    fs::write(outside.join("secreto.md"), "fora do projeto\n").expect("w");
    std::os::unix::fs::symlink(outside.join("secreto.md"), report_path(&root)).expect("ln");
    let state = state(&root, true, None);
    let (status, body) = get(&state, &report_uri(PROJECT, SPEC)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!body.to_string().contains("fora do projeto"));
}

/// Backend que entra em pânico ao ser chamado (F-05).
struct Boom;

impl ChatBackend for Boom {
    fn complete<'a>(
        &'a self,
        _req: &'a ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        Box::pin(async { panic!("falha simulada no turno do relatório") })
    }
}

#[tokio::test]
async fn panic_in_generation_is_reported_as_internal() {
    let root = project("panic", false);
    let state = state(&root, true, Some(Arc::new(Boom)));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events, vec![("report_failed".into(), json!({"spec_id": SPEC, "code": "internal"}))]);
    assert!(!report_path(&root).exists());
}

#[tokio::test]
async fn unavailable_git_is_reported_as_unknown_not_as_zero() {
    let root = project("nogit-log", false);
    // `.git` real sem commits: `git log <base>` falha, então o Git fica indisponível.
    git(&root, &["init", "-q"]);
    let backend = Scripted::new(vec![text(GOOD_REPORT)]);
    let state = state(&root, true, Some(backend.clone()));
    let mut rx = state.events().subscribe();
    generate(&state, &mut rx).await;
    let req = &backend.requests()[0];
    let facts = req
        .messages
        .iter()
        .find(|m| m.content.starts_with(REPORT_FACTS_LABEL))
        .expect("bloco de fatos");
    assert!(facts.content.contains("indisponível"), "{}", facts.content);
    assert!(!facts.content.contains("Commits de integração (0"), "{}", facts.content);
}

#[tokio::test]
async fn gitfile_worktree_is_refused_before_model_and_base_stays_clean() {
    let base = project("worktree-base", true);
    let linked = PathBuf::from(format!("{}-linked", base.display()));
    let _ = fs::remove_dir_all(&linked);
    git(&base, &["worktree", "add", "-q", linked.to_str().expect("utf8"), "-b", "wt-report"]);
    assert!(linked.join(".git").is_file(), "worktree usa gitfile");
    let backend = Scripted::new(vec![text(GOOD_REPORT)]);
    let state = state(&linked, true, Some(backend.clone()));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(
        events,
        vec![("report_failed".into(), json!({"spec_id": SPEC, "code": "git_unsupported"}))]
    );
    assert!(backend.requests().is_empty(), "sem exclude garantido não chama o modelo");
    assert!(!linked.join(".sdd/specs").join(SPEC).join(REPORT_FILE).exists());
    assert_eq!(git(&linked, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn completing_a_spec_updates_and_commits_architecture_memory() {
    let root = project("archmem", true);
    let mut doc = String::from("# Proj\n");
    for sec in crate::agents::arch_memory::section_headings(crate::i18n::Lang::PtBr) {
        doc.push_str(&format!("\n{sec}\ntexto\n"));
    }
    let backend = Scripted::new(vec![text(GOOD_REPORT), text(&doc)]);
    let state = state(&root, true, Some(backend.clone()));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events, vec![("report_ready".into(), json!({"spec_id": SPEC}))]);
    assert_eq!(backend.requests().len(), 2);
    assert!(fs::read_to_string(root.join("ARCHITECTURE.md")).expect("arquivo").contains("## Restrições"));
    assert!(git(&root, &["log", "-1", "--format=%s"]).contains("atualiza ARCHITECTURE.md"));
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn invalid_architecture_answer_changes_nothing() {
    let root = project("archbad", true);
    let backend = Scripted::new(vec![text(GOOD_REPORT), text("não é o arquivo")]);
    let state = state(&root, true, Some(backend));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events[0].0, "report_ready");
    assert!(!root.join("ARCHITECTURE.md").exists());
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn docker_project_test_environment_is_cleaned_before_report_and_noted_in_it() {
    use crate::server::envs::tests::{docker_project, provider, Rec};
    let root = project("docker-clean", false);
    docker_project(&root);
    let rec = Rec::with_ports();
    let state = state(&root, true, Some(Scripted::new(vec![text(GOOD_REPORT)]))).with_compose_provider(provider(&rec)).with_app_observe_window(Duration::ZERO);
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events, vec![("report_ready".into(), json!({"spec_id": SPEC}))]);
    let calls = rec.compose_calls();
    assert_eq!(calls.len(), 1, "só o -test; o -run não é tocado");
    assert_eq!(calls[0].0, format!("nants-{PROJECT}-test"));
    assert_eq!(calls[0].1, ["down", "-v", "--remove-orphans"]);
    let md = fs::read_to_string(report_path(&root)).expect("relatório");
    assert!(md.starts_with(GOOD_REPORT.trim_end()) && md.contains(&env_clean_line(crate::i18n::Lang::PtBr)), "{md}");
}

/// Spec 102: com a interface em en na hora da geração, os textos fixos do servidor saem em inglês e
/// o turno do relatório recebe o prompt em inglês.
#[tokio::test]
async fn report_fixed_texts_follow_the_active_language_at_generation() {
    use crate::server::envs::tests::{docker_project, provider, Rec};
    let root = project("docker-clean-en", false);
    docker_project(&root);
    let rec = Rec::with_ports();
    let backend = Scripted::new(vec![text(GOOD_REPORT)]);
    let state = state(&root, true, Some(backend.clone())).with_compose_provider(provider(&rec)).with_app_observe_window(Duration::ZERO);
    state.set_lang(Some(PROJECT), crate::i18n::Lang::En);
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events[0].0, "report_ready");
    let md = fs::read_to_string(report_path(&root)).expect("relatório");
    assert!(md.contains("_Test environment cleaned up"), "{md}");
    assert!(!md.contains("Ambiente de testes limpo"), "{md}");
    let system = backend.requests()[0].messages[0].content.clone();
    assert_eq!(system, crate::agents::prompts::report_system_for(crate::i18n::Lang::En));
}

#[tokio::test]
async fn host_project_report_has_no_environment_line_and_no_docker_calls() {
    use crate::server::envs::tests::{provider, Rec};
    let root = project("host-clean", false);
    let rec = Rec::with_ports();
    let state = state(&root, true, Some(Scripted::new(vec![text(GOOD_REPORT)]))).with_compose_provider(provider(&rec));
    let mut rx = state.events().subscribe();
    generate(&state, &mut rx).await;
    assert!(rec.compose_calls().is_empty());
    assert_eq!(fs::read_to_string(report_path(&root)).expect("relatório"), GOOD_REPORT);
}

#[test]
fn facts_list_test_run_of_every_done_task_redacted_and_tolerate_absence() {
    let secrets = SecretSet::new(vec![SECRET.to_string()]);
    let root = project("test-run-facts", false);
    let tasks = root.join(".sdd/specs").join(SPEC).join("tasks");
    let old_ev = "test_evidence: \"node --test: 3 passed\"\n";
    let with_run = |id: &str, tail: &str| {
        TASK_MD.replace("id: \"T01\"", &format!("id: \"{id}\"")).replace(
            old_ev,
            &format!("test_run:\n  command: \"cargo test\"\n  exit_code: 0\n  at: \"2026-10-16T10:00:00Z\"\n  summary: \"{tail}\"\n"),
        )
    };
    fs::write(tasks.join("T01.md"), with_run("T01", "resumo-um")).expect("t1");
    fs::write(tasks.join("T02.md"), with_run("T02", &format!("resumo-dois {SECRET}"))).expect("t2");
    let bare = TASK_MD.replace("id: \"T01\"", "id: \"T03\"").replace(old_ev, "");
    fs::write(tasks.join("T03.md"), bare).expect("t3");
    let facts = build_facts(&root, SPEC, &secrets).expect("fatos");
    assert!(facts.contains("- T01: `cargo test` exit=0 em 2026-10-16T10:00:00Z: resumo-um"), "{facts}");
    assert!(facts.contains("- T02: ") && facts.contains("resumo-dois"), "{facts}");
    assert!(facts.contains(&format!("- T03: {NO_EVIDENCE}")), "{facts}");
    assert!(!facts.contains(SECRET), "{facts}");
}

const ENV_SETTINGS: &str = "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"npm test\"\nenvironment: docker\n";

fn docker_project(tag: &str, compose: &str) -> PathBuf {
    let root = project(tag, false);
    fs::write(root.join(".sdd/settings.yaml"), ENV_SETTINGS).expect("settings");
    let spec = root.join(".sdd/specs").join(SPEC).join("spec.md");
    let raw = fs::read_to_string(&spec).expect("spec");
    fs::write(&spec, raw.replace("test_command: \"node --test\"\n", "run_command: \"npm start\"\n")).expect("spec");
    fs::create_dir_all(root.join(".nants")).expect("nants");
    fs::write(root.join(".nants/Dockerfile"), "# base\nFROM node:20.11.1-alpine AS build\nRUN true\nFROM --platform=linux/amd64 alpine:3.19\n").expect("df");
    fs::write(root.join(".nants/compose.yaml"), compose).expect("compose");
    root
}

#[test]
fn facts_environment_lists_images_services_ports_and_commands() {
    let secrets = SecretSet::new(vec![SECRET.to_string()]);
    let compose = "services:\n  workspace:\n    build: .\n    ports:\n      - \"3000:3000\"\n      - target: 80\n        published: 8080\n  db:\n    image: postgres:16.2\n";
    let root = docker_project("env-facts", compose);
    let facts = build_facts(&root, SPEC, &secrets).expect("fatos");
    assert!(facts.contains("Ambiente: docker"), "{facts}");
    assert!(facts.contains("Imagens base (.nants/Dockerfile): node:20.11.1-alpine, alpine:3.19"), "{facts}");
    assert!(facts.contains("workspace (portas 3000:3000, 8080:80)"), "{facts}");
    assert!(facts.contains("db (imagem postgres:16.2; sem portas publicadas)"), "{facts}");
    assert!(
        facts.contains("Executar fora do Nants: docker compose -f .nants/compose.yaml run --rm --service-ports workspace sh -lc 'npm start'"),
        "{facts}"
    );
    assert!(facts.contains("Testar fora do Nants: docker compose -f .nants/compose.yaml run --rm --service-ports workspace sh -lc 'npm test'"), "{facts}");
    assert!(facts.contains("Docker com Compose"), "{facts}");
}

#[test]
fn facts_environment_without_nants_is_host_and_has_no_docker_lines() {
    let secrets = SecretSet::new(vec![SECRET.to_string()]);
    let root = project("env-host", false);
    let facts = build_facts(&root, SPEC, &secrets).expect("fatos");
    assert!(facts.contains("Ambiente: host"), "{facts}");
    assert!(!facts.contains("Imagens base") && !facts.contains("Serviços e portas"), "{facts}");
    assert!(!facts.contains("fora do Nants"), "{facts}");
}

#[test]
fn facts_environment_tolerates_invalid_compose_and_redacts_secrets() {
    let secrets = SecretSet::new(vec![SECRET.to_string()]);
    let bad = docker_project("env-bad", "services: [unclosed\n  - : :");
    let facts = build_facts(&bad, SPEC, &secrets).expect("fatos");
    assert!(facts.contains("(.nants/compose.yaml): não foi possível ler"), "{facts}");
    let compose = format!("services:\n  workspace:\n    image: reg/{SECRET}:1\n    ports:\n      - \"{SECRET}:3000\"\n");
    let root = docker_project("env-secret", &compose);
    let facts = build_facts(&root, SPEC, &secrets).expect("fatos");
    assert!(facts.contains("workspace"), "{facts}");
    assert!(!facts.contains(SECRET), "{facts}");
}

/// Spec 086 T07: com `covers`, o servidor acrescenta "## Rastreabilidade" (sem o modelo), com o
/// commit de integração de cada tarefa, critério sem tarefa e redação de segredos.
#[tokio::test]
async fn report_appends_traceability_with_integration_commits() {
    let root = project("trace", true);
    let spec_dir = root.join(".sdd/specs").join(SPEC);
    let spec = format!(
        "{SPEC_MD}\n## Critérios de Aceitação Globais\n- [ ] Login funciona com a chave {SECRET}.\n- [ ] Logout limpa a sessão.\n"
    );
    fs::write(spec_dir.join("spec.md"), spec).expect("spec");
    let task = TASK_MD.replace("allowed_scope:", "covers: [\"G1\"]\nallowed_scope:");
    fs::write(spec_dir.join("tasks/T01.md"), task).expect("task");
    fs::create_dir_all(root.join("src")).expect("src");
    fs::write(root.join("src/a.txt"), "a\n").expect("a");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "feat(001-exemplo): T01 - Primeira tarefa"]);
    git(&root, &["branch", "-M", "main"]);
    let hash = git(&root, &["rev-parse", "--short=7", "HEAD"]).trim().to_string();

    let state = state(&root, true, Some(Scripted::new(vec![text(GOOD_REPORT)])));
    let mut rx = state.events().subscribe();
    let events = generate(&state, &mut rx).await;
    assert_eq!(events[0].0, "report_ready", "{events:?}");
    let stored = fs::read_to_string(report_path(&root)).expect("relatório");
    assert!(stored.starts_with(GOOD_REPORT.trim_end()), "{stored}");
    let section = stored.split("## Rastreabilidade\n").nth(1).expect("seção no fim");
    let lines: Vec<&str> = section.lines().collect();
    assert_eq!(lines.len(), 2, "{stored}");
    assert!(lines[0].starts_with("- G1: Login funciona com a chave "), "{stored}");
    assert!(lines[0].ends_with(&format!("(T01, commit {hash})")), "{stored}");
    assert_eq!(lines[1], "- G2: Logout limpa a sessão. (sem tarefa declarada)");
    assert!(!stored.contains(SECRET), "{stored}");
    assert!(stored.len() <= REPORT_MAX_BYTES);
}

/// Spec 086 T07: resposta no limite não empurra a rastreabilidade para fora do relatório.
#[tokio::test]
async fn traceability_survives_oversized_answer_within_limit() {
    let root = project("trace-big", false);
    let spec_dir = root.join(".sdd/specs").join(SPEC);
    fs::write(spec_dir.join("spec.md"), format!("{SPEC_MD}\n## Critérios de Aceitação Globais\n- [ ] Um.\n")).expect("spec");
    let task = TASK_MD.replace("allowed_scope:", "covers: [\"G1\"]\nallowed_scope:");
    fs::write(spec_dir.join("tasks/T01.md"), task).expect("task");
    let big = format!("# Relatório\n{}", "linha longa de texto\n".repeat(10_000));
    let state = state(&root, true, Some(Scripted::new(vec![text(&big)])));
    let mut rx = state.events().subscribe();
    generate(&state, &mut rx).await;
    let stored = fs::read_to_string(report_path(&root)).expect("relatório");
    assert!(stored.len() <= REPORT_MAX_BYTES, "{}", stored.len());
    assert!(
        stored.ends_with("## Rastreabilidade\n- G1: Um. (T01, sem commit de integração)\n"),
        "{}",
        &stored[stored.len() - 200..]
    );
}

/// Spec 086 T07 (F-01): segredo longo que passa do corte de 60 caracteres do critério é redigido
/// antes do corte; nenhum prefixo dele aparece no relatório.
#[tokio::test]
async fn traceability_redacts_long_secret_before_cutting_the_criterion() {
    const LONG: &str = "sk-proj-LONGO-0123456789abcdefghijklmnopqrstuvwxyz";
    let root = project("trace-long-secret", false);
    let spec_dir = root.join(".sdd/specs").join(SPEC);
    let spec = format!("{SPEC_MD}\n## Critérios de Aceitação Globais\n- [ ] O login aceita a chave de API {LONG} no header.\n");
    fs::write(spec_dir.join("spec.md"), spec).expect("spec");
    let task = TASK_MD.replace("allowed_scope:", "covers: [\"G1\"]\nallowed_scope:");
    fs::write(spec_dir.join("tasks/T01.md"), task).expect("task");
    let mut cfg = config(&root, true);
    cfg.connections.push(Connection {
        id: "longa".into(),
        name: "Chave longa".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: LONG.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
    });
    let state = AppState::new(root.join("config.toml"), cfg, PORT)
        .with_report_backend(Scripted::new(vec![text(GOOD_REPORT)]));
    let mut rx = state.events().subscribe();
    generate(&state, &mut rx).await;
    let stored = fs::read_to_string(report_path(&root)).expect("relatório");
    assert!(stored.contains("## Rastreabilidade\n- G1: O login aceita a chave de API ***"), "{stored}");
    for n in 8..=LONG.len() {
        assert!(!stored.contains(&LONG[..n]), "prefixo de {n} caracteres vazou: {stored}");
    }
}
