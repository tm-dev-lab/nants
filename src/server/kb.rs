//! API da base de conhecimento do projeto (spec 134/T01). O armazenamento fica em [`crate::kb`].
//!
//! Rotas, sob `/api/projects/:id`:
//! - `GET /kb`: lista os documentos e diz se há visão geral;
//! - `POST /kb/files?name=<nome>`: corpo = bytes do arquivo, `Content-Type:
//!   application/octet-stream` (exige preflight de CORS, como o JSON das outras rotas). Até
//!   [`kb::MAX_UPLOAD_BYTES`] (limite de corpo próprio da rota; acima, 413). Aceita md, txt e pdf;
//!   recusa nomes e conteúdos de segredo e duplicatas pelo `sha256`;
//! - `POST /kb/links`: `{ "url": "..." }`, registra o link com `status: pending`; a fila de
//!   extração ([`super::kb_queue`]) o lê uma vez;
//! - `GET /kb/docs/:doc`: a entrada, o corpo da nota e as seções (`sections`, com a âncora
//!   pública da citação e a linha do cabeçalho no corpo), redigidos;
//! - `PATCH /kb/docs/:doc`: `{ "role"?, "description"?, "applies_to"? }`;
//! - `DELETE /kb/docs/:doc`;
//! - `GET` e `PUT /kb/overview`: `{ "text": "..." }`, até [`kb::MAX_OVERVIEW_BYTES`].
//!
//! Toda gravação garante antes a linha [`kb::KB_EXCLUDE_LINE`] no `.git/info/exclude`: se o
//! exclude não pode ser gravado com segurança, nada é gravado. Todo erro tem `code`; a falha de
//! I/O da base é `kb_io_failed`.
//!
//! Spec 134/T02: PDF e link entram na fila de extração; cada mudança publica `kb_updated`. A
//! listagem devolve à fila o que ficou `pending` ou `extracting` (servidor reiniciado no meio) e
//! não cai por uma visão geral inválida editada no disco (`has_overview: false` e
//! `overview_invalid: true`).
//!
//! Spec 134/T06: cada documento da listagem traz `indicators` (`[{code, params}]`, C-24),
//! calculados sem modelo por [`kb::indicators`]; a interface traduz.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::config_api::parse_body;
use super::error::{params, ApiError};
use super::events::{validate_project_id, SecretSet};
use super::kb_queue;
use super::safe_fs::SafeFsError;
use super::projects::{ensure_local_excludes, find_project, now_rfc3339, safe_error, scrub_text};
use super::state::AppState;
use crate::kb::indicators::Indicator;
use crate::kb::{self, Entry, KbError};

/// `Content-Type` do corpo do envio de arquivo.
const UPLOAD_CONTENT_TYPE: &str = "application/octet-stream";

/// `true` para `POST /api/projects/<id>/kb/files`: a rota com corpo binário.
pub fn is_upload_path(path: &str) -> bool {
    path.strip_prefix("/api/projects/")
        .and_then(|rest| rest.strip_suffix("/kb/files"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// `Content-Type: application/octet-stream` (parâmetros ignorados), para o middleware.
pub fn is_upload_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case(UPLOAD_CONTENT_TYPE))
}

fn internal() -> ApiError {
    ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new())
}

/// Erro da base como erro da API, sempre com código.
fn kb_error(e: KbError, secrets: &SecretSet) -> ApiError {
    match e {
        KbError::Fs(SafeFsError::Io | SafeFsError::Missing) => {
            ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "kb_io_failed", Map::new())
        }
        KbError::Fs(fs) => safe_error(fs),
        KbError::Duplicate(id) => {
            ApiError::coded(StatusCode::CONFLICT, "kb_duplicate", params([("id", json!(scrub_text(secrets, &id)))]))
        }
        KbError::SecretName => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_secret_name", Map::new()),
        KbError::SecretContent => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_secret_content", Map::new()),
        KbError::Unsupported => ApiError::coded(StatusCode::UNSUPPORTED_MEDIA_TYPE, "kb_type_unsupported", Map::new()),
        KbError::NotText => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_text_invalid", Map::new()),
        KbError::Empty => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_file_empty", Map::new()),
        KbError::BadName => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "kb_name_invalid",
            params([("max", json!(kb::MAX_FILE_NAME_CHARS))]),
        ),
        KbError::BadUrl => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_url_invalid", Map::new()),
        KbError::UrlCredentials => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_url_credentials", Map::new()),
        KbError::NotFound => ApiError::coded(StatusCode::NOT_FOUND, "kb_doc_not_found", Map::new()),
        KbError::Corrupt => ApiError::coded(StatusCode::CONFLICT, "kb_manifest_invalid", Map::new()),
        KbError::BadDescription => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_description_invalid", Map::new()),
        KbError::DescriptionTooLong => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "kb_description_too_long",
            params([("max", json!(kb::MAX_DESCRIPTION_CHARS))]),
        ),
        KbError::BadAppliesTo => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "kb_applies_to_invalid",
            params([("max", json!(kb::MAX_APPLIES_TO)), ("max_chars", json!(kb::MAX_GLOB_CHARS))]),
        ),
        KbError::AppliesToStandardOnly => {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_applies_to_standard_only", Map::new())
        }
        KbError::OverviewTooLarge => ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "kb_overview_too_large",
            params([("max_kib", json!(kb::MAX_OVERVIEW_BYTES / 1024))]),
        ),
    }
}

