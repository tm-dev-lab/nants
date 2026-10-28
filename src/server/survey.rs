//! Levantamento inicial da arquitetura de um projeto existente e leitura do `ARCHITECTURE.md`.
//!
//! - `GET /api/projects/:id/architecture` → `{exists, content}` (redigido).
//! - `POST /api/projects/:id/architecture/survey` → 202; refaz o levantamento em segundo plano
//!   (409 se há turno de chat, execução ou relatório em andamento).
//! - Ao cadastrar um projeto existente (commits e arquivos, sem `ARCHITECTURE.md`), o servidor
//!   dispara o mesmo levantamento. O turno do Arquiteto é somente leitura; o servidor valida,
//!   grava e commita (`docs: levantamento inicial (Nants)`) com a base limpa, define o
//!   `test_command` se estiver vazio e publica um resumo na conversa.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map};
use tokio::sync::watch;

use super::approvals::{random_hex_id, ProjectApprover};
use super::architecture::{apply_with_message, Outcome};
use super::chat::{append_turns, architect_client, load_policy};
use super::error::ApiError;
use super::events::{validate_project_id, ApiEvent, BusActivitySink, EventType, SecretSet, MAX_MESSAGE_BYTES};
use super::projects::{find_project, scrub_text, write_checks_if_empty, write_test_command_if_empty};
use super::report::wait_shutdown;
use super::safe_fs;
use super::spec_result::has_real_git_dir;
use super::state::{AppState, ReserveError, REPORT_BUSY_CODE, SURVEY_RESERVATION};
use crate::agents::arch_memory::{parse_heading, read_raw, ARCH_FILE, SECTION_COMMANDS, SECTION_OVERVIEW, SECTION_STACK};
use crate::agents::architect::{truncate_bytes, ChatTurn, LlmArchitect, TurnRole};
use crate::agents::chat::ChatBackend;
use crate::config::Project;
use crate::harness::git::Git;
use crate::sdd::{check_test_command_shape, Check, Settings};

/// Prefixo dos commits do levantamento; o texto depois dele sai no idioma do projeto
/// (`commit.survey_first` e `commit.survey_redo`, spec 115 T04).
const SURVEY_COMMIT_PREFIX: &str = "docs: ";
const TEST_COMMAND_PREFIX: &str = "TEST_COMMAND:";
const CHECK_PREFIX: &str = "CHECK:";
const SETTINGS_REL: &str = ".sdd/settings.yaml";
/// Seções do resumo (visão geral, stack e comandos), reconhecidas pelo título em pt ou en (spec 125).
const SUMMARY_SECTIONS: [usize; 3] = [SECTION_OVERVIEW, SECTION_STACK, SECTION_COMMANDS];
const SUMMARY_SECTION_BYTES: usize = 900;

/// `GET /api/projects/:id/architecture`.
pub async fn get_architecture(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let project = find_project(&state, &id).await?;
    let secrets = state.secret_set().await;
    let root = PathBuf::from(&project.path);
    let content = tokio::task::spawn_blocking(move || read_raw(&root))
        .await
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))?;
    let body = match content {
        Some(text) => json!({"exists": true, "content": scrub_text(&secrets, &text)}),
        None => json!({"exists": false, "content": ""}),
    };
    Ok(Json(body).into_response())
}

/// `POST /api/projects/:id/architecture/survey`.
pub async fn post_survey(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    if architect_client(&state).await.is_err() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "architect_not_configured", Map::new()));
    }
    let busy = || ApiError::coded(StatusCode::CONFLICT, "survey_busy", Map::new());
    let (project, run_guard) = match state.reserve_run(&id, SURVEY_RESERVATION).await {
        Ok(ok) => ok,
        Err(ReserveError::NotFound) => {
            return Err(ApiError::coded(StatusCode::NOT_FOUND, "project_not_found", Map::new()))
        }
        Err(ReserveError::Busy) => return Err(busy()),
        Err(ReserveError::ReportInProgress) => {
            return Err(ApiError::coded(StatusCode::CONFLICT, REPORT_BUSY_CODE, Map::new()))
        }
    };
    let chat_guard = state.try_begin_chat(&id).ok_or_else(busy)?;
    let root = PathBuf::from(&project.path);
    if !has_real_git_dir(&root) {
        return Err(ApiError::coded(StatusCode::CONFLICT, "git_repo_unsupported", Map::new()));
    }
    let task_state = state.clone();
    tokio::spawn(async move {
        let _guards = (run_guard, chat_guard);
        run_survey(&task_state, &project).await;
    });
    Ok((StatusCode::ACCEPTED, Json(json!({"started": true}))).into_response())
}

