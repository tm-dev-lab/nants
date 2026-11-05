//! A resposta longa (spec 140 T14, C-27), só com o servidor falso do Telegram, o Gemini simulado
//! (com a fala presa até a liberação explícita) e o modelo simulado; nunca o Telegram real nem chave
//! real. A conversa grava o texto inteiro; o Telegram o divide em até três partes, com o teclado na
//! última e nenhuma frase fixa depois do texto do modelo (spec 141 T10, C-10); com a voz ligada (spec 141 T08, C-08), a nota fala o texto inteiro e
//! sai sem legenda, e as partes trazem o texto inteiro respondendo a ela; a janela mostra tudo.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::tests::{click_on, fake_server, live_after, lock, paired, temp, token, until, Fake, Scripted as Tg, World, FAST, OWNER};
use super::voice_tests::{bodies, datas, start_outbox};
use super::*;
use crate::agents::chat::ChatBackend;
use crate::config::Config;
use crate::i18n::tr_plural;
use crate::server::events::SecretSet;
use crate::server::nanty::agent::tests::{call, say, Scripted};
use crate::server::nanty::chat::tests::base_config;
use crate::server::nanty::chat::{self as conversation};
use crate::server::nanty::store::{ConversationMessage, Speaker};
use crate::server::nanty::{clock, memory, new_id, render, voice};

const BASE: &str = "http://192.168.0.68:3000";

/// O primeiro parágrafo: o essencial, curto.
const LEAD: &str = "Eu separaria a fila do armazenamento antes de crescer. O risco maior hoje é o disco cheio travar tudo.";

/// Um parágrafo de detalhe com cerca de 950 caracteres.
fn detail(n: usize) -> String {
    format!("Ponto {n}: {}", "a fila grava cada passo no disco local e depende da rede para avisar os outros. ".repeat(12).trim_end())
}

/// A resposta longa: o essencial e `paragraphs` parágrafos de detalhe, separados por linha em branco.
fn long_reply(paragraphs: usize) -> String {
    let mut parts = vec![LEAD.to_string()];
    parts.extend((1..=paragraphs).map(detail));
    parts.join("\n\n")
}

fn owner_text(update_id: i64, text: &str) -> Value {
    let date = i64::try_from(clock::now_ms() / 1000).unwrap_or(0);
    json!({"update_id": update_id, "message": {
        "message_id": update_id, "date": date,
        "chat": {"id": OWNER, "type": "private"},
        "from": {"id": OWNER, "is_bot": false},
        "text": text,
    }})
}

