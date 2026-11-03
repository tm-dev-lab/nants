//! Resumo da manhã do Nanty (spec 138 T05).
//!
//! - **Quando:** uma vez por dia local, na hora de `[nanty] digest_time` pelo relógio local (T01,
//!   C-15). Se o Nants estava desligado na hora, sai assim que ele volta, até
//!   [`DIGEST_LATE_MINUTES`] depois; mais tarde que isso, o dia passa sem resumo.
//! - **Do que é feito:** só fatos ([`DigestFacts`]): os projetos observados, quantos estão em
//!   andamento e pausados, o que espera a pessoa (cartões abertos e os avisos guardados para o
//!   resumo, como os da noite), o custo dos projetos no dia anterior e o menor espaço livre em disco.
//! - **O modelo** entra só se houver o que destacar e houver orçamento; o texto dele vai como veio e
//!   com o idioma marcado. Com ele, a lista fixa sai e fica só a frase curta do servidor junto
//!   (spec 139 T06, C-11, [`with_model_text`]); sem ele, a mensagem fixa (`code` + `params`) é a
//!   reserva, com uma frase por fato e sem zeros.
//! - **Dia vazio:** `[nanty] digest_empty` decide entre a mensagem curta e nenhuma.
//! - **Voz (spec 139 T08):** com a voz ligada, o resumo sai como nota de voz com legenda.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::clock::LocalTime;
use super::signals::SignalKind;
use super::store::{Channel, ConversationMessage, DigestItem, NantyState};
use super::turn::{self, SignalFact, TurnInput, TurnKind, TurnOutcome};
use super::{nanty_lang, Nanty};
use crate::config::{Config, DigestEmpty, NantyConfig, Urgency};
use crate::server::state::AppState;

/// Atraso máximo do resumo depois da hora marcada, em minutos (provisório).
pub const DIGEST_LATE_MINUTES: u16 = 180;
/// Avisos guardados listados no resumo (os mais recentes).
pub const MAX_DIGEST_LINES: usize = 10;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Código da mensagem do resumo com fatos.
pub const MORNING: &str = "nanty.digest.morning";
/// Código da mensagem curta do dia vazio.
pub const QUIET: &str = "nanty.digest.quiet";

/// Início do dia local de `now`, em ms.
pub fn day_start_ms(now: &LocalTime) -> u64 {
    now.epoch_ms.saturating_sub(u64::from(now.minute_of_day()) * 60_000 + now.epoch_ms % 60_000)
}

/// Já é hora do resumo de hoje e ele ainda não saiu.
pub fn due(cfg: &NantyConfig, st: &NantyState, now: &LocalTime) -> bool {
    let at = cfg.digest_minute();
    let minute = now.minute_of_day();
    st.digest_day != now.day_key() && minute >= at && minute < at.saturating_add(DIGEST_LATE_MINUTES)
}

/// Quanto falta para a hora do resumo de hoje (para o laço acordar na hora). `None` se já saiu ou
/// se a hora de hoje já passou.
pub fn next_due_ms(cfg: &NantyConfig, st: &NantyState, now: &LocalTime) -> Option<u64> {
    if st.digest_day == now.day_key() {
        return None;
    }
    let at = cfg.digest_minute();
    let minute = now.minute_of_day();
    (minute < at).then(|| u64::from(at - minute) * 60_000 - now.epoch_ms % 60_000)
}

/// Os fatos do resumo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DigestFacts {
    pub projects: u64,
    pub running: u64,
    pub paused: u64,
    /// Cartões abertos esperando a pessoa.
    pub waiting: u64,
    /// Avisos guardados para o resumo (a noite, o que só vai no resumo).
    pub items: Vec<DigestItem>,
    /// Custo dos projetos no dia anterior em US$ (só o que tem preço) e os tokens de todas as chamadas.
    pub cost_yesterday_usd: f64,
    pub tokens_yesterday: u64,
    /// Alguma chamada de ontem não tinha preço (o US$ não é o total).
    pub unpriced_yesterday: bool,
    /// Menor espaço livre medido nas pastas do Nants, em GB.
    pub disk_free_gb: Option<u64>,
    /// Um aviso de disco está aberto.
    pub disk_low: bool,
}

impl DigestFacts {
    /// Há o que destacar: alguém esperando, algo pausado, aviso guardado ou disco baixo.
    pub fn has_highlights(&self) -> bool {
        self.waiting > 0 || self.paused > 0 || !self.items.is_empty() || self.disk_low
    }
}

