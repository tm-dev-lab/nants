//! Testes do login pelo plano ChatGPT (spec 116, T01) contra um servidor OAuth e JWKS simulado,
//! local (127.0.0.1, porta livre). Nenhum teste usa a rede real nem o Docker. As chaves RSA são
//! fixas e só de teste ([`super::test_keys`]); os tokens são montados em partes. Pastas temporárias
//! com guarda `Drop`; relógio manual.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State as AxState};
use axum::http::{header, Method, Request, Response, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::{Config, ConnectionKind, WireApi, CHATGPT_PLAN_BASE_URL};
use crate::server::lan::{ModeSource, NetworkRun};
use crate::server::security::HostPolicy;
use crate::server::router;

const PORT: u16 = 3000;
const HOST: &str = "127.0.0.1:3000";
const LOOPBACK_PEER: &str = "127.0.0.1:50001";
const NAT_PEER: &str = "172.21.144.1:50000";
const T0_MS: u64 = 1_800_000_000_000;
const EMAIL: &str = "pessoa@example.com";
const SUB: &str = "user-123";

/// `client_id` emitido falso, montado em partes.
fn issued_client() -> String {
    format!("{}{}", concat!("oai", "app_"), "TesteEmitido01")
}

/// Tokens falsos montados em partes (nunca com cara de segredo real).
fn fake(kind: &str, n: u32) -> String {
    format!("{}-{kind}-{n}", concat!("tok", "en"))
}

// --- pasta e relógio ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("nants-chatgpt-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("pasta temporária");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
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

#[derive(Clone)]
struct Clock(Arc<AtomicU64>);

impl Clock {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(T0_MS)))
    }

    fn handle(&self) -> AuthClock {
        let c = Arc::clone(&self.0);
        Arc::new(move || c.load(Ordering::SeqCst))
    }

    fn secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst) / 1000
    }

    fn advance_secs(&self, s: u64) {
        self.0.fetch_add(s * 1000, Ordering::SeqCst);
    }
}

// --- chaves e tokens ---------------------------------------------------------------------------

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex")).collect()
}

struct TestKey {
    kid: &'static str,
    pair: RsaKeyPair,
}

impl TestKey {
    fn a() -> Self {
        Self { kid: "kid-a", pair: RsaKeyPair::from_der(&unhex(super::test_keys::KEY_A)).expect("chave A") }
    }

    fn b() -> Self {
        Self { kid: "kid-b", pair: RsaKeyPair::from_der(&unhex(super::test_keys::KEY_B)).expect("chave B") }
    }

    fn jwk(&self) -> Value {
        let c = ring::signature::RsaPublicKeyComponents::<Vec<u8>>::from(self.pair.public());
        json!({"kty": "RSA", "alg": "RS256", "use": "sig", "kid": self.kid, "n": jwt::b64url_encode(&c.n), "e": jwt::b64url_encode(&c.e)})
    }

    /// JWT RS256 com o cabeçalho e as declarações dados.
    fn sign_with_header(&self, header: Value, claims: &Value) -> String {
        let h = jwt::b64url_encode(header.to_string().as_bytes());
        let c = jwt::b64url_encode(claims.to_string().as_bytes());
        let input = format!("{h}.{c}");
        let mut sig = vec![0u8; self.pair.public().modulus_len()];
        self.pair.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), input.as_bytes(), &mut sig).expect("assinatura");
        format!("{input}.{}", jwt::b64url_encode(&sig))
    }

    fn sign(&self, claims: &Value) -> String {
        self.sign_with_header(json!({"alg": "RS256", "kid": self.kid, "typ": "JWT"}), claims)
    }
}

fn claims(nonce: &str, aud: &str, exp: u64) -> Value {
    json!({"iss": ISSUER, "aud": aud, "exp": exp, "iat": exp - 3600, "nonce": nonce, "sub": SUB, "email": EMAIL})
}

// --- servidor OAuth simulado -------------------------------------------------------------------

type Reply = Box<dyn Fn(&HashMap<String, String>) -> (u16, Value) + Send + Sync>;

struct Mock {
    a: TestKey,
    b: TestKey,
    /// `kid`s publicados no JWKS.
    published: Mutex<Vec<&'static str>>,
    jwks_hits: AtomicUsize,
    token_forms: Mutex<Vec<HashMap<String, String>>>,
    revoke_forms: Mutex<Vec<HashMap<String, String>>>,
    token_reply: Mutex<Reply>,
    revoke_status: Mutex<u16>,
    /// Atraso do endpoint de token (concorrência).
    token_delay: Mutex<Duration>,
}

impl Mock {
    fn token_hits(&self) -> usize {
        self.token_forms.lock().expect("lock").len()
    }

    fn last_token_form(&self) -> HashMap<String, String> {
        self.token_forms.lock().expect("lock").last().cloned().expect("pedido de token")
    }

    fn reply(&self, f: impl Fn(&HashMap<String, String>) -> (u16, Value) + Send + Sync + 'static) {
        *self.token_reply.lock().expect("lock") = Box::new(f);
    }
}

/// Corpo form-urlencoded (a mesma decodificação da query de uma URL).
fn form_of(body: &[u8]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(body);
    query_of(&format!("http://form.test/?{text}"))
}

async fn jwks(AxState(m): AxState<Arc<Mock>>) -> axum::Json<Value> {
    m.jwks_hits.fetch_add(1, Ordering::SeqCst);
    let published = m.published.lock().expect("lock").clone();
    let keys: Vec<Value> = [&m.a, &m.b].iter().filter(|k| published.contains(&k.kid)).map(|k| k.jwk()).collect();
    axum::Json(json!({ "keys": keys }))
}

