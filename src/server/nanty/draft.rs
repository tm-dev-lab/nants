//! Levar ao Arquiteto como rascunho (spec 140 T15, G7, C-15).
//!
//! O Nanty não tem confiança para falar com o Arquiteto, que escreve spec e pode ter internet. Por
//! isso a ideia nunca entra como turno interno: ela só vira um rascunho na caixa de mensagem do
//! Arquiteto, e é a pessoa quem revê e envia.
//!
//! - **A oferta nasce de uma ferramenta** ([`PROPOSE_TO_ARCHITECT`]), só no modo de conversa. O
//!   servidor confere o projeto (observado), passa o texto pelo redator e pelo `clean`
//!   ([`format::clean_whole`]) e recusa o que passa do teto (`[nanty.chat] architect_draft_chars`,
//!   provisório): recusa, nunca corte, para a pessoa ver o texto inteiro.
//! - **O cartão congela o texto** ([`ACTION`]): o texto exato, a marca e a fonte vão nas linhas do
//!   cartão, que o Telegram e a janela mostram inteiras. O "sim" vale pelo token daquele cartão: uma
//!   proposta nova troca o cartão e o token. No Telegram, o texto completo vem antes do botão; o que não
//!   cabe sai sem o "sim", com a linha que aponta a janela (o padrão da T07, F-04, no `outbox`).
//! - **O clique grava o rascunho** ([`ArchitectDraft`]) no estado do Nanty, um por projeto, de forma
//!   atômica (a gravação do `state.json`). Nada vai ao Arquiteto: a janela do projeto lê o rascunho
//!   (`GET /api/projects/:id/chat/draft`) e o põe na caixa para a pessoa editar, enviar ou descartar.
//! - **Procedência:** com contaminação na sessão ou na janela, o rascunho vai marcado como dado.
//!   Enquanto ele estiver pendente, toda mensagem da pessoa ao Arquiteto do projeto sai sem internet,
//!   com uma nota ao modelo ([`for_turn`]); ele só sai do estado quando um turno que o carregou termina
//!   com sucesso ([`consume`]) ou quando a pessoa o descarta. A mensagem que saiu sem internet grava a
//!   marca no `.chat.jsonl`, e os turnos internos seguintes a herdam, também depois de um reinício. Contaminação vinda de outro projeto que não o destino é recusada, na ferramenta e
//!   de novo no clique (`nanty_draft_other_project`).
//! - **Escopo:** o rascunho e o pedido no cartão levam o projeto e a procedência ([`Scoped`]); a
//!   limpeza de projeto e a varredura os alcançam.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::act::{self, Answer, Outcome};
use super::agent::Session;
use super::journal::{JournalEntry, JournalKind};
use super::memory::card::CARD_LINES;
use super::provenance::{unique, Scoped, TaintSource};
use super::store::{Card, Channel, DecidedBy, DecidedCard, Delivery};
use super::telegram::format;
use super::turn::is_code;
use super::{chat, redact, tools, InstanceStatus, Nanty};
use crate::config::{Config, Urgency};
use crate::i18n::{tr, Lang};
use crate::server::error::ApiError;
use crate::server::events::{validate_project_id, ApiEvent, EventType, SecretSet};
use crate::server::state::AppState;

/// A ferramenta do modelo: propõe levar uma ideia ao Arquiteto de um projeto.
pub const PROPOSE_TO_ARCHITECT: &str = "propose_to_architect";
/// A ação do cartão (fora do catálogo: só o toque decide, e `act` nunca a executa).
pub const ACTION: &str = "architect_draft";
/// O texto do cartão.
pub const CARD_CODE: &str = "nanty.card.architect_draft";
/// O fim do grupo do cartão (`<projeto>:architect_draft`): um por projeto.
const GROUP_KIND: &str = "architect_draft";
/// O pedido guardado no cartão.
const PARAM_DRAFT: &str = "draft";
/// Validade do cartão: um dia.
const CARD_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// A parte do `nanty_changed` que a janela do projeto escuta.
pub const EVENT_PART: &str = "architect_draft";

