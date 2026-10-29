//! Testes dos dispositivos autorizados (spec 112, T01). Incluem o caso NAT: todas as requisições
//! com o mesmo IP de origem fora do loopback (o do Windows). Nada chama o Docker; pastas
//! temporárias com guarda `Drop`; relógio manual.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::server::lan::{ModeSource, NetworkRun};
use crate::server::security::HostPolicy;
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";
/// O IP do Windows no WSL NAT: todas as conexões externas chegam com ele.
const NAT_PEER: &str = "172.21.144.1:50000";
const LOOPBACK_PEER: &str = "127.0.0.1:50001";
const T0: u64 = 1_800_000_000_000;

/// Pasta temporária própria, apagada no fim (passe ou falhe).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("nants-devices-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("pasta temporária");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Relógio manual em milissegundos.
#[derive(Clone)]
struct Clock(Arc<AtomicU64>);

impl Clock {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(T0)))
    }

    fn handle(&self) -> DeviceClock {
        let c = Arc::clone(&self.0);
        Arc::new(move || c.load(Ordering::SeqCst))
    }

    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

fn lan_state(dir: &TempDir, clock: &Clock) -> AppState {
    AppState::new(dir.path().join("config.toml"), Config::default(), 3000)
        .with_data_dir(dir.path().join("data"))
        .with_device_clock(clock.handle())
        .with_host_policy(HostPolicy::Lan)
        .with_network_run(NetworkRun { lan: true, source: ModeSource::Flag, requested_port: 3000 })
}

fn store_in(dir: &TempDir, clock: &Clock) -> DeviceStore {
    DeviceStore::with_clock(Some(dir.path().join("data").join(DEVICES_FILE)), clock.handle())
}

