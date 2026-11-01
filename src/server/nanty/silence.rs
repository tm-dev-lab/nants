//! Silêncio pedido na conversa (spec 139 T04, G3, C-07 e C-08).
//!
//! - **Aplicado direto:** o silêncio de tudo só reduz avisos, tem validade (até
//!   [`super::chat::SILENCE_MAX_HOURS`]) e não muda canal, token nem destinatário. O comando e a
//!   ferramenta `set_silence` (num turno sem dado não confiável) o ligam na hora ([`apply`]).
//! - **A confirmação é do servidor:** o cartão do grupo [`GROUP_END`] diz, pelo dicionário, até quando
//!   vale (a hora calculada aqui, nunca a do modelo) e leva o botão para desligar
//!   ([`ActionId::EndSilence`]), no Telegram e na janela.
//! - **Turno contaminado (C-07):** depois de uma ferramenta não confiável (ou com uma mensagem
//!   encaminhada no contexto), o silêncio vira cartão de confirmação ([`propose`],
//!   [`ActionId::SetSilence`]): só o clique o liga.
//! - **Duração ou hora do relógio (spec 139 T12):** o pedido ([`Request`]) é um número de horas ou
//!   uma hora `HH:MM`, a próxima ocorrência ([`next_clock`]), nunca além das 24 horas. O cartão do
//!   turno contaminado guarda o mesmo pedido e vence na hora que mostra: o clique recalcula, sem
//!   nunca passar dela ([`card_end`]).
//! - **O urgente continua chegando, sem som:** o filtro trata o silêncio de tudo como o horário de
//!   silêncio ([`super::filter`]).

use serde_json::{json, Map};

use super::actions::ActionId;
use super::journal::{JournalEntry, JournalKind};
use super::store::{Card, Channel, Delivery, NantyState, Silence};
use super::Nanty;
use crate::config::{Config, Urgency};

/// Grupo do cartão de confirmação e o do cartão de desligar (um cartão por grupo).
pub const GROUP_SET: &str = "*:set_silence";
pub const GROUP_END: &str = "*:end_silence";
/// O porquê do cartão de confirmação e o do cartão de desligar.
pub const CARD_SET: &str = "nanty.card.set_silence";
pub const CARD_END: &str = "nanty.card.end_silence";
const HOUR_MS: u64 = 60 * 60 * 1000;
const MINUTE_MS: u64 = 60 * 1000;
const DAY_MINUTES: u64 = 24 * 60;

/// O que a pessoa pediu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Por algumas horas.
    Hours(u64),
    /// Até a próxima ocorrência de uma hora do relógio (hora, minuto).
    Clock(u8, u8),
}

impl Request {
    /// O fim do silêncio pedido, a partir de agora.
    pub fn end(self, now_ms: u64) -> u64 {
        match self {
            Request::Hours(hours) => until_ms(now_ms, hours),
            // Sem a hora local (nunca visto), vale a duração padrão.
            Request::Clock(hour, minute) => match super::clock::local_time_at(now_ms) {
                Some(now) => next_clock(&now, hour, minute),
                None => until_ms(now_ms, super::chat::SILENCE_DEFAULT_HOURS),
            },
        }
    }

    /// O pedido guardado num cartão (o clique o recalcula); sem nada, a duração padrão.
    pub fn of_card(params: &Map<String, serde_json::Value>) -> Self {
        let clock = params.get(CLOCK_PARAM).and_then(serde_json::Value::as_str).and_then(parse_clock);
        match clock {
            Some((hour, minute)) => Request::Clock(hour, minute),
            None => Request::Hours(params.get("hours").and_then(serde_json::Value::as_u64).unwrap_or(super::chat::SILENCE_DEFAULT_HOURS)),
        }
    }

    fn store(self, params: &mut Map<String, serde_json::Value>) {
        match self {
            Request::Hours(hours) => params.insert("hours".into(), json!(hours.clamp(1, super::chat::SILENCE_MAX_HOURS))),
            Request::Clock(hour, minute) => params.insert(CLOCK_PARAM.into(), json!(format!("{hour:02}:{minute:02}"))),
        };
    }
}

/// Parâmetro do cartão com a hora do relógio pedida.
const CLOCK_PARAM: &str = "clock";
/// Parâmetro do cartão com o fim mostrado nele, em ms.
const SHOWN_PARAM: &str = "until_ms";

