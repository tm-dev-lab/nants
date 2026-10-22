//! Explorador de arquivos do projeto (spec 066): endpoints SOMENTE leitura.
//!
//! - `GET /api/projects/:id/files?source=base[&dir=<rel>]`: lista plana pelo git (tracked +
//!   untracked não ignorados), pastas do disco ignoradas pelo git como `ignored_dir`, e, com
//!   `dir`, o conteúdo de uma pasta via `read_dir` confinado (teto de 500 entradas).
//! - `GET /api/projects/:id/file?source=base&path=<rel>[&reveal=1]`: conteúdo de um arquivo.
//!
//! Confinamento como `safe_fs`: nenhum componente pode ser symlink, `.git` e `.sdd/worktrees`
//! nunca aparecem, e as mensagens de erro são estáticas (sem eco de caminho do disco).
//!
//! Ponto de extensão: [`Source`] e [`resolve_root`]. A T02 acrescenta `Source::Task` (raiz da
//! worktree da tarefa ativa); qualquer `source` fora de `base`/`task` é 400.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::error::ApiError;
use super::events::{validate_project_id, SecretSet};
use super::projects::{find_project, read_head_branch, real_children, scrub_text};
use super::recovery::read_root;
use super::safe_fs::{self, SafeFsError};
use super::state::AppState;
use crate::config::Project;
use crate::harness::git::Git;
use crate::sdd::{Settings, Task, TaskStatus};

/// Limite de leitura de um arquivo (512 KiB).
pub const MAX_FILE_BYTES: u64 = 512 * 1024;
/// Teto de caminhos na lista plana.
pub const MAX_LIST_PATHS: usize = 20_000;
/// Teto de entradas ao listar uma pasta ignorada.
pub const MAX_DIR_ENTRIES: usize = 500;
/// Varredura máxima de uma pasta (antes de ordenar e cortar em `MAX_DIR_ENTRIES`).
const MAX_DIR_SCAN: usize = 5_000;
/// Bytes inspecionados em busca de NUL para detectar binário.
const BINARY_PROBE: usize = 8 * 1024;
/// Tamanho máximo aceito para `path`/`dir`.
const MAX_REL_LEN: usize = 4096;
/// Máscara dos valores de segredo.
const MASK: &str = "•••";
/// Validade máxima da lista em cache (novos arquivos não rastreados e `.gitignore` aparecem).
const LIST_TTL: Duration = Duration::from_secs(2);
/// Quantidade máxima de raízes no cache da lista.
const CACHE_MAX_ROOTS: usize = 64;

const MSG_SOURCE: &str = "fonte inválida";
const MSG_PATH: &str = "caminho inválido";
const MSG_NOT_FOUND: &str = "não encontrado";
const MSG_IO: &str = "falha de I/O no diretório do projeto";
const MSG_NO_GIT: &str = "repositório git indisponível";
const MSG_INTERNAL: &str = "falha interna";

/// Fonte da árvore: base integrada ou worktree da tarefa ativa (resolvida no servidor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Base,
    Task,
}

impl Source {
    fn parse(raw: Option<&str>) -> Result<Self, ApiError> {
        match raw {
            None | Some("base") => Ok(Self::Base),
            Some("task") => Ok(Self::Task),
            Some(_) => Err(ApiError::new(StatusCode::BAD_REQUEST, MSG_SOURCE)),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Task => "task",
        }
    }
}

/// Raiz resolvida da fonte; `task` só existe para `Source::Task`.
#[derive(Debug, Clone)]
struct Resolved {
    root: PathBuf,
    task: Option<ActiveTask>,
}

/// Tarefa ativa com worktree registrada.
#[derive(Debug, Clone)]
struct ActiveTask {
    id: String,
    branch: String,
    base: String,
}

/// 409 estático: sem tarefa ativa ou sem worktree.
fn no_active_task() -> Response {
    (StatusCode::CONFLICT, Json(json!({ "reason": "no_active_task" }))).into_response()
}

