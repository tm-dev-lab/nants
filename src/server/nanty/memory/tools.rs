//! As ferramentas da memória na conversa (spec 140 T07, C-22): `remember`, `recall` e `forget`, só no
//! modo de conversa ([`crate::server::nanty::tools::Mode`]).
//!
//! - **`remember`** grava (ou troca, com `replace_id`) uma entrada. Num turno limpo, grava como
//!   `model_clean` e o servidor põe o rodapé "guardei isto" com o botão de apagar. Num turno
//!   contaminado (pela leitura do turno ou pelo histórico na janela, T04), vira pedido no cartão de
//!   confirmação. O texto passa pelo redator antes de tudo, e o que repete uma nota do resumo da
//!   conversa é recusado (C-03).
//! - **`recall`** busca pelo BM25 ([`super::recall`]) e devolve o texto inteiro, como dado; a entrada
//!   importada vai no envelope de dado não confiável e contamina o turno.
//! - **`forget`** apaga (reversível por um tempo); num turno contaminado, vira pedido no cartão.
//! - **Teto por mensagem** ([`MAX_WRITES`]): gravar e apagar somam.

use serde_json::{json, Value};

use super::card::{Op, Proposal, Replaced};
use super::ops::{self, Refusal, Remember};
use super::{Source, SourceKind, Topic, QUERY_MAX_CHARS, RECALL_MAX};
use crate::config::Config;
use crate::i18n::{tr, Lang};
use crate::server::events::SecretSet;
use crate::server::nanty::agent::{envelope, Session};
use crate::server::nanty::provenance::TaintKind;
use crate::server::nanty::summary::SummarySource;
use crate::server::nanty::{chat, tools, turn, Nanty};

/// Gravações e apagamentos atendidos numa mensagem (provisório).
pub const MAX_WRITES: usize = 3;

fn args_of(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or(Value::Null)
}

