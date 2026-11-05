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
//!   - **A fala do modelo (spec 140 T02, C-29):** o aviso de um cartão novo espera um pouco o texto do
//!     turno proativo ([`NOTICE_TEXT_WAIT`], urgente [`NOTICE_TEXT_WAIT_URGENT`]), e a nota sai uma só:
//!     o áudio fala esse texto, e a legenda leva o texto e o porquê do observador. Há um limite total
//!     até a fala ficar pronta ([`NOTICE_TOTAL`]). Sem texto a tempo, com vários cartões, com recusa,
//!     sem orçamento ou com falha do modelo, vale o caminho de antes: o aviso fala a frase fixa, e o
//!     texto tardio segue como mensagem.
//!   - **Um áudio por aviso:** o texto do modelo que não entrou na nota segue em texto, e o cartão
//!     pedido na conversa também: a fala é a da resposta que o cita. A rajada continua em texto.
//!   - **Edição:** a nota de voz é editada pelo `editMessageCaption`, com a linha final dentro de 1024.
//!   - **Recusa:** o Telegram recusou a nota (4xx), o aviso sai pelo `sendMessage`, com os botões.
//! - **Cenário proativo (spec 140 T12, C-14):** a mensagem do cenário ([`crate::server::nanty::scenario`])
//!   nunca entra no resumo da fila: com a rajada, ela espera e sai depois, inteira, sem botões.
//! - **Link do Nants (spec 140 T03, C-30):** a mensagem com o caminho ([`link::url_of`]) leva um botão
//!   com a URL. Ela não entra no resumo da fila nem na legenda: o texto vai à parte da nota de voz. Se
//!   o Telegram recusar o botão (localhost ou IP privado podem ser recusados; só um bot real confirma),
//!   a mensagem sai de novo sem ele, com o endereço como texto. O endereço entra depois do `clean`, que
//!   neutralizaria o link, e só o servidor o escreve.
//! - **Resposta longa (spec 140 T14, C-27):** a conversa grava o texto inteiro, e a fila o divide em
//!   até três mensagens ([`format::split`]), que saem uma por chamada, na ordem, antes de qualquer
//!   outro envio e nunca no resumo da fila. Acima de três, a última termina com a marca do corte
//!   (spec 141 T10, C-10: sem frase fixa). O teclado (o link do Nants e os botões de apagar) vai na
//!   última parte, que é a anotada para a edição de depois.
//! - **Voz tudo ou nada (spec 141 T08, C-08):** a nota de uma mensagem fala o texto inteiro
//!   ([`voice::speak`]). A transcrição vai completa: na legenda, com o teclado, quando o texto cabe
//!   nela; senão a nota sai sem legenda e sem teclado, e o texto inteiro segue em mensagens que
//!   respondem a ela (`reply_parameters`), em partes, com o teclado na última.
//! - **Erros:** nenhum derruba o Nanty. 429 espera o `retry_after`; rede e 5xx esperam cada vez mais;
//!   uma recusa do Telegram (4xx) desiste daquele envio e fica no registro.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
use crate::server::nanty::link;
use crate::server::nanty::memory;
use crate::server::nanty::render;
use crate::server::nanty::turn;
use crate::server::nanty::store::{Card, Channel, ConversationMessage, DecidedBy, Delivery, NantyState, SentCard, SentKind, Speaker};
use crate::server::nanty::voice::{self, Spoken, Tone};
use crate::server::nanty::{nanty_lang, new_id, InstanceStatus, Nanty};
use crate::server::state::AppState;

/// Intervalo mínimo entre duas chamadas ao chat (C-10).
pub const SEND_GAP: Duration = Duration::from_secs(1);
/// Renovação do "digitando" (spec 139 T03, C-09): o Telegram o mostra por até 5 segundos.
pub const TYPING_EVERY: Duration = Duration::from_secs(4);
/// Prazo de cada trecho da fala (spec 139 T08, C-14; por trecho desde a spec 141 T08; provisório,
/// abaixo do prazo da chamada ao provedor): vencido, a fala para, sem pedir os trechos restantes, e a
/// saída vai em texto.
pub const SPEECH_DEADLINE: Duration = Duration::from_secs(45);
/// Espera pelo texto do turno antes de falar o aviso (spec 140 T02, C-29; provisório): vencida, ele
/// fala a frase fixa.
pub const NOTICE_TEXT_WAIT: Duration = Duration::from_secs(15);
/// A mesma espera para o aviso urgente (provisório).
pub const NOTICE_TEXT_WAIT_URGENT: Duration = Duration::from_secs(8);
/// Limite total do aviso em voz, da espera ao fim da fala (provisório): a fala só tem o que sobrou.
pub const NOTICE_TOTAL: Duration = Duration::from_secs(60);
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
    /// A nota levou o texto inteiro: falta só o link do Nants (spec 140 T03).
    link_only: bool,
    /// Os botões de apagar da memória já foram (na nota de voz): o texto que falta sai sem eles.
    buttons_sent: bool,
    /// As partes que faltam da resposta longa (spec 140 T14), na ordem. A última leva o teclado.
    parts: Vec<String>,
    /// A nota de voz sem legenda a que o texto responde (spec 141 T08): o `message_id` dela.
    reply_to: Option<i64>,
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
    /// Cartões sozinhos num turno proativo em andamento (spec 140 T02): o texto deles ainda pode vir.
    solo_turns: BTreeSet<String>,
}

/// A fala de um cartão, gerada fora da fila (spec 139 T08, C-14).
#[derive(Debug, Clone)]
enum Speech {
    /// A tarefa à parte espera o texto do turno (spec 140 T02).
    Waiting,
    /// A tarefa à parte está gerando.
    Pending,
    Ready(Arc<Notice>),
    /// Sem fala (prazo, orçamento ou falha): o cartão sai em texto.
    Text,
}

/// A fala pronta de um aviso.
#[derive(Debug)]
struct Notice {
    spoken: Spoken,
    /// O texto do modelo que o áudio fala (spec 140 T02). Sem ele, o áudio fala a frase fixa.
    said: Option<Said>,
}

