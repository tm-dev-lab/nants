//! `GET /api/projects/:id/specs/:spec_id/result`: o que a spec construiu, em leitura somente.
//!
//! Lê tarefas e spec do disco, commits de integração `feat(<spec>): Txx - ...` na branch base,
//! arquivos tocados por esses commits (sem `.sdd/`), trecho do README e o `test_command`.
//! Não cria worktree, não escreve nada e não muda status. Tudo roda em `spawn_blocking` sob
//! `GIT_TIMEOUT`; erros de Git ou de I/O viram 500 estático, sem eco.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use super::error::ApiError;
use super::events::{validate_project_id, SecretSet};
use super::projects::{
    find_project, format_utc, load_tasks, next_action_view, read_head_branch, safe_error,
    scrub_text, text_field, INVALID_FILE,
};
use super::runs::GIT_TIMEOUT;
use super::safe_fs::{self, MAX_READ_BYTES};
use super::state::AppState;
use crate::harness::git::Git;
use crate::sdd::{
    check_test_command_shape, Settings, Spec, SpecStatus, TaskStatus, TEST_COMMAND_MAX_CHARS,
};

/// Tamanho máximo de `spec_id` (como nas demais rotas de spec).
const MAX_SPEC_ID_LEN: usize = 64;
const SPEC_ID_MSG: &str = "spec_id deve conter apenas [a-zA-Z0-9_-] e ter de 1 a 64 caracteres";
const SPEC_NOT_FOUND: &str = "spec não encontrada";
const TIMEOUT_MSG: &str = "tempo limite na leitura do resultado";
const GIT_READ_MSG: &str = "falha ao ler o histórico do Git";
const INTERNAL_MSG: &str = "falha interna";

const SPECS_DIR: &str = ".sdd/specs";
const SETTINGS_REL: &str = ".sdd/settings.yaml";
const README_REL: &str = "README.md";
const SDD_DIR: &str = ".sdd";

/// Commits de integração listados (os mais recentes, em ordem cronológica).
const MAX_COMMITS: usize = 100;
/// Arquivos listados no conjunto da spec.
const MAX_FILES: usize = 200;
/// README lido do disco: acima disso é ignorado (→ `null`).
const README_MAX_READ_BYTES: u64 = 64 * 1024;
/// Trecho do README exposto: linhas e bytes.
const README_MAX_LINES: usize = 60;
const README_MAX_BYTES: usize = 8 * 1024;
/// Caracteres do hash curto.
const SHORT_HASH_LEN: usize = 7;