fn req(method: Method, uri: &str, peer: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri).header(header::HOST, HOST);
    if let Some(token) = cookie {
        b = b.header(header::COOKIE, format!("outro=1; {COOKIE_NAME}={token}"));
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

async fn json_body(resp: Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("corpo");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn set_cookie(resp: &Response<Body>) -> Option<String> {
    resp.headers().get(header::SET_COOKIE).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn token_of(cookie: &str) -> String {
    cookie
        .split(';')
        .next()
        .and_then(|kv| kv.split_once('='))
        .map(|(_, v)| v.to_string())
        .expect("token no cookie")
}

fn location(resp: &Response<Body>) -> Option<&str> {
    resp.headers().get(header::LOCATION).and_then(|v| v.to_str().ok())
}

/// Autoriza um aparelho pelo link de entrada e devolve o token do cookie.
async fn authorize_by_link(state: &AppState) -> String {
    let t = state.devices().issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    let resp = send(state, req(Method::GET, &format!("/pair?t={t}"), NAT_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    token_of(&set_cookie(&resp).expect("cookie"))
}

// --- Unidade -----------------------------------------------------------------------------------

#[test]
fn constant_time_compare_and_hash() {
    assert!(ct_eq(b"abc", b"abc"));
    assert!(!ct_eq(b"abc", b"abd"));
    assert!(!ct_eq(b"abc", b"abcd"));
    assert_eq!(hash_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
}

#[test]
fn open_routes_are_only_the_entry_and_its_assets() {
    let get = Method::GET;
    for open in ["/connect", "/pair", "/assets/index-abc.js", "/assets/x.css", "/brand/logo.svg", "/favicon.ico"] {
        assert!(is_open(&get, open), "{open}");
    }
    assert!(is_open(&Method::HEAD, "/assets/x.woff2"));
    assert!(is_open(&Method::POST, "/api/devices/pair"));
    for closed in [
        "/", "/api/config", "/api/events", "/api/devices", "/api/devices/code", "/api/devices/pair", "/projects/x",
        "/assets/", "/assets/../api/config", "/assets/%2e%2e/x", "/brand/..\\x", "/connect/x",
    ] {
        assert!(!is_open(&get, closed), "{closed}");
    }
    assert!(!is_open(&Method::POST, "/api/devices/code"));
    assert!(!is_open(&Method::POST, "/connect"));
    assert!(!is_open(&Method::DELETE, "/api/devices/pair"));
}

#[test]
fn suggested_names_follow_the_browser_and_the_system() {
    use crate::i18n::Lang::{En, PtBr};
    let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
    let win_chrome = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
    let win_edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36 Edg/120.0";
    let android_ff = "Mozilla/5.0 (Android 14; Mobile; rv:120.0) Gecko/120.0 Firefox/120.0";
    let mac_safari = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
    assert_eq!(suggest_name(Some(iphone), PtBr), "Safari no iPhone");
    assert_eq!(suggest_name(Some(iphone), En), "Safari on iPhone");
    assert_eq!(suggest_name(Some(win_chrome), PtBr), "Chrome no Windows");
    assert_eq!(suggest_name(Some(win_edge), En), "Edge on Windows");
    assert_eq!(suggest_name(Some(android_ff), PtBr), "Firefox no Android");
    assert_eq!(suggest_name(Some(mac_safari), PtBr), "Safari no Mac");
    assert_eq!(suggest_name(Some("curl/8.0"), PtBr), "Dispositivo");
    assert_eq!(suggest_name(None, En), "Device");
    assert_eq!(suggest_name(Some("Something (Linux)"), PtBr), "Navegador no Linux");
}

#[test]
fn names_are_short_and_without_control_characters() {
    assert_eq!(sanitize_name("  Meu\n celular\t\u{202E}x "), Some("Meu celular x".into()));
    assert_eq!(sanitize_name("\u{0007}\u{200B}  "), None);
    assert_eq!(sanitize_name(&"a".repeat(100)).map(|n| n.chars().count()), Some(MAX_NAME_CHARS));
    assert_eq!(sanitize_name("Tablet da sala"), Some("Tablet da sala".into()));
}

#[test]
fn file_keeps_only_the_hash_with_0600_and_survives_a_restart() {
    let dir = TempDir::new("file");
    let clock = Clock::new();
    let store = store_in(&dir, &clock);
    let t = store.issue_entry_link(crate::i18n::Lang::En).expect("link");
    let (device, token) = store.redeem_entry_link(&t).expect("grava").expect("válido");
    assert_eq!(device.name, "This computer");
    assert!(device.this_computer);
    assert_eq!(token.len(), 64);
    let path = dir.path().join("data").join(DEVICES_FILE);
    let raw = std::fs::read_to_string(&path).expect("arquivo");
    assert!(!raw.contains(&token), "o token nunca é gravado");
    assert!(raw.contains(&hash_hex(&token)), "só o hash");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("data"))
        .expect("pasta")
        .filter_map(Result::ok)
        .filter(|e| e.file_name() != DEVICES_FILE)
        .collect();
    assert!(leftovers.is_empty(), "escrita atômica sem sobras");
    // Reinício: outra instância lê o arquivo.
    let again = store_in(&dir, &clock);
    assert_eq!(again.authenticate(&token), Some(device.id.clone()));
    assert_eq!(again.authenticate(&"0".repeat(64)), None);
    assert_eq!(again.authenticate("curto"), None);
    assert!(!format!("{again:?}").contains(&device.token_hash));
}

#[test]
fn last_seen_is_written_at_most_once_a_minute() {
    let dir = TempDir::new("seen");
    let clock = Clock::new();
    let store = store_in(&dir, &clock);
    let t = store.issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    let (_, token) = store.redeem_entry_link(&t).expect("grava").expect("válido");
    let path = dir.path().join("data").join(DEVICES_FILE);
    let seen = || -> u64 {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("arquivo")).expect("json");
        v["devices"][0]["last_seen"].as_u64().expect("last_seen")
    };
    assert_eq!(seen(), T0 / 1000);
    clock.advance(30_000);
    store.authenticate(&token).expect("válido");
    assert_eq!(seen(), T0 / 1000, "antes de um minuto não grava");
    clock.advance(31_000);
    store.authenticate(&token).expect("válido");
    assert_eq!(seen(), T0 / 1000 + 61);
}

#[test]
fn entry_link_is_single_use_and_expires_in_ten_minutes() {
    let dir = TempDir::new("link");
    let clock = Clock::new();
    let store = store_in(&dir, &clock);
    let t = store.issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    assert!(store.redeem_entry_link("errado").expect("ok").is_none());
    assert!(store.redeem_entry_link(&t).expect("ok").is_some(), "o errado não consome");
    assert!(store.redeem_entry_link(&t).expect("ok").is_none(), "uso único");
    let t = store.issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    clock.advance(ENTRY_LINK_TTL_MS);
    assert!(store.redeem_entry_link(&t).expect("ok").is_none(), "expirado");
}

/// Spec 117: só o link de entrada do terminal marca "Este computador"; o código não marca, renomear
/// não tira a marcação, revogar apaga junto e um registro antigo sem o campo conta como falso
/// (mesmo com o nome "Este computador").
#[test]
fn only_the_entry_link_marks_this_computer() {
    let dir = TempDir::new("this-computer");
    let clock = Clock::new();
    let store = store_in(&dir, &clock);
    let t = store.issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    let (computer, _) = store.redeem_entry_link(&t).expect("grava").expect("válido");
    let CodeState::Open { code, .. } = store.open_window().expect("abre") else { panic!("aberta") };
    clock.advance(PAIR_MIN_INTERVAL_MS);
    let (phone, _) = store.pair(&code, "Este computador".into()).expect("pareia");
    assert!(!phone.this_computer, "o código nunca marca, nem com o mesmo nome");
    assert!(store.is_this_computer(&computer.id));
    assert!(!store.is_this_computer(&phone.id));
    assert!(!store.is_this_computer("ffffffffffffffff"));
    store.rename(&computer.id, "Notebook".into()).expect("renomeia");
    let raw = std::fs::read_to_string(dir.path().join("data").join(DEVICES_FILE)).expect("arquivo");
    let file: Value = serde_json::from_str(&raw).expect("json");
    let flags: Vec<_> = file["devices"].as_array().expect("lista").iter().map(|d| d["this_computer"].clone()).collect();
    assert_eq!(flags, vec![json!(true), json!(false)], "gravado no devices.json");
    let reopened = store_in(&dir, &clock);
    assert!(reopened.is_this_computer(&computer.id), "sobrevive ao reinício e à troca de nome");
    reopened.revoke(&computer.id).expect("revoga");
    assert!(!reopened.is_this_computer(&computer.id), "revogar apaga a marcação junto");
    // Registro antigo, sem o campo: conta como falso.
    let old = json!({"version": 1, "devices": [{"id": "0123456789abcdef", "name": "Este computador", "token_hash": "a".repeat(64), "created_at": 1, "last_seen": 1}]});
    std::fs::write(dir.path().join("data").join(DEVICES_FILE), old.to_string()).expect("grava");
    let legacy = store_in(&dir, &clock);
    assert_eq!(legacy.list().len(), 1);
    assert!(!legacy.is_this_computer("0123456789abcdef"));
}

#[test]
fn code_rotates_every_minute_and_the_window_lasts_ten_minutes() {
    let dir = TempDir::new("code");
    let clock = Clock::new();
    let store = store_in(&dir, &clock);
    assert_eq!(store.code_state(), CodeState::Closed);
    let CodeState::Open { code: first, seconds_to_next, expires_in } = store.open_window().expect("abre") else {
        panic!("janela aberta");
    };
    assert_eq!((first.len(), seconds_to_next, expires_in), (6, 60, 600));
    assert!(first.bytes().all(|b| b.is_ascii_digit()));
    clock.advance(59_000);
    let CodeState::Open { code, seconds_to_next, .. } = store.code_state() else { panic!("aberta") };
    assert_eq!((code.as_str(), seconds_to_next), (first.as_str(), 1));
    clock.advance(1_000);
    let CodeState::Open { code: second, seconds_to_next, expires_in } = store.code_state() else { panic!("aberta") };
    assert_eq!((seconds_to_next, expires_in), (60, 540));
    if second != first {
        clock.advance(PAIR_MIN_INTERVAL_MS);
        assert_eq!(store.pair(&first, "x".into()).err(), Some(PairError::Invalid { attempts_left: 4 }), "o código anterior não vale");
    }
    clock.advance(CODE_WINDOW_MS);
    assert_eq!(store.code_state(), CodeState::Closed, "a janela venceu");
    clock.advance(PAIR_MIN_INTERVAL_MS);
    assert_eq!(store.pair(&second, "x".into()).err(), Some(PairError::Inactive));
    // Fechar a janela invalida o código.
    let CodeState::Open { code, .. } = store.open_window().expect("abre") else { panic!("aberta") };
    store.close_window();
    clock.advance(PAIR_MIN_INTERVAL_MS);
    assert_eq!(store.pair(&code, "x".into()).err(), Some(PairError::Inactive));
}

#[test]
fn five_errors_lock_the_window_and_attempts_are_limited_per_second() {
    let dir = TempDir::new("brute");
    let clock = Clock::new();
    let store = store_in(&dir, &clock);
    let CodeState::Open { code, .. } = store.open_window().expect("abre") else { panic!("aberta") };
    let wrong = if code == "000000" { "000001" } else { "000000" };
    assert!(matches!(store.pair(wrong, "x".into()), Err(PairError::Invalid { attempts_left: 4 })));
    // Segunda tentativa no mesmo segundo: recusada sem contar erro.
    assert!(matches!(store.pair(&code, "x".into()), Err(PairError::RateLimited { .. })));
    for left in (0..4).rev() {
        clock.advance(PAIR_MIN_INTERVAL_MS);
        assert_eq!(store.pair(wrong, "x".into()).err(), Some(PairError::Invalid { attempts_left: left }));
    }
    assert_eq!(store.code_state(), CodeState::Locked);
    clock.advance(PAIR_MIN_INTERVAL_MS);
    assert_eq!(store.pair(&code, "x".into()).err(), Some(PairError::Locked), "nem o certo passa");
    // Janela nova destrava.
    let CodeState::Open { code, .. } = store.open_window().expect("abre") else { panic!("aberta") };
    clock.advance(PAIR_MIN_INTERVAL_MS);
    let (device, token) = store.pair(&code, "Celular".into()).expect("acertou");
    assert_eq!(device.name, "Celular");
    assert_eq!(store.authenticate(&token), Some(device.id));
    clock.advance(PAIR_MIN_INTERVAL_MS);
    assert_eq!(store.pair(&code, "x".into()).err(), Some(PairError::Inactive), "uso único");
}

// --- Integração do roteador ----------------------------------------------------------------------

#[tokio::test]
async fn nat_requests_without_device_are_refused_and_pages_go_to_the_entry() {
    let dir = TempDir::new("nat");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    for uri in ["/api/config", "/api/events", "/api/devices", "/api/projects/x/state", "/api/projects/x/file?path=a"] {
        let resp = send(&state, req(Method::GET, uri, NAT_PEER, None, None)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(json_body(resp).await["code"], CODE_DEVICE_REQUIRED, "{uri}");
    }
    let resp = send(&state, req(Method::PUT, "/api/config/network", NAT_PEER, None, Some(json!({"lan": false})))).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // As páginas voltam pela entrada com o caminho de origem (spec 128); a raiz não precisa.
    for (page, to) in [("/", ENTRY_PATH), ("/projects/abc", "/connect?next=/projects/abc"), ("/settings", "/connect?next=/settings")] {
        let resp = send(&state, req(Method::GET, page, NAT_PEER, None, None)).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER, "{page}");
        assert_eq!(location(&resp), Some(to));
    }
    // A página de entrada e os recursos dela abrem (o que não existe dá 404, não 401).
    let resp = send(&state, req(Method::GET, ENTRY_PATH, NAT_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(&state, req(Method::GET, "/assets/nao-existe.js", NAT_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // A troca do código abre (sem janela: código inativo, não 401).
    let resp = send(&state, req(Method::POST, PAIR_API_PATH, NAT_PEER, None, Some(json!({"code": "123456"})))).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(resp).await["code"], CODE_CODE_INACTIVE);
    // Cookie inventado não passa.
    let fake = "a".repeat(64);
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&fake), None)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Sem a informação da conexão também não passa (nunca é tratado como loopback).
    let mut r = req(Method::GET, "/api/config", NAT_PEER, None, None);
    r.extensions_mut().remove::<ConnectInfo<SocketAddr>>();
    assert_eq!(send(&state, r).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn real_loopback_needs_no_device_and_outside_network_mode_nothing_changes() {
    let dir = TempDir::new("loop");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    for peer in [LOOPBACK_PEER, "127.8.9.10:1", "[::1]:2"] {
        let resp = send(&state, req(Method::GET, "/api/config", peer, None, None)).await;
        assert_eq!(resp.status(), StatusCode::OK, "{peer}");
    }
    let resp = send(&state, req(Method::GET, "/api/devices", LOOPBACK_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await, json!({"devices": [], "current": null}));
    // Fora do modo rede: nem o caso NAT pede dispositivo, e /pair volta para a página de hoje.
    let local = AppState::new(dir.path().join("config.toml"), Config::default(), 3000).with_data_dir(dir.path().join("local"));
    let resp = send(&local, req(Method::GET, "/api/config", NAT_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(&local, req(Method::GET, "/pair?t=x", NAT_PEER, None, None)).await;
    assert_eq!((resp.status(), location(&resp)), (StatusCode::SEE_OTHER, Some("/")));
    assert!(set_cookie(&resp).is_none());
}

#[tokio::test]
async fn entry_link_authorizes_this_computer_with_a_lax_cookie() {
    let dir = TempDir::new("pairlink");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    let t = state.devices().issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    let resp = send(&state, req(Method::HEAD, &format!("/pair?t={t}"), NAT_PEER, None, None)).await;
    assert!(set_cookie(&resp).is_none(), "HEAD não consome o link");
    let resp = send(&state, req(Method::GET, "/pair?t=errado", NAT_PEER, None, None)).await;
    assert_eq!((resp.status(), location(&resp)), (StatusCode::SEE_OTHER, Some("/connect?reason=link_invalid")));
    assert!(set_cookie(&resp).is_none());
    let resp = send(&state, req(Method::GET, &format!("/pair?t={t}"), NAT_PEER, None, None)).await;
    assert_eq!((resp.status(), location(&resp)), (StatusCode::SEE_OTHER, Some("/")));
    let cookie = set_cookie(&resp).expect("cookie");
    for attr in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=31536000"] {
        assert!(cookie.contains(attr), "{cookie}");
    }
    assert!(!cookie.contains("Strict"), "{cookie}");
    assert!(!cookie.contains("Secure"), "HTTP local: sem Secure");
    let token = token_of(&cookie);
    // Uso único.
    let resp = send(&state, req(Method::GET, &format!("/pair?t={t}"), NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), Some("/connect?reason=link_invalid"));
    // O cookie abre tudo, do mesmo IP de origem.
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&token), None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = send(&state, req(Method::GET, "/api/devices", NAT_PEER, Some(&token), None)).await;
    let body = json_body(resp).await;
    assert_eq!(body["devices"][0]["name"], "Este computador");
    assert_eq!(body["devices"][0]["this_computer"], true);
    assert_eq!(body["devices"][0]["current"], true);
    assert!(!body.to_string().contains("token"), "{body}");
    // Expirado depois de 10 minutos.
    let t = state.devices().issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    clock.advance(ENTRY_LINK_TTL_MS + 1);
    let resp = send(&state, req(Method::GET, &format!("/pair?t={t}"), NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), Some("/connect?reason=link_invalid"));
}

#[tokio::test]
async fn cookie_is_renewed_at_most_once_a_day() {
    let dir = TempDir::new("renew");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    let token = authorize_by_link(&state).await;
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&token), None)).await;
    assert!(set_cookie(&resp).is_none(), "acabou de ser gravado");
    clock.advance(COOKIE_RENEW_EVERY_MS);
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&token), None)).await;
    assert!(set_cookie(&resp).expect("renovado").contains("Max-Age=31536000"));
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&token), None)).await;
    assert!(set_cookie(&resp).is_none(), "uma vez por dia");
}

