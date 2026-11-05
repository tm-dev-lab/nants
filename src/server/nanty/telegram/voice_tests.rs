//! Testes da voz em toda saída (spec 139 T08), só com o servidor falso do Telegram de
//! [`super::tests`] e o Gemini simulado de `voice::tests` (nunca o Telegram real nem chave real): o
//! aviso em voz com botões, decidido e editado pela legenda; a recusa que vira texto com os botões; o
//! prazo vencido; a rajada; a espera da aba; o resumo; a resposta na janela; o "gravando áudio" e a
//! voz desligada; o texto que entra na conversa antes da fala; a fala tardia que conta no orçamento.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::tests::{click_on, fake_server, live_after, lock, paired, token, until, Fake, Scripted, World, FAST, OWNER};
use super::*;
use crate::config::{Config, DigestEmpty};
use crate::server::events::SecretSet;
use crate::server::nanty::store::{ConversationMessage, SentCard, SentKind};
use crate::server::nanty::voice;
use crate::server::nanty::voice::tests::{fake_voice, style_of, voice_config, FakeVoice, Reply};
use crate::server::nanty::{chat, clock, digest, new_id, render};

const KEY: &str = "p1:plan_waiting";

/// Mundo pareado com a voz ligada no Gemini simulado.
pub(super) async fn world(tag: &str, edit: impl FnOnce(&mut Config)) -> (World, Arc<Fake>, Arc<FakeVoice>) {
    let tk = token(tag);
    let (fake, base) = fake_server(&tk).await;
    let (voice_fake, voice_base) = fake_voice().await;
    let w = World::with_token(tag, &base, &tk, |cfg| {
        paired(cfg);
        voice_config(cfg, &voice_base);
        edit(cfg);
    });
    (w, fake, voice_fake)
}

pub(super) fn start_outbox(w: &World) -> tokio::task::JoinHandle<()> {
    tokio::spawn(outbox::run(w.state.clone(), w.state.shutdown_signal()))
}

pub(super) fn card(group: &str, action: Option<&str>) -> Card {
    let now = clock::now_ms();
    let mut params = Map::new();
    params.insert("since".into(), json!("10:00"));
    Card {
        id: new_id(),
        group: group.into(),
        code: "nanty.why.plan_waiting".into(),
        params,
        action: action.map(str::to_string),
        token: action.map(|_| act::new_token()),
        created_at_ms: now,
        expires_at_ms: now + 3_600_000,
        delivery: Some(Delivery::Push),
        ..Card::default()
    }
}

pub(super) fn add(w: &World, cards: Vec<Card>) {
    w.state.nanty().update(|st| {
        st.cards.extend(cards);
        true
    });
}

pub(super) fn sent(w: &World, card_id: &str) -> Option<SentCard> {
    w.state.nanty().snapshot().telegram_sent.into_iter().find(|r| r.card_id == card_id)
}

pub(super) fn bodies(fake: &Fake, method: &str) -> Vec<Value> {
    fake.calls(method).into_iter().map(|(b, _)| b).collect()
}

