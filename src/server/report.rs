//! Relatório final da spec: `.sdd/specs/<id>/report.md`, gerado pelo Arquiteto ao integrar a
//! última tarefa (evento `integrated` do run).
//!
//! Fluxo, depois de `integrated` (nunca antes, e nunca bloqueia a conclusão):
//! 1. reserva o projeto para o relatório (exclui run, remoção e outro relatório do projeto);
//! 2. se o `report.md` já existe, não regenera nem sobrescreve;
//! 3. o servidor monta os FATOS (spec, tarefas, commits de integração, arquivos, `test_command`,
//!    evidência da última tarefa) e chama um turno somente de leitura do Arquiteto;
//! 4. o texto devolvido é redigido, limitado a [`REPORT_MAX_BYTES`] e gravado pelo servidor
//!    (escrita atômica, local via `.git/info/exclude`: a base continua limpa).
//!
//! Toda falha vira `report_failed {spec_id, code}` com código fixo; a spec segue `INTEGRATED`/`DONE`.
//! `GET /api/projects/:id/specs/:spec_id/report` devolve `{markdown, generated_at, source}` ou 404.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use crate::agents::runner::{final_base_prefix, manual_criteria, section_text, ACCEPTANCE, FINAL_BASE_PREFIX};
use serde_json::{json, Map, Value};
use tokio::sync::watch;

use super::approvals::ProjectApprover;
use super::chat::{architect_client, load_policy};
use super::error::ApiError;
use super::events::{validate_project_id, ApiEvent, EventType, SecretSet};
use super::projects::{ensure_local_excludes, find_project, format_utc, read_head_branch, scrub_text};
use super::safe_fs::{self, MAX_READ_BYTES, O_NOFOLLOW};
use super::spec_result::{git_section, has_real_git_dir, validate_spec_id};
use super::state::AppState;
use crate::server::events::BusActivitySink;
use crate::agents::architect::{truncate_bytes, LlmArchitect};
use crate::agents::chat::ChatBackend;
use crate::i18n::Lang;
use crate::sdd::{Settings, Spec, Task, TaskStatus};

/// Nome do arquivo do relatório, dentro de `.sdd/specs/<id>/`.
pub const REPORT_FILE: &str = "report.md";
/// Linha do `.git/info/exclude`: o relatório é local (não suja a base nem bloqueia integrações).
pub const REPORT_EXCLUDE_LINE: &str = ".sdd/specs/*/report.md";
/// Linha do `.git/info/exclude` para os temporários da escrita atômica.
pub const REPORT_TMP_EXCLUDE_LINE: &str = ".sdd/specs/*/.report.md.*.tmp";
/// Tamanho máximo do relatório gravado e exposto.
pub const REPORT_MAX_BYTES: usize = 64 * 1024;
/// Tempo total do turno do relatório (180 s, como o turno do chat).
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(180);

const SPECS_REL: &str = ".sdd/specs";
const SPEC_FILE: &str = "spec.md";
const TASKS_DIR: &str = "tasks";
const SETTINGS_REL: &str = ".sdd/settings.yaml";
const SOURCE: &str = "architect";
const NO_EVIDENCE: &str = "sem registro de execução dos testes";
const NO_TEST_COMMAND: &str = "(não definido)";

/// Limites dos fatos enviados ao modelo.
const MAX_TASKS: usize = 50;
const MAX_TITLE_BYTES: usize = 200;
const MAX_EVIDENCE_BYTES: usize = 1024;
const MAX_TEST_RUN_BYTES: usize = 700;
const MAX_FINAL_BASE_BYTES: usize = 500;
const MAX_TEST_FACTS_BYTES: usize = 6 * 1024;
const MAX_FILES_IN_FACTS: usize = 100;
/// Fatos de revisão por tarefa (spec 063): teto por tarefa e total.
const MAX_REVIEW_TASK_BYTES: usize = 700;
const MAX_REVIEW_FACTS_BYTES: usize = 4 * 1024;
/// Spec 078 T01: teto da linha de observações (findings que não bloquearam) de cada tarefa.
const MAX_OBSERVATIONS_BYTES: usize = 400;
const MAX_ENV_ITEMS: usize = 10;
const MAX_ENV_BYTES: usize = 2 * 1024;
const MAX_FACTS_BYTES: usize = 16 * 1024;
/// Teto da seção "## Rastreabilidade" (spec 086 T07); o corte é em fim de linha.
const MAX_TRACEABILITY_BYTES: usize = 8 * 1024;

/// Códigos fixos de falha (`report_failed.code`). Nunca carregam texto de modelo ou de transporte;
/// as falhas do provedor usam o código fixo de `ArchitectError` (ex.: `provider_http_400`).
pub const CODE_NOT_CONFIGURED: &str = "not_configured";
pub const CODE_SPEC_UNREADABLE: &str = "spec_unreadable";
pub const CODE_TIMEOUT: &str = "timeout";
pub const CODE_SHUTDOWN: &str = "shutdown";
pub const CODE_WRITE_FAILED: &str = "write_failed";
pub const CODE_INTERNAL: &str = "internal";
/// Repositório em worktree (`.git` gitfile): o exclude local não alcança; o relatório não é gerado.
pub const CODE_GIT_UNSUPPORTED: &str = "git_unsupported";