/// Pareado, sem voz, com a escuta ligada e o modelo simulado.
async fn world(tag: &str, model: Arc<Scripted>, edit: impl FnOnce(&mut Config, &PathBuf)) -> (World, Arc<Fake>) {
    let tk = token(tag);
    let (fake, base) = fake_server(&tk).await;
    let dir = temp(tag);
    let mut cfg = base_config(&dir, &[("p1", "Loja")]);
    cfg.nanty.telegram.bot_token = tk;
    paired(&mut cfg);
    edit(&mut cfg, &dir);
    let state = AppState::new(dir.join("config.toml"), cfg, 3000)
        .with_data_dir(dir.join("data"))
        .with_telegram_api(&base)
        .with_nanty_clock(clock::test_clock());
    assert_eq!(state.nanty().acquire(), crate::server::nanty::InstanceStatus::Active);
    state.nanty().telegram().set_tuning(FAST);
    state.nanty().set_backend(model as Arc<dyn ChatBackend>);
    let mut w = World { dir, state, listener: None };
    w.start();
    until("conectado", || w.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    (w, fake)
}

/// Com a voz ligada no Gemini simulado e o modelo simulado.
async fn voice_world(tag: &str, model: Arc<Scripted>, edit: impl FnOnce(&mut Config)) -> (World, Arc<Fake>, Arc<voice::tests::FakeVoice>) {
    let (w, fake, voice_fake) = super::voice_tests::world(tag, |c| {
        c.nanty.connection_id = "oai".into();
        c.nanty.model = "modelo".into();
        edit(c);
    })
    .await;
    w.state.nanty().set_backend(model as Arc<dyn ChatBackend>);
    (w, fake, voice_fake)
}

fn remember(title: &str) -> Value {
    json!({ "topic": "system", "title": title, "text": "O disco enche com os builds." })
}

fn sends(fake: &Fake) -> Vec<Value> {
    bodies(fake, "sendMessage")
}

fn text_of(body: &Value) -> String {
    body["text"].as_str().unwrap_or_default().to_string()
}

/// A resposta do Nanty gravada na conversa.
fn stored_reply(w: &World) -> ConversationMessage {
    w.state.nanty().conversation().into_iter().rev().find(|m| m.from == Speaker::Nanty && m.reply_to.is_some()).expect("resposta")
}

async fn person(w: &World, channel: Channel, text: &str) -> ConversationMessage {
    let cfg = w.state.config().read().await.clone();
    conversation::person_message(w.state.nanty(), &cfg.nanty, &SecretSet::from_config(&cfg), channel, text).expect("mensagem")
}

/// As palavras do texto, para conferir que nada se partiu nem se perdeu.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

// ---- Sem voz, no Telegram ----

/// Sem voz: a conversa grava a resposta inteira; o Telegram a divide em três partes, a última termina
/// com a marca do corte, e leva o teclado (o link e o botão de
/// apagar). O botão de apagar edita essa mensagem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_voice_a_long_reply_goes_in_three_parts_with_the_keyboard_on_the_last() {
    let huge = long_reply(16);
    let model = Scripted::new(vec![call("open_nants", json!({})), call("remember", remember("Disco")), say(&huge)]);
    let (w, fake) = world("t14-text", model, |c, _| c.nanty.address = BASE.into()).await;
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "como você faria a fila crescer?"));
    until("as três partes", || sends(&fake).len() == format::MAX_PARTS).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let all = sends(&fake);
    assert_eq!(all.len(), format::MAX_PARTS, "teto de três mensagens");

    // A conversa (e a janela) tem o texto inteiro, limpo e sem corte.
    let stored = stored_reply(&w);
    let whole = stored.text.clone().unwrap_or_default();
    assert_eq!(whole, format::clean_whole(&SecretSet::default(), &huge));
    assert!(format::units(&whole) > format::MAX_PARTS * format::MAX_MESSAGE_CHARS && !whole.contains('…'));

    let texts: Vec<String> = all.iter().map(text_of).collect();
    assert!(texts.iter().all(|t| format::units(t) <= format::MAX_MESSAGE_CHARS));
    assert!(texts[0].starts_with(LEAD), "em ordem: {}", texts[0].chars().take(80).collect::<String>());
    // O teclado só na última.
    assert!(all[..2].iter().all(|b| b.get("reply_markup").is_none()), "{:?}", &all[..2]);
    let rows = all[2]["reply_markup"]["inline_keyboard"].as_array().cloned().unwrap_or_default();
    assert_eq!(rows[0][0]["url"], json!(format!("{BASE}/")), "o link do Nants");
    let forget: Vec<Card> = w.state.nanty().snapshot().cards.into_iter().filter(|c| c.action.as_deref() == Some(memory::card::ACTION_FORGET)).collect();
    assert_eq!(forget.len(), 1);
    let token = forget[0].token.clone().unwrap_or_default();
    assert_eq!(datas(&all[2]["reply_markup"]), [callback_data(&forget[0].id, &token, Answer::Yes)]);
    // Spec 141 T10 (C-10): a última parte termina com a marca do corte, sem frase fixa depois.
    assert!(texts[2].ends_with('…'), "{}", texts[2]);
    // Nada partido: as palavras das partes são o começo das palavras da resposta, na ordem.
    let sent: Vec<String> = texts.iter().flat_map(|t| words(t)).collect();
    let body = &sent[..sent.len() - 1];
    assert_eq!(body, &words(&whole)[..body.len()]);

    // A edição de depois mira a mensagem que tem o teclado: o toque tira só o botão de apagar.
    let rec = w.state.nanty().snapshot().telegram_sent.into_iter().find(|r| r.embedded).expect("anotada");
    assert_eq!(rec.message_id, Some(3), "a última parte");
    fake.push(click_on(51, OWNER, OWNER, 3, &callback_data(&forget[0].id, &token, Answer::Yes)));
    until("o botão some", || !bodies(&fake, "editMessageReplyMarkup").is_empty()).await;
    let edit = &bodies(&fake, "editMessageReplyMarkup")[0];
    assert_eq!(edit["message_id"], json!(3));
    assert_eq!(edit["reply_markup"]["inline_keyboard"][0][0]["url"], json!(format!("{BASE}/")), "o link fica: {edit}");
}

