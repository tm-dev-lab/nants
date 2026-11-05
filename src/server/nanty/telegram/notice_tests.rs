//! Testes do aviso em voz com a fala do modelo (spec 140 T02, C-29), só com o servidor falso do
//! Telegram, o Gemini simulado e um modelo simulado preso (nunca o Telegram real nem chave real). Nada
//! aqui depende de tempo de sobra: o modelo e a fala só andam pela liberação explícita, e cada prazo
//! ou é curto com o modelo preso (vence sempre) ou é de horas (nunca vence).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Semaphore;

use super::tests::{click_on, live_after, lock, until, Fake, Scripted, World, FAST, OWNER};
use super::voice_tests::{add, bodies, card, datas, drained, nanty_lines, sent, start_outbox, text_reasons, world, DRAINED};
use super::*;
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::{Billing, ChatMessage, ChatRequest, ChatResponse, Role, Usage};
use crate::agents::LlmError;
use crate::config::{Config, Urgency};
use crate::server::nanty::store::{ConversationMessage, SentKind};
use crate::server::nanty::turn::{self, TurnOutcome};
use crate::server::nanty::voice::tests::{style_of, FakeVoice, Reply};
use crate::server::nanty::{clock, new_id, render};

const KEY: &str = "p1:plan_waiting";
const SAID: &str = "Boa notícia: o plano da Loja ficou pronto.";
const FOREVER: Duration = Duration::from_secs(3600);
const SHORT: Duration = Duration::from_millis(50);

/// Modelo que só responde depois da liberação ([`Self::release`]).
struct HeldModel {
    gate: Semaphore,
    asked: AtomicUsize,
    /// `None` = o provedor falha.
    reply: Mutex<Option<String>>,
}

impl HeldModel {
    fn new(reply: Option<String>) -> Arc<Self> {
        Arc::new(Self { gate: Semaphore::new(0), asked: AtomicUsize::new(0), reply: Mutex::new(reply) })
    }

    fn saying(summary: &str) -> Arc<Self> {
        Self::new(Some(json!({"summary": summary, "actions": []}).to_string()))
    }

    fn release(&self) {
        self.gate.add_permits(1);
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl ChatBackend for HeldModel {
    fn complete<'a>(&'a self, _req: &'a ChatRequest) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            self.asked.fetch_add(1, Ordering::SeqCst);
            if let Ok(permit) = self.gate.acquire().await {
                permit.forget();
            }
            match lock(&self.reply).clone() {
                Some(content) => Ok(ChatResponse {
                    message: ChatMessage::text(Role::Assistant, content),
                    finish_reason: Some("stop".into()),
                    usage: Some(Usage::new(10, 10)),
                    fallback_used: false,
                    web_search_calls: 0,
                    citations: Vec::new(),
                    web_search_rejected: false,
                    billing: Billing::Metered,
                }),
                None => Err(LlmError::Transport("falhou".into())),
            }
        })
    }
}

/// Mundo com a voz ligada, um modelo escolhido e o `model` simulado.
async fn with_model(tag: &str, model: &Arc<HeldModel>, tuning: Tuning, edit: impl FnOnce(&mut Config)) -> (World, Arc<Fake>, Arc<FakeVoice>) {
    let (w, fake, voice_fake) = world(tag, |cfg| {
        cfg.nanty.connection_id = "oai".into();
        cfg.nanty.model = "modelo".into();
        edit(cfg);
    })
    .await;
    w.state.nanty().set_backend(Arc::clone(model) as Arc<dyn ChatBackend>);
    w.state.nanty().telegram().set_tuning(tuning);
    (w, fake, voice_fake)
}

fn spawn_turn(w: &World) -> tokio::task::JoinHandle<Option<TurnOutcome>> {
    let state = w.state.clone();
    tokio::spawn(async move {
        let nanty = Arc::clone(state.nanty());
        turn::proactive(&state, &nanty).await
    })
}

fn caption_of(fake: &Fake, n: usize) -> String {
    bodies(fake, "sendVoice")[n]["caption"].as_str().unwrap_or_default().to_string()
}

