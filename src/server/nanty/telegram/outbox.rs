//! Fila de saída do Telegram (spec 138 T07, C-07, C-10 e C-12): os cartões, a edição depois da
//! decisão, as mensagens do Nanty e o resumo da manhã.
//!
//! - **Cartões:** cada cartão aberto que não é só do resumo sai uma vez (de novo só quando o filtro o
//!   renova, numa repetição). O `push` vai com som; o `silent` (urgente no horário de silêncio) vai
//!   com `disable_notification`; o `digest` fica para o resumo da manhã. Os botões são inline, com o
//!   `callback_data` `c:<id>:<token>:<y|n|l>` ([`super::callback_data`]); sem token, o cartão vai sem
//!   botões. O `message_id` de cada envio fica no estado ([`SentCard`]). O cartão novo espera
//!   [`SEND_SETTLE`]: a mesma rodada ainda pode pôr a oferta nele (id e token novos).
//! - **Edição:** decidido (na janela, no Telegram ou pelo Nanty), resolvido ou vencido, o cartão
//!   some do estado e a mensagem é editada: perde os botões e ganha a linha de quem decidiu e quando
//!   (ou de resolvido, ou de expirado). A oferta que sai do cartão (ou a renovação) só tira os botões
//!   da mensagem antiga.
//! - **Mensagens:** toda fala do Nanty na conversa (o turno proativo, o resumo da manhã e a resposta
//!   a quem escreveu no Telegram, T08) entra na fila; a resposta a quem escreveu na janela e o cartão
//!   pedido na janela ficam só nela. A mensagem entra pelo [`Outbox::push_message`] e sai pelo
//!   [`super::super::render`], no idioma do Nanty, sem som no horário de silêncio.
//! - **Tudo passa pelo [`super::format::clean`]:** o `SecretSet` da configuração, sem código, sem
//!   link, texto puro e no limite de 4096.
//! - **Limite (C-10):** no máximo uma chamada a cada [`SEND_GAP`] no chat. Mais de [`MAX_BURST`]
//!   envios esperando de uma vez viram uma mensagem só, o resumo da fila (sem botões: a decisão fica
//!   na janela).
//! - **Aba visível (C-07):** com uma aba do Nants visível (avisada pelo `visibilitychange`, em
//!   `POST /api/nanty/presence`), o envio espera `visible_hold_secs`; o que for decidido na janela
//!   nesse meio não sai. Ligado por padrão (`[nanty.telegram] hold_while_visible`).
//! - **Voz (spec 139 T08, C-14):** com a voz ligada, o aviso sai como nota de voz, com os botões e a
//!   legenda, e o texto completo à parte quando a legenda corta.
//!   - **Fora da fila:** a fala é gerada numa tarefa à parte, só depois das esperas acima (o que for
//!     decidido na janela não custa fala), uma de cada vez e com prazo. Enquanto ela anda, a fila
//!     segue com o resto, menos o que cita o cartão. Vencido o prazo, sem orçamento ou com falha, o
//!     aviso sai em texto.
//!   - **Um áudio por aviso:** o texto do modelo sobre o aviso segue em texto, e o cartão pedido na
//!     conversa também: a fala é a da resposta que o cita. A rajada continua em texto.
//!   - **Edição:** a nota de voz é editada pelo `editMessageCaption`, com a linha final dentro de 1024.
//!   - **Recusa:** o Telegram recusou a nota (4xx), o aviso sai pelo `sendMessage`, com os botões.
//! - **Erros:** nenhum derruba o Nanty. 429 espera o `retry_after`; rede e 5xx esperam cada vez mais;
//!   uma recusa do Telegram (4xx) desiste daquele envio e fica no registro.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::sync::{watch, Notify};

use super::client::{TelegramClient, TgError};
use super::format::{self, clean};
use super::{callback_data, journal, param, valid_token, Health, STALE_GROUP};
use crate::config::{Config, Urgency};
use crate::i18n::{tr, tr_plural, Lang};
use crate::server::devices::hash_hex;
use crate::server::events::SecretSet;
use crate::server::nanty::act::Answer;
use crate::server::nanty::actions::{self, ActionKind};
use crate::server::nanty::clock;
use crate::server::nanty::render;
use crate::server::nanty::store::{Card, Channel, ConversationMessage, DecidedBy, Delivery, NantyState, SentCard, SentKind, Speaker};
use crate::server::nanty::voice::{self, Spoken};
use crate::server::nanty::{nanty_lang, new_id, InstanceStatus, Nanty};
use crate::server::state::AppState;

/// Intervalo mínimo entre duas chamadas ao chat (C-10).
pub const SEND_GAP: Duration = Duration::from_secs(1);
/// Renovação do "digitando" (spec 139 T03, C-09): o Telegram o mostra por até 5 segundos.
pub const TYPING_EVERY: Duration = Duration::from_secs(4);
/// Prazo da fala (spec 139 T08, C-14; provisório, abaixo do prazo da chamada ao provedor): vencido,
/// a saída vai em texto.
pub const SPEECH_DEADLINE: Duration = Duration::from_secs(45);
/// Acomodação do cartão novo: a rodada que o criou ainda pode pôr a oferta nele (o que troca o id e
/// o token, T03); o envio espera para não mandar um cartão que já nasce vencido.
pub const SEND_SETTLE: Duration = Duration::from_secs(2);
/// Mais envios que isto esperando de uma vez viram um resumo da fila.
pub const MAX_BURST: usize = 5;
/// Validade do aviso de aba visível: a aba renova antes disso enquanto segue visível.
pub const PRESENCE_TTL_MS: u64 = 150_000;
/// Abas lembradas ao mesmo tempo.
const MAX_TABS: usize = 32;
/// Mensagens esperando na fila (as mais velhas saem).
const MAX_QUEUED: usize = 50;
/// Sem nada a enviar, a fila confere de novo neste intervalo (a espera da aba visível vence).
const IDLE: Duration = Duration::from_secs(1);
/// Sem token, sem dono ou com o token recusado, a fila confere de novo neste intervalo.
const OFF_WAIT: Duration = Duration::from_secs(5);
/// Espera crescente depois de falha de rede: o começo e o teto.
const RETRY_BASE: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Linhas do resumo da fila e o tamanho de cada uma.
const BATCH_LINES: usize = 10;
const BATCH_LINE_CHARS: usize = 160;
/// Tamanho do id de aba aceito.
const MAX_TAB_CHARS: usize = 64;