/// A rajada continua valendo: a resposta longa com o botão sai sozinha, as partes em seguida e na
/// ordem, e só depois o resumo da fila com o resto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_a_burst_the_long_reply_goes_whole_and_in_order_before_the_summary() {
    let (w, fake) = world("t14-burst", Scripted::new(Vec::new()), |_, _| {}).await;
    let nanty = Arc::clone(w.state.nanty());
    let cfg = w.state.config().read().await.clone();
    for i in 0..=outbox::MAX_BURST {
        let mut m = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Web, "x", Map::new());
        m.code = None;
        m.text = Some(format!("fala {i}"));
        nanty.push_message(&m);
    }
    let card = forget_button(&nanty, &cfg);
    let long = long_reply(6);
    let mut m = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Telegram, "nanty.chat.memory_saved", Map::new());
    m.params.insert("title".into(), json!("Disco"));
    m.text = Some(long.clone());
    m.reply_to = Some("0123456789abcdef".into());
    m.card_ids = vec![card];
    nanty.push_message(&m);
    let _out = start_outbox(&w);
    until("as partes e o resumo", || sends(&fake).len() == 3).await;
    let all = sends(&fake);
    assert!(text_of(&all[0]).starts_with(LEAD) && all[0].get("reply_markup").is_none());
    let saved = tr(Lang::PtBr, "nanty.chat.memory_saved", &[("title", "Disco")]);
    assert!(!text_of(&all[1]).contains(&saved) && long.trim_end().ends_with(text_of(&all[1]).trim_end()), "o texto do modelo fecha: {}", text_of(&all[1]));
    assert_eq!(datas(&all[1]["reply_markup"]).len(), 1, "o botão na última parte");
    let batch = tr_plural(Lang::PtBr, "nanty.telegram.batch", u64::try_from(outbox::MAX_BURST + 1).unwrap_or(0), &[]);
    assert!(text_of(&all[2]).starts_with(&batch), "o resumo da fila depois: {}", text_of(&all[2]));
}

fn forget_button(nanty: &Arc<crate::server::nanty::Nanty>, cfg: &Config) -> String {
    let remember = memory::ops::Remember { base_updated: None, topic: memory::Topic::System, title: "Disco".into(), text: "x".into(), replace_id: None, project_id: None };
    let source = memory::Source { kind: memory::SourceKind::ModelClean, message_id: None, channel: Some(Channel::Telegram), project_id: None, at_ms: nanty.now().epoch_ms };
    let entry = memory::ops::save(nanty, cfg, &remember, source).expect("grava");
    memory::card::forget_button(nanty, cfg, &entry, Channel::Telegram)
}

