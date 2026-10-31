//! Canal do Nanty no Telegram (spec 138 T06 e T07): o cliente, a escuta por long polling, o
//! pareamento com um único dono e a saúde do canal (T06); os cartões, a edição, o silêncio, a
//! formatação segura e a fila de saída ([`outbox`], [`format`], T07). Os comandos e o texto livre
//! (T08) são respondidos pela conversa ([`super::chat`]); a lista de comandos é publicada por idioma
//! (`setMyCommands`) quando o bot responde.
//!
//! - **Só long polling, nunca webhook:** o Nants não abre porta nenhuma. A escuta pede
//!   `getUpdates` com espera de [`POLL_SECS`] e `allowed_updates = ["message","callback_query"]`.
//! - **Só o dono da trava escuta (C-05):** a escuta nasce em `nanty::start`, junto do laço, e só
//!   quando este processo tem a trava da pasta de dados. Dois processos na mesma pasta nunca
//!   disputam o bot.
//! - **Offset depois de processar (C-10):** cada lote é processado e só então o offset
//!   (`update_id` + 1) vai para a configuração. Na volta, o Telegram reentrega o que não foi
//!   confirmado, e a escuta pula o `update_id` já visto (deduplicação).
//! - **Erros:** 401 (ou 404, token inexistente) vira "token inválido" e a escuta para até o token
//!   mudar; 409 é "outro programa usa este bot" (o terminal orienta a criar um bot por instalação);
//!   429 espera o `retry_after`; erro de rede espera cada vez mais (de [`BACKOFF_BASE`] a
//!   [`BACKOFF_MAX`]). Nenhum erro do Telegram derruba o Nants: a escuta só muda a saúde e espera.
//! - **Pareamento (C-11):** o código de uso único da tela ([`pairing`]) chega pelo link
//!   `t.me/<bot>?start=<código>`; só vale em chat `private`, e fixa o `from.id` e o `chat.id` do
//!   dono. Num grupo, o Nants ignora a mensagem e sai do grupo. Mensagem ou clique de outro `from.id`
//!   é ignorado e registrado. Desparear ou trocar o token limpa o chat, o dono e o offset.
//! - **Nants fechado:** o Nanty dorme; o Telegram guarda as mensagens (até 24 horas, regra do
//!   Telegram). Na volta (e depois de trocar o token), a escuta primeiro esvazia a fila com
//!   `getUpdates` sem espera: tudo o que estava lá é do tempo em que ninguém escutava. A mensagem do
//!   dono entra na conversa e vira um cartão marcado como antigo; o clique num botão vira um cartão
//!   antigo e nunca decide nada. Nenhuma ação sai de uma mensagem acumulada. O pareamento e os
//!   estranhos seguem a regra de sempre (o código vive só na memória, então um `/start` velho não
//!   acerta).

pub mod api;
pub mod client;
pub mod format;
pub mod outbox;
pub mod pairing;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::sync::{watch, Notify};

use self::client::{CallbackQuery, Message, TelegramClient, TgError, Update};
use self::pairing::{PairAttempt, PairState, Pairing};
use super::act::{self, Answer};
use super::journal::{JournalEntry, JournalKind};
use super::store::{Card, Channel, DecidedBy, Delivery};
use super::{nanty_lang, Nanty, Part};
use crate::config::NantyTelegram;
use crate::i18n::{tr, Lang};
use crate::server::devices::hash_hex;
use crate::server::state::AppState;

