//! Voz do Nanty (spec 138 T09, G5; só Gemini desde a spec 139 T07, G6): a fala com legenda, a nota
//! de voz transcrita, a lista de vozes e os áudios da conversa para a janela.
//!
//! - **Só o Gemini, depois o texto:** uma conexão do Google Gemini (`[nanty.voice] connection_id`,
//!   ou, vazia, a primeira com chave) fala e transcreve. A fala é o WAV de 24 kHz da Interactions
//!   API, que vira Opus em OGG ([`encode`]). Se ela falha, a resposta segue em texto. O modelo e a
//!   voz vêm da configuração (os iniciais provisórios ficam em `config::nanty`, nunca aqui).
//! - **Quando fala (spec 139 T08):** com a voz ligada, em toda saída do Nanty; desligada, só texto.
//!   - A resposta, na janela e no Telegram, e o resumo da manhã saem por [`deliver`]: o texto entra
//!     na conversa assim que fica pronto, e o áudio entra depois na mesma mensagem, fora da fila de
//!     saída. Na janela, o tocador nunca toca sozinho. No Telegram, a nota espera a fala.
//!   - O aviso com botões fala na fila de saída ([`super::telegram::outbox`]), depois da espera da
//!     aba visível, e o áudio entra na conversa quando sai ([`keep_notice`]). O texto do modelo
//!     sobre esse aviso não tem outro áudio.
//!   - O que é só controle fica em texto: a resposta ao clique, o cartão pedido na conversa (a fala
//!     é a da resposta que o cita) e o aviso de teste do Telegram.
//!   - A fala é a do texto que sai, já redigido, cortada em fim de frase ([`speech_text`]) e com
//!     prazo ([`speak_on_time`]). A nota sai com a legenda curta (até 1024) e, se a legenda cortou,
//!     o texto completo numa mensagem à parte.
//! - **Ouvir ([`hear`]):** só a nota de voz do dono (a escuta já filtra estranhos e grupos, T06).
//!   A duração e o tamanho são conferidos antes de baixar (até [`MAX_INPUT_BYTES`] e
//!   `max_input_secs`), e de novo no `getFile` e no download. A transcrição sai com `store: false`
//!   (o provedor não guarda); o texto entra na conversa redigido, com a marca `voice`, e segue pela
//!   fila única da T08.
//! - **Vozes ([`get_voices`]):** a lista do `/v1beta/voices` da conexão, com um cache curto em
//!   memória ([`VOICES_TTL`]), para os Ajustes.
//! - **Orçamento:** cada fala e cada transcrição entram no gasto do dia do Nanty (os tokens
//!   informados ou estimados e o US$ pelo preço do modelo, quando há) e contam em `voice_calls`.
//!   Sem orçamento, com o prazo vencido ou com a falha da fala, vai o texto: o aviso nunca se perde.
//!   O registro guarda só códigos (o provedor, o motivo), nunca o texto nem o áudio.
//! - **Os áudios ([`get_audio`]):** ficam em `<pasta do Nanty>/audio/<id>.ogg` (0600) e só são
//!   servidos pelo id de uma mensagem que ainda está na conversa: quando a mensagem sai na rotação,
//!   o áudio deixa de ser servido e o arquivo é apagado na próxima limpeza ([`prune`]).

pub mod encode;
pub mod provider;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::journal::{JournalEntry, JournalKind};
use super::jsonl::{ensure_private_dir, read_private_bytes, write_atomic};
use super::store::{AudioRef, Card, Channel, ConversationMessage};
use super::telegram::client::{TelegramClient, VoiceNote};
use super::telegram::format;
use super::{nanty_lang, render, turn, Avatar, Nanty};
use crate::agents::llm_client::{Billing, Usage};
use crate::config::{Config, Connection, ConnectionKind, VoiceMigration};
use crate::i18n::Lang;
use crate::server::error::ApiError;
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Pasta dos áudios dentro da pasta do Nanty.
pub const AUDIO_DIR: &str = "audio";
/// Maior nota de voz baixada (o limite do `getFile` do Telegram).
pub const MAX_INPUT_BYTES: u64 = 20 * 1024 * 1024;
/// Maior áudio guardado ou servido.
pub const MAX_AUDIO_BYTES: u64 = 20 * 1024 * 1024;
/// Maior texto mandado à fala (a resposta do modelo tem até 600; os textos fixos são curtos).
pub const MAX_SPEECH_CHARS: usize = 2000;
/// Tokens por segundo de áudio, para estimar a saída quando o provedor não informa (a taxa
/// publicada do áudio do Gemini).
pub const AUDIO_TOKENS_PER_SEC: u64 = 25;
/// Quanto a lista de vozes fica guardada em memória (provisório).
pub const VOICES_TTL: Duration = Duration::from_secs(10 * 60);
/// Um arquivo de áudio sem mensagem só é apagado depois disto (a mensagem dele pode estar sendo
/// gravada agora).
const PRUNE_GRACE: Duration = Duration::from_secs(10 * 60);
/// Cache privado: o id do áudio nunca muda de conteúdo.
const CACHE_CONTROL: &str = "private, max-age=3600";
/// Quem fala (o registro guarda este código).
pub const PROVIDER: &str = "gemini";