#[tokio::test]
async fn code_flow_through_the_api_with_the_same_source_ip_for_everyone() {
    let dir = TempDir::new("codeapi");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    let computer = authorize_by_link(&state).await;
    // Sem dispositivo, do IP do Windows: não abre janela.
    let resp = send(&state, req(Method::POST, "/api/devices/code", NAT_PEER, None, Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Escrita com Origin de fora: 403 da guarda.
    let mut r = req(Method::POST, "/api/devices/code", NAT_PEER, Some(&computer), Some(json!({})));
    r.headers_mut().insert(header::ORIGIN, "http://evil.example".parse().expect("origin"));
    assert_eq!(send(&state, r).await.status(), StatusCode::FORBIDDEN);
    let resp = send(&state, req(Method::POST, "/api/devices/code", NAT_PEER, Some(&computer), Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!((body["state"].as_str(), body["seconds_to_next"].as_u64(), body["expires_in"].as_u64()), (Some("open"), Some(60), Some(600)));
    assert_eq!(body["entry_path"], ENTRY_PATH);
    let code = body["code"].as_str().expect("código").to_string();
    let resp = send(&state, req(Method::GET, "/api/devices/code", NAT_PEER, Some(&computer), None)).await;
    assert_eq!(json_body(resp).await["code"], code.as_str());
    // O celular (mesmo IP de origem) acerta o código.
    clock.advance(PAIR_MIN_INTERVAL_MS);
    let mut r = req(Method::POST, PAIR_API_PATH, NAT_PEER, None, Some(json!({"code": code})));
    r.headers_mut().insert(header::USER_AGENT, "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) Version/17.0 Safari/604.1".parse().expect("ua"));
    let resp = send(&state, r).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let phone = token_of(&set_cookie(&resp).expect("cookie"));
    let body = json_body(resp).await;
    assert_eq!(body["device"]["name"], "Safari no iPhone");
    assert_ne!(phone, computer);
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&phone), None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // A janela fechou com o acerto.
    let resp = send(&state, req(Method::GET, "/api/devices/code", NAT_PEER, Some(&phone), None)).await;
    assert_eq!(json_body(resp).await["state"], "closed");
    // DELETE fecha a janela aberta.
    send(&state, req(Method::POST, "/api/devices/code", NAT_PEER, Some(&computer), Some(json!({})))).await;
    let resp = send(&state, req(Method::DELETE, "/api/devices/code", NAT_PEER, Some(&computer), Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(state.devices().code_state(), CodeState::Closed);
}

#[tokio::test]
async fn five_wrong_codes_give_429_and_warn_the_computer() {
    let dir = TempDir::new("lock");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    let mut events = state.events().subscribe();
    let CodeState::Open { code, .. } = state.devices().open_window().expect("abre") else { panic!("aberta") };
    let wrong = if code == "999999" { "999998" } else { "999999" };
    for left in (0..MAX_CODE_ERRORS).rev() {
        clock.advance(PAIR_MIN_INTERVAL_MS);
        let resp = send(&state, req(Method::POST, PAIR_API_PATH, NAT_PEER, None, Some(json!({"code": wrong})))).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = json_body(resp).await;
        assert_eq!((body["code"].as_str(), body["params"]["attempts_left"].as_u64()), (Some(CODE_CODE_INVALID), Some(u64::from(left))));
        assert!(!body.to_string().contains(&code), "a resposta não traz o código");
    }
    clock.advance(PAIR_MIN_INTERVAL_MS);
    let resp = send(&state, req(Method::POST, PAIR_API_PATH, NAT_PEER, None, Some(json!({"code": code})))).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json_body(resp).await["code"], CODE_CODE_LOCKED);
    // Limite global: duas tentativas no mesmo segundo.
    let resp = send(&state, req(Method::POST, PAIR_API_PATH, NAT_PEER, None, Some(json!({"code": code})))).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp.headers().contains_key(header::RETRY_AFTER));
    assert_eq!(json_body(resp).await["code"], CODE_PAIR_RATE_LIMITED);
    // A tela do computador fica sabendo.
    let mut locked = false;
    while let Ok(ev) = events.try_recv() {
        if ev.event_type() == "device_code_locked" {
            locked = true;
            assert!(!ev.payload.to_string().contains(&code));
        }
    }
    assert!(locked, "evento device_code_locked");
    let resp = send(&state, req(Method::GET, "/api/devices/code", LOOPBACK_PEER, None, None)).await;
    assert_eq!(json_body(resp).await["state"], "locked");
}

#[tokio::test]
async fn rename_and_revoke_through_the_api() {
    let dir = TempDir::new("manage");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    let first = authorize_by_link(&state).await;
    let second = authorize_by_link(&state).await;
    let resp = send(&state, req(Method::GET, "/api/devices", NAT_PEER, Some(&first), None)).await;
    let body = json_body(resp).await;
    let list = body["devices"].as_array().expect("lista").clone();
    assert_eq!(list.len(), 2);
    let first_id = body["current"].as_str().expect("atual").to_string();
    let second_id = list.iter().find(|d| d["id"] != first_id.as_str()).and_then(|d| d["id"].as_str()).expect("outro").to_string();
    // Renomear: limpo e curto; vazio é recusado; id desconhecido dá 404.
    let uri = format!("/api/devices/{second_id}");
    let resp = send(&state, req(Method::PATCH, &uri, NAT_PEER, Some(&first), Some(json!({"name": " Tablet\u{0007} da\nsala "})))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["device"]["name"], "Tablet da sala");
    let resp = send(&state, req(Method::PATCH, &uri, NAT_PEER, Some(&first), Some(json!({"name": " \u{0001} "})))).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let resp = send(&state, req(Method::PATCH, "/api/devices/0000000000000000", NAT_PEER, Some(&first), Some(json!({"name": "x"})))).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // Sem dispositivo: nada de gestão.
    let resp = send(&state, req(Method::DELETE, &uri, NAT_PEER, None, Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Revogar o outro: ele perde o acesso na hora.
    let resp = send(&state, req(Method::DELETE, &uri, NAT_PEER, Some(&first), Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(set_cookie(&resp).is_none(), "não é o atual");
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&second), None)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Revogar o próprio: o cookie é apagado.
    let resp = send(&state, req(Method::DELETE, &format!("/api/devices/{first_id}"), NAT_PEER, Some(&first), Some(json!({})))).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(set_cookie(&resp).expect("apaga").contains("Max-Age=0"));
    assert_eq!(send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&first), None)).await.status(), StatusCode::UNAUTHORIZED);
    // Revogar todos (do loopback).
    let a = authorize_by_link(&state).await;
    let b = authorize_by_link(&state).await;
    let resp = send(&state, req(Method::DELETE, "/api/devices", LOOPBACK_PEER, None, Some(json!({})))).await;
    assert_eq!(json_body(resp).await["revoked"], 2);
    for t in [a, b] {
        assert_eq!(send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&t), None)).await.status(), StatusCode::UNAUTHORIZED);
    }
    let raw = std::fs::read_to_string(dir.path().join("data").join(DEVICES_FILE)).expect("arquivo");
    assert!(serde_json::from_str::<Value>(&raw).expect("json")["devices"].as_array().is_some_and(Vec::is_empty));
}