/// Indicadores de um documento como `[{code, params}]`, com os textos redigidos.
fn indicators_view(list: &[Indicator], secrets: &SecretSet) -> Value {
    let items: Vec<Value> = list
        .iter()
        .map(|ind| {
            let params = match ind {
                Indicator::Duplicate { doc } => json!({ "doc": doc }),
                Indicator::NoHeadings => json!({}),
                Indicator::BrokenLink { target } => json!({ "target": scrub_text(secrets, target) }),
            };
            json!({ "code": ind.code(), "params": params })
        })
        .collect();
    Value::Array(items)
}

/// Entrada para a interface, com os textos livres redigidos.
pub(super) fn view(entry: &Entry, secrets: &SecretSet) -> Value {
    let scrub = |s: &str| scrub_text(secrets, s);
    json!({
        "id": entry.id,
        "title": scrub(&entry.title),
        "kind": entry.kind.as_str(),
        "source": entry.source.as_str(),
        "file_name": entry.file_name.as_deref().map(scrub),
        "source_url": entry.source_url.as_deref().map(scrub),
        "size": entry.size,
        "sha256": entry.sha256,
        "added_at": entry.added_at,
        "fetched_at": entry.fetched_at,
        "status": entry.status.as_str(),
        "status_reason": entry.status_reason,
        "pages": entry.pages,
        "extractor": entry.extractor,
        "role": entry.role.as_str(),
        "description": scrub(&entry.description),
        "applies_to": entry.applies_to.iter().map(|g| scrub(g)).collect::<Vec<_>>(),
        "links_out": entry.links_out,
    })
}

/// Raiz do projeto `id` já validado.
async fn project_root(state: &AppState, id: &str) -> Result<PathBuf, ApiError> {
    validate_project_id(id)?;
    let project = find_project(state, id).await?;
    Ok(PathBuf::from(&project.path))
}

/// Roda `op` fora do executor assíncrono.
async fn blocking<T: Send + 'static>(op: impl FnOnce() -> Result<T, ApiError> + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(op).await.map_err(|_| internal())?
}

/// Publica `kb_updated` e, se o documento está na fila, o entrega à extração.
fn announce(state: &AppState, project_id: &str, root: &std::path::Path, entry: &Entry, secrets: &Arc<SecretSet>) {
    kb_queue::publish(state.events(), project_id, entry, secrets);
    if entry.status.is_queued() {
        state.kb_queue().enqueue(project_id, root.to_path_buf(), &entry.id, secrets.clone());
    }
}

