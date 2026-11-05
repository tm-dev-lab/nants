//! Testes do caminho até o Nants (spec 140 T03, C-30), só com o servidor falso do Telegram e o modelo
//! simulado (nunca o Telegram real nem chave real): a reprodução do achado da rc.23 (o aviso, "abre
//! então" e o link), o que acontece em localhost, a recusa do botão, a nota de voz, o botão do cartão,
//! a rajada e a janela.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::tests::{click_on, fake_server, live_after, lock, paired, temp, token, until, Fake, Scripted as Tg, World, FAST, OWNER};
use super::voice_tests::{add, bodies, card, sent, start_outbox};
use super::*;
use crate::agents::chat::ChatBackend;
use crate::config::Config;
use crate::server::events::SecretSet;
use crate::server::lan::{ModeSource, NetworkRun};
use crate::server::nanty::agent::tests::{call, say, Scripted};
use crate::server::nanty::chat::tests::{base_config, write_project};
use crate::server::nanty::chat::{self as conversation};
use crate::server::nanty::store::{ConversationMessage, Speaker};
use crate::server::nanty::turn::{self, TurnOutcome};
use crate::server::nanty::{clock, new_id};
use crate::server::devices::ENTRY_PATH;

const BASE: &str = "http://192.168.0.68:3000";
const KEY: &str = "p1:plan_waiting";

fn owner_text(update_id: i64, text: &str) -> Value {
    let date = i64::try_from(clock::now_ms() / 1000).unwrap_or(0);
    json!({"update_id": update_id, "message": {
        "message_id": update_id, "date": date,
        "chat": {"id": OWNER, "type": "private"},
        "from": {"id": OWNER, "is_bot": false},
        "text": text,
    }})
}