/// Parada do relatório, com código fixo.
enum Stop {
    Failed(String),
}

impl Stop {
    fn failed(code: &str) -> Self {
        Self::Failed(code.to_string())
    }
}

/// Linha fixa do relatório quando o ambiente de testes Docker foi derrubado ao concluir a spec, no
/// idioma ativo na geração (spec 102).
pub fn env_clean_line(lang: Lang) -> String {
    crate::i18n::text(lang, "report.env_clean")
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Gatilho após `integrated`. Não bloqueia a conclusão: todo desfecho vira evento. A geração roda
/// em task própria (pânico vira `internal`). A reserva do projeto já é do run, retomada como
/// `__report__` pelo chamador: nenhuma aprovação, remoção ou ajuste entra antes do fim.
pub async fn after_integration(
    state: &AppState,
    project_id: &str,
    spec_id: &str,
    root: PathBuf,
    shutdown: watch::Receiver<bool>,
) {
    if validate_spec_id(spec_id).is_err() {
        return;
    }
    // Ambiente de testes (Docker): derrubado ANTES do relatório; o relatório registra a limpeza.
    let env_clean = {
        let (provider, env_root, pid) = (state.compose().clone(), root.clone(), project_id.to_string());
        match tokio::task::spawn_blocking(move || super::envs::cleanup_test(&provider, &env_root, &pid)).await {
            Ok(Ok(cleaned)) => cleaned,
            Ok(Err(e)) => {
                eprintln!("aviso: cleanup do ambiente de testes de {project_id}: {e}");
                false
            }
            Err(_) => false,
        }
    };
    let root_for_arch = root.clone();
    let had_report = report_exists(&root, spec_id);
    let shutdown_for_arch = shutdown.clone();
    let job = tokio::spawn(generate(
        state.clone(),
        project_id.to_string(),
        spec_id.to_string(),
        root,
        shutdown,
        env_clean,
    ));
    let outcome = job.await.unwrap_or_else(|_| Err(Stop::failed(CODE_INTERNAL)));
    let generated = outcome.is_ok() && !had_report;
    let event = match outcome {
        Ok(()) => ApiEvent::new(
            EventType::ReportReady,
            Some(project_id.to_string()),
            json!({"spec_id": spec_id}),
        ),
        Err(Stop::Failed(code)) => ApiEvent::new(
            EventType::ReportFailed,
            Some(project_id.to_string()),
            json!({"spec_id": spec_id, "code": code}),
        ),
    };
    state.events().publish(event);
    // Memória de arquitetura: só quando o relatório acabou de ser gerado (reexecução é idempotente).
    if generated {
        super::architecture::update_after_spec(state, project_id, spec_id, root_for_arch, shutdown_for_arch).await;
    }
}

/// Sequência da geração (na task de `after_integration`).
async fn generate(
    owned: AppState,
    project_id: String,
    spec_id: String,
    root: PathBuf,
    mut shutdown: watch::Receiver<bool>,
    env_clean: bool,
) -> Result<(), Stop> {
    let state = &owned;
    let project_id = project_id.as_str();
    let spec_id = spec_id.as_str();
    let sid = spec_id.to_string();
    let check_root = root.clone();
    let exists = blocking(move || report_exists(&check_root, &sid))
        .await
        .ok_or_else(|| Stop::failed(CODE_INTERNAL))?;
    if exists {
        return Ok(());
    }
    // Sem exclude local garantido (gitfile de worktree, symlink), a base ficaria suja: recusa antes do modelo.
    if !exclude_guaranteed(&root) {
        return Err(Stop::failed(CODE_GIT_UNSUPPORTED));
    }

    let secrets = state.secret_set().await;
    let sid = spec_id.to_string();
    let facts_root = root.clone();
    let facts_secrets = Arc::clone(&secrets);
    let facts = blocking(move || build_facts(&facts_root, &sid, &facts_secrets))
        .await
        .flatten()
        .ok_or_else(|| Stop::failed(CODE_SPEC_UNREADABLE))?;

    // Rastreabilidade critério global → tarefas → commit (spec 086 T07): montada pelo servidor.
    let sid = spec_id.to_string();
    let trace_root = root.clone();
    let trace_secrets = Arc::clone(&secrets);
    // Idioma ativo na hora da geração (spec 102): textos fixos do servidor e o turno do Arquiteto.
    let lang = state.project_lang(project_id);
    let lang_now = lang.get();
    let trace = blocking(move || traceability(&trace_root, &sid, &trace_secrets, lang_now)).await.flatten();

    let (model, effort, client) = architect_client(state)
        .await
        .map_err(|_| Stop::failed(CODE_NOT_CONFIGURED))?;
    let backend: Arc<dyn ChatBackend> = match state.report_backend() {
        Some(backend) => backend,
        None => Arc::new(client),
    };
    let policy = load_policy(root.clone()).await;
    let approver = Arc::new(ProjectApprover::new(
        project_id,
        root.clone(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::clone(&secrets),
    ));
    let activity = Arc::new(BusActivitySink::new(
        state.events().clone(),
        project_id,
        Arc::clone(&secrets),
    ).with_stage("report", "Escrevendo relatório final"));
    let architect = LlmArchitect::new(backend, model, Some(effort), policy, approver)
        .with_lang(lang)
        .with_activity(activity, Some(spec_id.to_string()), None)
        .with_usage(super::usage::FileUsageSink::redacting(root.clone(), project_id, Arc::clone(&secrets)))
        .with_memory(Arc::clone(&secrets));

    let turn = architect.report(&root, &facts);
    let answer = tokio::select! {
        biased;
        _ = wait_shutdown(&mut shutdown) => return Err(Stop::failed(CODE_SHUTDOWN)),
        joined = tokio::time::timeout(state.report_timeout(), turn) => match joined {
            Err(_) => return Err(Stop::failed(CODE_TIMEOUT)),
            Ok(Err(err)) => return Err(Stop::Failed(err.to_string())),
            Ok(Ok(text)) => text,
        },
    };

    // Texto final com uma quebra de linha, dentro do limite de bytes; o que o servidor acrescenta
    // (linha do ambiente limpo, rastreabilidade) tem espaço reservado e nunca é cortado.
    let mut tail = String::new();
    if env_clean {
        tail.push_str(&format!("\n{}\n", env_clean_line(lang_now)));
    }
    if let Some(section) = &trace {
        tail.push_str(&format!("\n{}\n", section.trim_end()));
    }
    let room = REPORT_MAX_BYTES.saturating_sub(1 + tail.len());
    let body = truncate_bytes(scrub_text(&secrets, &answer).trim_end(), room);
    let markdown = format!("{body}\n{tail}");
    let sid = spec_id.to_string();
    let write_root = root.clone();
    blocking(move || persist(&write_root, &sid, &markdown))
        .await
        .flatten()
        .ok_or_else(|| Stop::failed(CODE_WRITE_FAILED))
}

/// Espera o shutdown. Canal fechado também conta como shutdown.
pub(super) async fn wait_shutdown(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

/// Executa `job` no pool bloqueante. `None` se a task falhar (panic ou cancelamento).
async fn blocking<T, F>(job: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(job).await.ok()
}

/// `true` quando o `.git/info/exclude` do Nants alcança o repositório: sem `.git`, ou `.git` diretório
/// real. Gitfile (worktree linkado, cujo exclude é o do repositório principal) e symlink não: recusa.
pub(super) fn exclude_guaranteed(root: &Path) -> bool {
    match fs::symlink_metadata(root.join(".git")) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(meta) => meta.file_type().is_dir(),
    }
}

/// Caminho relativo ao projeto do relatório de uma spec.
fn report_rel(spec_id: &str) -> String {
    format!("{SPECS_REL}/{spec_id}/{REPORT_FILE}")
}

/// `true` quando já há um arquivo regular de relatório para a spec.
fn report_exists(root: &Path, spec_id: &str) -> bool {
    safe_fs::check_chain(root, &report_rel(spec_id))
        .ok()
        .and_then(|path| fs::symlink_metadata(path).ok())
        .is_some_and(|meta| meta.file_type().is_file())
}

/// Fatos do projeto para o modelo, redigidos e limitados. `None` se a spec não pode ser lida.
/// Tudo vem do disco e do Git; nada é inventado pelo modelo.
pub(super) fn build_facts(root: &Path, spec_id: &str, secrets: &SecretSet) -> Option<String> {
    let spec_rel = format!("{SPECS_REL}/{spec_id}");
    let text = safe_fs::read_regular(root, &format!("{spec_rel}/{SPEC_FILE}"), MAX_READ_BYTES).ok()?;
    let spec = Spec::parse(&text).ok()?;
    let settings = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES)
        .ok()
        .and_then(|raw| Settings::parse(&raw).ok());
    let test_command = settings
        .as_ref()
        .map(|s| s.test_command.trim().to_string())
        .filter(|c| !c.is_empty())
        .or_else(|| spec.frontmatter.test_command.clone())
        .unwrap_or_else(|| NO_TEST_COMMAND.to_string());
    let base = settings
        .as_ref()
        .map(|s| s.base_branch.clone())
        .unwrap_or_else(|| read_head_branch(root));
    // Falha do Git vira "indisponível", nunca "0 commits": o prompt proíbe afirmar ausência.
    let git = if has_real_git_dir(root) {
        git_section(root, &base, spec_id, secrets).ok()
    } else {
        Some((Vec::new(), Vec::new(), false))
    };
    let git_ok = git.is_some();
    let (commits, files, files_truncated) = git.unwrap_or_default();
    let tasks = read_tasks(root, &spec_rel);

    let mut out = String::new();
    out.push_str(&format!("Spec: {spec_id} ({})\n", title_of(secrets, &spec.frontmatter.title)));
    out.push_str(&format!("Status da spec: {}\n", enum_name(&spec.frontmatter.status)));
    out.push_str(&format!("test_command: {}\n", scrub_text(secrets, &test_command)));
    out.push_str(&format!("Tarefas ({}):\n", tasks.len()));
    for task in &tasks {
        out.push_str(&format!(
            "- {} · {} · {}\n",
            scrub_text(secrets, &task.frontmatter.id),
            title_of(secrets, &task.frontmatter.title),
            enum_name(&task.frontmatter.status)
        ));
    }
    let manual: Vec<String> = tasks
        .iter()
        .flat_map(|t| {
            let accept = section_text(&t.body, ACCEPTANCE).unwrap_or_default();
            let id = scrub_text(secrets, &t.frontmatter.id);
            manual_criteria(&accept).into_iter().map(move |m| format!("- {id}: {m}"))
        })
        .collect();
    if !manual.is_empty() {
        out.push_str("Verificação manual sugerida (itens (manual) das tarefas):\n");
        for m in &manual {
            out.push_str(&scrub_text(secrets, &truncate_bytes(m, MAX_TITLE_BYTES * 2)));
            out.push('\n');
        }
    }
    out.push_str("Resultado dos testes na integração, por tarefa concluída:\n");
    let mut test_bytes = 0usize;
    let mut omitted = 0usize;
    for t in tasks.iter().filter(|t| t.frontmatter.status == TaskStatus::Done) {
        let id = scrub_text(secrets, &t.frontmatter.id);
        let detail = match &t.frontmatter.test_run {
            Some(run) => {
                let code = run.exit_code.map_or("?".to_string(), |c| c.to_string());
                format!(
                    "`{}` exit={code} em {}: {}",
                    scrub_text(secrets, &run.command),
                    scrub_text(secrets, &run.at),
                    scrub_text(secrets, run.summary.trim())
                )
            }
            None => t
                .frontmatter
                .test_evidence
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(|e| truncate_bytes(&scrub_text(secrets, e), MAX_EVIDENCE_BYTES))
                .unwrap_or_else(|| NO_EVIDENCE.to_string()),
        };
        let line = format!("- {id}: {}\n", truncate_bytes(&detail, MAX_TEST_RUN_BYTES));
        if test_bytes + line.len() > MAX_TEST_FACTS_BYTES {
            omitted += 1;
            continue;
        }
        test_bytes += line.len();
        out.push_str(&line);
    }
    if omitted > 0 {
        out.push_str(&format!("- (+{omitted} tarefas sem espaço para o resultado dos testes)\n"));
    }
    if let Some(text) = final_base_fact(root, spec_id, secrets) {
        out.push_str(&format!("Verificação final na base (após a última integração): {text}\n"));
    }
    out.push_str(&environment_facts(
        root,
        settings.as_ref(),
        spec.frontmatter.run_command.as_deref(),
        spec.frontmatter.nothing_to_run.as_deref(),
        &test_command,
    ));
    out.push_str(&review_facts(&tasks, secrets));
    out.push_str(&skills_facts(root, settings.as_ref(), secrets));
    if git_ok {
        out.push_str(&format!(
            "Commits de integração ({}, do mais antigo ao mais recente):\n",
            commits.len()
        ));
        for commit in &commits {
            out.push_str(&format!("- {} {}\n", commit.hash, commit.subject));
        }
        let cut = if files_truncated { " (lista cortada)" } else { "" };
        out.push_str(&format!(
            "Arquivos alterados pelos commits, sem .sdd/ ({}{cut}):\n",
            files.len().min(MAX_FILES_IN_FACTS)
        ));
        for file in files.iter().take(MAX_FILES_IN_FACTS) {
            out.push_str(&format!("- {} {}\n", file.change, file.path));
        }
    } else {
        out.push_str(
            "Commits de integração: indisponível (falha ao ler o Git).\n\
             Arquivos alterados: indisponível (falha ao ler o Git).\n",
        );
    }
    Some(scrub_text(secrets, &truncate_bytes(&out, MAX_FACTS_BYTES)))
}

/// Seção "## Rastreabilidade" do relatório (spec 086 T07), sem o modelo: critério global →
/// tarefas que o declaram em `covers` → commit de integração `feat(<spec>): Txx - ` na base (o mais
/// recente da tarefa). `None` se a spec não pode ser lida ou o plano não declara `covers`.
/// Redigida e limitada a [`MAX_TRACEABILITY_BYTES`], cortada em fim de linha.
pub(super) fn traceability(root: &Path, spec_id: &str, secrets: &SecretSet, lang: Lang) -> Option<String> {
    let spec_rel = format!("{SPECS_REL}/{spec_id}");
    let text = safe_fs::read_regular(root, &format!("{spec_rel}/{SPEC_FILE}"), MAX_READ_BYTES).ok()?;
    let spec = Spec::parse(&text).ok()?;
    let tasks = read_tasks(root, &spec_rel);
    if tasks.iter().all(|t| t.frontmatter.covers.is_empty()) {
        return None;
    }
    let base = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES)
        .ok()
        .and_then(|raw| Settings::parse(&raw).ok())
        .map(|s| s.base_branch)
        .unwrap_or_else(|| read_head_branch(root));
    let commits = if has_real_git_dir(root) {
        git_section(root, &base, spec_id, secrets).map(|(c, _, _)| c).unwrap_or_default()
    } else {
        Vec::new()
    };
    // Redige antes de cortar o critério: segredo que passa do corte não pode vazar o prefixo.
    let body = scrub_text(secrets, &spec.body);
    let section = crate::sdd::coverage::traceability_section_in(lang, &body, &tasks, |id| {
        commits.iter().rev().find(|c| c.task_id == id).map(|c| c.hash.clone())
    })?;
    let scrubbed = scrub_text(secrets, &section);
    if scrubbed.len() <= MAX_TRACEABILITY_BYTES {
        return Some(scrubbed);
    }
    let cut = truncate_bytes(&scrubbed, MAX_TRACEABILITY_BYTES);
    let whole_lines = cut.rfind('\n').map_or(cut.as_str(), |i| &cut[..=i]);
    Some(whole_lines.to_string())
}