/// Mensagem do Nanty esperando a vez.
#[derive(Debug, Clone)]
struct Queued {
    message: ConversationMessage,
    at_ms: u64,
    /// A nota de voz já saiu (ou o Telegram a recusou): falta o texto completo (T09).
    voice_sent: bool,
}

#[derive(Debug, Default)]
struct Inner {
    queue: VecDeque<Queued>,
    /// Aba visível -> até quando o aviso vale.
    tabs: BTreeMap<String, u64>,
    /// Mensagens que só perdem os botões (o botão de um cartão que a fila não conhece mais).
    strips: VecDeque<(i64, i64)>,
    /// Respostas da conversa em andamento que pediram o "digitando" (spec 139 T03).
    typing: usize,
    /// Falas em andamento que pediram o "gravando áudio" (spec 139 T08).
    recording: usize,
    /// Quando o último "digitando" ou "gravando áudio" saiu.
    typing_last: Option<std::time::Instant>,
    /// A fala dos cartões, por id (spec 139 T08).
    speech: BTreeMap<String, CardSpeech>,
}

/// A fala de um cartão, gerada fora da fila (spec 139 T08, C-14).
#[derive(Debug, Clone)]
enum Speech {
    /// A tarefa à parte está gerando.
    Pending,
    Ready(Arc<Spoken>),
    /// Sem fala (prazo, orçamento ou falha): o cartão sai em texto.
    Text,
}

/// A fala de uma mensagem de cartão: a renovação (outro `created_at_ms`) é um aviso novo.
#[derive(Debug, Clone)]
struct CardSpeech {
    created_at_ms: u64,
    state: Speech,
}

/// O "digitando" pedido por uma resposta da conversa (spec 139 T03, C-09): enquanto existir, a fila
/// o renova a cada [`TYPING_EVERY`], só quando não há nada a enviar e com o mesmo intervalo entre
/// chamadas dos outros envios ([`SEND_GAP`]). Solto em qualquer saída.
#[derive(Debug)]
pub struct Typing<'a>(&'a Outbox);

impl Drop for Typing<'_> {
    fn drop(&mut self) {
        let mut inner = self.0.lock();
        inner.typing = inner.typing.saturating_sub(1);
        inner.action_ended();
    }
}

/// O "gravando áudio" pedido por uma fala (spec 139 T08): como o [`Typing`], e vale mais que ele
/// enquanto existir. Solto em qualquer saída.
#[derive(Debug)]
pub struct Recording<'a>(&'a Outbox);

impl Drop for Recording<'_> {
    fn drop(&mut self) {
        let mut inner = self.0.lock();
        inner.recording = inner.recording.saturating_sub(1);
        inner.action_ended();
    }
}

impl Inner {
    /// Sem nada pedido, o próximo pedido sai na hora.
    fn action_ended(&mut self) {
        if self.typing == 0 && self.recording == 0 {
            self.typing_last = None;
        }
    }
}

/// A fila de saída deste processo (em memória; os cartões enviados ficam no estado).
#[derive(Debug, Default)]
pub struct Outbox {
    inner: Mutex<Inner>,
    wake: Notify,
}