fn spoken_text(voice_fake: &FakeVoice, n: usize) -> String {
    voice_fake.calls("gemini")[n]["input"][0]["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

/// O texto fixo do cartão no Telegram (o aviso de teste não cita projeto cadastrado).
fn why(c: &Card) -> String {
    render::card_text(Lang::PtBr, c)
}

fn notices(w: &World) -> Vec<Map<String, Value>> {
    w.state.nanty().journal_tail(1000).into_iter().filter(|e| e.kind == JournalKind::Voice && e.code == "voice.notice").map(|e| e.params).collect()
}

/// Uma fala solta do Nanty, para entrar na fila depois do resto.
fn queued_text(text: &str) -> ConversationMessage {
    let mut message = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Web, "x", Map::new());
    message.code = None;
    message.text = Some(text.into());
    message
}

fn milestone() -> Card {
    let mut done = card("p1:spec_done", Some("approve_plan"));
    done.code = "nanty.why.spec_done".into();
    done
}

/// O caso do teste da rc.23: uma mensagem só, com o áudio falando o texto do modelo e no tom da tabela
/// do sinal. A conversa tem uma linha com texto e áudio, e a edição usa esse texto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_notice_waits_for_the_text_and_goes_as_one_message_that_speaks_it() {
    let model = HeldModel::saying(SAID);
    let (mut w, fake, voice_fake) =
        with_model("n-one", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    w.start();
    let _out = start_outbox(&w);
    until("conectado", || w.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    let c = milestone();
    let (id, tok, fixed) = (c.id.clone(), c.token.clone().unwrap_or_default(), why(&c));
    add(&w, vec![c]);
    let turning = spawn_turn(&w);
    until("o modelo foi chamado", || model.asked() == 1).await;
    assert!(voice_fake.calls("gemini").is_empty(), "a fala espera o texto");
    model.release();
    until("nota", || fake.calls("sendVoice").len() == 1).await;

    assert_eq!(spoken_text(&voice_fake, 0), SAID, "o áudio fala o texto do modelo");
    assert_eq!(style_of(&voice_fake, 0), json!("warm and upbeat"), "o tom vem da tabela do sinal");
    let note = format!("{SAID}\n{fixed}");
    assert_eq!(caption_of(&fake, 0), note, "o texto do modelo e o porquê do dicionário");
    let markup: Value = serde_json::from_str(bodies(&fake, "sendVoice")[0]["reply_markup"].as_str().unwrap_or_default()).expect("JSON");
    assert_eq!(datas(&markup).len(), 3, "com os botões");

    until("registro", || sent(&w, &id).is_some()).await;
    let rec = sent(&w, &id).expect("registro");
    assert_eq!((rec.kind, rec.text.as_str(), rec.tail.as_deref()), (SentKind::Voice, note.as_str(), Some(fixed.as_str())));
    turning.await.expect("turno");
    drained(&w, &fake).await;
    assert_eq!(fake.sent_texts(), [DRAINED], "nada repetido em texto");
    assert_eq!((fake.calls("sendVoice").len(), voice_fake.calls("gemini").len()), (1, 1), "um áudio por aviso");

    // A conversa: uma mensagem do Nanty, com o texto falado e o áudio, que a janela toca.
    until("áudio na mensagem", || nanty_lines(&w).iter().any(|m| m.audio.is_some())).await;
    let lines: Vec<_> = nanty_lines(&w).into_iter().filter(|m| m.text.as_deref() != Some(DRAINED)).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!((lines[0].text.as_deref(), lines[0].card_ids.as_slice()), (Some(SAID), std::slice::from_ref(&id)));
    let audio = lines[0].audio.clone().expect("áudio");
    assert!(w.state.nanty().read_audio(&audio.id).is_some());
    assert_eq!(notices(&w)[0]["source"], "model");

    // Decidido: a edição usa o texto falado, com a linha final.
    fake.push(click_on(100, OWNER, OWNER, 1, &callback_data(&id, &tok, Answer::No)));
    until("legenda editada", || fake.calls("editMessageCaption").len() == 1).await;
    let edited = bodies(&fake, "editMessageCaption")[0]["caption"].as_str().unwrap_or_default().to_string();
    assert!(edited.starts_with(&note) && edited.len() > note.len(), "{edited}");
    w.stop().await;
}

/// O texto que já está na fila quando a espera da aba acaba é usado na hora.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_text_already_in_the_queue_is_spoken_without_waiting() {
    let model = HeldModel::saying(SAID);
    model.release();
    let (w, fake, voice_fake) = with_model("n-ready", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    add(&w, vec![card(KEY, None)]);
    assert!(matches!(spawn_turn(&w).await.expect("turno"), Some(TurnOutcome::Reply(_))));
    let _out = start_outbox(&w);
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    assert_eq!(spoken_text(&voice_fake, 0), SAID);
    drained(&w, &fake).await;
    assert_eq!(fake.sent_texts(), [DRAINED]);
}

/// Vencido o prazo, sai o aviso com a frase fixa, e o texto tardio segue como mensagem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn past_the_wait_the_notice_speaks_the_fixed_line_and_the_late_text_follows() {
    let model = HeldModel::saying(SAID);
    let (w, fake, voice_fake) = with_model("n-late", &model, Tuning { notice_text_wait: SHORT, notice_total: FOREVER, ..FAST }, |_| {}).await;
    let _out = start_outbox(&w);
    let c = card(KEY, Some("approve_plan"));
    let (id, fixed) = (c.id.clone(), why(&c));
    add(&w, vec![c]);
    let turning = spawn_turn(&w);
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    assert_eq!(spoken_text(&voice_fake, 0), fixed);
    assert_eq!(caption_of(&fake, 0), fixed);
    until("registro", || sent(&w, &id).is_some()).await;
    assert_eq!(sent(&w, &id).and_then(|r| r.tail), None);
    assert_eq!(notices(&w)[0]["reason"], "deadline");

    until("o modelo foi chamado", || model.asked() == 1).await;
    model.release();
    until("texto tardio", || fake.sent_texts() == [SAID]).await;
    turning.await.expect("turno");
    assert_eq!(fake.calls("sendVoice").len(), 1);
    assert_eq!(voice_fake.calls("gemini").len(), 1);
    let lines = nanty_lines(&w);
    assert_eq!(lines.len(), 2, "o aviso com áudio e o texto: {lines:?}");
    assert!(lines.iter().any(|m| m.audio.is_some() && m.text.is_none()) && lines.iter().any(|m| m.audio.is_none() && m.text.as_deref() == Some(SAID)));
}

/// O urgente tem o prazo curto: sai mesmo com o prazo normal longo.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_urgent_notice_has_its_own_short_wait() {
    let model = HeldModel::saying(SAID);
    let tuning = Tuning { notice_text_wait: FOREVER, notice_text_wait_urgent: SHORT, notice_total: FOREVER, ..FAST };
    let (w, fake, _voice) = with_model("n-urgent", &model, tuning, |_| {}).await;
    let _out = start_outbox(&w);
    let mut c = card(KEY, None);
    c.urgency = Some(Urgency::Urgent);
    add(&w, vec![c]);
    let turning = spawn_turn(&w);
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    until("o modelo foi chamado", || model.asked() == 1).await;
    model.release();
    until("texto tardio", || fake.sent_texts() == [SAID]).await;
    turning.await.expect("turno");
}

