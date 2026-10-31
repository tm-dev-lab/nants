//! Filtro dos avisos do Nanty (spec 138 T02), no modelo do Alertmanager, sem modelo:
//! - **grupo:** projeto e tipo (`<projeto>:<tipo>`, ou `*:<tipo>` sem projeto);
//! - **espera** (`group_wait`): a condição nova só vira aviso depois da espera;
//! - **intervalo do grupo** (`group_interval`): fatos mudaram, avisa de novo só depois dele;
//! - **repetição** (`repeat_interval`): sem mudança, repete só depois dele (o resumo não repete);
//! - **inibição** ([`INHIBITIONS`]): "sem crédito" cala "execução bloqueada" do mesmo projeto;
//! - **silêncios** com validade (`NantyState::silences`);
//! - **horário de silêncio:** o urgente vai sem som e o resto vai para o resumo.
//!
//! **Resolvido:** a condição de um tipo coberto pela rodada que não aparece mais fecha o cartão e
//! vira um "feito" com o resultado `resolved`.
//!
//! **Rodada sem novidade:** sem condição nova, sem fatos novos e sem prazo vencido, nada muda e
//! nada é gravado. O cartão leva só contagens e códigos (C-18), mais o observador e o horário do
//! porquê (C-06).

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::clock::LocalTime;
use super::journal::{JournalEntry, JournalKind};
use super::signals::{urgency_of, Signal, SignalKind, INHIBITIONS};
use super::store::{Alert, Card, Delivery, DigestItem, DoneItem, NantyState};
use crate::config::{NantyConfig, Urgency};

/// Resultado de uma rodada do filtro.
#[derive(Debug, Default)]
pub struct Outcome {
    /// O estado mudou (e precisa ser gravado).
    pub changed: bool,
    /// Linhas do registro (só nas transições).
    pub journal: Vec<JournalEntry>,
}

/// Resultado `resolved` do "feito" quando a condição some.
pub const RESOLVED: &str = "resolved";

fn hhmm(now: &LocalTime) -> String {
    format!("{:02}:{:02}", now.hour, now.minute)
}

/// Como o aviso sai, pela urgência e pelo horário de silêncio.
pub fn delivery_for(urgency: Urgency, quiet: bool) -> Delivery {
    match (urgency, quiet) {
        (Urgency::Digest, _) | (Urgency::Normal, true) => Delivery::Digest,
        (Urgency::Urgent, true) => Delivery::Silent,
        (_, false) => Delivery::Push,
    }
}

/// O silêncio vale para o sinal: do projeto, do grupo ou de tudo, ainda dentro da validade.
fn silenced(st: &NantyState, signal: &Signal, now_ms: u64) -> bool {
    let group = signal.group();
    st.silences.iter().any(|s| {
        s.until_ms > now_ms
            && s.project_id.as_deref().is_none_or(|p| signal.project_id.as_deref() == Some(p))
            && s.group.as_deref().is_none_or(|g| g == group)
    })
}

/// Outro sinal ativo do mesmo projeto cala este (tabela [`INHIBITIONS`]).
fn inhibited(signal: &Signal, active: &BTreeMap<String, &Signal>) -> bool {
    INHIBITIONS.iter().any(|(source, target)| {
        *target == signal.kind
            && active.values().any(|other| other.kind == *source && other.project_id == signal.project_id)
    })
}

fn entry(now_ms: u64, kind: JournalKind, code: &str, project_id: Option<&str>, params: Map<String, Value>) -> JournalEntry {
    let mut e = JournalEntry::new(now_ms, kind, code);
    e.project_id = project_id.map(str::to_string);
    e.params = params;
    e
}

fn filter_params(signal: &Signal, urgency: Urgency, delivery: Option<Delivery>) -> Map<String, Value> {
    let mut p = Map::new();
    p.insert("kind".into(), json!(signal.kind.as_str()));
    p.insert("group".into(), json!(signal.group()));
    p.insert("urgency".into(), json!(urgency));
    if let Some(d) = delivery {
        p.insert("delivery".into(), json!(d));
    }
    p
}

/// Parâmetros do cartão e do resumo: os fatos (contagens e códigos), o observador, o horário em que
/// a condição foi vista e a idade em minutos.
fn card_params(signal: &Signal, since: &str, age_ms: u64) -> Map<String, Value> {
    let mut p = signal.facts.clone();
    p.extend(signal.extra.clone());
    p.insert("observer".into(), json!(signal.kind.observer().as_str()));
    p.insert("since".into(), json!(since));
    p.insert("age_min".into(), json!(age_ms / 60_000));
    p
}

/// Validade do cartão: a condição aberta fica até o dobro da repetição (cada repetição renova); o
/// marco, até a repetição.
pub(super) fn card_ttl(cfg: &NantyConfig, one_shot: bool) -> u64 {
    let repeat = cfg.filter.repeat_interval_ms().max(60_000);
    if one_shot {
        repeat
    } else {
        repeat.saturating_mul(2)
    }
}