/// Prioridade da tarefa ativa (menor vence): IN_PROGRESS > IN_REVIEW > CHANGES_REQUESTED > BLOCKED.
/// BLOCKED entra porque a worktree continua existindo (e é o que o usuário quer inspecionar).
fn active_rank(status: TaskStatus) -> Option<u8> {
    match status {
        TaskStatus::InProgress => Some(0),
        TaskStatus::InReview => Some(1),
        TaskStatus::ChangesRequested => Some(2),
        TaskStatus::Blocked => Some(3),
        _ => None,
    }
}

/// Id sem separadores nem relativos (vem do frontmatter, nunca do cliente).
fn plain_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\']) && !id.starts_with('-')
}

/// Tarefa ativa de maior prioridade (empate: spec e id em ordem) cuja worktree é real, está dentro do projeto e
/// registrada em `git worktree list`. `None` = sem tarefa ativa/worktree.
fn find_active_task(project_root: &Path) -> Option<(PathBuf, ActiveTask)> {
    let specs_rel = ".sdd/specs";
    safe_fs::check_chain(project_root, specs_rel).ok()?;
    let mut spec_dirs = real_children(&project_root.join(specs_rel), true);
    spec_dirs.sort();
    let base = safe_fs::read_regular(project_root, ".sdd/settings.yaml", safe_fs::MAX_READ_BYTES)
        .ok()
        .and_then(|raw| Settings::parse(&raw).ok())
        .map(|s| s.base_branch)
        .filter(|b| !b.is_empty() && !b.starts_with('-'))
        .unwrap_or_else(|| read_head_branch(project_root));
    let git = Git::new(project_root);
    let worktrees = git.worktree_list().ok()?;
    let mut best: Option<(u8, PathBuf, ActiveTask)> = None;
    for dir in spec_dirs {
        let tasks_rel = format!("{specs_rel}/{dir}/tasks");
        if safe_fs::check_chain(project_root, &tasks_rel).is_err() {
            continue;
        }
        let mut names: Vec<String> = real_children(&project_root.join(&tasks_rel), false)
            .into_iter()
            .filter(|n| n.ends_with(".md") && !n.starts_with('.'))
            .collect();
        names.sort();
        for name in names {
            let Ok(raw) = safe_fs::read_regular(
                project_root,
                &format!("{tasks_rel}/{name}"),
                safe_fs::MAX_READ_BYTES,
            ) else {
                continue;
            };
            let Ok(task) = Task::parse(&raw) else { continue };
            let meta = task.frontmatter;
            let Some(rank) = active_rank(meta.status) else { continue };
            if !plain_id(&meta.spec_id) || !plain_id(&meta.id) || best.as_ref().is_some_and(|(r, _, _)| *r <= rank) {
                continue;
            }
            let wt = read_root(project_root, &meta.spec_id, &meta.id);
            if wt == project_root {
                continue;
            }
            let Ok(want) = fs::canonicalize(&wt) else { continue };
            let registered = worktrees.iter().find(|w| fs::canonicalize(&w.path).is_ok_and(|p| p == want));
            let Some(branch) = registered.and_then(|w| w.branch.clone()) else { continue };
            if branch.starts_with('-') {
                continue;
            }
            best = Some((rank, wt, ActiveTask { id: meta.id, branch, base: base.clone() }));
        }
    }
    best.map(|(_, wt, task)| (wt, task))
}

/// Raiz do disco para a fonte (I/O: chamar em `blocking`). `None` = `no_active_task`.
fn resolve_root(project: &Project, source: Source) -> Option<Resolved> {
    match source {
        Source::Base => Some(Resolved { root: PathBuf::from(&project.path), task: None }),
        Source::Task => find_active_task(Path::new(&project.path))
            .map(|(root, task)| Resolved { root, task: Some(task) }),
    }
}

/// Alterados da tarefa: `base...branch` (commitados) e status da worktree (não commitados).
fn changed_paths(root: &Path, task: &ActiveTask) -> Vec<String> {
    let git = Git::new(root);
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Ok(paths) = git.changed_paths(&task.base, &task.branch) {
        set.extend(paths);
    }
    if let Ok(paths) = git.porcelain_paths(&[]) {
        set.extend(paths);
    }
    set.into_iter().filter(|p| !is_hidden(p)).take(MAX_LIST_PATHS).collect()
}

