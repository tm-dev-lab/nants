//! Laço de agente da conversa do Nanty (spec 139 T03, G1, C-02, C-03, C-06 e C-10).
//!
//! A conversa deixa de ser uma chamada única com um pacote fixo: o modelo recebe a lista de
//! projetos (nome, id e fase numa linha), os avisos abertos com os ids dos cartões, a memória da
//! conversa e as ferramentas de leitura ([`super::tools`]), e escolhe o que consultar.
//!
//! - **Reaproveita o laço dos agentes:** os ids de chamada passam por `assign_missing_call_ids`,
//!   `sync_provider_call_ids` e `check_call_ids`; o modelo é o `ChatBackend` (o simulado nos testes);
//!   a resposta do assistente entra no histórico como veio, com os `provider_items` (o plano ChatGPT
//!   exige o reenvio exato).
//! - **Limites** (`[nanty.chat]`, provisórios, gravados só se mudarem): rodadas por mensagem, prazo
//!   total, chamadas atendidas por resposta (as demais recebem o aviso do limite: toda chamada tem
//!   resultado) e tokens por mensagem. O orçamento do dia é conferido antes de cada rodada. A última
//!   rodada (pelo número ou pelo teto de tokens) vai sem ferramentas.
//! - **Uma vez por vez:** a fila única dos turnos ([`super::turn::TurnQueue`]) vale para o laço todo.
//! - **Cache (C-10):** o prefixo estável (sistema e ferramentas) leva um `cache_key` fixo da
//!   conversa ([`crate::agents::usage::cache_key`] só sobre o sistema); a lista de projetos, os avisos
//!   e a memória vêm depois, na mensagem da pessoa.
//! - **Memória (C-06):** as últimas mensagens numa janela de tempo, com a hora; só o texto da pessoa
//!   e a resposta final do Nanty (resultado de ferramenta nunca é gravado na conversa).
//! - **Resposta em texto puro (C-03):** o texto final segue para o `format::clean` em [`super::chat`].
//! - **Turno contaminado (T04, C-07):** a [`Session`] da mensagem marca quando há dado não confiável
//!   no contexto: a leitura de `project_details`, uma mensagem encaminhada ou a citação de terceiro
//!   (na mensagem ou na memória). Daí em diante, o silêncio vira cartão de confirmação.
//! - **Texto de terceiro (T04, C-07):** a mensagem encaminhada vai em `forwarded` e a citação de
//!   terceiro em `quote`, cada uma no envelope de dado não confiável; nunca em `message`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::time::Instant;

use super::store::{Channel, ConversationMessage, Relayed, Speaker};
use super::turn::{self, TurnKind};
use super::{render, tools, Avatar, Nanty};
use crate::agents::chat::ChatBackend;
use crate::agents::llm_agents::{assign_missing_call_ids, check_call_ids};
use crate::agents::llm_client::{sync_provider_call_ids, Billing, ChatMessage, ChatRequest, Role, Usage};
use crate::config::{Config, ConnectionKind, Urgency};
use crate::i18n::Lang;
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Sessão do `cache_key` da conversa.
const CACHE_SESSION: &str = "nanty-chat";
/// Projetos na lista do contexto, no máximo.
const MAX_LISTED: usize = 100;
/// Avisos abertos no contexto, os mais recentes.
const MAX_NOTICES: usize = 10;
/// Teto de cada mensagem da memória, em caracteres.
const MAX_MEMORY_CHARS: usize = 1500;

/// Limites de uma mensagem, da configuração (`[nanty.chat]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub rounds: usize,
    pub deadline: Duration,
    pub calls: usize,
    pub tokens: u64,
}

impl Limits {
    pub fn of(cfg: &Config) -> Self {
        let c = &cfg.nanty.chat;
        Self { rounds: c.rounds(), deadline: c.deadline(), calls: c.calls(), tokens: c.tokens() }
    }
}

