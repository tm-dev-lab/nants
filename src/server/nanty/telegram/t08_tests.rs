//! Testes da conversa pelo Telegram (spec 138 T08), só com o servidor falso do Telegram de
//! [`super::tests`] e o modelo simulado (nunca o Telegram real nem chave real): a lista de comandos
//! publicada por idioma, a resposta no canal onde a pessoa escreveu (a da janela não vai ao
//! Telegram), a resposta sem a espera da aba visível e o pausar que vira cartão com botões.

use std::time::Duration;

use serde_json::{json, Value};

use super::tests::{click_on, fake_server, live_after, paired, temp, token, until, Fake, World, OWNER};
use super::*;
use crate::agents::chat::ChatBackend;
use crate::server::nanty::chat::tests::{base_config, write_project, FakeModel};
use crate::server::nanty::chat::{self as conversation, command_menu};
use crate::server::nanty::render;
use crate::server::nanty::store::Speaker;

fn start_outbox(w: &World) -> tokio::task::JoinHandle<()> {
    tokio::spawn(outbox::run(w.state.clone(), w.state.shutdown_signal()))
}

fn owner_text(update_id: i64, text: &str) -> Value {
    let date = i64::try_from(crate::server::nanty::clock::now_ms() / 1000).unwrap_or(0);
    json!({"update_id": update_id, "message": {
        "message_id": update_id, "date": date,
        "chat": {"id": OWNER, "type": "private"},
        "from": {"id": OWNER, "is_bot": false},
        "text": text,
    }})
}

