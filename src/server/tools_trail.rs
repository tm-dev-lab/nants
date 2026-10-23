//! Leitura da trilha de ações dos agentes (`.sdd/specs/<spec>/tools.jsonl`, spec 085) para a UI.
//!
//! - `GET /api/projects/:id/specs/:spec_id/tools?task=&before=&limit=` devolve uma página em ordem
//!   cronológica, paginada para trás: `seq` é a posição (1, 2, …) da linha no arquivo, e `before`
//!   pede só as linhas com `seq < before`. Sem `before`, a página é a janela final.
//! - `task` filtra pelo `task_id`; `FINAL` é a verificação final (gravada com `task_id: "FINAL"`).
//! - A leitura passa por `safe_fs` (sem symlink, só arquivo regular, com teto de bytes). Arquivo
//!   ausente, inseguro ou grande demais vira lista vazia; linhas inválidas são ignoradas.
//! - O texto já foi redigido na gravação, mas passa de novo pela redação do servidor.
//!
//! A contagem por tarefa (`counts`) alimenta o link "Ações (N)" do cartão no estado do projeto e
//! fica em cache por caminho, tamanho e mtime: o arquivo só é relido quando muda.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::error::ApiError;
use super::events::{
    message_params, message_text, normalize_id_field, valid_message_code, validate_project_id,
    SecretSet, MAX_TOOLS_TRAIL_BYTES, TOOLS_TRAIL_FILE,
};
use super::projects::find_project;
use super::safe_fs;
use super::spec_result::validate_spec_id;
use super::state::AppState;

const SPECS_REL: &str = ".sdd/specs";
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;
/// Teto de leitura: o teto de gravação com folga (arquivo editado à mão maior que isso é ignorado).
const READ_MAX_BYTES: u64 = MAX_TOOLS_TRAIL_BYTES + 64 * 1024;
/// Máximo de arquivos lembrados no cache de contagens (ao passar, o cache é esvaziado).
const CACHE_MAX_ENTRIES: usize = 256;
const MAX_TASK_LEN: usize = 64;

/// Uma linha válida da trilha, com a posição no arquivo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailItem {
    pub seq: u64,
    pub ts: u64,
    pub agent: String,
    pub task_id: Option<String>,
    pub kind: String,
    pub text: String,
    pub stage: Option<String>,
    /// `code` da etapa e `code`/`params` do rótulo (spec 102); linhas antigas não têm.
    pub stage_code: Option<String>,
    pub code: Option<String>,
    pub params: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Deserialize)]
struct RawLine {
    ts: u64,
    agent: String,
    #[serde(default)]
    task_id: Option<String>,
    kind: String,
    text: String,
    #[serde(default)]
    stage: Option<String>,
    #[serde(default)]
    stage_code: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    params: Option<serde_json::Map<String, Value>>,
}

/// Página de leitura: itens em ordem cronológica, total do filtro e se há anteriores.
#[derive(Debug, PartialEq, Eq)]
pub struct TrailPage {
    pub items: Vec<TrailItem>,
    pub total: usize,
    pub has_more: bool,
}

/// Caminho relativo ao projeto da trilha de uma spec.
pub fn trail_rel(spec_id: &str) -> String {
    format!("{SPECS_REL}/{spec_id}/{TOOLS_TRAIL_FILE}")
}

/// Bytes da trilha via `safe_fs`. Ausente, inseguro, especial ou grande demais → `None`.
fn read_bytes(root: &Path, spec_id: &str) -> Option<Vec<u8>> {
    safe_fs::read_bounded(root, &trail_rel(spec_id), READ_MAX_BYTES)
        .ok()
        .and_then(|read| read.data)
}

/// Linhas válidas, numeradas pela posição no arquivo (a contagem inclui as inválidas, para o
/// `seq` ser estável entre leituras de um arquivo que só cresce).
fn parse_items(bytes: &[u8]) -> Vec<TrailItem> {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .zip(1u64..)
        .filter_map(|(line, seq)| {
            let raw: RawLine = serde_json::from_str(line).ok()?;
            Some(TrailItem {
                seq,
                ts: raw.ts,
                agent: raw.agent,
                task_id: raw.task_id,
                kind: raw.kind,
                text: raw.text,
                stage: raw.stage,
                stage_code: raw.stage_code,
                code: raw.code,
                params: raw.params,
            })
        })
        .collect()
}