/// Seção "Ambiente" (spec 075): determinística, lida do disco. O agente de relatório não lê `.nants`,
/// então imagens, serviços, portas e comandos equivalentes fora do Nants chegam por aqui. O texto é
/// redigido por `build_facts` (scrub final) e limitado. Spec 131 T05: sem `run_command`, o fato
/// positivo de como o produto é usado no Nants (o Console, onde App e Execução abrem), com a decisão
/// `nothing_to_run` do plano quando houver, para o relatório não inventar como iniciar.
fn environment_facts(
    root: &Path,
    settings: Option<&Settings>,
    run_command: Option<&str>,
    nothing_to_run: Option<&str>,
    test_command: &str,
) -> String {
    use crate::sdd::docker_command::{unwrap_workspace_command, wrap_workspace};
    use crate::sdd::Environment;
    let docker = settings.is_some_and(|s| s.environment == Environment::Docker);
    let mut out = format!("Ambiente: {}\n", if docker { "docker" } else { "host" });
    let mut files: Vec<String> = fs::read_dir(root.join(".nants"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("Dockerfile"))
        .collect();
    files.sort();
    for name in files.iter().take(MAX_ENV_ITEMS) {
        let rel = format!(".nants/{name}");
        match safe_fs::read_regular(root, &rel, MAX_READ_BYTES) {
            Ok(text) => {
                let images: Vec<&str> = text
                    .lines()
                    .map(str::trim)
                    .filter(|l| l.get(..5).is_some_and(|h| h.eq_ignore_ascii_case("FROM ")))
                    .filter_map(|l| l[5..].split_whitespace().find(|t| !t.starts_with("--")))
                    .take(MAX_ENV_ITEMS)
                    .collect();
                let list = if images.is_empty() { "nenhuma".to_string() } else { images.join(", ") };
                out.push_str(&format!("- Imagens base ({rel}): {list}\n"));
            }
            Err(_) => out.push_str(&format!("- Imagens base ({rel}): não foi possível ler\n")),
        }
    }
    match safe_fs::read_regular(root, ".nants/compose.yaml", MAX_READ_BYTES) {
        Ok(text) => match compose_services(&text) {
            Some(list) => out.push_str(&format!("- Serviços e portas publicadas (.nants/compose.yaml): {list}\n")),
            None => out.push_str("- Serviços e portas publicadas (.nants/compose.yaml): não foi possível ler\n"),
        },
        Err(safe_fs::SafeFsError::Missing) => {}
        Err(_) => out.push_str("- Serviços e portas publicadas (.nants/compose.yaml): não foi possível ler\n"),
    }
    let run_command = run_command.map(str::trim).filter(|c| !c.is_empty());
    if run_command.is_none() {
        let decision = nothing_to_run
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(|d| format!(" (decisão do plano, nothing_to_run: {d})"))
            .unwrap_or_default();
        out.push_str(&format!(
            "- Executar: a spec não declara run_command{decision}. No Nants, o produto é usado pelo Console do projeto, onde as janelas App e Execução abrem; não há o que iniciar pelo botão de iniciar o app, e não há comando de execução a inventar.\n"
        ));
    }
    if docker {
        if let Some(run) = run_command {
            let inner = unwrap_workspace_command(run).unwrap_or_else(|| run.to_string());
            out.push_str(&format!("- Executar fora do Nants: {}\n", wrap_workspace(&inner)));
        }
        if test_command != NO_TEST_COMMAND && !test_command.trim().is_empty() {
            let inner = unwrap_workspace_command(test_command).unwrap_or_else(|| test_command.trim().to_string());
            out.push_str(&format!("- Testar fora do Nants: {}\n", wrap_workspace(&inner)));
        }
        out.push_str("- Pré-requisito no host: Docker com Compose (sem runtime da linguagem)\n");
    }
    truncate_bytes(&out, MAX_ENV_BYTES)
}

/// `nome (imagem; portas: …)` por serviço do compose, ou `None` se o YAML não é legível.
fn compose_services(text: &str) -> Option<String> {
    let doc: serde_yaml::Value = serde_yaml::from_str(text).ok()?;
    let services = doc.get("services")?.as_mapping()?;
    let scalar = |v: &serde_yaml::Value| match v {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let items: Vec<String> = services
        .iter()
        .take(MAX_ENV_ITEMS)
        .filter_map(|(k, v)| {
            let name = k.as_str()?;
            let image = v.get("image").and_then(|i| i.as_str()).map(|i| format!("imagem {i}"));
            let ports: Vec<String> = v
                .get("ports")
                .and_then(|p| p.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|p| match p {
                            serde_yaml::Value::Mapping(_) => {
                                let target = p.get("target").and_then(scalar)?;
                                Some(match p.get("published").and_then(scalar) {
                                    Some(host) => format!("{host}:{target}"),
                                    None => target,
                                })
                            }
                            other => scalar(other),
                        })
                        .take(MAX_ENV_ITEMS)
                        .collect()
                })
                .unwrap_or_default();
            let ports = if ports.is_empty() { "sem portas publicadas".to_string() } else { format!("portas {}", ports.join(", ")) };
            Some(match image {
                Some(i) => format!("{name} ({i}; {ports})"),
                None => format!("{name} ({ports})"),
            })
        })
        .collect();
    Some(if items.is_empty() { "nenhum serviço".to_string() } else { items.join("; ") })
}

