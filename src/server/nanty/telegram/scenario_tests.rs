//! O cenário proativo pelo Telegram (spec 140 T12, C-14), só com o servidor falso do Telegram, o
//! Gemini simulado e um modelo simulado preso (nunca o Telegram real nem chave real): o aviso sai
//! primeiro e nunca espera a análise; o cenário vem depois, em texto, sem botões, mesmo com a voz
//! ligada; e fica fora da rajada.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::{Notify, Semaphore};

use super::tests::{until, World};
use super::voice_tests::{add, bodies, card, sent, start_outbox, world};
use super::*;
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::{ChatRequest, ChatResponse};
use crate::agents::LlmError;
use crate::config::{Config, Project, Urgency};
use crate::i18n::tr_plural;
use crate::server::nanty::agent::tests::{say, Scripted};
use crate::server::nanty::store::ConversationMessage;
use crate::server::nanty::{clock, new_id, scenario};

const HONEST: &str = "A tarefa parou nos testes. A causa provável é uma dependência nova; eu olharia a saída dos testes antes de retomar.";

/// O roteiro preso: cada pedido espera uma licença do teste.
struct Held {
    script: Arc<Scripted>,
    gate: Semaphore,
    asked: Notify,
}

impl ChatBackend for Held {
    fn complete<'a>(&'a self, req: &'a ChatRequest) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            self.asked.notify_one();
            self.gate.acquire().await.expect("portão").forget();
            self.script.complete(req).await
        })
    }
}

/// A pasta do projeto `p1` dentro da pasta do mundo de teste (`tests::temp`).
fn project_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("nants-nanty-t06-{}-{tag}", std::process::id())).join("p1")
}

/// O mundo pareado, com a voz ligada, o cenário ligado, o projeto `p1` e o modelo preso.
async fn scenario_world(tag: &str, edit: impl FnOnce(&mut Config)) -> (World, Arc<tests::Fake>, Arc<Held>) {
    let (w, fake, _voice) = world(tag, |cfg| {
        cfg.nanty.connection_id = "oai".into();
        cfg.nanty.model = "modelo".into();
        cfg.nanty.analysis.proactive = true;
        cfg.projects.push(Project {
            id: "p1".into(),
            name: "Loja".into(),
            path: project_dir(tag).to_str().expect("utf-8").into(),
            created_at: "2026-10-30T00:00:00Z".into(),
        });
        edit(cfg);
    })
    .await;
    std::fs::create_dir_all(project_dir(tag)).expect("projeto");
    let model = Arc::new(Held { script: Scripted::new(vec![say(HONEST)]), gate: Semaphore::new(0), asked: Notify::new() });
    w.state.nanty().set_backend(Arc::clone(&model) as Arc<dyn ChatBackend>);
    (w, fake, model)
}

fn blocked(id: &str) -> Card {
    let mut c = card("p1:run_blocked", None);
    c.id = id.into();
    c.project_id = Some("p1".into());
    c.code = "nanty.why.run_blocked".into();
    c.urgency = Some(Urgency::Normal);
    c.params.insert("task_id".into(), json!("T02"));
    c.params.insert("spec_id".into(), json!("001-demo"));
    c
}

fn lead(lang: Lang) -> String {
    tr(lang, scenario::LEAD, &[])
}

/// As chamadas que levam texto ao chat, em ordem.
fn order(fake: &tests::Fake) -> Vec<(String, Value)> {
    tests::lock(&fake.calls).iter().filter(|(m, _, _)| m == "sendMessage" || m == "sendVoice").map(|(m, b, _)| (m.clone(), b.clone())).collect()
}

fn mentions_scenario(fake: &tests::Fake) -> bool {
    let captions = bodies(fake, "sendVoice").into_iter().filter_map(|b| b["caption"].as_str().map(str::to_string));
    fake.sent_texts().into_iter().chain(captions).any(|t| t.contains("dependência"))
}