async fn token(AxState(m): AxState<Arc<Mock>>, body: Bytes) -> axum::response::Response {
    let form = form_of(&body);
    let delay = *m.token_delay.lock().expect("lock");
    m.token_forms.lock().expect("lock").push(form.clone());
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let (status, body) = (m.token_reply.lock().expect("lock"))(&form);
    (StatusCode::from_u16(status).expect("status"), axum::Json(body)).into_response()
}

async fn revoke(AxState(m): AxState<Arc<Mock>>, body: Bytes) -> StatusCode {
    let form = form_of(&body);
    m.revoke_forms.lock().expect("lock").push(form);
    StatusCode::from_u16(*m.revoke_status.lock().expect("lock")).expect("status")
}

async fn spawn_mock() -> (Arc<Mock>, String) {
    let mock = Arc::new(Mock {
        a: TestKey::a(),
        b: TestKey::b(),
        published: Mutex::new(vec!["kid-a"]),
        jwks_hits: AtomicUsize::new(0),
        token_forms: Mutex::new(Vec::new()),
        revoke_forms: Mutex::new(Vec::new()),
        token_reply: Mutex::new(Box::new(|_| (500, json!({})))),
        revoke_status: Mutex::new(200),
        token_delay: Mutex::new(Duration::ZERO),
    });
    let app = Router::new()
        .route("/jwks", get(jwks))
        .route("/token", post(token))
        .route("/revoke", post(revoke))
        .with_state(Arc::clone(&mock));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (mock, format!("http://{addr}"))
}

fn endpoints(base: &str) -> ChatgptEndpoints {
    ChatgptEndpoints {
        authorize: "https://auth.example.test/authorize".into(),
        token: format!("{base}/token"),
        revoke: format!("{base}/revoke"),
        jwks: format!("{base}/jwks"),
    }
}

/// Resposta de troca com o ID token assinado pela chave A para o `nonce` dado.
fn login_reply(m: &Mock, nonce: &str, exp: u64) -> Value {
    json!({
        "access_token": fake("acesso", 1),
        "refresh_token": fake("renova", 1),
        "id_token": m.a.sign(&claims(nonce, &issued_client(), exp)),
        "expires_in": 3600,
        "scope": SCOPES,
        "token_type": "Bearer",
    })
}

// --- servidor do Nants -------------------------------------------------------------------------

struct Env {
    dir: TempDir,
    clock: Clock,
    mock: Arc<Mock>,
    state: AppState,
}

async fn env(tag: &str) -> Env {
    env_with(tag, |s| s).await
}

async fn env_with(tag: &str, f: impl FnOnce(AppState) -> AppState) -> Env {
    let dir = TempDir::new(tag);
    let clock = Clock::new();
    let (mock, base) = spawn_mock().await;
    let state = f(AppState::new(dir.path().join("config.toml"), Config::default(), PORT)
        .with_data_dir(dir.data())
        .with_chatgpt(ChatgptAuth::new(endpoints(&base), clock.handle())));
    Env { dir, clock, mock, state }
}

