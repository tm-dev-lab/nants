//! Testes da gravação dos Ajustes e das Regras do Nanty (spec 138 T11): só o que mudou vai para o
//! arquivo, cada campo é validado com o código traduzido, o token nunca passa pelo `PUT`, o nível
//! de uma ação por id do catálogo (a faixa "só a pessoa" travada), a hora local do Nants na leitura
//! e o mesmo critério de pareamento nas três leituras.

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::{ActionLevel, Config, Connection, ConnectionKind};
use crate::server::nanty::clock::{at, fixed_clock};
use crate::server::state::AppState;

/// 2026-10-30 09:41 UTC (a hora fixa do relógio local dos testes).
const NOW_MS: u64 = 1_793_353_260_000;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-t11-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

fn connection(id: &str) -> Connection {
    Connection {
        id: id.into(),
        name: id.into(),
        base_url: "http://127.0.0.1:9".into(),
        api_key: String::new(),
        wire_api: Default::default(),
        kind: Default::default(),
        base_url_override: None,
    }
}

fn state(data: &PathBuf, change: impl FnOnce(&mut Config)) -> AppState {
    let mut cfg = Config::default();
    // c1: Google Gemini (fala e transcrição); c2: por chave (só a fala); c3: o plano ChatGPT (nenhuma).
    cfg.connections = vec![
        Connection { kind: ConnectionKind::GoogleGemini, ..connection("c1") },
        connection("c2"),
        Connection { kind: ConnectionKind::ChatgptPlan, ..connection("c3") },
    ];
    change(&mut cfg);
    AppState::new(data.join("config.toml"), cfg, 3000)
        .with_data_dir(data.clone())
        .with_nanty_clock(fixed_clock(at((2026, 10, 30), 9, 41, NOW_MS)))
}