/// Lê uma página da trilha. `task` filtra por `task_id`; `before` exclusivo; `limit` ≥ 1.
pub fn read_page(
    root: &Path,
    spec_id: &str,
    task: Option<&str>,
    before: Option<u64>,
    limit: usize,
) -> TrailPage {
    let Some(bytes) = read_bytes(root, spec_id) else {
        return TrailPage {
            items: Vec::new(),
            total: 0,
            has_more: false,
        };
    };
    let matching: Vec<TrailItem> = parse_items(&bytes)
        .into_iter()
        .filter(|item| task.is_none_or(|t| item.task_id.as_deref() == Some(t)))
        .collect();
    let total = matching.len();
    let window: Vec<TrailItem> = matching
        .into_iter()
        .filter(|item| before.is_none_or(|b| item.seq < b))
        .collect();
    let start = window.len().saturating_sub(limit.max(1));
    TrailPage {
        has_more: start > 0,
        items: window.into_iter().skip(start).collect(),
        total,
    }
}

/// Assinatura do arquivo usada pelo cache: tamanho e mtime.
type Signature = (u64, Option<SystemTime>);

/// Caminho e assinatura da trilha (sem symlink, só arquivo regular); senão `None`.
fn trail_signature(root: &Path, spec_id: &str) -> Option<(PathBuf, Signature)> {
    let path = safe_fs::check_chain(root, &trail_rel(spec_id)).ok()?;
    let meta = fs::symlink_metadata(&path).ok()?;
    meta.file_type().is_file().then(|| (path, (meta.len(), meta.modified().ok())))
}

type Marks = Vec<(String, u64)>;

static TIMES_CACHE: OnceLock<Mutex<HashMap<PathBuf, (Signature, Marks)>>> = OnceLock::new();

/// Instantes (`ts`, segundos Unix) das ações de cada `task_id` da trilha, na ordem do arquivo.
/// Alimenta o tempo ativo por tarefa da rota de uso (086/T06). Ausente ou inseguro → vazio.
/// Fica em cache por caminho, tamanho e mtime: a trilha só é relida quando muda.
pub fn task_timestamps(root: &Path, spec_id: &str) -> Vec<(String, u64)> {
    let Some((path, signature)) = trail_signature(root, spec_id) else {
        return Vec::new();
    };
    let cache = TIMES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, marks)) = guard.get(&path).filter(|(sig, _)| *sig == signature) {
            return marks.clone();
        }
    }
    let marks: Vec<(String, u64)> = read_bytes(root, spec_id)
        .map(|bytes| {
            parse_items(&bytes)
                .into_iter()
                .filter_map(|item| item.task_id.map(|task| (task, item.ts)))
                .collect()
        })
        .unwrap_or_default();
    let mut guard = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.len() >= CACHE_MAX_ENTRIES {
        guard.clear();
    }
    guard.insert(path, (signature, marks.clone()));
    marks
}

struct CachedCounts {
    signature: Signature,
    counts: BTreeMap<String, usize>,
}

static COUNTS_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedCounts>>> = OnceLock::new();

fn counts_cache() -> &'static Mutex<HashMap<PathBuf, CachedCounts>> {
    COUNTS_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Número de ações por `task_id` (chave já normalizada como o id da tarefa no estado). Arquivo
/// ausente ou inseguro → vazio. Relê só quando tamanho ou mtime mudam.
pub fn counts(root: &Path, spec_id: &str, secrets: &SecretSet) -> BTreeMap<String, usize> {
    let rel = trail_rel(spec_id);
    let Ok(path) = safe_fs::check_chain(root, &rel) else {
        return BTreeMap::new();
    };
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return BTreeMap::new();
    };
    if !meta.file_type().is_file() {
        return BTreeMap::new();
    }
    let signature: Signature = (meta.len(), meta.modified().ok());
    {
        let cache = counts_cache().lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = cache.get(&path).filter(|c| c.signature == signature) {
            return hit.counts.clone();
        }
    }
    let mut counts = BTreeMap::new();
    if let Some(bytes) = read_bytes(root, spec_id) {
        for item in parse_items(&bytes) {
            if let Some(task) = item.task_id {
                *counts.entry(normalize_id_field(secrets, &task)).or_insert(0) += 1;
            }
        }
    }
    let mut cache = counts_cache().lock().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= CACHE_MAX_ENTRIES {
        cache.clear();
    }
    cache.insert(
        path,
        CachedCounts {
            signature,
            counts: counts.clone(),
        },
    );
    counts
}