/// Pareado, com os projetos Loja e Blog, a escuta e a fila ligadas. `lan`: em modo rede.
async fn world(tag: &str, model: Arc<Scripted>, lan: bool, edit: impl FnOnce(&mut Config, &PathBuf)) -> (World, Arc<Fake>) {
    let tk = token(tag);
    let (fake, base) = fake_server(&tk).await;
    let dir = temp(tag);
    let mut cfg = base_config(&dir, &[("p1", "Loja"), ("p2", "Blog")]);
    cfg.nanty.telegram.bot_token = tk;
    paired(&mut cfg);
    edit(&mut cfg, &dir);
    write_project(&dir.join("p1"), "Vitrine", "nada", None);
    write_project(&dir.join("p2"), "Posts", "nada", None);
    let state = AppState::new(dir.join("config.toml"), cfg, 3000)
        .with_data_dir(dir.join("data"))
        .with_telegram_api(&base)
        .with_nanty_clock(clock::test_clock());
    let state = if lan { state.with_network_run(NetworkRun { lan: true, source: ModeSource::Flag, requested_port: 3000 }) } else { state };
    assert_eq!(state.nanty().acquire(), crate::server::nanty::InstanceStatus::Active);
    state.nanty().telegram().set_tuning(FAST);
    state.nanty().set_backend(model as Arc<dyn ChatBackend>);
    let mut w = World { dir, state, listener: None };
    w.start();
    until("conectado", || w.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    (w, fake)
}

fn sends(fake: &Fake) -> Vec<Value> {
    bodies(fake, "sendMessage")
}

fn url_button(body: &Value) -> Option<String> {
    body["reply_markup"]["inline_keyboard"][0][0]["url"].as_str().map(str::to_string)
}

fn text_of(body: &Value) -> String {
    body["text"].as_str().unwrap_or_default().to_string()
}

fn summary(text: &str) -> String {
    json!({ "summary": text }).to_string()
}

/// O achado da rc.23: o aviso oferece, ela responde "abre então", e o link chega. Antes, o Nanty
/// respondia que não conseguia: `open_nants` era entregue ao modelo sem nenhum efeito.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rc23_finding_the_notice_offers_abre_entao_and_the_link_arrives() {
    let offer = "A especificação está pronta para você conferir. Posso mandar o acesso ao Nants.";
    let model = Scripted::new(vec![say(&summary(offer)), call("open_nants", json!({"project_id": "p1"})), say("Pronto, é só tocar.")]);
    let (w, fake) = world("link-rc23", Arc::clone(&model), true, |_, _| {}).await;
    let _out = start_outbox(&w);
    let nanty = Arc::clone(w.state.nanty());
    // Um aparelho já entrou pela rede: o endereço é o que ele alcança.
    nanty.note_device_host("192.168.0.68:3000");

    // O aviso: o modelo só recebe o que existe de verdade.
    let c = Card { project_id: Some("p1".into()), ..card(KEY, Some("approve_plan")) };
    add(&w, vec![c]);
    assert!(matches!(turn::proactive(&w.state, &nanty).await, Some(TurnOutcome::Reply(_))));
    let input = model.input(0);
    assert_eq!(input["can_do"]["channel"], "telegram");
    assert_eq!(input["can_do"]["buttons"], json!(["approve_plan", "remind_later"]));
    let conversation_list = input["can_do"]["conversation"].as_array().cloned().unwrap_or_default();
    assert!(conversation_list.contains(&json!("open_nants")), "{input}");
    until("aviso e texto", || sends(&fake).iter().any(|b| text_of(b).contains(offer))).await;
    assert!(sends(&fake).iter().all(|b| url_button(b).is_none()));

    // "Abre então": a resposta curta vale, com o aviso na memória.
    fake.push(owner_text(50, "Abre então"));
    until("o link", || sends(&fake).iter().any(|b| url_button(b).is_some())).await;
    let requests = model.requests();
    assert!(requests[1].tools.iter().any(|t| t.name == "open_nants"), "a ferramenta existe nesta conversa");
    assert!(json!(model.memory(1)).to_string().contains("Posso mandar o acesso"), "ela responde à oferta");

    let body = sends(&fake).into_iter().find(|b| url_button(b).is_some()).expect("link");
    assert_eq!(url_button(&body).as_deref(), Some(&*format!("{BASE}/?space=p1")));
    assert_eq!(body["reply_markup"]["inline_keyboard"][0][0]["text"], tr(Lang::PtBr, "nanty.telegram.open_button", &[]));
    let text = text_of(&body);
    // Spec 141 T10 (C-10): o texto do modelo, sem frase fixa depois; o botão leva o acesso.
    assert!(text.starts_with("Pronto, é só tocar.") && !text.contains(&tr(Lang::PtBr, "nanty.chat.open_link", &[])), "{text}");
    let url = url_button(&body).unwrap_or_default();
    assert!(!url.contains("token") && !url.contains(ENTRY_PATH), "sem token e sem link de entrada: {url}");

    // O resultado da ferramenta e o que o modelo viu nunca trazem o endereço.
    assert!(!model.everything().contains("192.168"), "o endereço não passa pelo modelo");
    assert!(model.results(2).iter().any(|(_, text)| text == &tr(Lang::PtBr, "nanty.tool.open_link", &[])));
    let saved = nanty.conversation().into_iter().rev().find(|m| m.from == Speaker::Nanty).expect("resposta");
    assert_eq!(link::url_of(&saved), Some(&*format!("{BASE}/?space=p1")));
}

/// Um link escrito pelo modelo continua neutralizado: só o do servidor abre.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_link_written_by_the_model_never_becomes_the_button() {
    let model = Scripted::new(vec![call("open_nants", json!({})), say("Abra https://evil.example/entrar")]);
    let (w, fake) = world("link-evil", model, false, |c, _| c.nanty.address = BASE.into()).await;
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "abre o nants"));
    until("resposta", || !sends(&fake).is_empty()).await;
    let body = &sends(&fake)[0];
    assert_eq!(url_button(body).as_deref(), Some(&*format!("{BASE}/")));
    assert!(text_of(body).contains("evil[.]example") && !text_of(body).contains("https://"), "{body}");
}

