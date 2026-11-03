//! Rotas da memória para a aba Memória (spec 140 T07; a tela é a T08). Passam pelas proteções de toda
//! rota da API (a guarda de `Host`/`Origin` e, em modo rede, o dispositivo autorizado), e cada erro
//! tem código.
//!
//! - `GET /api/nanty/memory`: os temas (com a situação de cada arquivo: `ok`, `missing`, `invalid` ou
//!   `newer`), as entradas, inclusive as apagadas que ainda voltam, e os tetos.
//! - `PUT /api/nanty/memory/:id`: `{title, text, base_updated}`. A edição vale só sobre a versão que a
//!   pessoa leu (o `updated`, como o `base_sha256` da base de conhecimento); grava como `person_said`.
//! - `POST /api/nanty/memory/:id/delete` e `.../restore`: apagar (reversível por um tempo) e devolver.
//! - `POST /api/nanty/memory/export` (T08): a cópia que a aba baixa. É POST, e não GET, para nenhuma
//!   navegação de outro site disparar o download com o cookie `Lax`; pede dispositivo como toda rota
//!   da API. Só as entradas ativas e de projeto ainda cadastrado ([`exportable`]), passadas de novo
//!   pelo redator com os segredos de agora (a chave cadastrada depois da gravação também sai).
//!
//! Cada mudança e a exportação vão ao registro do Nanty (`journal.jsonl`, como as decisões dos
//! cartões): `memory.edited`, `memory.deleted`, `memory.restored`, `memory.swapped` (a nota que a devolvida tirou de lugar, que vai para as apagadas) e `memory.exported`, com o id, o tema
//! e o canal (`web`). Só códigos e ids: nunca o título nem o texto (C-10).
//!
//! Na leitura, cada entrada traz `stale` (saiu do índice por falta de uso) e, se apagada,
//! `restore_until_ms` (o fim do prazo para restaurar): a aba não refaz a conta com o relógio dela.

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::ops::{self, Refusal};
use super::store::{MemoryError, Snapshot};
use super::{Entry, EntryState, Source, SourceKind, Topic, MEMORY_VERSION};
use crate::server::error::ApiError;
use crate::config::Config;
use crate::server::events::SecretSet;
use crate::server::nanty::api::parse_json;
use crate::server::nanty::provenance::Scoped;
use crate::server::nanty::redact;
use crate::server::nanty::store::Channel;
use crate::server::nanty::journal::{JournalEntry, JournalKind};
use crate::server::nanty::{InstanceStatus, Nanty};
use crate::server::state::AppState;

/// O erro da rota para uma recusa.
pub fn api_error(refusal: &Refusal) -> ApiError {
    let max = |n: usize| {
        let mut p = Map::new();
        p.insert("max".into(), json!(n));
        p
    };
    match refusal {
        Refusal::TitleEmpty | Refusal::TextEmpty => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_memory_empty", Map::new()),
        Refusal::TitleLong => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_memory_title_long", max(super::TITLE_MAX_CHARS)),
        Refusal::TextLong(n) => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_memory_too_long", max(*n)),
        Refusal::Full(n) => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_memory_full", max(*n)),
        Refusal::NotFound => ApiError::coded(StatusCode::NOT_FOUND, "nanty_memory_not_found", Map::new()),
        Refusal::Gone => ApiError::coded(StatusCode::NOT_FOUND, "nanty_memory_gone", Map::new()),
        Refusal::Changed => ApiError::coded(StatusCode::CONFLICT, "nanty_memory_changed", Map::new()),
        Refusal::Duplicate(_) => ApiError::coded(StatusCode::CONFLICT, "nanty_memory_duplicate", Map::new()),
        Refusal::Conflict(current) => {
            ApiError::coded(StatusCode::CONFLICT, "nanty_memory_conflict", Map::new()).with_field("entry", json!(current))
        }
        Refusal::File(MemoryError::Newer(_)) => ApiError::coded(StatusCode::CONFLICT, "nanty_memory_read_only", Map::new()),
        Refusal::File(MemoryError::Invalid(_)) => ApiError::coded(StatusCode::CONFLICT, "nanty_memory_file_invalid", Map::new()),
        Refusal::File(MemoryError::Io) => ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "nanty_memory_io", Map::new()),
        Refusal::File(MemoryError::NotOwner) => ApiError::coded(StatusCode::CONFLICT, "nanty_unavailable", Map::new()),
    }
}

