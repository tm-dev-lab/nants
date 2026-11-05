//! Testes do `can_do` (spec 140 T03, C-30): cada item tem efeito de verdade. Só com o estado local e
//! o teclado do Telegram, sem modelo e sem rede.

use std::path::PathBuf;

use serde_json::{json, Map, Value};

use super::*;
use crate::agents::prompts::{nanty_chat_system, nanty_chat_tools, nanty_system, NANTY_OFFER_RULE};
use crate::i18n::Lang;
use crate::server::lan::{ModeSource, NetworkRun};
use crate::server::nanty::chat::tests::{base_config, write_project};
use crate::server::nanty::store::{Alert, NantyState};
use crate::server::nanty::telegram::outbox;
use crate::server::nanty::InstanceStatus;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-can-do-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

fn state_of(tag: &str, lan: bool, edit: impl FnOnce(&mut Config)) -> AppState {
    let dir = temp(tag);
    let mut cfg = base_config(&dir, &[("p1", "Loja"), ("p2", "Blog")]);
    write_project(&dir.join("p1"), "Vitrine", "nada", None);
    write_project(&dir.join("p2"), "Posts", "nada", None);
    edit(&mut cfg);
    let state = AppState::new(dir.join("config.toml"), cfg, 3000).with_data_dir(dir.join("data"));
    let state = if lan { state.with_network_run(NetworkRun { lan: true, source: ModeSource::Flag, requested_port: 3000 }) } else { state };
    assert_eq!(state.nanty().acquire(), InstanceStatus::Active);
    state
}

fn card(id: &str, action: Option<&str>) -> Card {
    Card {
        id: id.into(),
        group: format!("{id}:plan_waiting"),
        code: "nanty.why.plan_waiting".into(),
        token: Some("tok".into()),
        action: action.map(str::to_string),
        expires_at_ms: u64::MAX,
        ..Card::default()
    }
}

/// Os rótulos dos botões do teclado do Telegram.
fn labels(keys: &Value) -> Vec<String> {
    let rows = keys.as_array().cloned().unwrap_or_default();
    rows.iter().flat_map(|r| r.as_array().cloned().unwrap_or_default()).filter_map(|b| b["text"].as_str().map(str::to_string)).collect()
}

/// Spec 140 T03: cada botão do `can_do` existe no teclado do cartão, e nada a mais vira oferta.
#[test]
fn every_button_exists_on_the_card() {
    let cases = [None, Some("approve_plan"), Some("resume_run"), Some("raise_limit"), Some("change_channel"), Some("allow_command_always")];
    for action in cases {
        let c = card("c1", action);
        let keys = outbox::keyboard(Lang::PtBr, &c).expect("teclado");
        let shown = labels(&keys);
        let buttons = card_buttons(&c);
        for id in &buttons {
            let label = crate::i18n::tr(Lang::PtBr, &id.label_code(), &[]);
            assert!(shown.contains(&label), "{action:?}: {label} fora de {shown:?}");
        }
        assert!(buttons.contains(&ActionId::RemindLater), "{action:?}");
        assert!(!buttons.contains(&ActionId::OpenNants), "{action:?}: abrir o Nants só vem da conversa");
    }
    assert_eq!(card_buttons(&card("c1", Some("raise_limit"))), [ActionId::RaiseLimit, ActionId::RemindLater]);
    assert!(card_buttons(&card("c1", Some("end_silence"))).is_empty(), "os botões do silêncio não são oferta");
    assert!(card_buttons(&Card { token: None, ..card("c1", Some("approve_plan")) }).is_empty(), "sem token, sem botão");
}

/// Toda capacidade de conversa é uma ferramenta que existe.
#[test]
fn every_conversation_capability_is_a_real_tool() {
    let state = state_of("tools", false, |c| c.nanty.address = "http://192.168.0.68:3000".into());
    let tools: Vec<String> = nanty_chat_tools().into_iter().map(|t| t.name).collect();
    for channel in [Channel::Web, Channel::Telegram] {
        let cfg = state.config().try_read().expect("config").clone();
        for name in CanDo::now(&state, &cfg, state.nanty(), channel).conversation {
            assert!(tools.iter().any(|t| t == name), "{name}");
        }
    }
}