#[derive(Debug, Deserialize)]
pub struct FilesQuery {
    source: Option<String>,
    dir: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    source: Option<String>,
    path: Option<String>,
    reveal: Option<String>,
    diff: Option<String>,
}

/// Entrada da lista: `kind` é `file`, `symlink`, `dir` ou `ignored_dir`.
#[derive(Debug, Clone, Serialize)]
struct Entry {
    path: String,
    kind: &'static str,
}

#[derive(Debug)]
struct Listing {
    entries: Vec<Entry>,
    truncated: bool,
}

/// Cache da lista plana por raiz: `(chave, lista)`; chave = fonte + HEAD + mtime do índice.
type ListCache = Mutex<HashMap<PathBuf, (String, Instant, Arc<Listing>)>>;

fn cache() -> &'static ListCache {
    static CACHE: OnceLock<ListCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn safe_error(e: SafeFsError) -> ApiError {
    match e {
        SafeFsError::Missing => ApiError::new(StatusCode::NOT_FOUND, MSG_NOT_FOUND),
        SafeFsError::Io => ApiError::internal(MSG_IO),
        SafeFsError::Unsafe
        | SafeFsError::NotRegular
        | SafeFsError::TooLarge
        | SafeFsError::Invalid => ApiError::new(StatusCode::BAD_REQUEST, MSG_PATH),
    }
}

fn bad_path() -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, MSG_PATH)
}

/// `.git` em qualquer componente ou `.sdd/worktrees` (e abaixo) nunca são expostos.
fn is_hidden(rel: &str) -> bool {
    rel.split('/').any(|c| c == ".git")
        || rel == ".sdd/worktrees"
        || rel.starts_with(".sdd/worktrees/")
}

/// Valida um caminho relativo vindo da query: forma (split_rel), tamanho, controle e ocultos.
fn validate_rel(rel: &str) -> Result<(), ApiError> {
    if rel.len() > MAX_REL_LEN || rel.chars().any(char::is_control) {
        return Err(bad_path());
    }
    safe_fs::split_rel(rel).map_err(|_| bad_path())?;
    if is_hidden(rel) {
        return Err(bad_path());
    }
    Ok(())
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| ApiError::internal(MSG_INTERNAL))?
}

fn lock_cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, (String, Instant, Arc<Listing>)>> {
    cache().lock().unwrap_or_else(|e| e.into_inner())
}

/// Chave do cache: fonte, HEAD (ou `unborn`) e mtime do índice do git.
fn cache_key(source: Source, git: &Git) -> String {
    let head = git.rev_parse("HEAD").unwrap_or_else(|_| "unborn".to_string());
    let index_mtime = git
        .git_path("index")
        .and_then(|p| fs::symlink_metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{}|{head}|{index_mtime}", source.name())
}

/// Lista plana pelo git, com `ignored_dir`/`symlink` descobertos por `read_dir` só nas pastas
/// já conhecidas (sem varredura recursiva).
fn build_listing(root: &Path) -> Result<Listing, ApiError> {
    let git = Git::new(root);
    let paths = git
        .ls_files_visible()
        .map_err(|_| ApiError::new(StatusCode::CONFLICT, MSG_NO_GIT))?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut listed: HashSet<String> = HashSet::new();
    let mut known_dirs: HashSet<String> = HashSet::new();
    known_dirs.insert(String::new());
    let mut truncated = false;
    for path in paths {
        if is_hidden(&path) {
            continue;
        }
        let kind = match fs::symlink_metadata(root.join(&path)) {
            Ok(m) if m.file_type().is_symlink() => "symlink",
            Ok(m) if m.file_type().is_file() => "file",
            _ => continue,
        };
        if entries.len() >= MAX_LIST_PATHS {
            truncated = true;
            break;
        }
        let mut dir = path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            if !known_dirs.insert(parent.to_string()) {
                break;
            }
            dir = parent;
        }
        listed.insert(path.clone());
        entries.push(Entry { path, kind });
    }

    let mut dirs: Vec<&String> = known_dirs.iter().collect();
    dirs.sort();
    let mut extra: Vec<Entry> = Vec::new();
    for dir in dirs {
        let abs = if dir.is_empty() {
            root.to_path_buf()
        } else {
            match safe_fs::check_chain(root, dir) {
                Ok(p) => p,
                Err(_) => continue,
            }
        };
        let Ok(read) = fs::read_dir(&abs) else { continue };
        for child in read.flatten().take(MAX_DIR_SCAN) {
            let Ok(name) = child.file_name().into_string() else { continue };
            let rel = if dir.is_empty() { name } else { format!("{dir}/{name}") };
            if is_hidden(&rel) {
                continue;
            }
            let Ok(ft) = child.file_type() else { continue };
            if ft.is_dir() && !known_dirs.contains(&rel) {
                extra.push(Entry { path: rel, kind: "ignored_dir" });
            } else if ft.is_symlink() && !listed.contains(&rel) {
                extra.push(Entry { path: rel, kind: "symlink" });
            } else if ft.is_file() && !listed.contains(&rel) {
                // Arquivo regular ignorado pelo git (ex.: `.env`); o cliente esconde dotfiles.
                extra.push(Entry { path: rel, kind: "file" });
            }
        }
    }
    entries.extend(extra);
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    if entries.len() > MAX_LIST_PATHS {
        entries.truncate(MAX_LIST_PATHS);
        truncated = true;
    }
    Ok(Listing { entries, truncated })
}