#[tokio::test]
async fn revoking_drops_the_open_event_stream() {
    let dir = TempDir::new("sse");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    let token = authorize_by_link(&state).await;
    let other = authorize_by_link(&state).await;
    let resp = send(&state, req(Method::GET, "/api/events", NAT_PEER, Some(&token), None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let kept = send(&state, req(Method::GET, "/api/events", NAT_PEER, Some(&other), None)).await;
    let reader = tokio::spawn(async move { resp.into_body().collect().await.map(|_| ()) });
    let mut kept_reader = tokio::spawn(async move { kept.into_body().collect().await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!reader.is_finished(), "o stream fica aberto até a revogação");
    let id = state.devices().authenticate(&token).expect("id");
    state.devices().revoke(&id).expect("revoga").expect("existia");
    let ended = tokio::time::timeout(Duration::from_secs(5), reader).await;
    assert!(ended.is_ok(), "o stream do revogado terminou");
    // O do outro dispositivo segue aberto.
    assert!(tokio::time::timeout(Duration::from_millis(100), &mut kept_reader).await.is_err());
    state.trigger_shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(5), kept_reader).await;
}

#[tokio::test]
async fn the_served_server_sees_real_loopback() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = TempDir::new("tcp");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let state = AppState::new(dir.path().join("config.toml"), Config::default(), port)
        .with_data_dir(dir.path().join("data"))
        .with_host_policy(HostPolicy::Lan)
        .with_network_run(NetworkRun { lan: true, source: ModeSource::Flag, requested_port: port });
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(crate::server::serve_with_shutdown(listener, state, async move {
        let _ = rx.await;
    }));
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("conecta");
    let request = format!("GET /api/devices HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    sock.write_all(request.as_bytes()).await.expect("envia");
    let mut out = String::new();
    sock.read_to_string(&mut out).await.expect("lê");
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let _ = tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), server).await;
}