/// O código do erro de uma recusa (o cartão que falhou o cita).
pub fn refusal_code(refusal: &Refusal) -> &'static str {
    api_error(refusal).code().unwrap_or("internal")
}

/// A exportação: a versão, a hora e as entradas por tema (a aba a oferece na T08).
pub fn export(snapshot: &Snapshot, now_ms: u64) -> Value {
    let mut topics = Map::new();
    for topic in Topic::ALL {
        let entries: Vec<&Entry> = snapshot.entries().filter(|e| e.topic == topic).collect();
        topics.insert(topic.as_str().to_string(), json!(entries));
    }
    json!({ "version": MEMORY_VERSION, "exported_at_ms": now_ms, "topics": topics })
}

/// O que sai na exportação: só as entradas ativas, e só as gerais ou de projeto ainda cadastrado (a
/// de projeto excluído não sai, mesmo antes da varredura), com título e texto redigidos de novo.
pub fn exportable(cfg: &Config, secrets: &SecretSet, snapshot: &Snapshot) -> Snapshot {
    let mut out = snapshot.clone();
    out.retain(|e| e.active() && e.within(|id| cfg.projects.iter().any(|p| p.id == id)));
    for entry in out.topics.iter_mut().flat_map(|t| t.entries.iter_mut()) {
        entry.title = redact::scrub(secrets, &entry.title);
        entry.text = redact::scrub(secrets, &entry.text);
    }
    out
}

/// Uma entrada como a aba a lê: a gravada, mais `stale` e o fim do prazo da apagada.
fn listed(entry: &Entry, now_ms: u64, cfg: &crate::config::NantyMemory) -> Value {
    let mut value = json!(entry);
    value["stale"] = json!(entry.active() && entry.stale(now_ms, cfg));
    if entry.state == EntryState::Deleted {
        if let Some(at) = entry.deleted_at {
            value["restore_until_ms"] = json!(at.saturating_add(cfg.deleted_keep_ms()));
        }
    }
    value
}

/// `POST /api/nanty/memory/export`: a cópia para baixar (a importação fica para depois).
pub async fn post_export(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let cfg = state.config().read().await.clone();
    let secrets = state.secret_set().await;
    let nanty = std::sync::Arc::clone(state.nanty());
    let now = nanty.now().epoch_ms;
    let snap = tokio::task::spawn_blocking({
        let n = std::sync::Arc::clone(&nanty);
        move || n.memory()
    })
    .await
    .map_err(|_| api_error(&Refusal::File(MemoryError::Io)))?;
    let out = exportable(&cfg, &secrets, &snap);
    let mut e = JournalEntry::new(now, JournalKind::Conversation, "memory.exported");
    e.params.insert("count".into(), json!(out.entries().count()));
    e.params.insert("by".into(), json!(Channel::Web));
    nanty.record(&e);
    Ok(Json(export(&out, now)))
}

/// `GET /api/nanty/memory`.
pub async fn get_memory(State(state): State<AppState>) -> Json<Value> {
    let nanty = std::sync::Arc::clone(state.nanty());
    let cfg = state.config().read().await.nanty.memory.clone();
    let now = nanty.now().epoch_ms;
    let snap = tokio::task::spawn_blocking({
        let n = std::sync::Arc::clone(&nanty);
        move || n.memory()
    })
    .await
    .unwrap_or_default();
    let topics: Vec<Value> = Topic::ALL
        .iter()
        .map(|topic| {
            let entries: Vec<Value> = snap.entries().filter(|e| e.topic == *topic).map(|e| listed(e, now, &cfg)).collect();
            json!({ "topic": topic.as_str(), "status": snap.status(*topic), "entries": entries })
        })
        .collect();
    Json(json!({
        "writable": nanty.status() == InstanceStatus::Active,
        "topics": topics,
        "limits": {
            "title_max_chars": super::TITLE_MAX_CHARS,
            "fact_max_chars": cfg.fact_chars(),
            "runbook_max_chars": cfg.runbook_chars(),
            "max_active": cfg.active(),
            "deleted_keep_days": cfg.deleted_keep_days.max(1),
        },
    }))
}