/// O fim do silêncio no clique do cartão: o pedido recalculado, nunca além da hora mostrada.
pub fn card_end(params: &Map<String, serde_json::Value>, now_ms: u64) -> u64 {
    let end = Request::of_card(params).end(now_ms);
    params.get(SHOWN_PARAM).and_then(serde_json::Value::as_u64).map_or(end, |shown| end.min(shown))
}

/// Hora do relógio `HH:MM` (de 00:00 a 23:59), exata; qualquer outra forma é `None`.
pub fn parse_clock(raw: &str) -> Option<(u8, u8)> {
    let (hour, minute) = raw.trim().split_once(':')?;
    let number = |part: &str| (part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit())).then(|| part.parse::<u8>().ok()).flatten();
    let (hour, minute) = (number(hour)?, number(minute)?);
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// A próxima ocorrência da hora do relógio, no minuto exato. A hora que já passou é a de amanhã; o
/// minuto atual também, então o fim fica sempre depois de agora e nunca além de 24 horas.
pub fn next_clock(now: &super::clock::LocalTime, hour: u8, minute: u8) -> u64 {
    let current = u64::from(now.hour) * 60 + u64::from(now.minute);
    let target = u64::from(hour) * 60 + u64::from(minute);
    let ahead = match (target + DAY_MINUTES - current) % DAY_MINUTES {
        0 => DAY_MINUTES,
        minutes => minutes,
    };
    (now.epoch_ms - now.epoch_ms % MINUTE_MS) + ahead * MINUTE_MS
}

/// Hora local `HH:MM`.
pub fn hhmm(at_ms: u64) -> String {
    super::clock::local_time_at(at_ms).map(|t| format!("{:02}:{:02}", t.hour, t.minute)).unwrap_or_default()
}

/// O fim do silêncio de `hours` horas a partir de agora (até o máximo).
pub fn until_ms(now_ms: u64, hours: u64) -> u64 {
    now_ms.saturating_add(hours.clamp(1, super::chat::SILENCE_MAX_HOURS).saturating_mul(HOUR_MS))
}

/// O silêncio de tudo ainda valendo (o fim mais longo).
pub fn global_until(st: &NantyState, now_ms: u64) -> Option<u64> {
    st.silences.iter().filter(|s| s.project_id.is_none() && s.group.is_none() && s.until_ms > now_ms).map(|s| s.until_ms).max()
}

fn is_silence_card(c: &Card) -> bool {
    c.group == GROUP_SET || c.group == GROUP_END
}

fn card(action: ActionId, code: &str, params: Map<String, serde_json::Value>, now_ms: u64, expires_ms: u64, channel: Channel) -> Card {
    Card {
        id: super::new_id(),
        project_id: None,
        group: if action == ActionId::SetSilence { GROUP_SET } else { GROUP_END }.to_string(),
        code: code.to_string(),
        params,
        action: Some(action.as_str().to_string()),
        token: Some(super::act::new_token()),
        created_at_ms: now_ms,
        expires_at_ms: expires_ms,
        urgency: Some(Urgency::Normal),
        delivery: Some(Delivery::Push),
        // Pedido na conversa: pelo Telegram vai ao chat logo; pela janela fica só nela.
        requested: Some(channel),
        ..Card::default()
    }
}

/// Liga o silêncio de tudo até `until` no estado e põe o cartão com o botão de desligar (o de antes
/// sai). Devolve o id do cartão.
pub fn apply_in(st: &mut NantyState, now_ms: u64, until: u64, channel: Channel) -> String {
    st.silences.retain(|s| s.project_id.is_some() || s.group.is_some());
    st.silences.push(Silence { project_id: None, group: None, until_ms: until });
    st.cards.retain(|c| !is_silence_card(c));
    let mut params = Map::new();
    params.insert("until".into(), json!(hhmm(until)));
    let off = card(ActionId::EndSilence, CARD_END, params, now_ms, until, channel);
    let id = off.id.clone();
    st.cards.push(off);
    id
}

/// Desliga o silêncio de tudo no estado e tira os cartões do silêncio. `true` se mudou.
pub fn end_in(st: &mut NantyState) -> bool {
    let before = (st.silences.len(), st.cards.len());
    st.silences.retain(|s| s.project_id.is_some() || s.group.is_some());
    st.cards.retain(|c| !is_silence_card(c));
    before != (st.silences.len(), st.cards.len())
}

