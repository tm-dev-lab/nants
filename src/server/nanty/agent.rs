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
//! - **Cache (C-10; spec 140 T06, C-17):** o prefixo estável (sistema e ferramentas) leva um
//!   `cache_key` fixo da conversa ([`crate::agents::usage::cache_key`] só sobre o sistema); depois vêm
//!   o resumo e o histórico como mensagens de verdade, que só mudam no degrau da janela, e por último
//!   a mensagem com a hora, os projetos, os avisos e o texto da pessoa ([`history`]).
//! - **Histórico (C-06; spec 140 T06):** a janela em degraus, sem limite de tempo, com a data e a hora
//!   de cada mensagem; só o texto da pessoa e a resposta final do Nanty (resultado de ferramenta nunca
//!   é gravado na conversa).
//! - **Resposta em texto puro (C-03):** o texto final segue para o `format::clean` em [`super::chat`].
//! - **Tom da voz (spec 140 T01, C-28):** o marcador da primeira linha sai aqui, antes de tudo
//!   ([`tone::take_marker`]), e turno contaminado fica sempre no neutro.
//! - **Turno contaminado (T04, C-07):** a [`Session`] da mensagem marca quando há dado não confiável
//!   no contexto: a leitura de `project_details`, uma mensagem encaminhada ou a citação de terceiro
//!   (na mensagem ou na memória). Daí em diante, o silêncio vira cartão de confirmação.
//! - **Procedência (spec 140 T04, C-01, C-02 e C-05):** a contaminação deixa de morrer no fim do
//!   turno: a [`Session`] guarda as fontes ([`Taint`]), a resposta as grava e a memória devolve ao
//!   turno seguinte, até a quarentena tirar o texto do contexto ([`provenance`]).
//! - **Texto de terceiro (T04, C-07):** a mensagem encaminhada vai em `forwarded` e a citação de
//!   terceiro em `quote`, cada uma no envelope de dado não confiável; nunca em `message`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::time::Instant;

use super::can_do::CanDo;
use super::link::Open;
use super::provenance::{self, Taint, TaintKind, TaintSource};
use super::store::{Channel, ConversationMessage, Relayed};
use super::turn::{self, TurnKind};
use super::voice::{tone, Tone};
use super::{history, memory, render, tools, Nanty};
use crate::agents::chat::ChatBackend;
use crate::agents::llm_agents::{assign_missing_call_ids, check_call_ids};
use crate::agents::llm_client::{sync_provider_call_ids, Billing, ChatMessage, ChatRequest, Role, Usage};
use crate::config::{Config, Connection, ConnectionKind, Urgency};
use crate::i18n::Lang;
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Sessão do `cache_key` da conversa.
const CACHE_SESSION: &str = "nanty-chat";
/// Projetos na lista do contexto, no máximo.
const MAX_LISTED: usize = 100;
/// Avisos abertos no contexto, os mais recentes.
const MAX_NOTICES: usize = 10;

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

    /// Os limites da análise (`[nanty.analysis]`, spec 140 T11): prazo, rodadas e tokens próprios; as
    /// chamadas por resposta seguem as da conversa.
    pub fn analysis(cfg: &Config) -> Self {
        let a = &cfg.nanty.analysis;
        Self { rounds: a.rounds(), deadline: a.deadline(), calls: cfg.nanty.chat.calls(), tokens: a.tokens() }
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
    /// O modo do turno (spec 140 T04, C-06): as ferramentas que existem nele.
    pub mode: tools::Mode,
    /// O dado não confiável no contexto do turno, por fonte (C-07; spec 140 T04, C-01 e C-05). Vazio é
    /// turno limpo, e é daqui que vem a procedência gravada na resposta.
    pub tainted: Taint,
    /// Leituras de `project_details` já atendidas.
    pub details: usize,
    /// Projetos consultados.
    pub read: Vec<String>,
    /// Cartões criados na mensagem (ações propostas e o silêncio).
    pub cards: Vec<String>,
    /// Ids do catálogo dos cartões de ação.
    pub actions: Vec<String>,
    pub silence: Option<SilenceEffect>,
    /// O pedido de abrir o Nants (spec 140 T03, C-30): o servidor acrescenta o caminho à resposta.
    pub open: Option<Open>,
    /// Idioma de quem falou (spec 139 T05): o dos resultados das ferramentas.
    pub lang: Lang,
    /// A mensagem da pessoa que abriu o turno (spec 140 T07): a procedência da memória gravada nele.
    pub person_id: Option<String>,
    /// O que as ferramentas da memória fizeram (spec 140 T07).
    pub memory: memory::Effects,
    /// O texto que a pessoa escreveu agora (spec 140 T07): o que ela repete não conta como vindo do
    /// resumo.
    pub person_text: Option<String>,
    /// Projetos cujo conteúdo foi aberto neste turno (spec 140 T10, C-16): `project_details` e a leitura
    /// profunda ([`super::deep`]).
    pub opened: Vec<String>,
    /// O modelo já recebeu conteúdo de projeto neste turno: daqui em diante, só os de [`Self::opened`].
    pub opened_seen: bool,
    /// O teto de leitura do turno (`[nanty.read]`, spec 140 T09), criado na primeira leitura profunda.
    pub read_budget: Option<super::files::Budget>,
    /// O projeto do aviso (spec 140 T12, C-11): na análise, o despachante só aceita este projeto, em
    /// qualquer ferramenta. Posto pelo [`super::analysis::spawn`].
    pub pinned: Option<String>,
    /// O que as ferramentas devolveram na análise (spec 140 T12): a saída do cenário é conferida contra
    /// isto, para nenhum trecho lido sair copiado.
    pub seen: Vec<String>,
    /// O `deep_think` desta mensagem (spec 140 T13, C-25): a rodada em curso e o desfecho, uma vez.
    pub think: super::think::Think,
    /// O cartão de levar ao Arquiteto já saiu nesta mensagem (spec 140 T15): um por mensagem.
    pub drafted: bool,
    /// As fontes lidas pelo próprio turno (spec 141 T07, C-11), fora as herdadas da janela: só a leitura
    /// de outro projeto aqui recusa o rascunho ao Arquiteto ([`super::draft`]).
    pub fresh: Vec<TaintSource>,
}