/// Id de aba aceito: letras, dígitos, `-` e `_`.
pub fn valid_tab(tab: &str) -> bool {
    (1..=MAX_TAB_CHARS).contains(&tab.len()) && tab.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Outbox {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Acorda a fila (algo mudou no estado).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Fala do Nanty: entra na fila. A da pessoa não volta para ela, e a resposta a quem escreveu na
    /// janela fica só na janela (T08).
    pub fn push_message(&self, message: &ConversationMessage, now_ms: u64) {
        if message.from != Speaker::Nanty || (message.reply_to.is_some() && message.channel == Channel::Web) {
            return;
        }
        {
            let mut inner = self.lock();
            inner.queue.push_back(Queued { message: message.clone(), at_ms: now_ms, voice_sent: false });
            while inner.queue.len() > MAX_QUEUED {
                inner.queue.pop_front();
            }
        }
        self.wake();
    }

    /// Pede o "digitando" enquanto o guarda existir (só a resposta a quem escreveu pelo Telegram).
    pub fn typing(&self) -> Typing<'_> {
        self.lock().typing += 1;
        self.wake();
        Typing(self)
    }

    /// Pede o "gravando áudio" enquanto o guarda existir (só a fala para quem escreveu pelo Telegram).
    pub fn recording(&self) -> Recording<'_> {
        self.lock().recording += 1;
        self.wake();
        Recording(self)
    }

    /// Há "digitando" ou "gravando áudio" pedido.
    fn typing_active(&self) -> bool {
        let inner = self.lock();
        inner.typing > 0 || inner.recording > 0
    }

    /// A ação a mandar (ou renovar) agora, se for a hora: marca o envio.
    fn action_due(&self, every: Duration) -> Option<&'static str> {
        let mut inner = self.lock();
        let action = match (inner.recording > 0, inner.typing > 0) {
            (true, _) => "record_voice",
            (false, true) => "typing",
            (false, false) => return None,
        };
        if inner.typing_last.is_some_and(|at| at.elapsed() < every) {
            return None;
        }
        inner.typing_last = Some(std::time::Instant::now());
        Some(action)
    }

    /// A tarefa da fala do cartão começou.
    fn speech_started(&self, card: &Card) {
        self.lock().speech.insert(card.id.clone(), CardSpeech { created_at_ms: card.created_at_ms, state: Speech::Pending });
    }

    /// A fala terminou (`None` = fica o texto). Sem efeito se o cartão saiu ou foi renovado no meio.
    fn speech_done(&self, card_id: &str, created_at_ms: u64, spoken: Option<Spoken>) {
        {
            let mut inner = self.lock();
            let Some(entry) = inner.speech.get_mut(card_id).filter(|e| e.created_at_ms == created_at_ms) else { return };
            entry.state = spoken.map_or(Speech::Text, |s| Speech::Ready(Arc::new(s)));
        }
        self.wake();
    }

    /// A fala pronta do cartão.
    fn spoken(&self, card: &Card) -> Option<Arc<Spoken>> {
        match self.lock().speech.get(&card.id) {
            Some(CardSpeech { created_at_ms, state: Speech::Ready(spoken) }) if *created_at_ms == card.created_at_ms => Some(Arc::clone(spoken)),
            _ => None,
        }
    }

    /// Esquece a fala do que já saiu.
    fn forget_speech(&self, card_id: &str) {
        self.lock().speech.remove(card_id);
    }

    /// Esquece a fala dos cartões que saíram do estado ou foram renovados (decididos no meio dela).
    fn keep_speech_of(&self, st: &NantyState) {
        self.lock().speech.retain(|id, e| st.cards.iter().any(|c| c.id == *id && c.created_at_ms == e.created_at_ms));
    }

    /// O texto completo do aviso cuja legenda cortou: é a próxima mensagem, sem espera e sem voz.
    fn push_full_text(&self, text: &str) {
        let mut message = ConversationMessage::from_nanty(new_id(), 0, Channel::Telegram, "", Map::new());
        message.code = None;
        message.text = Some(text.to_string());
        self.lock().queue.push_front(Queued { message, at_ms: 0, voice_sent: true });
    }

    /// Mensagens esperando.
    pub fn queued(&self) -> usize {
        self.lock().queue.len()
    }

    /// Tira os botões de uma mensagem que a fila não conhece.
    pub fn strip(&self, chat_id: i64, message_id: i64) {
        {
            let mut inner = self.lock();
            if !inner.strips.contains(&(chat_id, message_id)) {
                inner.strips.push_back((chat_id, message_id));
            }
            while inner.strips.len() > MAX_QUEUED {
                inner.strips.pop_front();
            }
        }
        self.wake();
    }

    /// Aviso de uma aba: visível (renova por [`PRESENCE_TTL_MS`]) ou oculta (sai).
    pub fn presence(&self, tab: &str, visible: bool, now_ms: u64) {
        {
            let mut inner = self.lock();
            inner.tabs.retain(|_, until| *until > now_ms);
            if visible {
                if inner.tabs.len() >= MAX_TABS && !inner.tabs.contains_key(tab) {
                    // Cheio: a aba mais perto de vencer dá lugar.
                    if let Some(oldest) = inner.tabs.iter().min_by_key(|(_, u)| **u).map(|(k, _)| k.clone()) {
                        inner.tabs.remove(&oldest);
                    }
                }
                inner.tabs.insert(tab.to_string(), now_ms.saturating_add(PRESENCE_TTL_MS));
            } else {
                inner.tabs.remove(tab);
            }
        }
        self.wake();
    }

    /// Há uma aba do Nants visível agora.
    pub fn visible(&self, now_ms: u64) -> bool {
        self.lock().tabs.values().any(|until| *until > now_ms)
    }

    fn clear_queue(&self) {
        let mut inner = self.lock();
        inner.queue.clear();
        inner.strips.clear();
        inner.speech.clear();
    }
}

// ---- Textos ----

fn hhmm(at_ms: u64) -> String {
    clock::local_time_at(at_ms).map(|t| format!("{:02}:{:02}", t.hour, t.minute)).unwrap_or_default()
}

/// Texto do cartão: o projeto (pelo nome) e o porquê do observador.
fn card_text(lang: Lang, cfg: &Config, card: &Card) -> String {
    let why = render::card_text(lang, card);
    let project = card.project_id.as_deref().and_then(|id| cfg.projects.iter().find(|p| p.id == id)).map(|p| p.name.as_str());
    match project {
        Some(name) => format!("{}\n{why}", tr(lang, "nanty.telegram.card_project", &[("project", name)])),
        None => why,
    }
}

fn button(text: String, card: &Card, token: &str, answer: Answer) -> Value {
    json!({ "text": text, "callback_data": callback_data(&card.id, token, answer) })
}