/// Abrir o Nants só entra quando leva a algum lugar.
#[tokio::test]
async fn open_nants_is_offered_only_where_it_goes_somewhere() {
    let has = |c: &CanDo| c.conversation.contains(&tools::OPEN_NANTS);
    // Em localhost, sem endereço: no Telegram não há o que entregar; na janela, sim.
    let local = state_of("local", false, |_| {});
    let cfg = local.config().read().await.clone();
    assert!(!has(&CanDo::now(&local, &cfg, local.nanty(), Channel::Telegram)));
    assert!(has(&CanDo::now(&local, &cfg, local.nanty(), Channel::Web)));
    // O `Host` guardado de antes não vale fora do modo rede.
    local.nanty().note_device_host("192.168.0.68:3000");
    assert!(!has(&CanDo::now(&local, &cfg, local.nanty(), Channel::Telegram)));

    // Em modo rede, só depois de um aparelho aparecer.
    let lan = state_of("lan", true, |_| {});
    let cfg = lan.config().read().await.clone();
    assert!(!has(&CanDo::now(&lan, &cfg, lan.nanty(), Channel::Telegram)));
    lan.nanty().note_device_host("192.168.0.68:3000");
    assert!(has(&CanDo::now(&lan, &cfg, lan.nanty(), Channel::Telegram)));
    assert_eq!(crate::server::nanty::link::address(&lan, &cfg).as_deref(), Some("http://192.168.0.68:3000"));

    // O campo dos Ajustes vale sobre o `Host` e fora do modo rede.
    let set = state_of("set", false, |c| c.nanty.address = "https://nants.example.com".into());
    let cfg = set.config().read().await.clone();
    assert!(has(&CanDo::now(&set, &cfg, set.nanty(), Channel::Telegram)));
    let both = state_of("both", true, |c| c.nanty.address = "https://nants.example.com".into());
    both.nanty().note_device_host("192.168.0.68:3000");
    let cfg = both.config().read().await.clone();
    assert_eq!(crate::server::nanty::link::address(&both, &cfg).as_deref(), Some("https://nants.example.com"));
}

fn alert(kind: &str, project: Option<&str>, facts: Value) -> Alert {
    let facts: Map<String, Value> = facts.as_object().cloned().unwrap_or_default();
    Alert { kind: kind.into(), project_id: project.map(str::to_string), facts, first_seen_ms: 1, since: "10:00".into(), notified: None, suppressed: None }
}

/// As ações são as de `requestable`, nos projetos observados, sem desligada e sem repetição.
#[tokio::test]
async fn the_actions_are_what_the_proposal_accepts_now() {
    let state = state_of("actions", false, |c| c.nanty.excluded_projects = vec!["p2".into()]);
    state.nanty().update(|st: &mut NantyState| {
        st.alerts.insert("p1:plan_waiting".into(), alert("plan_waiting", Some("p1"), json!({"spec_id": "002-demo"})));
        st.alerts.insert("p2:plan_waiting".into(), alert("plan_waiting", Some("p2"), json!({"spec_id": "002-demo"})));
        st.alerts.insert("docker_orphans".into(), alert("docker_orphans", None, json!({"count": 2})));
        true
    });
    let cfg = state.config().read().await.clone();
    let got = CanDo::now(&state, &cfg, state.nanty(), Channel::Telegram).actions;
    let want = [Offer { action: ActionId::CleanOrphans, project_id: None }, Offer { action: ActionId::ApprovePlan, project_id: Some("p1".into()) }];
    assert_eq!(got.len(), want.len(), "{got:?}");
    assert!(want.iter().all(|o| got.contains(o)), "{got:?}");

    let mut off = cfg.clone();
    off.nanty.actions.insert("approve_plan".into(), crate::config::ActionLevel::Off);
    let got = CanDo::now(&state, &off, state.nanty(), Channel::Web).actions;
    assert_eq!(got, [Offer { action: ActionId::CleanOrphans, project_id: None }], "a desligada não é oferta");
}

/// Só os cartões do turno contam; os do resumo e os vencidos, não.
#[tokio::test]
async fn the_buttons_are_those_of_the_turn() {
    let state = state_of("cards", false, |_| {});
    let cfg = state.config().read().await.clone();
    let (a, b) = (card("c1", Some("approve_plan")), card("c2", Some("resume_run")));
    let turn = CanDo::of(&state, &cfg, state.nanty(), Channel::Web, &[&a]);
    assert_eq!(turn.buttons, [ActionId::ApprovePlan, ActionId::RemindLater]);
    let both = CanDo::of(&state, &cfg, state.nanty(), Channel::Web, &[&a, &b]);
    assert_eq!(both.buttons, [ActionId::ApprovePlan, ActionId::RemindLater, ActionId::ResumeRun], "sem repetição");

    let digest = Card { urgency: Some(Urgency::Digest), ..card("c3", Some("resume_run")) };
    let gone = Card { expires_at_ms: 0, ..card("c4", Some("resume_run")) };
    state.nanty().update(|st| {
        st.cards = vec![a.clone(), digest.clone(), gone.clone()];
        true
    });
    assert_eq!(CanDo::now(&state, &cfg, state.nanty(), Channel::Web).buttons, [ActionId::ApprovePlan, ActionId::RemindLater]);
}

