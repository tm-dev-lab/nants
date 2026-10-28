//! Teste ponta a ponta em processo (spec 053/T04): rotas HTTP -> chat do Arquiteto -> aprovar ->
//! runner real (Desenvolvedor, testes no host, Revisor, squash na base) -> relatório ->
//! ARCHITECTURE.md -> `finalization_done`. Um único backend de LLM roteirizado responde por papel
//! (identificado pelo prompt de sistema). Sem rede, sem docker, repositório git temporário.

use std::collections::VecDeque;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{self, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::json;
use tokio::sync::broadcast;
use tower::ServiceExt;

use crate::agents::architect::testing::{calls, text, Reply};
use crate::agents::chat::ChatBackend;
use crate::agents::llm_agents::FINAL_CHECK_INTRO;
use crate::agents::llm_client::{ChatRequest, Role};
use crate::agents::prompts::{architect_system, architecture_update_system, plan_review_system, report_system, reviewer_system};
use crate::config::{AgentConfig, Config, Connection, Effort, Project, WireApi};
use crate::sdd::{Spec, SpecStatus, Task, TaskStatus};
use crate::server::events::ApiEvent;
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";
const PROJECT: &str = "proj";
const SPEC: &str = "e2e-demo";
const WAIT: Duration = Duration::from_secs(20);
const TEST_COMMAND: &str = "test -f src/t01.txt";

const SPEC_MD: &str = "---\nid: \"e2e-demo\"\ntitle: \"Demo ponta a ponta\"\nstatus: \"AWAITING_APPROVAL\"\nbase_branch: \"main\"\nenvironment: \"docker\"\ntest_command: \"test -f src/t01.txt\"\ncreated_at: \"2026-10-15T00:00:00Z\"\n---\n## Contexto e Problema\nEntregar dois arquivos.\n\n## Abordagem Técnica\nArquivos de texto em src.\n\n## Critérios de Aceitação Globais\n- [ ] src/t01.txt e src/t02.txt existem.\n";

fn task_md(id: &str, title: &str, deps: &str) -> String {
    format!(
        "---\nid: \"{id}\"\nspec_id: \"{SPEC}\"\ntitle: \"{title}\"\nstatus: \"TODO\"\ndepends_on: [{deps}]\nbranch: \"\"\nattempts: 0\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope:\n  - \"src/**\"\n---\n## Objetivo\nCriar o arquivo da tarefa {id} ({title}).\n\n## Critérios de Aceitação\n- [ ] O arquivo existe.\n"
    )
}

const ARCH_MD: &str = "# Demo\n\n## Visão geral\nDemo.\n\n## Stack\nShell.\n\n## Estrutura\nsrc/.\n\n## Comandos\ntest -f src/t01.txt\n\n## Padrões e convenções\nNenhum.\n\n## Decisões\n- 2026-10-15: demo.\n\n## Restrições\nNenhuma.\n";

/// Backend roteirizado: responde conforme o papel (1ª mensagem de sistema) e registra as chamadas.
struct RoleLlm {
    /// Veredictos do Revisor, na ordem (`true` = APPROVED); esgotados, aprova.
    verdicts: Mutex<VecDeque<bool>>,
    log: Mutex<Vec<&'static str>>,
    review_calls: AtomicUsize,
    /// Cenário spec 055: o Desenvolvedor da T01 pergunta ao Arquiteto e ganha escopo para `docs/**`.
    ask: bool,
}

impl RoleLlm {
    fn new(verdicts: &[bool]) -> Arc<Self> {
        Arc::new(Self {
            verdicts: Mutex::new(verdicts.iter().copied().collect()),
            log: Mutex::new(Vec::new()),
            review_calls: AtomicUsize::new(0),
            ask: false,
        })
    }

    fn asking() -> Arc<Self> {
        Arc::new(Self {
            verdicts: Mutex::new(VecDeque::new()),
            log: Mutex::new(Vec::new()),
            review_calls: AtomicUsize::new(0),
            ask: true,
        })
    }

    fn count(&self, role: &str) -> usize {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).iter().filter(|r| **r == role).count()
    }

    fn answer(&self, req: &ChatRequest) -> (&'static str, Reply) {
        let system = req.messages.first().map(|m| m.content.as_str()).unwrap_or_default();
        let after_tool = req.messages.last().is_some_and(|m| m.role == Role::Tool);
        if system.starts_with(&architect_system()) {
            if after_tool {
                return ("architect", text("Plano gravado: 2 tarefas."));
            }
            let w = |path: &str, content: String| json!({"path": path, "content": content}).to_string();
            let a = w(&format!(".sdd/specs/{SPEC}/spec.md"), SPEC_MD.to_string());
            let b = w(&format!(".sdd/specs/{SPEC}/tasks/T01.md"), task_md("T01", "Entrega alfa", ""));
            let c = w(&format!(".sdd/specs/{SPEC}/tasks/T02.md"), task_md("T02", "Entrega beta", "\"T01\""));
            return (
                "architect",
                calls(&[("w1", "write_sdd_artifacts", &a), ("w2", "write_sdd_artifacts", &b), ("w3", "write_sdd_artifacts", &c)]),
            );
        }
        if system.starts_with(&report_system()) {
            return ("report", text("# Relatório\n\n## Como executar\nNada a executar: são arquivos de texto em `src/`."));
        }
        if system.starts_with(&architecture_update_system()) {
            return ("architecture", text(ARCH_MD));
        }
        if system.starts_with(&plan_review_system()) {
            return ("plan_review", text(r#"{"issues": []}"#));
        }
        if system.starts_with(&reviewer_system()) {
            self.review_calls.fetch_add(1, Ordering::SeqCst);
            let approved = self.verdicts.lock().unwrap_or_else(PoisonError::into_inner).pop_front().unwrap_or(true);
            let body = if approved {
                json!({"verdict": "APPROVED", "summary_message": "ok", "criteria": [], "findings": []})
            } else {
                json!({"verdict": "CHANGES_REQUESTED", "summary_message": "ajustar", "criteria": [], "findings": [
                    {"id": "F-01", "severity": "MEDIUM", "location": "src/t01.txt:1", "description": "conteúdo vazio", "recommendation": "escrever o conteúdo"}]})
            };
            return ("reviewer", text(&body.to_string()));
        }
        if system.starts_with(crate::server::advisor::SYSTEM) {
            return ("advisor", text(r#"{"answer":"Pode gravar a nota em docs/.","scope_add":["docs/**"]}"#));
        }
        if system.starts_with("Você é o Desenvolvedor") {
            let all_text: String = req.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n");
            if self.ask && !all_text.contains("Entrega beta") {
                let n = req.messages.iter().filter(|m| m.role == Role::Tool).count();
                return ("developer", match n {
                    0 => calls(&[("q1", "ask_architect", &json!({"question": "Posso gravar docs/nota.md?"}).to_string())]),
                    1 => calls(&[("d0", "write_file", &json!({"path": "docs/nota.md", "content": "nota\n"}).to_string())]),
                    2 => calls(&[("d1", "write_file", &json!({"path": "src/t01.txt", "content": "T01 entregue\n"}).to_string())]),
                    _ => text("Arquivos criados.\n\nCritérios\n- O arquivo existe: src/t01.txt"),
                });
            }
            if after_tool || req.messages.last().is_some_and(|m| m.content.starts_with(FINAL_CHECK_INTRO)) {
                return ("developer", text("Arquivo criado.\n\nCritérios\n- O arquivo existe: src/*.txt"));
            }
            let all: String = req.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n");
            let (file, id) = if all.contains("Correção da verificação final") { ("src/t03.txt", "T03") } else if all.contains("Entrega beta") { ("src/t02.txt", "T02") } else { ("src/t01.txt", "T01") };
            let args = json!({"path": file, "content": format!("{id} entregue\n")}).to_string();
            return ("developer", calls(&[("d1", "write_file", &args)]));
        }
        ("unknown", Err(crate::agents::LlmError::Transport("papel desconhecido".into())))
    }
}

impl ChatBackend for RoleLlm {
    fn complete<'a>(&'a self, req: &'a ChatRequest) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        let (role, reply) = self.answer(req);
        self.log.lock().unwrap_or_else(PoisonError::into_inner).push(role);
        Box::pin(async move { reply })
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=Teste", "-c", "user.email=teste@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn setup(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nants-e2e-{}-{tag}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    let root = dir.join("repo");
    fs::create_dir_all(root.join(".sdd")).expect("sdd");
    // Todo projeto é docker (spec 074): ambiente aprovado e registrado no settings.
    fs::create_dir_all(root.join(".nants")).expect("nants");
    fs::write(root.join(".nants/compose.yaml"), "services:\n  workspace:\n    image: node:20\n").expect("compose");
    let hash = crate::sdd::environment::validate_and_hash(&root).expect("hash do ambiente");
    fs::write(
        root.join(".sdd/settings.yaml"),
        format!("base_branch: main\nmerge_strategy: squash\ntest_command: \"{TEST_COMMAND}\"\nenvironment: docker\nenvironment_hash: {hash}\n"),
    )
    .expect("settings");
    fs::write(root.join("README.md"), "demo\n").expect("readme");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "README.md", ".sdd/settings.yaml", ".nants"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    root
}

fn config_for(root: &Path) -> Config {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "c".into(),
        name: "Conexão".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: "sk-e2e".into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
    let agent = |model: &str| AgentConfig { connection_id: "c".into(), model: model.into(), effort: Effort::Low, max_steps: None };
    cfg.agents.architect = agent("arq");
    cfg.agents.developer = agent("dev");
    cfg.agents.reviewer = agent("rev");
    cfg.projects.push(Project {
        id: PROJECT.into(),
        name: "Projeto".into(),
        path: root.display().to_string(),
        created_at: "2026-10-15T00:00:00Z".into(),
    });
    cfg
}

/// Docker falso (sem daemon): `run ... -lc <cmd>` executa o comando num `sh` local no diretório da
/// worktree (o que o container faria, já que a worktree é montada em /work); os demais subcomandos
/// (down, build, up) respondem sucesso.
struct LocalShellCompose;

impl crate::harness::compose::ComposeRunner for LocalShellCompose {
    fn run(&self, call: &crate::harness::compose::ComposeCall) -> Result<crate::harness::compose::ComposeOutput, String> {
        use crate::harness::compose::ComposeOutput;
        let Some(at) = call.args.iter().position(|a| a == "-lc") else {
            return Ok(ComposeOutput { code: Some(0), output: String::new(), timed_out: false });
        };
        let command = call.args.get(at + 1).ok_or("comando ausente")?;
        let out = Command::new("sh")
            .args(["-lc", command])
            .current_dir(&call.cwd)
            .output()
            .map_err(|e| e.to_string())?;
        let mut output = String::from_utf8_lossy(&out.stdout).to_string();
        output.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(ComposeOutput { code: out.status.code(), output, timed_out: false })
    }
}

fn state_for(root: &Path, llm: &Arc<RoleLlm>) -> AppState {
    let overrides = root.with_file_name("compose-overrides");
    AppState::new(root.with_file_name("config.toml"), config_for(root), 3000)
        .with_compose_provider(crate::server::envs::ComposeProvider::with_runner(Arc::new(LocalShellCompose), overrides))
        .with_llm_backend(llm.clone())
        .with_shutdown_grace(Duration::from_millis(200))
}

async fn post(state: &AppState, uri: &str, body: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("req");
    let resp = router(state.clone()).oneshot(req).await.expect("resposta");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("corpo");
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// Consome eventos até `finalization_done`; devolve todos.
async fn until_finalized(rx: &mut broadcast::Receiver<ApiEvent>) -> Vec<ApiEvent> {
    let mut out = Vec::new();
    loop {
        let ev = tokio::time::timeout(WAIT, rx.recv()).await.expect("evento no prazo").expect("canal");
        let done = ev.event_type() == "finalization_done";
        out.push(ev);
        if done {
            return out;
        }
    }
}

fn types(events: &[ApiEvent]) -> Vec<&str> {
    events.iter().map(|e| e.event_type()).collect()
}

fn task(root: &Path, id: &str) -> Task {
    let p = root.join(".sdd/specs").join(SPEC).join("tasks").join(format!("{id}.md"));
    Task::parse(&fs::read_to_string(p).expect("tarefa")).expect("parse")
}

fn spec_status(root: &Path) -> SpecStatus {
    Spec::parse(&fs::read_to_string(root.join(".sdd/specs").join(SPEC).join("spec.md")).expect("spec"))
        .expect("parse")
        .frontmatter
        .status
}

/// Planeja pelo chat, aprova e espera a finalização. Devolve os eventos desde a aprovação.
async fn plan_and_run(state: &AppState, root: &Path) -> Vec<ApiEvent> {
    let (code, body) = post(state, &format!("/api/projects/{PROJECT}/chat"), r#"{"message":"planeje a demo"}"#).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert!(root.join(".sdd/specs").join(SPEC).join("tasks/T02.md").exists(), "plano gravado pelo Arquiteto");
    assert_eq!(spec_status(root), SpecStatus::AwaitingApproval);
    // A aprovação espera a revisão do plano disparada pelo turno (spec 128 T04), como a tela.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while super::plan_review::review_running(root, SPEC) {
        assert!(std::time::Instant::now() < deadline, "revisão do plano não terminou");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut rx = state.events().subscribe();
    let (code, body) = post(state, &format!("/api/projects/{PROJECT}/specs/{SPEC}/approve"), "{}").await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    until_finalized(&mut rx).await
}

fn feat_commits(root: &Path) -> Vec<String> {
    let prefix = format!("feat({SPEC}): T");
    git(root, &["log", "main", "--format=%s"]).lines().filter(|l| l.starts_with(&prefix)).map(String::from).collect()
}

fn assert_finished(root: &Path, events: &[ApiEvent]) {
    let t = types(events);
    for expected in ["project_status", "report_ready", "finalization_done"] {
        assert!(t.contains(&expected), "faltou {expected}: {t:?}");
    }
    assert!(!t.contains(&"report_failed"), "{t:?}");
    assert!(
        events.iter().any(|e| e.event_type() == "project_status" && e.payload["status"] == "integrated"),
        "project_status integrated: {t:?}"
    );
    assert!(t.iter().position(|x| *x == "report_ready") < t.iter().position(|x| *x == "finalization_done"));
    assert_eq!(spec_status(root), SpecStatus::Completed);
    for id in ["T01", "T02"] {
        assert_eq!(task(root, id).frontmatter.status, TaskStatus::Done, "{id}");
    }
    let commits = feat_commits(root);
    assert_eq!(commits.len(), 2, "{commits:?}");
    assert!(commits.iter().any(|c| c.starts_with(&format!("feat({SPEC}): T01 - "))));
    assert!(commits.iter().any(|c| c.starts_with(&format!("feat({SPEC}): T02 - "))));
    assert!(root.join("src/t01.txt").exists() && root.join("src/t02.txt").exists());
    assert!(root.join(".sdd/specs").join(SPEC).join("report.md").exists());
    assert!(git(root, &["log", "main", "--format=%s"]).contains("docs(e2e-demo): atualiza ARCHITECTURE.md"));
    assert!(root.join("ARCHITECTURE.md").exists());
}

#[tokio::test]
async fn happy_path_plans_approves_and_finalizes() {
    let root = setup("happy");
    let llm = RoleLlm::new(&[]);
    let state = state_for(&root, &llm);
    let events = plan_and_run(&state, &root).await;
    assert_finished(&root, &events);
    assert_eq!(task(&root, "T01").frontmatter.attempts, 0);
    assert_eq!(task(&root, "T02").frontmatter.attempts, 0);
    assert_eq!((llm.count("reviewer"), llm.count("report"), llm.count("architecture")), (3, 1, 1));
    assert!(llm.count("developer") > 0);
    assert_eq!(llm.count("unknown"), 0);
    assert!(state.runs().active(PROJECT).is_none(), "vaga liberada");
}

#[tokio::test]
async fn one_rejection_then_approval_counts_one_attempt() {
    let root = setup("reject");
    let llm = RoleLlm::new(&[false, true, true]);
    let state = state_for(&root, &llm);
    let events = plan_and_run(&state, &root).await;
    assert_finished(&root, &events);
    assert_eq!(task(&root, "T01").frontmatter.attempts, 1);
    assert_eq!(task(&root, "T02").frontmatter.attempts, 0);
    assert_eq!(llm.review_calls.load(Ordering::SeqCst), 4);
    assert_eq!(llm.count("unknown"), 0);
}

/// Reinício no meio: a T01 já está integrada na base (commit feito) mas o disco diz INTEGRATING,
/// a T02 ainda é TODO e sobrou uma pasta de worktree órfã. Continuar conclui tudo.
#[tokio::test]
async fn resume_after_crash_between_commit_and_done() {
    let root = setup("resume");
    let llm = RoleLlm::new(&[]);
    let state = state_for(&root, &llm);
    plan_and_run(&state, &root).await;
    // Volta a base ao commit da T01 e simula a queda antes de gravar DONE.
    let t01 = git(&root, &["log", "main", "--format=%H %s"])
        .lines()
        .find(|l| l.contains(&format!("feat({SPEC}): T01 - ")))
        .and_then(|l| l.split(' ').next().map(String::from))
        .expect("commit da T01");
    git(&root, &["reset", "-q", "--hard", &t01]);
    git(&root, &["worktree", "prune"]);
    let tasks = root.join(".sdd/specs").join(SPEC).join("tasks");
    let mut t = task(&root, "T01");
    t.frontmatter.status = TaskStatus::Integrating;
    fs::write(tasks.join("T01.md"), t.render().expect("render")).expect("T01");
    let mut t = task(&root, "T02");
    t.frontmatter.status = TaskStatus::Todo;
    t.frontmatter.attempts = 0;
    fs::write(tasks.join("T02.md"), t.render().expect("render")).expect("T02");
    let _ = Command::new("git").args(["branch", "-D", &format!("sdd/{SPEC}/T02")]).current_dir(&root).output();
    let spec_path = root.join(".sdd/specs").join(SPEC).join("spec.md");
    let spec = fs::read_to_string(&spec_path).expect("spec").replace("COMPLETED", "IN_PROGRESS");
    fs::write(&spec_path, spec).expect("spec");
    let _ = fs::remove_file(root.join(".sdd/specs").join(SPEC).join("report.md"));
    let orphan = root.join(".sdd/worktrees").join(SPEC).join("T01");
    fs::create_dir_all(&orphan).expect("órfã");
    fs::write(orphan.join("sobra.txt"), "x").expect("sobra");
    assert!(!root.join("src/t02.txt").exists(), "base voltou ao commit da T01");

    let mut rx = state.events().subscribe();
    let (code, body) = post(&state, &format!("/api/projects/{PROJECT}/specs/{SPEC}/resume"), "{}").await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    let events = until_finalized(&mut rx).await;
    assert_finished(&root, &events);
    assert!(!orphan.exists(), "worktree órfã limpa");
}

/// Spec 055: o Desenvolvedor pergunta ao Arquiteto, ganha escopo em `docs/**` e escreve ali na
/// mesma execução; pergunta/resposta ficam na conversa e na tarefa.
#[tokio::test]
async fn developer_asks_architect_and_gets_scope_in_same_run() {
    let root = setup("ask");
    let llm = RoleLlm::asking();
    let state = state_for(&root, &llm);
    let events = plan_and_run(&state, &root).await;
    assert_finished(&root, &events);
    assert_eq!(llm.count("advisor"), 1);
    assert_eq!(llm.count("unknown"), 0);
    assert!(root.join("docs/nota.md").exists(), "escrita no caminho ampliado integrada");
    let t = task(&root, "T01");
    assert!(t.frontmatter.allowed_scope.iter().any(|g| g == "docs/**"));
    assert!(t.body.contains("## Orientações do Arquiteto") && t.body.contains("Posso gravar docs/nota.md?"));
    let dialog = fs::read_to_string(root.join(".sdd/specs").join(SPEC).join("dialog.jsonl")).expect("diálogo");
    assert!(dialog.contains("Pergunta ao Arquiteto: Posso gravar docs/nota.md?"), "{dialog}");
    assert!(dialog.contains("Resposta ao developer"), "{dialog}");
}

/// Liga o YOLO (a confirmação da tarefa de correção é automática) e commita a configuração.
fn enable_yolo(root: &Path) {
    let p = root.join(".sdd/settings.yaml");
    let text = fs::read_to_string(&p).expect("settings");
    fs::write(&p, format!("{text}yolo: true\n")).expect("yolo");
    git(root, &["add", ".sdd/settings.yaml"]);
    git(root, &["commit", "-q", "-m", "yolo"]);
}

fn messages(events: &[ApiEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.event_type() == "agent_message")
        .filter_map(|e| e.payload["text"].as_str().map(String::from))
        .collect()
}

/// Spec 059/T02: a verificação final reprova, o Arquiteto cria a T03, ela é integrada e a
/// reverificação aprova.
#[tokio::test]
async fn final_verification_creates_fix_task_and_reverifies() {
    let root = setup("final-fix");
    enable_yolo(&root);
    let llm = RoleLlm::new(&[true, true, false, true, true]);
    let state = state_for(&root, &llm);
    let events = plan_and_run(&state, &root).await;
    assert_eq!(spec_status(&root), SpecStatus::Completed);
    let t03 = task(&root, "T03");
    assert_eq!(t03.frontmatter.status, TaskStatus::Done);
    assert_eq!(t03.frontmatter.title, "Correção da verificação final");
    assert!(root.join("src/t03.txt").exists());
    assert_eq!(llm.review_calls.load(Ordering::SeqCst), 5);
    let msgs = messages(&events);
    assert!(msgs.iter().any(|m| m.starts_with("Verificação final: 0 de 1 critérios atendidos")), "{msgs:?}");
    assert!(msgs.iter().any(|m| m.starts_with("Verificação final: 1 de 1 critérios atendidos")), "{msgs:?}");
    assert!(!msgs.iter().any(|m| m.contains("AVISO")), "{msgs:?}");
}

/// Segunda falha: conclui com aviso explícito, sem nova tarefa.
#[tokio::test]
async fn final_verification_second_failure_completes_with_warning() {
    let root = setup("final-warn");
    enable_yolo(&root);
    let llm = RoleLlm::new(&[true, true, false, true, false]);
    let state = state_for(&root, &llm);
    let events = plan_and_run(&state, &root).await;
    assert_eq!(spec_status(&root), SpecStatus::Completed);
    assert!(!root.join(".sdd/specs").join(SPEC).join("tasks/T04.md").exists());
    assert_eq!(llm.review_calls.load(Ordering::SeqCst), 5);
    assert!(messages(&events).iter().any(|m| m.contains("AVISO") && m.contains("concluída assim mesmo")));
}
