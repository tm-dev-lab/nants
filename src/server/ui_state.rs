//! Estado da interface salvo no servidor (spec 093 T01, decisão 6).
//!
//! - `GET/PUT /api/ui-state` (global) e `GET/PUT /api/projects/:id/ui-state` (por projeto);
//! - envelope `{version: 1, rev, data}`; sem arquivo, `rev: 0` e `data: {}`;
//! - o PUT recebe `{base_rev, section, value}` e grava só aquela seção (chave `[a-z0-9_.-]` de 1 a 64
//!   caracteres; `.` separa seções aninhadas, ex.: `layouts.executing.current`). `base_rev` diferente
//!   do atual → 409 com o envelope atual. O servidor não interpreta `value`;
//! - arquivos em `<dados>/ui/global.json` e `<dados>/ui/projects/<id>.json`, teto de 64 KiB por
//!   documento, gravação atômica (temporário + `rename`) sem seguir symlink;
//! - o PUT por projeto confere a existência do projeto sob o read lock da configuração e grava ainda
//!   com ele; a remoção do projeto (write lock) apaga o arquivo, então um PUT tardio vira 404 e nunca
//!   recria o arquivo.
//!
//! Mensagens de erro são estáticas: nunca ecoam caminhos nem o conteúdo enviado.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::error::ApiError;
use super::events::validate_project_id;
use super::safe_fs::{self, SafeFsError, O_NOFOLLOW};
use super::state::AppState;

/// Teto de cada documento (e do corpo do PUT): 64 KiB.
pub const UI_STATE_MAX_BYTES: usize = 64 * 1024;
/// Versão do envelope.
pub const UI_STATE_VERSION: u32 = 1;
/// Tamanho máximo da chave de seção.
const MAX_SECTION_LEN: usize = 64;
/// Pasta do estado da UI sob o diretório de dados.
const UI_DIR_REL: &str = "ui";
/// Pasta dos documentos por projeto.
const PROJECTS_DIR_REL: &str = "ui/projects";
/// Documento global.
const GLOBAL_FILE: &str = "global.json";

/// Ponto de pausa de teste no PUT por projeto, antes do read lock da configuração.
pub const PAUSE_UI_BEFORE_LOOKUP: &str = "ui_state_before_lookup";
/// Ponto de pausa de teste no PUT por projeto, com o projeto encontrado e o read lock seguro.
pub const PAUSE_UI_AFTER_LOOKUP: &str = "ui_state_after_lookup";


/// Envelope devolvido pelo GET/PUT (e pelo 409).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiEnvelope {
    pub version: u32,
    pub rev: u64,
    pub data: Map<String, Value>,
}

impl UiEnvelope {
    fn empty() -> Self {
        Self { version: UI_STATE_VERSION, rev: 0, data: Map::new() }
    }
}

/// Corpo do PUT.
#[derive(Deserialize)]
struct PutInput {
    base_rev: u64,
    section: String,
    value: Value,
}

/// Documento: global ou de um projeto (id já validado).
#[derive(Debug, Clone)]
enum Doc {
    Global,
    Project(String),
}

impl Doc {
    /// (pasta relativa ao diretório de dados, nome do arquivo).
    fn location(&self) -> (&'static str, String) {
        match self {
            Doc::Global => (UI_DIR_REL, GLOBAL_FILE.to_string()),
            Doc::Project(id) => (PROJECTS_DIR_REL, format!("{id}.json")),
        }
    }
}

/// Falha do armazenamento. Nunca carrega caminhos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreError {
    Unsafe,
    Io,
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Unsafe => ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "ui_state_unsafe", Map::new()),
            StoreError::Io => ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "ui_state_write_failed", Map::new()),
        }
    }
}

/// Seção válida: 1–64 caracteres `[a-z0-9_.-]`, partes separadas por `.` não vazias.
pub fn valid_section(section: &str) -> bool {
    !section.is_empty()
        && section.len() <= MAX_SECTION_LEN
        && section.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
        && section.split('.').all(|part| !part.is_empty())
}

/// Lê o documento. Ausente, ilegível como envelope ou acima do teto → envelope vazio (`rev: 0`):
/// o próximo PUT o substitui. Symlink ou arquivo especial → erro (nunca é sobrescrito).
fn read_doc(data_dir: &Path, doc: &Doc) -> Result<UiEnvelope, StoreError> {
    let (dir, file) = doc.location();
    match safe_fs::read_regular(data_dir, &format!("{dir}/{file}"), UI_STATE_MAX_BYTES as u64) {
        Ok(text) => Ok(parse_envelope(&text).unwrap_or_else(UiEnvelope::empty)),
        Err(SafeFsError::Missing | SafeFsError::TooLarge | SafeFsError::Invalid) => Ok(UiEnvelope::empty()),
        Err(SafeFsError::Unsafe | SafeFsError::NotRegular) => Err(StoreError::Unsafe),
        Err(SafeFsError::Io) => Err(StoreError::Io),
    }
}

