//! Turno do modelo do Nanty (spec 138 T05, G3).
//!
//! - **Entrada só com sinais estruturados (C-01):** o modelo recebe um JSON montado aqui, campo a
//!   campo, só com códigos, ids, contagens, papéis, custo e disco ([`TurnInput`]). Os fatos de cada
//!   aviso passam por [`structured`]: número, booleano ou texto com forma de código (letras, dígitos
//!   e `._:-`, até [`MAX_CODE_LEN`]). Texto de spec, log, saída de agente ou commit nunca chega aqui
//!   (os observadores só guardam contagens e códigos), e um texto livre que escapasse é descartado.
//! - **Resposta validada ([`validate`]):** um objeto JSON `{summary, actions}`, sem outros campos; o
//!   resumo curto, sem caractere de controle; as ações só ids do catálogo que a entrada ofereceu. O
//!   que vier fora disso é descartado inteiro, e o aviso segue com o texto fixo do observador.
//! - **Um turno por vez ([`TurnQueue`]):** uma fila única para o Telegram e a janela, com guarda
//!   que libera a vez em qualquer saída, no padrão do `busy_chats` (`AppState::try_begin_chat`).
//! - **Orçamento diário ([`budget_view`]):** o gasto do Nanty fica no estado dele, à parte dos
//!   projetos. Com preço, conta US$; sem preço (ou pelo plano), conta tokens, e o status mostra.
//!   Quando acaba, nenhum turno sai: os avisos seguem com o texto fixo do observador, sem modelo.
//! - **Idioma (C-08):** o prompt pede o idioma do Nanty; o texto do modelo é gravado como veio, com
//!   o idioma marcado (`lang`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::actions::{self, ActionId, ActionKind};
use super::journal::{JournalEntry, JournalKind};
use super::store::{Card, Channel, ConversationMessage, Delivery, NantyState, Spend, Speaker};
use super::{nanty_lang, Avatar, Nanty};
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::{Billing, ChatMessage, ChatRequest, Role, Usage};
use crate::config::{Config, Connection, ConnectionKind, Urgency};
use crate::server::state::AppState;

/// Maior resumo aceito do modelo, em caracteres.
pub const MAX_SUMMARY_CHARS: usize = 600;
/// Ações que o modelo pode sugerir num turno.
pub const MAX_MODEL_ACTIONS: usize = 3;
/// Turnos na fila (o que roda e os que esperam); além disso, o turno não sai.
pub const MAX_QUEUED_TURNS: usize = 4;
/// Tempo máximo de uma chamada ao modelo do Nanty (provisório).
pub const TURN_TIMEOUT: Duration = Duration::from_secs(90);
/// Avisos levados a um turno (os mais recentes).
pub const MAX_SIGNALS_IN: usize = 20;
/// Maior texto com forma de código aceito na entrada.
pub const MAX_CODE_LEN: usize = 64;

// ---- Fila única ----

/// Fila única dos turnos do Nanty: um por vez, na ordem de chegada (o `Mutex` do tokio é justo).
#[derive(Debug, Default)]
pub struct TurnQueue {
    lock: Arc<tokio::sync::Mutex<()>>,
    queued: Arc<AtomicUsize>,
}

/// Lugar na fila: solto em qualquer saída (inclusive se a espera for cancelada).
#[derive(Debug)]
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A vez do turno: enquanto existir, nenhum outro turno do Nanty roda.
#[derive(Debug)]
pub struct TurnGuard {
    _turn: tokio::sync::OwnedMutexGuard<()>,
    _slot: Slot,
}

impl TurnQueue {
    fn slot(&self) -> Option<Slot> {
        let before = self.queued.fetch_add(1, Ordering::SeqCst);
        let slot = Slot(Arc::clone(&self.queued));
        (before < MAX_QUEUED_TURNS).then_some(slot)
    }

    /// Entra na fila e espera a vez. `None` com a fila cheia.
    pub async fn enter(&self) -> Option<TurnGuard> {
        let slot = self.slot()?;
        let turn = Arc::clone(&self.lock).lock_owned().await;
        Some(TurnGuard { _turn: turn, _slot: slot })
    }

    /// A vez agora, sem esperar (`None` se há um turno rodando ou a fila está cheia).
    pub fn try_enter(&self) -> Option<TurnGuard> {
        let slot = self.slot()?;
        let turn = Arc::clone(&self.lock).try_lock_owned().ok()?;
        Some(TurnGuard { _turn: turn, _slot: slot })
    }