/// O caso do critério 7: com o modelo da análise preso, o aviso sai, outro aviso também sai, e o
/// cenário só vem depois da liberação, em texto, sem botões, como mensagem à parte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_notice_never_waits_for_the_analysis_and_the_scenario_comes_after_as_plain_text() {
    let (w, fake, model) = scenario_world("sc-first", |_| {}).await;
    let _out = start_outbox(&w);
    add(&w, vec![blocked("c1")]);
    until("o aviso saiu", || sent(&w, "c1").is_some_and(|r| r.message_id.is_some())).await;

    let nanty = Arc::clone(w.state.nanty());
    let running = scenario::tick(&w.state, &nanty).await.expect("começou");
    model.asked.notified().await;

    // Com a análise presa, um aviso novo sai inteiro.
    let other = card("p1:plan_waiting", None);
    let other_id = other.id.clone();
    add(&w, vec![other]);
    until("o outro aviso saiu", || sent(&w, &other_id).is_some_and(|r| r.message_id.is_some())).await;
    assert!(!mentions_scenario(&fake), "nada do cenário antes da liberação");

    model.gate.add_permits(1);
    running.await.expect("tarefa");
    until("o cenário saiu", || fake.sent_texts().iter().any(|t| t.contains("dependência"))).await;

    let calls = order(&fake);
    let at = calls.iter().position(|(_, b)| b["text"].as_str().is_some_and(|t| t.contains("dependência"))).expect("cenário");
    let (method, body) = &calls[at];
    assert_eq!(method, "sendMessage", "em texto, mesmo com a voz ligada");
    assert!(body.get("reply_markup").is_none_or(Value::is_null), "sem botões: {body}");
    let text = body["text"].as_str().unwrap_or_default();
    assert!(text.starts_with(&lead(Lang::PtBr)), "a linha fixa abre: {text}");
    // Os dois avisos saíram antes.
    assert_eq!(at, calls.len() - 1, "{calls:?}");
    assert!(calls.len() >= 3, "{calls:?}");
    // Na conversa, sem áudio e sem cartão.
    let message = w.state.nanty().conversation().into_iter().find(|m| m.code.as_deref() == Some(scenario::LEAD)).expect("na conversa");
    assert!(message.audio.is_none() && message.card_ids.is_empty());
}

/// A rajada: o aviso que saiu no resumo da fila não ganha cenário, e a mensagem do cenário nunca vira
/// uma linha do resumo.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_scenario_stays_out_of_the_burst() {
    let (w, fake, model) = scenario_world("sc-burst", |_| {}).await;
    let mut cards: Vec<Card> = (0..super::outbox::MAX_BURST + 1).map(|i| card(&format!("p1:x{i}"), None)).collect();
    cards.push(blocked("c1"));
    add(&w, cards);
    let mut message = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Web, scenario::LEAD, Map::new());
    message.text = Some(HONEST.into());
    w.state.nanty().push_message(&message);
    let _out = start_outbox(&w);
    until("o cenário saiu à parte", || fake.sent_texts().iter().any(|t| t.contains("dependência"))).await;
    let texts = fake.sent_texts();
    let batch = &texts[0];
    assert!(batch.starts_with(&tr_plural(Lang::PtBr, "nanty.telegram.batch", 7, &[])), "{batch}");
    assert!(!batch.contains("dependência"), "o cenário não entrou no resumo da fila");
    let alone = texts.iter().find(|t| t.contains("dependência")).expect("à parte");
    assert!(alone.contains(HONEST), "inteiro: {alone}");

    // O aviso da rajada não tem cenário.
    until("registrado", || sent(&w, "c1").is_some()).await;
    assert_eq!(sent(&w, "c1").and_then(|r| r.message_id), None);
    let nanty = Arc::clone(w.state.nanty());
    assert!(scenario::tick(&w.state, &nanty).await.is_none());
    let skipped: Vec<_> = nanty.journal_tail(100).into_iter().filter(|e| e.code == "scenario.skipped").collect();
    assert_eq!(skipped[0].params["reason"], "burst");
    model.gate.add_permits(1);
    assert!(model.script.requests().is_empty());
}

/// Revisão 1 (F-01): na borda da rajada, o cenário nunca entra no resumo da fila. Com exatamente o
/// teto de avisos e um cenário, e com um a menos e dois cenários, tudo sai item a item, e cada cenário
/// sai inteiro, com a linha fixa.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_scenario_never_enters_the_burst_at_the_edge() {
    let max = super::outbox::MAX_BURST;
    for (tag, others, scenarios) in [("sc-edge-a", max, 1), ("sc-edge-b", max - 1, 2)] {
        let (w, fake, _model) = scenario_world(tag, |cfg| cfg.nanty.voice.enabled = false).await;
        add(&w, (0..others).map(|i| card(&format!("p1:x{i}"), None)).collect());
        for n in 0..scenarios {
            let mut message = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Web, scenario::LEAD, Map::new());
            message.text = Some(format!("{HONEST} Caso {n}."));
            w.state.nanty().push_message(&message);
        }
        let _out = start_outbox(&w);
        until("os cenários saíram", || fake.sent_texts().iter().filter(|t| t.contains("dependência")).count() == scenarios).await;
        for text in fake.sent_texts() {
            assert!(!text.contains("de uma vez. Para não lotar"), "nenhum resumo da fila: {text}");
            if text.contains("dependência") {
                assert!(text.starts_with(&lead(Lang::PtBr)), "o cenário sai inteiro, com a linha fixa: {text}");
            }
        }
    }
}