/// A fala: a conexão, o modelo e a voz já resolvidos.
#[derive(Debug, Clone)]
pub struct Step {
    pub connection: Connection,
    pub model: String,
    pub voice: String,
}

/// O tipo de conexão serve para a fala? Só o Google Gemini. A mesma regra vale no [`speaker`] e
/// na gravação dos Ajustes (`nanty::settings`).
pub fn speaks(kind: ConnectionKind) -> bool {
    kind == ConnectionKind::GoogleGemini
}

/// O tipo de conexão serve para a transcrição? Só o Google Gemini. A mesma regra vale no
/// [`transcriber`] e na gravação dos Ajustes (`nanty::settings`).
pub fn transcribes(kind: ConnectionKind) -> bool {
    kind == ConnectionKind::GoogleGemini
}

/// A conexão serve para a voz inteira (falar e transcrever).
pub fn voice_capable(kind: ConnectionKind) -> bool {
    speaks(kind) && transcribes(kind)
}

/// A conexão da voz: a escolhida (`missing_connection` se sumiu, `unsupported` se não é do Gemini)
/// ou, vazia, a primeira do Google Gemini com chave (`no_connection` sem nenhuma).
pub fn voice_connection<'a>(cfg: &'a Config, id: &str) -> Result<&'a Connection, &'static str> {
    let id = id.trim();
    if id.is_empty() {
        return cfg.connections.iter().find(|c| voice_capable(c.kind) && c.has_usable_key()).ok_or("no_connection");
    }
    let conn = cfg.connections.iter().find(|c| c.id == id).ok_or("missing_connection")?;
    if voice_capable(conn.kind) {
        Ok(conn)
    } else {
        Err("unsupported")
    }
}

/// A fala: a conexão da voz, com o modelo e a voz da configuração.
pub fn speaker(cfg: &Config) -> Result<Step, &'static str> {
    let voice = &cfg.nanty.voice;
    let conn = voice_connection(cfg, &voice.connection_id)?;
    Ok(Step { connection: conn.clone(), model: voice.speech_model(), voice: voice.voice_name() })
}

/// A conexão e o modelo da transcrição: a mesma conexão da fala.
pub fn transcriber(cfg: &Config) -> Option<(Connection, String)> {
    let conn = voice_connection(cfg, &cfg.nanty.voice.connection_id).ok()?;
    Some((conn.clone(), cfg.nanty.voice.transcribe_model()))
}

/// A linha do registro da migração da voz antiga (C-16): quantos passos saíram (os da OpenAI,
/// entre eles), se o passo do Gemini virou a conexão e se a transcrição antiga era de outra
/// conexão. Só números e sim ou não.
pub fn migration_entry(nanty: &Nanty, done: &VoiceMigration) -> JournalEntry {
    let mut p = param("dropped", done.dropped);
    p.insert("moved".into(), json!(done.moved));
    p.insert("transcribe_dropped".into(), json!(done.transcribe_dropped));
    journal(nanty, "voice.migrated", p)
}

/// A linha do registro quando o arquivo não passou à forma nova (spec 139 T08): só o código da
/// falha. A migração roda de novo no próximo início.
pub fn migration_unsaved(nanty: &Nanty, code: &str) -> JournalEntry {
    journal(nanty, "voice.migration_unsaved", param("reason", code))
}

// ---- Registro e gasto ----

fn journal(nanty: &Nanty, code: &str, params: Map<String, Value>) -> JournalEntry {
    let mut entry = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Voice, code);
    entry.params = params;
    entry
}

fn param(name: &str, value: impl Into<Value>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(name.to_string(), value.into());
    map
}

fn exhausted(cfg: &Config, nanty: &Nanty) -> bool {
    turn::budget_view(cfg, &nanty.snapshot().spend_on(&nanty.now().day_key())).exhausted
}