#[test]
fn the_waits_are_the_provisional_ones_and_never_pass_the_total() {
    let tuning = Tuning::default();
    let mut c = card(KEY, None);
    assert_eq!(outbox::text_wait(&tuning, &c), Duration::from_secs(15));
    c.urgency = Some(Urgency::Urgent);
    assert_eq!(outbox::text_wait(&tuning, &c), Duration::from_secs(8));
    assert_eq!(tuning.notice_total, Duration::from_secs(60));
    assert_eq!(outbox::text_wait(&Tuning { notice_total: SHORT, ..tuning }, &c), SHORT);
}

/// A fala só tem o que sobrou do limite total: presa, o aviso sai em texto, e o texto segue à parte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_speech_lives_inside_the_total_limit() {
    let model = HeldModel::saying(SAID);
    model.release();
    let tuning = Tuning { notice_text_wait: FOREVER, notice_total: Duration::from_millis(200), speech_deadline: FOREVER, ..FAST };
    let (w, fake, voice_fake) = with_model("n-total", &model, tuning, |_| {}).await;
    voice_fake.hold_speech();
    add(&w, vec![card(KEY, Some("approve_plan"))]);
    spawn_turn(&w).await.expect("turno");
    let _out = start_outbox(&w);
    until("a fala começou", || voice_fake.calls("gemini").len() == 1).await;
    assert_eq!(spoken_text(&voice_fake, 0), SAID);
    until("aviso e texto", || fake.calls("sendMessage").len() == 2).await;
    let sends = bodies(&fake, "sendMessage");
    assert_eq!(datas(&sends[0]["reply_markup"]).len(), 3, "o cartão, com os botões");
    assert_eq!(sends[1]["text"], SAID);
    assert!(fake.calls("sendVoice").is_empty());
    assert_eq!(text_reasons(&w), [json!(["deadline"])]);
    voice_fake.release_speech(1);
}

