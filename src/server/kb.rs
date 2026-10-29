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
//!   pública da citação e a linha do cabeçalho no corpo), redigidos; e, desde a spec 136 T03, os
//!   links (`links_to`, `cited_by`) e as specs que o citam (`used_in_plans`) ([`super::kb_read`]);
//! - `GET /kb/docs/:doc?section=<âncora>`: só a seção, com as subseções, com teto e redação
//!   (spec 136 T03); seção inexistente → 404 `kb_section_not_found`;
//! - `GET /kb/search?q=&limit=`: a busca, com os destaques calculados no servidor (spec 136 T03);
//! - `GET /specs/:spec_id/kb-sources`: as fontes da base citadas no plano (spec 136 T03);
//! - `PATCH /kb/docs/:doc`: `{ "description"? }`; o `role` e o `applies_to` da 134 são aceitos e
//!   ignorados (spec 136 T02, C-06: sem classificação manual);
//! - `DELETE /kb/docs/:doc`;
//! - `GET` e `PUT /kb/overview`: `{ "text": "..." }`, até [`kb::MAX_OVERVIEW_BYTES`];
//! - `POST /kb/written` (spec 136 T10): `{ "title", "content"? }`, cria um documento escrito no
//!   Nants (`source: "written"`), pronto e indexado na hora, com `kb_updated`. Texto até
//!   [`kb::MAX_WRITTEN_BYTES`] (corpo até [`kb::MAX_WRITTEN_BODY_BYTES`]); título vazio
//!   (`kb_title_empty`), longo (`kb_title_too_long`) ou repetido (`kb_title_taken`, 409, com o id
//!   do outro); texto com segredo (`kb_content_secret`). O mesmo texto de outro documento não é
//!   recusado: vira o indicador `duplicate`;
//! - `PUT /kb/docs/:doc/content` (spec 136 T10): `{ "content", "base_sha256" }`, troca o texto do
//!   documento escrito ou enviado em markdown ou texto. `base_sha256` diferente do atual → 409
//!   `kb_edit_conflict`, com o `doc` e o `content` atuais no corpo; PDF e link → 422
//!   `kb_not_editable`. O título não muda por aqui (fica o do frontmatter da nota): o corpo não
//!   aceita `title`, e a troca de título, que mexe no catálogo e nas citações que a pessoa vê, é
//!   outra operação, com as próprias recusas;
//! - `GET /kb/docs/:doc/content` (spec 136 T11): `{ "content", "sha256" }`, o texto-fonte de um
//!   editável para o editor (o `sha256` é o `base_sha256` do `PUT`). PDF e link → 422
//!   `kb_not_editable`; original que a redação mudaria → 422 `kb_source_redacted`.
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
use super::kb_read;
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
        KbError::OverviewTooLarge => ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "kb_overview_too_large",
            params([("max_kib", json!(kb::MAX_OVERVIEW_BYTES / 1024))]),
        ),
        KbError::TitleEmpty => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_title_empty", Map::new()),
        KbError::TitleTooLong => ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "kb_title_too_long",
            params([("max", json!(kb::MAX_TITLE_CHARS))]),
        ),
        KbError::TitleTaken(id) => {
            ApiError::coded(StatusCode::CONFLICT, "kb_title_taken", params([("id", json!(scrub_text(secrets, &id)))]))
        }
        KbError::WrittenTooLarge => ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "kb_written_too_large",
            params([("max_kib", json!(kb::MAX_WRITTEN_BYTES / 1024))]),
        ),
        KbError::EditConflict => ApiError::coded(StatusCode::CONFLICT, "kb_edit_conflict", Map::new()),
        KbError::NotEditable => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_not_editable", Map::new()),
        KbError::SourceRedacted => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_source_redacted", Map::new()),
    }
}