fn text_arg(args: &Value, name: &str) -> Option<String> {
    args.get(name).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// O id de uma entrada vindo do modelo: 16 hex, como o `new_id`; o resto some.
fn id_arg(args: &Value, name: &str) -> Option<String> {
    text_arg(args, name).filter(|id| id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A frase de uma recusa, para o modelo.
fn refusal_text(lang: Lang, refusal: &Refusal) -> String {
    match refusal {
        Refusal::TitleEmpty | Refusal::TextEmpty => tr(lang, "nanty.tool.memory_empty", &[]),
        Refusal::TitleLong => tr(lang, "nanty.tool.memory_title_long", &[("max", &super::TITLE_MAX_CHARS.to_string())]),
        Refusal::TextLong(max) => tr(lang, "nanty.tool.memory_too_long", &[("max", &max.to_string())]),
        Refusal::Full(max) => tr(lang, "nanty.tool.memory_full", &[("max", &max.to_string())]),
        Refusal::NotFound | Refusal::Gone | Refusal::Changed => tr(lang, "nanty.tool.memory_not_found", &[]),
        Refusal::Duplicate(id) => tr(lang, "nanty.tool.memory_duplicate", &[("id", id)]),
        Refusal::Conflict(_) | Refusal::File(_) => tr(lang, "nanty.tool.memory_unavailable", &[]),
    }
}

/// As notas do modelo no resumo da conversa (o que já está nelas não vira memória).
fn summary_notes(nanty: &Nanty) -> Vec<String> {
    nanty.summary().items.into_iter().filter(|i| i.source == SummarySource::Model).filter_map(|i| i.text).collect()
}

/// `remember(topic, title, text, replace_id?, project_id?)`.
pub fn remember(nanty: &Nanty, cfg: &Config, lang: Lang, secrets: &SecretSet, arguments: &str, session: &mut Session) -> String {
    let args = args_of(arguments);
    let Some(topic) = args.get("topic").and_then(Value::as_str).and_then(Topic::parse) else {
        return tr(lang, "nanty.tool.memory_topic", &[]);
    };
    let (title, text) = match ops::clean(&cfg.nanty.memory, secrets, topic, &text_arg(&args, "title").unwrap_or_default(), &text_arg(&args, "text").unwrap_or_default()) {
        Ok(pair) => pair,
        Err(r) => return refusal_text(lang, &r),
    };
    let project_id = match text_arg(&args, "project_id") {
        Some(id) => match chat::observed(cfg).into_iter().find(|p| p.id == id) {
            Some(p) => Some(p.id.clone()),
            None => return tr(lang, "nanty.tool.unknown_project", &[("id", if turn::is_code(&id) { &id } else { "?" })]),
        },
        None => None,
    };
    // A nota trocada, como está agora: o cartão a mostra, e a troca só vale sobre esta versão.
    let replaced = match args.get("replace_id").filter(|v| !v.is_null()) {
        Some(_) => match id_arg(&args, "replace_id").and_then(|id| nanty.memory().find(&id).cloned()).filter(|e| super::visible(cfg, e)) {
            Some(entry) => Some(entry),
            None => return tr(lang, "nanty.tool.memory_not_found", &[]),
        },
        None => None,
    };
    if session.memory.writes >= MAX_WRITES {
        return tr(lang, "nanty.tool.memory_limit", &[("max", &MAX_WRITES.to_string())]);
    }
    // C-03: o que o resumo da conversa já diz nunca vira memória.
    let notes = summary_notes(nanty);
    let refs: Vec<&str> = notes.iter().map(String::as_str).collect();
    let said = session.person_text.clone().unwrap_or_default();
    if super::from_summary(&text, &refs, &said) || super::from_summary(&title, &refs, &said) {
        return tr(lang, "nanty.tool.memory_from_summary", &[]);
    }
    session.memory.writes += 1;
    if let Some(p) = &project_id {
        if !session.read.contains(p) {
            session.read.push(p.clone());
        }
    }
    let request = Remember {
        topic,
        title,
        text,
        replace_id: replaced.as_ref().map(|e| e.id.clone()),
        project_id,
        base_updated: replaced.as_ref().map(|e| e.updated),
    };
    let now = nanty.now().epoch_ms;
    if session.is_tainted() {
        session.memory.proposals.push(Proposal {
            op: Op::Remember(request),
            message_id: session.person_id.clone(),
            channel: session.channel,
            taint: session.tainted.sources.clone(),
            at_ms: now,
            replaces: replaced.as_ref().map(Replaced::of),
        });
        return tr(lang, "nanty.tool.memory_proposed", &[]);
    }
    let source = Source { kind: SourceKind::ModelClean, message_id: session.person_id.clone(), channel: Some(session.channel), project_id: request.project_id.clone(), at_ms: now };
    match ops::save(nanty, cfg, &request, source) {
        Ok(entry) => {
            let card = super::card::forget_button(nanty, cfg, &entry, session.channel);
            session.memory.cards.push(card.clone());
            session.memory.saved.push(super::Saved { id: entry.id.clone(), title: entry.title.clone(), card: Some(card) });
            tr(lang, "nanty.tool.memory_saved", &[("id", &entry.id)])
        }
        Err(r) => refusal_text(lang, &r),
    }
}

/// `forget(id)`.
pub fn forget(nanty: &Nanty, cfg: &Config, lang: Lang, arguments: &str, session: &mut Session) -> String {
    let args = args_of(arguments);
    let memory = nanty.memory();
    let Some(entry) = id_arg(&args, "id").and_then(|id| memory.find(&id).cloned()).filter(|e| super::visible(cfg, e)) else {
        return tr(lang, "nanty.tool.memory_not_found", &[]);
    };
    if session.memory.writes >= MAX_WRITES {
        return tr(lang, "nanty.tool.memory_limit", &[("max", &MAX_WRITES.to_string())]);
    }
    session.memory.writes += 1;
    if session.is_tainted() {
        session.memory.proposals.push(Proposal {
            op: Op::Forget { id: entry.id.clone(), title: entry.title.clone(), project_id: entry.source.project_id.clone() },
            message_id: session.person_id.clone(),
            channel: session.channel,
            taint: session.tainted.sources.clone(),
            at_ms: nanty.now().epoch_ms,
            replaces: None,
        });
        return tr(lang, "nanty.tool.memory_proposed", &[]);
    }
    match ops::delete(nanty, cfg, &entry.id) {
        Ok(gone) => {
            session.memory.forgotten.push(gone.title);
            tr(lang, "nanty.tool.memory_forgotten", &[])
        }
        Err(r) => refusal_text(lang, &r),
    }
}

/// `recall(query, topic?)`: as entradas que mais casam, com o texto inteiro.
pub fn recall(nanty: &Nanty, cfg: &Config, lang: Lang, arguments: &str, session: &mut Session) -> String {
    let args = args_of(arguments);
    let query: String = text_arg(&args, "query").unwrap_or_default().chars().take(QUERY_MAX_CHARS).collect();
    let topic = args.get("topic").and_then(Value::as_str).and_then(Topic::parse);
    let memory = nanty.memory();
    let hits = super::recall(cfg, &memory, &query, topic, RECALL_MAX);
    if hits.is_empty() {
        return tr(lang, "nanty.tool.memory_recall_none", &[]);
    }
    // Spec 140 T17 (da T10, C-16): a memória importada de um projeto é conteúdo dele, e passa pelo
    // mesmo `admit` da leitura profunda: um segundo projeto só no mesmo pedido do modelo que o primeiro.
    let hits: Vec<_> = hits
        .into_iter()
        .filter(|e| e.source.kind.trusted() || e.source.project_id.as_deref().is_none_or(|p| session.admit(p)))
        .collect();
    if hits.is_empty() {
        return tr(lang, "nanty.tool.memory_recall_none", &[]);
    }
    let items: Vec<Value> = hits
        .iter()
        .map(|e| {
            let trusted = e.source.kind.trusted();
            if !trusted {
                session.read_taint(TaintKind::ImportedMemory, e.source.project_id.as_deref());
            }
            let wrap = |s: &str| if trusted { json!(s) } else { json!(envelope(s)) };
            let mut item = json!({ "id": e.id, "topic": e.topic.as_str(), "title": wrap(&e.title), "text": wrap(&e.text), "source": e.source.kind });
            if let Some(p) = &e.source.project_id {
                item["project_id"] = json!(p);
            }
            item
        })
        .collect();
    let ids: Vec<String> = hits.iter().map(|e| e.id.clone()).collect();
    ops::touch(nanty, cfg, &ids);
    let intro = tr(lang, "nanty.tool.memory_recall_intro", &[]);
    format!("{intro}\n{}", tools::seal(&json!({ "memory": items }).to_string()))
}