/// Com vários cartões, ninguém espera: cada um fala a frase fixa, e o texto único vai à parte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_several_cards_nobody_waits_and_the_text_follows() {
    let model = HeldModel::saying(SAID);
    let (w, fake, voice_fake) = with_model("n-many", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    let (a, b) = (card(KEY, None), card("p2:plan_waiting", None));
    let fixed = [why(&a), why(&b)];
    add(&w, vec![a, b]);
    let turning = spawn_turn(&w);
    until("o modelo foi chamado", || model.asked() == 1).await;
    let _out = start_outbox(&w);
    until("as duas notas", || fake.calls("sendVoice").len() == 2).await;
    assert_eq!((caption_of(&fake, 0), caption_of(&fake, 1)), (fixed[0].clone(), fixed[1].clone()));
    model.release();
    until("texto", || fake.sent_texts() == [SAID]).await;
    turning.await.expect("turno");
    assert_eq!(voice_fake.calls("gemini").len(), 2);
}

/// Um cartão novo no meio da espera encerra a espera: um aviso não segura o outro.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_card_ends_the_wait_of_the_one_before() {
    let model = HeldModel::saying(SAID);
    let (w, fake, voice_fake) = with_model("n-second", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    let _out = start_outbox(&w);
    let first = card(KEY, None);
    let (id, fixed) = (first.id.clone(), why(&first));
    add(&w, vec![first]);
    let turning = spawn_turn(&w);
    until("o modelo foi chamado", || model.asked() == 1).await;
    add(&w, vec![card("p2:plan_waiting", None)]);
    until("a primeira nota", || fake.calls("sendVoice").len() == 1).await;
    assert_eq!(caption_of(&fake, 0), fixed);
    assert_eq!(notices(&w)[0]["reason"], "several");
    until("registro", || sent(&w, &id).is_some()).await;
    assert_eq!(sent(&w, &id).map(|r| r.tail), Some(None));
    model.release();
    until("texto tardio", || fake.sent_texts() == [SAID]).await;
    turning.await.expect("turno");
    // O segundo agora está sozinho, e o turno dele chega a tempo.
    let second = spawn_turn(&w);
    until("o segundo turno", || model.asked() == 2).await;
    model.release();
    until("a segunda nota", || fake.calls("sendVoice").len() == 2).await;
    second.await.expect("turno");
    assert_eq!(spoken_text(&voice_fake, 1), SAID);
}

/// A falha do modelo e a resposta inválida acabam a espera na hora, com a frase fixa.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_or_invalid_turn_ends_the_wait_with_the_fixed_line() {
    for (tag, reply) in [("n-fail", None), ("n-invalid", Some("não é JSON".to_string()))] {
        let model = HeldModel::new(reply);
        let (w, fake, voice_fake) = with_model(tag, &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
        let _out = start_outbox(&w);
        let c = card(KEY, Some("approve_plan"));
        let fixed = why(&c);
        add(&w, vec![c]);
        let turning = spawn_turn(&w);
        until("o modelo foi chamado", || model.asked() == 1).await;
        assert!(voice_fake.calls("gemini").is_empty(), "espera o turno");
        model.release();
        until("nota", || fake.calls("sendVoice").len() == 1).await;
        assert_eq!(caption_of(&fake, 0), fixed);
        turning.await.expect("turno");
        drained(&w, &fake).await;
        assert_eq!(fake.sent_texts(), [DRAINED]);
        assert_eq!(notices(&w)[0]["reason"], "no_text");
    }
}