/// Abre (ou renova, com o mesmo id) o cartão do grupo, ou guarda no resumo.
#[allow(clippy::too_many_arguments)]
fn deliver(
    st: &mut NantyState,
    signal: &Signal,
    urgency: Urgency,
    delivery: Delivery,
    params: Map<String, Value>,
    to_digest: bool,
    now_ms: u64,
    ttl: u64,
) {
    let group = signal.group();
    if to_digest {
        st.push_digest(DigestItem {
            at_ms: now_ms,
            project_id: signal.project_id.clone(),
            group: group.clone(),
            code: signal.kind.why_code(),
            params: params.clone(),
        });
    }
    if urgency == Urgency::Digest {
        return;
    }
    let card = Card {
        id: String::new(),
        project_id: signal.project_id.clone(),
        group: group.clone(),
        code: signal.kind.why_code(),
        params,
        created_at_ms: now_ms,
        expires_at_ms: now_ms.saturating_add(ttl),
        urgency: Some(urgency),
        delivery: Some(delivery),
        ..Card::default()
    };
    // Um cartão por grupo: a renovação mantém o id e a ação oferecida (T03), com o mesmo token.
    match st.cards.iter_mut().find(|c| c.group == group) {
        Some(open) => {
            let id = std::mem::take(&mut open.id);
            let (action, plan_hash, spec_id, token) =
                (open.action.take(), open.plan_hash.take(), open.spec_id.take(), open.token.take());
            *open = Card { id, action, plan_hash, spec_id, token, ..card };
        }
        None => st.cards.push(Card { id: super::new_id(), ..card }),
    }
}

/// Fecha o grupo: tira a condição e o último envio; o cartão aberto vira um "feito" `resolved`.
fn resolve(st: &mut NantyState, group: &str, now_ms: u64, out: &mut Outcome) {
    let Some(alert) = st.alerts.remove(group) else { return };
    st.last_sent.remove(group);
    out.changed = true;
    let closed: Vec<Card> = st.cards.iter().filter(|c| c.group == group).cloned().collect();
    st.cards.retain(|c| c.group != group);
    for card in closed {
        st.push_done(DoneItem {
            at_ms: now_ms,
            project_id: card.project_id.clone(),
            code: card.code.clone(),
            params: card.params.clone(),
            action: None,
            decided_by: None,
            result: RESOLVED.to_string(),
            card_id: Some(card.id.clone()),
            undo: None,
        });
    }
    let mut params = Map::new();
    params.insert("kind".into(), json!(alert.kind));
    params.insert("group".into(), json!(group));
    out.journal.push(entry(now_ms, JournalKind::Filter, RESOLVED, alert.project_id.as_deref(), params));
}

/// Já há um cartão ou um item do resumo do grupo com os mesmos fatos (evento repetido). Marco sem
/// fato nenhum não tem como ser reconhecido: nunca é dado como repetido.
fn already_delivered(st: &NantyState, group: &str, facts: &Map<String, Value>) -> bool {
    if facts.is_empty() {
        return false;
    }
    let same = |params: &Map<String, Value>| facts.iter().all(|(k, v)| params.get(k) == Some(v));
    st.cards.iter().any(|c| c.group == group && same(&c.params))
        || st.digest.iter().any(|d| d.group == group && same(&d.params))
}