/// O que o silêncio fez na mensagem (T04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilenceEffect {
    /// Ligado direto, até o instante (ms).
    Applied(u64),
    /// Virou o cartão de confirmação (turno contaminado), com o fim calculado.
    Proposed(u64),
    /// Desligado.
    Ended,
}

/// O que uma mensagem fez, ferramenta a ferramenta (T04).
#[derive(Debug, Clone)]
pub struct Session {
    /// Canal da pessoa: o cartão pedido sai por ele.
    pub channel: Channel,
    /// Há dado não confiável no contexto do turno (C-07).
    pub tainted: bool,
    /// Leituras de `project_details` já atendidas.
    pub details: usize,
    /// Projetos consultados.
    pub read: Vec<String>,
    /// Cartões criados na mensagem (ações propostas e o silêncio).
    pub cards: Vec<String>,
    /// Ids do catálogo dos cartões de ação.
    pub actions: Vec<String>,
    pub silence: Option<SilenceEffect>,
    /// Idioma de quem falou (spec 139 T05): o dos resultados das ferramentas.
    pub lang: Lang,
}

impl Session {
    pub fn new(channel: Channel, tainted: bool, lang: Lang) -> Self {
        Self { channel, tainted, details: 0, read: Vec::new(), cards: Vec::new(), actions: Vec::new(), silence: None, lang }
    }
}

/// A entrada do modelo e se ela já traz dado não confiável (uma mensagem encaminhada ou citação de
/// terceiro, agora ou na memória).
#[derive(Debug, Clone)]
pub struct Input {
    pub text: String,
    pub untrusted: bool,
}

/// Como o laço terminou.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// O texto final do modelo (ainda sem o `clean`) e as rodadas.
    Reply { text: String, rounds: usize },
    /// Nenhum texto do modelo: o motivo em código (`no_model`, `budget`, `busy`, `deadline`,
    /// `failed`, `invalid`, `rounds`).
    Skipped(&'static str),
}

// ---- A entrada ----

/// Texto de terceiro no envelope de dado não confiável: o texto vira uma string JSON sem `<` nem
/// `>` ([`tools::seal`]), então nada dele abre nem fecha o envelope.
pub fn envelope(text: &str) -> String {
    let body = tools::seal(&serde_json::to_string(text).unwrap_or_default());
    format!("{} {body} {}", tools::UNTRUSTED_OPEN, tools::UNTRUSTED_CLOSE)
}

/// O que a mensagem da pessoa traz de terceiro, nos campos do contexto: a encaminhada inteira em
/// `forwarded` (e o campo do texto, `key`, fica vazio), a citação em `quote`, cortada em `max`
/// caracteres como o texto (spec 139 T05). `true` se trouxe.
fn relay_fields(item: &mut Value, key: &str, relayed: Option<&Relayed>, text: &str, max: usize) -> bool {
    let Some(r) = relayed else { return false };
    if r.forwarded {
        item["forwarded"] = json!(envelope(text));
        item[key] = json!("");
    }
    if r.third_party {
        let quote: String = r.quote.as_deref().unwrap_or_default().trim().chars().take(max).collect();
        item["quote"] = json!(envelope(&quote));
    }
    r.forwarded || r.third_party
}

