//! O que a pessoa viu na conversa é o que o modelo vê (spec 141 T06, G4, C-07).
//!
//! - **Fatos do servidor por mensagem:** a mensagem do Nanty que cita cartões grava, na hora em que
//!   entra na conversa, os fatos de cada um ([`NoticeFact`]): o cartão, o projeto, o código do sinal,
//!   o `since` e os números do porquê. No histórico, eles abrem a mensagem num cabeçalho de dado
//!   selado ([`header`]), com o nome validado do projeto ([`Names`]). Nada do texto do modelo nem do
//!   projeto entra nele: o nome vem da configuração, sem controle nem `<>`, até [`NAME_MAX_CHARS`].
//! - **Eventos:** a decisão de um cartão (quem, quando, qual), a resolução e a expiração viram
//!   mensagens de evento na conversa ([`Pending`], gravadas por [`super::Nanty::update`]), em vez de
//!   reescrever a mensagem antiga: o cache de prefixo do provedor continua valendo. A janela as mostra
//!   como selo; o histórico as leva como dado ([`event_entry`]). A decisão entra sempre; a resolução e
//!   a expiração, só do cartão que a pessoa viu (citado na conversa ou enviado ao Telegram).
//! - **Foco:** o projeto do último aviso, decisão ou resposta a um aviso nas últimas mensagens vai no
//!   fim do contexto ([`focus`]), só como fato. A resposta do Telegram a um aviso liga a pergunta ao
//!   cartão ([`NoticeRef`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::store::{Card, Channel, ConversationMessage, DecidedBy, NantyState, Speaker};
use super::turn::is_code;
use crate::config::Config;
use crate::server::events::SecretSet;

/// Teto do nome do projeto no cabeçalho, em caracteres.
pub const NAME_MAX_CHARS: usize = 64;
/// Fatos por mensagem, no máximo.
pub const MAX_FACTS: usize = 8;
/// Mensagens do fim da janela em que o foco é procurado.
pub const FOCUS_LOOKBACK: usize = 8;
/// Respostas do Telegram que ainda ligam a um aviso.
pub const MAX_NOTICE_REFS: usize = 100;
/// Prefixo dos códigos de evento.
pub const EVENT_PREFIX: &str = "nanty.event.";
/// Os números do porquê que vão nos fatos (os únicos parâmetros numéricos dos `nanty.why.*`).
const NUMBERS: [&str; 2] = ["count", "min_gb"];

/// Os fatos de um cartão, só de campos do servidor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeFact {
    pub card_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// O tipo do sinal (`plan_waiting`), ou o código do cartão que não é aviso de observador.
    pub signal: String,
    /// `HH:MM` em que a condição foi vista.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gb: Option<u64>,
}

/// `HH:MM` com dígitos.
fn is_hhmm(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 5 && b[2] == b':' && [0, 1, 3, 4].iter().all(|i| b[*i].is_ascii_digit())
}

impl NoticeFact {
    /// Os fatos de um cartão. `None` se o id não tem a forma de código.
    pub fn of(card: &Card) -> Option<Self> {
        if !is_code(&card.id) || !is_code(&card.code) {
            return None;
        }
        let number = |key: &str| card.params.get(key).and_then(Value::as_u64);
        Some(Self {
            card_id: card.id.clone(),
            project_id: card.project_id.clone().filter(|p| is_code(p)),
            signal: card.code.strip_prefix("nanty.why.").unwrap_or(&card.code).to_string(),
            since: card.params.get("since").and_then(Value::as_str).filter(|s| is_hhmm(s)).map(str::to_string),
            count: number(NUMBERS[0]),
            min_gb: number(NUMBERS[1]),
        })
    }

    /// O fato como vai ao modelo, com o nome validado do projeto.
    fn value(&self, names: &Names) -> Value {
        let mut v = json!({ "card_id": self.card_id, "signal": self.signal });
        if let Some(p) = &self.project_id {
            v["project_id"] = json!(p);
            if let Some(name) = names.get(p) {
                v["project"] = json!(name);
            }
        }
        if let Some(since) = &self.since {
            v["since"] = json!(since);
        }
        for (key, value) in NUMBERS.iter().zip([self.count, self.min_gb]) {
            if let Some(n) = value {
                v[*key] = json!(n);
            }
        }
        v
    }
}