/// O Telegram recusa o teclado da última parte: ela sai de novo sem ele, inteira, com a linha da aba
/// Memória, e nada fica preso na fila.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_keyboard_on_the_last_part_sends_it_again_whole() {
    let (w, fake) = world("t14-refused", Scripted::new(Vec::new()), |_, _| {}).await;
    w.state.nanty().telegram().set_tuning(Tuning { send_gap: Duration::from_millis(400), ..FAST });
    let nanty = Arc::clone(w.state.nanty());
    let cfg = w.state.config().read().await.clone();
    let card = forget_button(&nanty, &cfg);
    let mut m = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Telegram, "nanty.chat.memory_saved", Map::new());
    m.params.insert("title".into(), json!("Disco"));
    m.text = Some(long_reply(6));
    m.reply_to = Some("0123456789abcdef".into());
    m.card_ids = vec![card];
    nanty.push_message(&m);
    let _out = start_outbox(&w);
    until("a primeira parte", || sends(&fake).len() == 1).await;
    lock(&fake.send_script).push_back(Tg::Status(400));
    until("de novo sem o teclado", || sends(&fake).len() == 3).await;
    let all = sends(&fake);
    assert_eq!(datas(&all[1]["reply_markup"]).len(), 1, "a última parte levou o botão");
    assert!(all[2].get("reply_markup").is_none());
    let (refused, again) = (text_of(&all[1]), text_of(&all[2]));
    let line = tr(Lang::PtBr, "nanty.telegram.forget_in_window", &[]);
    assert_eq!(again, format!("{refused}\n{line}"), "a parte inteira, sem corte");
    assert!(format::units(&again) <= format::MAX_MESSAGE_CHARS);
    assert_eq!(w.state.nanty().telegram().outbox().queued(), 0, "nada preso");
}

// ---- Com voz, no Telegram e na janela ----

/// O que o Gemini simulado falou, na ordem dos trechos.
fn spoken_texts(voice_fake: &voice::tests::FakeVoice) -> Vec<String> {
    voice_fake.calls("gemini").iter().map(|c| c["input"][0]["content"][0]["text"].as_str().unwrap_or_default().to_string()).collect()
}

/// A resposta como vai ao Telegram e à fala: o texto redigido.
fn whole_of(message: &ConversationMessage) -> String {
    format::clean_whole(&SecretSet::default(), &render::message_text(Lang::PtBr, message))
}

/// Spec 141 T08 (C-08), com a voz ligada no Telegram: a conversa recebe o texto inteiro antes da fala;
/// o áudio fala o texto inteiro, em trechos, numa nota só, sem legenda e sem teclado, porque o texto
/// não cabe na legenda; as partes trazem o texto inteiro, uma vez, respondendo à nota, e a última leva
/// o botão.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_voice_the_note_speaks_everything_and_the_text_answers_it() {
    let long = long_reply(6);
    let model = Scripted::new(vec![call("remember", remember("Disco")), say(&long)]);
    let (w, fake, voice_fake) = voice_world("t14-voice", model, |_| {}).await;
    let _out = start_outbox(&w);
    voice_fake.hold_speech();
    let asked = person(&w, Channel::Telegram, "como você faria a fila crescer?").await;
    let state = w.state.clone();
    let replying = tokio::spawn(async move { conversation::reply(&state, &asked).await });
    until("a fala pedida", || voice_fake.calls("gemini").len() == 1).await;
    // O texto inteiro já está na conversa, antes do áudio; nada saiu ao Telegram ainda.
    let stored = stored_reply(&w);
    assert_eq!(stored.text.as_deref(), Some(long.as_str()));
    assert!(stored.audio.is_none() && bodies(&fake, "sendVoice").is_empty());
    voice_fake.release_speech(voice::MAX_CHUNKS);
    let _ = replying.await;
    until("as partes", || sends(&fake).len() == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // A fala: o texto inteiro, em trechos que somam tudo.
    let whole = whole_of(&stored_reply(&w));
    let said = spoken_texts(&voice_fake);
    assert_eq!(said.len(), voice::speech_chunks(&whole, voice::CHUNK_CHARS).len());
    assert!(said.len() > 1 && said.iter().all(|t| t.chars().count() <= voice::CHUNK_CHARS));
    assert_eq!(words(&said.join(" ")), words(&whole), "a fala é o texto inteiro");

    // Uma nota só, sem legenda e sem teclado.
    let note = bodies(&fake, "sendVoice");
    assert_eq!(note.len(), 1);
    assert!(note[0]["caption"].as_str().unwrap_or_default().is_empty(), "sem legenda: {}", note[0]);
    assert!(note[0].get("reply_markup").is_none(), "o teclado vai na última parte: {}", note[0]);
    // O texto inteiro, uma vez, em partes que respondem à nota (a 1).
    let all = sends(&fake);
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(all.iter().all(|b| b["reply_parameters"]["message_id"] == json!(1)), "{all:?}");
    let texts: Vec<String> = all.iter().map(text_of).collect();
    assert!(texts[0].starts_with(LEAD));
    assert_eq!(words(&texts.join("\n")), words(&whole), "a transcrição completa, uma vez");
    assert!(all[0].get("reply_markup").is_none() && datas(&all[1]["reply_markup"]).len() == 1);
    let stored = stored_reply(&w);
    assert!(whole.trim_end().ends_with(texts[1].trim_end()), "o texto do modelo fecha, sem frase fixa: {}", texts[1]);
    // A janela: a resposta inteira, com o tocador.
    assert!(stored.audio.is_some() && stored.text.as_deref() == Some(long.as_str()));
    let rec = w.state.nanty().snapshot().telegram_sent.into_iter().find(|r| r.embedded).expect("anotada");
    assert_eq!(rec.message_id, Some(3), "a nota é a 1; as partes, 2 e 3");
    let journal = serde_json::to_string(&w.state.nanty().journal_tail(1000)).expect("json");
    assert!(!journal.contains(LEAD), "o registro guarda só códigos");
}