/// A mensagem da pessoa como vai ao modelo (JSON): a hora, a lista de projetos observados (uma
/// linha cada, com a fase da fonte única), os avisos abertos com os ids dos cartões, a memória e o
/// texto dela (já redigido). O texto de terceiro vai no envelope de dado não confiável.
pub async fn input(state: &AppState, nanty: &Nanty, cfg: &Config, lang: Lang, person: &ConversationMessage, text: &str) -> Input {
    let secrets = SecretSet::from_config(cfg);
    let now = nanty.now();
    let phases = super::observe::observed_phases(state, cfg).await;
    let projects: Vec<String> = phases
        .iter()
        .take(MAX_LISTED)
        .map(|(p, phase)| {
            let phase = match phase {
                Some(phase) => crate::i18n::tr(lang, &tools::phase_code(*phase), &[]),
                None => crate::i18n::tr(lang, "nanty.status.unread", &[]),
            };
            format!("{} | {} | {}", p.id, tools::project_name(&secrets, p), phase)
        })
        .collect();
    let observed = |id: &Option<String>| id.as_deref().is_none_or(|id| cfg.nanty.observes(id));
    let snap = nanty.snapshot();
    let mut cards: Vec<_> = snap
        .cards
        .iter()
        .filter(|c| c.expires_at_ms > now.epoch_ms && c.urgency != Some(Urgency::Digest) && c.awaits_person() && observed(&c.project_id))
        .collect();
    cards.sort_by_key(|c| c.created_at_ms);
    let notices: Vec<Value> = cards
        .iter()
        .rev()
        .take(MAX_NOTICES)
        .rev()
        .map(|c| {
            let mut n = json!({ "card_id": c.id, "at": tools::hhmm(c.created_at_ms), "text": render::card_text(lang, c) });
            if let Some(p) = &c.project_id {
                n["project_id"] = json!(p);
            }
            n
        })
        .collect();
    let (memory, mut untrusted) = memory(nanty, cfg, lang, person, now.epoch_ms);
    let mut body = json!({
        "now": tools::hhmm(now.epoch_ms),
        "projects": projects,
        "notices": notices,
        "memory": memory,
        "message": text,
    });
    untrusted |= relay_fields(&mut body, "message", person.relayed.as_ref(), text, super::chat::MAX_QUOTE_CHARS);
    let text = secrets.scrub_exact(&serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string()));
    Input { text, untrusted }
}

/// A memória (C-06): as últimas mensagens da janela de tempo, antes da mensagem atual, com a hora.
/// Só o texto da pessoa e o texto final do Nanty (o fixo, montado no idioma, ou o do modelo). O texto
/// de terceiro vai no envelope, e `true` diz que a memória traz algum (C-07).
fn memory(nanty: &Nanty, cfg: &Config, lang: Lang, person: &ConversationMessage, now_ms: u64) -> (Vec<Value>, bool) {
    let chat = &cfg.nanty.chat;
    let from = now_ms.saturating_sub(chat.memory_window_ms());
    let mut items: Vec<(Value, bool)> = nanty
        .conversation()
        .into_iter()
        .filter(|m| m.id != person.id && m.at_ms >= from && m.at_ms <= person.at_ms)
        .filter(|m| m.project_id.as_deref().is_none_or(|id| cfg.nanty.observes(id)))
        .filter_map(|m| {
            let text = match m.from {
                Speaker::Person => m.text.clone().unwrap_or_default(),
                Speaker::Nanty => render::message_text(lang, &m),
            };
            let text: String = text.trim().chars().take(MAX_MEMORY_CHARS).collect();
            if text.is_empty() {
                return None;
            }
            let from = match m.from {
                Speaker::Person => "person",
                Speaker::Nanty => "nanty",
            };
            let mut item = json!({ "at": tools::hhmm(m.at_ms), "from": from, "text": text });
            let relayed = m.from == Speaker::Person && relay_fields(&mut item, "text", m.relayed.as_ref(), &text, MAX_MEMORY_CHARS);
            if !m.card_ids.is_empty() {
                item["card_ids"] = json!(m.card_ids);
            }
            Some((item, relayed))
        })
        .collect();
    let keep = chat.memory();
    if items.len() > keep {
        items.drain(..items.len() - keep);
    }
    let untrusted = items.iter().any(|(_, r)| *r);
    (items.into_iter().map(|(item, _)| item).collect(), untrusted)
}

// ---- O laço ----

fn skip(nanty: &Nanty, reason: &'static str) -> Ended {
    let _ = turn::skipped(nanty, TurnKind::Conversation, reason);
    Ended::Skipped(reason)
}