fn estimate_tokens(text: &str) -> u64 {
    (text.len() / 4 + 1) as u64
}

/// Soma a chamada ao gasto do dia e devolve o custo (para o registro).
fn charge(cfg: &Config, nanty: &Nanty, model: &str, input: u64, output: u64) -> Option<f64> {
    let usage = Usage::new(u32::try_from(input).unwrap_or(u32::MAX), u32::try_from(output).unwrap_or(u32::MAX));
    let usd = turn::turn_cost(cfg, model, &usage, Billing::Metered);
    let day = nanty.now().day_key();
    nanty.update(|st| {
        st.add_voice_spend(&day, input, output, usd);
        true
    });
    nanty.changed(super::Part::Status);
    usd
}

/// O avatar volta ao estado derivado em qualquer saída.
struct Speaking<'a>(&'a Nanty);

impl Drop for Speaking<'_> {
    fn drop(&mut self) {
        self.0.set_busy(None);
    }
}

// ---- A fala ----

/// Uma fala pronta: a nota de voz (Opus em OGG) e a duração.
#[derive(Debug, Clone)]
pub struct Spoken {
    pub ogg: Vec<u8>,
    pub duration_secs: u32,
}

/// Chama o Gemini, soma o gasto e codifica o PCM em Opus.
async fn speak_step(cfg: &Config, nanty: &Nanty, step: &Step, text: &str) -> Result<Spoken, &'static str> {
    let audio = provider::gemini_speak(&step.connection, &step.model, &step.voice, text, None).await?;
    let bytes = audio.bytes.clone();
    let encoded = tokio::task::spawn_blocking(move || {
        let pcm = encode::pcm_of_gemini(&bytes)?;
        let secs = pcm.duration_secs();
        encode::ogg_opus(&pcm).map(|ogg| (ogg, secs))
    })
    .await
    .map_err(|_| "encode")?;
    // O provedor já cobrou: o gasto entra mesmo se a codificação falhar.
    let secs = encoded.as_ref().map(|(_, s)| *s).unwrap_or(0);
    let output = audio.output_tokens.unwrap_or(u64::from(secs) * AUDIO_TOKENS_PER_SEC);
    charge(cfg, nanty, &step.model, audio.input_tokens.unwrap_or_else(|| estimate_tokens(text)), output);
    let (ogg, duration_secs) = encoded?;
    if ogg.len() as u64 > MAX_AUDIO_BYTES {
        return Err("too_big");
    }
    Ok(Spoken { ogg, duration_secs })
}

/// O texto da fala: inteiro até `max` caracteres; mais longo, cortado no último fim de frase que
/// cabe (a pontuação seguida de espaço, ou uma quebra de linha) e, sem nenhum, na última palavra
/// inteira. A fala nunca termina no meio de uma frase por causa do limite.
pub fn speech_text(text: &str, max: usize) -> String {
    let Some((limit, _)) = text.char_indices().nth(max) else { return text.trim().to_string() };
    let ends = |i: usize, c: char| {
        let after = i + c.len_utf8();
        let closes = matches!(c, '.' | '!' | '?' | '…') && text[after..].chars().next().is_none_or(char::is_whitespace);
        (closes || c == '\n').then_some(after)
    };
    let head = &text[..limit];
    let sentence = head.char_indices().filter_map(|(i, c)| ends(i, c)).last().filter(|end| !text[..*end].trim().is_empty());
    let cut = sentence.or_else(|| head.rfind(char::is_whitespace)).unwrap_or(limit);
    text[..cut].trim().to_string()
}

/// Como [`speech_text`], preservando o fecho do texto (a frase do servidor, spec 139 T12): o corte
/// fica no que vem antes dele.
pub fn speech_keeping(text: &str, tail: Option<&str>, max: usize) -> String {
    let head = tail.and_then(|t| Some((text.strip_suffix(t)?.trim_end(), t)));
    match head {
        Some((head, tail)) if text.chars().count() > max && tail.chars().count() < max => {
            format!("{}\n{tail}", speech_text(head, max.saturating_sub(tail.chars().count() + 1)))
        }
        _ => speech_text(text, max),
    }
}