fn parse_envelope(text: &str) -> Option<UiEnvelope> {
    serde_json::from_str::<UiEnvelope>(text).ok().filter(|e| e.version == UI_STATE_VERSION)
}

/// Grava `bytes` como o documento, de forma atômica: temporário exclusivo (`O_NOFOLLOW`, 0600) na
/// mesma pasta e `rename` por cima. Nenhum componente pode ser symlink; um alvo que não é arquivo
/// regular é recusado.
fn write_doc(data_dir: &Path, doc: &Doc, bytes: &[u8]) -> Result<(), StoreError> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(data_dir).map_err(|_| StoreError::Io)?;
    let (dir_rel, file) = doc.location();
    private_dir(data_dir, UI_DIR_REL)?;
    if dir_rel != UI_DIR_REL {
        private_dir(data_dir, dir_rel)?;
    }
    let dir = safe_fs::check_chain(data_dir, dir_rel).map_err(map_safe)?;
    let target = dir.join(&file);
    match fs::symlink_metadata(&target) {
        Ok(m) if m.file_type().is_file() => {}
        Ok(_) => return Err(StoreError::Unsafe),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(StoreError::Io),
    }
    let suffix = super::approvals::random_hex_id().map_err(|_| StoreError::Io)?;
    let tmp = dir.join(format!(".{file}.{suffix}.tmp"));
    let written = write_tmp(&tmp, bytes).and_then(|()| fs::rename(&tmp, &target).map_err(|_| StoreError::Io));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Garante `rel` sob o diretório de dados como diretório real (sem symlink em nenhum componente),
/// criado com 0700 e restrito a 0700 se já existia mais aberto (como os overrides do Compose).
fn private_dir(data_dir: &Path, rel: &str) -> Result<(), StoreError> {
    let path = safe_fs::check_chain(data_dir, rel).map_err(map_safe)?;
    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(StoreError::Io),
    }
    safe_fs::ensure_dirs(data_dir, rel).map_err(map_safe)?;
    let meta = fs::symlink_metadata(&path).map_err(|_| StoreError::Io)?;
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|_| StoreError::Io)?;
    }
    Ok(())
}

fn write_tmp(tmp: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW)
        .open(tmp)
        .map_err(|_| StoreError::Io)?;
    f.write_all(bytes).map_err(|_| StoreError::Io)?;
    f.sync_all().map_err(|_| StoreError::Io)
}

fn map_safe(e: SafeFsError) -> StoreError {
    match e {
        SafeFsError::Unsafe | SafeFsError::NotRegular => StoreError::Unsafe,
        _ => StoreError::Io,
    }
}

/// Apaga o documento do projeto removido (chamado sob o write lock da remoção). Ausente é sucesso;
/// um symlink no lugar do arquivo é removido como link (o alvo nunca é tocado). Falha só vira aviso.
pub fn remove_project_doc(data_dir: Option<&Path>, project_id: &str) {
    let Some(data_dir) = data_dir else {
        return;
    };
    if validate_project_id(project_id).is_err() {
        return;
    }
    let Ok(dir) = safe_fs::check_chain(data_dir, PROJECTS_DIR_REL) else {
        eprintln!("aviso: estado da interface do projeto removido não foi apagado (pasta insegura)");
        return;
    };
    let path = dir.join(format!("{project_id}.json"));
    match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() || m.file_type().is_symlink() => {
            if fs::remove_file(&path).is_err() {
                eprintln!("aviso: estado da interface do projeto removido não foi apagado");
            }
        }
        Ok(_) => eprintln!("aviso: estado da interface do projeto removido não é arquivo regular"),
        Err(_) => {}
    }
}

/// Aplica `value` em `section` (com `.` = aninhado). Um nível intermediário que não é objeto é
/// substituído por um objeto: o conteúdo é do cliente e o servidor não o interpreta.
fn set_section(data: &mut Map<String, Value>, section: &str, value: Value) {
    let parts: Vec<&str> = section.split('.').collect();
    let Some((last, path)) = parts.split_last() else {
        return;
    };
    let mut cur = data;
    for part in path {
        let slot = cur.entry((*part).to_string()).or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        let Value::Object(next) = slot else {
            return;
        };
        cur = next;
    }
    cur.insert((*last).to_string(), value);
}