/// O texto do turno que virou a fala de um aviso.
#[derive(Debug)]
struct Said {
    /// A mensagem do turno, já gravada na conversa e ainda na fila.
    message: ConversationMessage,
    /// O texto dela, já redigido.
    text: String,
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

/// O turno proativo de um cartão só (spec 140 T02): enquanto existir, o aviso pode esperar o texto.
/// Solto em qualquer saída, o que acorda quem espera.
#[derive(Debug)]
pub struct SoloTurn<'a> {
    outbox: &'a Outbox,
    card_id: String,
}

impl Drop for SoloTurn<'_> {
    fn drop(&mut self) {
        self.outbox.lock().solo_turns.remove(&self.card_id);
        self.outbox.nudge();
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
    /// Acorda a espera pelo texto do turno.
    news: Notify,
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

    /// Acorda a espera pelo texto do turno (algo mudou para ela).
    pub fn nudge(&self) {
        self.news.notify_waiters();
    }

    /// Fala do Nanty: entra na fila. A da pessoa não volta para ela, e a resposta a quem escreveu na
    /// janela fica só na janela (T08).
    pub fn push_message(&self, message: &ConversationMessage, now_ms: u64) {
        if message.from != Speaker::Nanty || (message.reply_to.is_some() && message.channel == Channel::Web) {
            return;
        }
        {
            let mut inner = self.lock();
            inner.queue.push_back(Queued { message: message.clone(), at_ms: now_ms, voice_sent: false, link_only: false, buttons_sent: false, parts: Vec::new(), reply_to: None });
            while inner.queue.len() > MAX_QUEUED {
                inner.queue.pop_front();
            }
        }
        self.nudge();
        self.wake();
    }

    /// Marca o turno proativo de um cartão só. Quem chama marca junto com o `turned_until_ms`.
    pub fn solo_turn(&self, card_id: &str) -> SoloTurn<'_> {
        self.lock().solo_turns.insert(card_id.to_string());
        SoloTurn { outbox: self, card_id: card_id.to_string() }
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

    /// A tarefa da fala do cartão começou, esperando o texto do turno.
    fn speech_started(&self, card: &Card) {
        self.lock().speech.insert(card.id.clone(), CardSpeech { created_at_ms: card.created_at_ms, state: Speech::Waiting });
    }

    /// A espera acabou e a fala vai ao provedor. `false` = o cartão saiu ou foi renovado: nada a falar.
    fn speech_speaking(&self, card: &Card) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.speech.get_mut(&card.id).filter(|e| e.created_at_ms == card.created_at_ms) else { return false };
        entry.state = Speech::Pending;
        true
    }

    /// Outro cartão quer falar: quem espera o texto deixa de esperar. Só muda quem ainda espera: a
    /// decisão vem de uma cópia do mapa, e a fala pode ter acabado nesse meio (sem tarefa para concluir
    /// um `Pending`, o cartão ficaria preso).
    fn hurry(&self, card_id: &str) {
        if let Some(entry) = self.lock().speech.get_mut(card_id).filter(|e| matches!(e.state, Speech::Waiting)) {
            entry.state = Speech::Pending;
        }
        self.nudge();
    }

    /// O que já se sabe do texto do turno sobre o cartão: a mensagem, o motivo de falar sem ela, ou
    /// `None`, que ainda vale esperar. `picked`: um turno já pegou o cartão.
    fn turn_text(&self, card: &Card, picked: bool) -> Option<Result<ConversationMessage, &'static str>> {
        let inner = self.lock();
        let Some(entry) = inner.speech.get(&card.id).filter(|e| e.created_at_ms == card.created_at_ms) else { return Some(Err(GONE)) };
        if let Some(q) = inner.queue.iter().find(|q| about_only(&q.message, &card.id)) {
            return Some(Ok(q.message.clone()));
        }
        if !matches!(entry.state, Speech::Waiting) {
            return Some(Err("several"));
        }
        (picked && !inner.solo_turns.contains(&card.id)).then_some(Err("no_text"))
    }

    /// A fala terminou (`None` = fica o texto). Sem efeito se o cartão saiu ou foi renovado no meio.
    fn speech_done(&self, card_id: &str, created_at_ms: u64, spoken: Option<Notice>) {
        {
            let mut inner = self.lock();
            let Some(entry) = inner.speech.get_mut(card_id).filter(|e| e.created_at_ms == created_at_ms) else { return };
            entry.state = spoken.map_or(Speech::Text, |s| Speech::Ready(Arc::new(s)));
        }
        self.wake();
    }

    /// A fala pronta do cartão.
    fn notice(&self, card: &Card) -> Option<Arc<Notice>> {
        match self.lock().speech.get(&card.id) {
            Some(CardSpeech { created_at_ms, state: Speech::Ready(notice) }) if *created_at_ms == card.created_at_ms => Some(Arc::clone(notice)),
            _ => None,
        }
    }

    /// Esquece a fala do que já saiu.
    fn forget_speech(&self, card_id: &str) {
        self.lock().speech.remove(card_id);
        self.nudge();
    }

    /// Esquece a fala dos cartões que não vão mais sair: fora do estado, renovados (decididos no meio
    /// dela) ou vencidos. O que espera por eles segue.
    fn keep_speech_of(&self, st: &NantyState, now_ms: u64) {
        let mut inner = self.lock();
        let before = inner.speech.len();
        inner.speech.retain(|id, e| st.cards.iter().any(|c| c.id == *id && c.created_at_ms == e.created_at_ms && sendable(c, now_ms)));
        let changed = inner.speech.len() != before;
        drop(inner);
        if changed {
            self.nudge();
        }
    }

    /// O texto completo do aviso cuja legenda cortou: é a próxima mensagem, sem espera e sem voz.
    fn push_full_text(&self, text: &str) {
        let mut message = ConversationMessage::from_nanty(new_id(), 0, Channel::Telegram, "", Map::new());
        message.code = None;
        message.text = Some(text.to_string());
        self.lock().queue.push_front(Queued { message, at_ms: 0, voice_sent: true, link_only: false, buttons_sent: true, parts: Vec::new(), reply_to: None });
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
        drop(inner);
        self.nudge();
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
pub(crate) fn keyboard(lang: Lang, card: &Card) -> Option<Value> {
    let token = card.token.as_deref()?;
    let mut rows: Vec<Value> = Vec::new();
    let mut last = Vec::new();
    // A confirmação da memória (spec 140 T07), com o "não" e sem "lembrar depois". O botão de apagar
    // nunca sai como cartão: vai no teclado da resposta ([`forget_rows`]).
    match card.action.as_deref() {
        Some(memory::card::ACTION_CONFIRM) => {
            return Some(json!([
                [button(tr(lang, "nanty.action.memory_confirm", &[]), card, token, Answer::Yes)],
                [button(tr(lang, "nanty.telegram.button_no", &[]), card, token, Answer::No)],
            ]));
        }
        // Spec 140 T15: levar ao Arquiteto, com o "não" e sem "lembrar depois".
        Some(crate::server::nanty::draft::ACTION) => {
            return Some(json!([
                [button(tr(lang, "nanty.action.architect_draft", &[]), card, token, Answer::Yes)],
                [button(tr(lang, "nanty.telegram.button_no", &[]), card, token, Answer::No)],
            ]));
        }
        _ => {}
    }
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
    /// Outro cartão espera a vez de falar: o que espera o texto do turno fala já (id do cartão).
    Hurry(String),
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

/// A mensagem é o texto do turno proativo sobre este cartão, e só sobre ele.
fn about_only(message: &ConversationMessage, card_id: &str) -> bool {
    message.from == Speaker::Nanty
        && message.code.is_none()
        && message.reply_to.is_none()
        && message.audio.is_none()
        && matches!(message.card_ids.as_slice(), [only] if only == card_id)
        && message.text.as_deref().is_some_and(|t| !t.trim().is_empty())
}

/// O cartão fala: com a voz ligada, menos o pedido na conversa (a fala é a da resposta que o cita).
fn ahead(speech: Option<&SpeechMap>, card: &Card) -> Ahead {
    let Some(speech) = speech.filter(|_| card.requested.is_none()) else { return Ahead::Go };
    match speech.get(&card.id).filter(|e| e.created_at_ms == card.created_at_ms) {
        None => Ahead::Start,
        Some(CardSpeech { state: Speech::Waiting | Speech::Pending, .. }) => Ahead::Pending,
        Some(_) => Ahead::Go,
    }
}

/// Pode ir ao Telegram: aberto, não antigo, não só do resumo e não pedido na janela (T08).
fn sendable(card: &Card, now_ms: u64) -> bool {
    card.expires_at_ms > now_ms
        // Spec 140 T07: o botão de apagar da memória vai na resposta que o cita, nunca à parte.
        && card.action.as_deref() != Some(memory::card::ACTION_FORGET)
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
    // Spec 140 T14: a resposta longa que já começou sai até o fim, parte a parte, antes de tudo: nada
    // entra no meio dela, e ela nunca vai ao resumo da fila.
    if let Some(q) = queue.iter().find(|q| !q.parts.is_empty()) {
        return Some(Op::Send(vec![Item::Message(q.clone())]));
    }
    let unsent = |c: &Card| !st.telegram_sent.iter().any(|r| r.card_id == c.id && r.chat_id == chat_id && r.created_at_ms >= c.created_at_ms);
    let mut items: Vec<Item> = st
        .cards
        .iter()
        .filter(|c| sendable(c, now_ms) && released(c.created_at_ms, waits.card(c), now_ms))
        .filter(|c| unsent(c))
        .cloned()
        .map(Item::Card)
        .collect();
    // Spec 140 T12 (C-14): o cenário proativo fica fora da rajada: na linha de 160 caracteres do resumo
    // da fila ele perderia o sentido. Ele espera a rajada sair e vai depois, inteiro.
    let scenario = |q: &&Queued| q.message.code.as_deref() == Some(crate::server::nanty::scenario::LEAD);
    let ready = |q: &&Queued| released(q.at_ms, waits.message(&q.message), now_ms);
    items.extend(queue.iter().filter(ready).filter(|q| !scenario(q)).cloned().map(Item::Message));
    // A rajada se decide só com o que não é cenário, e o cenário nunca entra no resumo da fila.
    if items.len() > MAX_BURST {
        // Spec 140 T03: o resumo da fila não leva botão, e o link pedido não pode sumir nele.
        // Spec 140 T07: nem o botão de apagar da memória.
        let link = items
            .iter()
            .find(|i| matches!(i, Item::Message(q) if link::url_of(&q.message).is_some() || !memory_buttons(st, &q.message, now_ms).is_empty()))
            .cloned();
        return Some(Op::Send(link.map_or(items, |item| vec![item])));
    }
    // Sem rajada, a fila segue item a item, e os cenários entram por último.
    items.extend(queue.iter().filter(ready).filter(scenario).cloned().map(Item::Message));
    // Uma fala de cada vez. O cartão que espera a dele não segura o resto da fila, só o que o cita. O
    // que cita um cartão com fala em andamento ou pronta espera por ela: pode virar a fala dele. O
    // texto do turno sobre um cartão só também espera o cartão na acomodação (ou na aba visível), que
    // ainda nem começou a falar. Com a voz ligada, e a espera é finita: o cartão é liberado ou sai.
    let speaking = speech.is_some_and(|s| s.values().any(|e| matches!(e.state, Speech::Waiting | Speech::Pending)));
    let waiting_text = speech.and_then(|s| s.iter().find(|(_, e)| matches!(e.state, Speech::Waiting))).map(|(id, _)| id);
    let speaks = |id: &str| speech.is_some_and(|s| s.contains_key(id));
    let settling = |m: &ConversationMessage| {
        speech.is_some()
            && st.cards.iter().any(|c| {
                about_only(m, &c.id) && c.requested.is_none() && sendable(c, now_ms) && !released(c.created_at_ms, waits.card(c), now_ms) && unsent(c)
            })
    };
    let mut waiting: Vec<&str> = Vec::new();
    for item in &items {
        match item {
            Item::Card(card) => match ahead(speech, card) {
                Ahead::Go => return Some(Op::Send(vec![item.clone()])),
                Ahead::Start if !speaking => return Some(Op::Speak(card.clone())),
                // Vários cartões: a espera pelo texto não segura um aviso novo (C-29).
                Ahead::Start => match waiting_text {
                    Some(id) => return Some(Op::Hurry(id.clone())),
                    None => waiting.push(&card.id),
                },
                Ahead::Pending => waiting.push(&card.id),
            },
            Item::Message(q) => {
                if !q.message.card_ids.iter().any(|id| waiting.contains(&id.as_str()) || speaks(id)) && !settling(&q.message) {
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
    outbox.keep_speech_of(&snapshot, now_ms);
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
        // Spec 140 T07: o botão de apagar estava na resposta; só ele some, e o texto fica.
        Op::Finish(rec) if rec.embedded => {
            if let Some(message_id) = rec.message_id {
                let rest: Vec<Card> = st
                    .telegram_sent
                    .iter()
                    .filter(|r| r.embedded && r.chat_id == rec.chat_id && r.message_id == rec.message_id && r.card_id != rec.card_id)
                    .filter_map(|r| st.cards.iter().find(|c| c.id == r.card_id))
                    .cloned()
                    .collect();
                // Os outros botões da mensagem ficam: o link do Nants (revisão 1, F-02) e os de apagar
                // que ainda valem.
                let mut rows: Vec<Value> = rec.url.as_deref().map(|url| link_row(lang, url)).into_iter().collect();
                rows.extend(forget_rows(lang, &rest));
                let (method, edited) = if rows.is_empty() {
                    ("editMessageReplyMarkup", client.clear_buttons(rec.chat_id, message_id).await)
                } else {
                    ("editMessageReplyMarkup", client.set_buttons(rec.chat_id, message_id, Value::Array(rows)).await)
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
        Op::Finish(rec) => {
            let line = final_line(lang, st, &rec.card_id);
            if let Some(message_id) = rec.message_id {
                let (method, edited) = match rec.kind {
                    SentKind::Text => {
                        let text = clean(secrets, &format!("{}\n\n{line}", rec.text));
                        ("editMessageText", client.edit_text(rec.chat_id, message_id, &text).await)
                    }
                    SentKind::Voice => {
                        let caption = closed_caption(&rec.text, rec.tail.as_deref(), &clean(secrets, &line));
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
                    // Spec 140 T07 (revisão 2, F-04): o texto que a pessoa aprova nunca chega cortado
                    // junto do "sim"; o que não cabe sai sem ele, apontando a janela.
                    let whole = format::clean_whole(secrets, &card_text(lang, cfg, &card));
                    let offered = keyboard(lang, &card);
                    // O cartão que perdeu o "sim" e aponta a janela (F-04 da T07) fica como o `fit_card` o deixou.
                    let to_window = offered.as_ref().is_some_and(has_yes) && format::units(&whole) > format::MAX_MESSAGE_CHARS;
                    let (fixed, keys) = fit_card(nanty, lang, &card, whole.clone(), offered);
                    let whole = if to_window { fixed.clone() } else { whole };
                    let buttons = keys.is_some();
                    let silent = card.delivery == Some(Delivery::Silent);
                    // Nem nota de voz com o "sim" e a legenda cortada em 1024: o cartão da memória nunca
                    // fala, e o que tem o "sim" e passa da legenda sai em texto.
                    let long_yes = keys.as_ref().is_some_and(has_yes) && format::units(&fixed) > format::MAX_CAPTION_CHARS;
                    let notice = outbox.notice(&card).filter(|_| cfg.nanty.voice.enabled && !memory::card::is_memory_card(&card) && !crate::server::nanty::draft::is_draft_card(&card) && !long_yes);
                    let said = notice.as_ref().and_then(|n| n.said.as_ref());
                    // Com a fala do modelo, a nota leva o texto dele e fecha com o porquê do observador.
                    // Spec 140 T14 (revisão 1): a nota e o texto completo à parte usam o texto inteiro do
                    // cartão; a nota só sai sem o "sim" cortado (`long_yes`), e o que passa de uma
                    // mensagem vai em partes pelo [`format::split`].
                    let note = said.map_or_else(|| whole.clone(), |s| format!("{}\n{whole}", s.text));
                    let tail = said.map(|_| whole.as_str());
                    let mut voiced = None;
                    if let Some(notice) = &notice {
                        let caption = format::fit_keeping(&note, tail, format::MAX_CAPTION_CHARS);
                        let spoken = &notice.spoken;
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
                        None => match client.send(chat_id, &fixed, silent, keys).await {
                            Ok(id) => id,
                            Err(e) => match fail("sendMessage", &e, retry) {
                                Some(step) => return Err(step),
                                None => None,
                            },
                        },
                    };
                    if let Some(notice) = &notice {
                        // Leitura e gravação curtas de arquivo local, como as do estado aqui.
                        voice::keep_notice(nanty, &card, lang, &notice.spoken, said.map(|s| &s.message));
                    }
                    if let Some(said) = said.filter(|_| kind == SentKind::Voice) {
                        // A nota levou o texto: ele não sai de novo. Recusada, ele segue na fila.
                        outbox.lock().queue.retain(|m| m.message.id != said.message.id);
                    }
                    outbox.forget_speech(&card.id);
                    if kind == SentKind::Voice && format::units(&note) > format::MAX_CAPTION_CHARS {
                        outbox.push_full_text(&note);
                    }
                    let (text, tail) = match kind {
                        SentKind::Voice => (note.clone(), tail.map(str::to_string)),
                        SentKind::Text => (fixed.clone(), None),
                    };
                    nanty.update_quiet(|s| {
                        s.put_sent(SentCard {
                            card_id: card.id.clone(),
                            chat_id,
                            message_id,
                            created_at_ms: card.created_at_ms,
                            buttons: buttons && message_id.is_some(),
                            text,
                            tail,
                            at_ms: now_ms,
                            kind,
                            embedded: false,
                            url: None,
                        });
                        if to_window {
                            s.mark_window_only(&card.id);
                        }
                        true
                    });
                }
                Item::Message(q) => {
                    let url = link::url_of(&q.message);
                    // Spec 140 T07: os botões de apagar da memória vão no teclado da própria resposta.
                    let own = crate::server::nanty::language::tagged(q.message.lang.as_deref()).unwrap_or(lang);
                    let held = if q.buttons_sent { Vec::new() } else { memory_buttons(st, &q.message, now_ms) };
                    let rows = forget_rows(own, &held);
                    // Spec 140 T14: a resposta longa já começou: sai a próxima parte.
                    if !q.parts.is_empty() {
                        return send_part(ctx, &q, own, url, &held, &rows, retry).await;
                    }
                    // Spec 140 T14 (C-27): a limpeza sem o corte; quem divide é o [`format::split`].
                    let text = format::clean_whole(secrets, &render::message_text(lang, &q.message));
                    match send_voice(ctx, &q, &text, &rows, retry).await? {
                        Voiced::Sent { whole, keyed, note, buttons_sent } => {
                            record_buttons(ctx, &held, keyed, SentKind::Voice, &text, None, None);
                            let mut inner = outbox.lock();
                            if whole && url.is_none() {
                                inner.queue.retain(|m| m.message.id != q.message.id);
                            } else if let Some(m) = inner.queue.iter_mut().find(|m| m.message.id == q.message.id) {
                                m.voice_sent = true;
                                m.link_only = whole;
                                m.buttons_sent = buttons_sent;
                                // Spec 141 T08: o texto inteiro que segue a nota sem legenda responde a ela.
                                m.reply_to = note.filter(|_| !whole);
                            }
                            return Ok(());
                        }
                        Voiced::Refused => {
                            if let Some(m) = outbox.lock().queue.iter_mut().find(|m| m.message.id == q.message.id) {
                                m.voice_sent = true;
                            }
                            return Ok(());
                        }
                        Voiced::NoVoice => {}
                    }
                    // A legenda já levou o texto: o botão do link vai numa linha curta de dado (spec
                    // 141 T10, C-10: a frase que fechava o texto do modelo saiu).
                    let carrier = tr(own, "nanty.chat.open_link", &[]);
                    let body = if q.link_only && url.is_some() { carrier.as_str() } else { text.as_str() };
                    // Spec 140 T14 (C-27): o texto longo vira partes, que saem uma por vez, na ordem.
                    let parts = format::split(body, format::MAX_MESSAGE_CHARS, keyboard_room(url, &rows), format::MAX_PARTS);
                    if parts.len() > 1 {
                        if let Some(m) = outbox.lock().queue.iter_mut().find(|m| m.message.id == q.message.id) {
                            m.parts = parts;
                        }
                        // Nada foi ao Telegram: a primeira parte sai já, no passo seguinte.
                        return Err(Step::Idle(Duration::ZERO));
                    }
                    let keyed = if let Some(url) = url {
                        send_link(ctx, &q.message, body, url, &rows, q.reply_to, retry).await?
                    } else if !text.is_empty() {
                        send_keyed(ctx, own, &text, &rows, q.reply_to, retry).await?
                    } else {
                        None
                    };
                    record_buttons(ctx, &held, keyed, SentKind::Text, &text, None, url);
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
                            tail: None,
                            at_ms: now_ms,
                            kind: SentKind::Text,
                            embedded: false,
                            url: None,
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
        Op::Hurry(card_id) => {
            outbox.hurry(&card_id);
            return Err(Step::Idle(Duration::ZERO));
        }
    }
    Ok(())
}

/// Motivo de uma espera sem fala: o cartão saiu ou foi renovado.
const GONE: &str = "gone";
/// Motivo de falar sem esperar: não há modelo ou orçamento para o turno.
const NO_TURN: &str = "no_turn";

/// Quanto o aviso espera o texto do turno: menos no urgente, e nunca além do limite total.
pub(super) fn text_wait(tuning: &super::Tuning, card: &Card) -> Duration {
    let wait = if card.urgency == Some(Urgency::Urgent) { tuning.notice_text_wait_urgent } else { tuning.notice_text_wait };
    wait.min(tuning.notice_total)
}

/// O cartão ainda vai sair: segue no estado, sem renovação, e enviável.
fn still_out(nanty: &Nanty, card: &Card) -> bool {
    let now_ms = nanty.now().epoch_ms;
    nanty.snapshot().cards.iter().any(|c| c.id == card.id && c.created_at_ms == card.created_at_ms && sendable(c, now_ms))
}

/// Espera o texto do turno sobre o cartão até `limit`. Sem nada a esperar, responde na hora: o texto
/// já está na fila, o turno acabou sem ele, ou o cartão saiu. A espera nunca chama o modelo. O cartão
/// é conferido no estado, e não só no mapa de fala, que a fila limpa no passo seguinte: decidido na
/// janela, ele não custa fala nem quando o texto chega nesse meio.
async fn wait_text(nanty: &Nanty, card: &Card, limit: Duration) -> Result<ConversationMessage, &'static str> {
    let outbox = nanty.telegram().outbox();
    let end = tokio::time::Instant::now() + limit;
    loop {
        let news = outbox.news.notified();
        tokio::pin!(news);
        // Inscrita antes de olhar: o que mudar depois da olhada ainda acorda esta espera.
        news.as_mut().enable();
        if !still_out(nanty, card) {
            return Err(GONE);
        }
        let picked = card.created_at_ms <= nanty.snapshot().turned_until_ms;
        if let Some(answer) = outbox.turn_text(card, picked) {
            return answer;
        }
        if tokio::time::timeout_at(end, news).await.is_err() {
            return Err(if still_out(nanty, card) { "deadline" } else { GONE });
        }
    }
}

/// Gera a fala do cartão numa tarefa à parte (spec 139 T08, C-14), no tom do tipo do sinal (spec 140
/// T01). Antes, espera um pouco o texto do turno (spec 140 T02, C-29): com ele, o áudio fala o texto
/// do modelo; sem ele, a frase da legenda. A fala fica com o que sobrou do limite total.
/// A espera tem prazo e o [`voice::speak_within`] sempre responde, então nada deixa o cartão esperando
/// para sempre.
fn start_speech(ctx: &Ctx<'_>, card: &Card) {
    let outbox = ctx.nanty.telegram().outbox();
    outbox.speech_started(card);
    let fixed = clean(ctx.secrets, &card_text(ctx.lang, ctx.cfg, card));
    let card_whole = format::clean_whole(ctx.secrets, &card_text(ctx.lang, ctx.cfg, card));
    let can_wait = turn::can_run(ctx.cfg, ctx.nanty);
    let tuning = ctx.nanty.telegram().tuning();
    let (card, state, secrets, lang) = (card.clone(), ctx.state.clone(), ctx.secrets.clone(), ctx.lang);
    tokio::spawn(async move {
        let nanty = state.nanty();
        let outbox = nanty.telegram().outbox();
        let started = tokio::time::Instant::now();
        let waited = if can_wait { wait_text(nanty, &card, text_wait(&tuning, &card)).await } else { Err(NO_TURN) };
        let said = waited.and_then(|message| {
            // Spec 140 T14 (revisão 1): sem o corte de 4096; o texto completo à parte passa pela divisão.
            let text = format::clean_whole(&secrets, &render::message_text(lang, &message));
            if text.is_empty() {
                return Err("no_text");
            }
            // Spec 141 T08 (revisão 2, F-02): a nota fala o texto do modelo só se o texto completo
            // à parte (ele e o do cartão) aparece inteiro; senão, fala a frase fixa, e o texto do
            // modelo segue como mensagem.
            let note = format!("{text}\n{card_whole}");
            if !voice::speakable(&note) {
                return Err("too_long");
            }
            Ok(Said { message, text })
        });
        // O que for decidido na janela durante a espera não custa fala.
        if matches!(said, Err(GONE)) || !outbox.speech_speaking(&card) {
            outbox.wake();
            return;
        }
        let waited = started.elapsed();
        match &said {
            Ok(_) => voice::record_notice(nanty, None, waited),
            Err(reason) if *reason != NO_TURN => voice::record_notice(nanty, Some(reason), waited),
            Err(_) => {}
        }
        let text = said.as_ref().map_or(fixed.as_str(), |s| s.text.as_str());
        let left = tuning.notice_total.saturating_sub(waited);
        let spoken = voice::speak_within(&state, nanty, text, Tone::of_card(&card), left).await.ok();
        outbox.speech_done(&card.id, card.created_at_ms, spoken.map(|spoken| Notice { spoken, said: said.ok() }));
    });
}

/// A legenda da nota de voz depois da decisão: o texto, cortado para a linha final caber em 1024, sem
/// perder o porquê que fecha o texto (`tail`, spec 140 T02).
pub(super) fn closed_caption(text: &str, tail: Option<&str>, line: &str) -> String {
    let room = format::MAX_CAPTION_CHARS.saturating_sub(format::units(line) + 2);
    format!("{}\n\n{line}", format::fit_keeping(text, tail, room))
}

/// O que a nota de voz de uma mensagem fez.
enum Voiced {
    /// Não há nota a mandar (sem áudio, já enviada, ou o arquivo saiu): segue o texto.
    NoVoice,
    /// A nota saiu. `whole`: a legenda levou o texto inteiro, nada mais a mandar. `keyed`: o
    /// `message_id` da nota que levou os botões. `note`: o `message_id` da nota, a que o texto
    /// inteiro responde quando não coube na legenda. `buttons_sent`: os botões já foram com ela (ou
    /// foram recusados nela); senão, vão com o texto.
    Sent { whole: bool, keyed: Option<i64>, note: Option<i64>, buttons_sent: bool },
    /// O Telegram recusou a nota: o texto completo vai na próxima vez (o aviso nunca se perde).
    Refused,
}

/// A nota de voz de uma mensagem com áudio (T09, C-12; tudo ou nada desde a spec 141 T08, C-08): a
/// fala do texto inteiro. Quando o texto (redigido) cabe na legenda, ele vai
/// nela, com os botões de apagar da memória (spec 140 T07); recusado o teclado, a nota sai de novo
/// sem ele, com a linha que aponta a janela. Quando não cabe, a nota sai sem legenda e sem teclado, e
/// o texto inteiro segue em mensagens que respondem a ela, com os botões na última.
async fn send_voice(ctx: &Ctx<'_>, q: &Queued, text: &str, rows: &[Value], retry: &mut Option<Duration>) -> Result<Voiced, Step> {
    let Some(audio) = q.message.audio.as_ref().filter(|_| !q.voice_sent) else { return Ok(Voiced::NoVoice) };
    let nanty = ctx.nanty;
    // Leitura curta de um arquivo local (como as gravações do estado que a fila já faz aqui).
    let Some(ogg) = nanty.read_audio(&audio.id) else { return Ok(Voiced::NoVoice) };
    let whole = !text.is_empty() && format::units(text) <= format::MAX_CAPTION_CHARS;
    let caption = if whole { text } else { "" };
    let keys = (whole && !rows.is_empty()).then(|| Value::Array(rows.to_vec()));
    let keyed = keys.is_some();
    let e = match ctx.client.send_voice(ctx.chat_id, ogg.clone(), caption, audio.duration_secs, ctx.quiet, keys).await {
        Ok(id) => return Ok(Voiced::Sent { whole, keyed: id.filter(|_| keyed), note: id, buttons_sent: whole }),
        Err(e) => e,
    };
    if let Some(step) = on_error(nanty, "sendVoice", &e, retry) {
        return Err(step);
    }
    if !keyed {
        return Ok(Voiced::Refused);
    }
    journal(nanty, "telegram.buttons_refused", param("error", e.code()));
    let line = forget_line(ctx, &q.message);
    let room = format::MAX_CAPTION_CHARS.saturating_sub(format::units(&line) + 1);
    let head = format::fit(text, room);
    let plain = format!("{head}\n{line}");
    match ctx.client.send_voice(ctx.chat_id, ogg, plain.trim_start(), audio.duration_secs, ctx.quiet, None).await {
        Ok(id) => Ok(Voiced::Sent { whole: head == text && !text.is_empty(), keyed: None, note: id, buttons_sent: true }),
        Err(e) => match on_error(nanty, "sendVoice", &e, retry) {
            Some(step) => Err(step),
            None => Ok(Voiced::Refused),
        },
    }
}

/// A folga da última mensagem (spec 140 T14, revisão 1, F-01): com teclado (o link ou os botões de
/// apagar), a recusa dele acrescenta linhas, e a mensagem tem de caber inteira com elas, sem perder o
/// fim do texto. Sem teclado, nada é acrescentado.
fn keyboard_room(url: Option<&str>, rows: &[Value]) -> usize {
    if url.is_some() || !rows.is_empty() {
        format::LAST_PART_ROOM
    } else {
        0
    }
}

/// A próxima parte da resposta longa (spec 140 T14, C-27). As do meio saem sem teclado; a última leva
/// o link do Nants e os botões de apagar, com a recusa do teclado de sempre, e é a mensagem que a
/// edição de depois mira (o `message_id` anotado). Uma parte recusada pelo Telegram fica para trás, e
/// a fila segue com a próxima.
async fn send_part(ctx: &Ctx<'_>, q: &Queued, lang: Lang, url: Option<&str>, held: &[Card], rows: &[Value], retry: &mut Option<Duration>) -> Result<(), Step> {
    let outbox = ctx.nanty.telegram().outbox();
    let Some(part) = q.parts.first() else { return Ok(()) };
    if q.parts.len() > 1 {
        if let Err(e) = ctx.client.send_replying(ctx.chat_id, part, ctx.quiet, None, q.reply_to).await {
            if let Some(step) = on_error(ctx.nanty, "sendMessage", &e, retry) {
                return Err(step);
            }
        }
        if let Some(m) = outbox.lock().queue.iter_mut().find(|m| m.message.id == q.message.id) {
            m.parts.remove(0);
        }
        return Ok(());
    }
    let keyed = match url {
        Some(url) => send_link(ctx, &q.message, part, url, rows, q.reply_to, retry).await?,
        None => send_keyed(ctx, lang, part, rows, q.reply_to, retry).await?,
    };
    record_buttons(ctx, held, keyed, SentKind::Text, part, None, url);
    outbox.lock().queue.retain(|m| m.message.id != q.message.id);
    Ok(())
}

/// A linha que aponta a aba Memória, no idioma da mensagem: o lugar do botão recusado.
fn forget_line(ctx: &Ctx<'_>, message: &ConversationMessage) -> String {
    let lang = crate::server::nanty::language::tagged(message.lang.as_deref()).unwrap_or(ctx.lang);
    tr(lang, "nanty.telegram.forget_in_window", &[])
}

/// O texto com os botões de apagar da memória (spec 140 T07). Sem botão, o envio de sempre. Recusado o
/// teclado, a mensagem sai de novo sem ele, com a linha que aponta a aba Memória; o motivo fica no
/// registro. Devolve o `message_id` da mensagem que levou os botões. `reply_to`: a nota de voz a que
/// o texto responde (spec 141 T08).
async fn send_keyed(ctx: &Ctx<'_>, lang: Lang, text: &str, rows: &[Value], reply_to: Option<i64>, retry: &mut Option<Duration>) -> Result<Option<i64>, Step> {
    let Ctx { nanty, client, chat_id, quiet, .. } = *ctx;
    let keys = (!rows.is_empty()).then(|| Value::Array(rows.to_vec()));
    let keyed = keys.is_some();
    let e = match client.send_replying(chat_id, text, quiet, keys, reply_to).await {
        Ok(id) => return Ok(id.filter(|_| keyed)),
        Err(e) => e,
    };
    if let Some(step) = on_error(nanty, "sendMessage", &e, retry) {
        return Err(step);
    }
    if !keyed {
        return Ok(None);
    }
    journal(nanty, "telegram.buttons_refused", param("error", e.code()));
    let line = tr(lang, "nanty.telegram.forget_in_window", &[]);
    let room = format::MAX_MESSAGE_CHARS.saturating_sub(format::units(&line) + 1);
    let plain = format!("{}\n{line}", format::fit(text, room));
    if let Err(e) = client.send_replying(chat_id, plain.trim_start(), quiet, None, reply_to).await {
        if let Some(step) = on_error(nanty, "sendMessage", &e, retry) {
            return Err(step);
        }
    }
    Ok(None)
}

/// O texto com o botão do link e, abaixo, os de apagar da memória (spec 140 T07). Recusado o teclado,
/// a mensagem sai de novo sem ele, com o endereço como texto (e a linha da aba Memória, se havia
/// botão dela); o motivo fica no registro, porque só um bot real diz o que o Telegram aceita.
/// `reply_to`: a nota de voz a que o texto responde (spec 141 T08).
#[allow(clippy::too_many_arguments)]
async fn send_link(ctx: &Ctx<'_>, message: &ConversationMessage, text: &str, url: &str, rows: &[Value], reply_to: Option<i64>, retry: &mut Option<Duration>) -> Result<Option<i64>, Step> {
    let Ctx { nanty, client, chat_id, quiet, lang, .. } = *ctx;
    let lang = crate::server::nanty::language::tagged(message.lang.as_deref()).unwrap_or(lang);
    let mut keys = vec![link_row(lang, url)];
    keys.extend(rows.iter().cloned());
    let e = match client.send_replying(chat_id, text, quiet, Some(Value::Array(keys)), reply_to).await {
        Ok(id) => return Ok(id.filter(|_| !rows.is_empty())),
        Err(e) => e,
    };
    if let Some(step) = on_error(nanty, "sendMessage", &e, retry) {
        return Err(step);
    }
    journal(nanty, "telegram.link_refused", param("error", e.code()));
    let mut line = tr(lang, "nanty.telegram.open_address", &[("url", url)]);
    if !rows.is_empty() {
        line = format!("{line}\n{}", tr(lang, "nanty.telegram.forget_in_window", &[]));
    }
    let room = format::MAX_MESSAGE_CHARS.saturating_sub(format::units(&line) + 1);
    let plain = format!("{}\n{line}", format::fit(text, room));
    if let Err(e) = client.send_replying(chat_id, plain.trim_start(), quiet, None, reply_to).await {
        if let Some(step) = on_error(nanty, "sendMessage", &e, retry) {
            return Err(step);
        }
    }
    Ok(None)
}

/// O cartão como vai ao Telegram (spec 140 T07, revisão 2, F-04): se o texto, já redigido, passa do
/// limite da mensagem, o "sim" sai do teclado (o "não" e o "lembrar depois" ficam, porque não gravam
/// nem executam nada) e uma linha do dicionário manda decidir na janela, onde o texto vai inteiro.
fn fit_card(nanty: &Nanty, lang: Lang, card: &Card, text: String, keys: Option<Value>) -> (String, Option<Value>) {
    if format::units(&text) <= format::MAX_MESSAGE_CHARS {
        return (text, keys);
    }
    // Sem o "sim" no teclado, o corte de sempre não esconde nada que a pessoa aprove por aqui.
    if !keys.as_ref().is_some_and(has_yes) {
        return (format::fit(&text, format::MAX_MESSAGE_CHARS), keys);
    }
    let rows: Vec<Value> = keys
        .as_ref()
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let kept: Vec<Value> = row.as_array().into_iter().flatten().filter(|b| !is_yes(b)).cloned().collect();
                    (!kept.is_empty()).then_some(Value::Array(kept))
                })
                .collect()
        })
        .unwrap_or_default();
    let line = tr(lang, "nanty.telegram.card_in_window", &[]);
    let room = format::MAX_MESSAGE_CHARS.saturating_sub(format::units(&line) + 1);
    let mut p = param("card", card.id.as_str());
    p.insert("action".into(), json!(card.action));
    journal(nanty, "telegram.card_too_long", p);
    (format!("{}\n{line}", format::fit(&text, room)), (!rows.is_empty()).then_some(Value::Array(rows)))
}

/// O botão do "sim" (o que executa, grava ou liga) num teclado de cartão.
fn is_yes(button: &Value) -> bool {
    button["callback_data"].as_str().and_then(super::parse_callback).is_some_and(|(_, _, a)| a == Answer::Yes)
}

/// O teclado tem o "sim".
fn has_yes(keys: &Value) -> bool {
    keys.as_array().into_iter().flatten().flat_map(|row| row.as_array().into_iter().flatten()).any(is_yes)
}

/// A linha do botão que leva ao Nants (spec 140 T03).
fn link_row(lang: Lang, url: &str) -> Value {
    json!([{ "text": tr(lang, "nanty.telegram.open_button", &[]), "url": url }])
}

/// Os botões de apagar da memória que a mensagem cita e que ainda valem (spec 140 T07).
fn memory_buttons(st: &NantyState, message: &ConversationMessage, now_ms: u64) -> Vec<Card> {
    message
        .card_ids
        .iter()
        .filter_map(|id| {
            st.cards.iter().find(|c| {
                &c.id == id && c.action.as_deref() == Some(memory::card::ACTION_FORGET) && c.expires_at_ms > now_ms && c.token.is_some()
            })
        })
        .cloned()
        .collect()
}

/// Teto do título da nota no botão (o Telegram corta o texto do botão).
const FORGET_LABEL_CHARS: usize = 40;

/// Uma linha de teclado por botão de apagar, com o título da nota: com mais de uma, cada botão diz qual.
pub(crate) fn forget_rows(lang: Lang, cards: &[Card]) -> Vec<Value> {
    cards
        .iter()
        .filter_map(|card| {
            let token = card.token.as_deref()?;
            let note = card.params.get("note").and_then(Value::as_str).unwrap_or_default();
            let label = tr(lang, "nanty.telegram.forget_button", &[("note", &format::fit(note, FORGET_LABEL_CHARS))]);
            Some(json!([button(label, card, token, Answer::Yes)]))
        })
        .collect()
}

/// Anota a mensagem que levou os botões: cada cartão aponta para ela, e ao sair só o botão dele some.
#[allow(clippy::too_many_arguments)]
fn record_buttons(ctx: &Ctx<'_>, held: &[Card], keyed: Option<i64>, kind: SentKind, text: &str, tail: Option<&str>, url: Option<&str>) {
    let Some(message_id) = keyed else { return };
    if held.is_empty() {
        return;
    }
    ctx.nanty.update_quiet(|s| {
        for card in held {
            s.put_sent(SentCard {
                card_id: card.id.clone(),
                chat_id: ctx.chat_id,
                message_id: Some(message_id),
                created_at_ms: card.created_at_ms,
                buttons: true,
                text: text.to_string(),
                tail: tail.map(str::to_string),
                at_ms: ctx.now_ms,
                kind,
                embedded: true,
                url: url.map(str::to_string),
            });
        }
        true
    });
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
mod speech_state_tests {
    use super::*;

    fn card() -> Card {
        Card { id: "c1".into(), created_at_ms: 7, ..Card::default() }
    }

    fn map(outbox: &Outbox) -> SpeechMap {
        outbox.lock().speech.clone()
    }

    /// Spec 140 T02: a decisão de apressar vem de uma cópia do mapa, e a fala pode acabar nesse meio.
    /// A fala concluída nunca volta para `Pending`: nenhuma tarefa existe para concluí-la.
    #[test]
    fn a_late_hurry_never_brings_a_finished_speech_back() {
        let (outbox, c) = (Outbox::default(), card());
        outbox.speech_started(&c);
        assert!(outbox.speech_speaking(&c));
        outbox.speech_done(&c.id, c.created_at_ms, None);
        assert_eq!(ahead(Some(&map(&outbox)), &c), Ahead::Go);
        outbox.hurry(&c.id);
        assert_eq!(ahead(Some(&map(&outbox)), &c), Ahead::Go, "sem fala: sai em texto");

        let spoken = Spoken { ogg: vec![1], duration_secs: 1 };
        outbox.speech_started(&c);
        outbox.speech_done(&c.id, c.created_at_ms, Some(Notice { spoken, said: None }));
        outbox.hurry(&c.id);
        assert!(outbox.notice(&c).is_some(), "a fala pronta continua pronta");
    }

    /// Quem ainda espera deixa de esperar.
    #[test]
    fn a_hurry_ends_only_the_wait() {
        let (outbox, c) = (Outbox::default(), card());
        outbox.speech_started(&c);
        assert_eq!(outbox.turn_text(&c, false), None, "ainda vale esperar");
        outbox.hurry(&c.id);
        assert_eq!(outbox.turn_text(&c, false), Some(Err("several")));
        assert_eq!(ahead(Some(&map(&outbox)), &c), Ahead::Pending, "a tarefa segue e conclui");
    }
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