fn req(method: Method, uri: &str, peer: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri).header(header::HOST, HOST);
    if let Some(token) = cookie {
        b = b.header(header::COOKIE, format!("{}={token}", crate::server::devices::COOKIE_NAME));
    }
    let body = match body {
        Some(v) => {
            b = b.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let mut r = b.body(body).expect("requisição");
    let addr: SocketAddr = peer.parse().expect("endereço");
    r.extensions_mut().insert(ConnectInfo(addr));
    r
}

async fn send(state: &AppState, r: Request<Body>) -> Response<Body> {
    router(state.clone()).oneshot(r).await.expect("resposta")
}

async fn body_text(resp: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("corpo");
    String::from_utf8_lossy(&bytes).to_string()
}

fn location(resp: &Response<Body>) -> String {
    resp.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

/// Parâmetros da URL de autorização.
fn query_of(url: &str) -> HashMap<String, String> {
    reqwest::Url::parse(url).expect("url").query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

/// Inicia o login (de loopback) e devolve os parâmetros da URL.
async fn start(e: &Env, body: Value) -> HashMap<String, String> {
    let resp = send(&e.state, req(Method::POST, START_PATH, LOOPBACK_PEER, None, Some(body))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: Value = serde_json::from_str(&body_text(resp).await).expect("json");
    assert_eq!(v["local"], json!(true));
    query_of(v["url"].as_str().expect("url"))
}

fn callback_uri(params: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("http://127.0.0.1:3000/auth/callback").expect("url");
    url.query_pairs_mut().extend_pairs(params);
    format!("{}?{}", url.path(), url.query().unwrap_or(""))
}

async fn callback(e: &Env, params: &[(&str, &str)]) -> String {
    let resp = send(&e.state, req(Method::GET, &callback_uri(params), LOOPBACK_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    location(&resp)
}

/// Login completo; devolve o id da conexão.
async fn login(e: &Env) -> String {
    let q = start(e, json!({})).await;
    let reply = login_reply(&e.mock, &q["nonce"], e.clock.secs() + 3600);
    e.mock.reply(move |_| (200, reply.clone()));
    let client = issued_client();
    let loc = callback(e, &[("code", "codigo-1"), ("state", &q["state"]), ("client_id", &client)]).await;
    loc.strip_prefix("/?chatgpt=connected&connection=").expect(&loc).to_string()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).expect("arquivo").permissions().mode() & 0o777
}

fn creds(e: &Env, id: &str) -> Option<Credentials> {
    store::load_credentials(&e.dir.data(), id).expect("credenciais legíveis")
}

// --- testes ------------------------------------------------------------------------------------

#[tokio::test]
async fn first_login_builds_the_exact_url_and_saves_everything() {
    let e = env("feliz").await;
    let q = start(&e, json!({})).await;
    for (k, v) in [
        ("response_type", "code"),
        ("client_id", DYNAMIC_CLIENT_ID),
        ("agent_name_hint", "Nants"),
        ("redirect_uri", "http://127.0.0.1:3000/auth/callback"),
        ("scope", "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"),
        ("code_challenge_method", "S256"),
        ("resource", "https://api.openai.com/v1"),
    ] {
        assert_eq!(q.get(k).map(String::as_str), Some(v), "{k}");
    }
    assert!(!q.contains_key("id_token_hint") && !q.contains_key("login_hint"));
    assert!(q["state"].len() >= 43 && q["nonce"].len() >= 43 && q["state"] != q["nonce"]);
    // Identificador da instalação: gravado antes do login, 0600, reutilizado.
    let host = q["ext_agent_host_id"].clone();
    assert!(store::valid_host_id(&host), "{host}");
    assert_eq!(&host[23..24], "4", "UUID v4");
    let host_file = e.dir.data().join(store::HOST_ID_FILE);
    assert_eq!(mode(&host_file), 0o600);
    let again = start(&e, json!({})).await;
    assert_eq!(again["ext_agent_host_id"], host);
    assert_ne!(again["state"], q["state"], "state novo a cada tentativa");
    assert_ne!(again["code_challenge"], q["code_challenge"], "PKCE novo a cada tentativa");

    let reply = login_reply(&e.mock, &q["nonce"], e.clock.secs() + 3600);
    e.mock.reply(move |_| (200, reply.clone()));
    let client = issued_client();
    let loc = callback(&e, &[("code", "codigo-1"), ("state", &q["state"]), ("client_id", &client)]).await;
    let id = loc.strip_prefix("/?chatgpt=connected&connection=").expect(&loc).to_string();

    // Troca: form-urlencoded, sem secret, com o client emitido, o verificador e o resource.
    let form = e.mock.last_token_form();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "codigo-1");
    assert_eq!(form["redirect_uri"], "http://127.0.0.1:3000/auth/callback");
    assert_eq!(form["client_id"], issued_client());
    assert_eq!(form["resource"], RESOURCE);
    assert_eq!(pkce_challenge(&form["code_verifier"]), q["code_challenge"]);
    assert!(!form.keys().any(|k| k.contains("secret")));

    // Credenciais em 0600 com todos os campos.
    let path = store::credentials_path(&e.dir.data(), &id).expect("caminho");
    assert_eq!(mode(&path), 0o600);
    let c = creds(&e, &id).expect("credenciais");
    assert_eq!(c.client_id, issued_client());
    assert_eq!(c.access_token, fake("acesso", 1));
    assert_eq!(c.refresh_token, fake("renova", 1));
    assert!(!c.id_token.is_empty());
    assert_eq!(c.expires_at, e.clock.secs() + 3600);
    assert!(c.scopes.iter().any(|s| s == DIRECT_SCOPE));
    assert_eq!((c.email.as_str(), c.sub.as_str(), c.saved_at), (EMAIL, SUB, e.clock.secs()));
    let dbg = format!("{c:?}");
    for secret in [&c.access_token, &c.refresh_token, &c.id_token, &c.email] {
        assert!(!dbg.contains(secret.as_str()), "Debug vazou: {dbg}");
    }

    // Conexão criada: tipo plano, endereço e protocolo fixos, sem chave; gravada no TOML.
    let cfg = e.state.config().read().await.clone();
    let conn = cfg.connections.iter().find(|c| c.id == id).expect("conexão");
    assert_eq!(conn.kind, ConnectionKind::ChatgptPlan);
    assert_eq!((conn.base_url.as_str(), conn.wire_api), (CHATGPT_PLAN_BASE_URL, WireApi::Responses));
    assert_eq!(conn.name, format!("ChatGPT ({EMAIL})"));
    let toml = std::fs::read_to_string(e.dir.path().join("config.toml")).expect("config");
    assert!(toml.contains("kind = \"chatgpt_plan\"") && !toml.contains("api_key"), "{toml}");
    assert!(!toml.contains(&c.access_token) && !toml.contains(&c.refresh_token));

    // Estado e leitura da configuração: conectada, com o e-mail, sem segredos.
    let status = body_text(send(&e.state, req(Method::GET, &format!("/api/connections/{id}/chatgpt"), LOOPBACK_PEER, None, None)).await).await;
    assert_eq!(serde_json::from_str::<Value>(&status).expect("json"), json!({"state": "connected", "email": EMAIL}));
    let config = body_text(send(&e.state, req(Method::GET, "/api/config", LOOPBACK_PEER, None, None)).await).await;
    let v: Value = serde_json::from_str(&config).expect("json");
    let view = v["connections"].as_array().and_then(|l| l.iter().find(|c| c["id"] == json!(id))).expect("view");
    assert_eq!(view["kind"], json!("chatgpt_plan"));
    assert_eq!(view["chatgpt"], json!({"state": "connected", "email": EMAIL}));
    for secret in [&c.access_token, &c.refresh_token, &c.id_token, &c.client_id] {
        assert!(!config.contains(secret.as_str()) && !status.contains(secret.as_str()));
    }

    // O state é de uso único.
    let reuse = callback(&e, &[("code", "codigo-1"), ("state", &q["state"]), ("client_id", &client)]).await;
    assert_eq!(reuse, "/?chatgpt=error&reason=state_invalid");
}

#[tokio::test]
async fn reauthorization_uses_the_saved_client_and_rejects_another() {
    let e = env("reauth").await;
    let id = login(&e).await;
    let before = creds(&e, &id).expect("credenciais");
    let q = start(&e, json!({"connection_id": id})).await;
    assert_eq!(q["client_id"], issued_client());
    assert_eq!(q["login_hint"], EMAIL);
    assert_eq!(q["id_token_hint"], before.id_token);
    assert!(!q.contains_key("agent_name_hint"));
    let reply = login_reply(&e.mock, &q["nonce"], e.clock.secs() + 3600);
    e.mock.reply(move |_| (200, reply.clone()));
    let other = format!("{}{}", concat!("oai", "app_"), "OutroCliente");
    let loc = callback(&e, &[("code", "c2"), ("state", &q["state"]), ("client_id", &other)]).await;
    assert_eq!(loc, "/?chatgpt=error&reason=client_id_mismatch");
    assert_eq!(creds(&e, &id), Some(before), "nada muda");
    // Sem client_id no retorno, vale o salvo.
    let q = start(&e, json!({"connection_id": id, "name": "Meu plano"})).await;
    let reply = login_reply(&e.mock, &q["nonce"], e.clock.secs() + 3600);
    e.mock.reply(move |_| (200, reply.clone()));
    let loc = callback(&e, &[("code", "c3"), ("state", &q["state"])]).await;
    assert_eq!(loc, format!("/?chatgpt=connected&connection={id}"));
    assert_eq!(e.mock.last_token_form()["client_id"], issued_client());
    let cfg = e.state.config().read().await.clone();
    assert_eq!(cfg.connections.len(), 1, "atualiza a mesma conexão");
    assert_eq!(cfg.connections[0].name, "Meu plano");
}

#[tokio::test]
async fn wrong_or_expired_state_and_access_denied_are_refused() {
    let e = env("state").await;
    let q = start(&e, json!({})).await;
    let client = issued_client();
    assert_eq!(callback(&e, &[("code", "c"), ("state", "outro"), ("client_id", &client)]).await, "/?chatgpt=error&reason=state_invalid");
    assert_eq!(callback(&e, &[("code", "c")]).await, "/?chatgpt=error&reason=state_invalid");
    // Vencida (10 minutos).
    e.clock.advance_secs(10 * 60);
    assert_eq!(callback(&e, &[("code", "c"), ("state", &q["state"]), ("client_id", &client)]).await, "/?chatgpt=error&reason=state_invalid");
    // Recusa na tela da OpenAI: a tentativa também é consumida.
    let q = start(&e, json!({})).await;
    assert_eq!(callback(&e, &[("error", "access_denied"), ("state", &q["state"])]).await, "/?chatgpt=error&reason=access_denied");
    assert_eq!(callback(&e, &[("code", "c"), ("state", &q["state"]), ("client_id", &client)]).await, "/?chatgpt=error&reason=state_invalid");
    let q = start(&e, json!({})).await;
    assert_eq!(callback(&e, &[("error", "server_error"), ("state", &q["state"])]).await, "/?chatgpt=error&reason=authorize_failed");
    let q = start(&e, json!({})).await;
    assert_eq!(callback(&e, &[("code", "c"), ("state", &q["state"])]).await, "/?chatgpt=error&reason=client_id_missing");
    assert_eq!(e.mock.token_hits(), 0, "nada foi trocado");
    assert!(e.state.config().read().await.connections.is_empty());
}

#[tokio::test]
async fn id_token_problems_and_missing_scope_are_refused() {
    let e = env("idtoken").await;
    let now = e.clock.secs();
    let client = issued_client();
    type Mint = Box<dyn Fn(&Mock, &str) -> String>;
    let cases: Vec<(&str, Mint)> = vec![
        ("assinatura de outra chave", Box::new(move |m, n| m.b.sign_with_header(json!({"alg": "RS256", "kid": "kid-a"}), &claims(n, &issued_client(), now + 600)))),
        ("aud errado", Box::new(move |m, n| m.a.sign(&claims(n, "outro-cliente", now + 600)))),
        ("iss errado", Box::new(move |m, n| {
            let mut c = claims(n, &issued_client(), now + 600);
            c["iss"] = json!("https://auth.example.test");
            m.a.sign(&c)
        })),
        ("nonce errado", Box::new(move |m, _| m.a.sign(&claims("outro-nonce", &issued_client(), now + 600)))),
        ("vencido", Box::new(move |m, n| m.a.sign(&claims(n, &issued_client(), now - 10)))),
        ("alg none", Box::new(move |m, n| {
            let signed = m.a.sign(&claims(n, &issued_client(), now + 600));
            let rest = signed.split_once('.').map(|(_, r)| r).unwrap_or("");
            format!("{}.{rest}", jwt::b64url_encode(br#"{"alg":"none","kid":"kid-a"}"#))
        })),
        ("assinatura adulterada", Box::new(move |m, n| {
            let signed = m.a.sign(&claims(n, &issued_client(), now + 600));
            let (input, _) = signed.rsplit_once('.').unwrap_or(("", ""));
            format!("{input}.{}", jwt::b64url_encode(&[7u8; 256]))
        })),
    ];
    for (what, mint) in cases {
        let q = start(&e, json!({})).await;
        let id_token = mint(&e.mock, &q["nonce"]);
        e.mock.reply(move |_| (200, json!({"access_token": "a", "refresh_token": "r", "id_token": id_token, "expires_in": 3600, "scope": SCOPES})));
        let loc = callback(&e, &[("code", "c"), ("state", &q["state"]), ("client_id", &client)]).await;
        assert_eq!(loc, "/?chatgpt=error&reason=id_token_invalid", "{what}");
    }
    // Escopo sem chatgpt.tokens.use.direct.
    let q = start(&e, json!({})).await;
    let mut reply = login_reply(&e.mock, &q["nonce"], now + 3600);
    reply["scope"] = json!("openid profile email offline_access");
    e.mock.reply(move |_| (200, reply.clone()));
    let loc = callback(&e, &[("code", "c"), ("state", &q["state"]), ("client_id", &client)]).await;
    assert_eq!(loc, "/?chatgpt=error&reason=scope_missing");
    // Troca recusada.
    let q = start(&e, json!({})).await;
    e.mock.reply(|_| (400, json!({"error": "invalid_grant"})));
    let loc = callback(&e, &[("code", "c"), ("state", &q["state"]), ("client_id", &client)]).await;
    assert_eq!(loc, "/?chatgpt=error&reason=token_exchange_failed");
    assert!(e.state.config().read().await.connections.is_empty(), "nenhuma conexão criada");
    let names: Vec<String> = std::fs::read_dir(e.dir.data())
        .map(|d| d.flatten().map(|f| f.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    assert_eq!(names, vec![store::HOST_ID_FILE.to_string()], "nenhuma credencial gravada");
}

#[tokio::test]
async fn unknown_kid_fetches_the_jwks_again() {
    let e = env("kid").await;
    let id = login(&e).await;
    assert_eq!(e.mock.jwks_hits.load(Ordering::SeqCst), 1);
    // A OpenAI troca a chave: o token novo vem com o kid B, que o cache ainda não conhece.
    e.mock.published.lock().expect("lock").push("kid-b");
    let q = start(&e, json!({"connection_id": id})).await;
    let mut reply = login_reply(&e.mock, &q["nonce"], e.clock.secs() + 3600);
    reply["id_token"] = json!(e.mock.b.sign(&claims(&q["nonce"], &issued_client(), e.clock.secs() + 3600)));
    e.mock.reply(move |_| (200, reply.clone()));
    let loc = callback(&e, &[("code", "c"), ("state", &q["state"])]).await;
    assert_eq!(loc, format!("/?chatgpt=connected&connection={id}"));
    assert_eq!(e.mock.jwks_hits.load(Ordering::SeqCst), 2, "uma nova busca");
    // Com o kid A conhecido, o cache vale.
    let q = start(&e, json!({"connection_id": id})).await;
    let reply = login_reply(&e.mock, &q["nonce"], e.clock.secs() + 3600);
    e.mock.reply(move |_| (200, reply.clone()));
    callback(&e, &[("code", "c"), ("state", &q["state"])]).await;
    assert_eq!(e.mock.jwks_hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn refresh_rotates_once_for_concurrent_callers() {
    let e = env("refresh").await;
    let id = login(&e).await;
    let auth = Arc::clone(e.state.chatgpt());
    let dir = e.dir.data();
    let hits = e.mock.token_hits();
    // Longe do vencimento: usa o atual, sem chamar o servidor.
    assert_eq!(auth.access_token(&dir, &id).await, Ok(fake("acesso", 1)));
    assert_eq!(e.mock.token_hits(), hits);
    // A menos de 5 minutos: renova, uma vez só para cinco chamadas ao mesmo tempo.
    e.clock.advance_secs(3600 - 4 * 60);
    *e.mock.token_delay.lock().expect("lock") = Duration::from_millis(150);
    e.mock.reply(|_| (200, json!({"access_token": fake("acesso", 2), "refresh_token": fake("renova", 2), "expires_in": 3600})));
    let calls: Vec<_> = (0..5)
        .map(|_| {
            let auth = Arc::clone(&auth);
            let dir = dir.clone();
            let id = id.clone();
            tokio::spawn(async move { auth.access_token(&dir, &id).await })
        })
        .collect();
    for call in calls {
        assert_eq!(call.await.expect("tarefa"), Ok(fake("acesso", 2)));
    }
    assert_eq!(e.mock.token_hits(), hits + 1, "renovações serializadas");
    let form = e.mock.last_token_form();
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], fake("renova", 1));
    assert_eq!(form["client_id"], issued_client());
    assert_eq!(form["resource"], RESOURCE);
    let c = creds(&e, &id).expect("credenciais");
    assert_eq!((c.access_token.as_str(), c.refresh_token.as_str()), (fake("acesso", 2).as_str(), fake("renova", 2).as_str()));
    assert_eq!(c.expires_at, e.clock.secs() + 3600);
    assert_eq!(mode(&store::credentials_path(&dir, &id).expect("caminho")), 0o600);
    // Falha passageira (503): tenta depois, sem mudar nada.
    e.clock.advance_secs(3600);
    *e.mock.token_delay.lock().expect("lock") = Duration::ZERO;
    e.mock.reply(|_| (503, json!({})));
    assert_eq!(auth.access_token(&dir, &id).await, Err(TokenError::Unavailable));
    assert_eq!(creds(&e, &id), Some(c));
    assert!(matches!(auth.status(Some(&dir), &id), PlanStatus::Connected { .. }));
}

#[tokio::test]
async fn terminal_refresh_errors_ask_for_a_new_login_without_retrying() {
    for body in [json!({"error": "invalid_grant"}), json!({"error": {"code": "refresh_token_expired", "message": "x"}})] {
        let e = env("terminal").await;
        let id = login(&e).await;
        let auth = Arc::clone(e.state.chatgpt());
        let dir = e.dir.data();
        e.clock.advance_secs(3600);
        let reply = body.clone();
        e.mock.reply(move |_| (400, reply.clone()));
        let hits = e.mock.token_hits();
        assert_eq!(auth.access_token(&dir, &id).await, Err(TokenError::NeedsLogin));
        assert_eq!(auth.access_token(&dir, &id).await, Err(TokenError::NeedsLogin));
        assert_eq!(e.mock.token_hits(), hits + 1, "não repete");
        let c = creds(&e, &id).expect("credenciais");
        assert!(c.needs_login && c.access_token.is_empty() && c.refresh_token.is_empty());
        assert_eq!(c.client_id, issued_client(), "fica para a reautorização");
        assert_eq!(auth.status(Some(&dir), &id), PlanStatus::NeedsLogin { email: EMAIL.into() });
        // Reconectar: reautorização com o client salvo.
        let q = start(&e, json!({"connection_id": id})).await;
        assert_eq!(q["client_id"], issued_client());
    }
}

#[tokio::test]
async fn logout_revokes_and_deletes_and_tolerates_a_revoke_failure() {
    let e = env("logout").await;
    let id = login(&e).await;
    let resp = send(&e.state, req(Method::POST, &format!("/api/connections/{id}/chatgpt/logout"), LOOPBACK_PEER, None, Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(serde_json::from_str::<Value>(&body_text(resp).await).expect("json"), json!({"state": "disconnected", "revoked": true}));
    let form = e.mock.revoke_forms.lock().expect("lock").last().cloned().expect("revogação");
    assert_eq!(form["token"], fake("renova", 1));
    assert_eq!(form["token_type_hint"], "refresh_token");
    assert_eq!(form["client_id"], issued_client());
    assert_eq!(creds(&e, &id), None, "credenciais apagadas");
    assert!(e.state.config().read().await.connections.iter().any(|c| c.id == id), "a conexão fica, desconectada");
    assert_eq!(e.state.chatgpt().status(Some(&e.dir.data()), &id), PlanStatus::Disconnected);
    assert_eq!(e.state.chatgpt().access_token(&e.dir.data(), &id).await, Err(TokenError::NotConnected));

    // Revogação falhando: aviso, e as credenciais locais saem do mesmo jeito.
    let e = env("logout-falha").await;
    let id = login(&e).await;
    *e.mock.revoke_status.lock().expect("lock") = 500;
    let resp = send(&e.state, req(Method::POST, &format!("/api/connections/{id}/chatgpt/logout"), LOOPBACK_PEER, None, Some(json!({})))).await;
    assert_eq!(serde_json::from_str::<Value>(&body_text(resp).await).expect("json"), json!({"state": "disconnected", "revoked": false}));
    assert_eq!(creds(&e, &id), None);
}

#[tokio::test]
async fn plan_routes_refuse_key_connections_and_unknown_ids() {
    let e = env("rotas").await;
    {
        let mut cfg = e.state.config().write().await;
        let mut key = Connection::chatgpt_plan("chave", "Chave");
        key.kind = ConnectionKind::ApiKey;
        key.api_key = "env:QUALQUER".into();
        cfg.connections.push(key);
    }
    for (method, uri) in [(Method::GET, "/api/connections/chave/chatgpt"), (Method::POST, "/api/connections/chave/chatgpt/logout")] {
        let body = (method == Method::POST).then(|| json!({}));
        let resp = send(&e.state, req(method, uri, LOOPBACK_PEER, None, body)).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body_text(resp).await.contains("chatgpt_not_plan"));
    }
    let resp = send(&e.state, req(Method::GET, "/api/connections/nada/chatgpt", LOOPBACK_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = send(&e.state, req(Method::POST, START_PATH, LOOPBACK_PEER, None, Some(json!({"connection_id": "chave"})))).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    // Pelo PUT de conexões, o plano não nasce e a conexão salva só troca de nome.
    let put = |body: Value| req(Method::PUT, "/api/config/connections", LOOPBACK_PEER, None, Some(body));
    let resp = send(&e.state, put(json!({"id": "novo", "name": "x", "kind": "chatgpt_plan"}))).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_text(resp).await.contains("chatgpt_plan_needs_login"));
    let id = login(&e).await;
    let resp = send(&e.state, put(json!({"id": id, "name": "Plano da casa", "base_url": "https://outro.example/v1", "api_key": "env:X"}))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: Value = serde_json::from_str(&body_text(resp).await).expect("json");
    assert_eq!((v["kind"].clone(), v["api_key_masked"].clone()), (json!("chatgpt_plan"), json!("")));
    let conn = e.state.config().read().await.connections.iter().find(|c| c.id == id).cloned().expect("conexão");
    assert_eq!((conn.name.as_str(), conn.base_url.as_str(), conn.api_key.as_str()), ("Plano da casa", CHATGPT_PLAN_BASE_URL, ""));
    let resp = send(&e.state, put(json!({"id": id, "name": "y", "kind": "api_key", "base_url": "https://x.example/v1", "api_key": "env:X"}))).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_text(resp).await.contains("connection_kind_fixed"));
}

fn lan(s: AppState) -> AppState {
    s.with_host_policy(HostPolicy::Lan).with_network_run(NetworkRun { lan: true, source: ModeSource::Flag, requested_port: PORT })
}

#[tokio::test]
async fn network_mode_binds_the_attempt_to_this_computer_or_the_device() {
    let e = env_with("rede", lan).await;
    // Sem dispositivo, de fora do loopback: nem início nem retorno sem state válido.
    let resp = send(&e.state, req(Method::POST, START_PATH, NAT_PEER, None, Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let resp = send(&e.state, req(Method::GET, &callback_uri(&[("code", "c"), ("state", "x")]), NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), crate::server::devices::ENTRY_PATH, "segue a regra dos dispositivos");
    // Iniciado neste computador: o retorno de fora do loopback é recusado e NÃO consome o state
    // (um state vazado não cancela o login de quem iniciou).
    let q = start(&e, json!({})).await;
    let client = issued_client();
    let resp = send(&e.state, req(Method::GET, &callback_uri(&[("code", "c"), ("state", &q["state"]), ("client_id", &client)]), NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), "/?chatgpt=error&reason=state_invalid");
    assert_eq!(e.mock.token_hits(), 0);
    assert!(e.state.chatgpt().callback_pending(&q["state"]), "segue pendente");
    let reply = login_reply(&e.mock, &q["nonce"], e.clock.secs() + 3600);
    e.mock.reply(move |_| (200, reply.clone()));
    let loc = callback(&e, &[("code", "c"), ("state", &q["state"]), ("client_id", &client)]).await;
    assert!(loc.starts_with("/?chatgpt=connected&connection="), "quem iniciou termina: {loc}");
    // Iniciado por um aparelho autorizado: `local` falso; o retorno vale no loopback.
    let link = e.state.devices().issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    let (_, cookie) = e.state.devices().redeem_entry_link(&link).expect("troca").expect("dispositivo");
    let resp = send(&e.state, req(Method::POST, START_PATH, NAT_PEER, Some(&cookie), Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: Value = serde_json::from_str(&body_text(resp).await).expect("json");
    assert_eq!(v["local"], json!(false));
    let q = query_of(v["url"].as_str().expect("url"));
    assert_eq!(q["redirect_uri"], "http://127.0.0.1:3000/auth/callback");
    let reply = login_reply(&e.mock, &q["nonce"], e.clock.secs() + 3600);
    e.mock.reply(move |_| (200, reply.clone()));
    let loc = callback(&e, &[("code", "c"), ("state", &q["state"]), ("client_id", &client)]).await;
    assert!(loc.starts_with("/?chatgpt=connected&connection="), "{loc}");
}

#[test]
fn claims_check_leeway_audience_list_and_subject() {
    let key = TestKey::a();
    let now = 1_800_000_000;
    let check = |c: &Value| {
        let parsed = jwt::parse(&key.sign(c)).expect("token");
        jwt::verify_signature(&parsed, &jwt::parse_jwks(json!({"keys": [key.jwk()]}).to_string().as_bytes())[0])
            .and_then(|()| jwt::check_claims(&parsed, ISSUER, "cli", Some("n"), now))
    };
    assert!(check(&claims("n", "cli", now - 3)).is_ok(), "dentro da tolerância de 5 s");
    assert_eq!(check(&claims("n", "cli", now - 5)).err(), Some(jwt::IdTokenError::Expired));
    let mut list = claims("n", "cli", now + 60);
    list["aud"] = json!(["outro", "cli"]);
    assert_eq!(check(&list).err(), Some(jwt::IdTokenError::Audience), "várias audiências sem azp");
    list["azp"] = json!("outro");
    assert_eq!(check(&list).err(), Some(jwt::IdTokenError::Audience), "azp de outro cliente");
    list["azp"] = json!("cli");
    assert!(check(&list).is_ok(), "azp igual ao client_id");
    list["aud"] = json!(["cli"]);
    list.as_object_mut().map(|o| o.remove("azp"));
    assert!(check(&list).is_ok(), "uma audiência dispensa azp");
    list["aud"] = json!([]);
    assert_eq!(check(&list).err(), Some(jwt::IdTokenError::Audience));
    let mut no_sub = claims("n", "cli", now + 60);
    no_sub["sub"] = json!("");
    assert_eq!(check(&no_sub).err(), Some(jwt::IdTokenError::Subject));
    let hs = key.sign_with_header(json!({"alg": "HS256", "kid": "kid-a"}), &claims("n", "cli", now + 60));
    assert_eq!(jwt::parse(&hs).err().map(|e| e.code()), Some("algorithm"));
    assert_eq!(jwt::parse("a.b").err().map(|e| e.code()), Some("malformed"));
    // JWKS: só RSA de assinatura; o resto fica de fora.
    let doc = json!({"keys": [{"kty": "EC", "kid": "x"}, {"kty": "RSA", "use": "enc", "n": "AQAB", "e": "AQAB"}, key.jwk()]});
    assert_eq!(jwt::parse_jwks(doc.to_string().as_bytes()).len(), 1);
}

#[test]
fn files_are_private_and_the_host_id_is_reused() {
    let dir = TempDir::new("arquivos");
    let data = dir.data();
    let first = store::load_or_create_host_id(&data).expect("cria");
    assert_eq!(store::load_or_create_host_id(&data).expect("lê"), first);
    assert_eq!(mode(&data.join(store::HOST_ID_FILE)), 0o600);
    assert_eq!(mode(&data) & 0o077, 0, "pasta só do dono");
    // Arquivo inválido: troca por um novo.
    std::fs::write(data.join(store::HOST_ID_FILE), "{}").expect("grava");
    let fresh = store::load_or_create_host_id(&data).expect("recria");
    assert!(store::valid_host_id(&fresh) && fresh != first);
    // Link simbólico no lugar das credenciais: recusado.
    let target = dir.path().join("alvo.json");
    std::fs::write(&target, "{}").expect("alvo");
    std::os::unix::fs::symlink(&target, data.join("chatgpt-conn-link.json")).expect("link");
    assert!(store::load_credentials(&data, "link").is_err());
    assert!(store::credentials_path(&data, "../fora").is_none());
    // Sobra nenhuma de temporário.
    let leftovers = std::fs::read_dir(&data).expect("pasta").flatten().filter(|f| f.file_name().to_string_lossy().ends_with(".tmp")).count();
    assert_eq!(leftovers, 0);
}

/// "Nada sensível no log": o código de produção só escreve no terminal por `terminal_notice`, e
/// só com o id da conexão ou um motivo fixo; os `Debug` não mostram segredos.
#[test]
fn nothing_sensitive_goes_to_the_log() {
    // `mod.rs` não tem testes embutidos: o arquivo todo é produção.
    let production = include_str!("mod.rs");
    for macro_name in ["dbg!", "print!(", "log::", "tracing::"] {
        assert!(!production.contains(macro_name), "{macro_name}");
    }
    assert_eq!(production.matches("println!").count(), 1, "só o eprintln! de terminal_notice");
    assert_eq!(production.matches("eprintln!").count(), 1, "só dentro de terminal_notice");
    for call in production.split("terminal_notice(\"").skip(1) {
        let args = call.split(");").next().unwrap_or("");
        for name in args.split("(\"").skip(1).map(|p| p.split('"').next().unwrap_or("")) {
            assert!(matches!(name, "id" | "reason"), "parâmetro de log inesperado: {name}");
        }
    }
    let jwt_source = include_str!("jwt.rs");
    let store_source = include_str!("store.rs");
    for s in [jwt_source, store_source] {
        assert!(!s.contains("eprintln!") && !s.contains("println!"));
    }
    assert_eq!(format!("{:?}", ChatgptAuth::default()), "ChatgptAuth");
}

#[test]
fn old_config_still_loads_and_the_plan_connection_is_normalized() {
    let dir = TempDir::new("config");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[[connections]]\nid = \"velha\"\nname = \"n\"\nbase_url = \"https://x.example/v1\"\napi_key = \"env:K\"\n\n\
         [[connections]]\nid = \"plano\"\nname = \"p\"\nbase_url = \"https://editado.example\"\napi_key = \"sobra\"\nwire_api = \"chat\"\nkind = \"chatgpt_plan\"\n",
    )
    .expect("grava");
    let cfg = Config::load_from(&path).expect("carrega");
    assert_eq!(cfg.connections[0].kind, ConnectionKind::ApiKey);
    assert_eq!(cfg.connections[0].api_key, "env:K");
    let plan = &cfg.connections[1];
    assert_eq!((plan.kind, plan.base_url.as_str(), plan.wire_api, plan.api_key.as_str()), (ConnectionKind::ChatgptPlan, CHATGPT_PLAN_BASE_URL, WireApi::Responses, ""));
    let text = toml::to_string(&cfg).expect("toml");
    assert_eq!(text.matches("kind =").count(), 1, "a por chave não grava o tipo: {text}");
}

#[tokio::test]
async fn refresh_with_another_account_asks_for_a_new_login() {
    let e = env("outra-conta").await;
    let id = login(&e).await;
    let auth = Arc::clone(e.state.chatgpt());
    let dir = e.dir.data();
    e.clock.advance_secs(3600);
    let mut other = claims("", &issued_client(), e.clock.secs() + 3600);
    other["sub"] = json!("outra-pessoa");
    other.as_object_mut().map(|o| o.remove("nonce"));
    let id_token = e.mock.a.sign(&other);
    e.mock.reply(move |_| (200, json!({"access_token": fake("acesso", 9), "refresh_token": fake("renova", 9), "id_token": id_token, "expires_in": 3600})));
    assert_eq!(auth.access_token(&dir, &id).await, Err(TokenError::NeedsLogin));
    let c = creds(&e, &id).expect("credenciais");
    assert!(c.needs_login && c.access_token.is_empty() && c.refresh_token.is_empty());
    assert_eq!(c.sub, SUB, "nada da outra conta foi gravado");
    // Mesma conta: o ID token novo é aceito.
    let e = env("mesma-conta").await;
    let id = login(&e).await;
    e.clock.advance_secs(3600);
    let mut same = claims("", &issued_client(), e.clock.secs() + 3600);
    same.as_object_mut().map(|o| o.remove("nonce"));
    let id_token = e.mock.a.sign(&same);
    let expected = id_token.clone();
    e.mock.reply(move |_| (200, json!({"access_token": fake("acesso", 9), "refresh_token": fake("renova", 9), "id_token": id_token, "expires_in": 3600})));
    assert_eq!(e.state.chatgpt().access_token(&e.dir.data(), &id).await, Ok(fake("acesso", 9)));
    assert_eq!(creds(&e, &id).expect("credenciais").id_token, expected);
}

#[tokio::test]
async fn a_connection_named_host_never_touches_the_installation_id() {
    let e = env("host").await;
    e.state.config().write().await.connections.push(Connection::chatgpt_plan("host", "Host"));
    let q = start(&e, json!({"connection_id": "host"})).await;
    let host_file = e.dir.data().join(store::HOST_ID_FILE);
    let before = std::fs::read_to_string(&host_file).expect("identificador");
    let reply = login_reply(&e.mock, &q["nonce"], e.clock.secs() + 3600);
    e.mock.reply(move |_| (200, reply.clone()));
    let client = issued_client();
    let loc = callback(&e, &[("code", "c"), ("state", &q["state"]), ("client_id", &client)]).await;
    assert_eq!(loc, "/?chatgpt=connected&connection=host");
    let creds_path = store::credentials_path(&e.dir.data(), "host").expect("caminho");
    assert_ne!(creds_path, host_file);
    assert_eq!(std::fs::read_to_string(&host_file).expect("identificador"), before, "login não sobrescreve");
    let resp = send(&e.state, req(Method::POST, "/api/connections/host/chatgpt/logout", LOOPBACK_PEER, None, Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!creds_path.exists());
    assert_eq!(std::fs::read_to_string(&host_file).expect("identificador"), before, "logout não apaga");
    assert_eq!(start(&e, json!({})).await["ext_agent_host_id"], q["ext_agent_host_id"]);
}