/// Os fatos dos cartões citados, os que ainda estão no estado.
pub fn facts_of(st: &NantyState, card_ids: &[String]) -> Vec<NoticeFact> {
    card_ids
        .iter()
        .filter_map(|id| st.cards.iter().find(|c| &c.id == id))
        .filter_map(NoticeFact::of)
        .take(MAX_FACTS)
        .collect()
}

/// Os nomes dos projetos observados, como vão ao modelo.
pub type Names = BTreeMap<String, String>;

/// O nome validado: o de [`super::tools::project_name`] (sem controle e redigido), sem `<` nem `>`,
/// até [`NAME_MAX_CHARS`].
pub fn safe_name(secrets: &SecretSet, project: &crate::config::Project) -> String {
    super::tools::project_name(secrets, project)
        .chars()
        .filter(|c| !c.is_control() && *c != '<' && *c != '>')
        .take(NAME_MAX_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Os nomes dos projetos que o Nanty observa (C-16: o resto nem entra).
pub fn names(cfg: &Config) -> Names {
    let secrets = SecretSet::from_config(cfg);
    cfg.projects.iter().filter(|p| cfg.nanty.observes(&p.id)).map(|p| (p.id.clone(), safe_name(&secrets, p))).collect()
}

/// O cabeçalho de dado da mensagem: os fatos do servidor, selados. `None` sem fato.
pub fn header(names: &Names, facts: &[NoticeFact]) -> Option<String> {
    if facts.is_empty() {
        return None;
    }
    Some(super::tools::seal(&json!({ "server_facts": facts_value(names, facts) }).to_string()))
}

/// A lista dos fatos como vai ao modelo. Nada nela tem `<` nem `>`: os ids são códigos, e o nome
/// passou por [`safe_name`].
pub fn facts_value(names: &Names, facts: &[NoticeFact]) -> Value {
    Value::Array(facts.iter().map(|f| f.value(names)).collect())
}

// ---- Eventos ----

/// O tipo de um evento de cartão.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Decided(DecidedBy),
    Resolved,
    Expired,
}

impl EventKind {
    pub fn code(self) -> &'static str {
        match self {
            EventKind::Decided(DecidedBy::Web) => "nanty.event.decided.web",
            EventKind::Decided(DecidedBy::Telegram) => "nanty.event.decided.telegram",
            EventKind::Decided(DecidedBy::Auto) => "nanty.event.decided.auto",
            EventKind::Resolved => "nanty.event.resolved",
            EventKind::Expired => "nanty.event.expired",
        }
    }

    fn name(self) -> &'static str {
        match self {
            EventKind::Decided(_) => "decided",
            EventKind::Resolved => "resolved",
            EventKind::Expired => "expired",
        }
    }
}

/// As respostas de decisão que viram parâmetro (as do dicionário).
const ANSWERS: [&str; 4] = ["yes", "no", "later", "undo"];

/// Um evento a gravar, visto na mudança do estado.
#[derive(Debug, Clone)]
pub struct Pending {
    pub fact: NoticeFact,
    pub kind: EventKind,
    pub answer: Option<String>,
    pub at_ms: u64,
    /// Só entra se a pessoa viu o cartão: a decisão não precisa (ela decidiu).
    pub needs_seen: bool,
    /// O cartão foi ao Telegram.
    pub sent: bool,
}

/// Os cartões que saíram do estado com uma decisão, uma resolução ou a validade vencida. A troca de
/// id (a oferta posta no cartão) e a limpeza de projeto não são eventos.
pub fn events(before: &[Card], st: &NantyState, now_ms: u64) -> Vec<Pending> {
    let mut out = Vec::new();
    for card in before.iter().filter(|c| !st.cards.iter().any(|n| n.id == c.id)) {
        let Some(fact) = NoticeFact::of(card) else { continue };
        let sent = st.telegram_sent.iter().any(|r| r.card_id == card.id);
        let decided = st.decided.iter().rev().find(|d| d.card_id == card.id);
        let resolved = st.done.iter().rev().find(|d| d.card_id.as_deref() == Some(card.id.as_str()) && d.result == super::filter::RESOLVED);
        let (kind, answer, at_ms, needs_seen) = match (decided, resolved) {
            (Some(d), _) => (EventKind::Decided(d.decided_by), Some(d.answer.clone()), d.at_ms, false),
            (None, Some(done)) => (EventKind::Resolved, None, done.at_ms, true),
            (None, None) if card.expires_at_ms <= now_ms => (EventKind::Expired, None, now_ms, true),
            _ => continue,
        };
        let answer = answer.filter(|a| ANSWERS.contains(&a.as_str()));
        out.push(Pending { fact, kind, answer, at_ms, needs_seen, sent });
    }
    out
}