/// `true` para repositório com commits e arquivos além do commit inicial vazio, sem `ARCHITECTURE.md`.
pub fn is_brownfield(root: &Path) -> bool {
    has_real_git_dir(root)
        && std::fs::symlink_metadata(root.join(ARCH_FILE)).is_err()
        && Git::new(root).head_file_count() > 0
}

/// Disparo automático ao cadastrar. Silencioso quando não é projeto existente, quando o Arquiteto
/// não está configurado ou quando o projeto está ocupado.
pub fn spawn_initial_survey(state: &AppState, project: Project) {
    let state = state.clone();
    tokio::spawn(async move {
        let root = PathBuf::from(&project.path);
        if !tokio::task::spawn_blocking(move || is_brownfield(&root)).await.unwrap_or(false) {
            return;
        }
        if architect_client(&state).await.is_err() {
            return;
        }
        let Ok((project, run_guard)) = state.reserve_run(&project.id, SURVEY_RESERVATION).await else {
            return;
        };
        let Ok((_, chat_guard)) = state.reserve_chat(&project.id).await else {
            return;
        };
        let _guards = (run_guard, chat_guard);
        run_survey(&state, &project).await;
    });
}

/// Roda o levantamento e publica o desfecho na conversa (resumo ou aviso).
async fn run_survey(state: &AppState, project: &Project) {
    let root = PathBuf::from(&project.path);
    let secrets = state.secret_set().await;
    let mut shutdown = state.shutdown_signal();
    let text = match survey(state, &project.id, &root, &secrets, &mut shutdown).await {
        Ok(summary) => summary,
        Err(code) => survey_failed_text(state.project_lang(&project.id).get(), code),
    };
    publish(state, &project.id, &root, &secrets, &text).await;
}

/// Aviso de falha do levantamento na conversa, no idioma do projeto (spec 113 T04).
fn survey_failed_text(lang: crate::i18n::Lang, code: &str) -> String {
    crate::i18n::tr(lang, "survey.failed", &[("code", code)])
}

async fn publish(state: &AppState, project_id: &str, root: &Path, secrets: &SecretSet, text: &str) {
    let text = truncate_bytes(&scrub_text(secrets, text), MAX_MESSAGE_BYTES);
    let turn_id = random_hex_id().ok();
    let turn = ChatTurn {
        role: TurnRole::Architect,
        text: text.clone(),
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        turn_id: turn_id.clone(),
        internal: false,
        attachments: Vec::new(),
        notice: None,
    };
    let owned = root.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || append_turns(&owned, &[turn])).await;
    state.events().publish(ApiEvent::new(
        EventType::AgentMessage,
        Some(project_id.to_string()),
        json!({"agent": "architect", "text": text, "turn_id": turn_id.unwrap_or_default()}),
    ));
}