/// Os botões do cartão: a ação oferecida (se a pessoa pode fazê-la por aqui), "não" e "lembrar
/// depois". Sem token, nenhum.
fn keyboard(lang: Lang, card: &Card) -> Option<Value> {
    let token = card.token.as_deref()?;
    let mut rows: Vec<Value> = Vec::new();
    let mut last = Vec::new();
    let action = card.action.as_deref().and_then(actions::parse);
    // O silêncio da conversa (spec 139 T04): o botão da ação; a confirmação também tem o "não", e
    // nenhum dos dois tem "lembrar depois".
    if let Some(action) = action.filter(|a| a.entry().kind == ActionKind::Silence) {
        rows.push(json!([button(tr(lang, &action.label_code(), &[]), card, token, Answer::Yes)]));
        if action == actions::ActionId::SetSilence {
            rows.push(json!([button(tr(lang, "nanty.telegram.button_no", &[]), card, token, Answer::No)]));
        }
        return Some(Value::Array(rows));
    }
    if let Some(action) = action {
        if matches!(action.entry().kind, ActionKind::Executes | ActionKind::Link) {
            rows.push(json!([button(tr(lang, &action.label_code(), &[]), card, token, Answer::Yes)]));
        }
        last.push(button(tr(lang, "nanty.telegram.button_no", &[]), card, token, Answer::No));
    }
    last.push(button(tr(lang, "nanty.action.remind_later", &[]), card, token, Answer::Later));
    rows.push(Value::Array(last));
    Some(Value::Array(rows))
}

fn by_key(by: DecidedBy) -> &'static str {
    match by {
        DecidedBy::Auto => "auto",
        DecidedBy::Telegram => "telegram",
        DecidedBy::Web => "web",
    }
}

/// A linha do fim do cartão: quem decidiu e quando, resolvido ou expirado.
fn final_line(lang: Lang, st: &NantyState, card_id: &str) -> String {
    if let Some(d) = st.decided.iter().rev().find(|d| d.card_id == card_id) {
        let answer = tr(lang, &format!("nanty.telegram.answer.{}", d.answer), &[]);
        return tr(lang, &format!("nanty.telegram.decided.{}", by_key(d.decided_by)), &[("answer", &answer), ("time", &hhmm(d.at_ms))]);
    }
    if let Some(done) = st.done.iter().rev().find(|d| d.card_id.as_deref() == Some(card_id) && d.result == crate::server::nanty::filter::RESOLVED) {
        return tr(lang, "nanty.telegram.resolved", &[("time", &hhmm(done.at_ms))]);
    }
    tr(lang, "nanty.telegram.expired", &[])
}

// ---- O que fazer agora ----

/// Um envio novo.
#[derive(Debug, Clone)]
enum Item {
    Card(Card),
    Message(Queued),
}

/// A próxima chamada ao Telegram.
#[derive(Debug)]
enum Op {
    /// Tirar os botões de uma mensagem que a fila não conhece.
    Strip(i64, i64),
    /// O cartão saiu do estado: a mensagem vira o texto final, sem botões.
    Finish(SentCard),
    /// O cartão segue aberto, mas a mensagem antiga perde os botões (oferta que saiu ou renovação).
    Clear(SentCard),
    /// Envios novos: um só, ou o resumo da fila.
    Send(Vec<Item>),
    /// Gerar a fala do cartão, numa tarefa à parte (não é uma chamada ao Telegram).
    Speak(Card),
}

/// A fala dos cartões vista pela escolha da próxima chamada (spec 139 T08). `None` em
/// [`next_op`] = voz desligada.
type SpeechMap = BTreeMap<String, CardSpeech>;

/// O que falta para o cartão sair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ahead {
    /// Pode sair: em voz, com a fala pronta, ou em texto.
    Go,
    /// A fala ainda não começou.
    Start,
    /// A fala está sendo gerada.
    Pending,
}

/// O cartão fala: com a voz ligada, menos o pedido na conversa (a fala é a da resposta que o cita).
fn ahead(speech: Option<&SpeechMap>, card: &Card) -> Ahead {
    let Some(speech) = speech.filter(|_| card.requested.is_none()) else { return Ahead::Go };
    match speech.get(&card.id).filter(|e| e.created_at_ms == card.created_at_ms) {
        None => Ahead::Start,
        Some(CardSpeech { state: Speech::Pending, .. }) => Ahead::Pending,
        Some(_) => Ahead::Go,
    }
}

/// Pode ir ao Telegram: aberto, não antigo, não só do resumo e não pedido na janela (T08).
fn sendable(card: &Card, now_ms: u64) -> bool {
    card.expires_at_ms > now_ms
        && card.requested != Some(Channel::Web)
        && !card.group.starts_with(STALE_GROUP)
        && card.delivery != Some(Delivery::Digest)
        && card.urgency != Some(Urgency::Digest)
}

/// A espera acabou para o item criado em `at_ms`.
fn released(at_ms: u64, wait_ms: u64, now_ms: u64) -> bool {
    wait_ms == 0 || now_ms >= at_ms.saturating_add(wait_ms)
}

/// Esperas antes do envio: a do cartão (a maior entre a acomodação e a da aba visível) e a da
/// mensagem (só a da aba visível). O que responde a quem escreveu no Telegram (T08) não espera a
/// aba: o cartão pedido só espera a acomodação, e a resposta sai logo.
#[derive(Debug, Clone, Copy)]
struct Waits {
    card_ms: u64,
    message_ms: u64,
    settle_ms: u64,
}

impl Waits {
    fn card(&self, card: &Card) -> u64 {
        if card.requested == Some(Channel::Telegram) {
            self.settle_ms
        } else {
            self.card_ms
        }
    }

    fn message(&self, message: &ConversationMessage) -> u64 {
        if message.reply_to.is_some() {
            0
        } else {
            self.message_ms
        }
    }
}