/// Endereço da API do Telegram em produção.
pub const TELEGRAM_API: &str = "https://api.telegram.org";
/// Espera do `getUpdates` (long polling), em segundos.
pub const POLL_SECS: u64 = 25;
/// Primeira espera depois de um erro de rede ou de um conflito; dobra a cada falha.
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Teto da espera crescente.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Sem token (ou com um token inválido), a escuta reconfere a configuração neste intervalo (o
/// `env:NOME` pode passar a valer sem um aviso da tela).
const RECHECK: Duration = Duration::from_secs(60);
/// Validade do cartão de uma mensagem acumulada.
pub const STALE_CARD_TTL_MS: u64 = 12 * 60 * 60 * 1000;
/// Teto de cartões de mensagens acumuladas abertos ao mesmo tempo.
pub const MAX_STALE_CARDS: usize = 20;
/// Prefixo do grupo dos cartões de mensagens acumuladas.
const STALE_GROUP: &str = "telegram:stale:";

/// Saúde do canal, para a tela (T11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// A escuta não roda neste processo (sem a trava, ou ainda não ligada).
    Off,
    /// Sem token.
    NotConfigured,
    /// Token novo, ainda sem resposta do Telegram.
    Connecting,
    Connected,
    /// O Telegram recusou o token (401): a escuta para até o token mudar.
    InvalidToken,
    /// Outro programa usa este bot (409).
    Conflict,
    /// Falha de rede: a escuta tenta de novo com espera crescente.
    NetworkError,
    /// O Telegram pediu para esperar (429).
    RateLimited,
}

impl Health {
    /// Chave do aviso do terminal na entrada deste estado (só os problemas).
    fn notice(self) -> Option<&'static str> {
        match self {
            Health::InvalidToken => Some("nanty.log.telegram_invalid_token"),
            Health::Conflict => Some("nanty.log.telegram_conflict"),
            Health::NetworkError => Some("nanty.log.telegram_network"),
            _ => None,
        }
    }
}

/// Esperas e prazos da escuta (os testes encurtam).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tuning {
    pub poll_secs: u64,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// Intervalo mínimo entre dois envios ao chat (T07, C-10).
    pub send_gap: Duration,
    /// Espera do cartão novo antes do envio (T07, [`outbox::SEND_SETTLE`]).
    pub send_settle: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            poll_secs: POLL_SECS,
            backoff_base: BACKOFF_BASE,
            backoff_max: BACKOFF_MAX,
            send_gap: outbox::SEND_GAP,
            send_settle: outbox::SEND_SETTLE,
        }
    }
}

struct Inner {
    health: Health,
    /// `username` do bot (do `getMe`), para o link de pareamento.
    bot: Option<String>,
    pairing: Pairing,
    /// Hash do token que o Telegram recusou: a escuta não tenta de novo com ele.
    invalid_token: Option<String>,
    tuning: Tuning,
}

/// Estado do canal neste processo (em memória).
pub struct Link {
    inner: Mutex<Inner>,
    /// Acorda a escuta quando a configuração do canal muda (token, desparear).
    wake: Notify,
    /// A fila de saída (T07).
    outbox: outbox::Outbox,
}

impl Default for Link {
    fn default() -> Self {
        Self {
            outbox: outbox::Outbox::default(),
            inner: Mutex::new(Inner {
                health: Health::Off,
                bot: None,
                pairing: Pairing::default(),
                invalid_token: None,
                tuning: Tuning::default(),
            }),
            wake: Notify::new(),
        }
    }
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link").field("health", &self.health()).finish_non_exhaustive()
    }
}

impl Link {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn health(&self) -> Health {
        self.lock().health
    }

    /// A fila de saída (T07).
    pub fn outbox(&self) -> &outbox::Outbox {
        &self.outbox
    }

    /// Muda a saúde; `true` se mudou.
    fn set_health(&self, health: Health) -> bool {
        let mut inner = self.lock();
        let changed = inner.health != health;
        inner.health = health;
        changed
    }

    /// `username` do bot, quando o Telegram já respondeu ao `getMe`.
    pub fn bot(&self) -> Option<String> {
        self.lock().bot.clone()
    }

    fn set_bot(&self, bot: Option<String>) {
        self.lock().bot = bot;
    }

    fn invalid_token(&self) -> Option<String> {
        self.lock().invalid_token.clone()
    }

    fn mark_invalid(&self, key: String) {
        self.lock().invalid_token = Some(key);
    }

    pub fn tuning(&self) -> Tuning {
        self.lock().tuning
    }