impl Session {
    pub fn new(channel: Channel, tainted: Taint, lang: Lang) -> Self {
        Self {
            channel,
            mode: tools::Mode::Conversation,
            tainted,
            details: 0,
            read: Vec::new(),
            cards: Vec::new(),
            actions: Vec::new(),
            silence: None,
            open: None,
            lang,
            person_id: None,
            memory: memory::Effects::default(),
            person_text: None,
            opened: Vec::new(),
            opened_seen: false,
            read_budget: None,
            pinned: None,
            seen: Vec::new(),
            think: super::think::Think::default(),
            drafted: false,
            fresh: Vec::new(),
        }
    }

    /// Pode abrir o conteúdo do projeto (spec 140 T10, C-16)? Um projeto já aberto, sim; outro, só
    /// enquanto o modelo ainda não viu conteúdo, ou seja, no mesmo pedido em que pediu o primeiro.
    /// Depois disso, só num novo turno da pessoa (uma sessão nova). Quem recebe `true` lê.
    pub fn admit(&mut self, project_id: &str) -> bool {
        if self.opened.iter().any(|p| p == project_id) {
            return true;
        }
        if self.opened_seen {
            return false;
        }
        self.opened.push(project_id.to_string());
        true
    }

    /// Fim de uma rodada de ferramentas: o que foi aberto nela chega ao modelo no pedido seguinte.
    pub fn end_round(&mut self) {
        if !self.opened.is_empty() {
            self.opened_seen = true;
        }
    }

    /// O turno de análise: só as ferramentas de [`tools::Mode::Analysis`], com a procedência de nascença.
    pub fn analysis(channel: Channel, tainted: Taint, lang: Lang) -> Self {
        Self { mode: tools::Mode::Analysis, ..Self::new(channel, tainted, lang) }
    }

    /// Uma leitura de dado não confiável feita pelo próprio turno ([`Taint::read`]), guardada também à
    /// parte da herança (spec 141 T07, C-11).
    pub fn read_taint(&mut self, kind: TaintKind, project_id: Option<&str>) {
        self.tainted.read(kind, project_id);
        let source = TaintSource { kind, project_id: project_id.map(str::to_string) };
        if !self.fresh.contains(&source) {
            self.fresh.push(source);
        }
    }

    /// O turno tem dado não confiável: nada se grava nem se silencia sozinho.
    pub fn is_tainted(&self) -> bool {
        self.tainted.is_tainted()
    }
}