/// Sem orçamento, ninguém espera, e o aviso vai em texto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_budget_the_notice_does_not_wait() {
    let model = HeldModel::saying(SAID);
    let (w, fake, voice_fake) = with_model("n-budget", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    let nanty = Arc::clone(w.state.nanty());
    let (day, limit) = (nanty.now().day_key(), w.state.config().read().await.nanty.daily_budget_tokens);
    nanty.update(|st| {
        st.add_spend(&day, limit, 0, None);
        true
    });
    let _out = start_outbox(&w);
    add(&w, vec![card(KEY, Some("approve_plan"))]);
    until("texto", || fake.calls("sendMessage").len() == 1).await;
    assert!(fake.calls("sendVoice").is_empty() && voice_fake.calls("gemini").is_empty());
    assert_eq!(model.asked(), 0);
}

/// Spec 140 T02, revisão 1: dois cartões sem orçamento. A fala do primeiro acaba enquanto o segundo
/// pede a vez, e nenhum dos dois fica preso.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_cards_without_budget_both_go_in_text() {
    let model = HeldModel::saying(SAID);
    let (w, fake, voice_fake) = with_model("n-two-budget", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    let nanty = Arc::clone(w.state.nanty());
    let (day, limit) = (nanty.now().day_key(), w.state.config().read().await.nanty.daily_budget_tokens);
    nanty.update(|st| {
        st.add_spend(&day, limit, 0, None);
        true
    });
    let (a, b) = (card(KEY, Some("approve_plan")), card("p2:plan_waiting", Some("approve_plan")));
    let ids = [a.id.clone(), b.id.clone()];
    add(&w, vec![a, b]);
    let _out = start_outbox(&w);
    until("os dois avisos", || fake.calls("sendMessage").len() == 2).await;
    assert!(bodies(&fake, "sendMessage").iter().all(|b| datas(&b["reply_markup"]).len() == 3), "com os botões");
    until("registros", || ids.iter().all(|id| sent(&w, id).is_some())).await;
    assert!(fake.calls("sendVoice").is_empty() && voice_fake.calls("gemini").is_empty());
    assert_eq!(model.asked(), 0);
}

/// O turno rápido: o texto entra na fila antes de o cartão acabar a acomodação. Ele não sai solto, e
/// a nota é uma só, com a fala do modelo (o achado da rc.23, no modelo rápido).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_text_ready_during_the_settle_waits_for_the_card() {
    let model = HeldModel::saying(SAID);
    model.release();
    let tuning = Tuning { send_settle: FOREVER, notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST };
    let (w, fake, voice_fake) = with_model("n-settle", &model, tuning, |_| {}).await;
    let _out = start_outbox(&w);
    let c = card(KEY, None);
    let fixed = why(&c);
    add(&w, vec![c]);
    assert!(matches!(spawn_turn(&w).await.expect("turno"), Some(TurnOutcome::Reply(_))));
    // A fila é em ordem: o marcador só passa com o texto do turno ainda retido.
    drained(&w, &fake).await;
    assert_eq!(fake.sent_texts(), [DRAINED], "o texto espera o cartão");
    assert!(voice_fake.calls("gemini").is_empty());

    w.state.nanty().telegram().set_tuning(Tuning { send_settle: Duration::ZERO, ..tuning });
    w.state.nanty().telegram().outbox().wake();
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    assert_eq!(spoken_text(&voice_fake, 0), SAID);
    assert_eq!(caption_of(&fake, 0), format!("{SAID}\n{fixed}"));
    let nanty = w.state.nanty();
    nanty.push_message(&queued_text(DRAINED));
    until("o marcador de novo", || fake.sent_texts() == [DRAINED, DRAINED]).await;
}