/// Revisões por tarefa (spec 063): tentativas, último veredito e findings registrados nas
/// reprovações. Só tarefas com reprovação; vazio se nenhuma. Base das "Lições do projeto".
fn review_facts(tasks: &[Task], secrets: &SecretSet) -> String {
    let mut body = String::new();
    for t in tasks {
        let log = section_text(&t.body, crate::agents::runner::FINDINGS_LOG).unwrap_or_default();
        let observations = section_text(&t.body, crate::agents::runner::OBSERVATIONS).unwrap_or_default();
        if t.frontmatter.attempts == 0 && log.trim().is_empty() && observations.trim().is_empty() {
            continue;
        }
        let mut item = format!(
            "- {}: tentativas={}, veredito={}\n",
            scrub_text(secrets, &t.frontmatter.id),
            t.frontmatter.attempts,
            enum_name(&t.frontmatter.last_verdict)
        );
        for line in log.lines().filter(|l| !l.trim().is_empty()) {
            item.push_str("  ");
            item.push_str(&scrub_text(secrets, line));
            item.push('\n');
        }
        let mut item = truncate_bytes(&item, MAX_REVIEW_TASK_BYTES);
        if !item.ends_with('\n') {
            item.push('\n');
        }
        let notes: Vec<&str> = observations.lines().map(|l| l.trim().trim_start_matches("- ").trim()).filter(|l| !l.is_empty()).collect();
        if !notes.is_empty() {
            let line = scrub_text(secrets, &notes.join(" | "));
            item.push_str(&format!("  observações (não bloquearam): {}\n", truncate_bytes(&line, MAX_OBSERVATIONS_BYTES)));
        }
        if body.len() + item.len() > MAX_REVIEW_FACTS_BYTES {
            body.push_str("- (+ tarefas sem espaço)\n");
            break;
        }
        body.push_str(&item);
        if !body.ends_with('\n') {
            body.push('\n');
        }
    }
    if body.is_empty() {
        return String::new();
    }
    format!("Revisões por tarefa (tentativas, veredito, findings das reprovações e observações que não bloquearam; cite a origem, ex.: T03/F-02):\n{body}")
}