fn journal(nanty: &Nanty, code: &str, request: Option<Request>, channel: Channel) {
    let mut e = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Conversation, code);
    if let Some(request) = request {
        request.store(&mut e.params);
    }
    e.params.insert("channel".into(), json!(channel));
    nanty.record(&e);
}

/// Liga o silêncio de tudo agora (o comando e a ferramenta num turno limpo). Devolve o fim e o id do
/// cartão de desligar.
pub fn apply(nanty: &Nanty, request: Request, channel: Channel) -> (u64, String) {
    let now_ms = nanty.now().epoch_ms;
    let until = request.end(now_ms);
    let mut id = String::new();
    nanty.update(|st| {
        id = apply_in(st, now_ms, until, channel);
        true
    });
    journal(nanty, "silence.on", Some(request), channel);
    (until, id)
}

/// O cartão de confirmação do silêncio (o turno contaminado): nada muda até o clique. Devolve o fim
/// calculado agora (o clique recalcula a partir dele) e o id do cartão.
pub fn propose(nanty: &Nanty, cfg: &Config, request: Request, channel: Channel) -> (u64, String) {
    let now_ms = nanty.now().epoch_ms;
    let until = request.end(now_ms);
    let mut params = Map::new();
    request.store(&mut params);
    params.insert("until".into(), json!(hhmm(until)));
    params.insert(SHOWN_PARAM.into(), json!(until));
    // Depois da hora mostrada, o cartão não vale mais: um clique atrasado não estende o silêncio.
    let expires = now_ms.saturating_add(super::filter::card_ttl(&cfg.nanty, true)).min(until);
    let ask = card(ActionId::SetSilence, CARD_SET, params, now_ms, expires, channel);
    let id = ask.id.clone();
    nanty.update(|st| {
        // O cartão de desligar (se houver) segue valendo até o clique na confirmação nova.
        st.cards.retain(|c| c.group != GROUP_SET);
        st.cards.push(ask);
        true
    });
    journal(nanty, "silence.proposed", Some(request), channel);
    (until, id)
}

/// Desliga o silêncio de tudo (o comando, a ferramenta com zero horas). `true` se havia algo.
pub fn end(nanty: &Nanty, channel: Channel) -> bool {
    let changed = nanty.update(end_in);
    journal(nanty, "silence.off", None, channel);
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::nanty::clock::at;

    /// 19:40:30 num dia qualquer: o relógio com a época alinhada ao minuto mais 30 s.
    const NOW_MS: u64 = 1_800_000_000_000 + 30_000;

    fn minutes(end: u64) -> u64 {
        (end - (NOW_MS - 30_000)) / MINUTE_MS
    }

    #[test]
    fn the_clock_is_the_next_occurrence_and_never_past_a_day() {
        let now = at((2026, 10, 31), 19, 40, NOW_MS);
        assert_eq!(minutes(next_clock(&now, 21, 0)), 80, "ainda hoje, no minuto exato");
        assert_eq!(minutes(next_clock(&now, 9, 0)), 13 * 60 + 20, "já passou: amanhã");
        assert_eq!(minutes(next_clock(&now, 19, 41)), 1);
        assert_eq!(minutes(next_clock(&now, 19, 39)), DAY_MINUTES - 1);
        let same = next_clock(&now, 19, 40);
        assert_eq!(minutes(same), DAY_MINUTES, "o minuto atual é o de amanhã");
        assert!(same > NOW_MS && same - NOW_MS <= 24 * HOUR_MS, "sempre depois de agora, até 24 horas");
    }

    #[test]
    fn only_a_real_clock_time_is_accepted() {
        assert_eq!(parse_clock("09:00"), Some((9, 0)));
        assert_eq!(parse_clock(" 23:59 "), Some((23, 59)));
        assert_eq!(parse_clock("00:00"), Some((0, 0)));
        for bad in ["", "9", "9:00", "09:0", "24:00", "33:00", "12:60", "09:00:00", "21h", "-1:00", "ab:cd", "2026-11-01T09:00"] {
            assert_eq!(parse_clock(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_card_keeps_the_same_request() {
        for request in [Request::Hours(3), Request::Clock(9, 5)] {
            let mut params = Map::new();
            request.store(&mut params);
            assert_eq!(Request::of_card(&params), request);
        }
        assert_eq!(Request::of_card(&Map::new()), Request::Hours(crate::server::nanty::chat::SILENCE_DEFAULT_HOURS));
    }
}