/// Os `callback_data` de um `reply_markup`.
pub(super) fn datas(markup: &Value) -> Vec<String> {
    markup["inline_keyboard"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .flat_map(|r| r.as_array().cloned().unwrap_or_default())
                .filter_map(|b| b["callback_data"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Os motivos das linhas `voice.text` do registro.
pub(super) fn text_reasons(w: &World) -> Vec<Value> {
    w.state
        .nanty()
        .journal_tail(1000)
        .into_iter()
        .filter(|e| e.kind == JournalKind::Voice && e.code == "voice.text")
        .filter_map(|e| e.params.get("reasons").cloned())
        .collect()
}

/// A mensagem da pessoa, já na conversa.
async fn person(w: &World, channel: Channel, text: &str) -> ConversationMessage {
    let cfg = w.state.config().read().await.clone();
    chat::person_message(w.state.nanty(), &cfg.nanty, &SecretSet::from_config(&cfg), channel, text).expect("mensagem")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_goes_as_voice_with_buttons_and_the_decision_edits_the_caption() {
    let (mut w, fake, voice_fake) = world("v-card", |_| {}).await;
    w.start();
    let _out = start_outbox(&w);
    until("conectado", || w.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    let c = card(KEY, Some("approve_plan"));
    let (id, tok) = (c.id.clone(), c.token.clone().unwrap_or_default());
    add(&w, vec![c]);
    until("nota enviada", || fake.calls("sendVoice").len() == 1).await;

    let body = &bodies(&fake, "sendVoice")[0];
    let caption = body["caption"].as_str().unwrap_or_default().to_string();
    assert!(caption.contains("Um plano espera a sua aprovação"), "{body}");
    // No multipart, o teclado vai como JSON em texto.
    let markup: Value = serde_json::from_str(body["reply_markup"].as_str().unwrap_or_default()).expect("JSON");
    assert_eq!(datas(&markup), [callback_data(&id, &tok, Answer::Yes), callback_data(&id, &tok, Answer::No), callback_data(&id, &tok, Answer::Later)]);
    // A fala é o texto da legenda, e o gasto contou.
    assert_eq!(voice_fake.calls("gemini")[0]["input"][0]["content"][0]["text"], json!(caption));
    assert!(fake.calls("sendMessage").is_empty(), "a legenda levou o texto inteiro");

    until("registro", || sent(&w, &id).is_some()).await;
    let rec = sent(&w, &id).expect("registro");
    assert_eq!((rec.kind, rec.buttons, rec.message_id), (SentKind::Voice, true, Some(1)));
    // O áudio entra na conversa, para a janela tocar, sem voltar para a fila.
    let conv = w.state.nanty().conversation();
    let notice = conv.iter().find(|m| m.audio.is_some()).expect("aviso com áudio");
    assert_eq!((notice.card_ids.as_slice(), notice.channel), (std::slice::from_ref(&id), Channel::Telegram));
    assert_eq!(render::message_text(Lang::PtBr, notice), render::code_text(Lang::PtBr, "nanty.why.plan_waiting", &notice.params));
    let audio = notice.audio.clone().expect("áudio");
    assert!(w.state.nanty().read_audio(&audio.id).is_some());

    // Decidido: a legenda ganha a linha final e a nota perde os botões.
    fake.push(click_on(100, OWNER, OWNER, 1, &callback_data(&id, &tok, Answer::No)));
    until("legenda editada", || fake.calls("editMessageCaption").len() == 1).await;
    let edit = &bodies(&fake, "editMessageCaption")[0];
    assert_eq!((edit["chat_id"].clone(), edit["message_id"].clone()), (json!(OWNER), json!(1)));
    assert!(edit.get("reply_markup").is_none(), "sem botões: {edit}");
    let edited = edit["caption"].as_str().unwrap_or_default();
    assert!(edited.starts_with(&caption) && edited.len() > caption.len(), "{edited}");
    assert!(fake.calls("editMessageText").is_empty(), "nota de voz não se edita como texto");
    until("registro fechado", || sent(&w, &id).is_none()).await;
    assert_eq!(fake.calls("sendVoice").len(), 1, "um áudio por aviso");
    w.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_card_keeps_the_final_line_inside_the_caption_and_the_full_text_goes_apart() {
    let (w, fake, _voice) = world("v-long", |_| {}).await;
    let _out = start_outbox(&w);
    let mut c = card(KEY, None);
    c.params.insert("since".into(), json!("quase pronto ".repeat(150).trim()));
    let id = c.id.clone();
    let full = render::card_text(Lang::PtBr, &c);
    add(&w, vec![c]);
    until("texto à parte", || !fake.sent_texts().is_empty()).await;
    let caption = bodies(&fake, "sendVoice")[0]["caption"].as_str().unwrap_or_default().to_string();
    assert!(format::units(&caption) <= format::MAX_CAPTION_CHARS && caption.ends_with('…'), "{}", caption.len());
    assert_eq!(fake.sent_texts(), [full.trim().to_string()]);
    until("registro", || sent(&w, &id).is_some()).await;

    // Resolvido: a linha final cabe inteira na legenda.
    w.state.nanty().update(|st| {
        st.cards.retain(|c| c.id != id);
        true
    });
    until("legenda editada", || fake.calls("editMessageCaption").len() == 1).await;
    let edited = bodies(&fake, "editMessageCaption")[0]["caption"].as_str().unwrap_or_default().to_string();
    assert!(format::units(&edited) <= format::MAX_CAPTION_CHARS, "{}", format::units(&edited));
    assert!(edited.ends_with(&tr(Lang::PtBr, "nanty.telegram.expired", &[])), "{edited}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_note_becomes_text_with_the_buttons_and_the_card_is_edited_as_text() {
    let (mut w, fake, _voice) = world("v-refused", |_| {}).await;
    w.start();
    let _out = start_outbox(&w);
    until("conectado", || w.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    lock(&fake.voice_script).push_back(Scripted::Status(400));
    let c = card(KEY, Some("approve_plan"));
    let (id, tok) = (c.id.clone(), c.token.clone().unwrap_or_default());
    add(&w, vec![c]);
    until("texto", || fake.calls("sendMessage").len() == 1).await;
    assert_eq!(fake.calls("sendVoice").len(), 1);
    let body = &bodies(&fake, "sendMessage")[0];
    assert_eq!(datas(&body["reply_markup"]).len(), 3, "os botões seguem: {body}");
    until("registro", || sent(&w, &id).is_some()).await;
    let rec = sent(&w, &id).expect("registro");
    assert_eq!((rec.kind, rec.buttons), (SentKind::Text, true));
    assert_eq!(w.journal("telegram.send_failed")[0]["method"], "sendVoice");
    // O áudio já gerado fica na conversa.
    assert!(w.state.nanty().conversation().iter().any(|m| m.audio.is_some()));

    let message_id = rec.message_id.unwrap_or_default();
    fake.push(click_on(101, OWNER, OWNER, message_id, &callback_data(&id, &tok, Answer::No)));
    until("texto editado", || fake.calls("editMessageText").len() == 1).await;
    assert!(fake.calls("editMessageCaption").is_empty());
    w.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_passing_failure_of_the_note_keeps_the_speech_for_the_next_try() {
    let (w, fake, voice_fake) = world("v-retry", |_| {}).await;
    let _out = start_outbox(&w);
    lock(&fake.voice_script).push_back(Scripted::Retry(1));
    let c = card(KEY, None);
    let id = c.id.clone();
    add(&w, vec![c]);
    until("segunda tentativa", || fake.calls("sendVoice").len() == 2).await;
    until("registro", || sent(&w, &id).is_some()).await;
    assert_eq!(sent(&w, &id).map(|r| r.kind), Some(SentKind::Voice));
    assert_eq!(voice_fake.calls("gemini").len(), 1, "a fala não é paga duas vezes");
    assert!(fake.calls("sendMessage").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_speech_sends_the_card_in_text_and_never_holds_the_queue() {
    let (w, fake, voice_fake) = world("v-late", |_| {}).await;
    w.state.nanty().telegram().set_tuning(Tuning { speech_deadline: Duration::from_secs(2), ..FAST });
    voice_fake.hold_speech();
    let _out = start_outbox(&w);
    let nanty = w.state.nanty();
    let c = card(KEY, Some("approve_plan"));
    let id = c.id.clone();
    add(&w, vec![c]);
    until("a fala começou", || voice_fake.calls("gemini").len() == 1).await;

    // A resposta a quem escreveu passa na frente; o texto que cita o cartão espera por ele.
    let mut cited = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Web, "x", Map::new());
    cited.code = None;
    cited.text = Some("Sobre o aviso.".into());
    cited.card_ids = vec![id.clone()];
    nanty.push_message(&cited);
    let mut answer = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Telegram, "x", Map::new());
    answer.code = None;
    answer.text = Some("Oi.".into());
    answer.reply_to = Some("0123456789abcdef".into());
    nanty.push_message(&answer);
    until("resposta", || fake.sent_texts().iter().any(|t| t == "Oi.")).await;
    assert_eq!(fake.sent_texts(), ["Oi."], "a fala do cartão não segura a fila");

    until("cartão em texto", || fake.calls("sendMessage").len() == 3).await;
    let sends = bodies(&fake, "sendMessage");
    assert_eq!(datas(&sends[1]["reply_markup"]).len(), 3, "o cartão, com os botões");
    assert_eq!(sends[2]["text"], "Sobre o aviso.");
    assert!(fake.calls("sendVoice").is_empty());
    assert_eq!(text_reasons(&w), [json!(["deadline"])]);
    until("registro", || sent(&w, &id).is_some()).await;
    assert_eq!(sent(&w, &id).map(|r| r.kind), Some(SentKind::Text));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_stays_in_text_without_any_speech() {
    let (w, fake, voice_fake) = world("v-burst", |_| {}).await;
    let _out = start_outbox(&w);
    let cards: Vec<Card> = (0..=outbox::MAX_BURST).map(|i| card(&format!("p{i}:plan_waiting"), None)).collect();
    add(&w, cards);
    until("resumo da fila", || fake.calls("sendMessage").len() == 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.calls("sendMessage").len(), 1);
    assert!(fake.calls("sendVoice").is_empty() && voice_fake.calls("gemini").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_is_decided_in_the_window_costs_no_speech() {
    // A espera da aba não vence sozinha no teste: quem a termina é a aba que sai.
    let (w, fake, voice_fake) = world("v-hold", |cfg| cfg.nanty.telegram.visible_hold_secs = 3600).await;
    let _out = start_outbox(&w);
    let nanty = w.state.nanty();
    nanty.telegram().outbox().presence("aba-1", true, clock::now_ms());
    let (decided, kept) = (card(KEY, None), card("p2:plan_waiting", None));
    let gone = decided.id.clone();
    add(&w, vec![decided, kept]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(voice_fake.calls("gemini").is_empty(), "a fala espera a aba");
    nanty.update(|st| {
        st.cards.retain(|c| c.id != gone);
        true
    });
    nanty.telegram().outbox().presence("aba-1", false, clock::now_ms());
    until("o outro fala", || fake.calls("sendVoice").len() == 1).await;
    assert_eq!(voice_fake.calls("gemini").len(), 1, "o que saiu na janela não custou fala");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_model_text_about_a_notice_and_the_requested_card_have_no_second_audio() {
    let (w, fake, voice_fake) = world("v-one", |_| {}).await;
    let _out = start_outbox(&w);
    let c = card(KEY, None);
    let mut about = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Web, "x", Map::new());
    about.code = None;
    about.text = Some("O plano da Loja chegou.".into());
    about.card_ids = vec![c.id.clone()];
    add(&w, vec![c]);
    w.state.nanty().push_message(&about);
    let mut requested = card("p3:pause", None);
    requested.requested = Some(Channel::Telegram);
    add(&w, vec![requested]);
    until("os três", || fake.calls("sendMessage").len() == 2 && fake.calls("sendVoice").len() == 1).await;
    assert_eq!(voice_fake.calls("gemini").len(), 1);
    assert_eq!(fake.sent_texts().iter().filter(|t| t.as_str() == "O plano da Loja chegou.").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_budget_or_with_a_failure_the_card_goes_in_text_with_the_reason() {
    let (w, fake, voice_fake) = world("v-budget", |_| {}).await;
    let _out = start_outbox(&w);
    let nanty = w.state.nanty();
    lock(&voice_fake.gemini).push_back(Reply::Status(500));
    add(&w, vec![card(KEY, Some("approve_plan"))]);
    until("texto", || fake.calls("sendMessage").len() == 1).await;
    assert_eq!(text_reasons(&w), [json!(["provider_error"])]);
    let day = nanty.now().day_key();
    let limit = w.state.config().read().await.nanty.daily_budget_tokens;
    nanty.update(|st| {
        st.add_spend(&day, limit, 0, None);
        true
    });
    add(&w, vec![card("p2:plan_waiting", Some("approve_plan"))]);
    until("texto de novo", || fake.calls("sendMessage").len() == 2).await;
    assert!(datas(&bodies(&fake, "sendMessage")[1]["reply_markup"]).len() == 3);
    assert_eq!(text_reasons(&w)[1], json!(["budget"]));
    assert_eq!(voice_fake.calls("gemini").len(), 1, "sem orçamento nem chama");
    assert!(fake.calls("sendVoice").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_morning_digest_speaks() {
    let (w, fake, _voice) = world("v-digest", |cfg| {
        cfg.nanty.digest_time = "11:00".into();
        cfg.nanty.digest_empty = DigestEmpty::Short;
    })
    .await;
    let _out = start_outbox(&w);
    let nanty = Arc::clone(w.state.nanty());
    assert!(digest::maybe_send(&w.state, &nanty).await);
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    let message = nanty.conversation().into_iter().find(|m| m.code.as_deref() == Some(digest::QUIET)).expect("resumo");
    assert!(message.audio.is_some(), "a janela também toca");
    assert_eq!(bodies(&fake, "sendVoice")[0]["caption"], json!(render::message_text(Lang::PtBr, &message)));
    assert!(fake.calls("sendMessage").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_in_the_window_has_audio_and_stays_in_the_window() {
    let (w, fake, voice_fake) = world("v-window", |_| {}).await;
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Web, "/status").await;
    let reply = chat::reply(&w.state, &asked).await.expect("resposta");
    let audio = reply.audio.clone().expect("áudio na resposta");
    assert!(w.state.nanty().read_audio(&audio.id).is_some(), "a janela toca pela rota");
    let saved = w.state.nanty().conversation().into_iter().find(|m| m.id == reply.id).expect("gravada");
    assert_eq!(saved.audio, Some(audio));
    assert_eq!(voice_fake.calls("gemini").len(), 1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fake.calls("sendVoice").is_empty() && fake.calls("sendMessage").is_empty(), "fica só na janela");
    assert!(fake.calls("sendChatAction").is_empty(), "o gravando é só para quem escreveu pelo Telegram");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_written_message_on_telegram_is_answered_in_voice_with_the_recording_action() {
    let (w, fake, voice_fake) = world("v-record", |_| {}).await;
    voice_fake.hold_speech();
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Telegram, "/status").await;
    assert!(!asked.voice, "a pessoa escreveu");
    let state = w.state.clone();
    let replying = tokio::spawn(async move { chat::reply(&state, &asked).await });
    until("renovado", || fake.calls("sendChatAction").len() >= 2).await;
    voice_fake.release_speech(1);
    let reply = replying.await.expect("tarefa").expect("resposta");
    assert!(reply.audio.is_some());
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    let actions: Vec<Value> = bodies(&fake, "sendChatAction").iter().map(|b| b["action"].clone()).collect();
    assert!(actions.iter().all(|a| a == "record_voice"), "só o gravando: {actions:?}");
    assert!(fake.calls("sendMessage").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn voice_off_is_only_text_everywhere() {
    let (w, fake, voice_fake) = world("v-off", |cfg| {
        cfg.nanty.voice.enabled = false;
        cfg.nanty.digest_time = "11:00".into();
        cfg.nanty.digest_empty = DigestEmpty::Short;
    })
    .await;
    let _out = start_outbox(&w);
    let nanty = Arc::clone(w.state.nanty());
    add(&w, vec![card(KEY, Some("approve_plan"))]);
    until("cartão", || fake.calls("sendMessage").len() == 1).await;
    let asked = person(&w, Channel::Telegram, "/status").await;
    assert!(chat::reply(&w.state, &asked).await.expect("resposta").audio.is_none());
    let window = person(&w, Channel::Web, "/status").await;
    assert!(chat::reply(&w.state, &window).await.expect("resposta").audio.is_none());
    assert!(digest::maybe_send(&w.state, &nanty).await);
    until("resposta e resumo", || fake.calls("sendMessage").len() == 3).await;
    assert!(fake.calls("sendVoice").is_empty() && voice_fake.calls("gemini").is_empty());
    assert!(nanty.conversation().iter().all(|m| m.audio.is_none()));
    assert!(text_reasons(&w).is_empty(), "desligada não é falha");
}

/// Quem foi gravado antes da T08 é texto, e o tipo só é gravado quando é voz.
#[test]
fn old_sent_records_are_text() {
    let old: SentCard = serde_json::from_value(json!({"card_id": "c", "chat_id": 1, "created_at_ms": 2, "at_ms": 3})).expect("registro antigo");
    assert_eq!(old.kind, SentKind::Text);
    assert!(serde_json::to_value(&old).expect("json").get("kind").is_none());
    let voice = SentCard { kind: SentKind::Voice, ..old };
    assert_eq!(serde_json::to_value(&voice).expect("json")["kind"], "voice");
}

// ---- O texto antes da fala (revisão 1) ----

pub(super) fn nanty_lines(w: &World) -> Vec<ConversationMessage> {
    w.state.nanty().conversation().into_iter().filter(|m| m.from == crate::server::nanty::store::Speaker::Nanty).collect()
}

/// Quantos avisos `nanty_changed` da conversa saíram desde a assinatura.
fn conversation_events(rx: &mut tokio::sync::broadcast::Receiver<crate::server::events::ApiEvent>) -> usize {
    let mut count = 0;
    while let Ok(event) = rx.try_recv() {
        if event.payload["part"] == "conversation" {
            count += 1;
        }
    }
    count
}

/// Marcador do fim da fila de saída.
pub(super) const DRAINED: &str = "fim da fila";

/// Espera a fila de saída chegar ao fim: um texto entra por último e sai depois do que veio antes.
pub(super) async fn drained(w: &World, fake: &Fake) {
    let mut last = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Web, "x", Map::new());
    last.code = None;
    last.text = Some(DRAINED.into());
    w.state.nanty().push_message(&last);
    until("fim da fila", || fake.sent_texts().iter().any(|t| t == DRAINED)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_window_shows_the_text_at_once_and_the_player_joins_the_same_message() {
    let (w, _fake, voice_fake) = world("v-first", |_| {}).await;
    voice_fake.hold_speech();
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Web, "/status").await;
    let mut events = w.state.events().subscribe();
    chat::spawn_reply(&w.state, asked);
    until("texto na janela", || !nanty_lines(&w).is_empty()).await;
    let first = nanty_lines(&w).remove(0);
    assert!(first.audio.is_none(), "o texto não espera a fala");
    voice_fake.release_speech(1);
    until("tocador", || nanty_lines(&w).iter().any(|m| m.audio.is_some())).await;
    let all = w.state.nanty().conversation();
    assert_eq!(all.len(), 2, "a pessoa e uma resposta só");
    assert_eq!(all[1].id, first.id, "a mesma mensagem, na mesma posição");
    assert_eq!(conversation_events(&mut events), 2, "a janela relê nas duas gravações");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_telegram_the_note_waits_for_the_speech_and_goes_once() {
    let (w, fake, voice_fake) = world("v-note", |_| {}).await;
    voice_fake.hold_speech();
    let _out = start_outbox(&w);
    let asked = person(&w, Channel::Telegram, "/status").await;
    chat::spawn_reply(&w.state, asked);
    until("texto na conversa", || !nanty_lines(&w).is_empty()).await;
    until("a fala começou", || voice_fake.calls("gemini").len() == 1).await;
    assert!(fake.calls("sendVoice").is_empty() && fake.calls("sendMessage").is_empty(), "o Telegram espera a fala");
    voice_fake.release_speech(1);
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    until("áudio na mensagem", || nanty_lines(&w).iter().any(|m| m.audio.is_some())).await;
    let lines = nanty_lines(&w);
    assert_eq!(lines.len(), 1);
    assert_eq!(bodies(&fake, "sendVoice")[0]["caption"], json!(render::message_text(Lang::PtBr, &lines[0])));
    // A fila é em ordem: o marcador depois da nota só sai com tudo o que veio antes já enviado.
    drained(&w, &fake).await;
    assert_eq!(fake.calls("sendVoice").len(), 1, "a nota sai uma vez só");
    assert_eq!(fake.sent_texts(), [DRAINED], "sem repetir em texto");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_speech_keeps_the_single_line_and_goes_in_text() {
    let (w, fake, voice_fake) = world("v-one-line", |_| {}).await;
    let _out = start_outbox(&w);
    lock(&voice_fake.gemini).push_back(Reply::Status(500));
    let asked = person(&w, Channel::Telegram, "/status").await;
    let mut events = w.state.events().subscribe();
    let reply = chat::reply(&w.state, &asked).await.expect("resposta");
    until("texto", || fake.calls("sendMessage").len() == 1).await;
    assert!(reply.audio.is_none() && fake.calls("sendVoice").is_empty());
    let lines = nanty_lines(&w);
    assert_eq!(lines, [ConversationMessage { seq: lines[0].seq, ..reply }]);
    assert!(lines[0].seq.is_some());
    assert_eq!(conversation_events(&mut events), 1, "sem áudio, sem segunda linha");
}

/// A primeira fala fica presa e a segunda não pode passar dela: a ordem é a de entrada, na conversa e
/// no Telegram.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_replies_in_a_row_keep_their_order() {
    let (w, fake, voice_fake) = world("v-order", |_| {}).await;
    voice_fake.hold_speech();
    let _out = start_outbox(&w);
    let first = person(&w, Channel::Telegram, "/status").await;
    chat::spawn_reply(&w.state, first);
    until("primeira resposta", || nanty_lines(&w).len() == 1).await;
    let second = person(&w, Channel::Telegram, "/ajuda").await;
    chat::spawn_reply(&w.state, second);
    until("segunda resposta", || nanty_lines(&w).len() == 2).await;
    until("a primeira fala começou", || voice_fake.calls("gemini").len() == 1).await;
    assert!(fake.calls("sendVoice").is_empty(), "o texto das duas já está na conversa");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(voice_fake.calls("gemini").len(), 1, "a segunda espera a vez dela");
    voice_fake.release_speech(1);
    until("primeira nota", || fake.calls("sendVoice").len() == 1).await;
    voice_fake.release_speech(1);
    until("duas notas", || fake.calls("sendVoice").len() == 2).await;
    until("os dois áudios", || nanty_lines(&w).iter().all(|m| m.audio.is_some())).await;
    let lines = nanty_lines(&w);
    assert_eq!(lines[0].code.as_deref(), Some("nanty.chat.status"));
    assert_eq!(lines[1].code.as_deref(), Some("nanty.chat.help_no_model"));
    let captions: Vec<Value> = bodies(&fake, "sendVoice").iter().map(|b| b["caption"].clone()).collect();
    let expected: Vec<Value> = lines.iter().map(|m| json!(render::message_text(Lang::PtBr, m))).collect();
    assert_eq!(captions, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_digest_is_kept_before_its_speech() {
    let (w, fake, voice_fake) = world("v-digest-first", |cfg| {
        cfg.nanty.digest_time = "11:00".into();
        cfg.nanty.digest_empty = DigestEmpty::Short;
    })
    .await;
    voice_fake.hold_speech();
    let _out = start_outbox(&w);
    let (state, nanty) = (w.state.clone(), Arc::clone(w.state.nanty()));
    let sending = tokio::spawn(async move { digest::maybe_send(&state, &nanty).await });
    until("resumo na conversa", || !nanty_lines(&w).is_empty()).await;
    assert!(nanty_lines(&w)[0].audio.is_none(), "se o processo cair na fala, o resumo já está gravado");
    voice_fake.release_speech(1);
    assert!(sending.await.expect("tarefa"));
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    let lines = nanty_lines(&w);
    assert!(lines.len() == 1 && lines[0].audio.is_some());
}

/// O prazo vence, o texto segue na hora (spec 141 T08, revisão 1: o prazo é por trecho). A fala para
/// ali: o trecho encerrado entra no gasto pela estimativa, porque o provedor pode tê-lo cobrado, e a
/// resposta tardia não conta de novo nem vira áudio.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_speech_past_the_deadline_still_counts_in_the_budget() {
    let (w, _fake, voice_fake) = world("v-charge", |_| {}).await;
    w.state.nanty().telegram().set_tuning(Tuning { speech_deadline: Duration::from_millis(100), ..FAST });
    voice_fake.hold_speech();
    let nanty = w.state.nanty();
    let spend = || nanty.snapshot().spend_on(&nanty.now().day_key());
    let late = voice::speak_on_time(&w.state, nanty, "Oi.", voice::Tone::Neutral).await;
    assert_eq!(late.map(|_| ()), Err(vec!["deadline"]));
    assert_eq!(text_reasons(&w), [json!(["deadline"])]);
    assert_eq!(spend().voice_calls, 1);
    assert!(spend().input_tokens > 0 && spend().output_tokens > 0);
    voice_fake.release_speech(1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(spend().voice_calls, 1, "a resposta tardia não conta de novo");
    let ok = nanty.journal_tail(100).iter().filter(|e| e.kind == JournalKind::Voice && e.code == "voice.ok").count();
    assert_eq!(ok, 0);
}

/// Spec 140 T01: o aviso fala no tom do tipo do sinal, e o pedido de decisão fica sem anotação.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_speaks_in_the_tone_of_its_signal() {
    let (w, fake, voice_fake) = world("v-tone", |_| {}).await;
    let _out = start_outbox(&w);
    let mut done = card("p1:spec_done", None);
    done.code = "nanty.why.spec_done".into();
    add(&w, vec![done]);
    until("marco", || fake.calls("sendVoice").len() == 1).await;
    assert_eq!(style_of(&voice_fake, 0), json!("warm and upbeat"));
    let mut disk = card("*:disk_low", None);
    disk.code = "nanty.why.disk_low".into();
    add(&w, vec![disk]);
    until("disco", || fake.calls("sendVoice").len() == 2).await;
    assert_eq!(style_of(&voice_fake, 1), json!("concerned but composed"));
    add(&w, vec![card(KEY, None)]);
    until("plano", || fake.calls("sendVoice").len() == 3).await;
    assert_eq!(style_of(&voice_fake, 2), Value::Null);
}

/// Spec 140 T01: recusada a anotação, o aviso sai em voz mesmo assim, sem esperar outra rodada.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_with_a_refused_style_still_goes_as_voice() {
    let (w, fake, voice_fake) = world("v-tone-400", |_| {}).await;
    let _out = start_outbox(&w);
    lock(&voice_fake.gemini).push_back(Reply::Status(400));
    let mut blocked = card("p1:run_blocked", None);
    blocked.code = "nanty.why.run_blocked".into();
    add(&w, vec![blocked]);
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    assert_eq!(voice_fake.calls("gemini").len(), 2);
    assert_eq!((style_of(&voice_fake, 0), style_of(&voice_fake, 1)), (json!("serious and steady"), Value::Null));
    assert!(fake.calls("sendMessage").is_empty());
}

/// A última linha vence, e a posição continua a da primeira.
#[test]
fn lines_of_the_same_id_merge_in_the_first_position() {
    let line = |id: &str, text: &str| {
        let mut m = ConversationMessage::from_nanty(id.into(), 0, Channel::Web, "x", Map::new());
        m.text = Some(text.into());
        m
    };
    let merged = crate::server::nanty::store::merged_lines(vec![line("a", "1"), line("b", "2"), line("a", "3")]);
    let got: Vec<(&str, Option<&str>)> = merged.iter().map(|m| (m.id.as_str(), m.text.as_deref())).collect();
    assert_eq!(got, [("a", Some("3")), ("b", Some("2"))]);
}