#[tokio::test]
async fn any_listen_outside_loopback_requires_a_device_even_without_lan() {
    let dir = TempDir::new("exposed");
    let clock = Clock::new();
    // `--host 0.0.0.0 --allow-remote` sem `--lan`: política de Host local, modo de rede desligado.
    let state = AppState::new(dir.path().join("config.toml"), Config::default(), 3000)
        .with_data_dir(dir.path().join("data"))
        .with_device_clock(clock.handle())
        .with_exposed(true);
    assert!(state.lan_mode());
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let resp = send(&state, req(Method::GET, "/", NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), Some(ENTRY_PATH));
    let resp = send(&state, req(Method::GET, "/api/config", LOOPBACK_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let token = authorize_by_link(&state).await;
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, Some(&token), None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // Escuta só no loopback, sem rede: nada muda.
    let local = AppState::new(dir.path().join("config.toml"), Config::default(), 3000).with_exposed(false);
    assert!(!local.lan_mode());
}

#[tokio::test]
async fn entry_urls_in_wsl_use_the_windows_addresses_with_a_short_cache() {
    use crate::server::lan::{NetProbe, WINDOWS_IPS_ARGS};
    use crate::server::portproxy::WinTools;
    use crate::server::prereqs::CmdOutput;
    use std::sync::atomic::AtomicUsize;
    let dir = TempDir::new("winips");
    let clock = Clock::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let probe = NetProbe {
        tools: WinTools {
            run: Arc::new(move |p, a| {
                assert_eq!((p, a), ("powershell.exe", &WINDOWS_IPS_ARGS[..]));
                c.fetch_add(1, Ordering::SeqCst);
                Some(CmdOutput {
                    success: true,
                    stdout: "172.21.144.1|vEthernet (WSL)|0|1\n169.254.1.1|Ethernet|0|0\n192.168.0.68|Wi-Fi|1|1\n".into(),
                    stderr: String::new(),
                })
            }),
            exists: Arc::new(|p| p == "powershell.exe"),
        },
        wsl: true,
        local_ips: Some(vec![std::net::Ipv4Addr::new(172, 21, 154, 252)]),
    };
    let state = lan_state(&dir, &clock).with_net_probe(probe);
    for _ in 0..2 {
        let resp = send(&state, req(Method::POST, "/api/devices/code", LOOPBACK_PEER, None, Some(json!({})))).await;
        let body = json_body(resp).await;
        assert_eq!(body["entry_urls"], json!(["http://192.168.0.68:3000/connect"]), "{body}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cache curto");
    // Sem conferência: como antes (só o Host da página, se for IP da rede).
    let failing = NetProbe {
        tools: WinTools { run: Arc::new(|_: &str, _: &[&str]| None), exists: Arc::new(|_: &str| true) },
        wsl: true,
        local_ips: Some(Vec::new()),
    };
    let state = lan_state(&dir, &clock).with_net_probe(failing);
    let resp = send(&state, req(Method::POST, "/api/devices/code", LOOPBACK_PEER, None, Some(json!({})))).await;
    assert_eq!(json_body(resp).await["entry_urls"], json!([]));
}

/// Revogação durante o próprio handler (F-01): o stream devolvido depois dela também termina.
async fn revoke_then_stream(State(state): State<AppState>, Extension(access): Extension<Access>) -> axum::response::Response {
    use axum::response::sse::{Event, Sse};
    if let Some(id) = access.device.as_deref() {
        state.devices().revoke(id).expect("revoga").expect("existia");
    }
    Sse::new(futures_util::stream::pending::<Result<Event, std::convert::Infallible>>()).into_response()
}

#[tokio::test]
async fn revocation_inside_the_handler_still_ends_the_stream() {
    let dir = TempDir::new("race");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    let token = authorize_by_link(&state).await;
    let app = axum::Router::new()
        .route("/sse", axum::routing::get(revoke_then_stream))
        .layer(axum::middleware::from_fn_with_state(state.clone(), require_device))
        .with_state(state.clone());
    let resp = app.oneshot(req(Method::GET, "/sse", NAT_PEER, Some(&token), None)).await.expect("resposta");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(set_cookie(&resp).is_none(), "revogado não renova o cookie");
    let ended = tokio::time::timeout(Duration::from_secs(5), resp.into_body().collect()).await;
    assert!(ended.is_ok(), "o stream do dispositivo revogado no handler terminou");
}

// --- Spec 128 T01: cookie Lax, volta à página de origem e diagnóstico da negação ---------------

#[test]
fn device_cookie_is_lax_both_when_set_and_when_cleared() {
    let token = "b".repeat(64);
    let set = device_cookie(&token).expect("cabeçalho");
    assert_eq!(
        set.to_str().expect("ascii"),
        format!("nants_device={token}; Max-Age=31536000; Path=/; HttpOnly; SameSite=Lax")
    );
    assert_eq!(clear_cookie().to_str().expect("ascii"), "nants_device=; Max-Age=0; Path=/; HttpOnly; SameSite=Lax");
}

#[test]
fn next_accepts_only_relative_paths_of_nants() {
    for ok in ["/", "/projects/abc", "/projects/abc?tab=chat", "/x?a=1#sec", "/settings#devices", "/connectx", "/%2F%2Fevil"] {
        assert_eq!(safe_next(ok), Some(ok), "{ok}");
    }
    let long = format!("/{}", "a".repeat(MAX_NEXT_BYTES));
    for bad in [
        "", "//evil", "//evil.com/x", "http://evil.com", "https://evil.com/", "javascript:alert(1)", "evil.com", "x/y",
        "\\evil", "/\\evil", "/a\\b", "/\t/evil", "/a b", "/a\nb", "/connect", "/connect?next=/x", "/connect#x", long.as_str(),
    ] {
        assert_eq!(safe_next(bad), None, "{bad:?}");
    }
    assert!(safe_next(&format!("/{}", "a".repeat(MAX_NEXT_BYTES - 1))).is_some(), "2 KB exatos passam");
    // A entrada leva o caminho codificado; o recusado e a raiz levam à entrada pura.
    assert_eq!(entry_location(Some("/p?a=1&b=2#h")), "/connect?next=/p%3Fa%3D1%26b%3D2%23h");
    assert_eq!(entry_location(Some("//evil")), ENTRY_PATH);
    assert_eq!(entry_location(Some("/")), ENTRY_PATH);
    assert_eq!(entry_location(None), ENTRY_PATH);
}

#[tokio::test]
async fn denied_page_goes_to_the_entry_with_next_and_the_link_returns_to_it() {
    let dir = TempDir::new("next");
    let clock = Clock::new();
    let state = lan_state(&dir, &clock);
    let resp = send(&state, req(Method::GET, "/projects/abc?tab=chat", NAT_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&resp), Some("/connect?next=/projects/abc%3Ftab%3Dchat"));
    let resp = send(&state, req(Method::HEAD, "/settings", NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), Some("/connect?next=/settings"));
    // Caminho que começa com `//` não vira destino.
    let resp = send(&state, req(Method::GET, "//evil.com/x", NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), Some(ENTRY_PATH));
    // Rota com segredo na URL não vira `next` (o `code` e o `state` não passam pela entrada).
    let resp = send(&state, req(Method::GET, "/auth/finish?code=c&state=s", NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), Some(ENTRY_PATH));
    // A API continua com 401 JSON, sem redirecionar.
    let resp = send(&state, req(Method::GET, "/api/config", NAT_PEER, None, None)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(location(&resp).is_none());
    // O link de entrada com `next` volta para ele; com um `next` recusado, para a raiz.
    let t = state.devices().issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    let resp = send(&state, req(Method::GET, &format!("/pair?t={t}&next=%2Fprojects%2Fabc%3Ftab%3Dchat"), NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), Some("/projects/abc?tab=chat"));
    assert!(set_cookie(&resp).is_some());
    let t = state.devices().issue_entry_link(crate::i18n::Lang::PtBr).expect("link");
    let resp = send(&state, req(Method::GET, &format!("/pair?t={t}&next=%2F%2Fevil.com"), NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), Some("/"));
    // Link inválido: a entrada com o motivo e o mesmo `next`.
    let resp = send(&state, req(Method::GET, "/pair?t=errado&next=%2Fsettings", NAT_PEER, None, None)).await;
    assert_eq!(location(&resp), Some("/connect?reason=link_invalid&next=/settings"));
}

#[test]
fn denial_line_has_no_secret_and_is_rate_limited() {
    let dir = TempDir::new("denial");
    let clock = Clock::new();
    let store = store_in(&dir, &clock);
    let first = Denial { path: "/projects/abc", cookie: DenialCookie::Absent, sec_fetch_site: Some("cross-site") };
    assert_eq!(
        store.denial_line(&first).as_deref(),
        Some("nants.device_denied path=/projects/abc cookie=none sec_fetch_site=cross-site")
    );
    let unknown = Denial { path: "/", cookie: DenialCookie::Unknown, sec_fetch_site: None };
    assert_eq!(store.denial_line(&unknown).as_deref(), Some("nants.device_denied path=/ cookie=unknown"));
    let odd = Denial { path: "/x", cookie: DenialCookie::Absent, sec_fetch_site: Some("abc\" injetado") };
    assert_eq!(store.denial_line(&odd).as_deref(), Some("nants.device_denied path=/x cookie=none sec_fetch_site=other"));
    // Limite: DENIAL_LOG_MAX linhas por janela; a primeira da janela seguinte conta as suprimidas.
    for _ in 3..DENIAL_LOG_MAX {
        assert!(store.denial_line(&unknown).is_some());
    }
    assert!(store.denial_line(&unknown).is_none());
    assert!(store.denial_line(&unknown).is_none());
    clock.advance(DENIAL_LOG_WINDOW_MS);
    assert_eq!(store.denial_line(&unknown).as_deref(), Some("nants.device_denied path=/ cookie=unknown suppressed=2"));
    assert_eq!(store.denial_line(&unknown).as_deref(), Some("nants.device_denied path=/ cookie=unknown"));
}

/// Efeito de uma rota GET (auditoria da spec 128 T01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GetEffect {
    /// Só lê.
    Read,
    /// Renova um cache derivado ou um diagnóstico, sem mudar configuração, projeto nem
    /// dispositivo; repetir não faz mal e quem dispara de outro site não lê a resposta:
    /// - `/api/config/pricing/estimate?refresh`: baixa de novo o catálogo público de preços e
    ///   regrava o cache dele (o preço da configuração só muda no `POST .../autofill`);
    /// - `/api/config/connections/:id/models?refresh`: consulta a lista de modelos e guarda o
    ///   cache em memória; com o plano ChatGPT, pode renovar o token de acesso que já existe;
    /// - `/api/system/prereqs` e `/api/readiness`: rodam o diagnóstico da máquina (só leitura) e
    ///   guardam o resultado em memória;
    /// - `/api/devices/code`: troca o código vencido da janela já aberta, como o relógio faria
    ///   no próximo acesso (não abre janela nem zera erros).
    Cache,
    /// Altera estado, só com um segredo próprio na URL.
    Secret,
}

/// Toda rota GET do roteador (`src/server/mod.rs`), classificada. Rota nova sem classificação
/// faz o teste falhar: com o cookie `Lax`, um GET de outro site leva o cookie.
const GET_ROUTES: &[(&str, GetEffect)] = &[
    ("/api/config", GetEffect::Read),
    ("/api/config/pricing/estimate", GetEffect::Cache),
    ("/api/config/network", GetEffect::Read),
    ("/api/projects/:id/usage", GetEffect::Read),
    ("/api/config/connections/:id/models", GetEffect::Cache),
    ("chatgpt::CALLBACK_PATH", GetEffect::Read),
    ("chatgpt::FINISH_PATH", GetEffect::Secret),
    ("/api/connections/:id/chatgpt", GetEffect::Read),
    ("/api/projects/:id/state", GetEffect::Read),
    ("/api/projects/:id/activity", GetEffect::Read),
    ("/api/projects/:id/files", GetEffect::Read),
    ("/api/projects/:id/file", GetEffect::Read),
    ("/api/projects/:id/app", GetEffect::Read),
    ("/api/projects/:id/app/logs", GetEffect::Read),
    ("/api/projects/:id/app/exec", GetEffect::Read),
    ("/api/projects/:id/app/guide/:spec_id", GetEffect::Read),
    ("/api/projects/:id/specs/:spec_id/result", GetEffect::Read),
    ("/api/projects/:id/specs/:spec_id/captures/:name", GetEffect::Read),
    ("/api/projects/:id/specs/:spec_id/dialog", GetEffect::Read),
    ("/api/projects/:id/specs/:spec_id/tools", GetEffect::Read),
    ("/api/projects/:id/specs/:spec_id/report", GetEffect::Read),
    ("/api/projects/:id/architecture", GetEffect::Read),
    ("/api/projects/:id/chat", GetEffect::Read),
    ("/api/projects/:id/chat/attachments/:name", GetEffect::Read),
    ("/api/projects/:id/kb", GetEffect::Read),
    ("/api/projects/:id/kb/docs/:doc", GetEffect::Read),
    ("/api/projects/:id/kb/overview", GetEffect::Read),
    ("/api/ui-state", GetEffect::Read),
    ("/api/projects/:id/ui-state", GetEffect::Read),
    ("/api/attention", GetEffect::Read),
    ("/api/system/prereqs", GetEffect::Cache),
    ("/api/readiness", GetEffect::Cache),
    ("/api/fs/browse", GetEffect::Read),
    ("/api/events", GetEffect::Read),
    ("devices::PAIR_LINK_PATH", GetEffect::Secret),
    ("/api/devices", GetEffect::Read),
    ("/api/devices/code", GetEffect::Cache),
];

/// Rotas GET do roteador, lidas do código de `router` (primeiro argumento de cada `.route(` cujo
/// método inclui `get(`), mais o `fallback` dos arquivos da interface.
fn router_get_routes() -> Vec<String> {
    let body = router_body();
    let mut out = Vec::new();
    for chunk in body.split(".route(").skip(1) {
        // Fecha o `.route(` no parêntese correspondente.
        let mut depth = 1i32;
        let mut end = chunk.len();
        for (i, c) in chunk.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let call = &chunk[..end];
        let (path, methods) = call.split_once(',').expect("rota com handler");
        let has_get = methods
            .match_indices("get(")
            .any(|(i, _)| !methods[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_'));
        if has_get {
            out.push(path.trim().trim_matches('"').to_string());
        }
    }
    if body.contains(".fallback(static_ui::static_handler)") {
        out.push("fallback".to_string());
    }
    out
}

/// Corpo de `router` em `mod.rs`.
fn router_body() -> &'static str {
    let source = include_str!("mod.rs");
    let body = source.split("pub fn router(").nth(1).expect("router");
    body.split("\n}\n").next().expect("fim do router")
}

/// Construções do axum que registram rotas fora de `.route(` e que o analisador de
/// [`router_get_routes`] não enxerga (spec 128 T05, observação da revisão da T01).
const UNAUDITED_ROUTER_CALLS: [&str; 5] = ["any(", ".nest(", ".nest_service(", ".merge(", "route_service("];

/// Chamadas de [`UNAUDITED_ROUTER_CALLS`] no código de `body`, sem contar comentários de linha.
/// `any(` e `route_service(` só contam como nome inteiro (não como fim de outro nome, ex.: `company(`).
fn unaudited_router_calls(body: &str) -> Vec<&'static str> {
    let code: String = body.lines().map(|line| line.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
    UNAUDITED_ROUTER_CALLS
        .into_iter()
        .filter(|call| {
            code.match_indices(call).any(|(i, _)| {
                call.starts_with('.') || !code[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
        })
        .collect()
}

#[test]
fn unaudited_router_calls_detects_each_form() {
    for call in [
        ".route(\"/x\", any(h))",
        ".nest(\"/api\", inner)",
        ".nest_service(\"/s\", svc)",
        ".merge(other)",
        ".route_service(\"/r\", svc)",
    ] {
        assert_eq!(unaudited_router_calls(call).len(), 1, "{call}");
    }
    assert!(unaudited_router_calls(".route(\"/x\", get(company(h)))").is_empty());
    assert!(unaudited_router_calls("// .merge(antigo)\n.route(\"/x\", get(h))").is_empty());
}

#[test]
fn get_routes_with_effect_are_audited() {
    // Rota registrada fora de `.route(` escaparia da auditoria abaixo: o roteador não pode usá-las.
    assert_eq!(
        unaudited_router_calls(router_body()),
        Vec::<&str>::new(),
        "o router usa uma construção que a auditoria das rotas GET não enxerga; registre a rota com .route("
    );
    let found = router_get_routes();
    assert!(found.iter().any(|p| p == "fallback"), "{found:?}");
    for path in found.iter().filter(|p| *p != "fallback") {
        assert!(
            GET_ROUTES.iter().any(|(p, _)| p == path),
            "rota GET nova sem auditoria: {path}. Classifique em GET_ROUTES (e, se tiver efeito, exija um segredo próprio)"
        );
    }
    for (path, _) in GET_ROUTES {
        assert!(found.iter().any(|p| p == path), "rota auditada que não existe mais: {path}");
    }
    // As únicas rotas GET que alteram estado, cada uma com o próprio segredo na URL.
    let secret: Vec<&str> = GET_ROUTES.iter().filter(|(_, e)| *e == GetEffect::Secret).map(|(p, _)| *p).collect();
    assert_eq!(secret, ["chatgpt::FINISH_PATH", "devices::PAIR_LINK_PATH"]);
    assert_eq!(crate::server::chatgpt::FINISH_PATH, "/auth/finish");
    assert_eq!(PAIR_LINK_PATH, "/pair");
}