/// O cartão saiu durante a acomodação: o texto segue, sem fala.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_that_leaves_during_the_settle_frees_its_text() {
    let model = HeldModel::saying(SAID);
    model.release();
    let (w, fake, voice_fake) = with_model("n-settle-gone", &model, Tuning { send_settle: FOREVER, ..FAST }, |_| {}).await;
    let _out = start_outbox(&w);
    let c = card(KEY, None);
    let gone = c.id.clone();
    add(&w, vec![c]);
    spawn_turn(&w).await.expect("turno");
    drained(&w, &fake).await;
    assert_eq!(fake.sent_texts(), [DRAINED]);
    w.state.nanty().update(|st| {
        st.cards.retain(|c| c.id != gone);
        true
    });
    w.state.nanty().telegram().outbox().wake();
    until("texto", || fake.sent_texts() == [DRAINED, SAID]).await;
    assert!(voice_fake.calls("gemini").is_empty() && fake.calls("sendVoice").is_empty());
}

/// Com a voz desligada, a acomodação não segura o texto, como antes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_the_voice_off_the_settle_does_not_hold_the_text() {
    let model = HeldModel::saying(SAID);
    model.release();
    let (w, fake, _voice) = with_model("n-settle-off", &model, Tuning { send_settle: FOREVER, ..FAST }, |cfg| cfg.nanty.voice.enabled = false).await;
    let _out = start_outbox(&w);
    add(&w, vec![card(KEY, None)]);
    spawn_turn(&w).await.expect("turno");
    until("texto", || fake.sent_texts() == [SAID]).await;
}

/// A fala do texto falhou: o aviso sai em texto, e o texto do modelo também, sem nada perdido.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_speech_of_the_text_keeps_the_notice_and_the_text() {
    let model = HeldModel::saying(SAID);
    model.release();
    let (w, fake, voice_fake) = with_model("n-speech-fail", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    lock(&voice_fake.gemini).push_back(Reply::Status(500));
    add(&w, vec![card(KEY, Some("approve_plan"))]);
    spawn_turn(&w).await.expect("turno");
    let _out = start_outbox(&w);
    until("aviso e texto", || fake.calls("sendMessage").len() == 2).await;
    let sends = bodies(&fake, "sendMessage");
    assert_eq!(datas(&sends[0]["reply_markup"]).len(), 3);
    assert_eq!(sends[1]["text"], SAID);
    assert_eq!(text_reasons(&w), [json!(["provider_error"])]);
    assert!(fake.calls("sendVoice").is_empty());
}

/// A nota recusada: o cartão sai em texto com os botões, e o texto do modelo segue à parte. O áudio
/// já pago fica na mensagem do texto, e a edição é a do cartão em texto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_note_with_the_text_falls_back_to_the_card_and_the_message() {
    let model = HeldModel::saying(SAID);
    model.release();
    let (w, fake, _voice) = with_model("n-refused", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    lock(&fake.voice_script).push_back(Scripted::Status(400));
    let c = card(KEY, Some("approve_plan"));
    let (id, fixed) = (c.id.clone(), why(&c));
    add(&w, vec![c]);
    spawn_turn(&w).await.expect("turno");
    let _out = start_outbox(&w);
    until("aviso e texto", || fake.calls("sendMessage").len() == 2).await;
    let sends = bodies(&fake, "sendMessage");
    assert_eq!((sends[0]["text"].as_str(), datas(&sends[0]["reply_markup"]).len()), (Some(fixed.as_str()), 3));
    assert_eq!(sends[1]["text"], SAID);
    assert_eq!(fake.calls("sendVoice").len(), 1);
    until("registro", || sent(&w, &id).is_some()).await;
    let rec = sent(&w, &id).expect("registro");
    assert_eq!((rec.kind, rec.text.as_str(), rec.tail), (SentKind::Text, fixed.as_str(), None));
    let lines = nanty_lines(&w);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].audio.is_some() && lines[0].text.as_deref() == Some(SAID));
}

/// A falha passageira da nota não paga outra fala nem perde o texto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_passing_failure_keeps_the_spoken_text_for_the_next_try() {
    let model = HeldModel::saying(SAID);
    model.release();
    let (w, fake, voice_fake) = with_model("n-retry", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    lock(&fake.voice_script).push_back(Scripted::Retry(1));
    add(&w, vec![card(KEY, None)]);
    spawn_turn(&w).await.expect("turno");
    let _out = start_outbox(&w);
    until("segunda tentativa", || fake.calls("sendVoice").len() == 2).await;
    drained(&w, &fake).await;
    assert_eq!(caption_of(&fake, 1), caption_of(&fake, 0));
    assert_eq!(voice_fake.calls("gemini").len(), 1, "a fala não é paga duas vezes");
    assert_eq!(fake.sent_texts(), [DRAINED]);
}