fn cached_listing(source: Source, root: &Path, ttl: Duration) -> Result<Arc<Listing>, ApiError> {
    let git = Git::new(root);
    let key = cache_key(source, &git);
    if let Some((k, at, listing)) = lock_cache().get(root) {
        if *k == key && at.elapsed() < ttl {
            return Ok(Arc::clone(listing));
        }
    }
    let listing = Arc::new(build_listing(root)?);
    let mut map = lock_cache();
    if map.len() >= CACHE_MAX_ROOTS {
        map.clear();
    }
    map.insert(root.to_path_buf(), (key, Instant::now(), Arc::clone(&listing)));
    Ok(listing)
}

/// Conteúdo de uma pasta (tipicamente ignorada pelo git) via `read_dir` confinado.
fn list_dir(root: &Path, rel: &str) -> Result<Listing, ApiError> {
    let abs = safe_fs::check_chain(root, rel).map_err(safe_error)?;
    let meta = fs::symlink_metadata(&abs)
        .map_err(|e| safe_error(if e.kind() == std::io::ErrorKind::NotFound {
            SafeFsError::Missing
        } else {
            SafeFsError::Io
        }))?;
    if !meta.file_type().is_dir() {
        return Err(bad_path());
    }
    let read = fs::read_dir(&abs).map_err(|_| ApiError::internal(MSG_IO))?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut scan_cut = false;
    for (i, child) in read.flatten().enumerate() {
        if i >= MAX_DIR_SCAN {
            scan_cut = true;
            break;
        }
        let Ok(name) = child.file_name().into_string() else { continue };
        if name == ".git" {
            continue;
        }
        let path = format!("{rel}/{name}");
        if is_hidden(&path) {
            continue;
        }
        let Ok(ft) = child.file_type() else { continue };
        let kind = if ft.is_symlink() {
            "symlink"
        } else if ft.is_dir() {
            "dir"
        } else if ft.is_file() {
            "file"
        } else {
            continue;
        };
        entries.push(Entry { path, kind });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let truncated = scan_cut || entries.len() > MAX_DIR_ENTRIES;
    entries.truncate(MAX_DIR_ENTRIES);
    Ok(Listing { entries, truncated })
}

/// `GET /api/projects/:id/files?source=base|task[&dir=<rel>]`
pub async fn list_files(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(query): Query<FilesQuery>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let project = find_project(&state, &id).await?;
    let source = Source::parse(query.source.as_deref())?;
    if let Some(dir) = &query.dir {
        validate_rel(dir)?;
    }
    let dir = query.dir;
    let body = blocking(move || {
        let Some(resolved) = resolve_root(&project, source) else { return Ok(None) };
        let root = resolved.root;
        let value = match dir {
            Some(dir) => {
                let listing = list_dir(&root, &dir)?;
                json!({
                    "source": source.name(),
                    "dir": dir,
                    "entries": listing.entries,
                    "truncated": listing.truncated,
                })
            }
            None => {
                let listing = cached_listing(source, &root, LIST_TTL)?;
                let mut value = json!({
                    "source": source.name(),
                    "entries": listing.entries,
                    "truncated": listing.truncated,
                });
                if let (Some(task), Some(map)) = (&resolved.task, value.as_object_mut()) {
                    map.insert("task_id".into(), json!(task.id));
                    map.insert("changed".into(), json!(changed_paths(&root, task)));
                }
                value
            }
        };
        Ok(Some(value))
    })
    .await?;
    Ok(match body {
        Some(value) => Json(value).into_response(),
        None => no_active_task(),
    })
}

/// FNV-1a de 64 bits em hex (identidade do ETag; não é segurança).
fn fnv_hex(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// `If-None-Match` casa com o ETag (lista separada por vírgula, `W/` ou `*`).
fn etag_matches(header: Option<&str>, etag: &str) -> bool {
    header.is_some_and(|h| {
        h.split(',').map(str::trim).any(|t| t == "*" || t.trim_start_matches("W/") == etag)
    })
}

/// Troca o valor de linhas `CHAVE=valor` / `CHAVE: valor` por `•••`. `None` se não é atribuição.
fn mask_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let body = match trimmed.strip_prefix("export ") {
        Some(rest) => rest.trim_start(),
        None => trimmed,
    };
    if !body.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    let key_end = body.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))?;
    let rest = body[key_end..].trim_start_matches([' ', '\t']);
    if !rest.starts_with(['=', ':']) {
        return None;
    }
    let after = rest[1..].trim_start_matches([' ', '\t']);
    if after.trim().is_empty() {
        return None;
    }
    // `after` é sufixo de `line`: o prefixo preservado vai até o início do valor.
    let keep = &line[..line.len() - after.len()];
    Some(format!("{keep}{MASK}"))
}

