//! Testes do servidor com a conexão pelo plano ChatGPT (spec 116 T02): lista de modelos, teste de
//! conexão, "precisa entrar de novo" e geração de imagem. Provedor simulado local (127.0.0.1);
//! nenhuma rede real nem Docker. Tokens falsos montados em partes; pasta temporária com guarda.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::store::{self, Credentials};
use super::{PlanStatus, DIRECT_SCOPE, SCOPES};
use crate::agents::llm_client::stream_tests::raw_server;
use crate::config::{AgentConfig, Config, Connection, Effort};
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";
const CONN: &str = "c-plano";
const JSON: &str = "application/json";

fn fake(kind: &str) -> String {
    format!("{}-{kind}", concat!("tok", "en"))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("nants-plan-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("pasta temporária");
        Self(dir)
    }

    fn data(&self) -> PathBuf {
        self.0.join("data")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Credenciais válidas por uma hora (sem renovação no teste).
fn save_login(dir: &Path) {
    let creds = Credentials {
        client_id: format!("{}{}", concat!("oai", "app_"), "Teste01"),
        access_token: fake("acesso"),
        refresh_token: fake("renova"),
        id_token: fake("id"),
        expires_at: now_secs() + 3600,
        scopes: SCOPES.split_whitespace().map(str::to_string).collect(),
        email: "pessoa@example.com".into(),
        sub: "user-1".into(),
        saved_at: now_secs(),
        needs_login: false,
    };
    assert!(creds.scopes.iter().any(|s| s == DIRECT_SCOPE));
    store::save_credentials(dir, CONN, &creds).expect("gravar credenciais");
}

/// Estado com a conexão pelo plano apontada para o provedor simulado e os três papéis nela.
fn state_for(dir: &TempDir, base: &str) -> AppState {
    let mut conn = Connection::chatgpt_plan(CONN, "ChatGPT (pessoa@example.com)");
    conn.base_url = base.to_string();
    let agent = AgentConfig { connection_id: CONN.into(), model: "gpt-plano".into(), effort: Effort::Low, max_steps: None };
    let mut cfg = Config::default();
    cfg.connections.push(conn);
    cfg.agents.architect = agent.clone();
    cfg.agents.developer = agent.clone();
    cfg.agents.reviewer = agent;
    AppState::new(dir.0.join("config.toml"), cfg, 3000).with_data_dir(dir.data())
}

async fn call(state: &AppState, method: Method, uri: &str, body: Option<Value>) -> Value {
    let mut b = Request::builder().method(method).uri(uri).header(header::HOST, HOST);
    let body = match body {
        Some(v) => {
            b = b.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = router(state.clone()).oneshot(b.body(body).expect("requisição")).await.expect("resposta");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("corpo");
    serde_json::from_slice(&bytes).expect("json")
}

fn requests(cap: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    cap.lock().expect("lock").clone()
}

const MODELS: &str = r#"{"models":[
    {"slug":"gpt-z","display_name":"GPT Z","visibility":"list"},
    {"slug":"gpt-oculto","display_name":"Oculto","visibility":"hide"},
    {"slug":"gpt-a","display_name":"GPT A","visibility":"list"}
]}"#;

#[tokio::test]
async fn models_of_the_plan_connection_are_filtered_in_server_order() {
    let dir = TempDir::new("modelos");
    save_login(&dir.data());
    let (base, cap) = raw_server(vec![(200, JSON, MODELS.to_string())]).await;
    let state = state_for(&dir, &base);
    let v = call(&state, Method::GET, &format!("/api/config/connections/{CONN}/models"), None).await;
    assert_eq!(v["models"], json!([{"id": "gpt-z", "name": "GPT Z"}, {"id": "gpt-a", "name": "GPT A"}]), "{v}");
    assert!(v.get("error").is_none());
    let head = requests(&cap)[0].to_ascii_lowercase();
    assert!(head.starts_with("get /v1/models"), "{head}");
    assert!(head.contains(&format!("authorization: bearer {}", fake("acesso"))), "{head}");
}

#[tokio::test]
async fn connection_test_uses_the_login_and_maps_plan_errors() {
    let dir = TempDir::new("teste");
    save_login(&dir.data());
    let limit = json!({"error": {"code": "subscription_sharing_usage_limit_exceeded"}}).to_string();
    let (base, cap) = raw_server(vec![(200, JSON, MODELS.to_string()), (429, JSON, limit)]).await;
    let state = state_for(&dir, &base);
    let v = call(&state, Method::POST, "/api/config/connections/test", Some(json!({"id": CONN}))).await;
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["models_count"], json!(2));
    let v = call(&state, Method::POST, "/api/config/connections/test", Some(json!({"id": CONN, "model": "gpt-z"}))).await;
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["error"], json!("plan_limit"), "{v}");
    let reqs = requests(&cap);
    assert_eq!(reqs.len(), 2, "o limite não repete");
    let body = reqs[1].split("\r\n\r\n").nth(1).unwrap_or_default();
    let body: Value = serde_json::from_str(body).expect("corpo");
    assert_eq!((body["stream"].clone(), body["store"].clone()), (json!(true), json!(false)));
}

#[tokio::test]
async fn without_login_the_plan_connection_asks_to_sign_in() {
    let dir = TempDir::new("sem-login");
    let (base, cap) = raw_server(vec![]).await;
    let state = state_for(&dir, &base);
    let v = call(&state, Method::GET, &format!("/api/config/connections/{CONN}/models"), None).await;
    assert_eq!(v["error"], json!("plan_needs_login"), "{v}");
    let v = call(&state, Method::POST, "/api/config/connections/test", Some(json!({"id": CONN, "model": "gpt-z"}))).await;
    assert_eq!(v["error"], json!("plan_needs_login"), "{v}");
    assert!(requests(&cap).is_empty(), "nenhuma chamada ao provedor sem login");
}

#[tokio::test]
async fn invalid_user_marks_the_connection_as_needing_login() {
    let dir = TempDir::new("invalido");
    save_login(&dir.data());
    let invalid = json!({"error": {"code": "subscription_sharing_invalid_user"}}).to_string();
    let (base, _) = raw_server(vec![(401, JSON, invalid)]).await;
    let state = state_for(&dir, &base);
    let v = call(&state, Method::POST, "/api/config/connections/test", Some(json!({"id": CONN, "model": "gpt-z"}))).await;
    assert_eq!(v["error"], json!("plan_needs_login"), "{v}");
    let status = state.chatgpt().status(state.data_dir(), CONN);
    assert!(matches!(status, PlanStatus::NeedsLogin { .. }), "{status:?}");
    let creds = store::load_credentials(&dir.data(), CONN).expect("ler").expect("credenciais");
    assert!(creds.needs_login && creds.access_token.is_empty() && creds.refresh_token.is_empty());
}

#[tokio::test]
async fn image_generation_is_not_offered_on_the_plan() {
    let dir = TempDir::new("imagem");
    save_login(&dir.data());
    let state = state_for(&dir, "http://127.0.0.1:9/v1");
    assert!(!state.developer_image_generation_available().await);
    let cfg = state.config().read().await.clone();
    assert!(crate::server::image_gen::LlmImageGenerator::for_developer(&cfg, &state).is_none());
    // O cliente do papel é o do modo plano (sem chave, com o token do login).
    let conn = cfg.connections[0].clone();
    let client = super::plan_client::llm_client(&state, &conn).expect("cliente");
    assert!(client.is_plan());
}