/// O que o clique grava: o texto que a pessoa viu, o projeto de destino e a procedência.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectDraft {
    pub id: String,
    pub project_id: String,
    pub text: String,
    /// As fontes não confiáveis do turno que propôs. Com alguma, o rascunho é marcado.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub taint: Vec<TaintSource>,
    pub channel: Channel,
    pub at_ms: u64,
}

impl ArchitectDraft {
    /// Veio de uma conversa que leu dado não confiável: vai marcado, e o turno sai sem internet.
    pub fn marked(&self) -> bool {
        !self.taint.is_empty()
    }
}

impl Scoped for ArchitectDraft {
    fn projects(&self) -> Vec<&str> {
        let mut out: Vec<&str> = vec![self.project_id.as_str()];
        for id in self.taint.iter().filter_map(|t| t.project_id.as_deref()) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }
}

/// O cartão é o de levar ao Arquiteto.
pub fn is_draft_card(card: &Card) -> bool {
    card.action.as_deref() == Some(ACTION)
}

fn group_of(project_id: &str) -> String {
    format!("{project_id}:{GROUP_KIND}")
}

/// Os projetos de onde veio a contaminação que não são o destino (C-15): com algum, recusa.
fn foreign<'a>(taint: &'a [TaintSource], destination: &str) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for id in taint.iter().filter_map(|t| t.project_id.as_deref()).filter(|id| *id != destination) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

fn journal(nanty: &Nanty, code: &str, project_id: Option<&str>, params: &[(&str, Value)]) {
    let mut e = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Conversation, code);
    e.project_id = project_id.map(str::to_string);
    for (k, v) in params {
        e.params.insert((*k).to_string(), v.clone());
    }
    nanty.record(&e);
}

fn line(code: String, params: Value) -> Value {
    json!({ "code": code, "params": params })
}

/// As linhas do cartão: o texto exato e, se marcado, a marca e a fonte (os tipos de leitura e os
/// projetos, só os ids na forma de código).
fn card_lines(draft: &ArchitectDraft) -> Vec<Value> {
    let mut out = vec![line("nanty.architect_draft.text".into(), json!({ "text": draft.text }))];
    if draft.marked() {
        out.push(line("nanty.architect_draft.marked".into(), json!({})));
        for code in unique(draft.taint.iter().map(|t| t.kind.code())) {
            if let Some(kind) = code.strip_prefix("nanty.taint.") {
                out.push(line(format!("nanty.memory.from.{kind}"), json!({})));
            }
        }
        let projects = unique(draft.projects().into_iter().filter(|id| is_code(id)).map(str::to_string)).join(", ");
        out.push(line("nanty.memory.from_project".into(), json!({ "projects": projects })));
    }
    out
}