    /// Troca as esperas da escuta (testes).
    pub fn set_tuning(&self, tuning: Tuning) {
        self.lock().tuning = tuning;
    }

    /// Estado da janela de pareamento (troca o código vencido).
    pub fn pair_state(&self, now_ms: u64) -> PairState {
        self.lock().pairing.state(now_ms)
    }

    /// Abre a janela de pareamento com um código novo (`None` sem gerador aleatório).
    pub fn open_pairing(&self, now_ms: u64) -> Option<PairState> {
        self.lock().pairing.open(now_ms)
    }

    fn attempt(&self, code: &str, now_ms: u64) -> PairAttempt {
        self.lock().pairing.attempt(code, now_ms)
    }

    /// Token novo (ou removido): esquece o bot, a recusa e a janela; acorda a escuta.
    pub fn reset(&self) {
        {
            let mut inner = self.lock();
            inner.bot = None;
            inner.invalid_token = None;
            inner.pairing.close();
        }
        self.wake.notify_one();
    }

    /// Desparear: fecha a janela e acorda a escuta.
    pub fn unpaired(&self) {
        self.lock().pairing.close();
        self.wake.notify_one();
    }
}

/// Muda a saúde e, se mudou, avisa a janela e (nos problemas) o terminal.
fn health_to(nanty: &Nanty, health: Health) {
    if nanty.telegram().set_health(health) {
        if let Some(key) = health.notice() {
            eprintln!("{}", crate::i18n::text(Lang::from_env(), key));
        }
        nanty.changed(Part::Status);
    }
}

/// Forma do token literal do Telegram: `<id do bot>:<segredo>`. Só esta forma entra na URL.
pub fn valid_token(token: &str) -> bool {
    let Some((id, secret)) = token.split_once(':') else { return false };
    (1..=20).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_digit())
        && (20..=100).contains(&secret.len())
        && secret.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `username` do bot com a forma do Telegram (entra no link).