fn parse_put(body: &Bytes) -> Result<PutInput, ApiError> {
    let input: PutInput = serde_json::from_slice(body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "ui_state_body_invalid", Map::new())
        } else {
            ApiError::coded(StatusCode::BAD_REQUEST, "json_malformed", Map::new())
        }
    })?;
    if !valid_section(&input.section) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "ui_section_invalid", Map::new()));
    }
    Ok(input)
}

fn data_dir_of(state: &AppState) -> Result<PathBuf, ApiError> {
    state.data_dir().map(Path::to_path_buf).ok_or_else(|| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "data_dir_unavailable", Map::new()))
}

/// Lê e grava sob o lock do estado da UI. Devolve 200 com o envelope novo ou 409 com o atual.
async fn apply_put(state: &AppState, data_dir: PathBuf, doc: Doc, input: PutInput) -> Result<Response, ApiError> {
    let _write = state.ui_state_lock().lock().await;
    let job = move || -> Result<Result<UiEnvelope, UiEnvelope>, ApiError> {
        let current = read_doc(&data_dir, &doc)?;
        if current.rev != input.base_rev {
            return Ok(Err(current));
        }
        let mut next = current;
        set_section(&mut next.data, &input.section, input.value);
        next.rev = next.rev.saturating_add(1);
        next.version = UI_STATE_VERSION;
        let bytes = serde_json::to_vec(&next).map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "ui_state_write_failed", Map::new()))?;
        if bytes.len() > UI_STATE_MAX_BYTES {
            return Err(ApiError::coded(StatusCode::PAYLOAD_TOO_LARGE, "ui_state_too_large", Map::new()));
        }
        write_doc(&data_dir, &doc, &bytes)?;
        Ok(Ok(next))
    };
    let outcome = tokio::task::spawn_blocking(job).await.map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))??;
    Ok(match outcome {
        Ok(envelope) => Json(envelope).into_response(),
        Err(current) => conflict(current),
    })
}

/// 409 com o envelope atual (mais `error`, para os clientes que só leem esse campo).
fn conflict(current: UiEnvelope) -> Response {
    let mut body = serde_json::to_value(&current).unwrap_or_else(|_| Value::Object(Map::new()));
    if let Some(obj) = body.as_object_mut() {
        obj.insert("error".into(), Value::String("ui_state_conflict".into()));
    }
    (StatusCode::CONFLICT, Json(body)).into_response()
}

async fn read_blocking(data_dir: PathBuf, doc: Doc) -> Result<UiEnvelope, ApiError> {
    tokio::task::spawn_blocking(move || read_doc(&data_dir, &doc).map_err(ApiError::from))
        .await
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))?
}

/// `GET /api/ui-state`
pub async fn get_global(State(state): State<AppState>) -> Result<Response, ApiError> {
    let data_dir = data_dir_of(&state)?;
    Ok(Json(read_blocking(data_dir, Doc::Global).await?).into_response())
}

/// `PUT /api/ui-state` com `{base_rev, section, value}`.
pub async fn put_global(State(state): State<AppState>, body: Bytes) -> Result<Response, ApiError> {
    let input = parse_put(&body)?;
    let data_dir = data_dir_of(&state)?;
    apply_put(&state, data_dir, Doc::Global, input).await
}

/// `GET /api/projects/:id/ui-state`: 404 sem o projeto.
pub async fn get_project(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let data_dir = data_dir_of(&state)?;
    let cfg = state.config().read().await;
    if !cfg.projects.iter().any(|p| p.id == id) {
        return Err(ApiError::coded(StatusCode::NOT_FOUND, "project_not_found", Map::new()));
    }
    let envelope = read_blocking(data_dir, Doc::Project(id.clone())).await;
    drop(cfg);
    Ok(Json(envelope?).into_response())
}

/// `PUT /api/projects/:id/ui-state`: confere o projeto e grava sob o mesmo read lock da
/// configuração. A remoção pede o write lock, então ou ela espera esta gravação terminar (e apaga o
/// arquivo depois), ou já terminou e esta requisição vê 404.
pub async fn put_project(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let input = parse_put(&body)?;
    let data_dir = data_dir_of(&state)?;
    state.pause(PAUSE_UI_BEFORE_LOOKUP).await;
    let cfg = state.config().read().await;
    if !cfg.projects.iter().any(|p| p.id == id) {
        return Err(ApiError::coded(StatusCode::NOT_FOUND, "project_not_found", Map::new()));
    }
    state.pause(PAUSE_UI_AFTER_LOOKUP).await;
    let result = apply_put(&state, data_dir, Doc::Project(id.clone()), input).await;
    drop(cfg);
    result
}

#[cfg(test)]
#[path = "ui_state_tests.rs"]
mod tests;