/// Perfis de especialista (spec 063): se a injeção estava ligada e o `updated_at` de cada perfil,
/// para comparar execuções com e sem perfil.
fn skills_facts(root: &Path, settings: Option<&Settings>, secrets: &SecretSet) -> String {
    use crate::sdd::skills::{read_profile, ROLES};
    let on = settings.is_none_or(|s| s.skills);
    let mut out = format!("Perfis de especialista (skills): {}\n", if on { "ligados" } else { "desligados (skills: false)" });
    for role in ROLES {
        match read_profile(root, role) {
            Some(p) => out.push_str(&format!(
                "- {role}: updated_at={}, source_spec={}\n",
                scrub_text(secrets, p.meta.updated_at.trim()),
                scrub_text(secrets, p.meta.source_spec.trim())
            )),
            None => out.push_str(&format!("- {role}: sem perfil\n")),
        }
    }
    out
}

/// Spec 077 T02: texto da última mensagem `tests` sem tarefa que começa com "verificação final na base"
/// (grava o runner em `dialog.jsonl`), sem o prefixo, redigido e truncado. `None` se não houver.
/// O prefixo vale em qualquer idioma suportado (spec 113 T04: a linha sai no idioma do projeto).
pub(super) fn final_base_fact(root: &Path, spec_id: &str, secrets: &SecretSet) -> Option<String> {
    let prefixes: Vec<String> = std::iter::once(FINAL_BASE_PREFIX.to_string())
        .chain([Lang::PtBr, Lang::En].into_iter().map(final_base_prefix))
        .collect();
    let page = super::dialog::read_page(root, spec_id, 0, usize::MAX);
    let (line, prefix) = page.messages.iter().rev().find_map(|m| {
        if m.agent != "tests" || m.task_id.is_some() {
            return None;
        }
        prefixes.iter().find(|p| m.text.starts_with(p.as_str())).map(|p| (m, p))
    })?;
    let rest = line.text[prefix.len()..].trim_start_matches(':').trim();
    let text = if rest.is_empty() { line.text.trim() } else { rest };
    Some(truncate_bytes(&scrub_text(secrets, text), MAX_FINAL_BASE_BYTES))
}