/// A entrada do modelo depois do sistema (o resumo, o histórico e a mensagem atual, spec 140 T06) e o
/// dado não confiável que ela já traz (uma mensagem encaminhada ou citação de terceiro, agora ou no
/// histórico, e as respostas contaminadas ainda inteiras nele).
#[derive(Debug, Clone)]
pub struct Input {
    pub messages: Vec<ChatMessage>,
    pub taint: Taint,
}

/// Como o laço terminou.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// O texto final do modelo (sem o marcador do tom e ainda sem o `clean`), o tom da fala e as
    /// rodadas.
    Reply { text: String, tone: Tone, rounds: usize },
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
pub(super) fn relay_fields(item: &mut Value, key: &str, relayed: Option<&Relayed>, text: &str, max: usize) -> bool {
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

/// A entrada como vai ao modelo, depois do sistema ([`history::context`]): o resumo e o histórico e,
/// por último, a mensagem da pessoa (JSON) com a data e a hora, a lista de projetos observados (uma
/// linha cada, com a fase da fonte única), os avisos abertos com os ids dos cartões e o texto dela (já
/// redigido). O texto de terceiro vai no envelope de dado não confiável.
pub async fn input(state: &AppState, nanty: &Nanty, cfg: &Config, lang: Lang, person: &ConversationMessage, text: &str) -> Input {
    let secrets = SecretSet::from_config(cfg);
    let now = nanty.now();
    let phases = super::observe::observed_phases(state, cfg).await;
    let projects: Vec<String> = phases
        .iter()
        .take(MAX_LISTED)
        .map(|(p, phase)| {
            // Spec 141 T09 (C-10): o código da fase, e não a frase; as frases ficam para a reserva.
            let phase = phase.map_or("unread", |phase| phase.as_str());
            format!("{} | {} | {}", p.id, tools::project_name(&secrets, p), phase)
        })
        .collect();
    let observed = |id: &Option<String>| id.as_deref().is_none_or(|id| cfg.nanty.observes(id));
    let snap = nanty.snapshot();
    let mut cards: Vec<_> = snap
        .cards
        .iter()
        .filter(|c| c.expires_at_ms > now.epoch_ms && c.urgency != Some(Urgency::Digest) && c.awaits_person() && observed(&c.project_id))
        // Spec 140 T07: o cartão de confirmação da memória traz o texto exato do turno contaminado; ele
        // não volta ao modelo como aviso (seria texto não confiável passando por confiável). Spec 140
        // T15: o de levar ao Arquiteto também, pelo mesmo motivo.
        .filter(|c| !memory::card::is_memory_card(c) && !super::draft::is_draft_card(c))
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
    let mut body = json!({
        "now": history::stamp(now.epoch_ms),
        "projects": projects,
        "notices": notices,
        "can_do": CanDo::now(state, cfg, nanty, person.channel),
        "message": text,
    });
    let relayed = relay_fields(&mut body, "message", person.relayed.as_ref(), text, super::chat::MAX_QUOTE_CHARS);
    let history::Context { mut messages, mut taint } = history::context(nanty, cfg, lang, person, body);
    if relayed {
        taint.add(TaintKind::Relayed, None);
        taint.origin.get_or_insert_with(|| person.id.clone());
    }
    for m in &mut messages {
        m.content = secrets.scrub_exact(&m.content);
    }
    Input { messages, taint }
}

/// O texto de terceiro que saiu da janela: a linha do servidor no lugar dele, sem envelope.
pub(super) fn quarantine_relayed(item: &mut Value, lang: Lang, at: &str, relayed: Option<&Relayed>) {
    let Some(r) = relayed else { return };
    let line = provenance::relayed_line(lang, at);
    if r.forwarded {
        item["text"] = json!(line);
    } else if r.third_party {
        item["quote"] = json!(line);
    }
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
pub(super) struct Spent {
    pub(super) input: u64,
    pub(super) output: u64,
    pub(super) usd: Option<f64>,
    /// Tokens das rodadas com o esforço maior do `deep_think` (spec 140 T13), já somados acima.
    pub(super) deep_tokens: u64,
    /// O desfecho do `deep_think`, que vai ao registro com o gasto.
    pub(super) deep_think: Option<&'static str>,
}

impl Spent {
    fn total(&self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

/// Soma o gasto de uma rodada no dia (o mesmo cálculo do turno proativo e do resumo). `analysis`: a
/// rodada é de uma análise, e entra também na parte dela (spec 140 T12, C-19).
#[allow(clippy::too_many_arguments)]
pub(super) fn account(
    nanty: &Nanty,
    cfg: &Config,
    plan: bool,
    model: &str,
    request: &ChatRequest,
    response: &crate::agents::llm_client::ChatResponse,
    spent: &mut Spent,
    analysis: bool,
) -> u64 {
    let billing = if plan || response.billing == Billing::ChatgptPlan { Billing::ChatgptPlan } else { response.billing };
    let usage = response.usage.unwrap_or_else(|| {
        let sent: u32 = request.messages.iter().map(|m| turn::estimate_tokens(&m.content)).fold(0, u32::saturating_add);
        Usage::new(sent, turn::estimate_tokens(&response.message.content))
    });
    let usd = turn::turn_cost(cfg, model, &usage, billing);
    let day = nanty.now().day_key();
    nanty.update(|st| {
        st.add_spend(&day, u64::from(usage.prompt), u64::from(usage.completion), usd);
        if analysis {
            st.add_analysis_spend(&day, u64::from(usage.prompt).saturating_add(u64::from(usage.completion)), usd);
        }
        true
    });
    nanty.changed(super::Part::Status);
    spent.input = spent.input.saturating_add(u64::from(usage.prompt));
    spent.output = spent.output.saturating_add(u64::from(usage.completion));
    if let Some(v) = usd {
        spent.usd = Some(spent.usd.unwrap_or(0.0) + v);
    }
    u64::from(usage.prompt).saturating_add(u64::from(usage.completion))
}

fn record(nanty: &Nanty, kind: TurnKind, code: &str, reason: Option<&str>, rounds: usize, calls: usize, spent: &Spent) {
    let mut entry = turn::turn_entry(nanty, code, kind, reason);
    entry.cost_usd = spent.usd;
    entry.input_tokens = Some(spent.input);
    entry.output_tokens = Some(spent.output);
    entry.params.insert("rounds".into(), json!(rounds));
    entry.params.insert("tool_calls".into(), json!(calls));
    // Spec 140 T13: o desfecho do `deep_think` e o que as rodadas maiores gastaram.
    if let Some(code) = spent.deep_think {
        entry.params.insert("deep_think".into(), json!(code));
        entry.params.insert("deep_tokens".into(), json!(spent.deep_tokens));
    }
    nanty.record(&entry);
}

/// O cliente do modelo do Nanty: o simulado dos testes ou o da conexão do `[nanty]`.
pub(super) fn backend(state: &AppState, nanty: &Nanty, conn: &Connection) -> Option<Arc<dyn ChatBackend>> {
    match nanty.backend() {
        Some(fake) => Some(fake),
        None => crate::server::chatgpt::plan_client::llm_client(state, conn)
            .and_then(|c| c.with_timeout(turn::TURN_TIMEOUT))
            .ok()
            .map(|client| Arc::new(client) as Arc<dyn ChatBackend>),
    }
}

/// O laço: confere o modelo e o orçamento, espera a vez na fila, e então, rodada a rodada, chama o
/// modelo e atende as ferramentas, até a resposta final em texto. `context` é o que vem depois do
/// sistema ([`Input::messages`]). A `session` guarda o que as ferramentas fizeram (os cartões e o
/// silêncio valem mesmo se o laço terminar sem texto).
pub async fn run(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, system: String, context: Vec<ChatMessage>, session: &mut Session) -> Ended {
    let Some((conn, model)) = turn::model_route(cfg).map(|(c, m)| (c.clone(), m.to_string())) else {
        return skip(nanty, "no_model");
    };
    if turn::budget_view(cfg, &nanty.snapshot().spend_on(&nanty.now().day_key())).exhausted {
        return skip(nanty, "budget");
    }
    let Some(_turn) = nanty.turns().enter().await else {
        return skip(nanty, "busy");
    };
    let Some(backend) = backend(state, nanty, &conn) else {
        return skip(nanty, "failed");
    };
    let rounds = Rounds { kind: TurnKind::Conversation, conn: &conn, model: &model, backend, limits: Limits::of(cfg), cancel: None };
    rounds.run(state, nanty, cfg, system, context, session).await
}

/// As rodadas de um turno com ferramentas, já com a vez dele: a da conversa ([`run`], com a vez da
/// fila única) ou a da análise ([`super::analysis`], com o lugar dela). O modelo e o orçamento já
/// foram conferidos por quem chama; o orçamento volta a ser conferido antes de cada rodada.
pub(super) struct Rounds<'a> {
    pub(super) kind: TurnKind,
    pub(super) conn: &'a Connection,
    pub(super) model: &'a str,
    pub(super) backend: Arc<dyn ChatBackend>,
    pub(super) limits: Limits,
    /// A condição de cancelamento (só a análise, spec 140 T11): conferida antes de cada rodada, depois
    /// de cada resposta e antes de cada ferramenta; a espera do modelo termina quando ela dispara.
    pub(super) cancel: Option<&'a super::analysis::Cancel>,
}

impl Rounds<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(super::analysis::Cancel::is_cancelled)
    }

    /// Encerra sem texto: o registro com o código e o motivo.
    fn stop(&self, nanty: &Nanty, code: &str, reason: &'static str, rounds: usize, calls: usize, spent: &Spent) -> Ended {
        record(nanty, self.kind, code, Some(reason), rounds, calls, spent);
        Ended::Skipped(if code == "turn.invalid" { "invalid" } else { reason })
    }

    pub(super) async fn run(&self, state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, system: String, context: Vec<ChatMessage>, session: &mut Session) -> Ended {
        let (limits, model) = (self.limits, self.model);
        // Spec 140 T12 (C-19): a análise também para quando gasta a fatia dela no dia.
        let analysis = self.kind == TurnKind::Analysis;
        let exhausted = || {
            let spend = nanty.snapshot().spend_on(&nanty.now().day_key());
            turn::budget_view(cfg, &spend).exhausted || (analysis && !super::analysis::share_left(cfg, &spend))
        };
        let deadline = Instant::now() + limits.deadline;
        let lang = session.lang;
        let secrets = SecretSet::from_config(cfg);
        let plan = self.conn.kind == ConnectionKind::ChatgptPlan;
        // O que o modelo recebe; o despachante confere a mesma lista (spec 140 T04, C-06).
        let mode = session.mode;
        let specs: Vec<_> = crate::agents::prompts::nanty_chat_tools().into_iter().filter(|s| mode.allows(&s.name)).collect();
        let system = ChatMessage::text(Role::System, system);
        let cache_key = crate::agents::usage::cache_key(Path::new(super::NANTY_DIR), CACHE_SESSION, std::slice::from_ref(&system));
        let mut messages = vec![system];
        messages.extend(context);
        let _thinking = nanty.thinking();
        let mut seq = 0usize;
        let mut spent = Spent::default();
        let mut calls_made = 0usize;
        // Spec 140 T13 (C-25): o esforço da rodada; o `deep_think` sobe as seguintes, uma vez.
        let configured = cfg.nanty.effort;
        let mut effort = configured;
        let rejection = super::think::rejection_key(&self.conn.id, model);
        let mut fell_back = nanty.effort_rejected(&rejection);
        for round in 0..limits.rounds {
            if self.cancelled() {
                return self.stop(nanty, "turn.skipped", "cancelled", round, calls_made, &spent);
            }
            if Instant::now() >= deadline {
                return self.stop(nanty, "turn.skipped", "deadline", round, calls_made, &spent);
            }
            // Antes de cada rodada: outro turno pode ter gastado o resto, e esta mensagem também gasta.
            if exhausted() {
                return self.stop(nanty, "turn.skipped", "budget", round, calls_made, &spent);
            }
            let last = round + 1 == limits.rounds || spent.total() >= limits.tokens;
            let request = ChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                tools: if last { Vec::new() } else { specs.clone() },
                effort: Some(effort),
                response_json: false,
                web_search: false,
                web_search_max_uses: None,
                cache_key: Some(cache_key.clone()),
            };
            let waited = tokio::time::timeout_at(deadline, self.backend.complete(&request));
            let waited = match self.cancel {
                None => Some(waited.await),
                Some(cancel) => tokio::select! {
                    biased;
                    () = cancel.cancelled() => None,
                    r = waited => Some(r),
                },
            };
            let response = match waited {
                None => return self.stop(nanty, "turn.skipped", "cancelled", round, calls_made, &spent),
                Some(Err(_)) => return self.stop(nanty, "turn.skipped", "deadline", round, calls_made, &spent),
                Some(Ok(Err(err))) => {
                    let reason = crate::agents::llm_client::provider_failure(&err).reason_kind().map(|(r, _)| r);
                    record(nanty, self.kind, "turn.failed", reason, round, calls_made, &spent);
                    return Ended::Skipped("failed");
                }
                Some(Ok(Ok(response))) => response,
            };
            let used = account(nanty, cfg, plan, model, &request, &response, &mut spent, analysis);
            if effort != configured {
                spent.deep_tokens = spent.deep_tokens.saturating_add(used);
            }
            // O adaptador tirou o esforço para o pedido passar: o modelo não aceita escolher o nível.
            if response.fallback_used {
                fell_back = true;
                if effort != configured {
                    // A rodada maior não passou com o nível: volta ao normal, e o processo lembra.
                    nanty.note_effort_rejected(rejection.clone());
                    effort = configured;
                    session.think.outcome = Some(super::think::Outcome::Rejected);
                    spent.deep_think = Some(super::think::Outcome::Rejected.code());
                }
            }
            // A resposta que chegou depois de a condição se resolver não vale: nada dela segue.
            if self.cancelled() {
                return self.stop(nanty, "turn.skipped", "cancelled", round + 1, calls_made, &spent);
            }
            let mut message = response.message;
            assign_missing_call_ids(&mut message, &messages, &mut seq);
            sync_provider_call_ids(&mut message);
            if check_call_ids(&message, &messages).is_err() {
                return self.stop(nanty, "turn.invalid", "call_id", round + 1, calls_made, &spent);
            }
            if message.tool_calls.is_empty() || last {
                // O começo com a data do histórico, se o modelo o imitou, nunca chega à pessoa.
                let (chosen, text) = tone::take_marker(&history::strip_stamp(&message.content));
                let text = history::strip_stamp(&text);
                if text.is_empty() {
                    let reason = if message.tool_calls.is_empty() { "empty" } else { "rounds" };
                    record(nanty, self.kind, "turn.invalid", Some(reason), round + 1, calls_made, &spent);
                    return Ended::Skipped(if reason == "empty" { "invalid" } else { "rounds" });
                }
                record(nanty, self.kind, "turn.ok", None, round + 1, calls_made, &spent);
                // Depois de todas as ferramentas: a contaminação de qualquer rodada já está na sessão.
                let tone = if session.is_tainted() { Tone::Neutral } else { chosen };
                return Ended::Reply { text, tone, rounds: round + 1 };
            }
            let calls = message.tool_calls.clone();
            // A resposta entra como veio (com os `provider_items`), antes dos resultados.
            messages.push(message);
            session.think.round = Some(super::think::Round {
                connection_id: self.conn.id.clone(),
                model: model.to_string(),
                plan,
                effort: configured,
                input_tokens: super::think::estimate_input(&messages, &specs),
                spent_tokens: spent.total(),
                max_tokens: limits.tokens,
                rejected: fell_back,
            });
            for (i, call) in calls.into_iter().enumerate() {
                if self.cancelled() {
                    return self.stop(nanty, "turn.skipped", "cancelled", round + 1, calls_made, &spent);
                }
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
                if analysis {
                    session.seen.push(content.clone());
                }
                messages.push(tool_result(call.id, content));
            }
            // Spec 140 T10 (C-16): o modelo vê agora o que foi lido; outro projeto, só num novo turno dela.
            session.end_round();
            // Spec 140 T13: o pedido aceito vale das próximas rodadas em diante, sobre as mesmas
            // mensagens (nada é lido de novo) e a mesma sessão (a procedência continua).
            match (session.think.outcome, spent.deep_think) {
                (Some(super::think::Outcome::Raised(raised)), None) => {
                    effort = raised;
                    spent.deep_think = Some(super::think::Outcome::Raised(raised).code());
                }
                (Some(outcome), None) => spent.deep_think = Some(outcome.code()),
                _ => {}
            }
        }
        record(nanty, self.kind, "turn.invalid", Some("rounds"), limits.rounds, calls_made, &spent);
        Ended::Skipped("rounds")
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
pub(crate) mod tests;
