//! Testes da saída do Telegram (spec 138 T07), só com o servidor falso do Telegram de
//! [`super::tests`] (nunca o Telegram real): envio, edição depois da decisão e do resolvido, os
//! cliques (válido, vencido, de estranho e duplicado), o silêncio, o texto puro, a URL neutralizada,
//! a redação, a fila com o limite e o resumo, a espera com aba visível e o token salvo de novo depois
//! de um 401.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::tests::{
    click_on, fake_server, live_after, lock, paired, token, until, until_async, Fake, Scripted, World, FAST, OWNER,
    STRANGER,
};
use super::*;
use crate::config::Config;
use crate::i18n::tr_plural;
use crate::server::nanty::clock;
use crate::server::nanty::store::{ConversationMessage, DoneItem, SentCard};

/// Liga a fila de saída do mundo de teste (o encerramento do `World` a para).
fn start_outbox(w: &World) -> tokio::task::JoinHandle<()> {
    tokio::spawn(outbox::run(w.state.clone(), w.state.shutdown_signal()))
}

/// Cartão aberto por uma hora, com o porquê do plano esperando.
fn card(group: &str, delivery: Delivery, action: Option<&str>) -> Card {
    let now = clock::now_ms();
    let mut params = Map::new();
    params.insert("since".into(), json!("10:00"));
    Card {
        id: crate::server::nanty::new_id(),
        group: group.into(),
        code: "nanty.why.plan_waiting".into(),
        params,
        action: action.map(str::to_string),
        token: action.map(|_| act::new_token()),
        created_at_ms: now,
        expires_at_ms: now + 3_600_000,
        delivery: Some(delivery),
        ..Card::default()
    }
}

fn add(w: &World, cards: Vec<Card>) {
    w.state.nanty().update(|st| {
        st.cards.extend(cards);
        true
    });
}

fn sent(w: &World, card_id: &str) -> Option<SentCard> {
    w.state.nanty().snapshot().telegram_sent.into_iter().find(|r| r.card_id == card_id)
}

fn sends(fake: &Fake) -> Vec<Value> {
    fake.calls("sendMessage").into_iter().map(|(b, _)| b).collect()
}

fn pt(key: &str) -> String {
    tr(Lang::PtBr, key, &[])
}

