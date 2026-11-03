//! Os cartões da memória (spec 140 T07, C-22 e C-24), no mecanismo de cartões que já existe (o token
//! aleatório do servidor, a validade e a decisão atômica), mas fora do catálogo de ações: o modelo
//! nunca os propõe, e `act` nunca os executa.
//!
//! - **Apagar o que foi guardado** ([`ACTION_FORGET`]): o rodapé "guardei isto" da resposta cita um
//!   cartão por entrada, com o botão de apagar. O toque apaga só aquela entrada. É um controle, não uma
//!   decisão pendente: fica fora de "Esperando você".
//! - **Confirmar o que o turno contaminado propôs** ([`ACTION_CONFIRM`], C-24): um cartão só, sem som,
//!   com o texto exato de cada pedido e a fonte (o tipo de leitura e o projeto). Um turno novo que
//!   propõe junta os pendentes e troca o cartão: no máximo um por turno. O "sim" grava como
//!   `person_confirmed`; o "não" descarta.
//! - **Quem decide:** a rota da janela e o botão do Telegram passam por [`decide`], que trata os
//!   cartões daqui e deixa os outros com [`act::decide`].

use std::sync::Arc;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::ops::{self, Remember};
use super::{Entry, Source, SourceKind};
use crate::config::{Config, Urgency};
use crate::server::error::ApiError;
use crate::server::nanty::act::{self, Answer, Outcome};
use crate::server::nanty::journal::{JournalEntry, JournalKind};
use crate::server::nanty::provenance::{unique, Scoped, TaintSource};
use crate::server::nanty::store::{Card, Channel, DecidedBy, DecidedCard, Delivery, NantyState};
use crate::server::nanty::turn::is_code;
use crate::server::nanty::{InstanceStatus, Nanty};
use crate::server::state::AppState;

/// Grupo do cartão de confirmação (um só).
pub const GROUP_CONFIRM: &str = "*:memory_confirm";
/// Começo do grupo do botão de apagar (um por entrada: o id vem depois).
pub const GROUP_FORGET: &str = "*:memory_forget:";
/// A ação do cartão de confirmação e a do botão de apagar.
pub const ACTION_CONFIRM: &str = "memory_confirm";
pub const ACTION_FORGET: &str = "memory_forget";
/// O texto de cada cartão.
pub const CARD_CONFIRM: &str = "nanty.card.memory_confirm";
pub const CARD_FORGET: &str = "nanty.card.memory_forget";
/// As linhas extras do cartão (`{code, params}`), que o Telegram e a janela mostram abaixo do texto.
pub const CARD_LINES: &str = "card_lines";
/// Pedidos pendentes no cartão de confirmação, no máximo (os mais novos ficam).
pub const MAX_PENDING: usize = 5;

/// O pedido de um turno contaminado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Remember(Remember),
    Forget {
        id: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
    },
}

/// Um pedido à espera da confirmação, com a fonte da contaminação.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub op: Op,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub channel: Channel,
    #[serde(default)]
    pub taint: Vec<TaintSource>,
    pub at_ms: u64,
    /// Na troca (revisão 1 da T07, F-01): a nota substituída como estava na proposta, que o cartão
    /// mostra ao lado do texto novo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<Replaced>,
}

/// A nota que um pedido troca, como estava na proposta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replaced {
    pub title: String,
    pub text: String,
    pub updated: u64,
    pub kind: SourceKind,
}

impl Replaced {
    pub fn of(entry: &Entry) -> Self {
        Self { title: entry.title.clone(), text: entry.text.clone(), updated: entry.updated, kind: entry.source.kind }
    }
}

/// Teto do texto da nota substituída no cartão, em caracteres.
const OLD_TEXT_CHARS: usize = 400;

impl Scoped for Proposal {
    fn projects(&self) -> Vec<&str> {
        let own = match &self.op {
            Op::Remember(r) => r.project_id.as_deref(),
            Op::Forget { project_id, .. } => project_id.as_deref(),
        };
        let mut out: Vec<&str> = Vec::new();
        for id in own.into_iter().chain(self.taint.iter().filter_map(|t| t.project_id.as_deref())) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }
}

/// O cartão é da memória.
pub fn is_memory_card(card: &Card) -> bool {
    matches!(card.action.as_deref(), Some(ACTION_CONFIRM | ACTION_FORGET))
}

fn line(code: String, params: Value) -> Value {
    json!({ "code": code, "params": params })
}