/// Resultado de ferramenta pareado com a chamada.
fn tool_result(call_id: String, content: String) -> ChatMessage {
    ChatMessage { role: Role::Tool, content, tool_call_id: Some(call_id), tool_calls: Vec::new(), provider_items: None, images: Vec::new() }
}

/// O gasto de uma mensagem, somado rodada a rodada.
#[derive(Debug, Default)]
struct Spent {
    input: u64,
    output: u64,
    usd: Option<f64>,
}

impl Spent {
    fn total(&self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

/// Soma o gasto de uma rodada no dia (o mesmo cálculo do turno proativo).
fn account(nanty: &Nanty, cfg: &Config, plan: bool, model: &str, request: &ChatRequest, response: &crate::agents::llm_client::ChatResponse, spent: &mut Spent) {
    let billing = if plan || response.billing == Billing::ChatgptPlan { Billing::ChatgptPlan } else { response.billing };
    let usage = response.usage.unwrap_or_else(|| {
        let sent: u32 = request.messages.iter().map(|m| turn::estimate_tokens(&m.content)).fold(0, u32::saturating_add);
        Usage::new(sent, turn::estimate_tokens(&response.message.content))
    });
    let usd = turn::turn_cost(cfg, model, &usage, billing);
    let day = nanty.now().day_key();
    nanty.update(|st| {
        st.add_spend(&day, u64::from(usage.prompt), u64::from(usage.completion), usd);
        true
    });
    nanty.changed(super::Part::Status);
    spent.input = spent.input.saturating_add(u64::from(usage.prompt));
    spent.output = spent.output.saturating_add(u64::from(usage.completion));
    if let Some(v) = usd {
        spent.usd = Some(spent.usd.unwrap_or(0.0) + v);
    }
}

fn record(nanty: &Nanty, code: &str, reason: Option<&str>, rounds: usize, calls: usize, spent: &Spent) {
    let mut entry = turn::turn_entry(nanty, code, TurnKind::Conversation, reason);
    entry.cost_usd = spent.usd;
    entry.input_tokens = Some(spent.input);
    entry.output_tokens = Some(spent.output);
    entry.params.insert("rounds".into(), json!(rounds));
    entry.params.insert("tool_calls".into(), json!(calls));
    nanty.record(&entry);
}

/// O laço: confere o modelo e o orçamento, espera a vez na fila, e então, rodada a rodada, chama o
/// modelo e atende as ferramentas, até a resposta final em texto. A `session` guarda o que as
/// ferramentas fizeram (os cartões e o silêncio valem mesmo se o laço terminar sem texto).
pub async fn run(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, system: String, user: String, session: &mut Session) -> Ended {
    let Some((conn, model)) = turn::model_route(cfg).map(|(c, m)| (c.clone(), m.to_string())) else {
        return skip(nanty, "no_model");
    };
    let exhausted = || turn::budget_view(cfg, &nanty.snapshot().spend_on(&nanty.now().day_key())).exhausted;
    if exhausted() {
        return skip(nanty, "budget");
    }
    let Some(_turn) = nanty.turns().enter().await else {
        return skip(nanty, "busy");
    };
    let backend: Arc<dyn ChatBackend> = match nanty.backend() {
        Some(fake) => fake,
        None => match crate::server::chatgpt::plan_client::llm_client(state, &conn).and_then(|c| c.with_timeout(turn::TURN_TIMEOUT)) {
            Ok(client) => Arc::new(client),
            Err(_) => return skip(nanty, "failed"),
        },
    };
    let limits = Limits::of(cfg);
    let deadline = Instant::now() + limits.deadline;
    let lang = session.lang;
    let secrets = SecretSet::from_config(cfg);
    let plan = conn.kind == ConnectionKind::ChatgptPlan;
    let specs = crate::agents::prompts::nanty_chat_tools();
    let system = ChatMessage::text(Role::System, system);
    let cache_key = crate::agents::usage::cache_key(Path::new(super::NANTY_DIR), CACHE_SESSION, std::slice::from_ref(&system));
    let mut messages = vec![system, ChatMessage::text(Role::User, user)];
    nanty.set_busy(Some(Avatar::Thinking));
    let _thinking = turn::Thinking(nanty);
    let mut seq = 0usize;
    let mut spent = Spent::default();
    let mut calls_made = 0usize;
    for round in 0..limits.rounds {
        if Instant::now() >= deadline {
            record(nanty, "turn.skipped", Some("deadline"), round, calls_made, &spent);
            return Ended::Skipped("deadline");
        }
        // Antes de cada rodada: outro turno pode ter gastado o resto, e esta mensagem também gasta.
        if exhausted() {
            record(nanty, "turn.skipped", Some("budget"), round, calls_made, &spent);
            return Ended::Skipped("budget");
        }
        let last = round + 1 == limits.rounds || spent.total() >= limits.tokens;
        let request = ChatRequest {
            model: model.clone(),
            messages: messages.clone(),
            tools: if last { Vec::new() } else { specs.clone() },
            effort: Some(cfg.nanty.effort),
            response_json: false,
            web_search: false,
            web_search_max_uses: None,
            cache_key: Some(cache_key.clone()),
        };
        let response = match tokio::time::timeout_at(deadline, backend.complete(&request)).await {
            Err(_) => {
                record(nanty, "turn.skipped", Some("deadline"), round, calls_made, &spent);
                return Ended::Skipped("deadline");
            }
            Ok(Err(err)) => {
                let reason = crate::agents::llm_client::provider_failure(&err).reason_kind().map(|(r, _)| r);
                record(nanty, "turn.failed", reason, round, calls_made, &spent);
                return Ended::Skipped("failed");
            }
            Ok(Ok(response)) => response,
        };
        account(nanty, cfg, plan, &model, &request, &response, &mut spent);
        let mut message = response.message;
        assign_missing_call_ids(&mut message, &messages, &mut seq);
        sync_provider_call_ids(&mut message);
        if check_call_ids(&message, &messages).is_err() {
            record(nanty, "turn.invalid", Some("call_id"), round + 1, calls_made, &spent);
            return Ended::Skipped("invalid");
        }
        if message.tool_calls.is_empty() || last {
            let text = message.content.trim().to_string();
            if text.is_empty() {
                let reason = if message.tool_calls.is_empty() { "empty" } else { "rounds" };
                record(nanty, "turn.invalid", Some(reason), round + 1, calls_made, &spent);
                return Ended::Skipped(if reason == "empty" { "invalid" } else { "rounds" });
            }
            record(nanty, "turn.ok", None, round + 1, calls_made, &spent);
            return Ended::Reply { text, rounds: round + 1 };
        }
        let calls = message.tool_calls.clone();
        // A resposta entra como veio (com os `provider_items`), antes dos resultados.
        messages.push(message);
        for (i, call) in calls.into_iter().enumerate() {
            // Toda chamada recebe resultado, para os pares continuarem válidos.
            let content = if i >= limits.calls {
                crate::i18n::tr(lang, "nanty.tool.skipped", &[])
            } else {
                calls_made += 1;
                let job = tools::call(state, nanty, cfg, lang, &secrets, &call, session);
                match tokio::time::timeout_at(deadline, job).await {
                    Ok(content) => content,
                    Err(_) => crate::i18n::tr(lang, "nanty.tool.deadline", &[]),
                }
            };
            messages.push(tool_result(call.id, content));
        }
    }
    record(nanty, "turn.invalid", Some("rounds"), limits.rounds, calls_made, &spent);
    Ended::Skipped("rounds")
}

#[cfg(test)]
#[path = "agent_tests.rs"]
pub(crate) mod tests;