/// Título redigido e limitado (texto de projeto é dado, não instrução).
fn title_of(secrets: &SecretSet, raw: &str) -> String {
    truncate_bytes(&scrub_text(secrets, raw.trim()), MAX_TITLE_BYTES)
}

/// Nome de um enum serializado (ex.: `DONE`), ou vazio.
fn enum_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Tarefas `tasks/*.md` válidas, por id. Arquivos inseguros, ilegíveis ou inválidos são ignorados.
fn read_tasks(root: &Path, spec_rel: &str) -> Vec<Task> {
    let tasks_rel = format!("{spec_rel}/{TASKS_DIR}");
    let Ok(dir) = safe_fs::check_chain(root, &tasks_rel) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".md") && !n.starts_with('.'))
        .collect();
    names.sort();
    let mut tasks: Vec<Task> = names
        .iter()
        .filter_map(|n| safe_fs::read_regular(root, &format!("{tasks_rel}/{n}"), MAX_READ_BYTES).ok())
        .filter_map(|raw| Task::parse(&raw).ok())
        .collect();
    tasks.sort_by(|a, b| a.frontmatter.id.cmp(&b.frontmatter.id));
    tasks.truncate(MAX_TASKS);
    tasks
}

/// Grava o relatório de forma atômica: temporário exclusivo no mesmo diretório, `fsync` e
/// `rename`. Antes, garante as entradas locais no `.git/info/exclude`.
fn persist(root: &Path, spec_id: &str, markdown: &str) -> Option<()> {
    if !exclude_guaranteed(root) {
        return None;
    }
    ensure_local_excludes(root).ok()?;
    let rel = report_rel(spec_id);
    let target = safe_fs::check_chain(root, &rel).ok()?;
    let dir = target.parent()?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{REPORT_FILE}.{}.{seq}.tmp", process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(markdown.as_bytes())?;
            file.sync_all()
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    // Revalida o destino logo antes do rename (rename não segue symlink no último componente).
    if safe_fs::check_chain(root, &rel).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    fs::rename(&tmp, &target).map_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
    .ok()
}