/// Tipos que contam como execução pausada ou parada.
const PAUSED_KINDS: [SignalKind; 5] =
    [SignalKind::NoCredit, SignalKind::ProviderStopped, SignalKind::RunBlocked, SignalKind::Budget, SignalKind::ReviewLoop];

/// Contagens do estado do Nanty (função pura): pausados (projetos distintos com condição aberta de
/// parada, pausa ou laço de revisão), esperando (cartões abertos fora do resumo), os avisos
/// guardados e o disco.
pub fn state_facts(st: &NantyState, now_ms: u64) -> DigestFacts {
    let mut paused: Vec<&str> = Vec::new();
    for alert in st.alerts.values() {
        let Some(kind) = SignalKind::parse(&alert.kind) else { continue };
        if PAUSED_KINDS.contains(&kind) {
            if let Some(p) = alert.project_id.as_deref() {
                if !paused.contains(&p) {
                    paused.push(p);
                }
            }
        }
    }
    let disk_low =
        st.alerts.values().any(|a| a.kind == SignalKind::DiskLow.as_str() || a.kind == SignalKind::TmpLow.as_str());
    DigestFacts {
        paused: paused.len() as u64,
        waiting: st
            .cards
            .iter()
            .filter(|c| c.expires_at_ms > now_ms && c.urgency != Some(Urgency::Digest) && c.awaits_person())
            .count() as u64,
        items: st.digest.clone(),
        disk_low,
        ..DigestFacts::default()
    }
}

/// Custo dos projetos entre `from_ms` e `to_ms` (o dia anterior), pelo `usage.jsonl` de cada um e
/// os preços da configuração: `(US$ com preço, tokens, alguma chamada sem preço)`.
pub fn projects_cost(roots: &[PathBuf], pricing: &std::collections::BTreeMap<String, crate::config::Pricing>, from_ms: u64, to_ms: u64) -> (f64, u64, bool) {
    let (mut usd, mut tokens, mut unpriced) = (0.0, 0u64, false);
    for root in roots {
        for record in crate::server::usage::load_records(root, None) {
            let Some(secs) = crate::server::usage::parse_utc(&record.at) else { continue };
            let at = secs.saturating_mul(1000);
            if at < from_ms || at >= to_ms {
                continue;
            }
            tokens = tokens.saturating_add(record.input).saturating_add(record.output);
            match crate::server::usage::cost_of(&record, pricing) {
                Some(cost) => usd += cost,
                None if !record.is_chatgpt_plan() => unpriced = true,
                None => {}
            }
        }
    }
    (usd, tokens, unpriced)
}

/// Junta os fatos do resumo: o estado do Nanty, os projetos observados (em andamento pela
/// verificação estrita), o custo de ontem e o disco pela sonda.
pub async fn gather(state: &AppState, nanty: &Arc<Nanty>, now: &LocalTime) -> DigestFacts {
    let cfg: Config = state.config().read().await.clone();
    let mut facts = state_facts(&nanty.snapshot(), now.epoch_ms);
    let observed: Vec<(String, PathBuf)> = cfg
        .projects
        .iter()
        .filter(|p| cfg.nanty.observes(&p.id))
        .map(|p| (p.id.clone(), PathBuf::from(&p.path)))
        .collect();
    facts.projects = observed.len() as u64;
    facts.running = observed.iter().filter(|(id, _)| state.project_busy_strict(id)).count() as u64;
    let today = day_start_ms(now);
    let roots: Vec<PathBuf> = observed.into_iter().map(|(_, root)| root).collect();
    let pricing = cfg.pricing.clone();
    let probe = nanty.probe();
    let disk_cfg = cfg.clone();
    let job = move || {
        let cost = projects_cost(&roots, &pricing, today.saturating_sub(DAY_MS), today);
        let disk = crate::server::doctor::disk_paths(&disk_cfg).iter().filter_map(|p| probe.free_bytes(p)).min();
        (cost, disk)
    };
    if let Ok(((usd, tokens, unpriced), disk)) = tokio::task::spawn_blocking(job).await {
        facts.cost_yesterday_usd = usd;
        facts.tokens_yesterday = tokens;
        facts.unpriced_yesterday = unpriced;
        facts.disk_free_gb = disk.map(|b| b / crate::config::GB_BYTES);
    }
    facts
}

fn line(code: &str, params: Value) -> Value {
    json!({ "code": code, "params": params })
}