/// Resultado de uma spec, como exibido à API.
#[derive(Debug, Serialize)]
pub struct ResultView {
    spec_id: String,
    title: String,
    status: Option<SpecStatus>,
    tasks: Vec<ResultTask>,
    commits: Vec<CommitView>,
    files: Vec<FileView>,
    files_truncated: bool,
    readme: Option<ReadmeView>,
    project_path: String,
    test_command: Option<String>,
    /// Comando para subir o sistema (spec 021), redigido; `null` se ausente ou inválido.
    run_command: Option<String>,
    /// URL local do sistema (spec 021), redigida; `null` se ausente ou inválida.
    run_url: Option<String>,
    /// `docker` quando o projeto roda em Compose (spec 030): "Executar para testar" usa o `-run`.
    environment: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ResultTask {
    id: String,
    title: String,
    status: Option<TaskStatus>,
    /// Texto fixo quando a tarefa não pôde ser lida (frontmatter inválido, símbolo especial...).
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CommitView {
    pub(super) hash: String,
    pub(super) subject: String,
    pub(super) task_id: String,
    date: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FileView {
    pub(super) path: String,
    pub(super) change: &'static str,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ReadmeView {
    text: String,
    truncated: bool,
}

/// `GET /api/projects/:id/specs/:spec_id/result`
pub async fn get_result(
    State(state): State<AppState>,
    AxPath((id, spec_id)): AxPath<(String, String)>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    validate_spec_id(&spec_id)?;
    let project = find_project(&state, &id).await?;
    let secrets = state.secret_set().await;
    let root = PathBuf::from(&project.path);
    let timeout = timeout_for(&root);
    let job_root = root.clone();
    let handle = tokio::task::spawn_blocking(move || build_result(&job_root, &spec_id, &secrets));
    match tokio::time::timeout(timeout, handle).await {
        Err(_) => Err(ApiError::new(StatusCode::GATEWAY_TIMEOUT, TIMEOUT_MSG)),
        Ok(Err(_)) => Err(ApiError::internal(INTERNAL_MSG)),
        Ok(Ok(result)) => Ok(Json(result?).into_response()),
    }
}

/// Monta o resultado no disco e no Git. Chamado dentro de `spawn_blocking`.
fn build_result(root: &Path, spec_id: &str, secrets: &SecretSet) -> Result<ResultView, ApiError> {
    test_gate(root);
    let project_path = scrub_text(secrets, &root.to_string_lossy());
    let spec_rel = format!("{SPECS_DIR}/{spec_id}");
    let text = safe_fs::read_regular(root, &format!("{spec_rel}/spec.md"), MAX_READ_BYTES)
        .map_err(spec_read_error)?;
    let spec = Spec::parse(&text).map_err(|_| ApiError::new(StatusCode::CONFLICT, INVALID_FILE))?;
    let title = text_field(secrets, "title", &spec.frontmatter.title);
    let tasks = load_tasks(root, &spec_rel, secrets)
        .into_iter()
        .map(|t| ResultTask {
            id: t.id,
            title: t.title,
            status: t.status,
            error: t.error,
        })
        .collect();
    let settings = read_settings(root);
    let settings_docker = settings.as_ref().is_some_and(|s| s.environment == crate::sdd::Environment::Docker);
    let base = settings
        .as_ref()
        .map(|s| s.base_branch.clone())
        .unwrap_or_else(|| read_head_branch(root));
    let (commits, files, files_truncated) = if has_real_git_dir(root) {
        git_section(root, &base, spec_id, secrets)?
    } else {
        (Vec::new(), Vec::new(), false)
    };
    Ok(ResultView {
        spec_id: spec_id.to_string(),
        title,
        status: Some(spec.frontmatter.status),
        tasks,
        commits,
        files,
        files_truncated,
        readme: readme_view(root, secrets),
        project_path,
        test_command: settings.and_then(|s| test_command_view(secrets, &s.test_command)),
        run_command: spec
            .frontmatter
            .run_command
            .as_deref()
            .map(|c| scrub_text(secrets, c)),
        run_url: spec.frontmatter.run_url.as_deref().map(|u| scrub_text(secrets, u)),
        environment: if settings_docker { "docker" } else { "host" },
    })
}

/// Spec ausente → 404 estático; symlink, especial ou grande → 409; resto de I/O → 500 estático.
fn spec_read_error(e: safe_fs::SafeFsError) -> ApiError {
    match e {
        safe_fs::SafeFsError::Missing => ApiError::new(StatusCode::NOT_FOUND, SPEC_NOT_FOUND),
        other => safe_error(other),
    }
}

/// `.git` é diretório real (não symlink nem gitfile). Só então o Git é consultado: link ou
/// gitfile podem apontar para um repositório fora do projeto. Nesses casos a seção fica vazia.
pub(super) fn has_real_git_dir(root: &Path) -> bool {
    fs::symlink_metadata(root.join(".git")).is_ok_and(|m| m.file_type().is_dir())
}

/// Commits de integração na base, arquivos tocados por eles e se a lista foi cortada.
pub(super) fn git_section(
    root: &Path,
    base: &str,
    spec_id: &str,
    secrets: &SecretSet,
) -> Result<(Vec<CommitView>, Vec<FileView>, bool), ApiError> {
    let git = Git::new(root);
    let prefix = format!("feat({spec_id}): T");
    let records = git
        .log_grep(base, &prefix)
        .map_err(|_| ApiError::internal(GIT_READ_MSG))?;
    // `log` vem do mais novo; pega os MAX_COMMITS mais recentes e devolve em ordem cronológica.
    let mut integrations: Vec<(String, CommitView)> = records
        .into_iter()
        .filter_map(|r| {
            let task_id = task_id_of(&r.subject, &prefix)?;
            let view = CommitView {
                hash: r.hash.get(..SHORT_HASH_LEN)?.to_string(),
                subject: next_action_view(secrets, Some(&r.subject)).unwrap_or_default(),
                task_id,
                date: format_utc(r.committed_at),
            };
            Some((r.hash, view))
        })
        .take(MAX_COMMITS)
        .collect();
    integrations.reverse();

    let mut states: BTreeMap<String, (Change, Change)> = BTreeMap::new();
    for (hash, _) in &integrations {
        for (letter, path) in git
            .commit_changes(hash)
            .map_err(|_| ApiError::internal(GIT_READ_MSG))?
        {
            let Some(kind) = Change::from_letter(letter) else {
                continue;
            };
            if path == SDD_DIR || path.starts_with(".sdd/") {
                continue;
            }
            states
                .entry(path)
                .and_modify(|s| s.1 = kind)
                .or_insert((kind, kind));
        }
    }
    let files: Vec<FileView> = states
        .into_iter()
        .filter_map(|(path, (first, last))| {
            final_change(first, last).map(|change| FileView {
                path: scrub_text(secrets, &path),
                change: change.letter(),
            })
        })
        .collect();
    let truncated = files.len() > MAX_FILES;
    let files = files.into_iter().take(MAX_FILES).collect();
    let commits = integrations.into_iter().map(|(_, view)| view).collect();
    Ok((commits, files, truncated))
}

/// `Txx` do assunto `<prefix>NN - ...` (com `prefix` = `feat(<spec>): T`); `None` se não casar.
pub(super) fn task_id_of(subject: &str, prefix: &str) -> Option<String> {
    let rest = subject.strip_prefix(prefix)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with(" - ") {
        return None;
    }
    Some(format!("T{digits}"))
}

/// Alteração de um arquivo em um commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Added,
    Modified,
    Deleted,
}

impl Change {
    /// Letra do `--name-status`. `T` (troca de tipo) conta como modificação.
    fn from_letter(letter: char) -> Option<Self> {
        match letter {
            'A' => Some(Self::Added),
            'M' | 'T' => Some(Self::Modified),
            'D' => Some(Self::Deleted),
            _ => None,
        }
    }

    fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
        }
    }
}

