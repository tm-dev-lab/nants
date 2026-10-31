//! Testes do núcleo do Nanty (spec 138 T01): ciclo de vida, trava, limpeza ao remover projeto,
//! `Lagged`, avatar e rotas de leitura.

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};
use tower::ServiceExt;

use super::clock::{at, fixed_clock};
use super::journal::{JournalEntry, JournalKind};
use super::store::{Card, Channel, ConversationMessage, DecidedBy, DoneItem, Silence, Speaker, Spend};
use super::*;
use crate::config::{Config, Project};
use crate::server::events::{EVENT_CHANNEL_CAPACITY, EventType};

const PORT: u16 = 3000;
const HOST: &str = "127.0.0.1:3000";
/// 2026-10-30 12:00 UTC.
const NOON_MS: u64 = 1_793_361_600_000;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

fn project(id: &str) -> Project {
    Project { id: id.into(), name: id.into(), path: format!("/nada/{id}"), created_at: "2026-10-30T00:00:00Z".into() }
}

/// Estado com pasta de dados própria e hora local fixa (meio-dia de 2026-10-30).
fn state_in(data: &PathBuf, cfg: Config) -> AppState {
    AppState::new(data.join("config.toml"), cfg, PORT)
        .with_data_dir(data.clone())
        .with_nanty_clock(fixed_clock(at((2026, 10, 30), 12, 0, NOON_MS)))
}

fn card(id: &str, project: Option<&str>, expires_at_ms: u64) -> Card {
    Card {
        id: id.into(),
        project_id: project.map(Into::into),
        group: format!("{}:plan_waiting", project.unwrap_or("system")),
        code: "plan_waiting".into(),
        params: Map::new(),
        created_at_ms: NOON_MS - 1000,
        expires_at_ms,
        ..Card::default()
    }
}

async fn wait_until(mut ok: impl FnMut() -> bool) {
    for _ in 0..500 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condição não chegou");
}

#[tokio::test]
async fn loop_starts_only_with_the_lock_and_stops_on_shutdown() {
    let data = dir("lifecycle");
    let first = state_in(&data, Config::default());
    assert_eq!(first.nanty().status(), InstanceStatus::NotStarted, "AppState::new não liga o laço");
    let running = start(&first);
    assert!(running.running());
    assert_eq!(first.nanty().status(), InstanceStatus::Active);

    // Outro processo com a mesma pasta de dados: não liga o Nanty.
    let second = state_in(&data, Config::default());
    let idle = start(&second);
    assert!(!idle.running());
    assert_eq!(second.nanty().status(), InstanceStatus::OtherInstance);
    idle.stop().await;

    // Outra instalação (outra pasta de dados): nunca se enxergam.
    let other_data = dir("lifecycle-other");
    let other = state_in(&other_data, Config::default());
    let other_loop = start(&other);
    assert_eq!(other.nanty().status(), InstanceStatus::Active);

    // O laço para no shutdown e solta a trava.
    first.trigger_shutdown();
    tokio::time::timeout(Duration::from_secs(5), running.stop()).await.expect("parou");
    assert_eq!(first.nanty().status(), InstanceStatus::Stopped);
    let third = state_in(&data, Config::default());
    let again = start(&third);
    assert_eq!(third.nanty().status(), InstanceStatus::Active, "a trava saiu com o laço");
    third.trigger_shutdown();
    again.stop().await;
    other.trigger_shutdown();
    other_loop.stop().await;
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&other_data);
}

#[tokio::test]
async fn no_data_dir_means_unavailable() {
    let nanty = Nanty::new(None, EventBus::new(), fixed_clock(at((2026, 10, 30), 12, 0, NOON_MS)));
    assert_eq!(nanty.acquire(), InstanceStatus::Unavailable);
    assert_eq!(nanty.avatar(&crate::config::NantyConfig::default()), Avatar::Sleeping);
    assert!(nanty.conversation().is_empty());
}