async fn survey(
    state: &AppState,
    project_id: &str,
    root: &Path,
    secrets: &Arc<SecretSet>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<String, &'static str> {
    let git_root = root.to_path_buf();
    let (clean, history, had_file) = tokio::task::spawn_blocking(move || {
        let git = Git::new(&git_root);
        let history = git.log_subjects("HEAD", 15).unwrap_or_default();
        (
            git.is_clean_ignoring_sdd().unwrap_or(false),
            history,
            std::fs::symlink_metadata(git_root.join(ARCH_FILE)).is_ok(),
        )
    })
    .await
    .map_err(|_| "interno")?;
    if !clean {
        return Err("base_suja");
    }
    let settings_root = root.to_path_buf();
    let (test_command_empty, checks_empty) = tokio::task::spawn_blocking(move || {
        let settings = safe_fs::read_regular(&settings_root, SETTINGS_REL, 64 * 1024)
            .ok()
            .and_then(|t| Settings::parse(&t).ok());
        (
            settings.as_ref().is_some_and(|s| s.test_command.trim().is_empty()),
            settings.as_ref().is_some_and(|s| s.checks.is_empty()),
        )
    })
    .await
    .map_err(|_| "interno")?;
    let facts = format!(
        "Projeto existente (levantamento inicial).\ntest_command: {}\nchecks: {}\nÚltimos commits:\n{}\n",
        if test_command_empty { "(vazio)" } else { "(já definido; não emita TEST_COMMAND)" },
        if checks_empty { "(nenhum)" } else { "(já definidos; não emita CHECK)" },
        history
            .iter()
            .map(|h| format!("- {}", scrub_text(secrets, h)))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let (model, effort, client) = architect_client(state).await.map_err(|_| "nao_configurado")?;
    let backend: Arc<dyn ChatBackend> = match state.report_backend() {
        Some(backend) => backend,
        None => Arc::new(client),
    };
    let policy = load_policy(root.to_path_buf()).await;
    let approver = Arc::new(ProjectApprover::new(
        project_id,
        root.to_path_buf(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::clone(secrets),
    )
    .with_lang(state.project_lang(project_id)));
    let activity = Arc::new(BusActivitySink::new(state.events().clone(), project_id, Arc::clone(secrets)));
    let architect =
        LlmArchitect::new(backend, model, Some(effort), policy, approver).with_lang(state.project_lang(project_id)).with_activity(activity, None, None).with_usage(super::usage::FileUsageSink::redacting(root.to_path_buf(), project_id, Arc::clone(secrets)));
    let turn = architect.survey_architecture(root, &facts);
    let answer = tokio::select! {
        biased;
        _ = wait_shutdown(shutdown) => return Err("shutdown"),
        joined = tokio::time::timeout(state.report_timeout(), turn) => match joined {
            Err(_) => return Err("timeout"),
            Ok(Err(_)) => return Err("falha_do_provedor"),
            Ok(Ok(text)) => text,
        },
    };

    let (candidate, markdown) = split_test_command(&answer);
    let (candidate_checks, markdown) = split_checks(markdown);
    let key = if had_file { "commit.survey_redo" } else { "commit.survey_first" };
    let msg = crate::i18n::commit_subject(state.project_lang(project_id).get(), SURVEY_COMMIT_PREFIX, key);
    let apply_root = root.to_path_buf();
    let apply_secrets = Arc::clone(secrets);
    let md = markdown.to_string();
    let lang = state.project_lang(project_id).get();
    let outcome = tokio::task::spawn_blocking(move || apply_with_message(lang, &apply_root, &msg, &md, &apply_secrets))
        .await
        .map_err(|_| "interno")?
        .map_err(|s| s.0)?;

    let mut note = String::new();
    if let (true, Some(cmd)) = (test_command_empty, candidate) {
        if let Ok(cmd) = check_test_command_shape(cmd) {
            let cmd = scrub_text(secrets, &cmd);
            let (r, s, c) = (root.to_path_buf(), Arc::clone(secrets), cmd.clone());
            let written = tokio::task::spawn_blocking(move || write_test_command_if_empty(&r, &c, &s)).await;
            if matches!(written, Ok(Ok(true))) {
                note = format!("\n\nDefini o comando de testes: `{cmd}`.");
            }
        }
    }
    if checks_empty && !candidate_checks.is_empty() {
        let names = candidate_checks.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(", ");
        let (r, s, c) = (root.to_path_buf(), Arc::clone(secrets), candidate_checks);
        let written = tokio::task::spawn_blocking(move || write_checks_if_empty(&r, &c, &s)).await;
        if matches!(written, Ok(Ok(true))) {
            note.push_str(&format!("\n\nDefini as checagens: {}.", names));
        }
    }
    let content = read_raw(root).unwrap_or_default();
    let head = match outcome {
        Outcome::Committed => "Levantamento concluído; gravei o ARCHITECTURE.md na raiz do projeto.",
        Outcome::Unchanged => "Levantamento refeito; o ARCHITECTURE.md já estava atualizado.",
    };
    Ok(format!("{head}\n\n{}{note}", summarize(&content)))
}

/// Separa a linha opcional `TEST_COMMAND: ...` do início da resposta.
fn split_test_command(answer: &str) -> (Option<&str>, &str) {
    let trimmed = answer.trim_start();
    if let Some(rest) = trimmed.strip_prefix(TEST_COMMAND_PREFIX) {
        let (line, tail) = rest.split_once('\n').unwrap_or((rest, ""));
        let cmd = line.trim().trim_matches('`').trim();
        return ((!cmd.is_empty()).then_some(cmd), tail);
    }
    (None, answer)
}

/// Separa as linhas opcionais `CHECK: nome | blocking|advisory | comando` do início da resposta.
/// Linhas malformadas são ignoradas; a lista final ainda passa por `validate_checks`.
fn split_checks(answer: &str) -> (Vec<Check>, &str) {
    let mut checks = Vec::new();
    let mut rest = answer.trim_start_matches(['\n', '\r']);
    while let Some(after) = rest.trim_start().strip_prefix(CHECK_PREFIX) {
        let (line, tail) = after.split_once('\n').unwrap_or((after, ""));
        rest = tail;
        let mut parts = line.splitn(3, '|').map(str::trim);
        if let (Some(name), Some(kind), Some(command)) = (parts.next(), parts.next(), parts.next()) {
            let command = command.trim_matches('`').trim();
            if matches!(kind, "blocking" | "advisory") && !command.is_empty() {
                checks.push(Check { name: name.to_string(), command: command.to_string(), blocking: kind == "blocking" });
            }
        }
    }
    (checks, rest)
}

/// Resumo (visão geral, stack e comandos) extraído do arquivo.
fn summarize(content: &str) -> String {
    let mut out = Vec::new();
    for section in SUMMARY_SECTIONS {
        let Some(start) = content.lines().position(|l| parse_heading(l).is_some_and(|(i, _)| i == section)) else {
            continue;
        };
        let header = content.lines().nth(start).unwrap_or_default().trim_end();
        let body: Vec<&str> = content
            .lines()
            .skip(start + 1)
            .take_while(|l| !l.starts_with("## "))
            .collect();
        let body = truncate_bytes(body.join("\n").trim(), SUMMARY_SECTION_BYTES);
        if !body.is_empty() {
            out.push(format!("{header}\n{body}"));
        }
    }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::time::Duration;

    use axum::body::{to_bytes, Body};
    use axum::http::{header, Method, Request};
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::agents::arch_memory::section_headings;
    use crate::agents::architect::testing::{text, Scripted};
    use crate::config::{AgentConfig, Config, Connection, Effort, WireApi};
    use crate::server::events::ApiEvent;
    use crate::server::router;

    const PROJECT: &str = "proj-survey";

    fn git(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=T", "-c", "user.email=t@e.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(root)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Repositório com arquivos commitados (ou só o commit inicial vazio) e settings locais.
    fn repo(tag: &str, files: bool) -> PathBuf {
        let root = std::env::temp_dir().join(format!("nants-survey-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".sdd/specs")).expect("sdd");
        git(&root, &["init", "-q", "-b", "main"]);
        if files {
            fs::write(root.join("main.rs"), "fn main() {}\n").expect("w");
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "-q", "-m", "feat: início"]);
        } else {
            git(&root, &["commit", "-q", "--allow-empty", "-m", "chore: inicializa"]);
        }
        let settings = "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"\"\n";
        fs::write(root.join(".sdd/settings.yaml"), settings).expect("settings");
        fs::write(root.join(".git/info/exclude"), ".sdd/\n").expect("exclude");
        root
    }

    fn state(root: &Path, backend: Option<Arc<dyn ChatBackend>>) -> AppState {
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "c".into(),
            name: "c".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: "sk-x".into(),
            wire_api: WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        cfg.agents.architect = AgentConfig { connection_id: "c".into(), model: "m".into(), effort: Effort::Low, max_steps: None };
        cfg.projects.push(Project {
            id: PROJECT.into(),
            name: "P".into(),
            path: root.display().to_string(),
            created_at: "2026-10-15T00:00:00Z".into(),
        });
        let state = AppState::new(root.join("config.toml"), cfg, 3000);
        match backend {
            Some(b) => state.with_report_backend(b),
            None => state,
        }
    }

    fn doc() -> String {
        let mut s = String::from("# P\n");
        for sec in section_headings(crate::i18n::Lang::PtBr) {
            s.push_str(&format!("\n{sec}\nconteúdo {sec}\n"));
        }
        s
    }

    async fn call(state: &AppState, method: Method, uri: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:3000")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("req");
        let resp = router(state.clone()).oneshot(req).await.expect("resp");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn wait_message(rx: &mut tokio::sync::broadcast::Receiver<ApiEvent>) -> String {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("tempo")
                .expect("evento");
            if event.event_type() == "agent_message" {
                return event.payload["text"].as_str().unwrap_or_default().to_string();
            }
        }
    }

    fn survey_uri() -> String {
        format!("/api/projects/{PROJECT}/architecture/survey")
    }

    #[test]
    fn brownfield_only_with_files_and_no_architecture() {
        assert!(is_brownfield(&repo("bf-yes", true)));
        assert!(!is_brownfield(&repo("bf-empty", false)));
        let r = repo("bf-has", true);
        fs::write(r.join(ARCH_FILE), "x").expect("w");
        assert!(!is_brownfield(&r));
    }

    #[tokio::test]
    async fn survey_commits_file_sets_test_command_and_posts_summary() {
        let root = repo("run", true);
        let reply = format!("TEST_COMMAND: cargo test\n{}", doc());
        let state = state(&root, Some(Scripted::new(vec![text(&reply)])));
        let mut rx = state.events().subscribe();
        let (status, _) = call(&state, Method::POST, &survey_uri()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let msg = wait_message(&mut rx).await;
        assert!(msg.contains("Levantamento concluído") && msg.contains("cargo test"), "{msg}");
        assert_eq!(git(&root, &["log", "-1", "--format=%s"]).trim(), "docs: levantamento inicial (Nants)");
        assert!(git(&root, &["status", "--porcelain"]).is_empty());
        let settings = fs::read_to_string(root.join(".sdd/settings.yaml")).expect("s");
        assert!(settings.contains("cargo test"), "{settings}");
        let (status, body) = call(&state, Method::GET, &format!("/api/projects/{PROJECT}/architecture")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["exists"], true);
        assert!(body["content"].as_str().unwrap_or_default().contains("## Stack"));
    }

    #[test]
    fn write_if_empty_preserves_human_command() {
        let root = repo("keep", true);
        let secrets = SecretSet::new(Vec::new());
        // O humano define o comando durante o turno: o valor dele fica.
        let human = "node_modules/.bin/vitest";
        write_test_command_if_empty(&root, human, &secrets).expect("w");
        let again = write_test_command_if_empty(&root, "cargo test", &secrets).expect("w2");
        assert!(!again);
        let settings = fs::read_to_string(root.join(".sdd/settings.yaml")).expect("s");
        assert!(settings.contains(human) && !settings.contains("cargo test"), "{settings}");
    }

    #[tokio::test]
    async fn dirty_base_does_not_commit_and_warns() {
        let root = repo("dirty", true);
        fs::write(root.join("sujo.txt"), "x").expect("w");
        let state = state(&root, Some(Scripted::new(vec![text(&doc())])));
        let mut rx = state.events().subscribe();
        let (status, _) = call(&state, Method::POST, &survey_uri()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let msg = wait_message(&mut rx).await;
        assert!(msg.contains("base_suja"), "{msg}");
        assert!(!root.join(ARCH_FILE).exists());
    }

    /// Spec 113 T04: o aviso de falha sai no idioma do projeto e aponta o caminho real (o
    /// Arquiteto cria o ARCHITECTURE.md ao planejar), sem citar botão ou aba que não existem.
    #[test]
    fn survey_failure_text_in_both_languages() {
        use crate::i18n::Lang;
        let pt = survey_failed_text(Lang::PtBr, "timeout");
        assert!(pt.starts_with("Não consegui concluir o levantamento da arquitetura (timeout)."), "{pt}");
        assert!(pt.contains("Arquiteto na conversa") && pt.contains("cria o ARCHITECTURE.md junto com o plano"), "{pt}");
        let en = survey_failed_text(Lang::En, "timeout");
        assert!(en.starts_with("I could not finish the architecture survey (timeout)."), "{en}");
        assert!(en.contains("Architect in the conversation") && en.contains("creates ARCHITECTURE.md along with the plan"), "{en}");
        for text in [&pt, &en] {
            for gone in ["Refazer", "levantamento\" na aba", "Redo", "tab", "botão", "button"] {
                assert!(!text.contains(gone), "caminho inexistente ({gone}): {text}");
            }
        }
        // O caminho citado existe: o prompt do Arquiteto manda criar o ARCHITECTURE.md ao planejar.
        assert!(crate::agents::prompts::architect_system().contains("se NÃO existir, escreva o inicial junto com o plano"));
    }

    #[tokio::test]
    async fn get_reports_missing_file_and_survey_is_409_when_busy() {
        let root = repo("busy", true);
        let state = state(&root, Some(Scripted::new(vec![])));
        let (status, body) = call(&state, Method::GET, &format!("/api/projects/{PROJECT}/architecture")).await;
        assert_eq!((status, body["exists"].clone()), (StatusCode::OK, Value::Bool(false)));
        let chat = state.try_begin_chat(PROJECT).expect("chat");
        let (status, _) = call(&state, Method::POST, &survey_uri()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        drop(chat);
        let _run = state.reserve_run(PROJECT, "001-x").await.expect("run");
        let (status, _) = call(&state, Method::POST, &survey_uri()).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn auto_survey_only_for_existing_projects() {
        let root = repo("auto-yes", true);
        let st = state(&root, Some(Scripted::new(vec![text(&doc())])));
        let mut rx = st.events().subscribe();
        let project = st.config().read().await.projects[0].clone();
        spawn_initial_survey(&st, project);
        let msg = wait_message(&mut rx).await;
        assert!(msg.contains("Levantamento concluído"), "{msg}");

        let empty = repo("auto-no", false);
        let st = state(&empty, Some(Scripted::new(vec![text(&doc())])));
        let mut rx = st.events().subscribe();
        let project = st.config().read().await.projects[0].clone();
        spawn_initial_survey(&st, project);
        let waited = tokio::time::timeout(Duration::from_millis(400), wait_message(&mut rx)).await;
        assert!(waited.is_err(), "repositório novo não deve disparar");
        assert!(!empty.join(ARCH_FILE).exists());
    }

    #[test]
    fn splits_optional_test_command_line() {
        let (c, md) = split_test_command("TEST_COMMAND: `cargo test`\n# T\n");
        assert_eq!(c, Some("cargo test"));
        assert_eq!(md, "# T\n");
        let (c, md) = split_test_command("# T\n");
        assert_eq!((c, md), (None, "# T\n"));
    }

    #[test]
    fn summary_takes_overview_stack_and_commands() {
        let doc = "# P\n\n## Visão geral\nApp X\n\n## Stack\nRust\n\n## Estrutura\nsrc\n\n## Comandos (a)\ncargo test\n";
        let s = summarize(doc);
        assert!(s.contains("App X") && s.contains("Rust") && s.contains("cargo test"));
        assert!(!s.contains("src\n"));
    }

    /// Spec 125: o resumo acha as seções pelo título em en também, e mostra o título do arquivo.
    #[test]
    fn summary_reads_english_headings() {
        let doc = "# P\n\n## Overview\nApp X\n\n## Stack\nRust\n\n## Structure\nsrc\n\n## Commands (install, test, run)\ncargo test\n";
        let s = summarize(doc);
        assert!(s.contains("## Overview\nApp X") && s.contains("## Stack\nRust"), "{s}");
        assert!(s.contains("## Commands (install, test, run)\ncargo test"), "{s}");
        assert!(!s.contains("src"), "{s}");
    }

    #[test]
    fn splits_check_lines() {
        let (c, md) = split_checks("CHECK: lint | advisory | npm run lint\nCHECK: build | blocking | `npm run build`\nCHECK: ruim\n# T\n");
        assert_eq!(c.len(), 2);
        assert!(!c[0].blocking && c[1].blocking);
        assert_eq!(c[1].command, "npm run build");
        assert_eq!(md, "# T\n");
        let (c, md) = split_checks("# T\n");
        assert!(c.is_empty());
        assert_eq!(md, "# T\n");
    }
}