/// Escolhe a próxima chamada. Registros de outro chat (desparear, trocar o token) ou sem mensagem
/// cujo cartão já saiu são esquecidos aqui, sem chamada (`forget`).
#[allow(clippy::too_many_arguments)]
fn next_op(
    st: &NantyState,
    queue: &[Queued],
    strip: Option<(i64, i64)>,
    speech: Option<&SpeechMap>,
    chat_id: i64,
    waits: Waits,
    now_ms: u64,
    forget: &mut Vec<String>,
) -> Option<Op> {
    for rec in &st.telegram_sent {
        let card = st.cards.iter().find(|c| c.id == rec.card_id);
        if rec.chat_id != chat_id || (card.is_none() && rec.message_id.is_none()) {
            forget.push(rec.card_id.clone());
        }
    }
    if let Some((chat, message)) = strip {
        return Some(Op::Strip(chat, message));
    }
    let live = || st.telegram_sent.iter().filter(|r| r.chat_id == chat_id && r.message_id.is_some());
    if let Some(rec) = live().find(|r| !st.cards.iter().any(|c| c.id == r.card_id)) {
        return Some(Op::Finish(rec.clone()));
    }
    let stale_buttons = live().find(|r| {
        r.buttons && st.cards.iter().find(|c| c.id == r.card_id).is_some_and(|c| c.token.is_none() || c.created_at_ms > r.created_at_ms)
    });
    if let Some(rec) = stale_buttons {
        return Some(Op::Clear(rec.clone()));
    }
    let mut items: Vec<Item> = st
        .cards
        .iter()
        .filter(|c| sendable(c, now_ms) && released(c.created_at_ms, waits.card(c), now_ms))
        .filter(|c| {
            !st.telegram_sent.iter().any(|r| r.card_id == c.id && r.chat_id == chat_id && r.created_at_ms >= c.created_at_ms)
        })
        .cloned()
        .map(Item::Card)
        .collect();
    items.extend(queue.iter().filter(|q| released(q.at_ms, waits.message(&q.message), now_ms)).cloned().map(Item::Message));
    if items.len() > MAX_BURST {
        return Some(Op::Send(items));
    }
    // Uma fala de cada vez. O cartão que espera a dele não segura o resto da fila, só o que o cita.
    let speaking = speech.is_some_and(|s| s.values().any(|e| matches!(e.state, Speech::Pending)));
    let mut waiting: Vec<&str> = Vec::new();
    for item in &items {
        match item {
            Item::Card(card) => match ahead(speech, card) {
                Ahead::Go => return Some(Op::Send(vec![item.clone()])),
                Ahead::Start if !speaking => return Some(Op::Speak(card.clone())),
                Ahead::Start | Ahead::Pending => waiting.push(&card.id),
            },
            Item::Message(q) => {
                if !q.message.card_ids.iter().any(|id| waiting.contains(&id.as_str())) {
                    return Some(Op::Send(vec![item.clone()]));
                }
            }
        }
    }
    None
}

// ---- O laço ----

/// Resultado de um passo.
enum Step {
    /// Uma chamada saiu: espera o intervalo do chat.
    Sent,
    /// Nada a fazer: espera o aviso da fila ou o próximo tique.
    Idle(Duration),
    /// Falha passageira: espera fixa.
    Wait(Duration),
}

struct Sender {
    key: String,
    client: TelegramClient,
}

/// A fila de saída, até o encerramento. Só roda no dono da trava (`nanty::start`).
pub async fn run(state: AppState, mut shutdown: watch::Receiver<bool>) {
    let nanty = Arc::clone(state.nanty());
    let mut sender: Option<Sender> = None;
    let mut retry: Option<Duration> = None;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let step = step(&state, &nanty, &mut sender, &mut retry).await;
        let outbox = nanty.telegram().outbox();
        match step {
            Step::Sent => {
                let gap = nanty.telegram().tuning().send_gap;
                tokio::select! {
                    () = crate::server::events::wait_shutdown(&mut shutdown) => break,
                    () = tokio::time::sleep(gap) => {}
                }
            }
            Step::Idle(dur) => tokio::select! {
                () = crate::server::events::wait_shutdown(&mut shutdown) => break,
                () = outbox.wake.notified() => {}
                () = tokio::time::sleep(dur) => {}
            },
            Step::Wait(dur) => tokio::select! {
                () = crate::server::events::wait_shutdown(&mut shutdown) => break,
                () = tokio::time::sleep(dur) => {}
            },
        }
    }
}

/// Espera crescente depois de uma falha de rede.
fn grow(retry: &mut Option<Duration>) -> Duration {
    let current = retry.unwrap_or(RETRY_BASE).min(RETRY_MAX);
    *retry = Some(current.saturating_mul(2).min(RETRY_MAX));
    current
}

/// O que fazer com o erro de uma chamada: `None` = recusa do Telegram (desistir desta chamada).
fn on_error(nanty: &Nanty, method: &str, e: &TgError, retry: &mut Option<Duration>) -> Option<Step> {
    match e {
        TgError::RetryAfter(secs) => Some(Step::Wait(Duration::from_secs(*secs))),
        TgError::Unauthorized => Some(Step::Wait(OFF_WAIT)),
        TgError::Network(_) | TgError::Conflict | TgError::Api(500..=599) => Some(Step::Wait(grow(retry))),
        TgError::Api(_) | TgError::BadResponse => {
            let mut p = param("method", method);
            p.insert("error".into(), json!(e.code()));
            journal(nanty, "telegram.send_failed", p);
            None
        }
    }
}