    /// Turnos na fila agora (o que roda e os que esperam).
    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::SeqCst)
    }
}

// ---- Entrada estruturada ----

/// Texto com forma de código: letras, dígitos e `._:-`, de 1 a [`MAX_CODE_LEN`].
pub fn is_code(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_CODE_LEN
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// Chaves aceitas em `facts` (T08, defesa extra sugerida na revisão da T05): as que os observadores,
/// o filtro e a oferta gravam. Qualquer outra fica de fora, mesmo com forma de código.
pub const FACT_KEYS: [&str; 12] =
    ["age_min", "attempts", "count", "free_gb", "min_gb", "observer", "reason", "role", "since", "spec_id", "stop", "task_id"];

/// Hora local `HH:MM`.
fn is_hhmm(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 5 && b[2] == b':' && [0, 1, 3, 4].iter().all(|i| b[*i].is_ascii_digit())
}

/// O valor aceito para cada chave de [`FACT_KEYS`]: contagens são números; os códigos vêm das
/// listas fixas do Nants (o motivo do bloqueio, a parada do provedor, o papel, o observador); os ids
/// (spec e tarefa) têm forma de código. Um texto com forma de código que veio de um arquivo do
/// projeto (o `next_action_code` de uma tarefa, por exemplo) só passa se for um código conhecido.
fn fact_allowed(key: &str, value: &Value) -> bool {
    let text = value.as_str();
    match key {
        "age_min" | "attempts" | "count" | "free_gb" | "min_gb" => value.is_u64(),
        "reason" => text.is_some_and(|v| crate::agents::runner::BLOCK_CODES.contains(&v)),
        "stop" => text.is_some_and(|v| crate::server::attention::PROVIDER_STOP_CODES.contains(&v)),
        "role" => matches!(text, Some("architect" | "developer" | "reviewer")),
        "observer" => text.is_some_and(|v| super::signals::SignalKind::ALL.iter().any(|k| k.observer().as_str() == v)),
        "since" => text.is_some_and(is_hhmm),
        "spec_id" | "task_id" => text.is_some_and(is_code),
        _ => false,
    }
}

/// Só o que é estruturado: as chaves de [`FACT_KEYS`], cada uma com o valor que ela aceita (número
/// ou código). Texto livre, chave desconhecida, lista, objeto e nulo ficam de fora.
pub fn structured(facts: &Map<String, Value>) -> Map<String, Value> {
    facts
        .iter()
        .filter(|(k, v)| FACT_KEYS.contains(&k.as_str()) && fact_allowed(k, v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Um aviso como o modelo o vê.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SignalFact {
    /// Tipo do sinal (`plan_waiting`, `disk_low`...).
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub urgency: Option<Urgency>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    /// Fatos estruturados ([`structured`]).
    pub facts: Map<String, Value>,
}

impl SignalFact {
    /// O aviso de um cartão: o tipo vem do código do porquê (`nanty.why.<tipo>`).
    pub fn of_card(card: &Card) -> Option<Self> {
        let kind = card.code.strip_prefix("nanty.why.")?;
        super::signals::SignalKind::parse(kind)?;
        Some(Self {
            kind: kind.to_string(),
            project_id: card.project_id.clone().filter(|p| is_code(p)),
            urgency: card.urgency,
            delivery: card.delivery,
            facts: structured(&card.params),
        })
    }

    /// Uma condição aberta (o resumo da manhã).
    pub fn of_alert(alert: &super::store::Alert) -> Option<Self> {
        let kind = super::signals::SignalKind::parse(&alert.kind)?;
        let mut facts = alert.facts.clone();
        facts.insert("since".into(), json!(alert.since));
        let facts = structured(&facts);
        Some(Self {
            kind: kind.as_str().to_string(),
            project_id: alert.project_id.clone().filter(|p| is_code(p)),
            urgency: None,
            delivery: None,
            facts,
        })
    }
}

/// O que o turno é.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    /// Avisos que acabaram de sair.
    Notice,
    /// O resumo da manhã.
    MorningDigest,
    /// A conversa: a pessoa escreveu (T08).
    Conversation,
}

/// Toda a entrada do modelo na proatividade: só sinais estruturados.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnInput {
    pub kind: TurnKind,
    pub signals: Vec<SignalFact>,
    /// Contagens (projetos, em andamento, pausados, esperando a pessoa...).
    pub counts: BTreeMap<&'static str, u64>,
    /// Custo em US$ (ontem nos projetos, hoje do Nanty), quando há preço.
    pub cost_usd: BTreeMap<&'static str, f64>,
    /// Tokens (ontem nos projetos), quando o custo não fecha em US$.
    pub tokens: BTreeMap<&'static str, u64>,
    /// Menor espaço livre medido nas pastas do Nants, em GB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_free_gb: Option<u64>,
    /// Ids do catálogo que o modelo pode sugerir.
    pub allowed_actions: Vec<ActionId>,
}

impl TurnInput {
    pub fn new(kind: TurnKind) -> Self {
        Self {
            kind,
            signals: Vec::new(),
            counts: BTreeMap::new(),
            cost_usd: BTreeMap::new(),
            tokens: BTreeMap::new(),
            disk_free_gb: None,
            allowed_actions: Vec::new(),
        }
    }

    /// O JSON que vai ao modelo.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// O que o modelo pode sugerir para estes cartões: a ação do catálogo que o servidor já ofereceu em
/// cada um (fora a faixa "só a pessoa") e as opções de qualquer cartão.
pub fn allowed_for(cards: &[Card]) -> Vec<ActionId> {
    let mut out = Vec::new();
    for card in cards {
        if let Some(id) = card.action.as_deref().and_then(actions::parse) {
            if id.entry().kind != ActionKind::PersonOnly && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    for option in [ActionId::RemindLater, ActionId::OpenNants] {
        if !cards.is_empty() && !out.contains(&option) {
            out.push(option);
        }
    }
    out
}

/// A entrada do turno proativo para os cartões que acabaram de sair.
pub fn notice_input(cards: &[Card], st: &NantyState, day: &str) -> TurnInput {
    let mut input = TurnInput::new(TurnKind::Notice);
    input.signals = cards.iter().rev().take(MAX_SIGNALS_IN).rev().filter_map(SignalFact::of_card).collect();
    input.counts.insert("new_notices", cards.len() as u64);
    input.counts.insert("open_cards", st.cards.len() as u64);
    input.counts.insert("digest_items", st.digest.len() as u64);
    let spend = st.spend_on(day);
    if spend.usd > 0.0 {
        input.cost_usd.insert("nanty_today", round_cents(spend.usd));
    }
    input.allowed_actions = allowed_for(cards);
    input
}

fn round_cents(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

// ---- Resposta ----

/// Resposta validada do modelo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub summary: String,
    pub actions: Vec<ActionId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReply {
    summary: String,
    #[serde(default)]
    actions: Vec<String>,
}

/// Por que a resposta foi descartada (código do registro).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    /// Não é o objeto JSON do formato.
    Format,
    /// Resumo vazio, longo demais ou com caractere de controle.
    Summary,
    /// Id fora do catálogo ou fora do que a entrada ofereceu, ou ações demais.
    Action,
}

impl Invalid {
    pub fn as_str(self) -> &'static str {
        match self {
            Invalid::Format => "format",
            Invalid::Summary => "summary",
            Invalid::Action => "action",
        }
    }
}

/// Valida a resposta: o objeto `{summary, actions}` e nada mais; o resumo de 1 a
/// [`MAX_SUMMARY_CHARS`] caracteres, sem controle (fora a quebra de linha); até
/// [`MAX_MODEL_ACTIONS`] ids, todos do catálogo e de `allowed`. Qualquer desvio descarta tudo.
pub fn validate(raw: &str, allowed: &[ActionId]) -> Result<Reply, Invalid> {
    // Só o objeto: o `serde` também aceitaria a lista com os campos em ordem.
    let value: Value = serde_json::from_str(raw.trim()).map_err(|_| Invalid::Format)?;
    if !value.is_object() {
        return Err(Invalid::Format);
    }
    let parsed: RawReply = serde_json::from_value(value).map_err(|_| Invalid::Format)?;
    let summary = parsed.summary.trim().to_string();
    let count = summary.chars().count();
    if count == 0 || count > MAX_SUMMARY_CHARS || summary.chars().any(|c| c.is_control() && c != '\n') {
        return Err(Invalid::Summary);
    }
    if parsed.actions.len() > MAX_MODEL_ACTIONS {
        return Err(Invalid::Action);
    }
    let mut out = Vec::new();
    for raw_id in &parsed.actions {
        let id = actions::parse(raw_id).filter(|id| allowed.contains(id)).ok_or(Invalid::Action)?;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(Reply { summary, actions: out })
}

// ---- Orçamento ----

/// Como o orçamento conta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetMode {
    /// Em US$: o modelo tem preço.
    Usd,
    /// Em tokens: o modelo não tem preço ou vai pelo plano.
    Tokens,
}

/// O orçamento do dia, para o status e para a decisão do turno.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetView {
    pub mode: BudgetMode,
    pub day: String,
    pub spent_usd: f64,
    pub budget_usd: f64,
    pub tokens: u64,
    pub budget_tokens: u64,
    pub unpriced_turns: u64,
    /// O modelo escolhido não tem preço (conta tokens).
    pub unpriced: bool,
    /// A conexão do Nanty é pelo plano.
    pub plan: bool,
    /// A conexão pelo plano é a mesma de algum papel: a cota é compartilhada.
    pub quota_shared: bool,
    pub exhausted: bool,
}

/// A conexão e o modelo do Nanty, se escolhidos e cadastrados.
pub fn model_route(cfg: &Config) -> Option<(&Connection, &str)> {
    let model = cfg.nanty.model.trim();
    if model.is_empty() {
        return None;
    }
    let conn = cfg.connections.iter().find(|c| c.id == cfg.nanty.connection_id)?;
    Some((conn, model))
}

/// O orçamento do dia `spend.day` pela configuração atual.
pub fn budget_view(cfg: &Config, spend: &Spend) -> BudgetView {
    let n = &cfg.nanty;
    let conn = cfg.connections.iter().find(|c| c.id == n.connection_id);
    let plan = conn.is_some_and(|c| c.kind == ConnectionKind::ChatgptPlan);
    let configured = model_route(cfg).is_some();
    let priced = !plan && cfg.pricing.contains_key(n.model.trim());
    let mode = if priced { BudgetMode::Usd } else { BudgetMode::Tokens };
    let roles = [&cfg.agents.architect, &cfg.agents.developer, &cfg.agents.reviewer];
    let quota_shared = plan && roles.iter().any(|r| r.connection_id == n.connection_id);
    let tokens = spend.input_tokens.saturating_add(spend.output_tokens);
    let budget_usd = n.budget_usd();
    let exhausted = match mode {
        BudgetMode::Usd => spend.usd >= budget_usd,
        BudgetMode::Tokens => tokens >= n.daily_budget_tokens,
    };
    BudgetView {
        mode,
        day: spend.day.clone(),
        spent_usd: spend.usd,
        budget_usd,
        tokens,
        budget_tokens: n.daily_budget_tokens,
        unpriced_turns: spend.unpriced_turns,
        unpriced: configured && !plan && !priced,
        plan,
        quota_shared,
        exhausted,
    }
}

/// Custo em US$ de um turno pelo preço do modelo (o mesmo `cost_of` do custo dos projetos). `None`
/// sem preço ou pelo plano.
pub fn turn_cost(cfg: &Config, model: &str, usage: &Usage, billing: Billing) -> Option<f64> {
    let record = crate::server::usage::UsageRecord {
        at: String::new(),
        project: String::new(),
        spec: None,
        task: None,
        agent: "nanty".into(),
        model: model.to_string(),
        input: u64::from(usage.prompt),
        output: u64::from(usage.completion),
        cached: usage.cached.map(u64::from),
        reasoning: usage.reasoning.map(u64::from),
        cache_write: usage.cache_write.map(u64::from),
        web_search_calls: None,
        images: None,
        image_quality: None,
        estimated: None,
        cut: None,
        billing: billing.as_record().map(str::to_string),
        context_bytes: None,
        messages: None,
        step: None,
        session: None,
        summary: None,
        stage: None,
        round: None,
        call: Default::default(),
    };
    crate::server::usage::cost_of(&record, &cfg.pricing)
}

// ---- O turno ----

/// Resultado de um turno.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Reply(Reply),
    /// Nenhum texto do modelo: o motivo em código (`no_model`, `budget`, `busy`, `failed`,
    /// `invalid`). O aviso segue com o texto fixo do observador.
    Skipped(&'static str),
}

/// Volta o avatar ao estado derivado em qualquer saída.
struct Thinking<'a>(&'a Nanty);

impl Drop for Thinking<'_> {
    fn drop(&mut self) {
        self.0.set_busy(None);
    }
}

fn turn_entry(nanty: &Nanty, code: &str, kind: TurnKind, reason: Option<&str>) -> JournalEntry {
    let mut entry = JournalEntry::new(nanty.now().epoch_ms, JournalKind::ModelTurn, code);
    entry.params.insert("turn".into(), json!(kind));
    if let Some(reason) = reason {
        entry.params.insert("reason".into(), json!(reason));
    }
    entry
}

fn skipped(nanty: &Nanty, kind: TurnKind, reason: &'static str) -> TurnOutcome {
    // Sem modelo escolhido não é novidade: o registro não enche a cada aviso.
    if reason != "no_model" {
        nanty.record(&turn_entry(nanty, "turn.skipped", kind, Some(reason)));
    }
    TurnOutcome::Skipped(reason)
}

/// Estimativa de tokens quando o provedor não informa o uso (4 caracteres por token).
fn estimate_tokens(text: &str) -> u32 {
    u32::try_from(text.len() / 4 + 1).unwrap_or(u32::MAX)
}

/// Um turno do modelo do Nanty com a entrada estruturada: confere o modelo e o orçamento, espera a
/// vez na fila, chama o modelo (o cliente dos papéis, com a conexão do `[nanty]`), soma o gasto do
/// dia e valida a resposta.
pub async fn run(state: &AppState, nanty: &Arc<Nanty>, input: &TurnInput) -> TurnOutcome {
    let lang = nanty_lang(&state.config().read().await.nanty);
    let call = Call {
        kind: input.kind,
        system: crate::agents::prompts::nanty_system(lang),
        user: input.to_json(),
        allowed: &input.allowed_actions,
        signals: input.signals.len(),
    };
    run_call(state, nanty, &call).await
}

/// Uma chamada ao modelo do Nanty: o tipo do turno, o prompt de sistema, a entrada (JSON montado
/// pelo servidor), os ids que a resposta pode trazer e quantos sinais foram (para o registro).
#[derive(Debug)]
pub struct Call<'a> {
    pub kind: TurnKind,
    pub system: String,
    pub user: String,
    pub allowed: &'a [ActionId],
    pub signals: usize,
}

