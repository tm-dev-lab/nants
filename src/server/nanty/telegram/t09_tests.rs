//! Testes da voz pelo Telegram (spec 138 T09), só com o servidor falso do Telegram de
//! [`super::tests`] e os provedores de voz simulados de `voice::tests` (nunca o Telegram real nem
//! chave real): a nota de voz do dono baixada, transcrita e respondida em voz; a legenda e o texto à
//! parte; a recusa do `sendVoice` que cai para o texto; os limites de duração e de tamanho antes do
//! download; o fim do orçamento; a nota de um estranho; e o download que nunca mostra o token.

use std::time::Duration;

use serde_json::{json, Value};

use super::client::TelegramClient;
use super::tests::{fake_server, live_after, lock, paired, token, until, Fake, Scripted, World, OWNER, STRANGER};
use super::*;
use crate::config::Config;
use crate::server::nanty::{clock, render};
use crate::server::nanty::store::{ConversationMessage, Speaker};
use crate::server::nanty::voice::tests::{fake_voice, gemini_key, small_ogg, voice_config, FakeVoice, Reply};
use crate::server::nanty::voice::{self, MAX_INPUT_BYTES};

const FILE_ID: &str = "voz-1";
const FILE_PATH: &str = "voice/file_1.oga";

fn start_outbox(w: &World) -> tokio::task::JoinHandle<()> {
    tokio::spawn(outbox::run(w.state.clone(), w.state.shutdown_signal()))
}

fn voice_update(update_id: i64, from: i64, duration: u64, size: Option<u64>) -> Value {
    let date = i64::try_from(clock::now_ms() / 1000).unwrap_or(0);
    let mut voice = json!({"file_id": FILE_ID, "file_unique_id": "u1", "duration": duration, "mime_type": "audio/ogg"});
    if let Some(size) = size {
        voice["file_size"] = json!(size);
    }
    json!({"update_id": update_id, "message": {
        "message_id": update_id, "date": date,
        "chat": {"id": from, "type": "private"},
        "from": {"id": from, "is_bot": false},
        "voice": voice,
    }})
}