/// A fala do texto pelo Gemini. `Err` com os motivos (códigos) quando ele não falou: o texto segue
/// sem áudio.
pub async fn speak(state: &AppState, nanty: &Nanty, text: &str) -> Result<Spoken, Vec<&'static str>> {
    let cfg = state.config().read().await.clone();
    let text = speech_text(text, MAX_SPEECH_CHARS);
    let result = match speaker(&cfg) {
        Err(code) => Err(code),
        Ok(_) if exhausted(&cfg, nanty) => Err("budget"),
        Ok(step) => {
            nanty.set_busy(Some(Avatar::Speaking));
            let speaking = Speaking(nanty);
            let result = speak_step(&cfg, nanty, &step, &text).await;
            drop(speaking);
            match &result {
                Ok(done) => {
                    let mut p = param("provider", PROVIDER);
                    p.insert("secs".into(), json!(done.duration_secs));
                    nanty.record(&journal(nanty, "voice.ok", p));
                }
                Err(code) => {
                    let mut p = param("provider", PROVIDER);
                    p.insert("reason".into(), json!(code));
                    nanty.record(&journal(nanty, "voice.failed", p));
                }
            }
            result
        }
    };
    result.map_err(|code| {
        let reasons = vec![code];
        nanty.record(&journal(nanty, "voice.text", param("reasons", json!(reasons))));
        reasons
    })
}

/// Como [`speak`], dentro do prazo da fala (spec 139 T08, C-14). Vencido, o motivo `deadline` fica
/// no registro e a saída vai em texto na hora. A chamada segue numa tarefa à parte, sem ser abortada:
/// ela termina sozinha, com o gasto e o registro reais, e a fala tardia é descartada. Assim toda fala
/// enviada ao provedor entra no orçamento.
pub async fn speak_on_time(state: &AppState, nanty: &Nanty, text: &str) -> Result<Spoken, Vec<&'static str>> {
    let deadline = nanty.telegram().tuning().speech_deadline;
    let (job_state, job_text) = (state.clone(), text.to_string());
    let mut job = tokio::spawn(async move { speak(&job_state, job_state.nanty(), &job_text).await });
    let reason = match tokio::time::timeout(deadline, &mut job).await {
        Ok(Ok(result)) => return result,
        Ok(Err(_)) => "failed",
        Err(_) => "deadline",
    };
    let reasons = vec![reason];
    nanty.record(&journal(nanty, "voice.text", param("reasons", json!(reasons))));
    Err(reasons)
}

/// Entrega a fala do Nanty (spec 139 T08): a resposta, o resumo e a recusa da nota de voz.
/// 1. O texto entra na conversa na hora: a janela o mostra, e ele não se perde se a fala falhar.
/// 2. Com a voz ligada, a fala é gerada (`recording` liga o "gravando áudio" para quem escreveu pelo
///    Telegram) e o áudio entra na mesma mensagem, numa segunda linha com o mesmo id. Sem fala, nada
///    muda, e o motivo fica no registro.
/// 3. A mensagem vai à fila de saída uma vez só: como nota de voz, ou em texto.
///
/// As mensagens falam uma de cada vez, na ordem em que entraram: uma fala curta não ultrapassa a
/// anterior no Telegram.
pub async fn deliver(state: &AppState, nanty: &Arc<Nanty>, message: &mut ConversationMessage, recording: bool) {
    keep(nanty, message).await;
    let enabled = state.config().read().await.nanty.voice.enabled;
    let record = (recording && enabled).then(|| nanty.telegram().outbox().recording());
    let turn = nanty.speech_turn.lock().await;
    attach_speech(state, nanty, message).await;
    drop(record);
    if message.audio.is_some() {
        keep(nanty, message).await;
    }
    let n = Arc::clone(nanty);
    let done = message.clone();
    let _ = tokio::task::spawn_blocking(move || n.send_out(&done)).await;
    drop(turn);
}

/// Grava a mensagem na conversa, fora do laço assíncrono.
async fn keep(nanty: &Arc<Nanty>, message: &ConversationMessage) {
    let n = Arc::clone(nanty);
    let saved = message.clone();
    let _ = tokio::task::spawn_blocking(move || n.keep_message(&saved)).await;
}