/// O que é decidido enquanto o aviso espera o texto não custa fala.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_decided_during_the_wait_costs_no_speech_and_frees_its_text() {
    let model = HeldModel::saying(SAID);
    let (w, fake, voice_fake) = with_model("n-gone", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    let _out = start_outbox(&w);
    let c = card(KEY, None);
    let gone = c.id.clone();
    add(&w, vec![c]);
    let turning = spawn_turn(&w);
    until("o modelo foi chamado", || model.asked() == 1).await;
    w.state.nanty().update(|st| {
        st.cards.retain(|c| c.id != gone);
        true
    });
    model.release();
    until("texto", || fake.sent_texts() == [SAID]).await;
    turning.await.expect("turno");
    assert!(voice_fake.calls("gemini").is_empty() && fake.calls("sendVoice").is_empty());
}

/// A legenda cortada não perde o porquê, o texto completo vai à parte, e a edição fica em 1024.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_note_keeps_the_reason_in_the_caption_and_in_the_edit() {
    let long = "Uma frase do modelo sobre o aviso. ".repeat(17);
    let long = long.trim();
    assert!(long.chars().count() <= turn::MAX_SUMMARY_CHARS);
    let model = HeldModel::saying(long);
    model.release();
    let (w, fake, _voice) = with_model("n-long", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    let mut c = card(KEY, None);
    c.params.insert("since".into(), json!("quase pronto ".repeat(40).trim()));
    let (id, fixed) = (c.id.clone(), why(&c));
    add(&w, vec![c]);
    spawn_turn(&w).await.expect("turno");
    let _out = start_outbox(&w);
    until("texto à parte", || !fake.sent_texts().is_empty()).await;
    let caption = caption_of(&fake, 0);
    assert!(format::units(&caption) <= format::MAX_CAPTION_CHARS && caption.ends_with(&fixed), "{caption}");
    assert_eq!(fake.sent_texts(), [format!("{long}\n{fixed}")]);
    until("registro", || sent(&w, &id).is_some()).await;
    w.state.nanty().update(|st| {
        st.cards.retain(|c| c.id != id);
        true
    });
    until("legenda editada", || fake.calls("editMessageCaption").len() == 1).await;
    let edited = bodies(&fake, "editMessageCaption")[0]["caption"].as_str().unwrap_or_default().to_string();
    let line = tr(Lang::PtBr, "nanty.telegram.expired", &[]);
    assert!(format::units(&edited) <= format::MAX_CAPTION_CHARS, "{}", format::units(&edited));
    assert!(edited.ends_with(&format!("{fixed}\n\n{line}")), "{edited}");
}

/// Spec 140 T14 (revisão 1): o texto completo à parte de um aviso maior que uma mensagem passa pela
/// divisão: sai em partes que cabem, sem corte, e a nota continua como na T02 (a fala do modelo, a
/// legenda com o porquê e os botões nela).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_note_longer_than_a_message_goes_apart_in_parts() {
    let model = HeldModel::saying(SAID);
    model.release();
    let (w, fake, voice_fake) = with_model("n-parts", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    let mut c = card(KEY, None);
    c.params.insert("since".into(), json!("quase pronto ".repeat(400).trim()));
    let fixed = why(&c);
    add(&w, vec![c]);
    spawn_turn(&w).await.expect("turno");
    let _out = start_outbox(&w);
    until("as partes", || fake.sent_texts().len() == 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.calls("sendVoice").len(), 1, "um áudio por aviso");
    assert_eq!(voice_fake.calls("gemini")[0]["input"][0]["content"][0]["text"], json!(SAID), "a fala do modelo, como na T02");
    let texts = fake.sent_texts();
    assert_eq!(texts.len(), 2);
    assert!(texts.iter().all(|t| format::units(t) <= format::MAX_MESSAGE_CHARS && !t.contains('…')));
    let joined: Vec<&str> = texts.iter().flat_map(|t| t.split_whitespace()).collect();
    let note = format!("{SAID}\n{fixed}");
    assert_eq!(joined, note.split_whitespace().collect::<Vec<_>>(), "nada se perde");
}