/// Aplica [`mask_line`] a cada linha preservando as quebras. Devolve `(texto, houve_máscara)`.
fn mask_secrets(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut masked = false;
    for raw in text.split_inclusive('\n') {
        let (line, eol) = match raw.strip_suffix("\r\n") {
            Some(l) => (l, "\r\n"),
            None => match raw.strip_suffix('\n') {
                Some(l) => (l, "\n"),
                None => (raw, ""),
            },
        };
        match mask_line(line) {
            Some(m) => {
                masked = true;
                out.push_str(&m);
            }
            None => out.push_str(line),
        }
        out.push_str(eol);
    }
    (out, masked)
}

enum FileOutcome {
    NoActiveTask,
    NotModified(String),
    Body(String, serde_json::Value),
}

fn read_file(
    root: &Path,
    source: Source,
    rel: &str,
    reveal: bool,
    if_none_match: Option<&str>,
    secrets: &SecretSet,
) -> Result<FileOutcome, ApiError> {
    let abs = safe_fs::check_chain(root, rel).map_err(safe_error)?;
    let meta = fs::symlink_metadata(&abs).map_err(|e| {
        safe_error(if e.kind() == std::io::ErrorKind::NotFound { SafeFsError::Missing } else { SafeFsError::Io })
    })?;
    if !meta.file_type().is_file() {
        return Err(bad_path());
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let ignored = Git::new(root).is_ignored(rel);
    let etag = format!(
        "\"{}\"",
        fnv_hex(&format!("{}|{rel}|{}|{mtime}|{reveal}|{ignored}", source.name(), meta.len()))
    );
    if etag_matches(if_none_match, &etag) {
        return Ok(FileOutcome::NotModified(etag));
    }
    let read = safe_fs::read_bounded(root, rel, MAX_FILE_BYTES).map_err(safe_error)?;
    let Some(data) = read.data else {
        return Ok(FileOutcome::Body(etag, json!({ "kind": "too_large", "size": read.size })));
    };
    let probe = &data[..data.len().min(BINARY_PROBE)];
    let text = if probe.contains(&0) { None } else { String::from_utf8(data).ok() };
    let Some(text) = text else {
        return Ok(FileOutcome::Body(etag, json!({ "kind": "binary", "size": read.size })));
    };
    let (text, masked) = if !reveal && ignored {
        mask_secrets(&text)
    } else {
        (text, false)
    };
    let content = scrub_text(secrets, &text);
    Ok(FileOutcome::Body(
        etag,
        json!({ "kind": "text", "size": read.size, "content": content, "masked": masked }),
    ))
}

/// Diff unificado de um arquivo da tarefa: `base` contra o estado atual da worktree (inclui o
/// não commitado). Arquivo novo não rastreado vira todo adicionado (leitura confinada); binário
/// devolve `{kind:"binary"}`. Teto de 512 KiB e scrub.
fn read_diff(
    root: &Path,
    task: &ActiveTask,
    rel: &str,
    secrets: &SecretSet,
) -> Result<serde_json::Value, ApiError> {
    safe_fs::check_chain(root, rel).map_err(safe_error)?;
    let git = Git::new(root);
    let text = if git.is_tracked(rel) {
        git.diff_file_worktree(&task.base, rel)
            .map_err(|_| ApiError::new(StatusCode::CONFLICT, MSG_NO_GIT))?
    } else if git.is_ignored(rel) {
        String::new()
    } else {
        match safe_fs::read_bounded(root, rel, MAX_FILE_BYTES) {
            Err(SafeFsError::Missing) => String::new(),
            Err(e) => return Err(safe_error(e)),
            Ok(read) => {
                let Some(data) = read.data else {
                    return Ok(json!({ "kind": "too_large", "size": read.size }));
                };
                let probe = &data[..data.len().min(BINARY_PROBE)];
                let text = if probe.contains(&0) { None } else { String::from_utf8(data).ok() };
                let Some(text) = text else {
                    return Ok(json!({ "kind": "binary", "size": read.size }));
                };
                added_hunk(rel, &text)
            }
        }
    };
    if text.lines().any(|l| l.starts_with("Binary files ") && l.ends_with(" differ")) {
        return Ok(json!({ "kind": "binary", "size": text.len() }));
    }
    if text.len() as u64 > MAX_FILE_BYTES {
        return Ok(json!({ "kind": "too_large", "size": text.len() }));
    }
    Ok(json!({ "kind": "diff", "size": text.len(), "content": scrub_text(secrets, &text) }))
}

/// Diff unificado de arquivo novo: todas as linhas como adição.
fn added_hunk(rel: &str, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = format!(
        "diff --git a/{rel} b/{rel}\nnew file mode 100644\n--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for line in &lines {
        out.push('+');
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    }
    out
}

/// `GET /api/projects/:id/file?source=base&path=<rel>[&reveal=1]`
pub async fn get_file(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let project = find_project(&state, &id).await?;
    let source = Source::parse(query.source.as_deref())?;
    let rel = query.path.ok_or_else(bad_path)?;
    validate_rel(&rel)?;
    let reveal = matches!(query.reveal.as_deref(), Some("1" | "true"));
    let diff = matches!(query.diff.as_deref(), Some("1" | "true"));
    if diff && source != Source::Task {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, MSG_SOURCE));
    }
    let secrets = state.secret_set().await;
    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let outcome = blocking(move || {
        let Some(resolved) = resolve_root(&project, source) else {
            return Ok(FileOutcome::NoActiveTask);
        };
        match (diff, &resolved.task) {
            (true, Some(task)) => {
                let value = read_diff(&resolved.root, task, &rel, &secrets)?;
                Ok(FileOutcome::Body(String::new(), value))
            }
            _ => read_file(&resolved.root, source, &rel, reveal, inm.as_deref(), &secrets),
        }
    })
    .await?;
    let (status, etag, body) = match outcome {
        FileOutcome::NoActiveTask => return Ok(no_active_task()),
        FileOutcome::NotModified(etag) => (StatusCode::NOT_MODIFIED, etag, None),
        FileOutcome::Body(etag, value) => (StatusCode::OK, etag, Some(value)),
    };
    let mut response = match body {
        Some(value) => Json(value).into_response(),
        None => status.into_response(),
    };
    if !etag.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&etag) {
            response.headers_mut().insert(header::ETAG, v);
        }
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}

#[cfg(test)]
#[path = "file_explorer_tests.rs"]
mod tests;