/// O turno com a entrada já montada (a proatividade e a conversa): a mesma fila, o mesmo orçamento e
/// a mesma validação.
pub async fn run_call(state: &AppState, nanty: &Arc<Nanty>, input: &Call<'_>) -> TurnOutcome {
    let cfg = state.config().read().await.clone();
    let Some((conn, model)) = model_route(&cfg).map(|(c, m)| (c.clone(), m.to_string())) else {
        return skipped(nanty, input.kind, "no_model");
    };
    let exhausted = || budget_view(&cfg, &nanty.snapshot().spend_on(&nanty.now().day_key())).exhausted;
    if exhausted() {
        return skipped(nanty, input.kind, "budget");
    }
    let Some(_turn) = nanty.turns().enter().await else {
        return skipped(nanty, input.kind, "busy");
    };
    // Outro turno pode ter gastado o resto enquanto este esperava.
    if exhausted() {
        return skipped(nanty, input.kind, "budget");
    }
    let backend: Arc<dyn ChatBackend> = match nanty.backend() {
        Some(fake) => fake,
        None => match crate::server::chatgpt::plan_client::llm_client(state, &conn).and_then(|c| c.with_timeout(TURN_TIMEOUT)) {
            Ok(client) => Arc::new(client),
            Err(_) => return skipped(nanty, input.kind, "failed"),
        },
    };
    let user = input.user.clone();
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![ChatMessage::text(Role::System, input.system.clone()), ChatMessage::text(Role::User, user.clone())],
        tools: Vec::new(),
        effort: Some(cfg.nanty.effort),
        response_json: true,
        web_search: false,
        web_search_max_uses: None,
        cache_key: None,
    };
    nanty.set_busy(Some(Avatar::Thinking));
    let thinking = Thinking(nanty);
    let response = backend.complete(&request).await;
    drop(thinking);
    let response = match response {
        Ok(r) => r,
        Err(err) => {
            let mut entry = turn_entry(nanty, "turn.failed", input.kind, None);
            if let Some((reason, _)) = crate::agents::llm_client::provider_failure(&err).reason_kind() {
                entry.params.insert("reason".into(), json!(reason));
            }
            nanty.record(&entry);
            return TurnOutcome::Skipped("failed");
        }
    };
    let plan = conn.kind == ConnectionKind::ChatgptPlan || response.billing == Billing::ChatgptPlan;
    let billing = if plan { Billing::ChatgptPlan } else { response.billing };
    let usage = response.usage.unwrap_or_else(|| {
        Usage::new(estimate_tokens(&user) + estimate_tokens(&request.messages[0].content), estimate_tokens(&response.message.content))
    });
    let usd = turn_cost(&cfg, &model, &usage, billing);
    let day = nanty.now().day_key();
    nanty.update(|st| {
        st.add_spend(&day, u64::from(usage.prompt), u64::from(usage.completion), usd);
        true
    });
    nanty.changed(super::Part::Status);
    let verdict = validate(&response.message.content, input.allowed);
    let mut entry = turn_entry(nanty, if verdict.is_ok() { "turn.ok" } else { "turn.invalid" }, input.kind, None);
    if let Err(why) = verdict {
        entry.params.insert("reason".into(), json!(why.as_str()));
    }
    entry.cost_usd = usd;
    entry.input_tokens = Some(u64::from(usage.prompt));
    entry.output_tokens = Some(u64::from(usage.completion));
    entry.params.insert("signals".into(), json!(input.signals));
    nanty.record(&entry);
    match verdict {
        Ok(reply) => TurnOutcome::Reply(reply),
        Err(_) => TurnOutcome::Skipped("invalid"),
    }
}