/// Com a voz ligada, a mensagem ganha a fala: o texto dela (como vai ao Telegram, já redigido) vira
/// áudio, que a janela toca e o Telegram recebe como nota de voz.
async fn attach_speech(state: &AppState, nanty: &Arc<Nanty>, message: &mut ConversationMessage) {
    let cfg = state.config().read().await.clone();
    if !cfg.nanty.voice.enabled {
        return;
    }
    let lang = nanty_lang(&cfg.nanty, nanty);
    let secrets = SecretSet::from_config(&cfg);
    let text = format::clean(&secrets, &render::message_text(lang, message));
    if text.is_empty() {
        return;
    }
    let tail = render::closing(lang, message).map(|line| format::clean(&secrets, &line));
    let text = speech_keeping(&text, tail.as_deref(), MAX_SPEECH_CHARS);
    let Ok(spoken) = speak_on_time(state, nanty, &text).await else { return };
    let n = Arc::clone(nanty);
    let audio = tokio::task::spawn_blocking(move || save_audio(&n, &spoken.ogg, spoken.duration_secs)).await.ok().flatten();
    if audio.is_none() {
        unsaved(nanty);
    }
    message.audio = audio;
}

/// O áudio não foi guardado: a mensagem segue em texto, com o motivo no registro.
fn unsaved(nanty: &Nanty) {
    nanty.record(&journal(nanty, "voice.text", param("reasons", json!(["save"]))));
}

/// O áudio de um aviso entra na conversa (spec 139 T08): a mensagem tem o texto fixo do cartão e o
/// áudio, com a mesma rotação das outras, para a janela tocar. Ela só é gravada: o aviso já foi ao
/// Telegram.
pub fn keep_notice(nanty: &Nanty, card: &Card, lang: Lang, spoken: &Spoken) {
    let Some(audio) = save_audio(nanty, &spoken.ogg, spoken.duration_secs) else {
        unsaved(nanty);
        return;
    };
    let mut message =
        ConversationMessage::from_nanty(super::new_id(), nanty.now().epoch_ms, Channel::Telegram, &card.code, card.params.clone());
    message.project_id = card.project_id.clone();
    message.card_ids = vec![card.id.clone()];
    message.lang = Some(lang.tag().to_string());
    message.audio = Some(audio);
    nanty.keep_message(&message);
}

// ---- Os áudios guardados ----

/// Id de áudio aceito: 16 hex (o `new_id`).
pub fn valid_audio_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Nanty {
    /// A pasta dos áudios (`None` sem pasta de dados).
    fn audio_dir(&self) -> Option<PathBuf> {
        self.store.as_ref().map(|s| s.dir().join(AUDIO_DIR))
    }

    /// Toda a conversa guardada (as gerações que a rotação ainda guarda).
    fn conversation_all(&self) -> Vec<ConversationMessage> {
        self.store.as_ref().map(|s| s.conversation().tail(usize::MAX)).unwrap_or_default()
    }

    /// O áudio de uma mensagem que ainda está na conversa.
    pub fn read_audio(&self, id: &str) -> Option<Vec<u8>> {
        if !valid_audio_id(id) {
            return None;
        }
        let referenced = self.conversation_all().iter().any(|m| m.audio.as_ref().is_some_and(|a| a.id == id));
        if !referenced {
            return None;
        }
        read_private_bytes(&self.audio_dir()?.join(format!("{id}.ogg")), MAX_AUDIO_BYTES)
    }
}

/// Grava um áudio (só o dono da trava) e devolve a referência.
pub fn save_audio(nanty: &Nanty, ogg: &[u8], duration_secs: u32) -> Option<AudioRef> {
    if !nanty.owns() || ogg.is_empty() || ogg.len() as u64 > MAX_AUDIO_BYTES {
        return None;
    }
    let dir = nanty.audio_dir()?;
    ensure_private_dir(&dir).ok()?;
    let id = super::new_id();
    write_atomic(&dir.join(format!("{id}.ogg")), ogg).ok()?;
    prune(nanty);
    Some(AudioRef { id, duration_secs, bytes: ogg.len() as u64 })
}

/// Apaga os áudios que nenhuma mensagem da conversa cita mais (a rotação levou a mensagem, ou o
/// projeto saiu), passada a espera de [`PRUNE_GRACE`].
pub fn prune(nanty: &Nanty) {
    prune_older(nanty, PRUNE_GRACE);
}

/// Como [`prune`], com a espera dada (os testes usam zero).
fn prune_older(nanty: &Nanty, grace: Duration) {
    if !nanty.owns() {
        return;
    }
    let Some(dir) = nanty.audio_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let referenced: Vec<String> = nanty.conversation_all().into_iter().filter_map(|m| m.audio.map(|a| a.id)).collect();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".ogg")) else { continue };
        if !valid_audio_id(id) || referenced.iter().any(|r| r == id) {
            continue;
        }
        let old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= grace);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn not_found() -> ApiError {
    ApiError::coded(StatusCode::NOT_FOUND, "not_found", Map::new())
}