/// A mensagem fixa do resumo (função pura): `None` num dia vazio com `digest_empty = skip`. Os
/// números vão em `params`, e as linhas extras (custo, disco e os avisos guardados) em
/// `params.lines`, cada uma `{code, params}`: a web e o Telegram montam o texto pelo dicionário.
pub fn compose(facts: &DigestFacts, cfg: &NantyConfig, id: String, at_ms: u64) -> Option<ConversationMessage> {
    let mut params = Map::new();
    params.insert("projects".into(), json!(facts.projects));
    if !facts.has_highlights() {
        return match cfg.digest_empty {
            DigestEmpty::Skip => None,
            DigestEmpty::Short => Some(ConversationMessage::from_nanty(id, at_ms, Channel::Web, QUIET, params)),
        };
    }
    params.insert("running".into(), json!(facts.running));
    params.insert("paused".into(), json!(facts.paused));
    params.insert("waiting".into(), json!(facts.waiting));
    params.insert("items".into(), json!(facts.items.len()));
    // Spec 139 T06: só o que não é zero vira frase, e cada número na frase dele.
    let mut lines = Vec::new();
    for (count, code) in [
        (facts.running, "nanty.digest.running"),
        (facts.paused, "nanty.digest.paused"),
        (facts.waiting, "nanty.chat.status_waiting"),
    ] {
        if count > 0 {
            lines.push(line(code, json!({ "count": count })));
        }
    }
    if facts.tokens_yesterday > 0 {
        let usd = (facts.cost_yesterday_usd * 100.0).round() / 100.0;
        let tokens = json!({ "cost_usd": usd, "tokens": facts.tokens_yesterday });
        // Sem nada com preço (o plano, por exemplo), só os tokens: nada de "US$ 0,00".
        let code = match (usd > 0.0, facts.unpriced_yesterday) {
            (false, _) => "nanty.digest.tokens",
            (true, true) => "nanty.digest.cost_partial",
            (true, false) => "nanty.digest.cost",
        };
        lines.push(line(code, tokens));
    }
    if let Some(gb) = facts.disk_free_gb {
        lines.push(line("nanty.digest.disk", json!({ "free_gb": gb })));
    }
    if !lines.is_empty() {
        params.insert("lines".into(), Value::Array(lines));
    }
    let why = held_why(facts);
    if !why.is_empty() {
        params.insert(WHY.into(), Value::Array(why));
    }
    let mut message = ConversationMessage::from_nanty(id, at_ms, Channel::Web, MORNING, params);
    message.project_id = None;
    Some(message)
}

/// Campo de `params` com o porquê dos avisos guardados (spec 139 T06, C-06 da 138): o cabeçalho
/// `nanty.digest.held` e um `nanty.why.*` por aviso, cada um `{code, params}`. Fica à parte de
/// `lines` porque sai sempre, com ou sem o texto do modelo: os avisos guardados não viram cartão,
/// então o porquê deles só existe aqui.
pub const WHY: &str = "why";

/// O porquê dos avisos guardados (os mais recentes, até [`MAX_DIGEST_LINES`]), do dicionário.
fn held_why(facts: &DigestFacts) -> Vec<Value> {
    let held: Vec<&DigestItem> = facts.items.iter().rev().take(MAX_DIGEST_LINES).rev().collect();
    if held.is_empty() {
        return Vec::new();
    }
    let mut out = vec![line("nanty.digest.held", json!({ "count": facts.items.len() }))];
    for item in held {
        let mut p = item.params.clone();
        if let Some(project) = &item.project_id {
            p.insert("project_id".into(), json!(project));
        }
        out.push(line(&item.code, Value::Object(p)));
    }
    out
}

/// Código da frase curta que vai junto do texto do modelo no resumo da manhã (spec 139 T06, C-11):
/// diz por que a mensagem chegou.
pub const LEAD: &str = "nanty.digest.lead";

/// O resumo com o texto do modelo (spec 139 T06, C-11): as linhas de contagem, custo e disco saem;
/// ficam a frase curta do servidor ([`LEAD`]), o porquê de cada aviso guardado ([`WHY`], do
/// dicionário) e o texto do modelo.
pub fn with_model_text(message: &mut ConversationMessage, facts: &DigestFacts, text: String) {
    let mut params = Map::new();
    let why = held_why(facts);
    if !why.is_empty() {
        params.insert(WHY.into(), Value::Array(why));
    }
    message.code = Some(LEAD.to_string());
    message.params = params;
    message.text = Some(text);
}

/// A entrada do modelo para o resumo: só fatos estruturados.
pub fn digest_input(facts: &DigestFacts, st: &NantyState) -> TurnInput {
    summary_input(TurnKind::MorningDigest, facts, st)
}