#[test]
fn the_closing_caption_stays_inside_the_limit_in_telegram_units() {
    let text = format!("{}\nPorquê.", "ç𝄞 ".repeat(400));
    let caption = outbox::closed_caption(&text, Some("Porquê."), "Linha final.");
    assert!(format::units(&caption) <= format::MAX_CAPTION_CHARS);
    assert!(caption.ends_with("Porquê.\n\nLinha final."), "{caption}");
    // Sem o fecho, como antes.
    assert_eq!(outbox::closed_caption("Curto.", None, "Linha."), "Curto.\n\nLinha.");
}

/// Com a voz desligada, nada muda: o cartão não espera, e o texto vai depois.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_the_voice_off_nothing_waits_and_there_is_no_audio() {
    let model = HeldModel::saying(SAID);
    let tuning = Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST };
    let (w, fake, voice_fake) = with_model("n-off", &model, tuning, |cfg| cfg.nanty.voice.enabled = false).await;
    let _out = start_outbox(&w);
    add(&w, vec![card(KEY, Some("approve_plan"))]);
    let turning = spawn_turn(&w);
    until("cartão", || fake.calls("sendMessage").len() == 1).await;
    until("o modelo foi chamado", || model.asked() == 1).await;
    model.release();
    until("texto", || fake.calls("sendMessage").len() == 2).await;
    turning.await.expect("turno");
    assert_eq!(fake.sent_texts()[1], SAID);
    assert!(fake.calls("sendVoice").is_empty() && voice_fake.calls("gemini").is_empty());
    assert!(notices(&w).is_empty());
}

/// Spec 141 T12 (sugestão do Revisor na T08): o texto do modelo é longo (no teto do resumo) e o
/// cartão também, e juntos passam do que o Telegram mostra em [`format::MAX_PARTS`] partes. A nota
/// nunca fala o que o texto não mostra: ela fala a frase fixa do cartão, o motivo `too_long` fica no
/// registro, e o texto do modelo segue inteiro como mensagem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_model_text_that_does_not_fit_speaks_the_fixed_line_and_goes_as_a_message() {
    let long = "Uma frase do modelo sobre o aviso. ".repeat(17);
    let long = long.trim().to_string();
    assert!(long.chars().count() <= turn::MAX_SUMMARY_CHARS);
    let model = HeldModel::saying(&long);
    model.release();
    let (w, fake, voice_fake) = with_model("n-toolong", &model, Tuning { notice_text_wait: FOREVER, notice_total: FOREVER, ..FAST }, |_| {}).await;
    let mut c = card(KEY, None);
    c.params.insert("since".into(), json!("quase pronto ".repeat(1000).trim()));
    let id = c.id.clone();
    add(&w, vec![c]);
    spawn_turn(&w).await.expect("turno");
    let _out = start_outbox(&w);
    until("nota", || fake.calls("sendVoice").len() == 1).await;
    until("o texto do modelo", || fake.sent_texts().iter().any(|t| t.contains(&long))).await;
    assert_eq!(fake.calls("sendVoice").len(), 1, "uma nota só");
    // A frase fixa do cartão (cortada para uma mensagem) sai em trechos de fim de frase, numa nota só.
    let spoken: Vec<String> = (0..voice_fake.calls("gemini").len()).map(|n| spoken_text(&voice_fake, n)).collect();
    let spoken = spoken.join(" ");
    assert!(!spoken.contains("Uma frase do modelo"), "a nota não fala o texto do modelo: {spoken}");
    assert!(spoken.contains("quase pronto"), "a nota fala a frase fixa do cartão: {spoken}");
    assert!(crate::server::nanty::voice::speakable(&spoken), "o que a nota fala aparece inteiro");
    assert_eq!(notices(&w)[0]["source"], "fixed");
    assert_eq!(notices(&w)[0]["reason"], "too_long");
    until("registro", || sent(&w, &id).is_some()).await;
    assert_eq!(sent(&w, &id).and_then(|r| r.tail), None, "a nota é a do cartão, sem o texto do modelo");
}