/// Uma rodada: `signals` é o que os observadores viram; `covered` são os tipos de condição que a
/// rodada observou por inteiro (a condição desses tipos que sumiu está resolvida). Marcos
/// ([`SignalKind::one_shot`]) avisam uma vez e nunca se resolvem.
pub fn apply(
    st: &mut NantyState,
    signals: &[Signal],
    covered: &[SignalKind],
    cfg: &NantyConfig,
    now: LocalTime,
) -> Outcome {
    let now_ms = now.epoch_ms;
    let quiet = now.within(cfg.quiet_window());
    let filter = &cfg.filter;
    let mut out = Outcome { changed: st.prune_expired(now_ms), journal: Vec::new() };

    let mut active: BTreeMap<String, &Signal> = BTreeMap::new();
    let mut one_shots: Vec<&Signal> = Vec::new();
    for signal in signals {
        if signal.kind.one_shot() {
            one_shots.push(signal);
        } else {
            active.insert(signal.group(), signal);
        }
    }

    // Resolvido: tipo coberto pela rodada e sem sinal agora (tipo desconhecido também sai).
    let gone: Vec<String> = st
        .alerts
        .iter()
        .filter(|(group, alert)| {
            !active.contains_key(*group) && SignalKind::parse(&alert.kind).is_none_or(|k| covered.contains(&k))
        })
        .map(|(group, _)| group.clone())
        .collect();
    for group in gone {
        resolve(st, &group, now_ms, &mut out);
    }

    for (group, signal) in &active {
        // Condição nova ou com fatos novos.
        match st.alerts.get_mut(group) {
            None => {
                st.alerts.insert(
                    group.clone(),
                    Alert {
                        kind: signal.kind.as_str().to_string(),
                        project_id: signal.project_id.clone(),
                        facts: signal.facts.clone(),
                        first_seen_ms: now_ms,
                        since: hhmm(&now),
                        notified: None,
                        suppressed: None,
                    },
                );
                out.changed = true;
                out.journal.push(entry(
                    now_ms,
                    JournalKind::Signal,
                    signal.kind.as_str(),
                    signal.project_id.as_deref(),
                    signal.facts.clone(),
                ));
            }
            Some(alert) if alert.facts != signal.facts => {
                alert.facts = signal.facts.clone();
                out.changed = true;
                out.journal.push(entry(
                    now_ms,
                    JournalKind::Signal,
                    signal.kind.as_str(),
                    signal.project_id.as_deref(),
                    signal.facts.clone(),
                ));
            }
            Some(_) => {}
        }

        let urgency = urgency_of(signal.kind, cfg);
        let suppression = if inhibited(signal, &active) {
            Some("inhibited")
        } else if silenced(st, signal, now_ms) {
            Some("silenced")
        } else {
            None
        };
        let last = st.last_sent.get(group).copied();
        let Some(alert) = st.alerts.get_mut(group) else { continue };
        if alert.suppressed.as_deref() != suppression {
            alert.suppressed = suppression.map(str::to_string);
            out.changed = true;
            if let Some(code) = suppression {
                out.journal.push(entry(
                    now_ms,
                    JournalKind::Filter,
                    code,
                    signal.project_id.as_deref(),
                    filter_params(signal, urgency, None),
                ));
            }
        }
        if suppression.is_some() {
            continue;
        }
        let since_last = |at: Option<u64>| at.map_or(u64::MAX, |at| now_ms.saturating_sub(at));
        let reason = match &alert.notified {
            None => (now_ms.saturating_sub(alert.first_seen_ms) >= filter.group_wait_ms()).then_some("notify"),
            Some(prev) if *prev != alert.facts => (since_last(last) >= filter.group_interval_ms()).then_some("update"),
            // O resumo não repete.
            Some(_) if urgency == Urgency::Digest => None,
            Some(_) => (since_last(last) >= filter.repeat_interval_ms()).then_some("repeat"),
        };
        let Some(reason) = reason else { continue };
        let delivery = delivery_for(urgency, quiet);
        let params = card_params(signal, &alert.since, now_ms.saturating_sub(alert.first_seen_ms));
        alert.notified = Some(alert.facts.clone());
        // A repetição renova o cartão, mas não enfileira o mesmo aviso no resumo outra vez.
        let to_digest = delivery == Delivery::Digest && reason != "repeat";
        deliver(st, signal, urgency, delivery, params, to_digest, now_ms, card_ttl(cfg, false));
        st.last_sent.insert(group.clone(), now_ms);
        out.changed = true;
        out.journal.push(entry(
            now_ms,
            JournalKind::Filter,
            reason,
            signal.project_id.as_deref(),
            filter_params(signal, urgency, Some(delivery)),
        ));
    }

    for signal in one_shots {
        let group = signal.group();
        let urgency = urgency_of(signal.kind, cfg);
        // Evento repetido: nada a registrar.
        if already_delivered(st, &group, &signal.facts) {
            continue;
        }
        out.journal.push(entry(
            now_ms,
            JournalKind::Signal,
            signal.kind.as_str(),
            signal.project_id.as_deref(),
            signal.facts.clone(),
        ));
        if silenced(st, signal, now_ms) {
            out.journal.push(entry(
                now_ms,
                JournalKind::Filter,
                "silenced",
                signal.project_id.as_deref(),
                filter_params(signal, urgency, None),
            ));
            continue;
        }
        let delivery = delivery_for(urgency, quiet);
        let params = card_params(signal, &hhmm(&now), 0);
        deliver(st, signal, urgency, delivery, params, delivery == Delivery::Digest, now_ms, card_ttl(cfg, true));
        st.last_sent.insert(group, now_ms);
        out.changed = true;
        out.journal.push(entry(
            now_ms,
            JournalKind::Filter,
            "notify",
            signal.project_id.as_deref(),
            filter_params(signal, urgency, Some(delivery)),
        ));
    }
    out
}

/// Milissegundos até a próxima condição sair da espera do grupo (para o laço olhar de novo na hora
/// certa, sem esperar a varredura periódica). `None` sem condição esperando.
pub fn next_due_ms(st: &NantyState, cfg: &NantyConfig, now_ms: u64) -> Option<u64> {
    st.alerts
        .values()
        .filter(|a| a.notified.is_none() && a.suppressed.is_none())
        .map(|a| a.first_seen_ms.saturating_add(cfg.filter.group_wait_ms()).saturating_sub(now_ms))
        .min()
}