/// Em localhost, sem endereço: o motivo e o que fazer, sem link e sem prometer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_localhost_it_says_to_open_on_the_computer() {
    let model = Scripted::new(vec![call("open_nants", json!({"project_id": "p1"})), say("Daqui não dá.")]);
    let (w, fake) = world("link-local", Arc::clone(&model), false, |_, _| {}).await;
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "abre o nants"));
    until("resposta", || !sends(&fake).is_empty()).await;
    let body = &sends(&fake)[0];
    assert_eq!(url_button(body), None);
    assert!(body.get("reply_markup").is_none(), "{body}");
    // Spec 141 T10, revisão 1 (F-01): sem link, o que fazer chega à pessoa: a linha curta do servidor
    // fecha o texto do modelo (a rede de segurança), e o resultado da ferramenta dá ao modelo o mesmo
    // caminho, sem dizer que o servidor avisa outra coisa.
    let line = tr(Lang::PtBr, "nanty.chat.open_computer", &[]);
    assert_eq!(text_of(body), format!("Daqui não dá.\n{line}"));
    assert!(line.contains("computador") && line.contains("Ajustes do Nanty") && line.contains("aparelho autorizado"), "{line}");
    let input = model.input(0);
    assert!(!input["can_do"]["conversation"].to_string().contains("open_nants"), "não é oferta: {input}");
    assert_eq!(model.results(1)[0].1, tr(Lang::PtBr, "nanty.tool.open_computer", &[]));
    let tool = &model.results(1)[0].1;
    // Revisão 2: a linha mantida é a fonte única dos passos; o resultado só diz que não deu.
    assert!(!tool.contains("Ajustes") && !tool.contains("aparelho autorizado") && tool.contains("sem repetir os passos"), "{tool}");
}

/// O Telegram recusou o botão (localhost ou IP privado podem ser recusados): a mensagem sai de novo,
/// sem o botão, com o endereço como texto, inteiro. Cai aqui com nome, sem o `[.]` do `clean`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_button_goes_again_with_the_address_as_text() {
    for (tag, address) in [("link-ref-ip", BASE), ("link-ref-name", "https://nants.example.com")] {
        let model = Scripted::new(vec![call("open_nants", json!({"project_id": "p1"})), say("Segue.")]);
        let (w, fake) = world(tag, model, false, |c, _| c.nanty.address = address.into()).await;
        lock(&fake.send_script).push_back(Tg::Status(400));
        let _out = start_outbox(&w);
        fake.push(owner_text(50, "abre o nants"));
        until("de novo", || sends(&fake).len() == 2).await;
        let (first, second) = (&sends(&fake)[0], &sends(&fake)[1]);
        let url = format!("{address}/?space=p1");
        assert_eq!(url_button(first).as_deref(), Some(url.as_str()));
        assert!(second.get("reply_markup").is_none(), "{second}");
        let want = format!("Segue.\n{}", tr(Lang::PtBr, "nanty.telegram.open_address", &[("url", &url)]));
        assert_eq!(text_of(second), want);
        assert_eq!(w.journal("telegram.link_refused")[0]["error"], "api_error");
        assert_eq!(w.state.nanty().telegram().outbox().queued(), 0, "a mensagem não fica presa");
    }
}

/// Falha passageira não é recusa: o botão vai de novo.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_passing_failure_keeps_the_button() {
    let model = Scripted::new(vec![call("open_nants", json!({})), say("Segue.")]);
    let (w, fake) = world("link-retry", model, false, |c, _| c.nanty.address = BASE.into()).await;
    lock(&fake.send_script).push_back(Tg::Retry(1));
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "abre o nants"));
    until("segunda tentativa", || sends(&fake).len() == 2).await;
    assert_eq!(url_button(&sends(&fake)[1]).as_deref(), Some(&*format!("{BASE}/")));
    assert!(w.journal("telegram.link_refused").is_empty());
}