/// A spec existe quando `spec.md` é arquivo regular sem symlink no caminho.
fn spec_exists(root: &Path, spec_id: &str) -> bool {
    let rel = format!("{SPECS_REL}/{spec_id}/spec.md");
    safe_fs::check_chain(root, &rel)
        .ok()
        .and_then(|path| fs::symlink_metadata(path).ok())
        .is_some_and(|meta| meta.is_file())
}

/// Query de `GET .../tools`. Valores brutos: a validação devolve 422 estático.
#[derive(Debug, Deserialize)]
pub struct ToolsQuery {
    task: Option<String>,
    before: Option<String>,
    limit: Option<String>,
}

/// `GET /api/projects/:id/specs/:spec_id/tools?task=&before=&limit=`: somente leitura.
pub async fn get_tools(
    State(state): State<AppState>,
    AxPath((id, spec_id)): AxPath<(String, String)>,
    Query(query): Query<ToolsQuery>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    validate_spec_id(&spec_id)?;
    let task = parse_task(query.task.as_deref())?;
    let before = parse_before(query.before.as_deref())?;
    let limit = parse_limit(query.limit.as_deref())?;
    let project = find_project(&state, &id).await?;
    let secrets = state.secret_set().await;
    let root = PathBuf::from(&project.path);
    let page = tokio::task::spawn_blocking(move || -> Result<TrailPage, ApiError> {
        if !spec_exists(&root, &spec_id) {
            return Err(ApiError::coded(StatusCode::NOT_FOUND, "spec_not_found", Map::new()));
        }
        Ok(read_page(&root, &spec_id, task.as_deref(), before, limit))
    })
    .await
    .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))??;
    Ok(Json(page_view(&secrets, page)).into_response())
}

/// `task` vazio é ignorado; senão `[a-zA-Z0-9_-]`, até 64 caracteres.
fn parse_task(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    match raw {
        None | Some("") => Ok(None),
        Some(text)
            if text.len() <= MAX_TASK_LEN
                && text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') =>
        {
            Ok(Some(text.to_string()))
        }
        Some(_) => Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "task_invalid", Map::new())),
    }
}

fn parse_before(raw: Option<&str>) -> Result<Option<u64>, ApiError> {
    match raw {
        None => Ok(None),
        Some(text) => text
            .parse::<u64>()
            .ok()
            .filter(|n| *n >= 1)
            .map(Some)
            .ok_or_else(|| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "before_invalid", Map::new())),
    }
}

fn parse_limit(raw: Option<&str>) -> Result<usize, ApiError> {
    match raw {
        None => Ok(DEFAULT_LIMIT),
        Some(text) => text
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or_else(|| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "limit_invalid", Map::new())),
    }
}

/// Saída do GET: redação de novo em texto, agente, tarefa e etapa.
fn page_view(secrets: &SecretSet, page: TrailPage) -> Value {
    let items: Vec<Value> = page
        .items
        .into_iter()
        .map(|item| {
            let mut row = json!({
                "seq": item.seq,
                "ts": item.ts,
                "agent": normalize_id_field(secrets, &item.agent),
                "task_id": item.task_id.map(|t| normalize_id_field(secrets, &t)),
                "kind": normalize_id_field(secrets, &item.kind),
                "text": message_text(secrets, &item.text),
            });
            if let Some(stage) = item.stage {
                row["stage"] = Value::String(message_text(secrets, &stage));
            }
            // Spec 102: só `code` no formato fixo; `params` saneados de novo.
            if let Some(code) = item.stage_code.filter(|c| valid_message_code(c)) {
                row["stage_code"] = Value::String(code);
            }
            if let Some(code) = item.code.filter(|c| valid_message_code(c)) {
                row["code"] = Value::String(code);
                row["params"] = Value::Object(message_params(secrets, &item.params.unwrap_or_default()));
            }
            row
        })
        .collect();
    json!({
        "items": items,
        "total": page.total,
        "has_more": page.has_more,
    })
}

#[cfg(test)]
#[path = "tools_trail_tests.rs"]
mod tests;
