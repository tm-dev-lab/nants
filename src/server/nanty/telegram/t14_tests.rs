//! A resposta longa (spec 140 T14, C-27), só com o servidor falso do Telegram, o Gemini simulado
//! (com a fala presa até a liberação explícita) e o modelo simulado; nunca o Telegram real nem chave
//! real. A conversa grava o texto inteiro; o Telegram o divide em até três partes, com o teclado e a
//! frase do servidor na última; com a voz ligada, o áudio e a legenda levam só o começo do primeiro
//! parágrafo, e as partes trazem o resto; a janela mostra tudo.

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

/// A frase do servidor como vai ao Telegram.
fn closing_of(message: &ConversationMessage) -> String {
    let line = render::closing(Lang::PtBr, message).expect("frase do servidor");
    format::clean(&SecretSet::default(), &line)
}

async fn person(w: &World, channel: Channel, text: &str) -> ConversationMessage {
    let cfg = w.state.config().read().await.clone();
    conversation::person_message(w.state.nanty(), &cfg.nanty, &SecretSet::from_config(&cfg), channel, text).expect("mensagem")
}

/// As palavras do texto, para conferir que nada se partiu nem se perdeu.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

// ---- Puros: quando a fala usa só o começo ----

#[test]
fn only_the_long_reply_that_opens_with_the_model_speaks_just_the_lead() {
    let model = |code: Option<&str>, text: &str| {
        let mut m = ConversationMessage::from_nanty(new_id(), 1, Channel::Telegram, code.unwrap_or("x"), Map::new());
        m.code = code.map(str::to_string);
        m.text = Some(text.to_string());
        m
    };
    let long = long_reply(3);
    let end = voice::long_lead(&model(None, &long), &long, 450).expect("longa");
    assert_eq!(long[..end].trim_end(), LEAD);
    // O primeiro parágrafo longo: a fala para no último fim de frase antes de 450 caracteres.
    let glued = long.replace("\n\n", " ");
    let end = voice::long_lead(&model(None, &glued), &glued, 450).expect("longa");
    assert!(glued[..end].chars().count() <= 450 && glued[..end].ends_with('.'), "{}", &glued[..end]);
    // A curta, a que cabe na legenda e a que abre pelo título do servidor seguem a fala de sempre.
    assert_eq!(voice::long_lead(&model(None, LEAD), LEAD, 450), None);
    let digest = model(Some(crate::server::nanty::digest::LEAD), &long);
    assert_eq!(voice::long_lead(&digest, &long, 450), None);
    // Com a frase do servidor no fim, a fala é a mesma.
    let closed = model(Some("nanty.chat.confirm"), &long);
    let text = render::message_text(Lang::PtBr, &closed);
    let end = voice::long_lead(&closed, &text, 450).expect("longa");
    assert_eq!(text[..end].trim_end(), LEAD);
}

// ---- Sem voz, no Telegram ----

/// Sem voz: a conversa grava a resposta inteira; o Telegram a divide em três partes, a última termina
/// com a linha que aponta a janela e a frase do servidor, e leva o teclado (o link e o botão de
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
    // A frase do servidor fecha a última parte, depois da linha da janela.
    let more = tr(Lang::PtBr, "nanty.telegram.rest_in_window", &[]);
    assert!(texts[2].ends_with(&format!("{more}\n{}", closing_of(&stored))), "{}", texts[2]);
    // Nada partido: as palavras das partes são o começo das palavras da resposta, na ordem.
    let sent: Vec<String> = texts.iter().flat_map(|t| words(t)).collect();
    let tail_words = words(&format!("{more}\n{}", closing_of(&stored))).len();
    let body = &sent[..sent.len() - tail_words];
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
    assert!(text_of(&all[1]).ends_with(&saved), "a frase do servidor fecha: {}", text_of(&all[1]));
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

/// Com a voz ligada no Telegram: a conversa recebe o texto inteiro antes da fala; o áudio fala só o
/// primeiro parágrafo; a legenda leva o mesmo trecho e avisa que o resto vem a seguir, sem teclado; as
/// partes trazem o resto, sem repetir o trecho, e a última leva o botão e a frase do servidor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_voice_the_note_speaks_the_lead_and_the_rest_follows_in_parts() {
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
    assert_eq!(voice_fake.calls("gemini")[0]["input"][0]["content"][0]["text"], json!(LEAD), "o áudio só com o primeiro parágrafo");
    voice_fake.release_speech(1);
    let _ = replying.await;
    until("as partes", || sends(&fake).len() == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let note = &bodies(&fake, "sendVoice");
    assert_eq!(note.len(), 1);
    let caption = note[0]["caption"].as_str().unwrap_or_default();
    assert_eq!(caption, format!("{LEAD}\n\n{}", tr(Lang::PtBr, "nanty.telegram.rest_follows", &[])));
    assert!(format::units(caption) <= format::MAX_CAPTION_CHARS);
    assert!(note[0].get("reply_markup").is_none(), "o teclado vai na última parte: {}", note[0]);
    let all = sends(&fake);
    assert_eq!(all.len(), 2, "{all:?}");
    let texts: Vec<String> = all.iter().map(text_of).collect();
    assert!(texts[0].starts_with(&detail(1)[..40]) && !texts.iter().any(|t| t.contains(LEAD)), "o trecho não se repete");
    assert!(all[0].get("reply_markup").is_none() && datas(&all[1]["reply_markup"]).len() == 1);
    let stored = stored_reply(&w);
    assert!(texts[1].ends_with(&closing_of(&stored)), "a frase do servidor fecha: {}", texts[1]);
    // A janela: a resposta inteira, com o tocador.
    assert!(stored.audio.is_some() && stored.text.as_deref() == Some(long.as_str()));
    let rec = w.state.nanty().snapshot().telegram_sent.into_iter().find(|r| r.embedded).expect("anotada");
    assert_eq!(rec.message_id, Some(3), "a nota é a 1; as partes, 2 e 3");
}