/// Mensagem do Nanty com o texto do modelo, como veio, e o idioma marcado.
pub fn model_message(nanty: &Nanty, lang: crate::i18n::Lang, reply: &Reply, card_ids: Vec<String>) -> ConversationMessage {
    ConversationMessage {
        id: super::new_id(),
        at_ms: nanty.now().epoch_ms,
        from: Speaker::Nanty,
        channel: Channel::Web,
        project_id: None,
        text: Some(reply.summary.clone()),
        code: None,
        params: Map::new(),
        lang: Some(lang.tag().to_string()),
        card_ids,
        actions: reply.actions.iter().map(|a| a.as_str().to_string()).collect(),
        reply_to: None,
        voice: false,
        audio: None,
    }
}

/// Turno proativo (depois de cada rodada): os cartões que acabaram de sair (fora os que só vão ao
/// resumo) viram uma entrada estruturada; com resposta válida, o resumo do modelo entra na conversa
/// ligado aos cartões. Sem modelo, sem orçamento ou com resposta inválida, nada muda: os cartões já
/// trazem o texto fixo do observador. Devolve o resultado (`None` sem cartão novo).
pub async fn proactive(state: &AppState, nanty: &Arc<Nanty>) -> Option<TurnOutcome> {
    // Sem modelo escolhido, nada a fazer (nem a gravar): os cartões já têm o texto fixo.
    if model_route(&*state.config().read().await).is_none() {
        return None;
    }
    let now = nanty.now().epoch_ms;
    let mut fresh: Vec<Card> = Vec::new();
    let mut snapshot = NantyState::default();
    // Escolhe e marca na mesma trava: duas chamadas juntas nunca pegam o mesmo cartão.
    nanty.update(|st| {
        fresh = st
            .cards
            .iter()
            .filter(|c| {
                c.created_at_ms > st.turned_until_ms
                    && c.requested.is_none()
                    && c.expires_at_ms > now
                    && c.urgency != Some(Urgency::Digest)
                    && c.delivery != Some(Delivery::Digest)
            })
            .cloned()
            .collect();
        let Some(latest) = st.cards.iter().map(|c| c.created_at_ms).max() else { return false };
        if latest <= st.turned_until_ms {
            return false;
        }
        st.turned_until_ms = latest;
        snapshot = st.clone();
        true
    });
    if fresh.is_empty() {
        return None;
    }
    let input = notice_input(&fresh, &snapshot, &nanty.now().day_key());
    let outcome = run(state, nanty, &input).await;
    if let TurnOutcome::Reply(reply) = &outcome {
        let lang = nanty_lang(&state.config().read().await.nanty);
        let mut message = model_message(nanty, lang, reply, fresh.iter().map(|c| c.id.clone()).collect());
        let first = fresh.first().and_then(|c| c.project_id.clone());
        if fresh.iter().all(|c| c.project_id == first) {
            message.project_id = first;
        }
        let n = Arc::clone(nanty);
        let _ = tokio::task::spawn_blocking(move || n.push_message(&message)).await;
    }
    Some(outcome)
}