/// `GET /api/projects/:id/specs/:spec_id/report`: somente leitura. Sem relatório seguro → 404.
pub async fn get_report(
    State(state): State<AppState>,
    AxPath((id, spec_id)): AxPath<(String, String)>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    validate_spec_id(&spec_id)?;
    let project = find_project(&state, &id).await?;
    let secrets = state.secret_set().await;
    let root = PathBuf::from(&project.path);
    let view = tokio::task::spawn_blocking(move || read_report(&root, &spec_id, &secrets))
        .await
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))?;
    match view {
        Some(view) => Ok(Json(view).into_response()),
        None => Err(ApiError::coded(StatusCode::NOT_FOUND, "report_not_found", Map::new())),
    }
}

/// Relatório redigido com a data de gravação (mtime). `None` se ausente, inseguro ou grande.
fn read_report(root: &Path, spec_id: &str, secrets: &SecretSet) -> Option<Value> {
    let rel = report_rel(spec_id);
    let text = safe_fs::read_regular(root, &rel, REPORT_MAX_BYTES as u64).ok()?;
    let path = safe_fs::check_chain(root, &rel).ok()?;
    let modified = fs::symlink_metadata(path).ok()?.modified().ok()?;
    let secs = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(json!({
        "markdown": scrub_text(secrets, &text),
        "generated_at": format_utc(secs),
        "source": SOURCE,
    }))
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;

/// Spec 077 T02: fato da verificação final na base, lido do `dialog.jsonl`.
#[cfg(test)]
mod final_base_tests {
    use super::*;

    const SPEC: &str = "001-exemplo";
    const SECRET: &str = "sk-SEGREDO-FINAL-777";
    const SPEC_MD: &str = "---\nid: \"001-exemplo\"\ntitle: \"Exemplo\"\nstatus: \"COMPLETED\"\nbase_branch: \"main\"\ntest_command: \"cargo test\"\ncreated_at: \"2026-10-14T00:00:00Z\"\n---\n## Contexto\nCorpo.\n";

    fn root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-report-final-{}-{tag}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let spec_dir = dir.join(SPEC_REL_TEST).join(SPEC);
        fs::create_dir_all(spec_dir.join("tasks")).expect("tasks");
        fs::write(spec_dir.join("spec.md"), SPEC_MD).expect("spec");
        dir
    }

    const SPEC_REL_TEST: &str = ".sdd/specs";

    fn write_dialog(root: &Path, lines: &[(u64, &str, Option<&str>, &str)]) {
        let body: String = lines
            .iter()
            .map(|(seq, agent, task, text)| {
                format!("{}\n", json!({"seq": seq, "ts": 1, "agent": agent, "task_id": task, "attempt": null, "text": text}))
            })
            .collect();
        fs::write(root.join(SPEC_REL_TEST).join(SPEC).join("dialog.jsonl"), body).expect("dialog");
    }

    #[test]
    fn facts_include_last_final_base_message_scrubbed() {
        let secrets = SecretSet::new(vec![SECRET.to_string()]);
        let root = root("com-fato");
        write_dialog(
            &root,
            &[
                (1, "tests", Some("T01"), "testes ✓ · checagens: build ✓"),
                (2, "tests", None, "verificação final na base: testes ✗ · checagens: build ✓"),
                (3, "tests", None, &format!("verificação final na base: testes ✓ · checagens: build ✓ {SECRET}")),
                (4, "reviewer", None, "verificação final na base: do revisor, ignorada"),
            ],
        );
        let facts = build_facts(&root, SPEC, &secrets).expect("fatos");
        assert!(
            facts.contains("Verificação final na base (após a última integração): testes ✓ · checagens: build ✓"),
            "{facts}"
        );
        assert!(!facts.contains("testes ✗") && !facts.contains(SECRET) && !facts.contains("ignorada"), "{facts}");
    }

    #[test]
    fn facts_report_failure_text_and_truncate() {
        let secrets = SecretSet::new(Vec::new());
        let root = root("falha");
        let long = format!("verificação final na base: testes ✗ · checagens: {}", "x".repeat(2000));
        write_dialog(&root, &[(1, "tests", None, &long)]);
        let fact = final_base_fact(&root, SPEC, &secrets).expect("fato");
        assert!(fact.starts_with("testes ✗ · checagens: x") && fact.len() <= MAX_FINAL_BASE_BYTES, "{fact}");
    }

    #[test]
    fn facts_omit_final_base_line_without_message() {
        let secrets = SecretSet::new(Vec::new());
        let root = root("sem-fato");
        let none = build_facts(&root, SPEC, &secrets).expect("fatos");
        assert!(!none.contains("Verificação final na base"), "{none}");
        write_dialog(&root, &[(1, "tests", Some("T01"), "verificação final na base: por tarefa, ignorada"), (2, "architect", None, "oi")]);
        let facts = build_facts(&root, SPEC, &secrets).expect("fatos");
        assert!(!facts.contains("Verificação final na base"), "{facts}");
    }

    /// Spec 113 T04: com o projeto em en, a linha da verificação final sai em en e segue lida.
    #[test]
    fn facts_read_final_base_line_written_in_english() {
        let secrets = SecretSet::new(Vec::new());
        let root = root("fato-en");
        write_dialog(
            &root,
            &[
                (1, "tests", None, "verificação final na base: testes ✗"),
                (2, "tests", None, "final verification on the base: tests ✓ · checks: build ✓"),
            ],
        );
        let fact = final_base_fact(&root, SPEC, &secrets).expect("fato");
        assert_eq!(fact, "tests ✓ · checks: build ✓");
    }
}