async fn step(state: &AppState, nanty: &Arc<Nanty>, sender: &mut Option<Sender>, retry: &mut Option<Duration>) -> Step {
    let outbox = nanty.telegram().outbox();
    let cfg: Config = state.config().read().await.clone();
    let tg = &cfg.nanty.telegram;
    let chat_id = tg.chat_id.filter(|_| tg.owner_id.is_some());
    let token = tg.resolve_token().filter(|t| valid_token(t));
    let (Some(chat_id), Some(token)) = (chat_id, token) else {
        // Sem dono: nada fica esperando para sair num pareamento futuro.
        outbox.clear_queue();
        *sender = None;
        return Step::Idle(OFF_WAIT);
    };
    if nanty.status() != InstanceStatus::Active || nanty.telegram().health() == Health::InvalidToken {
        return Step::Idle(OFF_WAIT);
    }
    let key = hash_hex(&token);
    if sender.as_ref().map(|s| s.key.as_str()) != Some(key.as_str()) {
        match TelegramClient::new(state.telegram_api(), token, Duration::ZERO) {
            Ok(client) => *sender = Some(Sender { key, client }),
            Err(e) => return on_error(nanty, "client", &e, retry).unwrap_or(Step::Idle(OFF_WAIT)),
        }
    }
    let Some(Sender { client, .. }) = sender.as_ref() else { return Step::Idle(OFF_WAIT) };
    let now_ms = nanty.now().epoch_ms;
    let hold_ms = if outbox.visible(now_ms) { tg.visible_hold_ms() } else { 0 };
    let settle_ms = u64::try_from(nanty.telegram().tuning().send_settle.as_millis()).unwrap_or(u64::MAX);
    let waits = Waits { card_ms: hold_ms.max(settle_ms), message_ms: hold_ms, settle_ms };
    let snapshot = nanty.snapshot();
    outbox.keep_speech_of(&snapshot);
    let (queue, strip, speech): (Vec<Queued>, Option<(i64, i64)>, SpeechMap) = {
        let inner = outbox.lock();
        (inner.queue.iter().cloned().collect(), inner.strips.front().copied(), inner.speech.clone())
    };
    let speech = cfg.nanty.voice.enabled.then_some(&speech);
    let mut forget = Vec::new();
    let op = next_op(&snapshot, &queue, strip, speech, chat_id, waits, now_ms, &mut forget);
    if !forget.is_empty() {
        nanty.update_quiet(|st| {
            let before = st.telegram_sent.len();
            st.telegram_sent.retain(|r| !forget.contains(&r.card_id));
            before != st.telegram_sent.len()
        });
    }
    let Some(op) = op else {
        // Spec 139 T03: sem nada a enviar, o "digitando" de uma resposta em andamento.
        let every = nanty.telegram().tuning().typing_every;
        if let Some(action) = outbox.action_due(every) {
            match client.send_chat_action(chat_id, action).await {
                Ok(()) => {
                    *retry = None;
                    return Step::Sent;
                }
                Err(e) => {
                    if let Some(step) = on_error(nanty, "sendChatAction", &e, retry) {
                        return step;
                    }
                }
            }
        }
        // Algo ainda espera a aba: confere de novo no próximo tique.
        return Step::Idle(if outbox.typing_active() { IDLE.min(every) } else { IDLE });
    };
    let lang = nanty_lang(&cfg.nanty, nanty);
    let secrets = SecretSet::from_config(&cfg);
    // O silêncio de tudo (spec 139 T04) cala o som das mensagens, como o horário de silêncio.
    let quiet = nanty.now().within(cfg.nanty.quiet_window()) || crate::server::nanty::silence::global_until(&snapshot, now_ms).is_some();
    let ctx = Ctx { state, nanty, client, cfg: &cfg, secrets: &secrets, lang, quiet, chat_id, now_ms, st: &snapshot };
    match execute(&ctx, op, retry).await {
        Ok(()) => {
            *retry = None;
            Step::Sent
        }
        Err(step) => step,
    }
}

/// O que uma chamada precisa saber.
struct Ctx<'a> {
    state: &'a AppState,
    nanty: &'a Nanty,
    client: &'a TelegramClient,
    cfg: &'a Config,
    secrets: &'a SecretSet,
    lang: Lang,
    /// Horário de silêncio agora: as mensagens vão sem som.
    quiet: bool,
    chat_id: i64,
    now_ms: u64,
    st: &'a NantyState,
}