/// `propose_to_architect(project_id, text)`: cria o cartão com o texto congelado. Nada vai ao
/// Arquiteto aqui, nem depois do clique: o clique só deixa o rascunho na caixa dele.
pub fn propose(nanty: &Nanty, cfg: &Config, lang: Lang, secrets: &SecretSet, arguments: &str, session: &mut Session) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let id = args.get("project_id").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if id.is_empty() {
        return tr(lang, "nanty.tool.bad_args", &[]);
    }
    let Some(project) = chat::observed(cfg).into_iter().find(|p| p.id == id) else {
        return tr(lang, "nanty.tool.unknown_project", &[("id", if is_code(id) { id } else { "?" })]);
    };
    let raw = args.get("text").and_then(Value::as_str).unwrap_or_default();
    let text = format::clean_whole(secrets, raw);
    if text.is_empty() {
        return tr(lang, "nanty.tool.draft_empty", &[]);
    }
    let max = cfg.nanty.chat.architect_draft_chars();
    if text.chars().count() > max {
        return tr(lang, "nanty.tool.draft_too_long", &[("max", &max.to_string())]);
    }
    // C-15: a contaminação que veio de outro projeto não atravessa para este.
    if !foreign(&session.tainted.sources, &project.id).is_empty() {
        journal(nanty, "architect_draft.refused", Some(&project.id), &[("reason", json!("other_project"))]);
        return tr(lang, "nanty.tool.draft_other_project", &[]);
    }
    if session.drafted {
        return tr(lang, "nanty.tool.draft_limit", &[]);
    }
    session.drafted = true;
    if !session.read.contains(&project.id) {
        session.read.push(project.id.clone());
    }
    let now = nanty.now().epoch_ms;
    let draft = ArchitectDraft {
        id: super::new_id(),
        project_id: project.id.clone(),
        text,
        taint: session.tainted.sources.clone(),
        channel: session.channel,
        at_ms: now,
    };
    let mut params = Map::new();
    params.insert("project".into(), json!(tools::project_name(secrets, project)));
    params.insert("marked".into(), json!(draft.marked()));
    params.insert(CARD_LINES.into(), Value::Array(card_lines(&draft)));
    params.insert(PARAM_DRAFT.into(), json!(draft));
    let group = group_of(&project.id);
    let card = Card {
        id: super::new_id(),
        project_id: Some(project.id.clone()),
        group: group.clone(),
        code: CARD_CODE.to_string(),
        params,
        action: Some(ACTION.to_string()),
        token: Some(act::new_token()),
        created_at_ms: now,
        expires_at_ms: now.saturating_add(CARD_TTL_MS),
        urgency: Some(Urgency::Normal),
        // A pessoa acabou de conversar: o cartão vai com a resposta, sem som.
        delivery: Some(Delivery::Silent),
        requested: Some(session.channel),
        ..Card::default()
    };
    let card_id = card.id.clone();
    // Uma proposta nova troca o cartão do projeto: o token antigo deixa de valer.
    nanty.update(|st| {
        st.cards.retain(|c| c.group != group);
        st.cards.push(card);
        true
    });
    journal(nanty, "architect_draft.proposed", Some(&project.id), &[("marked", json!(draft.marked())), ("channel", json!(session.channel))]);
    session.cards.push(card_id);
    tr(lang, "nanty.tool.draft_card", &[])
}

fn outcome(name: &'static str, params: Map<String, Value>) -> Outcome {
    Outcome { outcome: name, code: format!("nanty.outcome.{name}"), params }
}