#[tokio::test]
async fn removed_project_state_leaves_with_the_event() {
    let data = dir("removed");
    let mut cfg = Config::default();
    cfg.projects = vec![project("p1"), project("p2")];
    let st = state_in(&data, cfg);
    let running = start(&st);
    let nanty = Arc::clone(st.nanty());
    wait_until(|| nanty.sweeps() >= 1).await;
    nanty.update(|s| {
        s.cards.push(card("a", Some("p1"), NOON_MS + 60_000));
        s.cards.push(card("b", Some("p2"), NOON_MS + 60_000));
        s.silences.push(Silence { project_id: Some("p1".into()), group: None, until_ms: NOON_MS + 60_000 });
        true
    });
    let mut entry = JournalEntry::new(NOON_MS, JournalKind::Signal, "plan_waiting");
    entry.project_id = Some("p1".into());
    nanty.record(&entry);
    nanty.push_message(&ConversationMessage {
        id: new_id(),
        at_ms: NOON_MS,
        from: Speaker::Person,
        channel: Channel::Web,
        project_id: Some("p1".into()),
        text: Some("olha o p1".into()),
        code: None,
        params: Map::new(),
        lang: None,
        card_ids: Vec::new(),
        actions: Vec::new(),
        reply_to: None,
        voice: false,
        audio: None,
    });
    assert_eq!(nanty.journal_tail(10).len(), 1);
    assert_eq!(nanty.conversation().len(), 1);

    st.events().publish(ApiEvent::new(EventType::ProjectRemoved, Some("p1".into()), json!({"project_id": "p1"})));
    wait_until(|| nanty.snapshot().cards.len() == 1).await;
    wait_until(|| nanty.conversation().is_empty()).await;
    let snap = nanty.snapshot();
    assert_eq!(snap.cards[0].id, "b");
    assert!(snap.silences.is_empty());
    assert!(nanty.journal_tail(10).is_empty(), "o registro do projeto removido saiu");
    // Gravado na pasta de dados: um reinício lê o estado já limpo.
    let reloaded = Store::new(data.join(NANTY_DIR)).load();
    assert_eq!(reloaded.cards.len(), 1);
    st.trigger_shutdown();
    running.stop().await;
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn lagged_triggers_a_full_sweep() {
    let data = dir("lagged");
    let mut cfg = Config::default();
    cfg.projects = vec![project("p1")];
    let st = state_in(&data, cfg);
    let nanty = Arc::clone(st.nanty());
    // Antes do laço: um cartão, uma linha do registro e uma mensagem de um projeto que já não
    // existe (removido enquanto outro processo era o dono), e as de um projeto que existe.
    nanty.update(|s| {
        s.cards.push(card("velho", Some("sumiu"), NOON_MS + 60_000));
        true
    });
    let journal = journal::Journal::new(data.join(NANTY_DIR));
    let conversation = Store::new(data.join(NANTY_DIR));
    for project in ["sumiu", "p1"] {
        let mut entry = JournalEntry::new(NOON_MS, JournalKind::Signal, "plan_waiting");
        entry.project_id = Some(project.into());
        journal.append(&entry).expect("registro");
        conversation
            .conversation()
            .append(&ConversationMessage {
                id: new_id(),
                at_ms: NOON_MS,
                from: Speaker::Person,
                channel: Channel::Web,
                project_id: Some(project.into()),
                text: Some("olha".into()),
                code: None,
                params: Map::new(),
                lang: None,
                card_ids: Vec::new(),
                actions: Vec::new(),
                reply_to: None,
                voice: false,
                audio: None,
            })
            .expect("conversa");
    }
    let running = start(&st);
    // O laço ainda não rodou (runtime de uma thread): estes eventos passam da capacidade.
    for n in 0..(EVENT_CHANNEL_CAPACITY + 10) {
        st.events().publish(ApiEvent::new(EventType::AgentActivity, Some("p1".into()), json!({ "n": n })));
    }
    wait_until(|| nanty.sweeps() >= 2).await;
    assert!(nanty.snapshot().cards.is_empty(), "a varredura tirou o projeto que sumiu");
    let lines: Vec<Option<String>> = nanty.journal_tail(10).into_iter().map(|e| e.project_id).collect();
    assert_eq!(lines, [Some("p1".to_string())], "o registro do projeto que sumiu saiu");
    let messages: Vec<Option<String>> = nanty.conversation().into_iter().map(|m| m.project_id).collect();
    assert_eq!(messages, [Some("p1".to_string())], "a conversa do projeto que sumiu saiu");
    st.trigger_shutdown();
    running.stop().await;
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn non_owner_never_writes_the_shared_folder() {
    let data = dir("non-owner");
    let owner = state_in(&data, Config::default());
    let running = start(&owner);
    let guest = state_in(&data, Config::default());
    let _ = start(&guest);
    guest.nanty().update(|s| {
        s.cards.push(card("x", None, NOON_MS + 60_000));
        true
    });
    guest.nanty().record(&JournalEntry::new(NOON_MS, JournalKind::Signal, "disk_low"));
    assert_eq!(Store::new(data.join(NANTY_DIR)).load(), store::NantyState::default());
    assert!(guest.nanty().journal_tail(5).is_empty());
    owner.trigger_shutdown();
    running.stop().await;
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn avatar_follows_status_busy_quiet_and_cards() {
    let data = dir("avatar");
    let cfg = crate::config::NantyConfig::default();
    let noon = Nanty::new(Some(data.join("a")), EventBus::new(), fixed_clock(at((2026, 10, 30), 12, 0, NOON_MS)));
    assert_eq!(noon.avatar(&cfg), Avatar::Sleeping, "desligado dorme");
    assert_eq!(noon.acquire(), InstanceStatus::Active);
    assert_eq!(noon.avatar(&cfg), Avatar::Attentive);
    noon.update(|s| {
        s.cards.push(card("c", None, NOON_MS + 1));
        true
    });
    assert_eq!(noon.avatar(&cfg), Avatar::Waiting);
    noon.set_busy(Some(Avatar::Thinking));
    assert_eq!(noon.avatar(&cfg), Avatar::Thinking);
    noon.set_busy(Some(Avatar::Speaking));
    assert_eq!(noon.avatar(&cfg), Avatar::Speaking);
    noon.set_busy(None);
    assert_eq!(noon.avatar(&cfg), Avatar::Waiting);
    // Relógio injetado: às 23:00 locais, dentro do silêncio (22:00 a 07:00), dorme.
    let night = Nanty::new(Some(data.join("b")), EventBus::new(), fixed_clock(at((2026, 10, 30), 23, 0, NOON_MS)));
    night.acquire();
    assert_eq!(night.avatar(&cfg), Avatar::Sleeping);
    let _ = std::fs::remove_dir_all(&data);
}

async fn get(app: axum::Router, path: &str) -> (StatusCode, Value) {
    let req = Request::builder().uri(path).header(header::HOST, HOST).body(Body::empty()).expect("req");
    let resp = app.oneshot(req).await.expect("resp");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("corpo").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn read_routes_show_config_masked_feed_conversation_and_status() {
    let data = dir("routes");
    let token = format!("{}:{}", 4_440_001, "AAF-segredo-das-rotas");
    let mut cfg = Config::default();
    cfg.nanty.model = "m-nanty".into();
    cfg.nanty.connection_id = "c1".into();
    cfg.nanty.lang = Some("en".into());
    cfg.nanty.telegram.bot_token = token.clone();
    cfg.nanty.telegram.chat_id = Some(777);
    cfg.nanty.telegram.owner_id = Some(778);
    // T10: o cabeçalho conta os projetos observados (o excluído fica de fora).
    for id in ["p1", "p2", "p3"] {
        cfg.projects.push(crate::config::Project { id: id.into(), name: id.into(), path: String::new(), created_at: String::new() });
    }
    cfg.nanty.excluded_projects = vec!["p2".into()];
    let st = state_in(&data, cfg);
    let running = start(&st);
    let nanty = Arc::clone(st.nanty());
    wait_until(|| nanty.sweeps() >= 1).await;
    nanty.update(|s| {
        s.cards.push(card("aberto", None, NOON_MS + 60_000));
        s.cards.push(card("vencido", None, NOON_MS - 1));
        s.push_done(DoneItem {
            at_ms: NOON_MS - 5_000,
            project_id: None,
            code: "docker_cleanup_done".into(),
            params: Map::new(),
            action: Some("docker_cleanup".into()),
            decided_by: Some(DecidedBy::Auto),
            result: "ok".into(),
            card_id: None,
            undo: None,
        });
        s.push_done(DoneItem { at_ms: NOON_MS - 86_400_000, ..s.done[0].clone() });
        s.spend = Spend { day: "2026-10-30".into(), usd: 0.07, input_tokens: 900, output_tokens: 80, unpriced_turns: 0, voice_calls: 0 };
        true
    });
    nanty.push_message(&ConversationMessage {
        id: new_id(),
        at_ms: NOON_MS,
        from: Speaker::Nanty,
        channel: Channel::Telegram,
        project_id: None,
        text: None,
        code: Some("digest".into()),
        params: json!({"count": 1}).as_object().cloned().unwrap_or_default(),
        lang: None,
        card_ids: Vec::new(),
        actions: Vec::new(),
        reply_to: None,
        voice: false,
        audio: None,
    });
    let app = crate::server::router(st.clone());

    let (code, config) = get(app.clone(), "/api/nanty/config").await;
    assert_eq!(code, StatusCode::OK);
    let text = config.to_string();
    assert!(!text.contains("segredo") && !text.contains("777") && !text.contains("778"), "{text}");
    assert_eq!(config["telegram"]["bot_token_masked"], "********otas");
    assert_eq!(config["telegram"]["paired"], true);
    assert_eq!(config["lang"], "en");
    assert_eq!(config["quiet_start"], "22:00");

    let (code, feed) = get(app.clone(), "/api/nanty/feed").await;
    assert_eq!(code, StatusCode::OK);
    let waiting: Vec<&str> = feed["waiting"].as_array().expect("lista").iter().filter_map(|c| c["id"].as_str()).collect();
    assert_eq!(waiting, ["aberto"], "o vencido não aparece");
    assert_eq!(feed["done"].as_array().map(Vec::len), Some(2));
    assert_eq!(feed["today"]["done"], 1, "só o de hoje conta no resumo");
    assert_eq!(feed["today"]["day"], "2026-10-30");
    assert_eq!(feed["today"]["spend"]["usd"], 0.07);
    assert_eq!(feed["today"]["budget_usd"], 0.5);

    let (code, conversation) = get(app.clone(), "/api/nanty/conversation").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(conversation["messages"][0]["code"], "digest");
    assert_eq!(conversation["messages"][0]["from"], "nanty");

    let (code, status) = get(app.clone(), "/api/nanty/status").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(status["instance"], "active");
    assert_eq!(status["avatar"], "waiting");
    assert_eq!(status["model_configured"], true);
    assert_eq!(status["telegram_paired"], true);
    assert_eq!(status["projects_observed"], 2);

    st.trigger_shutdown();
    running.stop().await;
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn changes_reach_the_global_stream_as_nanty_changed() {
    let data = dir("sse");
    let st = state_in(&data, Config::default());
    let mut rx = st.events().subscribe();
    st.nanty().set_busy(Some(Avatar::Thinking));
    let ev = rx.recv().await.expect("evento");
    assert_eq!(ev.event_type(), "nanty_changed");
    assert_eq!(ev.project_id, None);
    assert_eq!(ev.payload, json!({"part": "avatar"}));
    let _ = std::fs::remove_dir_all(&data);
}