/// Os projetos de um resumo (spec 140 T04, C-16): os da entrada do modelo e os dos avisos guardados,
/// que saem no texto mesmo além do teto da entrada.
pub fn linked_projects(facts: &DigestFacts, input: &TurnInput) -> Vec<String> {
    super::provenance::unique(input.project_ids().into_iter().chain(facts.items.iter().filter_map(|i| i.project_id.clone())))
}

/// A entrada do resumo de um tipo (o da manhã ou o `/resumo`, spec 139 T06): só fatos
/// estruturados, sem contagem zerada (zero não é fato).
pub fn summary_input(kind: TurnKind, facts: &DigestFacts, st: &NantyState) -> TurnInput {
    let mut input = TurnInput::new(kind);
    input.signals = st.alerts.values().filter_map(SignalFact::of_alert).take(turn::MAX_SIGNALS_IN).collect();
    input.signals.extend(
        facts.items.iter().rev().take(turn::MAX_SIGNALS_IN).filter_map(|item| {
            let kind = item.code.strip_prefix("nanty.why.")?;
            SignalKind::parse(kind)?;
            Some(SignalFact {
                kind: kind.to_string(),
                project_id: item.project_id.clone().filter(|p| turn::is_code(p)),
                urgency: Some(Urgency::Digest),
                delivery: None,
                facts: turn::structured(&item.params),
            })
        }),
    );
    for (name, value) in [
        ("projects", facts.projects),
        ("running", facts.running),
        ("paused", facts.paused),
        ("waiting", facts.waiting),
        ("digest_items", facts.items.len() as u64),
    ] {
        if value > 0 {
            input.counts.insert(name, value);
        }
    }
    if facts.tokens_yesterday > 0 {
        input.tokens.insert("projects_yesterday", facts.tokens_yesterday);
        if !facts.unpriced_yesterday {
            input.cost_usd.insert("projects_yesterday", (facts.cost_yesterday_usd * 10_000.0).round() / 10_000.0);
        }
    }
    input.disk_free_gb = facts.disk_free_gb;
    input
}

/// Manda o resumo de hoje se for a hora: marca o dia e consome os avisos guardados na mesma trava
/// (sai uma vez só), monta a mensagem pelos fatos e, se houver o que destacar e orçamento, junta o
/// texto do modelo. `true` se uma mensagem saiu.
pub async fn maybe_send(state: &AppState, nanty: &Arc<Nanty>) -> bool {
    let cfg = state.config().read().await.nanty.clone();
    let now = nanty.now();
    let mut claimed = false;
    nanty.update(|st| {
        if !due(&cfg, st, &now) {
            return false;
        }
        st.digest_day = now.day_key();
        claimed = true;
        true
    });
    if !claimed {
        return false;
    }
    let facts = gather(state, nanty, &now).await;
    let snapshot = nanty.snapshot();
    // Os avisos guardados até agora entram neste resumo e saem da fila.
    let consumed = facts.items.len();
    nanty.update(|st| {
        let before = st.digest.len();
        st.digest.retain(|d| d.at_ms > now.epoch_ms);
        before != st.digest.len()
    });
    let Some(mut message) = compose(&facts, &cfg, super::new_id(), now.epoch_ms) else {
        record(nanty, "digest.skipped", consumed);
        return false;
    };
    // O resumo segue o idioma do Nanty (spec 139 T05), o texto fixo e o do modelo.
    message.lang = Some(nanty_lang(&cfg, nanty).tag().to_string());
    let mut input = digest_input(&facts, &snapshot);
    message.link_projects(&linked_projects(&facts, &input));
    if facts.has_highlights() {
        let full = state.config().read().await.clone();
        input.can_do = super::can_do::CanDo::now(state, &full, nanty, super::can_do::notice_channel(&full));
        if let TurnOutcome::Reply(reply) = turn::run(state, nanty, &input).await {
            with_model_text(&mut message, &facts, reply.summary);
            message.actions = reply.actions.iter().map(|a| a.as_str().to_string()).collect();
        }
    }
    record(nanty, "digest.sent", consumed);
    // Spec 139 T08: o resumo entra na conversa agora; com a voz ligada, a fala vem depois.
    super::voice::deliver(state, nanty, &mut message, super::voice::Tone::Neutral, false).await;
    true
}

fn record(nanty: &Nanty, code: &str, items: usize) {
    let mut entry = super::journal::JournalEntry::new(nanty.now().epoch_ms, super::journal::JournalKind::Filter, code);
    entry.params.insert("items".into(), json!(items));
    nanty.record(&entry);
}