/// Spec 141 T08, revisão 1 (F-01): a resposta que passa do que as três partes mostram não fala (o que
/// a nota fala sempre aparece escrito): nenhuma chamada de fala, nenhuma nota, o texto sai como sem
/// voz, em três partes com a linha da janela, e o motivo `too_long` fica no registro.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_voice_a_reply_past_the_three_parts_goes_as_text_only() {
    let huge = long_reply(16);
    let (w, fake, voice_fake) = voice_world("t14-voice-huge", Scripted::new(vec![say(&huge)]), |_| {}).await;
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Telegram, "como você faria a fila crescer?").await;
    conversation::reply(&w.state, &asked).await.expect("resposta");
    assert!(format::units(&whole_of(&stored_reply(&w))) > voice::MAX_SPOKEN_UNITS);
    until("as três partes", || sends(&fake).len() == format::MAX_PARTS).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(voice_fake.calls("gemini").is_empty(), "nenhuma chamada de fala");
    assert!(bodies(&fake, "sendVoice").is_empty() && stored_reply(&w).audio.is_none());
    let all = sends(&fake);
    assert_eq!(all.len(), format::MAX_PARTS);
    assert!(all.iter().all(|b| b.get("reply_parameters").is_none()));
    assert!(text_of(&all[2]).ends_with('…'), "como hoje: {}", text_of(&all[2]));
    let reasons: Vec<Value> = w.state.nanty().journal_tail(1000).into_iter().filter(|e| e.code == "voice.text").map(|e| e.params["reasons"].clone()).collect();
    assert_eq!(reasons, [json!(["too_long"])]);
}

/// Spec 141 T08, revisão 2 (F-02): abaixo das três partes cheias, mas com parágrafos que levam a
/// divisão real a cortar (sete de 1500 caracteres precisam de quatro partes): nenhuma chamada de fala
/// nem nota, o texto sai como sem voz, com a linha da janela, e `too_long` no registro.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_voice_a_reply_the_split_cuts_goes_as_text_only() {
    let para = |n: usize| format!("Ponto {n}: {}", "a fila grava cada passo no disco local e avisa os outros pela rede. ".repeat(22).trim_end());
    let reply = (1..=7).map(para).collect::<Vec<_>>().join("\n\n");
    let (w, fake, voice_fake) = voice_world("t14-voice-split", Scripted::new(vec![say(&reply)]), |_| {}).await;
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Telegram, "como você faria a fila crescer?").await;
    conversation::reply(&w.state, &asked).await.expect("resposta");
    let whole = whole_of(&stored_reply(&w));
    assert!(format::units(&whole) < voice::MAX_SPOKEN_UNITS, "abaixo do teto de segurança: {}", format::units(&whole));
    assert!(!voice::speakable(&whole));
    until("as três partes", || sends(&fake).len() == format::MAX_PARTS).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(voice_fake.calls("gemini").is_empty(), "nenhuma chamada de fala");
    assert!(bodies(&fake, "sendVoice").is_empty() && stored_reply(&w).audio.is_none());
    let all = sends(&fake);
    assert!(text_of(&all[format::MAX_PARTS - 1]).ends_with('…'), "a divisão real cortou: {}", text_of(&all[2]));
    let reasons: Vec<Value> = w.state.nanty().journal_tail(1000).into_iter().filter(|e| e.code == "voice.text").map(|e| e.params["reasons"].clone()).collect();
    assert_eq!(reasons, [json!(["too_long"])]);
}