/// Na janela, com a voz ligada: o áudio fala o primeiro parágrafo, a mensagem tem o texto inteiro, e
/// nada vai ao Telegram.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_the_window_with_voice_the_reply_is_whole_with_the_player() {
    let long = long_reply(6);
    let (w, fake, voice_fake) = voice_world("t14-web-voice", Scripted::new(vec![say(&long)]), |_| {}).await;
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Web, "como você faria a fila crescer?").await;
    let reply = conversation::reply(&w.state, &asked).await.expect("resposta");
    assert_eq!(reply.text.as_deref(), Some(long.as_str()));
    assert!(reply.audio.is_some(), "com o tocador");
    assert_eq!(voice_fake.calls("gemini")[0]["input"][0]["content"][0]["text"], json!(LEAD));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(sends(&fake).is_empty() && bodies(&fake, "sendVoice").is_empty(), "a resposta da janela fica nela");
    assert_eq!(stored_reply(&w).text.as_deref(), Some(long.as_str()));
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
/// com o endereço, e a frase do servidor continua no fim do texto. Antes, a mensagem única não tinha
/// folga e o corte da recusa levava a frase junto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_near_the_limit_with_a_refused_link_keeps_the_server_sentence() {
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
    assert!(again.ends_with(&format!("{}\n{address}", closing_of(&stored))) && !again.contains('…'), "{again}");
    assert!(format::units(&again) <= format::MAX_MESSAGE_CHARS);
    // Nada se perdeu entre as duas mensagens.
    let sent: Vec<String> = [text_of(&all[0]), text_of(&all[1])].iter().flat_map(|t| words(t)).collect();
    assert_eq!(sent, words(&whole));
}

/// F-02: com `lead_chars` no máximo aceito, o trecho falado cabe na legenda, sem `…`, e a legenda e as
/// partes juntas trazem o texto inteiro: nada é falado sem aparecer escrito.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_the_largest_lead_the_spoken_part_is_all_written() {
    let mut cfg = Config::default();
    cfg.nanty.voice.lead_chars = 2000;
    assert_eq!(cfg.nanty.voice.lead_chars(), 900, "o teto coerente com a legenda");
    // Um primeiro parágrafo longo, sem linha em branco, com caracteres de duas unidades.
    let long = format!("{} {}", "O plano \u{10400} segue firme e a fila aguenta bem o volume de hoje. ".repeat(40).trim_end(), detail(1));
    let model = Scripted::new(vec![say(&long)]);
    let (w, fake, voice_fake) = voice_world("t14-lead-max", model, |c| c.nanty.voice.lead_chars = 2000).await;
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Telegram, "como vai o plano?").await;
    conversation::reply(&w.state, &asked).await.expect("resposta");
    until("o resto", || !sends(&fake).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let caption = bodies(&fake, "sendVoice")[0]["caption"].as_str().unwrap_or_default().to_string();
    let follows = tr(Lang::PtBr, "nanty.telegram.rest_follows", &[]);
    let lead = caption.strip_suffix(&format!("\n\n{follows}")).expect("a linha do resto").to_string();
    assert!(!caption.contains('…') && format::units(&caption) <= format::MAX_CAPTION_CHARS, "{}", format::units(&caption));
    assert_eq!(voice_fake.calls("gemini")[0]["input"][0]["content"][0]["text"], json!(lead), "o falado é o escrito");
    let mut written = words(&lead);
    for body in sends(&fake) {
        written.extend(words(&text_of(&body)));
    }
    assert_eq!(written, words(&long));
    // O mesmo direto na função, com o valor sem teto.
    let mut m = ConversationMessage::from_nanty(new_id(), 1, Channel::Telegram, "x", Map::new());
    m.code = None;
    m.text = Some(long.clone());
    let end = voice::long_lead(&m, &long, 2000).expect("longa");
    assert!(format::units(&long[..end]) <= voice::lead_caption_budget() && long[..end].ends_with('.'));
}