/// Erro da escrita de um documento (spec 136 T10): o segredo e o byte nulo no texto têm os
/// próprios códigos (`kb_secret_content` e `kb_text_invalid` falam de arquivo enviado).
fn written_error(e: KbError, secrets: &SecretSet) -> ApiError {
    match e {
        KbError::SecretContent => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_content_secret", Map::new()),
        KbError::NotText => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "kb_content_invalid", Map::new()),
        other => kb_error(other, secrets),
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
        "description": scrub(&entry.description),
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WrittenInput {
    title: String,
    #[serde(default)]
    content: String,
}

/// `POST /api/projects/:id/kb/written` (spec 136 T10).
pub async fn post_written(State(state): State<AppState>, AxPath(id): AxPath<String>, body: Bytes) -> Result<Response, ApiError> {
    let input: WrittenInput = parse_body(&body)?;
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    let (scope, add_root) = (secrets.clone(), root.clone());
    let entry = blocking(move || {
        ensure_local_excludes(&add_root)?;
        kb::add_written(&add_root, &input.title, &input.content, &scope, &now_rfc3339()).map_err(|e| written_error(e, &scope))
    })
    .await?;
    announce(&state, &id, &root, &entry, &secrets);
    Ok((StatusCode::CREATED, Json(json!({ "doc": view(&entry, &secrets) }))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentInput {
    content: String,
    base_sha256: String,
}

/// `PUT /api/projects/:id/kb/docs/:doc/content` (spec 136 T10). No conflito, o corpo do 409 traz
/// o `doc` e o `content` atuais (redigidos), para a pessoa comparar sem outra leitura.
pub async fn put_content(
    State(state): State<AppState>,
    AxPath((id, doc)): AxPath<(String, String)>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input: ContentInput = parse_body(&body)?;
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    let (scope, edit_root) = (secrets.clone(), root.clone());
    let entry = blocking(move || {
        ensure_local_excludes(&edit_root)?;
        match kb::edit_content(&edit_root, &doc, &input.content, &input.base_sha256, &scope) {
            Ok(entry) => Ok(entry),
            Err(KbError::EditConflict) => {
                let err = written_error(KbError::EditConflict, &scope);
                Err(match kb::read_note(&edit_root, &doc, &scope) {
                    Ok((current, content)) => err.with_field("doc", view(&current, &scope)).with_field("content", json!(content)),
                    Err(_) => err,
                })
            }
            Err(e) => Err(written_error(e, &scope)),
        }
    })
    .await?;
    announce(&state, &id, &root, &entry, &secrets);
    Ok(Json(json!({ "doc": view(&entry, &secrets) })).into_response())
}

/// `GET /api/projects/:id/kb/docs/:doc/content` (spec 136 T11): o texto-fonte de um documento
/// editável e o `sha256` que o `PUT` espera em `base_sha256`. Só lê. PDF e link → 422
/// `kb_not_editable`; original que a redação mudaria → 422 `kb_source_redacted` (nunca o texto
/// redigido, que o editor gravaria por cima do original).
pub async fn get_content(State(state): State<AppState>, AxPath((id, doc)): AxPath<(String, String)>) -> Result<Response, ApiError> {
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    blocking(move || {
        let (entry, content) = kb::read_source(&root, &doc, &secrets).map_err(|e| kb_error(e, &secrets))?;
        Ok(Json(json!({ "content": content, "sha256": entry.sha256 })).into_response())
    })
    .await
}

/// `GET /api/projects/:id/kb/docs/:doc`. Com `?section=<âncora>`, só a seção (spec 136 T03).
pub async fn get_doc(
    State(state): State<AppState>,
    AxPath((id, doc)): AxPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    blocking(move || {
        if let Some(anchor) = query.get("section") {
            let entry = kb::read_entry(&root, &doc).map_err(|e| kb_error(e, &secrets))?;
            let section = kb_read::section_view(&root, &doc, anchor, &secrets).map_err(|e| match e {
                KbError::NotFound => ApiError::coded(StatusCode::NOT_FOUND, "kb_section_not_found", Map::new()),
                other => kb_error(other, &secrets),
            })?;
            return Ok(Json(json!({ "doc": view(&entry, &secrets), "section": section })).into_response());
        }
        let (entry, content) = kb::read_note(&root, &doc, &secrets).map_err(|e| kb_error(e, &secrets))?;
        // O índice vem do cache (montado uma vez por versão do manifesto); sem ele, a nota sai sem
        // seções e sem o contexto do "Citado por".
        let index = kb::index::index_for(&root).ok();
        // Seções com a âncora pública da citação (`s.<n>` quando o título traz segredo); o
        // documento fora do índice (ainda não pronto) não tem seções.
        let sections: Vec<Value> = index
            .as_ref()
            .and_then(|ix| ix.sections(&doc, &secrets))
            .unwrap_or_default()
            .into_iter()
            .map(|s| json!({ "anchor": s.anchor, "title": s.title, "level": s.level, "line": s.line }))
            .collect();
        let docs = kb::list(&root).map_err(|e| kb_error(e, &secrets))?;
        let (links_to, cited_by) = kb_read::links_view(&entry, &docs, index.as_deref(), &secrets);
        let used_in_plans = kb_read::used_in_plans(&root, &doc, &secrets);
        Ok(Json(json!({
            "doc": view(&entry, &secrets),
            "content": content,
            "sections": sections,
            "links_to": links_to,
            "cited_by": cited_by,
            "used_in_plans": used_in_plans,
        }))
        .into_response())
    })
    .await
}

/// `GET /api/projects/:id/kb/search?q=&limit=` (spec 136 T03, C-10).
pub async fn search(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let q = query.get("q").cloned().unwrap_or_default();
    if kb_read::query_too_long(&q) {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "kb_query_too_long",
            params([("max", json!(kb_read::MAX_QUERY_CHARS))]),
        ));
    }
    let limit = kb_read::search_limit(query.get("limit").map(String::as_str));
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    blocking(move || {
        let index = kb::index::index_for(&root).map_err(|e| kb_error(e, &secrets))?;
        Ok(Json(kb_read::search_view(&index, &q, limit, &secrets)).into_response())
    })
    .await
}

/// `GET /api/projects/:id/specs/:spec_id/kb-sources` (spec 136 T03): as fontes da base citadas
/// na spec e nas tarefas, para a janela do plano.
pub async fn get_plan_sources(
    State(state): State<AppState>,
    AxPath((id, spec_id)): AxPath<(String, String)>,
) -> Result<Response, ApiError> {
    super::spec_result::validate_spec_id(&spec_id)?;
    let root = project_root(&state, &id).await?;
    let secrets = state.secret_set().await;
    blocking(move || match kb_read::plan_sources(&root, &spec_id, &secrets).map_err(|e| kb_error(e, &secrets))? {
        Some(body) => Ok(Json(body).into_response()),
        None => Err(ApiError::coded(StatusCode::NOT_FOUND, "spec_not_found", Map::new())),
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