fn draft_of(card: &Card) -> Option<ArchitectDraft> {
    card.params.get(PARAM_DRAFT).and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Decide um cartão (a janela e o Telegram): o de levar ao Arquiteto aqui; os outros seguem para a
/// memória e, dela, para [`act::decide`]. O token que não confere, o cartão vencido e o já decidido
/// seguem a regra comum.
pub async fn decide(state: &AppState, card_id: &str, token: &str, answer: Answer, by: DecidedBy) -> Result<Outcome, ApiError> {
    let nanty = Arc::clone(state.nanty());
    let now_ms = nanty.now().epoch_ms;
    let mine = nanty
        .snapshot()
        .cards
        .iter()
        .find(|c| c.id == card_id && is_draft_card(c) && c.token.as_deref() == Some(token) && c.expires_at_ms > now_ms)
        .cloned();
    let Some(card) = mine else { return super::memory::card::decide(state, card_id, token, answer, by).await };
    if nanty.status() != InstanceStatus::Active {
        return Ok(outcome("unavailable", Map::new()));
    }
    if answer == Answer::Later {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_action_invalid", Map::new()));
    }
    let mut claimed = false;
    nanty.update(|st| {
        let Some(i) = st.cards.iter().position(|c| c.id == card_id && c.token.as_deref() == Some(token)) else { return false };
        st.cards.remove(i);
        st.push_decided(DecidedCard {
            card_id: card.id.clone(),
            token: token.to_string(),
            project_id: card.project_id.clone(),
            decided_by: by,
            answer: answer.as_str().to_string(),
            at_ms: now_ms,
        });
        claimed = true;
        true
    });
    // O outro clique chegou antes: a regra comum diz quem decidiu.
    if !claimed {
        return act::decide(state, card_id, token, answer, by).await;
    }
    let mut e = JournalEntry::new(now_ms, JournalKind::Decision, answer.as_str());
    e.project_id = card.project_id.clone();
    e.action = card.action.clone();
    e.decided_by = Some(by);
    e.params.insert("card_id".into(), json!(card_id));
    nanty.record(&e);
    if answer == Answer::No {
        return Ok(outcome("declined", Map::new()));
    }
    let Some(draft) = draft_of(&card).filter(|d| card.project_id.as_deref() == Some(d.project_id.as_str())) else {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_action_invalid", Map::new()));
    };
    let cfg = state.config().read().await.clone();
    // O projeto pode ter saído (ou deixado de ser observado) entre a proposta e o clique.
    let Some(project) = chat::observed(&cfg).into_iter().find(|p| p.id == draft.project_id) else {
        return Err(ApiError::coded(StatusCode::NOT_FOUND, "nanty_draft_project_gone", Map::new()));
    };
    // C-15, de novo no clique: nada que veio de outro projeto vira rascunho deste.
    if !foreign(&draft.taint, &draft.project_id).is_empty() {
        journal(&nanty, "architect_draft.refused", Some(&draft.project_id), &[("reason", json!("other_project"))]);
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_draft_other_project", Map::new()));
    }
    let saved = ArchitectDraft { at_ms: now_ms, ..draft };
    let (project_id, marked) = (saved.project_id.clone(), saved.marked());
    nanty.update(|st| {
        // Um só rascunho pendente por projeto: o novo troca o anterior.
        st.architect_drafts.retain(|d| d.project_id != project_id);
        st.architect_drafts.push(saved);
        true
    });
    publish(state, &project_id);
    journal(&nanty, "architect_draft.saved", Some(&project_id), &[("marked", json!(marked)), ("by", json!(by))]);
    let secrets = SecretSet::from_config(&cfg);
    let mut params = Map::new();
    params.insert("project".into(), json!(tools::project_name(&secrets, project)));
    params.insert("project_id".into(), json!(project_id));
    Ok(outcome("architect_draft_saved", params))
}

/// Avisa a janela do projeto que o rascunho mudou.
fn publish(state: &AppState, project_id: &str) {
    state.events().publish(ApiEvent::new(EventType::NantyChanged, Some(project_id.to_string()), json!({ "part": EVENT_PART })));
}

/// O rascunho pendente do projeto.
pub fn pending(nanty: &Nanty, project_id: &str) -> Option<ArchitectDraft> {
    nanty.snapshot().architect_drafts.into_iter().find(|d| d.project_id == project_id)
}

/// O que o rascunho pendente decide num turno do Arquiteto (spec 140 T15, revisão 1, F-01 e F-02).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftTurn {
    /// O turno sai sem internet.
    pub offline: bool,
    /// O modelo recebe o aviso do servidor junto do estado (só na mensagem da pessoa).
    pub note: bool,
    /// O rascunho que esta mensagem carrega: sai do estado só se o turno terminar com sucesso.
    pub carries: Option<String>,
}