/// Spec 078 T01: observações que não bloquearam entram nos fatos de revisão.
#[cfg(test)]
mod observation_tests {
    use super::*;

    const SECRET: &str = "sk-SEGREDO-OBS-999";

    fn task(id: &str, body: &str, attempts: u32) -> Task {
        let text = format!(
            "---\nid: \"{id}\"\nspec_id: \"001-exemplo\"\ntitle: \"t\"\nstatus: \"DONE\"\ndepends_on: []\nallowed_scope: [\"src/**\"]\nbranch: \"\"\nattempts: {attempts}\nmax_attempts: 3\nlast_review_verdict: \"APPROVED\"\n---\n{body}"
        );
        Task::parse(&text).expect("tarefa")
    }

    #[test]
    fn review_facts_list_observations_scrubbed_and_truncated() {
        let secrets = SecretSet::new(vec![SECRET.to_string()]);
        let long = "x".repeat(1000);
        let body = format!(
            "## Observações da revisão\n- F-02 [MEDIUM] src/a.rs:3: foco do diálogo {SECRET}\n- F-03 [MEDIUM] src/b.rs:9: {long}\n"
        );
        let facts = review_facts(&[task("T03", &body, 0), task("T04", "## Objetivo\nx\n", 0)], &secrets);
        assert!(facts.contains("- T03: tentativas=0"), "{facts}");
        assert!(facts.contains("observações (não bloquearam): F-02 [MEDIUM] src/a.rs:3: foco do diálogo"), "{facts}");
        assert!(!facts.contains(SECRET) && !facts.contains("T04"), "{facts}");
        let line = facts.lines().find(|l| l.contains("observações (não")).expect("linha");
        assert!(line.len() <= MAX_OBSERVATIONS_BYTES + 60, "{}", line.len());
    }

    #[test]
    fn review_facts_without_observations_or_attempts_is_empty() {
        let secrets = SecretSet::new(Vec::new());
        assert_eq!(review_facts(&[task("T01", "## Objetivo\nx\n", 0)], &secrets), "");
    }
}