/// As linhas do cartão de confirmação: o texto exato de cada pedido e, abaixo, a fonte (os tipos de
/// leitura e os projetos, só os ids na forma de código).
fn card_lines(proposals: &[Proposal]) -> Vec<Value> {
    let mut out = Vec::new();
    for p in proposals {
        match &p.op {
            Op::Remember(r) => match &p.replaces {
                Some(old) => {
                    let old_text = crate::server::nanty::telegram::format::fit(&old.text, OLD_TEXT_CHARS);
                    let params = json!({ "old_title": old.title, "old_text": old_text, "title": r.title, "text": r.text });
                    out.push(line(format!("nanty.memory.replace.{}", r.topic.as_str()), params));
                    out.push(line(format!("nanty.memory.old_source.{}", old.kind.as_str()), json!({})));
                }
                None => out.push(line(format!("nanty.memory.save.{}", r.topic.as_str()), json!({ "title": r.title, "text": r.text }))),
            },
            Op::Forget { title, .. } => out.push(line("nanty.memory.drop".into(), json!({ "title": title }))),
        }
        let kinds = unique(p.taint.iter().map(|t| t.kind.code()));
        for code in kinds {
            if let Some(kind) = code.strip_prefix("nanty.taint.") {
                out.push(line(format!("nanty.memory.from.{kind}"), json!({})));
            }
        }
        let projects = unique(p.projects().into_iter().filter(|id| is_code(id)).map(str::to_string)).join(", ");
        if !projects.is_empty() {
            out.push(line("nanty.memory.from_project".into(), json!({ "projects": projects })));
        }
    }
    out
}

fn pending_of(card: &Card) -> Vec<Proposal> {
    card.params
        .get("items")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect())
        .unwrap_or_default()
}

fn confirm_params(proposals: &[Proposal]) -> Map<String, Value> {
    let mut params = Map::new();
    params.insert("count".into(), json!(proposals.len()));
    params.insert("items".into(), json!(proposals));
    params.insert(CARD_LINES.into(), Value::Array(card_lines(proposals)));
    params
}

fn new_card(group: String, code: &str, action: &str, params: Map<String, Value>, project_id: Option<String>, now_ms: u64, ttl_ms: u64, channel: Channel) -> Card {
    Card {
        id: crate::server::nanty::new_id(),
        project_id,
        group,
        code: code.to_string(),
        params,
        action: Some(action.to_string()),
        token: Some(act::new_token()),
        created_at_ms: now_ms,
        expires_at_ms: now_ms.saturating_add(ttl_ms),
        urgency: Some(Urgency::Normal),
        // Sem som (C-24): a memória nunca acorda a pessoa.
        delivery: Some(Delivery::Silent),
        requested: Some(channel),
        ..Card::default()
    }
}

fn journal(nanty: &Nanty, code: &str, params: &[(&str, Value)]) {
    let mut e = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Conversation, code);
    for (k, v) in params {
        e.params.insert((*k).to_string(), v.clone());
    }
    nanty.record(&e);
}

/// O cartão de confirmação do fim de um turno contaminado: os pedidos dele mais os que ainda esperam,
/// num cartão só (o de antes sai), até [`MAX_PENDING`]. `None` sem pedido.
pub fn flush(nanty: &Nanty, cfg: &Config, proposals: &[Proposal], channel: Channel) -> Option<String> {
    if proposals.is_empty() {
        return None;
    }
    let now = nanty.now().epoch_ms;
    let snap = nanty.snapshot();
    let mut all: Vec<Proposal> = snap
        .cards
        .iter()
        .filter(|c| c.group == GROUP_CONFIRM && c.expires_at_ms > now)
        .flat_map(pending_of)
        .collect();
    for p in proposals {
        if !all.iter().any(|q| q.op == p.op) {
            all.push(p.clone());
        }
    }
    all.drain(..all.len().saturating_sub(MAX_PENDING));
    let card = new_card(GROUP_CONFIRM.into(), CARD_CONFIRM, ACTION_CONFIRM, confirm_params(&all), None, now, cfg.nanty.memory.confirm_ms(), channel);
    let id = card.id.clone();
    nanty.update(|st| {
        st.cards.retain(|c| c.group != GROUP_CONFIRM);
        st.cards.push(card);
        true
    });
    journal(nanty, "memory.proposed", &[("count", json!(all.len())), ("channel", json!(channel))]);
    Some(id)
}

/// O botão de apagar do rodapé "guardei isto": um cartão por entrada, ligado ao projeto dela.
pub fn forget_button(nanty: &Nanty, cfg: &Config, entry: &Entry, channel: Channel) -> String {
    let now = nanty.now().epoch_ms;
    let mut params = Map::new();
    // O título que foi guardado num turno limpo (nunca texto do projeto: o turno contaminado não grava).
    params.insert("note".into(), json!(entry.title));
    params.insert("entry_id".into(), json!(entry.id));
    let group = format!("{GROUP_FORGET}{}", entry.id);
    let card = new_card(group.clone(), CARD_FORGET, ACTION_FORGET, params, entry.source.project_id.clone(), now, cfg.nanty.memory.forget_button_ms(), channel);
    let id = card.id.clone();
    nanty.update(|st| {
        st.cards.retain(|c| c.group != group);
        st.cards.push(card);
        true
    });
    id
}