/// O id de uma entrada: 16 hex (o `new_id`).
fn validate_id(id: &str) -> Result<(), ApiError> {
    if id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_memory_id_invalid", Map::new()))
    }
}

fn owner(state: &AppState) -> Result<(), ApiError> {
    if state.nanty().status() == InstanceStatus::Active {
        Ok(())
    } else {
        Err(ApiError::coded(StatusCode::CONFLICT, "nanty_unavailable", Map::new()))
    }
}

fn entry_json(entry: &Entry) -> Json<Value> {
    Json(json!({ "entry": entry }))
}

/// A linha do registro de uma mudança pela aba (a auditoria do Nanty): o código, o id, o tema e o
/// projeto, sem o texto.
fn audit(nanty: &Nanty, code: &str, entry: &Entry) {
    let mut e = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Conversation, code);
    e.project_id.clone_from(&entry.source.project_id);
    e.params.insert("entry_id".into(), json!(entry.id));
    e.params.insert("topic".into(), json!(entry.topic.as_str()));
    e.params.insert("by".into(), json!(Channel::Web));
    nanty.record(&e);
}

/// Corpo de `PUT /api/nanty/memory/:id`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    title: String,
    text: String,
    base_updated: u64,
}

/// `PUT /api/nanty/memory/:id`: a pessoa edita o título e o texto.
pub async fn put_entry(State(state): State<AppState>, AxPath(id): AxPath<String>, body: Bytes) -> Result<Json<Value>, ApiError> {
    validate_id(&id)?;
    let input: EditInput = parse_json(&body)?;
    owner(&state)?;
    let cfg = state.config().read().await.clone();
    let secrets = state.secret_set().await;
    let nanty = std::sync::Arc::clone(state.nanty());
    let topic = nanty.memory().find(&id).map(|e| e.topic).ok_or_else(|| api_error(&Refusal::NotFound))?;
    let (title, text) = ops::clean(&cfg.nanty.memory, &secrets, topic, &input.title, &input.text).map_err(|r| api_error(&r))?;
    let source = Source { kind: SourceKind::PersonSaid, message_id: None, channel: Some(Channel::Web), project_id: None, at_ms: nanty.now().epoch_ms };
    let entry = tokio::task::spawn_blocking({
        let nanty = std::sync::Arc::clone(&nanty);
        move || ops::edit(&nanty, &cfg, &id, &title, &text, input.base_updated, source)
    })
    .await
    .map_err(|_| api_error(&Refusal::File(MemoryError::Io)))?
    .map_err(|r| api_error(&r))?;
    audit(&nanty, "memory.edited", &entry);
    Ok(entry_json(&entry))
}

/// `POST /api/nanty/memory/:id/delete`.
pub async fn post_delete(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Result<Json<Value>, ApiError> {
    validate_id(&id)?;
    owner(&state)?;
    let cfg = state.config().read().await.clone();
    let nanty = std::sync::Arc::clone(state.nanty());
    let entry = tokio::task::spawn_blocking({
        let nanty = std::sync::Arc::clone(&nanty);
        move || ops::delete(&nanty, &cfg, &id)
    })
    .await
    .map_err(|_| api_error(&Refusal::File(MemoryError::Io)))?
    .map_err(|r| api_error(&r))?;
    audit(&nanty, "memory.deleted", &entry);
    Ok(entry_json(&entry))
}

/// `POST /api/nanty/memory/:id/restore`.
pub async fn post_restore(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Result<Json<Value>, ApiError> {
    validate_id(&id)?;
    owner(&state)?;
    let cfg = state.config().read().await.clone();
    let nanty = std::sync::Arc::clone(state.nanty());
    let (entry, swapped) = tokio::task::spawn_blocking({
        let nanty = std::sync::Arc::clone(&nanty);
        move || ops::restore(&nanty, &cfg, &id)
    })
    .await
    .map_err(|_| api_error(&Refusal::File(MemoryError::Io)))?
    .map_err(|r| api_error(&r))?;
    audit(&nanty, "memory.restored", &entry);
    // Spec 140 T17: a nota ativa de mesmo título que a devolvida tirou de lugar foi para as apagadas.
    for other in &swapped {
        audit(&nanty, "memory.swapped", other);
    }
    Ok(entry_json(&entry))
}