async fn call(state: &AppState, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header(header::HOST, "127.0.0.1:3000");
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
    let resp = crate::server::router(state.clone()).oneshot(req.body(body).expect("req")).await.expect("resp");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("corpo").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn saved(data: &PathBuf) -> String {
    std::fs::read_to_string(data.join("config.toml")).unwrap_or_default()
}

#[tokio::test]
async fn put_config_changes_only_what_was_sent_and_writes_only_what_differs() {
    let data = dir("partial");
    let st = state(&data, |_| {});
    let (status, body) = call(
        &st,
        Method::PUT,
        "/api/nanty/config",
        Some(json!({"digest_time": "09:30", "quiet_start": "23:00", "connection_id": "c1", "model": " m-nanty ", "effort": "medium"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["digest_time"].as_str(), body["quiet_start"].as_str(), body["quiet_end"].as_str()), (Some("09:30"), Some("23:00"), Some("07:00")));
    assert_eq!((body["model"].as_str(), body["effort"].as_str()), (Some("m-nanty"), Some("medium")));
    assert_eq!(body["now"], "09:41", "a hora local do Nants (C-15)");
    let file = saved(&data);
    let section = file.split("[nanty]").nth(1).expect("seção");
    for absent in ["quiet_end", "daily_budget_usd", "voice", "telegram", "excluded_projects"] {
        assert!(!section.contains(absent), "{absent} gravado sem mudar: {file}");
    }
    assert!(section.contains("digest_time = \"09:30\""), "{file}");

    // Os blocos aninhados também mudam só no que veio.
    let (status, body) = call(
        &st,
        Method::PUT,
        "/api/nanty/config",
        Some(json!({
            "voice": {"enabled": true, "order": [{"connection_id": "c2"}], "transcribe": {"connection_id": "c1", "model": "m-t"}},
            "telegram": {"hold_while_visible": false},
            "reserve": {"developer": {"connection_id": "c2", "model": "m-r"}, "reviewer": {"connection_id": "", "model": ""}},
            "excluded_projects": ["p2", "p2", "p3"],
            "daily_budget_usd": 1.5,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["voice"]["enabled"], true);
    assert_eq!(body["voice"]["order"][0]["connection_id"], "c2");
    assert_eq!(body["voice"]["max_input_secs"], 300, "o que não veio fica");
    assert_eq!((body["telegram"]["hold_while_visible"].as_bool(), body["telegram"]["visible_hold_secs"].as_u64()), (Some(false), Some(180)));
    assert_eq!(body["reserve"]["developer"]["model"], "m-r");
    assert!(body["reserve"].get("reviewer").is_none(), "conexão vazia = sem reserva");
    assert_eq!(body["excluded_projects"], json!(["p2", "p3"]));
    let cfg = st.config().read().await.nanty.clone();
    assert_eq!((cfg.digest_time.as_str(), cfg.daily_budget_usd), ("09:30", 1.5));
    assert!(!cfg.observes("p2") && cfg.observes("p1"));

    // Sem mudança, nada é gravado.
    let before = std::fs::metadata(data.join("config.toml")).and_then(|m| m.modified()).expect("mtime");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let (status, _) = call(&st, Method::PUT, "/api/nanty/config", Some(json!({"digest_time": "09:30"}))).await;
    assert_eq!(status, StatusCode::OK);
    let after = std::fs::metadata(data.join("config.toml")).and_then(|m| m.modified()).expect("mtime");
    assert_eq!(before, after, "o pedido sem mudança não grava");
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn invalid_values_come_back_coded_and_change_nothing() {
    let data = dir("invalid");
    let st = state(&data, |_| {});
    let cases = [
        (json!({"digest_time": "8:00"}), "digest_time"),
        (json!({"quiet_end": "24:00"}), "quiet_end"),
        (json!({"daily_budget_usd": -1.0}), "daily_budget_usd"),
        (json!({"daily_budget_usd": 5000.0}), "daily_budget_usd"),
        (json!({"daily_budget_tokens": 1_000_000_000_u64}), "daily_budget_tokens"),
        (json!({"connection_id": "nao-existe"}), "connection_id"),
        (json!({"reserve": {"chefe": {"connection_id": "c1", "model": "m"}}}), "reserve"),
        (json!({"reserve": {"developer": {"connection_id": "c1", "model": " "}}}), "reserve.developer"),
        (json!({"voice": {"order": [{"connection_id": "nao-existe"}]}}), "voice.order"),
        (json!({"voice": {"max_input_secs": 2}}), "voice.max_input_secs"),
        // A conexão de um tipo que o caminho não usa (as regras de `voice::speaks` e `transcribes`).
        (json!({"voice": {"transcribe": {"connection_id": "c2", "model": ""}}}), "voice.transcribe"),
        (json!({"voice": {"transcribe": {"connection_id": "c3", "model": ""}}}), "voice.transcribe"),
        (json!({"voice": {"order": [{"connection_id": "c3"}]}}), "voice.order"),
        (json!({"voice": {"order": [{"connection_id": "c1"}, {"connection_id": "c3"}]}}), "voice.order"),
        (json!({"telegram": {"visible_hold_secs": 99_999}}), "telegram.visible_hold_secs"),
        (json!({"excluded_projects": ["../x"]}), "excluded_projects"),
    ];
    for (body, field) in cases {
        let (status, resp) = call(&st, Method::PUT, "/api/nanty/config", Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!((resp["code"].as_str(), resp["params"]["field"].as_str()), (Some("nanty_config_invalid"), Some(field)), "{body}: {resp}");
    }
    assert!(st.config().read().await.nanty.is_default(), "nenhum pedido inválido mudou algo");
    assert!(!data.join("config.toml").exists());
    for lang in crate::i18n::Lang::ALL {
        let text = crate::i18n::tr(lang, "serverError.nanty_config_invalid", &[("field", "digest_time")]);
        assert!(text.contains("digest_time") && !text.contains('{'), "{lang:?}: {text}");
    }
    let _ = std::fs::remove_dir_all(&data);
}

/// O token, o chat e o dono do Telegram nunca passam pelo `PUT` (só pelas rotas do canal), nem o
/// idioma (segue a interface, C-08).
#[tokio::test]
async fn the_token_and_the_chat_never_go_through_put() {
    let data = dir("token");
    let st = state(&data, |_| {});
    for body in [
        json!({"telegram": {"bot_token": format!("{}:{}", 1234, "segredo-do-put")}}),
        json!({"telegram": {"chat_id": 5}}),
        json!({"telegram": {"owner_id": 5}}),
        json!({"bot_token": "x"}),
        json!({"lang": "en"}),
        json!({"actions": {"approve_plan": "auto"}}),
    ] {
        let (status, resp) = call(&st, Method::PUT, "/api/nanty/config", Some(body.clone())).await;
        assert_eq!((status, resp["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("json_invalid_fields")), "{body}");
    }
    let cfg = st.config().read().await.nanty.clone();
    assert!(cfg.telegram.bot_token.is_empty() && cfg.telegram.chat_id.is_none() && cfg.lang.is_none());
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test]
async fn the_level_changes_by_catalog_id_and_the_person_only_band_stays_locked() {
    let data = dir("levels");
    let st = state(&data, |_| {});
    let (status, body) = call(&st, Method::GET, "/api/nanty/config", None).await;
    assert_eq!(status, StatusCode::OK);
    let catalog = body["catalog"].as_array().expect("catálogo");
    let ids: Vec<(&str, &str, &str)> = catalog
        .iter()
        .map(|e| (e["id"].as_str().unwrap_or(""), e["kind"].as_str().unwrap_or(""), e["level"].as_str().unwrap_or("")))
        .collect();
    assert_eq!(
        ids,
        [
            ("approve_plan", "executes", "ask"),
            ("resume_run", "executes", "ask"),
            ("retry_provider", "executes", "auto"),
            ("swap_reserve", "executes", "auto"),
            ("clean_orphans", "executes", "auto"),
            ("pause_run", "executes", "ask"),
            ("allow_command_always", "person_only", "ask"),
            ("change_channel", "person_only", "ask"),
        ],
        "as opções do cartão e os links não têm nível"
    );

    let (status, body) = call(&st, Method::PUT, "/api/nanty/actions/retry_provider", Some(json!({"level": "off"}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let retry = body["catalog"].as_array().and_then(|c| c.iter().find(|e| e["id"] == "retry_provider")).cloned().unwrap_or_default();
    assert_eq!((retry["level"].as_str(), retry["default_level"].as_str()), (Some("off"), Some("auto")));
    assert_eq!(st.config().read().await.nanty.actions.get("retry_provider"), Some(&ActionLevel::Off));
    assert!(saved(&data).contains("retry_provider = \"off\""));

    // De volta ao inicial: sai do arquivo.
    let (status, _) = call(&st, Method::PUT, "/api/nanty/actions/retry_provider", Some(json!({"level": "auto"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(st.config().read().await.nanty.actions.is_empty());
    assert!(!saved(&data).contains("retry_provider"));

    for id in ["allow_command_always", "change_channel", "remind_later", "open_nants"] {
        let (status, resp) = call(&st, Method::PUT, &format!("/api/nanty/actions/{id}"), Some(json!({"level": "auto"}))).await;
        assert_eq!((status, resp["code"].as_str()), (StatusCode::CONFLICT, Some("nanty_action_locked")), "{id}");
    }
    let (status, resp) = call(&st, Method::PUT, "/api/nanty/actions/apagar_tudo", Some(json!({"level": "auto"}))).await;
    assert_eq!((status, resp["code"].as_str()), (StatusCode::NOT_FOUND, Some("nanty_action_unknown")));
    let (status, resp) = call(&st, Method::PUT, "/api/nanty/actions/approve_plan", Some(json!({"level": "sempre"}))).await;
    assert_eq!((status, resp["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("json_invalid_fields")));
    assert!(st.config().read().await.nanty.actions.is_empty());
    let _ = std::fs::remove_dir_all(&data);
}

/// Em inglês, o Nanty não leva pronome (he, him, his): o nome ou a frase sem pronome.
#[test]
fn english_nanty_texts_have_no_he_him_or_his() {
    let found: Vec<&str> = crate::i18n::en::MESSAGES
        .iter()
        .filter(|(key, text)| {
            key.starts_with("nanty.")
                && text
                    .split(|c: char| !c.is_ascii_alphabetic())
                    .any(|w| matches!(w.to_ascii_lowercase().as_str(), "he" | "him" | "his" | "himself"))
        })
        .map(|(key, _)| *key)
        .collect();
    assert!(found.is_empty(), "{found:?}");
}

/// Pendência da T06: o status, a configuração e a vista do Telegram dizem "pareado" pelo mesmo
/// critério, o chat e o dono.
#[tokio::test]
async fn paired_uses_the_chat_and_the_owner_everywhere() {
    for (chat, owner, paired) in [(Some(7), None, false), (None, Some(8), false), (Some(7), Some(8), true)] {
        let data = dir("paired");
        let st = state(&data, |c| {
            c.nanty.telegram.bot_token = format!("{}:{}", 4321, "AAF-segredo-pareado");
            c.nanty.telegram.chat_id = chat;
            c.nanty.telegram.owner_id = owner;
        });
        let (_, status) = call(&st, Method::GET, "/api/nanty/status", None).await;
        let (_, config) = call(&st, Method::GET, "/api/nanty/config", None).await;
        let (_, telegram) = call(&st, Method::GET, "/api/nanty/telegram", None).await;
        assert_eq!(status["telegram_paired"], paired, "{chat:?} {owner:?}");
        assert_eq!(config["telegram"]["paired"], paired, "{chat:?} {owner:?}");
        assert_eq!(telegram["paired"], paired, "{chat:?} {owner:?}");
        for text in [status.to_string(), config.to_string(), telegram.to_string()] {
            assert!(!text.contains("segredo"), "{text}");
        }
        let _ = std::fs::remove_dir_all(&data);
    }
}