async fn world(tag: &str, edit: impl FnOnce(&mut Config)) -> (World, std::sync::Arc<Fake>, String) {
    let tk = token(tag);
    let (fake, base) = fake_server(&tk).await;
    let w = World::with_token(tag, &base, &tk, |cfg| {
        paired(cfg);
        edit(cfg);
    });
    (w, fake, tk)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn card_goes_with_buttons_and_is_edited_after_the_click() {
    let (mut w, fake, _tk) = world("t07-card", |_| {}).await;
    w.start();
    let _out = start_outbox(&w);
    until("conectado", || w.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    let c = card("p1:plan_waiting", Delivery::Push, Some("approve_plan"));
    let (id, tok) = (c.id.clone(), c.token.clone().unwrap_or_default());
    add(&w, vec![c]);
    until("enviado", || sends(&fake).len() == 1).await;
    let body = &sends(&fake)[0];
    assert_eq!(body["chat_id"], OWNER);
    assert!(body.get("parse_mode").is_none(), "texto puro: {body}");
    assert_eq!(body["link_preview_options"]["is_disabled"], true);
    assert!(body.get("disable_notification").is_none(), "o push vai com som");
    assert!(body["text"].as_str().is_some_and(|t| t.contains("Observador de atenção")), "{body}");
    let rows = body["reply_markup"]["inline_keyboard"].as_array().cloned().unwrap_or_default();
    let datas: Vec<String> = rows.iter().flat_map(|r| r.as_array().cloned().unwrap_or_default()).filter_map(|b| b["callback_data"].as_str().map(str::to_string)).collect();
    assert_eq!(datas, [callback_data(&id, &tok, Answer::Yes), callback_data(&id, &tok, Answer::No), callback_data(&id, &tok, Answer::Later)]);
    assert!(datas.iter().all(|d| d.len() <= 64));
    until("message_id guardado", || sent(&w, &id).is_some_and(|r| r.message_id == Some(1) && r.buttons)).await;

    // Estranho: nem resposta, nem decisão.
    fake.push(click_on(100, STRANGER, STRANGER, 1, &callback_data(&id, &tok, Answer::No)));
    until("estranho", || w.journal("telegram.stranger").len() == 1).await;
    assert!(fake.calls("answerCallbackQuery").is_empty());
    // O dono decide "não": resposta ao clique e a mensagem perde os botões, com quem e quando.
    fake.push(click_on(101, OWNER, OWNER, 1, &callback_data(&id, &tok, Answer::No)));
    until("respondido", || fake.calls("answerCallbackQuery").len() == 1).await;
    assert_eq!(fake.calls("answerCallbackQuery")[0].0["text"], json!(pt("nanty.outcome.declined")));
    until("editado", || fake.calls("editMessageText").len() == 1).await;
    let edit = &fake.calls("editMessageText")[0].0;
    assert_eq!((edit["chat_id"].clone(), edit["message_id"].clone()), (json!(OWNER), json!(1)));
    assert!(edit.get("reply_markup").is_none(), "sem botões: {edit}");
    let text = edit["text"].as_str().unwrap_or_default();
    let decided = w.state.nanty().snapshot().decided;
    let time = clock::local_time_at(decided[0].at_ms).map(|t| format!("{:02}:{:02}", t.hour, t.minute)).unwrap_or_default();
    let line = tr(Lang::PtBr, "nanty.telegram.decided.telegram", &[("answer", &pt("nanty.telegram.answer.no")), ("time", &time)]);
    assert!(text.contains("Observador de atenção") && text.ends_with(&line), "{text}");
    until("registro fechado", || sent(&w, &id).is_none()).await;
    // Duplicado: "já decidido", sem outra edição.
    fake.push(click_on(102, OWNER, OWNER, 1, &callback_data(&id, &tok, Answer::Yes)));
    until("duplicado", || fake.calls("answerCallbackQuery").len() == 2).await;
    assert_eq!(fake.calls("answerCallbackQuery")[1].0["text"], json!(pt("nanty.outcome.already_decided.telegram")));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.calls("editMessageText").len(), 1);
    assert!(fake.calls("editMessageReplyMarkup").is_empty());
    assert_eq!(sends(&fake).len(), 1, "nada é reenviado");
    w.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_resolved_and_unknown_cards() {
    let (mut w, fake, _tk) = world("t07-fim", |_| {}).await;
    w.start();
    let _out = start_outbox(&w);
    until("conectado", || w.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    let mut short = card("p1:plan_waiting", Delivery::Push, Some("approve_plan"));
    short.expires_at_ms = clock::now_ms() + 400;
    let (short_id, short_tok) = (short.id.clone(), short.token.clone().unwrap_or_default());
    let resolved = card("p2:run_blocked", Delivery::Push, None);
    let resolved_id = resolved.id.clone();
    add(&w, vec![short, resolved]);
    until("dois enviados", || sends(&fake).len() == 2).await;
    assert!(sends(&fake)[1].get("reply_markup").is_none(), "sem token, sem botões");
    // A fila grava o envio depois da resposta do servidor falso: espera o registro, sem lê-lo no
    // mesmo instante em que a chamada aparece.
    until("message_id do curto", || sent(&w, &short_id).is_some_and(|r| r.message_id.is_some())).await;
    let short_msg = sent(&w, &short_id).and_then(|r| r.message_id).unwrap_or_default();

    // Vencido: o clique responde que expirou e a mensagem vira "expirou".
    tokio::time::sleep(Duration::from_millis(450)).await;
    fake.push(click_on(200, OWNER, OWNER, short_msg, &callback_data(&short_id, &short_tok, Answer::Later)));
    until("resposta", || fake.calls("answerCallbackQuery").len() == 1).await;
    assert_eq!(fake.calls("answerCallbackQuery")[0].0["text"], json!(pt("nanty.outcome.expired")));
    until("expirou", || fake.calls("editMessageText").iter().any(|(b, _)| b["message_id"] == short_msg)).await;
    let edits = fake.calls("editMessageText");
    assert!(edits[0].0["text"].as_str().is_some_and(|t| t.ends_with(&pt("nanty.telegram.expired"))), "{:?}", edits[0].0);

    // Resolvido: o cartão sai como "feito" `resolved` e a mensagem diz quando.
    let now = clock::now_ms();
    w.state.nanty().update(|st| {
        st.cards.retain(|c| c.id != resolved_id);
        st.push_done(DoneItem {
            at_ms: now,
            project_id: Some("p2".into()),
            code: "nanty.why.run_blocked".into(),
            params: Map::new(),
            action: None,
            decided_by: None,
            result: "resolved".into(),
            card_id: Some(resolved_id.clone()),
            undo: None,
        });
        true
    });
    until("resolvido", || fake.calls("editMessageText").len() == 2).await;
    let time = clock::local_time_at(now).map(|t| format!("{:02}:{:02}", t.hour, t.minute)).unwrap_or_default();
    let text = fake.calls("editMessageText")[1].0["text"].as_str().unwrap_or_default().to_string();
    assert!(text.ends_with(&tr(Lang::PtBr, "nanty.telegram.resolved", &[("time", &time)])), "{text}");

    // Botão de um cartão que ninguém conhece mais: "expirou" e a mensagem só perde os botões.
    let ghost = crate::server::nanty::new_id();
    fake.push(click_on(201, OWNER, OWNER, 77, &callback_data(&ghost, &act::new_token(), Answer::Yes)));
    until("botões tirados", || fake.calls("editMessageReplyMarkup").iter().any(|(b, _)| b["message_id"] == 77)).await;
    // A escuta põe os botões na fila antes de responder ao clique: a fila pode tirá-los antes da
    // resposta chegar ao servidor falso. Espera a resposta, em vez de lê-la no mesmo instante.
    until("resposta ao fantasma", || fake.calls("answerCallbackQuery").len() == 2).await;
    assert_eq!(fake.calls("answerCallbackQuery")[1].0["text"], json!(pt("nanty.outcome.expired")));
    // O registro do resolvido sai depois da edição: espera a fila gravar.
    until("registros fechados", || w.state.nanty().snapshot().telegram_sent.is_empty()).await;
    w.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quiet_hours_send_silent_and_keep_digest_for_the_summary() {
    // Silêncio de uma hora antes a duas depois de agora (a hora do relógio do Nanty nos testes, fixa):
    // a fala do Nanty vai sem som.
    let hour = u32::from(clock::test_clock()().hour);
    let (w, fake, _tk) = world("t07-silencio", |cfg| {
        cfg.nanty.quiet_start = format!("{:02}:00", (hour + 23) % 24);
        cfg.nanty.quiet_end = format!("{:02}:00", (hour + 2) % 24);
    })
    .await;
    let _out = start_outbox(&w);
    let urgent = card("p1:no_credit", Delivery::Silent, None);
    let digest = card("p1:cost_alert", Delivery::Digest, None);
    let mut stale = card("telegram:stale:9", Delivery::Push, None);
    stale.code = "nanty.card.telegram_stale_click".into();
    add(&w, vec![urgent.clone(), digest.clone(), stale]);
    until("urgente", || sends(&fake).len() == 1).await;
    assert_eq!(sends(&fake)[0]["disable_notification"], true, "urgente no silêncio vai sem som");
    let nanty = w.state.nanty();
    nanty.push_message(&ConversationMessage::from_nanty(crate::server::nanty::new_id(), clock::now_ms(), Channel::Web, "nanty.telegram.test", Map::new()));
    until("mensagem", || sends(&fake).len() == 2).await;
    assert_eq!(sends(&fake)[1]["disable_notification"], true);
    assert_eq!(sends(&fake)[1]["text"], json!(pt("nanty.telegram.test")));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sends(&fake).len(), 2, "o resumo e o cartão antigo não saem");
    until("urgente gravado", || sent(&w, &urgent.id).is_some()).await;
    assert!(sent(&w, &digest.id).is_none(), "o resumo não vai na hora");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_text_neutral_links_and_redaction() {
    let (w, fake, tk) = world("t07-texto", |_| {}).await;
    let _out = start_outbox(&w);
    let model = |text: &str| {
        let mut m = ConversationMessage::from_nanty(crate::server::nanty::new_id(), clock::now_ms(), Channel::Web, "x", Map::new());
        m.code = None;
        m.text = Some(text.to_string());
        m
    };
    let nanty = w.state.nanty();
    nanty.push_message(&model(&format!("tarefa_1 *quase* pronta, veja https://evil.example/p?x=1 e o token {tk}\n```\nrm -rf /\n```\nfim")));
    until("texto", || sends(&fake).len() == 1).await;
    let text = sends(&fake)[0]["text"].as_str().unwrap_or_default().to_string();
    assert!(text.starts_with("tarefa_1 *quase* pronta"), "{text}");
    assert!(!text.contains("https") && !text.contains("://") && text.contains("evil[.]example/p?x=1"), "{text}");
    assert!(!text.contains(&tk) && text.contains("***"), "{text}");
    assert!(!text.contains("rm -rf") && text.ends_with("fim"), "{text}");
    assert!(sends(&fake)[0].get("parse_mode").is_none());
    // Longo demais: corta no limite.
    nanty.push_message(&model(&"palavra ".repeat(900)));
    until("longo", || sends(&fake).len() == 2).await;
    let long = sends(&fake)[1]["text"].as_str().unwrap_or_default().to_string();
    assert!(format::units(&long) <= format::MAX_MESSAGE_CHARS && long.ends_with('…'), "{}", long.len());
    // A fala da pessoa não volta para o Telegram.
    let mut person = model("oi");
    person.from = crate::server::nanty::store::Speaker::Person;
    nanty.push_message(&person);
    assert_eq!(nanty.telegram().outbox().queued(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_keeps_the_gap_merges_bursts_and_survives_errors() {
    let (w, fake, _tk) = world("t07-fila", |_| {}).await;
    w.state.nanty().telegram().set_tuning(Tuning { send_gap: Duration::from_millis(250), ..FAST });
    let _out = start_outbox(&w);
    let nanty = w.state.nanty();
    // Um 429 e um 500 antes: a fila espera e segue, sem derrubar nada.
    lock(&fake.send_script).extend([Scripted::Retry(1), Scripted::Status(500)]);
    for i in 0..3 {
        let mut params = Map::new();
        params.insert("n".into(), json!(i));
        nanty.push_message(&ConversationMessage::from_nanty(crate::server::nanty::new_id(), clock::now_ms(), Channel::Web, "nanty.telegram.test", params));
    }
    until_async("três", || async { fake.calls("sendMessage").len() == 5 }).await;
    let times: Vec<Instant> = fake.calls("sendMessage").iter().map(|(_, t)| *t).collect();
    assert!(times[1] - times[0] >= Duration::from_millis(950), "retry_after respeitado");
    for pair in times[2..].windows(2) {
        assert!(pair[1] - pair[0] >= Duration::from_millis(240), "uma por intervalo: {:?}", pair[1] - pair[0]);
    }
    // Uma rajada maior que o limite vira uma mensagem só, sem botões.
    let burst: Vec<Card> = (0..8).map(|i| card(&format!("p{i}:plan_waiting"), Delivery::Push, Some("approve_plan"))).collect();
    let ids: Vec<String> = burst.iter().map(|c| c.id.clone()).collect();
    add(&w, burst);
    until("resumo", || fake.calls("sendMessage").len() == 6).await;
    let body = fake.calls("sendMessage")[5].0.clone();
    let text = body["text"].as_str().unwrap_or_default();
    assert!(text.starts_with(&tr_plural(Lang::PtBr, "nanty.telegram.batch", 8, &[])), "{text}");
    assert_eq!(text.lines().count(), 9, "{text}");
    assert!(body.get("reply_markup").is_none());
    until("rajada gravada", || ids.iter().all(|id| sent(&w, id).is_some())).await;
    assert!(ids.iter().all(|id| sent(&w, id).is_some_and(|r| r.message_id.is_none())));
    assert_eq!(w.journal("telegram.batched").len(), 1);
    // Recusa do Telegram (400): desiste daquele envio, registra e segue.
    lock(&fake.send_script).push_back(Scripted::Status(400));
    let refused = card("x:disk_low", Delivery::Push, None);
    add(&w, vec![refused.clone()]);
    until("recusado", || sent(&w, &refused.id).is_some()).await;
    assert_eq!(w.journal("telegram.send_failed").len(), 1);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(fake.calls("sendMessage").len(), 7, "não tenta de novo");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn visible_tab_holds_the_send_and_web_decisions_never_go() {
    let (w, fake, _tk) = world("t07-aba", |cfg| cfg.nanty.telegram.visible_hold_secs = 1).await;
    let _out = start_outbox(&w);
    let (status, body) = w.post("/api/nanty/presence", json!({"tab": "aba-1", "visible": true})).await;
    assert_eq!((status, body["visible"].as_bool()), (StatusCode::OK, Some(true)), "{body}");
    let (status, body) = w.post("/api/nanty/presence", json!({"tab": "não vale", "visible": true})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("nanty_tab_invalid")));

    // Decidido na janela durante a espera: nunca vai ao Telegram.
    let web = card("p1:plan_waiting", Delivery::Push, Some("approve_plan"));
    let (web_id, web_tok) = (web.id.clone(), web.token.clone().unwrap_or_default());
    let held = card("p2:run_blocked", Delivery::Push, None);
    let start = Instant::now();
    add(&w, vec![web, held.clone()]);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(sends(&fake).is_empty(), "espera com a aba visível");
    let (status, body) = w.post(&format!("/api/nanty/cards/{web_id}/decide"), json!({"token": web_tok, "answer": "no"})).await;
    assert_eq!((status, body["outcome"].as_str()), (StatusCode::OK, Some("declined")), "{body}");
    until("depois da espera", || sends(&fake).len() == 1).await;
    assert!(start.elapsed() >= Duration::from_millis(950), "{:?}", start.elapsed());
    // O servidor falso registra a chamada antes de a fila gravar o envio no estado (a gravação vem
    // depois da resposta): espera o registro, em vez de lê-lo no mesmo instante.
    until("envio gravado", || sent(&w, &held.id).is_some()).await;
    let held_sent = sent(&w, &held.id).map(|r| r.at_ms).unwrap_or_default();
    assert!(held_sent >= held.created_at_ms + 1000, "a espera da aba visível vale: {held_sent} < {} + 1000", held.created_at_ms);
    assert!(sent(&w, &web_id).is_none(), "o decidido na janela não vai ao Telegram");
    // A aba ficou oculta: sai na hora.
    let (_, body) = w.post("/api/nanty/presence", json!({"tab": "aba-1", "visible": false})).await;
    assert_eq!(body["visible"], false);
    let now = Instant::now();
    add(&w, vec![card("p3:review_loop", Delivery::Push, None)]);
    until("na hora", || sends(&fake).len() == 2).await;
    assert!(now.elapsed() < Duration::from_millis(900), "{:?}", now.elapsed());
    assert_eq!(sends(&fake).len(), 2, "o decidido na janela não saiu");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hold_can_be_turned_off() {
    let (w, fake, _tk) = world("t07-sem-espera", |cfg| cfg.nanty.telegram.hold_while_visible = false).await;
    let _out = start_outbox(&w);
    w.post("/api/nanty/presence", json!({"tab": "aba-2", "visible": true})).await;
    let now = Instant::now();
    add(&w, vec![card("p1:plan_waiting", Delivery::Push, None)]);
    until("na hora", || sends(&fake).len() == 1).await;
    assert!(now.elapsed() < Duration::from_millis(900));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_token_saved_again_after_401_reopens_listening() {
    let tk = token("t07-401");
    let (fake, base) = fake_server(&token("outro-401")).await;
    let mut w = World::with_token("t07-401", &base, &tk, |_| {});
    w.start();
    until("recusado", || w.health() == Health::InvalidToken).await;
    // O bot volta a valer no Telegram e a pessoa salva o mesmo token.
    *lock(&fake.token) = tk.clone();
    let (status, _) = w.post("/api/nanty/telegram/token", json!({"token": tk})).await;
    assert_eq!(status, StatusCode::OK);
    until("conectado", || w.health() == Health::Connected).await;
    until_async("config intacta", || async { w.tg().await.bot_token == tk }).await;
    w.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offer_that_leaves_loses_the_buttons_and_renewal_goes_again() {
    let (w, fake, _tk) = world("t07-renova", |_| {}).await;
    let _out = start_outbox(&w);
    let c = card("p1:plan_waiting", Delivery::Push, Some("approve_plan"));
    let id = c.id.clone();
    add(&w, vec![c]);
    until("enviado", || sent(&w, &id).is_some_and(|r| r.buttons)).await;
    // A oferta sai do cartão (o token morre com ela): a mensagem perde os botões, o texto fica.
    w.state.nanty().update(|st| {
        if let Some(card) = st.cards.iter_mut().find(|c| c.id == id) {
            card.action = None;
            card.token = None;
        }
        true
    });
    until("botões tirados", || fake.calls("editMessageReplyMarkup").len() == 1).await;
    assert_eq!(fake.calls("editMessageReplyMarkup")[0].0["message_id"], 1);
    until("registro", || sent(&w, &id).is_some_and(|r| !r.buttons)).await;
    // A repetição do filtro renova o cartão (mesmo id): sai uma mensagem nova.
    w.state.nanty().update(|st| {
        if let Some(card) = st.cards.iter_mut().find(|c| c.id == id) {
            card.created_at_ms += 1;
        }
        true
    });
    until("de novo", || sends(&fake).len() == 2).await;
    until("novo message_id", || sent(&w, &id).is_some_and(|r| r.message_id == Some(2))).await;
    assert!(fake.calls("editMessageText").is_empty());
}

/// A rodada cria o cartão e logo depois põe a oferta nele (id e token novos, T03): com a
/// acomodação, só o cartão final sai, sem um "expirou" antes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_card_settles_before_going() {
    let (w, fake, _tk) = world("t07-acomoda", |_| {}).await;
    w.state.nanty().telegram().set_tuning(Tuning { send_settle: Duration::from_millis(500), ..FAST });
    let _out = start_outbox(&w);
    let first = card("p1:plan_waiting", Delivery::Push, None);
    let first_id = first.id.clone();
    add(&w, vec![first]);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let offered = crate::server::nanty::new_id();
    w.state.nanty().update(|st| {
        if let Some(card) = st.cards.iter_mut().find(|c| c.id == first_id) {
            card.id = offered.clone();
            card.action = Some("approve_plan".into());
            card.token = Some(act::new_token());
            card.created_at_ms = clock::now_ms();
        }
        true
    });
    until("enviado", || sends(&fake).len() == 1).await;
    assert!(sends(&fake)[0].get("reply_markup").is_some());
    tokio::time::sleep(Duration::from_millis(200)).await;
    until("cartão final gravado", || sent(&w, &offered).is_some()).await;
    assert!(sent(&w, &first_id).is_none(), "o cartão de antes da acomodação não saiu");
    assert!(fake.calls("editMessageText").is_empty(), "nenhum expirou à toa");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn morning_digest_goes_through_render() {
    let (w, fake, _tk) = world("t07-resumo", |_| {}).await;
    let _out = start_outbox(&w);
    let facts = crate::server::nanty::digest::DigestFacts {
        projects: 3,
        waiting: 1,
        disk_free_gb: Some(42),
        disk_low: true,
        ..Default::default()
    };
    let cfg = w.state.config().read().await.nanty.clone();
    let message =
        crate::server::nanty::digest::compose(&facts, &cfg, crate::server::nanty::new_id(), clock::now_ms()).expect("resumo");
    w.state.nanty().push_message(&message);
    until("resumo", || sends(&fake).len() == 1).await;
    let rendered = crate::server::nanty::render::message_text(Lang::PtBr, &message);
    let expected = format::clean(&crate::server::events::SecretSet::default(), &rendered);
    assert_eq!(sends(&fake)[0]["text"], json!(expected));
    assert!(expected.contains("42"), "{expected}");
}