/// A mensagem do evento: só do servidor, limpa, com os fatos do cartão.
pub fn event_message(p: &Pending) -> ConversationMessage {
    let mut params = Map::new();
    params.insert("time".into(), json!(super::tools::hhmm(p.at_ms)));
    if let Some(answer) = &p.answer {
        params.insert("answer".into(), json!(answer));
    }
    let channel = if p.kind == EventKind::Decided(DecidedBy::Telegram) { Channel::Telegram } else { Channel::Web };
    let mut m = ConversationMessage::from_nanty(super::new_id(), p.at_ms, channel, p.kind.code(), params);
    m.project_id = p.fact.project_id.clone();
    m.notices = vec![p.fact.clone()];
    m
}

/// A mensagem é um evento de cartão.
pub fn is_event(m: &ConversationMessage) -> bool {
    m.from == Speaker::Nanty && m.text.is_none() && m.code.as_deref().is_some_and(|c| c.starts_with(EVENT_PREFIX))
}

/// O tipo e quem decidiu, pelo código gravado.
fn event_fields(code: &str) -> Option<(&'static str, Option<&'static str>)> {
    let rest = code.strip_prefix(EVENT_PREFIX)?;
    match rest {
        "decided.web" => Some(("decided", Some("web"))),
        "decided.telegram" => Some(("decided", Some("telegram"))),
        "decided.auto" => Some(("decided", Some("auto"))),
        "resolved" => Some((EventKind::Resolved.name(), None)),
        "expired" => Some((EventKind::Expired.name(), None)),
        _ => None,
    }
}

/// O evento no histórico: um JSON de dado do servidor, com `at`, o tipo, quem decidiu e a resposta, e
/// os fatos do cartão. Só campos do servidor.
pub fn event_entry(at: &str, m: &ConversationMessage, names: &Names) -> String {
    let (kind, by) = m.code.as_deref().and_then(event_fields).unwrap_or(("unknown", None));
    let mut event = json!({ "event": kind });
    if let Some(by) = by {
        event["by"] = json!(by);
        if let Some(answer) = m.params.get("answer").and_then(Value::as_str).filter(|a| ANSWERS.contains(a)) {
            event["answer"] = json!(answer);
        }
    }
    event["notices"] = facts_value(names, &m.notices);
    super::tools::seal(&json!({ "at": at, "server_event": event }).to_string())
}

// ---- Foco ----

/// O foco do turno: o projeto do assunto corrente, só como fato. A pergunta que responde a um aviso
/// (`reply`) vence; senão, o último aviso ou evento entre as últimas [`FOCUS_LOOKBACK`] mensagens.
pub fn focus(lines: &[ConversationMessage], person: &ConversationMessage, names: &Names) -> Option<Value> {
    let observed = |f: &&NoticeFact| f.project_id.as_deref().is_some_and(|p| names.contains_key(p));
    if let Some(fact) = person.notices.iter().rev().find(observed) {
        return Some(focus_value(fact, "reply", names));
    }
    for m in lines.iter().rev().take(FOCUS_LOOKBACK) {
        let Some(fact) = m.notices.iter().rev().find(observed) else { continue };
        let via = match m.from {
            Speaker::Person => "reply",
            Speaker::Nanty => m.code.as_deref().and_then(event_fields).map_or("notice", |(kind, _)| kind),
        };
        return Some(focus_value(fact, via, names));
    }
    None
}

fn focus_value(fact: &NoticeFact, via: &str, names: &Names) -> Value {
    let mut v = fact.value(names);
    v["via"] = json!(via);
    v
}

// ---- Resposta do Telegram ----

/// Uma mensagem de aviso enviada ao Telegram: a resposta a ela liga a pergunta ao cartão, mesmo
/// depois de o cartão sair do estado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeRef {
    pub chat_id: i64,
    pub message_id: i64,
    pub fact: NoticeFact,
}

/// Os fatos do aviso a que a pessoa respondeu no Telegram, se o projeto é observado.
pub fn replied(st: &NantyState, chat_id: i64, message_id: i64, observes: impl Fn(&str) -> bool) -> Vec<NoticeFact> {
    st.notice_refs
        .iter()
        .rev()
        .find(|r| r.chat_id == chat_id && r.message_id == message_id)
        .map(|r| r.fact.clone())
        .filter(|f| f.project_id.as_deref().is_none_or(&observes))
        .into_iter()
        .collect()
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