#[test]
fn the_notice_channel_is_the_telegram_only_when_paired() {
    let mut cfg = Config::default();
    assert_eq!(notice_channel(&cfg), Channel::Web);
    cfg.nanty.telegram.chat_id = Some(1);
    assert_eq!(notice_channel(&cfg), Channel::Web, "sem dono");
    cfg.nanty.telegram.owner_id = Some(1);
    assert_eq!(notice_channel(&cfg), Channel::Telegram);
}

/// Spec 141 T08 (C-08): com a voz ligada, o prompt pede a resposta escrita para ser ouvida; desligada,
/// não. O primeiro parágrafo como único trecho ouvido (140/T14) saiu nos dois.
#[test]
fn with_voice_the_prompt_asks_to_write_for_the_ear() {
    for lang in [Lang::PtBr, Lang::En] {
        let (voiced, plain) = (nanty_chat_system(lang, true), nanty_chat_system(lang, false));
        assert!(voiced.contains(crate::agents::prompts::NANTY_SPOKEN_RULE.trim()) && voiced.contains("para ser ouvida"));
        assert!(!plain.contains("para ser ouvida"));
        for prompt in [&voiced, &plain] {
            assert!(!prompt.contains("primeiro parágrafo") && !prompt.contains("único trecho"), "{lang:?}");
        }
    }
}

/// A lista antiga saiu do prompt, e os dois mandam ficar no `can_do`, sem frase de exemplo.
#[test]
fn the_prompts_only_allow_what_is_in_can_do() {
    for lang in [Lang::PtBr, Lang::En] {
        for prompt in [nanty_system(lang), nanty_chat_system(lang, true)] {
            assert!(prompt.contains("can_do") && !prompt.contains("allowed_actions"));
            assert!(!prompt.contains("Posso abrir"), "sem frase pronta de exemplo");
        }
        assert!(!nanty_system(lang).contains("\"actions\""));
        assert!(nanty_chat_system(lang, true).contains("open_nants"));
    }
}

/// Revisão 1 (F-01): o "sim" precisa ter efeito. Os dois prompts só deixam oferecer `actions` e
/// `conversation`, e `buttons` fica como botão que a pessoa toca.
#[test]
fn a_yes_can_only_answer_what_works_in_the_reply() {
    assert!(NANTY_OFFER_RULE.contains("`actions` ou `conversation`") && NANTY_OFFER_RULE.contains("`buttons` só se cumprem"));
    for lang in [Lang::PtBr, Lang::En] {
        for prompt in [nanty_system(lang), nanty_chat_system(lang, true)] {
            assert!(prompt.contains(NANTY_OFFER_RULE), "{lang:?}");
            assert!(!prompt.contains("ofereça apenas"), "{lang:?}: a regra antiga não separava os grupos");
        }
    }
}

/// `remind_later` e `raise_limit` só existem no toque: ficam em `buttons`, e nada na conversa os cumpre.
#[tokio::test]
async fn the_touch_only_buttons_stay_only_in_buttons() {
    let state = state_of("touch", false, |_| {});
    let budget = super::super::signals::SignalKind::Budget.as_str();
    state.nanty().update(|st: &mut NantyState| {
        st.alerts.insert(format!("p1:{budget}"), alert(budget, Some("p1"), json!({})));
        true
    });
    let cfg = state.config().read().await.clone();
    let c = card("c1", Some("raise_limit"));
    let got = CanDo::of(&state, &cfg, state.nanty(), Channel::Web, &[&c]);
    assert_eq!(got.buttons, [ActionId::RaiseLimit, ActionId::RemindLater]);
    assert!(got.actions.is_empty(), "{:?}", got.actions);
    let tools: Vec<String> = nanty_chat_tools().into_iter().map(|t| t.name).collect();
    for id in got.buttons {
        assert!(!got.conversation.contains(&id.as_str()), "{id:?}");
        assert!(!tools.iter().any(|t| t == id.as_str()), "{id:?}: nenhuma ferramenta cumpre");
        assert_ne!(id.entry().kind, ActionKind::Executes, "{id:?}: `propose_action` só aceita o que executa");
    }
}

/// O `can_do` vai ao modelo com os ids, nunca com o endereço.
#[test]
fn can_do_never_carries_the_address() {
    let state = state_of("json", false, |c| c.nanty.address = "http://192.168.0.68:3000".into());
    let cfg = state.config().try_read().expect("config").clone();
    let text = serde_json::to_string(&CanDo::now(&state, &cfg, state.nanty(), Channel::Telegram)).expect("json");
    assert!(text.contains("\"channel\":\"telegram\"") && text.contains("open_nants"), "{text}");
    assert!(!text.contains("192.168"), "{text}");
}