/// Spec 141 T08: um trecho da fala falha no meio. Nenhum áudio parcial: nenhuma nota, a mensagem fica
/// sem áudio, o texto inteiro sai em partes (sem responder a nota nenhuma) e o motivo fica no
/// registro.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_chunk_sends_the_whole_text_and_no_audio() {
    let long = long_reply(6);
    let (w, fake, voice_fake) = voice_world("t14-voice-fail", Scripted::new(vec![say(&long)]), |_| {}).await;
    let _out = start_outbox(&w);
    lock(&voice_fake.gemini).push_back(voice::tests::Reply::Raw(voice::tests::wav(24_000, 1, &voice::tests::tone(24_000, 5))));
    lock(&voice_fake.gemini).push_back(voice::tests::Reply::Status(500));
    let asked = person(&w, Channel::Telegram, "como você faria a fila crescer?").await;
    conversation::reply(&w.state, &asked).await.expect("resposta");
    until("as partes", || sends(&fake).len() == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(voice_fake.calls("gemini").len(), 2, "parou no trecho que falhou");
    assert!(bodies(&fake, "sendVoice").is_empty(), "nenhum áudio parcial");
    let stored = stored_reply(&w);
    assert!(stored.audio.is_none());
    let all = sends(&fake);
    assert!(all.iter().all(|b| b.get("reply_parameters").is_none()));
    let texts: Vec<String> = all.iter().map(text_of).collect();
    assert_eq!(words(&texts.join("\n")), words(&whole_of(&stored)), "o texto inteiro");
    let journal = w.state.nanty().journal_tail(1000);
    let failed: Vec<_> = journal.iter().filter(|e| e.code == "voice.failed").collect();
    assert_eq!((failed[0].params["reason"].clone(), failed[0].params["chunk"].clone()), (json!("provider_error"), json!(1)));
    assert_eq!(journal.iter().filter(|e| e.code == "voice.text").count(), 1);
}

/// Spec 141 T08: o prazo vence num trecho. A fala tardia é descartada, e o texto inteiro sai sem áudio.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_chunk_sends_the_whole_text_and_no_audio() {
    let long = long_reply(6);
    let (w, fake, voice_fake) = voice_world("t14-voice-late", Scripted::new(vec![say(&long)]), |_| {}).await;
    w.state.nanty().telegram().set_tuning(Tuning { speech_deadline: Duration::from_millis(200), ..FAST });
    let _out = start_outbox(&w);
    voice_fake.hold_speech();
    let asked = person(&w, Channel::Telegram, "como você faria a fila crescer?").await;
    conversation::reply(&w.state, &asked).await.expect("resposta");
    until("as partes", || sends(&fake).len() == 2).await;
    // Soltas depois do prazo, as falas terminam (e contam no gasto), mas nada sai.
    voice_fake.release_speech(voice::MAX_CHUNKS);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(bodies(&fake, "sendVoice").is_empty(), "nenhum áudio parcial nem tardio");
    assert!(stored_reply(&w).audio.is_none());
    let reasons: Vec<Value> = w.state.nanty().journal_tail(1000).into_iter().filter(|e| e.code == "voice.text").map(|e| e.params["reasons"].clone()).collect();
    assert!(reasons.contains(&json!(["deadline"])), "{reasons:?}");
}