/// A regra do turno, com a fonte da verdade no servidor e em disco:
/// - **mensagem da pessoa:** com um rascunho marcado pendente no projeto, toda mensagem sai sem
///   internet e com o aviso, carregue ela o rascunho ou não; carrega quem manda o id dele ou traz o
///   texto dele inteiro;
/// - **turno interno:** sai sem internet com um rascunho marcado pendente ou quando a última mensagem
///   da pessoa no histórico saiu sem internet (`last_person_offline`, gravado no `.chat.jsonl`, que
///   sobrevive a um reinício). A próxima mensagem da pessoa sem marca encerra a cadeia.
///
/// Nada é consumido aqui ([`consume`]).
pub fn for_turn(state: &AppState, project_id: &str, internal: bool, draft_id: Option<&str>, message: &str, last_person_offline: bool) -> DraftTurn {
    let found = pending(state.nanty(), project_id);
    let marked = found.as_ref().is_some_and(ArchitectDraft::marked);
    if internal {
        return DraftTurn { offline: marked || last_person_offline, note: false, carries: None };
    }
    let carries = found.filter(|d| draft_id.is_some_and(|id| id == d.id) || (!d.text.trim().is_empty() && message.contains(d.text.trim()))).map(|d| d.id);
    DraftTurn { offline: marked, note: marked, carries }
}

/// O turno que carregou o rascunho terminou com sucesso: só agora ele sai do estado. Um turno que
/// falha não consome nada, e o reenvio herda a marca.
pub fn consume(state: &AppState, project_id: &str, draft_id: &str) {
    let nanty = state.nanty();
    let mut marked = false;
    let removed = nanty.update(|st| {
        let before = st.architect_drafts.len();
        marked = st.architect_drafts.iter().any(|d| d.id == draft_id && d.marked());
        st.architect_drafts.retain(|d| !(d.project_id == project_id && d.id == draft_id));
        st.architect_drafts.len() != before
    });
    if removed {
        journal(nanty, "architect_draft.sent", Some(project_id), &[("marked", json!(marked))]);
        publish(state, project_id);
    }
}

// ---- Rotas da janela do projeto ----

fn draft_view(secrets: &SecretSet, draft: &ArchitectDraft) -> Value {
    let kinds = unique(draft.taint.iter().map(|t| t.kind.code()));
    json!({
        "id": draft.id,
        "text": redact::scrub(secrets, &draft.text),
        "marked": draft.marked(),
        "kinds": kinds,
        "created_at_ms": draft.at_ms,
    })
}

async fn known_project(state: &AppState, id: &str) -> Result<(), ApiError> {
    validate_project_id(id)?;
    if state.config().read().await.projects.iter().any(|p| p.id == id) {
        Ok(())
    } else {
        Err(ApiError::coded(StatusCode::NOT_FOUND, "project_not_found", Map::new()))
    }
}

/// `GET /api/projects/:id/chat/draft`: o rascunho que o Nanty deixou na caixa do Arquiteto, ou `null`.
pub async fn get_draft(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Result<Json<Value>, ApiError> {
    known_project(&state, &id).await?;
    let secrets = state.secret_set().await;
    let draft = pending(state.nanty(), &id).map(|d| draft_view(&secrets, &d));
    Ok(Json(json!({ "draft": draft })))
}

/// Corpo de `POST /api/projects/:id/chat/draft/discard`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscardInput {
    id: String,
}

/// `POST /api/projects/:id/chat/draft/discard`: `{id}`; a pessoa descarta o rascunho. Responde
/// `{discarded}` (`false` quando aquele rascunho já não estava lá).
pub async fn post_discard(State(state): State<AppState>, AxPath(id): AxPath<String>, body: Bytes) -> Result<Json<Value>, ApiError> {
    known_project(&state, &id).await?;
    let input: DiscardInput = serde_json::from_slice(&body)
        .map_err(|_| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields", Map::new()))?;
    let nanty = state.nanty();
    let discarded = nanty.update(|st| {
        let before = st.architect_drafts.len();
        st.architect_drafts.retain(|d| !(d.project_id == id && d.id == input.id));
        st.architect_drafts.len() != before
    });
    if discarded {
        journal(nanty, "architect_draft.discarded", Some(&id), &[]);
        publish(&state, &id);
    }
    Ok(Json(json!({ "discarded": discarded })))
}

#[cfg(test)]
#[path = "draft_tests.rs"]
mod tests;