fn valid_username(name: &str) -> bool {
    (1..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Link de início do pareamento: `https://t.me/<bot>?start=<código>`.
pub fn start_link(bot: &str, code: &str) -> String {
    format!("https://t.me/{bot}?start={code}")
}

/// Dado de um botão de cartão (até 64 bytes): `c:<id do cartão>:<token>:<y|n|l>`. A T07 monta os
/// botões com isto; o clique do dono chega a [`act::decide`] como decisão pelo Telegram.
pub fn callback_data(card_id: &str, token: &str, answer: Answer) -> String {
    let a = match answer {
        Answer::Yes => "y",
        Answer::No => "n",
        Answer::Later => "l",
    };
    format!("c:{card_id}:{token}:{a}")
}

/// Lê o dado de um botão de cartão.
pub fn parse_callback(data: &str) -> Option<(String, String, Answer)> {
    let mut parts = data.split(':');
    if parts.next()? != "c" {
        return None;
    }
    let card = parts.next()?;
    let token = parts.next()?;
    let answer = match parts.next()? {
        "y" => Answer::Yes,
        "n" => Answer::No,
        "l" => Answer::Later,
        _ => return None,
    };
    let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
    if parts.next().is_some() || !hex(card, 16) || !hex(token, 32) {
        return None;
    }
    Some((card.to_string(), token.to_string(), answer))
}

/// O código do `/start <código>` (o link de início manda assim).
fn start_code(text: &str) -> Option<&str> {
    let rest = text.trim().strip_prefix("/start")?;
    let code = rest.strip_prefix(' ')?.trim();
    (!code.is_empty()).then_some(code)
}

/// Hash do token resolvido (a chave da sessão; o token em si não fica guardado à parte).
fn token_key(tg: &NantyTelegram) -> Option<String> {
    tg.resolve_token().map(|t| hash_hex(&t))
}

/// Muda a parte do Telegram na configuração e grava, só se o token ainda for o de `key` (a tela
/// pode ter trocado o token no meio de um lote). `true` se gravou (ou nada mudou).
async fn save_telegram(state: &AppState, key: &str, f: impl FnOnce(&mut NantyTelegram)) -> bool {
    let mut guard = state.config().write().await;
    if token_key(&guard.nanty.telegram).as_deref() != Some(key) {
        return false;
    }
    let mut next = guard.clone();
    f(&mut next.nanty.telegram);
    if next.nanty.telegram == guard.nanty.telegram {
        return true;
    }
    if crate::server::config_api::persist(state, &next).await.is_err() {
        eprintln!("{}", crate::i18n::text(Lang::from_env(), "nanty.log.save_failed"));
        return false;
    }
    *guard = next;
    true
}

/// Uma linha do registro do canal.
fn journal(nanty: &Nanty, code: &str, params: Map<String, Value>) {
    let mut entry = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Telegram, code);
    entry.params = params;
    nanty.record(&entry);
}

fn param(name: &str, value: impl Into<Value>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(name.to_string(), value.into());
    map
}

/// Espera `dur`, a mudança da configuração do canal ou o encerramento. `true` = encerrar.
async fn pause(link: &Link, shutdown: &mut watch::Receiver<bool>, dur: Duration) -> bool {
    tokio::select! {
        () = crate::server::events::wait_shutdown(shutdown) => true,
        () = link.wake.notified() => false,
        () = tokio::time::sleep(dur) => false,
    }
}

/// Espera fixa (o `retry_after` do 429): só o encerramento interrompe.
async fn hold(shutdown: &mut watch::Receiver<bool>, dur: Duration) -> bool {
    tokio::select! {
        () = crate::server::events::wait_shutdown(shutdown) => true,
        () = tokio::time::sleep(dur) => false,
    }
}

/// Espera crescente.
struct Backoff {
    next: Option<Duration>,
}

impl Backoff {
    fn step(&mut self, tuning: &Tuning) -> Duration {
        let current = self.next.unwrap_or(tuning.backoff_base).min(tuning.backoff_max);
        self.next = Some(current.saturating_mul(2).min(tuning.backoff_max));
        current
    }

    fn reset(&mut self) {
        self.next = None;
    }
}

/// Uma sessão de escuta: um token, o cliente e o offset.
struct Session {
    key: String,
    client: TelegramClient,
    offset: i64,
    /// O bot respondeu ao `getMe`.
    ready: bool,
    /// Esvaziando a fila acumulada (Nants fechado ou token novo): o que vier é antigo.
    drain: bool,
}

/// A escuta do Telegram, até o encerramento. Só roda no dono da trava (`nanty::start`).
pub async fn listen(state: AppState, mut shutdown: watch::Receiver<bool>) {
    let nanty = Arc::clone(state.nanty());
    let link = nanty.telegram();
    let mut session: Option<Session> = None;
    let mut backoff = Backoff { next: None };
    loop {
        if *shutdown.borrow() {
            break;
        }
        let tg = state.config().read().await.nanty.telegram.clone();
        let tuning = link.tuning();
        let Some(token) = tg.resolve_token() else {
            session = None;
            health_to(&nanty, Health::NotConfigured);
            if pause(link, &mut shutdown, RECHECK).await {
                break;
            }
            continue;
        };
        let key = hash_hex(&token);
        if link.invalid_token().as_deref() == Some(key.as_str()) || !valid_token(&token) {
            session = None;
            link.mark_invalid(key);
            health_to(&nanty, Health::InvalidToken);
            if pause(link, &mut shutdown, RECHECK).await {
                break;
            }
            continue;
        }
        if session.as_ref().map(|s| s.key.as_str()) != Some(key.as_str()) {
            let client = match TelegramClient::new(state.telegram_api(), token, Duration::from_secs(tuning.poll_secs)) {
                Ok(client) => client,
                Err(_) => {
                    health_to(&nanty, Health::NetworkError);
                    if pause(link, &mut shutdown, backoff.step(&tuning)).await {
                        break;
                    }
                    continue;
                }
            };
            backoff.reset();
            session = Some(Session { key: key.clone(), client, offset: tg.offset, ready: false, drain: true });
            health_to(&nanty, Health::Connecting);
        }
        let Some(s) = session.as_mut() else { continue };
        let result = if s.ready {
            let timeout = if s.drain { 0 } else { tuning.poll_secs };
            tokio::select! {
                () = crate::server::events::wait_shutdown(&mut shutdown) => break,
                () = link.wake.notified() => continue,
                r = s.client.get_updates(s.offset, timeout) => r.map(Some),
            }
        } else {
            tokio::select! {
                () = crate::server::events::wait_shutdown(&mut shutdown) => break,
                r = s.client.get_me() => r.map(|bot| {
                    link.set_bot(bot.username.filter(|u| valid_username(u)));
                    None
                }),
            }
        };
        match result {
            Ok(None) => {
                s.ready = true;
                backoff.reset();
                health_to(&nanty, Health::Connected);
                let lang = nanty_lang(&state.config().read().await.nanty);
                publish_commands(&nanty, &s.client, lang).await;
            }
            Ok(Some(updates)) => {
                backoff.reset();
                health_to(&nanty, Health::Connected);
                if updates.is_empty() {
                    s.drain = false;
                    continue;
                }
                let stale = s.drain;
                let mut last = None;
                for update in updates {
                    // Deduplicação: o `update_id` já processado (reentregue) é pulado.
                    if update.update_id < s.offset {
                        continue;
                    }
                    handle(&state, &nanty, s, &update, stale).await;
                    last = Some(update.update_id);
                }
                // Offset gravado depois de processar o lote.
                if let Some(last) = last {
                    s.offset = last.saturating_add(1);
                    let offset = s.offset;
                    save_telegram(&state, &s.key, |t| t.offset = offset).await;
                }
            }
            Err(TgError::Unauthorized) => {
                link.mark_invalid(s.key.clone());
                link.set_bot(None);
                session = None;
                health_to(&nanty, Health::InvalidToken);
                journal(&nanty, "telegram.invalid_token", Map::new());
            }
            Err(TgError::Conflict) => {
                health_to(&nanty, Health::Conflict);
                if pause(link, &mut shutdown, backoff.step(&tuning)).await {
                    break;
                }
            }
            Err(TgError::RetryAfter(secs)) => {
                health_to(&nanty, Health::RateLimited);
                if hold(&mut shutdown, Duration::from_secs(secs)).await {
                    break;
                }
            }
            Err(e) => {
                if nanty.telegram().health() != Health::NetworkError {
                    // O detalhe já vem sem URL e sem token (C-09).
                    eprintln!("{}", tr(Lang::from_env(), "nanty.log.telegram_network_detail", &[("detail", &e.to_string())]));
                }
                health_to(&nanty, Health::NetworkError);
                if pause(link, &mut shutdown, backoff.step(&tuning)).await {
                    break;
                }
            }
        }
    }
    health_to(&nanty, Health::Off);
}

/// Processa uma atualização. Nenhuma falha aqui interrompe a escuta.
async fn handle(state: &AppState, nanty: &Arc<Nanty>, s: &Session, update: &Update, stale: bool) {
    if let Some(message) = &update.message {
        on_message(state, nanty, s, update.update_id, message, stale).await;
    } else if let Some(query) = &update.callback_query {
        on_callback(state, nanty, s, update.update_id, query, stale).await;
    }
}

async fn on_message(state: &AppState, nanty: &Arc<Nanty>, s: &Session, update_id: i64, m: &Message, stale: bool) {
    if m.chat.kind != "private" {
        // Grupo, supergrupo ou canal: o Nanty não conversa em grupo e sai dele.
        let _ = s.client.leave_chat(m.chat.id).await;
        journal(nanty, "telegram.group_left", param("chat_type", m.chat.kind.clone()));
        return;
    }
    let Some(from) = m.from.as_ref().filter(|f| !f.is_bot) else { return };
    let cfg = state.config().read().await.nanty.clone();
    let lang = nanty_lang(&cfg);
    match (cfg.telegram.owner_id, cfg.telegram.chat_id) {
        (Some(owner), Some(chat)) if from.id == owner && m.chat.id == chat => {
            owner_message(state, nanty, update_id, m, stale).await;
        }
        (Some(_), Some(_)) => {
            journal(nanty, "telegram.stranger", param("kind", "message"));
        }
        _ => match m.text.as_deref().and_then(start_code) {
            Some(code) => pair(state, nanty, s, m.chat.id, from.id, code, lang).await,
            None => journal(nanty, "telegram.unpaired", param("kind", "message")),
        },
    }
}

/// O `/start <código>` sem dono pareado.
async fn pair(state: &AppState, nanty: &Arc<Nanty>, s: &Session, chat_id: i64, from_id: i64, code: &str, lang: Lang) {
    let attempt = if pairing::valid_code(code) {
        nanty.telegram().attempt(code, nanty.now().epoch_ms)
    } else {
        // Forma errada conta como erro, sem comparar.
        nanty.telegram().attempt("", nanty.now().epoch_ms)
    };
    match attempt {
        PairAttempt::Paired => {
            let saved = save_telegram(state, &s.key, |t| {
                t.chat_id = Some(chat_id);
                t.owner_id = Some(from_id);
            })
            .await;
            if saved {
                journal(nanty, "telegram.paired", Map::new());
                nanty.changed(Part::Status);
                let _ = s.client.send_message(chat_id, &tr(lang, "nanty.telegram.paired", &[])).await;
            } else {
                journal(nanty, "telegram.pair_save_failed", Map::new());
            }
        }
        other => {
            let params = match other {
                PairAttempt::Invalid { attempts_left } => param("attempts_left", attempts_left),
                PairAttempt::Locked => param("reason", "locked"),
                _ => param("reason", "inactive"),
            };
            journal(nanty, "telegram.pair_failed", params);
            nanty.changed(Part::Status);
            let _ = s.client.send_message(chat_id, &tr(lang, "nanty.telegram.pair_invalid", &[])).await;
        }
    }
}

/// Mensagem do dono: entra na conversa, redigida (T08). Ao vivo, o Nanty responde aqui, numa tarefa
/// à parte (a escuta segue enquanto o modelo pensa); a nota de voz é transcrita antes (T09).
/// Acumulada, vira só um cartão marcado como antigo: nenhum comando nem pedido sai de uma mensagem
/// acumulada, e a nota de voz acumulada nem é baixada.
async fn owner_message(state: &AppState, nanty: &Arc<Nanty>, update_id: i64, m: &Message, stale: bool) {
    let now = nanty.now().epoch_ms;
    let secrets = state.secret_set().await;
    let person = m.text.as_deref().and_then(|text| super::chat::person_message(nanty, &secrets, Channel::Telegram, text));
    if stale {
        let age_min = u64::try_from(m.date).ok().map(|date| (now / 1000).saturating_sub(date) / 60);
        stale_card(nanty, update_id, "message", age_min);
    } else if let Some(person) = person {
        super::chat::spawn_reply(state, person);
    } else if let Some(note) = m.voice.clone().filter(|_| m.text.is_none()) {
        // T09: a nota de voz do dono é baixada e transcrita numa tarefa à parte e segue como texto.
        super::voice::spawn_hear(state, note);
    }
}

/// Publica os comandos do bot (T08): um conjunto por idioma do Telegram (`pt` e `en`) e, para os
/// outros idiomas, o do Nanty. Falha não muda nada: os dois conjuntos são aceitos de qualquer jeito.
async fn publish_commands(nanty: &Nanty, client: &TelegramClient, lang: Lang) {
    let sets = [(Some("pt"), Lang::PtBr), (Some("en"), Lang::En), (None, lang)];
    for (code, set_lang) in sets {
        let list = super::chat::command_menu(set_lang);
        if let Err(e) = client.set_my_commands(&list, code).await {
            journal(nanty, "telegram.commands_failed", param("error", e.code()));
        }
    }
}

/// Cartão de uma mensagem ou de um clique acumulado enquanto o Nants estava fechado: marcado como
/// antigo, sem ação nem token, e só na janela (a T07 não o reenvia e o turno proativo não o vê).
fn stale_card(nanty: &Arc<Nanty>, update_id: i64, kind: &str, age_min: Option<u64>) {
    let now = nanty.now().epoch_ms;
    let mut params = param("stale", true);
    if let Some(age) = age_min {
        params.insert("age_min".into(), json!(age));
    }
    let card = Card {
        id: super::new_id(),
        group: format!("{STALE_GROUP}{update_id}"),
        code: format!("nanty.card.telegram_stale_{kind}"),
        params,
        created_at_ms: now,
        expires_at_ms: now.saturating_add(STALE_CARD_TTL_MS),
        delivery: Some(Delivery::Digest),
        ..Card::default()
    };
    let added = nanty.update(|st| {
        let open = st.cards.iter().filter(|c| c.group.starts_with(STALE_GROUP)).count();
        if open >= MAX_STALE_CARDS || st.cards.iter().any(|c| c.group == card.group) {
            return false;
        }
        st.cards.push(card);
        true
    });
    journal(nanty, "telegram.stale", {
        let mut p = param("kind", kind);
        p.insert("card".into(), json!(added));
        p
    });
}

async fn on_callback(state: &AppState, nanty: &Arc<Nanty>, s: &Session, update_id: i64, q: &CallbackQuery, stale: bool) {
    let cfg = state.config().read().await.nanty.clone();
    let lang = nanty_lang(&cfg);
    let same_chat = q.message.as_ref().is_none_or(|m| Some(m.chat.id) == cfg.telegram.chat_id);
    let owner = cfg.telegram.chat_id.is_some() && cfg.telegram.owner_id == Some(q.from.id) && same_chat;
    if !owner {
        journal(nanty, "telegram.stranger", param("kind", "click"));
        return;
    }
    if stale {
        stale_card(nanty, update_id, "click", None);
        let _ = s.client.answer_callback(&q.id, Some(&tr(lang, "nanty.telegram.stale_click", &[]))).await;
        return;
    }
    let Some((card_id, token, answer)) = q.data.as_deref().and_then(parse_callback) else {
        let _ = s.client.answer_callback(&q.id, None).await;
        return;
    };
    let text = match act::decide(state, &card_id, &token, answer, DecidedBy::Telegram).await {
        Ok(outcome) => {
            if outcome.outcome == "expired" {
                strip_clicked(nanty, q);
            }
            act::outcome_text(lang, &outcome)
        }
        Err(e) => {
            // Token que não confere: o botão é de uma oferta que já saiu do cartão.
            if e.code() == Some("not_found") {
                strip_clicked(nanty, q);
            }
            let reason = e
                .code()
                .map(|code| crate::server::error::fallback_text_in(lang, code, e.params().unwrap_or(&Map::new())))
                .unwrap_or_default();
            tr(lang, "nanty.outcome.failed", &[("error", &reason)])
        }
    };
    let _ = s.client.answer_callback(&q.id, Some(&text)).await;
}

/// O botão tocado não decide mais nada (cartão vencido ou desconhecido): a mensagem perde os botões.
/// O cartão que a fila conhece é editado por ela (com o "expirou"); o resto só perde os botões.
fn strip_clicked(nanty: &Nanty, q: &CallbackQuery) {
    let Some(m) = &q.message else { return };
    let known = nanty.snapshot().telegram_sent.iter().any(|r| r.message_id == Some(m.message_id) && r.chat_id == m.chat.id);
    if !known && m.message_id > 0 {
        nanty.telegram().outbox().strip(m.chat.id, m.message_id);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod t07_tests;

#[cfg(test)]
mod t08_tests;

#[cfg(test)]
mod t09_tests;