/// A nota de voz leva o texto inteiro; o botão sai numa linha curta só dele (spec 141 T10), e a fala
/// não lê o endereço.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_voice_the_note_goes_first_and_the_button_follows() {
    let (w, fake, voice_fake) = super::voice_tests::world("link-voice", |c| {
        c.nanty.address = BASE.into();
        c.nanty.connection_id = "oai".into();
        c.nanty.model = "modelo".into();
    })
    .await;
    let model = Scripted::new(vec![call("open_nants", json!({})), say("Segue.")]);
    w.state.nanty().set_backend(model as Arc<dyn ChatBackend>);
    let _out = start_outbox(&w);
    let cfg = w.state.config().read().await.clone();
    let person = conversation::person_message(w.state.nanty(), &cfg.nanty, &SecretSet::from_config(&cfg), Channel::Telegram, "abre o nants").expect("mensagem");
    conversation::reply(&w.state, &person).await.expect("resposta");
    until("o botão", || sends(&fake).len() == 1).await;
    let carrier = tr(Lang::PtBr, "nanty.chat.open_link", &[]);
    assert_eq!(bodies(&fake, "sendVoice")[0]["caption"], "Segue.");
    let body = &sends(&fake)[0];
    assert_eq!(url_button(body).as_deref(), Some(&*format!("{BASE}/")));
    assert_eq!(text_of(body), carrier, "o texto não sai duas vezes");
    assert!(!voice_fake.calls("gemini").iter().any(|c| c.to_string().contains("192.168")), "a fala não lê o endereço");
}

/// O botão de um cartão que leva ao Nants entrega o caminho, além do aviso do clique.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_card_link_button_delivers_the_way_in() {
    let (w, fake) = world("link-card", Scripted::new(Vec::new()), false, |c, _| c.nanty.address = BASE.into()).await;
    let c = Card { project_id: Some("p1".into()), delivery: None, ..card("p1:budget", Some("raise_limit")) };
    let (id, tok) = (c.id.clone(), c.token.clone().unwrap_or_default());
    add(&w, vec![c]);
    let _out = start_outbox(&w);
    until("cartão", || sent(&w, &id).is_some()).await;
    let message_id = sent(&w, &id).and_then(|r| r.message_id).unwrap_or_default();
    fake.push(click_on(101, OWNER, OWNER, message_id, &callback_data(&id, &tok, Answer::Yes)));
    until("o link", || sends(&fake).iter().any(|b| url_button(b).is_some())).await;
    let body = sends(&fake).into_iter().find(|b| url_button(b).is_some()).expect("link");
    assert_eq!(url_button(&body).as_deref(), Some(&*format!("{BASE}/?space=p1")));
    assert_eq!(fake.calls("answerCallbackQuery").len(), 1);
    assert!(w.state.nanty().snapshot().cards.iter().any(|c| c.id == id), "abrir não decide o cartão");
}

/// No resumo da fila, o link pedido não some: ele sai sozinho, com o botão.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_never_swallows_the_link() {
    let (w, fake) = world("link-burst", Scripted::new(Vec::new()), false, |c, _| c.nanty.address = BASE.into()).await;
    let nanty = w.state.nanty();
    for i in 0..=outbox::MAX_BURST {
        let mut m = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Web, "x", Map::new());
        m.code = None;
        m.text = Some(format!("fala {i}"));
        nanty.push_message(&m);
    }
    let open = link::Open::Link(format!("{BASE}/"));
    let mut m = ConversationMessage::from_nanty(new_id(), clock::now_ms(), Channel::Telegram, open.code(), open.params());
    m.reply_to = Some("0123456789abcdef".into());
    nanty.push_message(&m);
    let _out = start_outbox(&w);
    until("o resto", || sends(&fake).len() == 2).await;
    assert_eq!(url_button(&sends(&fake)[0]).as_deref(), Some(&*format!("{BASE}/")));
    assert_eq!(url_button(&sends(&fake)[1]), None);
}