/// Mundo pareado com os projetos Loja e Blog, o modelo simulado e a escuta e a fila ligadas.
async fn world(tag: &str, model: std::sync::Arc<FakeModel>) -> (World, std::sync::Arc<Fake>) {
    let tk = token(tag);
    let (fake, base) = fake_server(&tk).await;
    let dir = temp(tag);
    let mut cfg = base_config(&dir, &[("p1", "Loja"), ("p2", "Blog")]);
    cfg.nanty.telegram.bot_token = tk;
    paired(&mut cfg);
    write_project(&dir.join("p1"), "Vitrine", "nada", None);
    write_project(&dir.join("p2"), "Posts", "nada", None);
    let mut w = World::new(dir, &base, cfg);
    w.state.nanty().set_backend(model as std::sync::Arc<dyn ChatBackend>);
    w.start();
    until("conectado", || w.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    (w, fake)
}

fn sends(fake: &Fake) -> Vec<Value> {
    fake.calls("sendMessage").into_iter().map(|(b, _)| b).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_are_published_per_language_when_the_bot_answers() {
    let (w, fake) = world("t08-menu", FakeModel::new(&[])).await;
    until("comandos publicados", || fake.calls("setMyCommands").len() == 3).await;
    let calls: Vec<Value> = fake.calls("setMyCommands").into_iter().map(|(b, _)| b).collect();
    let by_code = |code: Option<&str>| calls.iter().find(|b| b["language_code"].as_str() == code).cloned().expect("lista");
    for (code, lang) in [(Some("pt"), Lang::PtBr), (Some("en"), Lang::En), (None, Lang::PtBr)] {
        let body = by_code(code);
        let expected: Vec<Value> =
            command_menu(lang).into_iter().map(|(name, about)| json!({"command": name, "description": about})).collect();
        assert_eq!(body["commands"], Value::Array(expected), "{code:?}");
    }
    let pt_body = by_code(Some("pt"));
    let pt_names: Vec<&str> = pt_body["commands"].as_array().map(|l| l.iter().filter_map(|c| c["command"].as_str()).collect()).unwrap_or_default();
    assert_eq!(pt_names, ["status", "resumo", "pausar", "silencio", "ajuda"]);
    drop(w);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reply_goes_where_the_person_wrote() {
    let reply = r#"{"summary": "Tudo calmo na Loja.", "actions": []}"#;
    let (w, fake) = world("t08-channel", FakeModel::new(&[reply, reply])).await;
    let _out = start_outbox(&w);
    // Uma aba visível segura os avisos, mas não a resposta a quem escreveu no Telegram.
    w.state.nanty().telegram().outbox().presence("aba1", true, crate::server::nanty::clock::now_ms());

    fake.push(owner_text(50, "/status"));
    until("resposta ao comando", || sends(&fake).len() == 1).await;
    let text = sends(&fake)[0]["text"].as_str().unwrap_or_default().to_string();
    assert!(text.starts_with(&tr(Lang::PtBr, "nanty.chat.status", &[("waiting", "0"), ("done", "0"), ("planned", "1")])), "{text}");

    fake.push(owner_text(51, "como está a Loja?"));
    until("resposta do modelo", || sends(&fake).len() == 2).await;
    assert_eq!(sends(&fake)[1]["text"], "Tudo calmo na Loja.");

    // A mesma conversa pela janela: a resposta fica nela.
    let secrets = w.state.secret_set().await;
    let person = conversation::person_message(w.state.nanty(), &secrets, Channel::Web, "e a Loja agora?").expect("mensagem");
    let answer = conversation::reply(&w.state, &person).await.expect("resposta");
    assert_eq!((answer.channel, answer.reply_to.as_deref()), (Channel::Web, Some(person.id.as_str())));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(sends(&fake).len(), 2, "a resposta à janela não vai ao Telegram");

    // A conversa é uma só, com a origem de cada mensagem.
    let conv = w.state.nanty().conversation();
    let origin: Vec<(Speaker, Channel)> = conv.iter().map(|m| (m.from, m.channel)).collect();
    assert_eq!(
        origin,
        [
            (Speaker::Person, Channel::Telegram),
            (Speaker::Nanty, Channel::Telegram),
            (Speaker::Person, Channel::Telegram),
            (Speaker::Nanty, Channel::Telegram),
            (Speaker::Person, Channel::Web),
            (Speaker::Nanty, Channel::Web),
        ]
    );
    assert_eq!(render::message_text(Lang::PtBr, &conv[5]), "Tudo calmo na Loja.");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_from_telegram_sends_the_card_and_only_the_click_stops() {
    let (w, fake) = world("t08-pause", FakeModel::new(&[])).await;
    let _out = start_outbox(&w);
    let guard = w.state.runs().try_start("p1", "002-demo").expect("vaga");
    fake.push(owner_text(60, "/pause"));
    until("cartão e resposta", || sends(&fake).len() == 2).await;
    let card = w.state.nanty().snapshot().cards.into_iter().find(|c| c.group == "p1:pause_run").expect("cartão");
    assert_eq!(card.requested, Some(Channel::Telegram));
    let token = card.token.clone().unwrap_or_default();
    let with_buttons = sends(&fake).into_iter().find(|b| b.get("reply_markup").is_some()).expect("cartão com botões");
    let datas: Vec<String> = with_buttons["reply_markup"]["inline_keyboard"]
        .as_array()
        .map(|rows| rows.iter().flat_map(|r| r.as_array().cloned().unwrap_or_default()).filter_map(|b| b["callback_data"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(datas.contains(&callback_data(&card.id, &token, Answer::Yes)), "{datas:?}");
    assert!(!guard.stop_signal().requested(), "nada para sem o clique");
    let sent = |w: &World| w.state.nanty().snapshot().telegram_sent.into_iter().find(|r| r.card_id == card.id).and_then(|r| r.message_id);
    until("message_id", || sent(&w).is_some()).await;
    let message_id = sent(&w).unwrap_or_default();
    fake.push(click_on(61, OWNER, OWNER, message_id, &callback_data(&card.id, &token, Answer::Yes)));
    until("parado", || guard.stop_signal().requested()).await;
    until("resposta ao clique", || !fake.calls("answerCallbackQuery").is_empty()).await;
    assert_eq!(fake.calls("answerCallbackQuery")[0].0["text"], json!(tr(Lang::PtBr, "nanty.outcome.done", &[])));
}