/// Um intervalo pedido no `Range` (T10): o Safari, inclusive o do iPhone, só toca um áudio servido
/// com intervalos (ele pede `bytes=0-1` antes de tocar e desiste sem a resposta 206).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// Sem `Range`, ou um pedido que a rota não atende por partes (vários intervalos, outra
    /// unidade): o arquivo inteiro, com 200.
    Full,
    /// Do byte `start` ao `end`, inclusive: 206.
    Part { start: u64, end: u64 },
    /// Fora do arquivo: 416.
    Unsatisfiable,
}

/// Lê o `Range` de um único intervalo em bytes (`a-b`, `a-` ou `-n`) para um arquivo de `len` bytes.
pub fn parse_range(raw: Option<&str>, len: u64) -> ByteRange {
    let Some(spec) = raw.map(str::trim).and_then(|r| r.strip_prefix("bytes=")) else { return ByteRange::Full };
    if spec.contains(',') {
        return ByteRange::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else { return ByteRange::Full };
    let (a, b) = (a.trim(), b.trim());
    let parsed = |v: &str| v.parse::<u64>().ok();
    let (start, end) = match (a.is_empty(), b.is_empty()) {
        // `-n`: os últimos `n` bytes.
        (true, false) => match parsed(b) {
            Some(0) => return ByteRange::Unsatisfiable,
            Some(n) => (len.saturating_sub(n), len.saturating_sub(1)),
            None => return ByteRange::Full,
        },
        (false, true) => match parsed(a) {
            Some(start) => (start, len.saturating_sub(1)),
            None => return ByteRange::Full,
        },
        (false, false) => match (parsed(a), parsed(b)) {
            (Some(start), Some(end)) if start <= end => (start, end.min(len.saturating_sub(1))),
            (Some(_), Some(_)) => return ByteRange::Unsatisfiable,
            _ => return ByteRange::Full,
        },
        (true, true) => return ByteRange::Full,
    };
    if len == 0 || start >= len {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Part { start, end }
}

/// `GET /api/nanty/audio/:id` (T09, para a janela da T10): o áudio de uma mensagem da conversa,
/// pelo id validado (nunca um caminho vindo do cliente). Id inválido, áudio que saiu da conversa ou
/// arquivo ausente: 404, sem eco. Aceita o `Range` de um intervalo (206 e `Content-Range`; fora do
/// arquivo, 416), que o Safari exige para tocar.
pub async fn get_audio(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    if !valid_audio_id(&id) {
        return Err(not_found());
    }
    let nanty = Arc::clone(state.nanty());
    let data = tokio::task::spawn_blocking(move || nanty.read_audio(&id)).await.ok().flatten().ok_or_else(not_found)?;
    let len = data.len() as u64;
    let range = parse_range(headers.get(header::RANGE).and_then(|v| v.to_str().ok()), len);
    let mut resp = match range {
        ByteRange::Full => data.into_response(),
        ByteRange::Part { start, end } => {
            let part = data.get(start as usize..=end as usize).map(<[u8]>::to_vec).unwrap_or_default();
            let mut resp = (StatusCode::PARTIAL_CONTENT, part).into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")) {
                resp.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            resp
        }
        ByteRange::Unsatisfiable => {
            let mut resp =
                ApiError::coded(StatusCode::RANGE_NOT_SATISFIABLE, "nanty_audio_range", Map::new()).into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{len}")) {
                resp.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            resp.headers_mut().insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return Ok(resp);
        }
    };
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(provider::OGG_MIME));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    Ok(resp)
}

// ---- As vozes ----

/// O que os Ajustes oferecem para a voz: as vozes e os modelos de fala.
#[derive(Debug, Clone, Default)]
pub struct VoiceOptions {
    voices: Vec<provider::VoiceInfo>,
    speech_models: Vec<String>,
}

/// A lista de vozes guardada: a conexão (id e endereço), quando foi lida e o que ela oferece.
#[derive(Debug, Default)]
pub struct VoicesCache {
    entry: Mutex<Option<(String, String, Instant, VoiceOptions)>>,
}

impl VoicesCache {
    fn get(&self, id: &str, endpoint: &str) -> Option<VoiceOptions> {
        let guard = self.entry.lock().unwrap_or_else(PoisonError::into_inner);
        let (cid, cend, at, voices) = guard.as_ref()?;
        (cid == id && cend == endpoint && at.elapsed() < VOICES_TTL).then(|| voices.clone())
    }

    fn put(&self, id: &str, endpoint: &str, voices: VoiceOptions) {
        *self.entry.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((id.to_string(), endpoint.to_string(), Instant::now(), voices));
    }
}

/// Consulta de `GET /api/nanty/voices`: sem `connection_id`, a conexão gravada; com ele, a pedida, e
/// vazio é a automática (a mesma regra de [`voice_connection`]), para o rascunho dos Ajustes.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoicesQuery {
    #[serde(default)]
    connection_id: Option<String>,
}