/// Faz a chamada. `Err` = falha passageira, com a espera; recusa do Telegram conta como feita
/// (desiste e segue).
async fn execute(ctx: &Ctx<'_>, op: Op, retry: &mut Option<Duration>) -> Result<(), Step> {
    let Ctx { nanty, client, cfg, secrets, lang, quiet, chat_id, now_ms, st, .. } = *ctx;
    let outbox = nanty.telegram().outbox();
    let fail = |method: &str, e: &TgError, retry: &mut Option<Duration>| on_error(nanty, method, e, retry);
    match op {
        Op::Strip(chat, message) => {
            if let Err(e) = client.clear_buttons(chat, message).await {
                if let Some(step) = fail("editMessageReplyMarkup", &e, retry) {
                    return Err(step);
                }
            }
            outbox.lock().strips.retain(|s| *s != (chat, message));
        }
        Op::Finish(rec) => {
            let line = final_line(lang, st, &rec.card_id);
            if let Some(message_id) = rec.message_id {
                let (method, edited) = match rec.kind {
                    SentKind::Text => {
                        let text = clean(secrets, &format!("{}\n\n{line}", rec.text));
                        ("editMessageText", client.edit_text(rec.chat_id, message_id, &text).await)
                    }
                    SentKind::Voice => {
                        let caption = closed_caption(&rec.text, &clean(secrets, &line));
                        ("editMessageCaption", client.edit_caption(rec.chat_id, message_id, &caption).await)
                    }
                };
                if let Err(e) = edited {
                    if let Some(step) = fail(method, &e, retry) {
                        return Err(step);
                    }
                }
            }
            nanty.update_quiet(|s| {
                s.telegram_sent.retain(|r| r.card_id != rec.card_id);
                true
            });
        }
        Op::Clear(rec) => {
            if let Some(message_id) = rec.message_id {
                if let Err(e) = client.clear_buttons(rec.chat_id, message_id).await {
                    if let Some(step) = fail("editMessageReplyMarkup", &e, retry) {
                        return Err(step);
                    }
                }
            }
            nanty.update_quiet(|s| {
                if let Some(r) = s.telegram_sent.iter_mut().find(|r| r.card_id == rec.card_id) {
                    r.buttons = false;
                }
                true
            });
        }
        Op::Send(items) if items.len() == 1 => {
            let Some(item) = items.into_iter().next() else { return Ok(()) };
            match item {
                Item::Card(card) => {
                    let text = clean(secrets, &card_text(lang, cfg, &card));
                    let keys = keyboard(lang, &card);
                    let buttons = keys.is_some();
                    let silent = card.delivery == Some(Delivery::Silent);
                    let spoken = outbox.spoken(&card).filter(|_| cfg.nanty.voice.enabled);
                    let mut voiced = None;
                    if let Some(spoken) = &spoken {
                        let caption = format::fit(&text, format::MAX_CAPTION_CHARS);
                        match client.send_voice(chat_id, spoken.ogg.clone(), &caption, spoken.duration_secs, silent, keys.clone()).await {
                            Ok(id) => voiced = Some(id),
                            // Recusada, o cartão sai em texto, com os botões: ele nunca se perde.
                            Err(e) => {
                                if let Some(step) = fail("sendVoice", &e, retry) {
                                    return Err(step);
                                }
                            }
                        }
                    }
                    let kind = if voiced.is_some() { SentKind::Voice } else { SentKind::Text };
                    let message_id = match voiced {
                        Some(id) => id,
                        None => match client.send(chat_id, &text, silent, keys).await {
                            Ok(id) => id,
                            Err(e) => match fail("sendMessage", &e, retry) {
                                Some(step) => return Err(step),
                                None => None,
                            },
                        },
                    };
                    if let Some(spoken) = &spoken {
                        // Leitura e gravação curtas de arquivo local, como as do estado aqui.
                        voice::keep_notice(nanty, &card, lang, spoken);
                    }
                    outbox.forget_speech(&card.id);
                    if kind == SentKind::Voice && format::units(&text) > format::MAX_CAPTION_CHARS {
                        outbox.push_full_text(&text);
                    }
                    nanty.update_quiet(|s| {
                        s.put_sent(SentCard {
                            card_id: card.id.clone(),
                            chat_id,
                            message_id,
                            created_at_ms: card.created_at_ms,
                            buttons: buttons && message_id.is_some(),
                            text,
                            at_ms: now_ms,
                            kind,
                        });
                        true
                    });
                }
                Item::Message(q) => {
                    let text = clean(secrets, &render::message_text(lang, &q.message));
                    let tail = render::closing(lang, &q.message).map(|line| clean(secrets, &line));
                    if let Some(sent) = send_voice(ctx, &q, &text, tail.as_deref(), retry).await? {
                        let mut inner = outbox.lock();
                        if sent {
                            inner.queue.retain(|m| m.message.id != q.message.id);
                        } else if let Some(m) = inner.queue.iter_mut().find(|m| m.message.id == q.message.id) {
                            m.voice_sent = true;
                        }
                        return Ok(());
                    }
                    if !text.is_empty() {
                        if let Err(e) = client.send(chat_id, &text, quiet, None).await {
                            if let Some(step) = fail("sendMessage", &e, retry) {
                                return Err(step);
                            }
                        }
                    }
                    outbox.lock().queue.retain(|m| m.message.id != q.message.id);
                }
            }
        }
        Op::Send(items) => {
            let text = clean(secrets, &batch_text(lang, &items));
            let silent = items.iter().all(|i| match i {
                Item::Card(c) => c.delivery == Some(Delivery::Silent),
                Item::Message(_) => quiet,
            });
            if let Err(e) = client.send(chat_id, &text, silent, None).await {
                if let Some(step) = fail("sendMessage", &e, retry) {
                    return Err(step);
                }
            }
            let ids: Vec<String> = items
                .iter()
                .filter_map(|i| match i {
                    Item::Message(q) => Some(q.message.id.clone()),
                    Item::Card(_) => None,
                })
                .collect();
            outbox.lock().queue.retain(|m| !ids.contains(&m.message.id));
            nanty.update_quiet(|s| {
                for item in &items {
                    if let Item::Card(card) = item {
                        s.put_sent(SentCard {
                            card_id: card.id.clone(),
                            chat_id,
                            message_id: None,
                            created_at_ms: card.created_at_ms,
                            buttons: false,
                            text: String::new(),
                            at_ms: now_ms,
                            kind: SentKind::Text,
                        });
                    }
                }
                true
            });
            for item in &items {
                if let Item::Card(card) = item {
                    outbox.forget_speech(&card.id);
                }
            }
            journal(nanty, "telegram.batched", param("count", items.len()));
        }
        Op::Speak(card) => {
            start_speech(ctx, &card);
            // Não foi uma chamada ao Telegram: a fila segue sem o intervalo do chat.
            return Err(Step::Idle(Duration::ZERO));
        }
    }
    Ok(())
}