/// Mundo pareado com a voz ligada (Gemini e OpenAI simulados), a escuta e a fila de saída.
async fn world(tag: &str, edit: impl FnOnce(&mut Config)) -> (World, std::sync::Arc<Fake>, std::sync::Arc<FakeVoice>) {
    let tk = token(tag);
    let (fake, base) = fake_server(&tk).await;
    let (voice_fake, voice_base) = fake_voice().await;
    let ogg = small_ogg();
    lock(&fake.files).insert(FILE_ID.into(), (Some(ogg.len() as u64), FILE_PATH.into()));
    lock(&fake.downloads).insert(FILE_PATH.into(), ogg);
    let mut w = World::with_token(tag, &base, &tk, |cfg| {
        paired(cfg);
        voice_config(cfg, &voice_base);
        edit(cfg);
    });
    w.start();
    until("conectado", || w.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    (w, fake, voice_fake)
}

fn sends(fake: &Fake, method: &str) -> Vec<Value> {
    fake.calls(method).into_iter().map(|(b, _)| b).collect()
}

fn conversation(w: &World) -> Vec<ConversationMessage> {
    w.state.nanty().conversation()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_voice_note_is_heard_and_answered_in_voice() {
    let (w, fake, voice_fake) = world("t09-hear", |_| {}).await;
    let _out = start_outbox(&w);
    // A transcrição é um comando com um segredo junto: o texto entra redigido e segue pela fila única.
    lock(&voice_fake.transcribe).push_back(Reply::Text(format!("/status {}", gemini_key())));
    fake.push(voice_update(80, OWNER, 3, Some(1000)));
    until("nota de voz enviada", || !fake.calls("sendVoice").is_empty()).await;

    // `getFile` e o download do caminho que ele deu.
    assert_eq!(sends(&fake, "getFile")[0]["file_id"], FILE_ID);
    assert_eq!(sends(&fake, "download")[0]["path"], FILE_PATH);
    // A pessoa na conversa: a transcrição redigida, com a marca da voz e o áudio dela.
    let conv = conversation(&w);
    let person = conv.iter().find(|m| m.from == Speaker::Person).cloned().expect("pessoa");
    assert_eq!((person.channel, person.voice, person.text.as_deref()), (Channel::Telegram, true, Some("/status ***")));
    assert!(person.audio.is_some(), "a nota da pessoa fica para a janela");
    // A resposta: o status, em voz, com a legenda inteira (curta: sem mensagem à parte).
    let reply = conv.iter().find(|m| m.from == Speaker::Nanty).cloned().expect("resposta");
    assert_eq!(reply.reply_to.as_deref(), Some(person.id.as_str()));
    assert!(reply.audio.is_some(), "a resposta tem a fala");
    let voice_call = &sends(&fake, "sendVoice")[0];
    let expected = render::message_text(Lang::PtBr, &reply);
    assert_eq!(voice_call["caption"], json!(expected));
    assert_eq!(voice_call["chat_id"], json!(OWNER.to_string()));
    assert!(voice_call["voice_bytes"].as_u64().unwrap_or(0) > 100, "{voice_call}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(fake.sent_texts().is_empty(), "a legenda levou o texto inteiro");
    // O Gemini falou (o primeiro da ordem) e o gasto contou a transcrição e a fala.
    assert_eq!(voice_fake.calls("gemini").len(), 1);
    assert!(voice_fake.calls("openai").is_empty());
    let spend = w.state.nanty().snapshot().spend_on(&w.state.nanty().now().day_key());
    assert_eq!(spend.voice_calls, 2);
    // Os dois áudios são servidos à janela.
    for audio in [person.audio, reply.audio].into_iter().flatten() {
        assert!(w.state.nanty().read_audio(&audio.id).is_some(), "{}", audio.id);
    }
    // Nada do texto, da chave ou do token no registro.
    let journal = serde_json::to_string(&w.state.nanty().journal_tail(1000)).expect("json");
    assert!(!journal.contains(&gemini_key()) && !journal.contains("status ***"), "{journal}");
}

/// Uma fala do Nanty com áudio, como resposta a quem escreveu no Telegram.
fn spoken_reply(w: &World, text: &str) -> ConversationMessage {
    let nanty = w.state.nanty();
    let audio = voice::save_audio(nanty, &small_ogg(), 1).expect("áudio");
    let mut m = ConversationMessage::from_nanty(crate::server::nanty::new_id(), clock::now_ms(), Channel::Telegram, "x", Map::new());
    m.code = None;
    m.text = Some(text.to_string());
    m.reply_to = Some("0123456789abcdef".into());
    m.audio = Some(audio);
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_text_goes_as_a_short_caption_and_the_full_text_apart() {
    let (w, fake, _voice) = world("t09-caption", |_| {}).await;
    let _out = start_outbox(&w);
    let long = "Projeto Loja segue bem. ".repeat(80);
    w.state.nanty().push_message(&spoken_reply(&w, &long));
    until("texto à parte", || !fake.sent_texts().is_empty()).await;
    let caption = sends(&fake, "sendVoice")[0]["caption"].as_str().unwrap_or_default().to_string();
    assert!(caption.encode_utf16().count() <= 1024 && caption.ends_with('…'), "{}", caption.len());
    assert_eq!(fake.sent_texts(), [long.trim().to_string()], "o texto completo, redigido como sempre");
    // As duas saídas respeitam o intervalo da fila (uma chamada por vez).
    let voice_at = fake.calls("sendVoice")[0].1;
    let text_at = fake.calls("sendMessage")[0].1;
    assert!(text_at.duration_since(voice_at) >= super::tests::FAST.send_gap, "intervalo da fila");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_voice_note_falls_back_to_the_text() {
    let (w, fake, _voice) = world("t09-refused", |_| {}).await;
    let _out = start_outbox(&w);
    lock(&fake.voice_script).push_back(Scripted::Status(400));
    let secret = gemini_key();
    w.state.nanty().push_message(&spoken_reply(&w, &format!("Tudo calmo. {secret}")));
    until("texto", || !fake.sent_texts().is_empty()).await;
    assert_eq!(fake.calls("sendVoice").len(), 1);
    assert_eq!(fake.sent_texts(), ["Tudo calmo. ***"], "o aviso não se perde e sai redigido");
    let failed = w.journal("telegram.send_failed");
    assert_eq!(failed[0]["method"], "sendVoice");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_voice_failing_answers_in_text() {
    let (w, fake, voice_fake) = world("t09-text", |_| {}).await;
    let _out = start_outbox(&w);
    lock(&voice_fake.gemini).push_back(Reply::Status(500));
    lock(&voice_fake.openai).push_back(Reply::Status(503));
    fake.push(voice_update(81, OWNER, 2, None));
    until("resposta em texto", || !fake.sent_texts().is_empty()).await;
    assert!(fake.calls("sendVoice").is_empty());
    let reply = conversation(&w).into_iter().find(|m| m.from == Speaker::Nanty).expect("resposta");
    assert!(reply.audio.is_none());
    assert_eq!(fake.sent_texts(), [render::message_text(Lang::PtBr, &reply)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duration_and_size_are_checked_before_the_download() {
    let (w, fake, voice_fake) = world("t09-limits", |cfg| cfg.nanty.voice.max_input_secs = 60).await;
    let _out = start_outbox(&w);
    // Longa demais: nem o `getFile`.
    fake.push(voice_update(90, OWNER, 61, Some(1000)));
    until("recusa da duração", || fake.sent_texts().len() == 1).await;
    assert_eq!(fake.sent_texts()[0], tr(Lang::PtBr, "nanty.voice.too_long", &[("max", "60")]));
    // Acima de 20 MB pelo que a mensagem informa: nem o `getFile`.
    fake.push(voice_update(91, OWNER, 5, Some(MAX_INPUT_BYTES + 1)));
    until("recusa do tamanho", || fake.sent_texts().len() == 2).await;
    assert_eq!(fake.sent_texts()[1], tr(Lang::PtBr, "nanty.voice.too_big", &[("max", "20")]));
    assert!(fake.calls("getFile").is_empty());
    // Sem tamanho na mensagem, mas o `getFile` diz que passa: sem download.
    lock(&fake.files).insert(FILE_ID.into(), (Some(MAX_INPUT_BYTES + 1), FILE_PATH.into()));
    fake.push(voice_update(92, OWNER, 5, None));
    until("recusa no getFile", || fake.sent_texts().len() == 3).await;
    assert_eq!(fake.calls("getFile").len(), 1);
    assert!(fake.calls("download").is_empty(), "nada baixado");
    assert!(voice_fake.calls("transcribe").is_empty());
    // As notas recusadas entram na conversa sem texto, com a marca, e o motivo vai ao registro.
    let notes: Vec<ConversationMessage> = conversation(&w).into_iter().filter(|m| m.from == Speaker::Person).collect();
    assert!(notes.len() == 3 && notes.iter().all(|m| m.voice && m.text.is_none() && m.audio.is_none()));
    let reasons: Vec<Value> = w
        .state
        .nanty()
        .journal_tail(1000)
        .into_iter()
        .filter(|e| e.code == "voice.listen_failed")
        .filter_map(|e| e.params.get("reason").cloned())
        .collect();
    assert_eq!(reasons, [json!("too_long"), json!("too_big"), json!("too_big")]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_budget_no_listening_and_strangers_are_ignored() {
    let (w, fake, voice_fake) = world("t09-budget", |_| {}).await;
    let _out = start_outbox(&w);
    let day = w.state.nanty().now().day_key();
    let limit = w.state.config().read().await.nanty.daily_budget_tokens;
    w.state.nanty().update(|st| {
        st.add_spend(&day, limit, 0, None);
        true
    });
    fake.push(voice_update(95, OWNER, 3, Some(1000)));
    until("recusa do orçamento", || fake.sent_texts().len() == 1).await;
    assert_eq!(fake.sent_texts()[0], tr(Lang::PtBr, "nanty.voice.budget", &[]));
    assert!(fake.calls("getFile").is_empty() && voice_fake.calls("transcribe").is_empty());
    // A nota de um estranho nem é baixada.
    fake.push(voice_update(96, STRANGER, 3, Some(1000)));
    until("estranho", || w.journal("telegram.stranger").len() == 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fake.calls("getFile").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn voice_off_answers_in_text_without_downloading() {
    let (w, fake, _voice) = world("t09-off", |cfg| cfg.nanty.voice.enabled = false).await;
    let _out = start_outbox(&w);
    fake.push(voice_update(97, OWNER, 3, Some(1000)));
    until("recusa", || fake.sent_texts().len() == 1).await;
    assert_eq!(fake.sent_texts()[0], tr(Lang::PtBr, "nanty.voice.off", &[]));
    assert!(fake.calls("getFile").is_empty());
}

#[tokio::test]
async fn the_download_never_shows_the_token() {
    let tk = token("t09-dl");
    let (fake, base) = fake_server(&tk).await;
    lock(&fake.downloads).insert(FILE_PATH.into(), vec![7u8; 64]);
    let client = TelegramClient::new(&base, tk.clone(), Duration::ZERO).expect("cliente");
    assert_eq!(client.download(FILE_PATH, 1000).await.expect("baixou"), vec![7u8; 64]);
    // Acima do teto e caminho fora da forma: recusados.
    assert!(client.download(FILE_PATH, 10).await.is_err());
    for bad in ["../config.toml", "/etc/passwd", "voice//x", "voice/a b"] {
        assert!(client.download(bad, 1000).await.is_err(), "{bad}");
    }
    assert!(lock(&fake.calls).iter().filter(|(m, _, _)| m == "download").count() == 2, "os caminhos ruins nem saem");
    // Rede caída: o erro não leva o token nem a URL.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("porta");
    let dead = format!("http://{}", listener.local_addr().expect("endereço"));
    drop(listener);
    let client = TelegramClient::new(&dead, tk.clone(), Duration::ZERO).expect("cliente");
    let err = client.download(FILE_PATH, 1000).await.expect_err("sem rede").to_string();
    assert!(!err.contains(&tk) && !err.contains("file/bot"), "{err}");
    let err = client.send_voice(1, small_ogg(), "oi", 1, false).await.expect_err("sem rede").to_string();
    assert!(!err.contains(&tk) && !err.contains("sendVoice"), "{err}");
}