/// Spec 141 T08: a resposta que cabe na legenda vai inteira nela, com o teclado, e nada mais sai.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_voice_a_short_reply_goes_whole_in_the_caption() {
    let model = Scripted::new(vec![call("remember", remember("Disco")), say(LEAD)]);
    let (w, fake, voice_fake) = voice_world("t14-voice-short", model, |_| {}).await;
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Telegram, "e a fila?").await;
    conversation::reply(&w.state, &asked).await.expect("resposta");
    until("a nota", || !bodies(&fake, "sendVoice").is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let whole = whole_of(&stored_reply(&w));
    assert_eq!(spoken_texts(&voice_fake), [whole.clone()], "um trecho, o texto inteiro");
    let note = &bodies(&fake, "sendVoice")[0];
    assert_eq!(note["caption"], json!(whole));
    let markup: Value = serde_json::from_str(note["reply_markup"].as_str().unwrap_or_default()).expect("JSON");
    assert_eq!(datas(&markup).len(), 1, "o botão na nota");
    assert!(sends(&fake).is_empty(), "a legenda levou tudo");
}

/// Na janela, com a voz ligada: o áudio fala o texto inteiro, a mensagem tem o texto inteiro, e nada
/// vai ao Telegram.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_the_window_with_voice_the_reply_is_whole_with_the_player() {
    let long = long_reply(6);
    let (w, fake, voice_fake) = voice_world("t14-web-voice", Scripted::new(vec![say(&long)]), |_| {}).await;
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Web, "como você faria a fila crescer?").await;
    let reply = conversation::reply(&w.state, &asked).await.expect("resposta");
    assert_eq!(reply.text.as_deref(), Some(long.as_str()));
    assert!(reply.audio.is_some(), "com o tocador");
    assert_eq!(words(&spoken_texts(&voice_fake).join(" ")), words(&whole_of(&reply)), "a fala é o texto inteiro");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(sends(&fake).is_empty() && bodies(&fake, "sendVoice").is_empty(), "a resposta da janela fica nela");
    assert_eq!(stored_reply(&w).text.as_deref(), Some(long.as_str()));
}

/// A frase real do teste (item 10 da spec 141): o modelo escreveu `[neutral]` sem o `tone: `.
const SHORT_MARKER_REPLY: &str = "[neutral]\nEssa ideia de jogo de viagem no tempo parece um projeto novo, não uma proposta para o Mini Street Fighter.";
const SHORT_MARKER_SAID: &str = "Essa ideia de jogo de viagem no tempo parece um projeto novo, não uma proposta para o Mini Street Fighter.";

/// Spec 141 T08 (critério 7): a marca curta não chega à pessoa, com a voz ligada, na janela, no
/// Telegram (a legenda) e na fala.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_short_tone_marker_never_reaches_the_person_with_voice() {
    let model = Scripted::new(vec![say(SHORT_MARKER_REPLY), say(SHORT_MARKER_REPLY)]);
    let (w, fake, voice_fake) = voice_world("t14-marker-voice", model, |_| {}).await;
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Telegram, "e aquela ideia do jogo?").await;
    let reply = conversation::reply(&w.state, &asked).await.expect("resposta");
    assert_eq!(reply.text.as_deref(), Some(SHORT_MARKER_SAID), "a janela");
    until("a nota", || !bodies(&fake, "sendVoice").is_empty()).await;
    assert_eq!(bodies(&fake, "sendVoice")[0]["caption"], json!(SHORT_MARKER_SAID), "o Telegram");
    assert_eq!(spoken_texts(&voice_fake), [SHORT_MARKER_SAID.to_string()], "a fala");
    // Pela janela, também.
    let asked = person(&w, Channel::Web, "e aquela ideia do jogo?").await;
    let reply = conversation::reply(&w.state, &asked).await.expect("resposta");
    assert_eq!(reply.text.as_deref(), Some(SHORT_MARKER_SAID));
    assert!(spoken_texts(&voice_fake).iter().all(|t| !t.contains("neutral")));
}