/// Tira do cartão de confirmação os pedidos que não passam em `keep` (a limpeza de projeto e a
/// varredura); sem pedido, o cartão sai. `true` se mudou.
pub fn retain_in(st: &mut NantyState, keep: impl Fn(&Proposal) -> bool) -> bool {
    let mut changed = false;
    st.cards.retain_mut(|card| {
        if card.group != GROUP_CONFIRM {
            return true;
        }
        let before = pending_of(card);
        let after: Vec<Proposal> = before.iter().filter(|p| keep(p)).cloned().collect();
        if after.len() == before.len() {
            return true;
        }
        changed = true;
        if after.is_empty() {
            return false;
        }
        card.params = confirm_params(&after);
        true
    });
    changed
}

/// A memória nunca muda daqui sem ser o dono: o mesmo "indisponível" das ações.
fn unavailable(nanty: &Nanty) -> bool {
    nanty.status() != InstanceStatus::Active
}

fn outcome(name: &'static str) -> Outcome {
    Outcome { outcome: name, code: format!("nanty.outcome.{name}"), params: Map::new() }
}

pub(super) fn failed(refusal: &ops::Refusal) -> Outcome {
    let mut out = outcome("memory_failed");
    let error = super::api::api_error(refusal);
    out.params.insert("error".into(), json!(error.code().unwrap_or("internal")));
    // Os parâmetros do erro (o teto, por exemplo) vão junto: o texto nunca sai com `{max}`.
    if let Some(params) = error.params() {
        out.params.insert("error_params".into(), Value::Object(params.clone()));
    }
    out
}

/// Decide um cartão (a janela e o Telegram): os da memória aqui, os outros em [`act::decide`]. O token
/// que não confere, o cartão vencido e o já decidido seguem a regra comum de `act`.
pub async fn decide(state: &AppState, card_id: &str, token: &str, answer: Answer, by: DecidedBy) -> Result<Outcome, ApiError> {
    let nanty = Arc::clone(state.nanty());
    let now_ms = nanty.now().epoch_ms;
    let mine = nanty
        .snapshot()
        .cards
        .iter()
        .find(|c| c.id == card_id && is_memory_card(c) && c.token.as_deref() == Some(token) && c.expires_at_ms > now_ms)
        .cloned();
    let Some(card) = mine else { return act::decide(state, card_id, token, answer, by).await };
    if unavailable(&nanty) {
        return Ok(outcome("unavailable"));
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
        return Ok(outcome("declined"));
    }
    let cfg = state.config().read().await.clone();
    let channel = if by == DecidedBy::Telegram { Channel::Telegram } else { Channel::Web };
    if card.action.as_deref() == Some(ACTION_FORGET) {
        let id = card.params.get("entry_id").and_then(Value::as_str).unwrap_or_default();
        return Ok(match ops::delete(&nanty, &cfg, id) {
            Ok(_) => {
                journal(&nanty, "memory.forgotten", &[("by", json!(channel))]);
                outcome("memory_forgotten")
            }
            Err(r) => failed(&r),
        });
    }
    Ok(confirm(&nanty, &cfg, &pending_of(&card), channel))
}

/// O "sim" do cartão de confirmação: cada pedido vale como se a pessoa o tivesse feito agora
/// (`person_confirmed`), com o mesmo texto que ela viu.
fn confirm(nanty: &Nanty, cfg: &Config, proposals: &[Proposal], channel: Channel) -> Outcome {
    let now = nanty.now().epoch_ms;
    let mut done = 0u64;
    let mut refusal = None;
    for p in proposals {
        let result = match &p.op {
            Op::Remember(r) => {
                let source = Source { kind: SourceKind::PersonConfirmed, message_id: p.message_id.clone(), channel: Some(channel), project_id: r.project_id.clone(), at_ms: now };
                ops::save(nanty, cfg, r, source).map(|_| ())
            }
            Op::Forget { id, .. } => ops::delete(nanty, cfg, id).map(|_| ()),
        };
        match result {
            Ok(()) => done += 1,
            Err(r) => refusal = Some(r),
        }
    }
    journal(nanty, "memory.confirmed", &[("count", json!(done)), ("by", json!(channel))]);
    match refusal {
        Some(r) if done == 0 => failed(&r),
        _ => outcome("memory_saved"),
    }
}