/// Gera a fala do cartão numa tarefa à parte, com o texto que vai na legenda (spec 139 T08, C-14).
/// O [`voice::speak_on_time`] sempre responde, então nada deixa o cartão esperando para sempre.
fn start_speech(ctx: &Ctx<'_>, card: &Card) {
    let outbox = ctx.nanty.telegram().outbox();
    outbox.speech_started(card);
    let text = clean(ctx.secrets, &card_text(ctx.lang, ctx.cfg, card));
    let (id, created_at_ms) = (card.id.clone(), card.created_at_ms);
    let state = ctx.state.clone();
    tokio::spawn(async move {
        let spoken = voice::speak_on_time(&state, state.nanty(), &text).await.ok();
        state.nanty().telegram().outbox().speech_done(&id, created_at_ms, spoken);
    });
}

/// A legenda da nota de voz depois da decisão: o texto, cortado para a linha final caber em 1024.
fn closed_caption(text: &str, line: &str) -> String {
    let room = format::MAX_CAPTION_CHARS.saturating_sub(format::units(line) + 2);
    format!("{}\n\n{line}", format::fit(text, room))
}

/// A nota de voz de uma mensagem com áudio (T09, C-12): a fala com a legenda curta (o mesmo texto
/// redigido, cortado em 1024, sem perder a frase do servidor que fecha o texto, `tail`). `None` = não há nota a mandar (sem áudio, já enviada, ou o arquivo
/// saiu): segue o texto. `Some(true)` = a legenda levou o texto inteiro, nada mais a mandar;
/// `Some(false)` = falta o texto completo, que vai na próxima vez (também quando o Telegram recusa a
/// nota: o aviso nunca se perde).
async fn send_voice(ctx: &Ctx<'_>, q: &Queued, text: &str, tail: Option<&str>, retry: &mut Option<Duration>) -> Result<Option<bool>, Step> {
    let Some(audio) = q.message.audio.as_ref().filter(|_| !q.voice_sent) else { return Ok(None) };
    let nanty = ctx.nanty;
    // Leitura curta de um arquivo local (como as gravações do estado que a fila já faz aqui).
    let Some(ogg) = nanty.read_audio(&audio.id) else { return Ok(None) };
    let caption = format::fit_keeping(text, tail, format::MAX_CAPTION_CHARS);
    let whole = caption == text;
    match ctx.client.send_voice(ctx.chat_id, ogg, &caption, audio.duration_secs, ctx.quiet, None).await {
        Ok(_) => Ok(Some(whole && !text.is_empty())),
        Err(e) => match on_error(nanty, "sendVoice", &e, retry) {
            Some(step) => Err(step),
            None => Ok(Some(false)),
        },
    }
}

/// O resumo da fila: quantos chegaram juntos e a primeira linha de cada um (até [`BATCH_LINES`]).
fn batch_text(lang: Lang, items: &[Item]) -> String {
    let count = u64::try_from(items.len()).unwrap_or(u64::MAX);
    let mut lines = vec![tr_plural(lang, "nanty.telegram.batch", count, &[])];
    for item in items.iter().take(BATCH_LINES) {
        let text = match item {
            Item::Card(card) => render::card_text(lang, card),
            Item::Message(q) => render::message_text(lang, &q.message),
        };
        let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
        lines.push(format!("- {}", format::fit(first, BATCH_LINE_CHARS)));
    }
    let rest = items.len().saturating_sub(BATCH_LINES);
    if rest > 0 {
        lines.push(tr_plural(lang, "nanty.telegram.batch_more", u64::try_from(rest).unwrap_or(u64::MAX), &[]));
    }
    lines.join("\n")
}

#[cfg(test)]
mod silence_tests {
    use super::*;

    fn datas(kb: &Value) -> Vec<String> {
        kb.as_array()
            .map(|rows| {
                rows.iter()
                    .flat_map(|r| r.as_array().cloned().unwrap_or_default())
                    .filter_map(|b| b["callback_data"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Spec 139 T04: o cartão do silêncio leva o botão da ação (e o "não", na confirmação), sem o
    /// "lembrar depois"; o texto é o do dicionário, com a hora calculada pelo servidor.
    #[test]
    fn the_silence_cards_carry_only_their_buttons_and_the_server_time() {
        let mut st = NantyState::default();
        let id = crate::server::nanty::silence::apply_in(&mut st, 0, 3_600_000, Channel::Telegram);
        let off = st.cards.iter().find(|c| c.id == id).cloned().expect("cartão");
        let token = off.token.clone().unwrap_or_default();
        let kb = keyboard(Lang::PtBr, &off).expect("botões");
        assert_eq!(datas(&kb), [callback_data(&off.id, &token, Answer::Yes)]);
        assert_eq!(kb[0][0]["text"], tr(Lang::PtBr, "nanty.action.end_silence", &[]));
        let until = off.params["until"].as_str().unwrap_or_default().to_string();
        assert_eq!(render::card_text(Lang::En, &off), tr(Lang::En, "nanty.card.end_silence", &[("until", &until)]));
        let ask = Card { action: Some("set_silence".into()), ..off };
        let kb = keyboard(Lang::En, &ask).expect("botões");
        assert_eq!(datas(&kb), [callback_data(&ask.id, &token, Answer::Yes), callback_data(&ask.id, &token, Answer::No)]);
        assert_eq!(kb[0][0]["text"], tr(Lang::En, "nanty.action.set_silence", &[]));
    }
}