/// Spec 141 T08 (critério 7): com a voz desligada, o caminho de texto puro também tira a marca curta,
/// no Telegram e na janela.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_short_tone_marker_never_reaches_the_person_without_voice() {
    let model = Scripted::new(vec![say(SHORT_MARKER_REPLY), say(SHORT_MARKER_REPLY)]);
    let (w, fake) = world("t14-marker-text", model, |_, _| {}).await;
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "e aquela ideia do jogo?"));
    until("a resposta", || !sends(&fake).is_empty()).await;
    assert_eq!(sends(&fake).iter().map(text_of).collect::<Vec<_>>(), [SHORT_MARKER_SAID.to_string()], "o Telegram");
    assert_eq!(stored_reply(&w).text.as_deref(), Some(SHORT_MARKER_SAID));
    assert!(bodies(&fake, "sendVoice").is_empty());
    let asked = person(&w, Channel::Web, "e aquela ideia do jogo?").await;
    let reply = conversation::reply(&w.state, &asked).await.expect("resposta");
    assert_eq!(reply.text.as_deref(), Some(SHORT_MARKER_SAID), "a janela");
}

/// Na janela, com a voz desligada: o texto inteiro, sem áudio e sem chamada de fala.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_the_window_without_voice_the_reply_is_whole() {
    let long = long_reply(16);
    let (w, fake, voice_fake) = voice_world("t14-web-text", Scripted::new(vec![say(&long)]), |c| c.nanty.voice.enabled = false).await;
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Web, "como você faria a fila crescer?").await;
    let reply = conversation::reply(&w.state, &asked).await.expect("resposta");
    assert_eq!(reply.text.as_deref(), Some(long.as_str()), "sem o corte de 4096");
    assert!(reply.audio.is_none() && voice_fake.calls("gemini").is_empty());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(sends(&fake).is_empty());
}

// ---- Revisão 1 ----

/// F-01: uma resposta de cerca de 4000 unidades, com o link recusado pelo Telegram, sai de novo inteira
/// com o endereço, sem corte. Antes, a mensagem única não tinha folga e o corte da recusa levava o fim
/// do texto junto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_near_the_limit_with_a_refused_link_goes_again_whole() {
    let text = long_reply(4);
    let model = Scripted::new(vec![call("open_nants", json!({})), say(&text)]);
    let (w, fake) = world("t14-near", model, |c, _| c.nanty.address = BASE.into()).await;
    w.state.nanty().telegram().set_tuning(Tuning { send_gap: Duration::from_millis(400), ..FAST });
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "como você faria a fila crescer?"));
    until("a primeira parte", || sends(&fake).len() == 1).await;
    lock(&fake.send_script).push_back(Tg::Status(400));
    until("de novo sem o botão", || sends(&fake).len() == 3).await;
    let stored = stored_reply(&w);
    let whole = format::clean(&SecretSet::default(), &render::message_text(Lang::PtBr, &stored));
    let units = format::units(&whole);
    assert!(units > format::MAX_MESSAGE_CHARS - format::LAST_PART_ROOM && units <= format::MAX_MESSAGE_CHARS, "{units}");
    let all = sends(&fake);
    assert!(all[0].get("reply_markup").is_none(), "{}", all[0]);
    assert_eq!(all[1]["reply_markup"]["inline_keyboard"][0][0]["url"], json!(format!("{BASE}/")));
    let again = text_of(&all[2]);
    let address = tr(Lang::PtBr, "nanty.telegram.open_address", &[("url", &format!("{BASE}/"))]);
    assert_eq!(again, format!("{}\n{address}", text_of(&all[1])), "a parte inteira, sem corte");
    assert!(whole.trim_end().ends_with(text_of(&all[1]).trim_end()) && !again.contains('…'), "{again}");
    assert!(format::units(&again) <= format::MAX_MESSAGE_CHARS);
    // Nada se perdeu entre as duas mensagens.
    let sent: Vec<String> = [text_of(&all[0]), text_of(&all[1])].iter().flat_map(|t| words(t)).collect();
    assert_eq!(sent, words(&whole));
}