/// A conexão que a tela vai usar para a voz, com a chave: o que decide o estado dela (pronta, sem
/// chave, sem conexão) nos Ajustes (T09). Na automática, um Gemini cadastrado sem chave é "sem
/// chave", não "sem conexão": a pessoa precisa saber o que falta.
fn voice_ready<'a>(cfg: &'a Config, id: &str) -> Result<&'a Connection, ApiError> {
    let no_key = || ApiError::coded(StatusCode::CONFLICT, "nanty_voice_no_key", Map::new());
    match voice_connection(cfg, id) {
        Ok(conn) if conn.has_usable_key() => Ok(conn),
        Ok(_) => Err(no_key()),
        Err("no_connection") if cfg.connections.iter().any(|c| voice_capable(c.kind)) => Err(no_key()),
        Err(_) => Err(ApiError::coded(StatusCode::CONFLICT, "nanty_voice_no_connection", Map::new())),
    }
}

/// `GET /api/nanty/voices` (spec 139 T07, para os Ajustes da T09): as vozes do `/v1beta/voices`
/// da conexão do Gemini (a gravada, ou a do `?connection_id=`, para a pessoa ver antes de gravar; o
/// valor vazio é a automática) e os modelos de fala dela (spec 139 T12; se só essa lista falhar, ela
/// vem vazia e as vozes seguem), com cache curto em memória. Sem conexão do Gemini: 409 `nanty_voice_no_connection`; sem chave:
/// 409 `nanty_voice_no_key`; o provedor falhou: 502 `nanty_voice_list_failed` com o motivo e a
/// conexão em `params` (a tela continua usável, com a lista de modelos dela). Nunca mostra a chave
/// nem o texto do provedor.
pub async fn get_voices(State(state): State<AppState>, query: Option<Query<VoicesQuery>>) -> Result<Json<Value>, ApiError> {
    let query = query.map(|Query(q)| q).unwrap_or_default();
    let cfg = state.config().read().await.clone();
    let wanted = query.connection_id.unwrap_or_else(|| cfg.nanty.voice.connection_id.clone());
    let conn = voice_ready(&cfg, &wanted)?.clone();
    let endpoint = conn.endpoint().to_string();
    let cache = &state.nanty().voices;
    let options = match cache.get(&conn.id, &endpoint) {
        Some(options) => options,
        None => {
            let voices = provider::gemini_voices(&conn).await.map_err(|code| match code {
                "no_key" => ApiError::coded(StatusCode::CONFLICT, "nanty_voice_no_key", Map::new()),
                other => {
                    let mut params = param("reason", other);
                    params.insert("connection_id".into(), json!(conn.id));
                    ApiError::coded(StatusCode::BAD_GATEWAY, "nanty_voice_list_failed", params)
                }
            })?;
            let speech_models = provider::gemini_speech_models(&conn).await.unwrap_or_default();
            let options = VoiceOptions { voices, speech_models };
            cache.put(&conn.id, &endpoint, options.clone());
            options
        }
    };
    Ok(Json(json!({
        "connection_id": conn.id,
        "voice": cfg.nanty.voice.voice_name(),
        "voices": options.voices,
        "speech_models": options.speech_models,
    })))
}

// ---- Ouvir ----

/// Por que a nota de voz não virou texto (o código da resposta fixa).
fn refusal(reason: &str) -> &'static str {
    match reason {
        "off" => "nanty.voice.off",
        "too_long" => "nanty.voice.too_long",
        "too_big" => "nanty.voice.too_big",
        "no_transcriber" => "nanty.voice.no_transcriber",
        "budget" => "nanty.voice.budget",
        _ => "nanty.voice.not_understood",
    }
}