/// Na janela, a tela do projeto, sem link; a resposta não vai ao Telegram.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_the_window_it_opens_the_screen_without_a_link() {
    let model = Scripted::new(vec![call("open_nants", json!({"project_id": "p1"})), say("Abrindo."), call("open_nants", json!({})), say("Qual deles?")]);
    let (w, fake) = world("link-web", model, false, |c, _| c.nanty.address = BASE.into()).await;
    let _out = start_outbox(&w);
    let cfg = w.state.config().read().await.clone();
    let secrets = SecretSet::from_config(&cfg);
    let person = conversation::person_message(w.state.nanty(), &cfg.nanty, &secrets, Channel::Web, "abre a Loja").expect("mensagem");
    let answer = conversation::reply(&w.state, &person).await.expect("resposta");
    assert_eq!(answer.code.as_deref(), Some("nanty.chat.open_screen"));
    assert_eq!(answer.params[link::OPEN_PROJECT], "p1");
    assert_eq!(answer.params["project"], "Loja");
    assert_eq!(link::url_of(&answer), None, "sem link");

    let person = conversation::person_message(w.state.nanty(), &cfg.nanty, &secrets, Channel::Web, "abre o nants").expect("mensagem");
    let answer = conversation::reply(&w.state, &person).await.expect("resposta");
    assert_eq!(answer.code.as_deref(), Some("nanty.chat.open_here"));
    assert!(answer.params.is_empty());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(sends(&fake).is_empty(), "a resposta à janela fica nela");
}

/// O pedido certo pelo caminho errado (`propose_action`) ainda entrega; o projeto fora da lista, não.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proposing_open_nants_still_delivers_and_an_unknown_project_is_refused() {
    let model = Scripted::new(vec![
        call("propose_action", json!({"action_id": "open_nants", "project_id": "p9"})),
        call("propose_action", json!({"action_id": "open_nants", "project_id": "p1"})),
        say("Segue."),
    ]);
    let (w, fake) = world("link-propose", Arc::clone(&model), false, |c, _| c.nanty.address = BASE.into()).await;
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "abre a Loja"));
    until("resposta", || !sends(&fake).is_empty()).await;
    assert_eq!(url_button(&sends(&fake)[0]).as_deref(), Some(&*format!("{BASE}/?space=p1")));
    assert_eq!(model.results(1)[0].1, tr(Lang::PtBr, "nanty.tool.unknown_project", &[("id", "p9")]));
}

/// O caminho junta-se a outro efeito da mesma mensagem, sem sumir.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_way_in_joins_another_effect_of_the_same_message() {
    let model = Scripted::new(vec![call("open_nants", json!({})).and("set_silence", json!({"hours": 2})), say("Certo.")]);
    let (w, fake) = world("link-both", model, false, |c, _| c.nanty.address = BASE.into()).await;
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "abre o nants e fica quieto"));
    until("resposta", || sends(&fake).iter().any(|b| url_button(b).is_some())).await;
    let body = sends(&fake).into_iter().find(|b| url_button(b).is_some()).expect("link");
    assert_eq!(url_button(&body).as_deref(), Some(&*format!("{BASE}/")));
    // Spec 141 T10 (C-10): o texto do modelo sai sozinho; os dois efeitos ficam como dado na
    // mensagem (a hora do silêncio e o caminho), e o caminho também no botão.
    assert_eq!(text_of(&body), "Certo.", "{body}");
    let saved = w.state.nanty().conversation().into_iter().rev().find(|m| m.from == Speaker::Nanty).expect("resposta");
    assert_eq!(saved.code.as_deref(), Some("nanty.chat.silence_set"));
    let why = saved.params["why"].as_array().cloned().unwrap_or_default();
    assert!(why.iter().any(|l| l["code"] == "nanty.chat.open_link"), "{:?}", saved.params);
}

/// O modelo caiu depois da ferramenta: o caminho sai assim mesmo, sem a reserva.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_way_in_survives_a_model_that_fails_afterwards() {
    let model = Scripted::new(vec![call("open_nants", json!({}))]);
    let (w, fake) = world("link-fail", model, false, |c, _| c.nanty.address = BASE.into()).await;
    let _out = start_outbox(&w);
    fake.push(owner_text(50, "abre o nants"));
    until("resposta", || !sends(&fake).is_empty()).await;
    let body = &sends(&fake)[0];
    assert_eq!(url_button(body).as_deref(), Some(&*format!("{BASE}/")));
    assert_eq!(text_of(body), tr(Lang::PtBr, "nanty.chat.open_link", &[]));
}