/// `GET /api/projects/:id/kb`.
pub async fn get_kb(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Result<Response, ApiError> {
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    let scope = secrets.clone();
    let list_root = root.clone();
    let (docs, body) = blocking(move || {
        let docs = kb::list(&list_root).map_err(|e| kb_error(e, &scope))?;
        // A visão geral é editável no disco: inválida (grande demais, não UTF-8, symlink) não
        // derruba a listagem.
        let (has_overview, overview_invalid) = match kb::read_overview(&list_root, &scope) {
            Ok(text) => (!text.is_empty(), false),
            Err(_) => (false, true),
        };
        // Os indicadores dependem do índice: sem ele (base ilegível), a lista sai sem eles.
        let indicators = kb::index::index_for(&list_root).map(|index| kb::indicators::compute(&docs, &index)).unwrap_or_default();
        let views: Vec<Value> = docs
            .iter()
            .map(|d| {
                let mut v = view(d, &scope);
                v["indicators"] = indicators_view(indicators.get(&d.id).map_or(&[][..], Vec::as_slice), &scope);
                v
            })
            .collect();
        let mut body = json!({ "docs": views, "has_overview": has_overview });
        if overview_invalid {
            body["overview_invalid"] = json!(true);
        }
        Ok((docs, body))
    })
    .await?;
    for doc in docs.iter().filter(|d| d.status.is_queued()) {
        state.kb_queue().enqueue(&id, root.clone(), &doc.id, secrets.clone());
    }
    Ok(Json(body).into_response())
}

/// `POST /api/projects/:id/kb/files?name=<nome>`.
pub async fn post_file(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    if body.len() > kb::MAX_UPLOAD_BYTES {
        return Err(ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "kb_file_too_large",
            params([("max_mib", json!(kb::MAX_UPLOAD_BYTES / (1024 * 1024)))]),
        ));
    }
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    let name = query.get("name").cloned().unwrap_or_default();
    let (scope, add_root) = (secrets.clone(), root.clone());
    let entry = blocking(move || {
        ensure_local_excludes(&add_root)?;
        kb::add_file(&add_root, &name, &body, &scope, &now_rfc3339()).map_err(|e| kb_error(e, &scope))
    })
    .await?;
    announce(&state, &id, &root, &entry, &secrets);
    Ok((StatusCode::CREATED, Json(json!({ "doc": view(&entry, &secrets) }))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkInput {
    url: String,
}

/// `POST /api/projects/:id/kb/links`.
pub async fn post_link(State(state): State<AppState>, AxPath(id): AxPath<String>, body: Bytes) -> Result<Response, ApiError> {
    let input: LinkInput = parse_body(&body)?;
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    let (scope, add_root) = (secrets.clone(), root.clone());
    let entry = blocking(move || {
        ensure_local_excludes(&add_root)?;
        kb::add_link(&add_root, &input.url, &scope, &now_rfc3339()).map_err(|e| kb_error(e, &scope))
    })
    .await?;
    announce(&state, &id, &root, &entry, &secrets);
    Ok((StatusCode::CREATED, Json(json!({ "doc": view(&entry, &secrets) }))).into_response())
}

/// `GET /api/projects/:id/kb/docs/:doc`.
pub async fn get_doc(State(state): State<AppState>, AxPath((id, doc)): AxPath<(String, String)>) -> Result<Response, ApiError> {
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    blocking(move || {
        let (entry, content) = kb::read_note(&root, &doc, &secrets).map_err(|e| kb_error(e, &secrets))?;
        // Seções com a âncora pública da citação (`s.<n>` quando o título traz segredo); o
        // documento fora do índice (ainda não pronto) não tem seções.
        let sections: Vec<Value> = kb::index::list_sections(&root, &doc, &secrets)
            .unwrap_or_default()
            .into_iter()
            .map(|s| json!({ "anchor": s.anchor, "title": s.title, "level": s.level, "line": s.line }))
            .collect();
        Ok(Json(json!({ "doc": view(&entry, &secrets), "content": content, "sections": sections })).into_response())
    })
    .await
}

/// `PATCH /api/projects/:id/kb/docs/:doc`.
pub async fn patch_doc(
    State(state): State<AppState>,
    AxPath((id, doc)): AxPath<(String, String)>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let patch: kb::Patch = parse_body(&body)?;
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    let scope = secrets.clone();
    let entry = blocking(move || {
        ensure_local_excludes(&root)?;
        kb::update(&root, &doc, &patch).map_err(|e| kb_error(e, &scope))
    })
    .await?;
    kb_queue::publish(state.events(), &id, &entry, &secrets);
    Ok(Json(json!({ "doc": view(&entry, &secrets) })).into_response())
}

/// `DELETE /api/projects/:id/kb/docs/:doc`.
pub async fn delete_doc(State(state): State<AppState>, AxPath((id, doc)): AxPath<(String, String)>) -> Result<Response, ApiError> {
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    let removed = doc.clone();
    blocking(move || kb::remove(&root, &removed).map_err(|e| kb_error(e, &secrets))).await?;
    kb_queue::publish_removed(state.events(), &id, &doc);
    Ok(Json(json!({ "removed": doc })).into_response())
}

/// `GET /api/projects/:id/kb/overview`.
pub async fn get_overview(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Result<Response, ApiError> {
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    blocking(move || {
        let text = kb::read_overview(&root, &secrets).map_err(|e| kb_error(e, &secrets))?;
        Ok(Json(json!({ "text": text, "max_bytes": kb::MAX_OVERVIEW_BYTES })).into_response())
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OverviewInput {
    text: String,
}

/// `PUT /api/projects/:id/kb/overview`.
pub async fn put_overview(State(state): State<AppState>, AxPath(id): AxPath<String>, body: Bytes) -> Result<Response, ApiError> {
    let input: OverviewInput = parse_body(&body)?;
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    blocking(move || {
        ensure_local_excludes(&root)?;
        kb::write_overview(&root, &input.text).map_err(|e| kb_error(e, &secrets))?;
        let text = kb::read_overview(&root, &secrets).map_err(|e| kb_error(e, &secrets))?;
        Ok(Json(json!({ "text": text, "max_bytes": kb::MAX_OVERVIEW_BYTES })).into_response())
    })
    .await
}

#[cfg(test)]
#[path = "kb_tests.rs"]
mod tests;