/// Estado final de um caminho a partir da primeira e da última alteração da spec.
/// Criado e removido na mesma spec não aparece; removido no fim é `D`; criado no começo é `A`.
fn final_change(first: Change, last: Change) -> Option<Change> {
    match (first, last) {
        (Change::Added, Change::Deleted) => None,
        (_, Change::Deleted) => Some(Change::Deleted),
        (Change::Added, _) => Some(Change::Added),
        _ => Some(Change::Modified),
    }
}

/// `settings.yaml` válido, ou `None` (ausente, inseguro, grande ou inválido).
fn read_settings(root: &Path) -> Option<Settings> {
    let text = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES).ok()?;
    Settings::parse(&text).ok()
}

/// `test_command` do settings, redigido. Forma inválida, excesso de tamanho ou vazio → `None`.
fn test_command_view(secrets: &SecretSet, raw: &str) -> Option<String> {
    let command = check_test_command_shape(raw).ok()?;
    let scrubbed = scrub_text(secrets, &command);
    let fits = scrubbed.chars().count() <= TEST_COMMAND_MAX_CHARS;
    (fits && !scrubbed.chars().any(char::is_control)).then_some(scrubbed)
}

/// Trecho do README: lido com limite, redigido antes do corte, texto puro, até 60 linhas e 8 KiB.
/// Ausente, symlink, especial, grande ou não UTF-8 → `None`.
fn readme_view(root: &Path, secrets: &SecretSet) -> Option<ReadmeView> {
    let text = safe_fs::read_regular(root, README_REL, README_MAX_READ_BYTES).ok()?;
    let scrubbed = scrub_text(secrets, &text);
    let plain: String = scrubbed
        .chars()
        .map(|c| if c.is_control() && c != '\n' && c != '\t' { ' ' } else { c })
        .collect();
    let lines: Vec<&str> = plain.lines().collect();
    let mut truncated = lines.len() > README_MAX_LINES;
    let mut out = lines
        .iter()
        .take(README_MAX_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if out.len() > README_MAX_BYTES {
        let mut end = README_MAX_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        truncated = true;
    }
    Some(ReadmeView {
        text: out.trim_end().to_string(),
        truncated,
    })
}

/// `spec_id` validado como nas demais rotas de spec: `[a-zA-Z0-9_-]`, 1 a 64 caracteres.
pub(super) fn validate_spec_id(id: &str) -> Result<(), ApiError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_SPEC_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ApiError::unprocessable(SPEC_ID_MSG))
    }
}

/// Tempo limite do job de leitura. Testes podem reduzi-lo por raiz.
fn timeout_for(root: &Path) -> Duration {
    #[cfg(test)]
    if let Some(timeout) = test_timeout(root) {
        return timeout;
    }
    let _ = root;
    GIT_TIMEOUT
}

/// Ganchos de teste: limite por raiz e ação executada dentro do job bloqueante.
#[cfg(test)]
pub(super) static TEST_HOOKS: std::sync::Mutex<Vec<(PathBuf, Box<dyn FnOnce() + Send>)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(super) static TEST_TIMEOUTS: std::sync::Mutex<Vec<(PathBuf, Duration)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn test_timeout(root: &Path) -> Option<Duration> {
    let overrides = TEST_TIMEOUTS.lock().unwrap_or_else(|e| e.into_inner());
    overrides.iter().find(|(r, _)| r == root).map(|(_, d)| *d)
}

#[cfg(test)]
fn test_gate(root: &Path) {
    let hook = {
        let mut hooks = TEST_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
        hooks
            .iter()
            .position(|(r, _)| r == root)
            .map(|i| hooks.remove(i).1)
    };
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(not(test))]
fn test_gate(_root: &Path) {}

#[cfg(test)]
#[path = "spec_result_tests.rs"]
mod tests;
