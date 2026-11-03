//! Levar ao Arquiteto pelo Telegram (spec 140 T15, C-15), só com o servidor falso do Telegram e o
//! modelo simulado: o texto completo vem antes do botão; o cartão que não cabe numa mensagem sai sem o
//! "sim", com a linha que aponta a janela (o padrão da T07, F-04); o clique pelo Telegram só deixa o
//! rascunho, sem turno do Arquiteto.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use super::tests::{click_on, fake_server, live_after, paired, temp, token, until, Fake, World, FAST, OWNER};
use super::voice_tests::{bodies, datas, start_outbox};
use super::*;
use crate::agents::chat::ChatBackend;
use crate::config::Config;
use crate::server::nanty::agent::tests::{call, say, Scripted};
use crate::server::nanty::chat::tests::{base_config, write_project};
use crate::server::nanty::{clock, draft, render};

fn owner_text(update_id: i64, text: &str) -> Value {
    let date = i64::try_from(clock::now_ms() / 1000).unwrap_or(0);
    json!({"update_id": update_id, "message": {
        "message_id": update_id, "date": date,
        "chat": {"id": OWNER, "type": "private"},
        "from": {"id": OWNER, "is_bot": false},
        "text": text,
    }})
}

/// Pareado, com a escuta ligada, o projeto `p1` (Loja) no disco e o modelo simulado.
async fn world(tag: &str, model: Arc<Scripted>, edit: impl FnOnce(&mut Config)) -> (World, Arc<Fake>) {
    let tk = token(tag);
    let (fake, base) = fake_server(&tk).await;
    let dir: PathBuf = temp(tag);
    let mut cfg = base_config(&dir, &[("p1", "Loja")]);
    write_project(&dir.join("p1"), "Plano da loja", "log", None);
    cfg.nanty.telegram.bot_token = tk;
    paired(&mut cfg);
    edit(&mut cfg);
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

fn draft_card(w: &World) -> Option<Card> {
    w.state.nanty().snapshot().cards.into_iter().find(draft::is_draft_card)
}

/// O envio do cartão ao Telegram: a mensagem com os botões dele.
fn card_body(fake: &Fake, card: &Card) -> Option<Value> {
    bodies(fake, "sendMessage").into_iter().find(|b| datas(&b["reply_markup"]).iter().any(|d| d.contains(&card.id)))
}

fn propose(text: &str) -> Arc<Scripted> {
    Scripted::new(vec![call(draft::PROPOSE_TO_ARCHITECT, json!({ "project_id": "p1", "text": text })), say("Deixei o cartão.")])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_whole_text_comes_before_the_button_and_the_click_only_leaves_the_draft() {
    let idea = "Separar o checkout em duas etapas, com o frete antes do pagamento.";
    let (w, fake) = world("t15-short", propose(idea), |_| {}).await;
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "leve a ideia do checkout ao Arquiteto da loja"));
    until("o cartão", || draft_card(&w).is_some()).await;
    let card = draft_card(&w).expect("cartão");
    assert_eq!(card.requested, Some(Channel::Telegram));
    until("o envio do cartão", || card_body(&fake, &card).is_some()).await;
    let body = card_body(&fake, &card).expect("enviado");
    let text = body["text"].as_str().unwrap_or_default();
    assert!(text.contains(idea), "o texto inteiro na mensagem do botão: {text}");
    let token = card.token.clone().unwrap_or_default();
    assert_eq!(datas(&body["reply_markup"]), [callback_data(&card.id, &token, Answer::Yes), callback_data(&card.id, &token, Answer::No)]);
    let label = body["reply_markup"]["inline_keyboard"][0][0]["text"].as_str().unwrap_or_default().to_string();
    assert_eq!(label, tr(Lang::PtBr, "nanty.action.architect_draft", &[]));

    let sent_id = || w.state.nanty().snapshot().telegram_sent.iter().find(|r| r.card_id == card.id).and_then(|r| r.message_id);
    until("o registro do envio", || sent_id().is_some()).await;
    let message_id = sent_id().expect("enviado");
    fake.push(click_on(51, OWNER, OWNER, message_id, &callback_data(&card.id, &token, Answer::Yes)));
    until("o rascunho", || !w.state.nanty().snapshot().architect_drafts.is_empty()).await;
    let drafts = w.state.nanty().snapshot().architect_drafts;
    assert_eq!((drafts[0].text.as_str(), drafts[0].channel), (idea, Channel::Telegram));
    let chat = std::fs::read_to_string(w.dir.join("p1/.sdd/specs/.chat.jsonl")).unwrap_or_default();
    assert_eq!(chat, "", "o clique não abre turno do Arquiteto");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_too_long_for_telegram_goes_without_yes_and_the_window_decides() {
    let idea = format!("Plano em detalhe. {}", "Cada etapa do checkout precisa do seu teste e da sua tela. ".repeat(80));
    let (w, fake) = world("t15-long", propose(&idea), |c| c.nanty.chat.architect_draft_chars = 8000).await;
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "leve o plano ao Arquiteto"));
    until("o cartão", || draft_card(&w).is_some()).await;
    let card = draft_card(&w).expect("cartão");
    until("o envio do cartão", || card_body(&fake, &card).is_some()).await;
    let body = card_body(&fake, &card).expect("enviado");
    let text = body["text"].as_str().unwrap_or_default();
    assert!(text.ends_with(&tr(Lang::PtBr, "nanty.telegram.card_in_window", &[])), "{text}");
    assert!(format::units(text) <= format::MAX_MESSAGE_CHARS);
    let token = card.token.clone().unwrap_or_default();
    assert_eq!(datas(&body["reply_markup"]), [callback_data(&card.id, &token, Answer::No)], "sem o sim");
    // A janela tem o texto inteiro e decide normalmente.
    assert!(render::card_text(Lang::PtBr, &card).contains(idea.trim()));
    let decide = json!({ "token": token, "answer": "yes" }).to_string();
    let out = crate::server::nanty::api::post_decide(axum::extract::State(w.state.clone()), axum::extract::Path(card.id.clone()), axum::body::Bytes::from(decide)).await.expect("decide").0;
    assert_eq!(out["outcome"], json!("architect_draft_saved"));
}