/// Baixa e transcreve a nota de voz. `Ok((texto, áudio))` ou o motivo em código.
async fn listen(state: &AppState, nanty: &Nanty, cfg: &Config, note: &VoiceNote) -> Result<(String, Vec<u8>), &'static str> {
    let voice = &cfg.nanty.voice;
    if !voice.enabled {
        return Err("off");
    }
    if note.duration > voice.max_input_secs {
        return Err("too_long");
    }
    if note.file_size.is_some_and(|s| s > MAX_INPUT_BYTES) {
        return Err("too_big");
    }
    let Some((conn, model)) = transcriber(cfg) else { return Err("no_transcriber") };
    if exhausted(cfg, nanty) {
        return Err("budget");
    }
    let token = cfg.nanty.telegram.resolve_token().filter(|t| super::telegram::valid_token(t)).ok_or("telegram")?;
    let client = TelegramClient::new(state.telegram_api(), token, Duration::ZERO).map_err(|e| e.code())?;
    let file = client.get_file(&note.file_id).await.map_err(|e| e.code())?;
    if file.file_size.is_some_and(|s| s > MAX_INPUT_BYTES) {
        return Err("too_big");
    }
    let path = file.file_path.ok_or("bad_response")?;
    let audio = client.download(&path, MAX_INPUT_BYTES).await.map_err(|e| e.code())?;
    nanty.set_busy(Some(Avatar::Thinking));
    let thinking = Speaking(nanty);
    let prompt = crate::agents::prompts::nanty_transcribe_prompt();
    let heard = provider::gemini_transcribe(&conn, &model, &prompt, &audio).await;
    drop(thinking);
    let heard = heard?;
    let output = heard.output_tokens.unwrap_or_else(|| estimate_tokens(&heard.text));
    let input = heard.input_tokens.unwrap_or(note.duration.saturating_mul(AUDIO_TOKENS_PER_SEC));
    charge(cfg, nanty, &model, input, output);
    Ok((heard.text, audio))
}

/// A nota de voz do dono (ao vivo, já filtrada pela escuta): baixa, transcreve e segue como texto
/// pela fila única da T08, com a marca `voice`. Quando não dá, a mensagem da pessoa entra na
/// conversa sem texto e o Nanty responde no Telegram com o motivo (texto fixo).
pub async fn hear(state: &AppState, note: &VoiceNote, relayed: Option<crate::server::nanty::store::Relayed>) {
    let nanty = Arc::clone(state.nanty());
    let cfg = state.config().read().await.clone();
    let secrets = SecretSet::from_config(&cfg);
    let heard = listen(state, &nanty, &cfg, note).await;
    let (text, audio_bytes, reason) = match heard {
        Ok((text, audio)) => (Some(text), Some(audio), None),
        Err(reason) => (None, None, Some(reason)),
    };
    let audio = match audio_bytes {
        Some(bytes) => {
            let n = Arc::clone(&nanty);
            let secs = u32::try_from(note.duration).unwrap_or(u32::MAX);
            tokio::task::spawn_blocking(move || save_audio(&n, &bytes, secs)).await.ok().flatten()
        }
        None => None,
    };
    let n = Arc::clone(&nanty);
    let nanty_cfg = cfg.nanty.clone();
    let person = tokio::task::spawn_blocking(move || {
        super::chat::person_message_from(&n, &nanty_cfg, &secrets, Channel::Telegram, text.as_deref(), true, audio, relayed)
    })
    .await
    .ok()
    .flatten();
    let Some(person) = person else { return };
    match reason {
        None => {
            nanty.record(&journal(&nanty, "voice.heard", param("secs", note.duration)));
            super::chat::reply(state, &person).await;
        }
        Some(reason) => {
            nanty.record(&journal(&nanty, "voice.listen_failed", param("reason", reason)));
            let code = refusal(reason);
            let params = match code {
                "nanty.voice.too_long" => param("max", cfg.nanty.voice.max_input_secs),
                "nanty.voice.too_big" => param("max", MAX_INPUT_BYTES / (1024 * 1024)),
                _ => Map::new(),
            };
            let mut message = ConversationMessage::from_nanty(super::new_id(), nanty.now().epoch_ms, Channel::Telegram, code, params);
            message.reply_to = Some(person.id.clone());
            // A nota sem texto não diz o idioma: vale o resolvido para ela (T05).
            message.lang = person.lang.clone();
            // Spec 139 T08: a recusa também é fala do Nanty.
            deliver(state, &nanty, &mut message, true).await;
        }
    }
}

/// Ouve numa tarefa à parte (a escuta segue enquanto baixa e transcreve).
pub fn spawn_hear(state: &AppState, note: VoiceNote, relayed: Option<crate::server::nanty::store::Relayed>) {
    let state = state.clone();
    tokio::spawn(async move {
        hear(&state, &note, relayed).await;
    });
}

#[cfg(test)]
pub(crate) mod tests;
