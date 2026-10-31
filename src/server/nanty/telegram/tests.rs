//! Testes do canal do Telegram (spec 138 T06), só com um servidor falso do Telegram local, numa
//! porta livre (nunca o Telegram real): o pareamento (certo, errado, 5 erros, outro chat, grupo),
//! 401, 409, 429 e erro de rede, um erro sem o token, a deduplicação e o offset, e a parada e a
//! retomada com as mensagens acumuladas.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, State as AxState};
use axum::http::{header, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::client::{TelegramClient, TgError};
use super::*;
use crate::config::Config;
use crate::server::nanty::store::Speaker;
use crate::server::nanty::InstanceStatus;

pub(super) const BOT: &str = "nanty_test_bot";
pub(super) const OWNER: i64 = 7001;
pub(super) const STRANGER: i64 = 7002;

/// Token com a forma do Telegram, montado em tempo de execução (nenhum literal com cara de segredo).
pub(super) fn token(tag: &str) -> String {
    format!("{}:{}{}", 4242, tag, "t".repeat(30))
}

pub(super) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- Servidor falso do Telegram ----

/// Resposta forçada do próximo `getUpdates`.
#[derive(Clone, Copy)]
pub(super) enum Scripted {
    Status(u16),
    Retry(u64),
}

#[derive(Default)]
pub(super) struct Fake {
    pub(super) token: Mutex<String>,
    updates: Mutex<Vec<Value>>,
    pub(super) script: Mutex<VecDeque<Scripted>>,
    /// Respostas forçadas dos próximos `sendMessage` (T07).
    pub(super) send_script: Mutex<VecDeque<Scripted>>,
    /// Último `message_id` dado a um `sendMessage` (T07).
    next_id: Mutex<i64>,
    /// Reentrega tudo, ignorando o offset (testa a deduplicação da escuta).
    ignore_offset: Mutex<bool>,
    pub(super) calls: Mutex<Vec<(String, Value, Instant)>>,
    /// T09: o `getFile` de cada `file_id` (tamanho informado e caminho) e o conteúdo de cada caminho.
    pub(super) files: Mutex<std::collections::HashMap<String, (Option<u64>, String)>>,
    pub(super) downloads: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    /// T09: respostas forçadas dos próximos `sendVoice`.
    pub(super) voice_script: Mutex<VecDeque<Scripted>>,
}

impl Fake {
    pub(super) fn calls(&self, method: &str) -> Vec<(Value, Instant)> {
        lock(&self.calls).iter().filter(|(m, _, _)| m == method).map(|(_, b, t)| (b.clone(), *t)).collect()
    }

    pub(super) fn sent_texts(&self) -> Vec<String> {
        self.calls("sendMessage").iter().filter_map(|(b, _)| b["text"].as_str().map(str::to_string)).collect()
    }

    pub(super) fn push(&self, update: Value) {
        lock(&self.updates).push(update);
    }
}

fn error(code: u16, extra: Value) -> Response {
    let mut body = json!({"ok": false, "error_code": code, "description": "fake"});
    if let (Some(map), Some(more)) = (body.as_object_mut(), extra.as_object()) {
        map.extend(more.clone());
    }
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body)).into_response()
}

/// Os campos de texto de um corpo `multipart/form-data` (o `sendVoice`, T09) e o tamanho do arquivo
/// `voice` em `voice_bytes`.
pub(super) fn multipart_fields(raw: &[u8]) -> Value {
    let text = String::from_utf8_lossy(raw);
    let Some(boundary) = text.lines().next().map(str::trim).filter(|b| b.starts_with("--")) else { return Value::Null };
    let mut out = Map::new();
    for part in text.split(boundary).skip(1) {
        let Some((head, body)) = part.split_once("\r\n\r\n") else { continue };
        let Some(name) = head.split("name=\"").nth(1).and_then(|r| r.split('"').next()) else { continue };
        let body = body.strip_suffix("\r\n").unwrap_or(body);
        if name == "voice" {
            // O arquivo binário: a posição exata vem dos bytes, não do texto com perdas.
            let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);
            let len = find(raw, b"name=\"voice\"")
                .and_then(|at| Some(at + find(&raw[at..], b"\r\n\r\n")? + 4))
                .and_then(|start| Some(find(&raw[start..], boundary.as_bytes())?.saturating_sub(2)))
                .unwrap_or(0);
            out.insert("voice_bytes".into(), json!(len));
        } else {
            out.insert(name.to_string(), json!(body));
        }
    }
    Value::Object(out)
}

/// `GET /file/bot<token>/<caminho>` (T09): o download do arquivo do `getFile`.
async fn file_handler(AxState(fake): AxState<Arc<Fake>>, AxPath((bot, path)): AxPath<(String, String)>) -> Response {
    lock(&fake.calls).push(("download".into(), json!({ "path": path }), Instant::now()));
    if bot != format!("bot{}", lock(&fake.token)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match lock(&fake.downloads).get(&path) {
        Some(bytes) => bytes.clone().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn fake_handler(
    AxState(fake): AxState<Arc<Fake>>,
    AxPath((bot, method)): AxPath<(String, String)>,
    body: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or_else(|_| multipart_fields(&body));
    lock(&fake.calls).push((method.clone(), body.clone(), Instant::now()));
    if bot != format!("bot{}", lock(&fake.token)) {
        return error(401, json!({}));
    }
    match method.as_str() {
        "getMe" => Json(json!({"ok": true, "result": {"id": 1, "is_bot": true, "username": BOT}})).into_response(),
        "getUpdates" => {
            let scripted = lock(&fake.script).pop_front();
            match scripted {
                Some(Scripted::Status(code)) => return error(code, json!({})),
                Some(Scripted::Retry(secs)) => return error(429, json!({"parameters": {"retry_after": secs}})),
                None => {}
            }
            let offset = body["offset"].as_i64().unwrap_or(0);
            let out: Vec<Value> = {
                let mut q = lock(&fake.updates);
                if *lock(&fake.ignore_offset) {
                    q.clone()
                } else {
                    // Como o Telegram: o offset confirma (apaga) o que veio antes.
                    q.retain(|u| u["update_id"].as_i64().unwrap_or(0) >= offset);
                    q.clone()
                }
            };
            if out.is_empty() && body["timeout"].as_u64().unwrap_or(0) > 0 {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            Json(json!({"ok": true, "result": out})).into_response()
        }
        "sendMessage" => {
            let scripted = lock(&fake.send_script).pop_front();
            match scripted {
                Some(Scripted::Status(code)) => return error(code, json!({})),
                Some(Scripted::Retry(secs)) => return error(429, json!({"parameters": {"retry_after": secs}})),
                None => {}
            }
            let id = {
                let mut next = lock(&fake.next_id);
                *next += 1;
                *next
            };
            Json(json!({"ok": true, "result": {"message_id": id, "date": 0, "chat": {"id": body["chat_id"], "type": "private"}}})).into_response()
        }
        "getFile" => {
            let id = body["file_id"].as_str().unwrap_or_default().to_string();
            match lock(&fake.files).get(&id).cloned() {
                Some((size, path)) => Json(json!({"ok": true, "result": {"file_id": id, "file_size": size, "file_path": path}})).into_response(),
                None => error(400, json!({})),
            }
        }
        "sendVoice" => {
            let scripted = lock(&fake.voice_script).pop_front();
            match scripted {
                Some(Scripted::Status(code)) => return error(code, json!({})),
                Some(Scripted::Retry(secs)) => return error(429, json!({"parameters": {"retry_after": secs}})),
                None => {}
            }
            let id = {
                let mut next = lock(&fake.next_id);
                *next += 1;
                *next
            };
            Json(json!({"ok": true, "result": {"message_id": id, "date": 0, "chat": {"id": 0, "type": "private"}}})).into_response()
        }
        _ => Json(json!({"ok": true, "result": true})).into_response(),
    }
}

/// Sobe o servidor falso numa porta livre do loopback.
pub(super) async fn fake_server(token: &str) -> (Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    *lock(&fake.token) = token.to_string();
    let app = Router::new()
        .route("/:bot/:method", post(fake_handler))
        .route("/file/:bot/*path", axum::routing::get(file_handler))
        .with_state(Arc::clone(&fake));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("porta");
    let addr = listener.local_addr().expect("endereço");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (fake, format!("http://{addr}"))
}

/// Endereço do loopback sem ninguém escutando (erro de rede).
async fn dead_base() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("porta");
    let addr = listener.local_addr().expect("endereço");
    drop(listener);
    format!("http://{addr}")
}

fn now_secs() -> i64 {
    i64::try_from(crate::server::nanty::clock::now_ms() / 1000).unwrap_or(0)
}

fn message(update_id: i64, chat_id: i64, chat_type: &str, from: i64, text: &str) -> Value {
    message_at(update_id, chat_id, chat_type, from, text, now_secs())
}

fn message_at(update_id: i64, chat_id: i64, chat_type: &str, from: i64, text: &str, date: i64) -> Value {
    json!({"update_id": update_id, "message": {
        "message_id": update_id, "date": date,
        "chat": {"id": chat_id, "type": chat_type},
        "from": {"id": from, "is_bot": false},
        "text": text,
    }})
}

pub(super) fn click(update_id: i64, from: i64, chat_id: i64, data: &str) -> Value {
    json!({"update_id": update_id, "callback_query": {
        "id": format!("q{update_id}"),
        "from": {"id": from, "is_bot": false},
        "message": {"message_id": 1, "date": now_secs(), "chat": {"id": chat_id, "type": "private"}},
        "data": data,
    }})
}

/// Clique num botão da mensagem `message_id` (T07).
pub(super) fn click_on(update_id: i64, from: i64, chat_id: i64, message_id: i64, data: &str) -> Value {
    let mut update = click(update_id, from, chat_id, data);
    update["callback_query"]["message"]["message_id"] = json!(message_id);
    update
}

// ---- Mundo de teste ----

pub(super) fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-t06-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

pub(super) const FAST: Tuning = Tuning {
    poll_secs: 1,
    backoff_base: Duration::from_millis(50),
    backoff_max: Duration::from_millis(200),
    send_gap: Duration::from_millis(50),
    send_settle: Duration::ZERO,
};

pub(super) struct World {
    pub(super) dir: PathBuf,
    pub(super) state: AppState,
    pub(super) listener: Option<tokio::task::JoinHandle<()>>,
}

impl World {
    pub(super) fn new(dir: PathBuf, base: &str, cfg: Config) -> Self {
        // Hora local fixa ao meio-dia (fora do silêncio): o envio não depende da hora em que a suíte roda.
        let state = AppState::new(dir.join("config.toml"), cfg, 3000)
            .with_data_dir(dir.join("data"))
            .with_telegram_api(base)
            .with_nanty_clock(crate::server::nanty::clock::test_clock());
        assert_eq!(state.nanty().acquire(), InstanceStatus::Active);
        state.nanty().telegram().set_tuning(FAST);
        Self { dir, state, listener: None }
    }

    pub(super) fn with_token(tag: &str, base: &str, token: &str, edit: impl FnOnce(&mut Config)) -> Self {
        let mut cfg = Config::default();
        cfg.nanty.telegram.bot_token = token.to_string();
        edit(&mut cfg);
        Self::new(temp(tag), base, cfg)
    }

    pub(super) fn start(&mut self) {
        let state = self.state.clone();
        self.listener = Some(tokio::spawn(listen(state.clone(), state.shutdown_signal())));
    }

    /// Para a escuta como o encerramento do Nants e solta a trava.
    pub(super) async fn stop(&mut self) {
        self.state.trigger_shutdown();
        if let Some(handle) = self.listener.take() {
            tokio::time::timeout(Duration::from_secs(5), handle).await.expect("a escuta para").expect("sem pânico");
        }
        self.state.nanty().release();
    }

    pub(super) fn health(&self) -> Health {
        self.state.nanty().telegram().health()
    }

    pub(super) async fn tg(&self) -> NantyTelegram {
        self.state.config().read().await.nanty.telegram.clone()
    }

    pub(super) fn journal(&self, code: &str) -> Vec<Map<String, Value>> {
        self.state
            .nanty()
            .journal_tail(1000)
            .into_iter()
            .filter(|e| e.kind == JournalKind::Telegram && e.code == code)
            .map(|e| e.params)
            .collect()
    }

    async fn request(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:3000")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
            .expect("req");
        let resp = crate::server::router(self.state.clone()).oneshot(req).await.expect("resp");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("corpo").to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    pub(super) async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.request(Method::POST, uri, Some(body)).await
    }

    async fn get(&self, uri: &str) -> Value {
        self.request(Method::GET, uri, None).await.1
    }

    /// Abre a janela de pareamento pela API e devolve o código.
    async fn open_code(&self) -> String {
        let (status, body) = self.post("/api/nanty/telegram/pair", json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["pairing"]["state"], "open", "{body}");
        let code = body["pairing"]["code"].as_str().expect("código").to_string();
        assert_eq!(body["pairing"]["link"], format!("https://t.me/{BOT}?start={code}"));
        code
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.state.trigger_shutdown();
        self.state.nanty().release();
        if !self.dir.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Espera a escuta sair do esvaziamento inicial: um `getUpdates` com espera depois do pedido `from`.
pub(super) async fn live_after(fake: &Fake, from: usize) {
    until("escuta ao vivo", || fake.calls("getUpdates").iter().skip(from).any(|(b, _)| b["timeout"] == 1)).await;
}

/// Espera a condição (até 5 s).
pub(super) async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(5), "esperou demais: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Espera a condição assíncrona (até 5 s).
pub(super) async fn until_async<F: std::future::Future<Output = bool>>(what: &str, mut f: impl FnMut() -> F) {
    let start = Instant::now();
    while !f().await {
        assert!(start.elapsed() < Duration::from_secs(5), "esperou demais: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(super) fn paired(cfg: &mut Config) {
    cfg.nanty.telegram.chat_id = Some(OWNER);
    cfg.nanty.telegram.owner_id = Some(OWNER);
}

// ---- Testes ----

#[tokio::test]
async fn pairing_right_wrong_group_and_strangers() {
    let tk = token("pair");
    let (fake, base) = fake_server(&tk).await;
    let mut w = World::with_token("pair", &base, &tk, |_| {});
    // Sem conexão ainda, não há link para mostrar.
    let (status, body) = w.post("/api/nanty/telegram/pair", json!({})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("nanty_telegram_not_connected")));
    w.start();
    until("conectado", || w.health() == Health::Connected && w.state.nanty().telegram().bot().is_some()).await;
    live_after(&fake, 0).await;
    let code = w.open_code().await;
    assert!(code.len() <= 64 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));

    // Código errado num chat privado: não pareia e responde que o código não vale.
    fake.push(message(1, 500, "private", 500, "/start errado"));
    // O código certo num grupo: o Nants ignora e sai do grupo.
    fake.push(message(2, -900, "group", 501, &format!("/start {code}")));
    until("grupo", || !fake.calls("leaveChat").is_empty()).await;
    assert_eq!(fake.calls("leaveChat")[0].0["chat_id"], -900);
    assert!(w.tg().await.chat_id.is_none(), "grupo nunca pareia");
    assert_eq!(w.journal("telegram.pair_failed").len(), 1);

    // O certo no chat privado: fixa o `from.id` e o `chat.id`.
    fake.push(message(3, OWNER, "private", OWNER, &format!("/start {code}")));
    until_async("pareado", || async { w.tg().await.chat_id == Some(OWNER) }).await;
    let tg = w.tg().await;
    assert_eq!((tg.chat_id, tg.owner_id), (Some(OWNER), Some(OWNER)));
    until("mensagens", || fake.sent_texts().len() >= 2).await;
    let texts = fake.sent_texts();
    assert_eq!(texts[0], tr(Lang::PtBr, "nanty.telegram.pair_invalid", &[]));
    assert_eq!(texts[1], tr(Lang::PtBr, "nanty.telegram.paired", &[]));
    // Uso único: a janela fechou.
    assert_eq!(w.get("/api/nanty/telegram").await["pairing"]["state"], "closed");
    let view = w.get("/api/nanty/telegram").await;
    assert_eq!((view["paired"].as_bool(), view["health"].as_str()), (Some(true), Some("connected")), "{view}");

    // Outro `from.id` (mensagem e clique) é ignorado e registrado; o dono entra na conversa.
    fake.push(message(4, STRANGER, "private", STRANGER, "oi, sou outro"));
    fake.push(click(5, STRANGER, STRANGER, "c:0123456789abcdef:0123456789abcdef0123456789abcdef:y"));
    fake.push(message(6, OWNER, "private", OWNER, "olá, Nanty"));
    until("conversa", || !w.state.nanty().conversation().is_empty()).await;
    until("estranhos", || w.journal("telegram.stranger").len() == 2).await;
    let kinds: Vec<Option<Value>> = w.journal("telegram.stranger").iter().map(|p| p.get("kind").cloned()).collect();
    assert_eq!(kinds, [Some(json!("message")), Some(json!("click"))]);
    // T08: o Nanty responde ao dono (sem modelo, o texto fixo); dos estranhos, nada entra.
    let conv: Vec<_> = w.state.nanty().conversation().into_iter().filter(|m| m.from == Speaker::Person).collect();
    assert_eq!(conv.len(), 1, "só a mensagem do dono");
    assert_eq!((conv[0].from, conv[0].channel, conv[0].text.as_deref()), (Speaker::Person, Channel::Telegram, Some("olá, Nanty")));
    assert!(fake.calls("answerCallbackQuery").is_empty(), "o clique do estranho fica sem resposta");
    // Offset gravado depois de processar, no arquivo da configuração.
    until_async("offset", || async { w.tg().await.offset == 7 }).await;
    let saved = Config::load_from(&w.dir.join("config.toml")).expect("config");
    assert_eq!((saved.nanty.telegram.offset, saved.nanty.telegram.owner_id), (7, Some(OWNER)));
    // Nada do pareamento vai para o registro (nem o código, nem o token).
    let journal = serde_json::to_string(&w.state.nanty().journal_tail(1000)).expect("json");
    assert!(!journal.contains(&code) && !journal.contains(&tk), "{journal}");
    w.stop().await;
}

#[tokio::test]
async fn five_errors_lock_the_code() {
    let tk = token("lock");
    let (fake, base) = fake_server(&tk).await;
    let mut w = World::with_token("lock", &base, &tk, |_| {});
    w.start();
    until("conectado", || w.state.nanty().telegram().bot().is_some()).await;
    let code = w.open_code().await;
    for i in 1..=5 {
        fake.push(message(i, 600, "private", 600, &format!("/start errado{i}")));
    }
    fake.push(message(6, OWNER, "private", OWNER, &format!("/start {code}")));
    until("seis tentativas", || w.journal("telegram.pair_failed").len() == 6).await;
    let left: Vec<Option<Value>> = w.journal("telegram.pair_failed").iter().map(|p| p.get("attempts_left").cloned()).collect();
    assert_eq!(left[..5], [Some(json!(4)), Some(json!(3)), Some(json!(2)), Some(json!(1)), Some(json!(0))]);
    assert_eq!(w.journal("telegram.pair_failed")[5]["reason"], "locked");
    assert!(w.tg().await.chat_id.is_none(), "nem o certo pareia depois da trava");
    assert_eq!(w.get("/api/nanty/telegram").await["pairing"]["state"], "locked");
    // Um código novo, gerado na tela, reabre.
    let fresh = w.open_code().await;
    fake.push(message(7, OWNER, "private", OWNER, &format!("/start {fresh}")));
    until_async("pareado", || async { w.tg().await.owner_id == Some(OWNER) }).await;
    w.stop().await;
}

#[tokio::test]
async fn unpair_and_token_change_clear_chat_and_offset() {
    let tk = token("clear");
    let (_fake, base) = fake_server(&tk).await;
    let w = World::with_token("clear", &base, &tk, |cfg| {
        paired(cfg);
        cfg.nanty.telegram.offset = 41;
    });
    let (status, body) = w.post("/api/nanty/telegram/pair", json!({})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("nanty_telegram_already_paired")));
    let (status, body) = w.post("/api/nanty/telegram/unpair", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["paired"], false);
    let tg = w.tg().await;
    assert_eq!((tg.chat_id, tg.owner_id, tg.offset), (None, None, 0));

    // Token novo: limpa o chat, o dono e o offset; a resposta só traz a máscara.
    {
        let mut cfg = w.state.config().write().await;
        paired(&mut cfg);
        cfg.nanty.telegram.offset = 9;
    }
    let other = token("outro");
    let (status, body) = w.post("/api/nanty/telegram/token", json!({"token": other})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.to_string().contains(&other), "{body}");
    assert_eq!(body["bot_token_masked"], crate::server::config_api::mask_api_key(&other));
    let tg = w.tg().await;
    assert_eq!((tg.bot_token.as_str(), tg.chat_id, tg.owner_id, tg.offset), (other.as_str(), None, None, 0));
    let saved = Config::load_from(&w.dir.join("config.toml")).expect("config");
    assert_eq!(saved.nanty.telegram.bot_token, other);

    // Token com forma errada e campo a mais são recusados; `env:NOME` e vazio valem.
    let (status, body) = w.post("/api/nanty/telegram/token", json!({"token": "abc/../x"})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("nanty_telegram_token_invalid")));
    let (status, _) = w.post("/api/nanty/telegram/token", json!({"token": other, "x": 1})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = w.post("/api/nanty/telegram/token", json!({"token": "env:NANTS_T06_BOT"})).await;
    assert_eq!((status, body["bot_token_masked"].as_str()), (StatusCode::OK, Some("env:NANTS_T06_BOT")));
    let (status, body) = w.post("/api/nanty/telegram/token", json!({"token": ""})).await;
    assert_eq!((status, body["configured"].as_bool()), (StatusCode::OK, Some(false)));
    let (status, body) = w.post("/api/nanty/telegram/test", json!({})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("nanty_telegram_not_configured")));
}

#[tokio::test]
async fn test_message_goes_to_the_owner() {
    let tk = token("teste");
    let (fake, base) = fake_server(&tk).await;
    let w = World::with_token("teste", &base, &tk, |_| {});
    let (status, body) = w.post("/api/nanty/telegram/test", json!({})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("nanty_telegram_not_paired")));
    paired(&mut *w.state.config().write().await);
    let (status, body) = w.post("/api/nanty/telegram/test", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sent = fake.calls("sendMessage");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0["chat_id"], OWNER);
    assert_eq!(sent[0].0["text"], tr(Lang::PtBr, "nanty.telegram.test", &[]));
    assert!(sent[0].0.get("parse_mode").is_none(), "texto puro");
    // O Telegram recusa o token: erro com código próprio.
    *lock(&fake.token) = token("trocado");
    let (status, body) = w.post("/api/nanty/telegram/test", json!({})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::BAD_GATEWAY, Some("nanty_telegram_token_rejected")));
}

#[tokio::test]
async fn invalid_token_stops_until_the_token_changes() {
    let good = token("bom");
    let (fake, base) = fake_server(&good).await;
    let mut w = World::with_token("t401", &base, &token("ruim"), |_| {});
    w.start();
    until("token inválido", || w.health() == Health::InvalidToken).await;
    assert_eq!(w.get("/api/nanty/status").await["telegram"], "invalid_token");
    let calls = lock(&fake.calls).len();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(lock(&fake.calls).len(), calls, "a escuta para com o token recusado");
    assert_eq!(w.journal("telegram.invalid_token").len(), 1);
    // Trocar o token acorda a escuta.
    let (status, _) = w.post("/api/nanty/telegram/token", json!({"token": good})).await;
    assert_eq!(status, StatusCode::OK);
    until("conectado", || w.health() == Health::Connected).await;
    w.stop().await;
    assert_eq!(w.health(), Health::Off, "a escuta parada fica desligada");
}

#[tokio::test]
async fn conflict_and_server_errors_back_off_and_recover() {
    let tk = token("conflito");
    let (fake, base) = fake_server(&tk).await;
    lock(&fake.script).extend([Scripted::Status(409), Scripted::Status(409), Scripted::Status(409)]);
    let mut w = World::with_token("t409", &base, &tk, |_| {});
    let mut seen = Vec::new();
    w.start();
    let start = Instant::now();
    while w.health() != Health::Connected || !lock(&fake.script).is_empty() {
        seen.push(w.health());
        assert!(start.elapsed() < Duration::from_secs(5), "{seen:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(seen.contains(&Health::Conflict), "{seen:?}");
    // Espera crescente entre os conflitos.
    let polls: Vec<Instant> = fake.calls("getUpdates").iter().map(|(_, t)| *t).collect();
    let gaps: Vec<Duration> = polls.windows(2).take(3).map(|p| p[1] - p[0]).collect();
    assert!(gaps[0] >= Duration::from_millis(45) && gaps[1] >= Duration::from_millis(95), "{gaps:?}");
    // Um 500 do Telegram é erro passageiro: não derruba a escuta.
    lock(&fake.script).push_back(Scripted::Status(500));
    until("500", || w.health() == Health::NetworkError).await;
    until("volta", || w.health() == Health::Connected).await;
    w.stop().await;
}

#[tokio::test]
async fn rate_limit_waits_for_retry_after() {
    let tk = token("cota");
    let (fake, base) = fake_server(&tk).await;
    let mut w = World::with_token("t429", &base, &tk, |_| {});
    w.start();
    until("conectado", || w.health() == Health::Connected).await;
    lock(&fake.script).push_back(Scripted::Retry(1));
    until("429", || w.health() == Health::RateLimited).await;
    until("volta", || w.health() == Health::Connected).await;
    let polls: Vec<Instant> = fake.calls("getUpdates").iter().map(|(_, t)| *t).collect();
    // O pedido que levou o 429 e o seguinte: pelo menos o `retry_after` entre eles.
    let gap = polls.windows(2).map(|p| p[1] - p[0]).max().expect("dois pedidos");
    assert!(gap >= Duration::from_millis(950), "{gap:?}");
    w.stop().await;
}

#[tokio::test]
async fn network_error_never_leaks_the_token_and_never_stops_nants() {
    let tk = token("rede");
    let base = dead_base().await;
    // O erro do reqwest, já sem URL e sem token.
    let client = TelegramClient::new(&base, tk.clone(), Duration::from_secs(1)).expect("cliente");
    let err = client.get_updates(0, 0).await.expect_err("sem servidor");
    let text = format!("{err} {err:?}");
    assert!(matches!(err, TgError::Network(_)), "{text}");
    assert!(!text.contains(&tk) && !text.contains("/bot"), "{text}");
    // Uma URL que o reqwest recusa ao montar (porta inválida) também não leva o token.
    let client = TelegramClient::new("http://127.0.0.1:99999", tk.clone(), Duration::from_secs(1)).expect("cliente");
    let err = client.get_me().await.expect_err("porta inválida");
    let text = format!("{err} {err:?}");
    assert!(!text.contains(&tk) && !text.contains("4242"), "{text}");
    assert!(!format!("{client:?}").contains(&tk));

    let mut w = World::with_token("rede", &base, &tk, |_| {});
    w.start();
    until("erro de rede", || w.health() == Health::NetworkError).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.health(), Health::NetworkError, "a escuta segue tentando");
    assert!(w.listener.as_ref().is_some_and(|h| !h.is_finished()));
    w.stop().await;
}

#[tokio::test]
async fn duplicates_are_processed_once_and_offset_advances() {
    let tk = token("dedup");
    let (fake, base) = fake_server(&tk).await;
    *lock(&fake.ignore_offset) = true;
    let mut w = World::with_token("dedup", &base, &tk, paired);
    w.start();
    until("conectado", || w.health() == Health::Connected).await;
    // Depois do esvaziamento inicial: a mensagem é ao vivo.
    live_after(&fake, 0).await;
    fake.push(message(10, OWNER, "private", OWNER, "uma vez só"));
    fake.push(message(11, OWNER, "private", OWNER, "e outra"));
    until_async("offset", || async { w.tg().await.offset == 12 }).await;
    // O servidor falso reentrega tudo a cada pedido: a escuta pula o que já viu.
    let polls = fake.calls("getUpdates").len();
    until("reentregas", || fake.calls("getUpdates").len() >= polls + 3).await;
    let texts: Vec<String> = w.state.nanty().conversation().into_iter().filter_map(|m| m.text).collect();
    assert_eq!(texts, ["uma vez só", "e outra"]);
    let last = fake.calls("getUpdates").last().map(|(b, _)| b["offset"].clone());
    assert_eq!(last, Some(json!(12)));
    // Mensagem ao vivo não vira cartão antigo.
    assert!(w.state.nanty().snapshot().cards.is_empty());
    w.stop().await;
}

#[tokio::test]
async fn closed_nants_backlog_becomes_stale_cards_never_actions() {
    let tk = token("volta");
    let (fake, base) = fake_server(&tk).await;
    let dir = temp("volta");
    let mut cfg = Config::default();
    cfg.nanty.telegram.bot_token = tk.clone();
    paired(&mut cfg);
    crate::server::config_api::save_atomic(&dir.join("config.toml"), &cfg).expect("config");

    // Primeira vida: um cartão aberto com token e uma mensagem processada.
    let mut first = World::new(dir.clone(), &base, cfg);
    first.start();
    until("conectado", || first.health() == Health::Connected).await;
    live_after(&fake, 0).await;
    fake.push(message(20, OWNER, "private", OWNER, "antes de fechar"));
    until_async("offset", || async { first.tg().await.offset == 21 }).await;
    let card_token = act::new_token();
    let card = Card {
        id: crate::server::nanty::new_id(),
        group: "system:disk".into(),
        code: "nanty.why.disk_low".into(),
        token: Some(card_token.clone()),
        created_at_ms: crate::server::nanty::clock::now_ms(),
        expires_at_ms: crate::server::nanty::clock::now_ms() + 3_600_000,
        ..Card::default()
    };
    let card_id = card.id.clone();
    first.state.nanty().update(|st| {
        st.cards.push(card);
        true
    });
    first.stop().await;
    let config = Config::load_from(&dir.join("config.toml")).expect("config gravada");
    assert_eq!(config.nanty.telegram.offset, 21, "o offset sobrevive ao fechamento");

    // Com o Nants fechado, o dono escreve e toca num botão do cartão.
    let old = now_secs() - 600;
    fake.push(message_at(21, OWNER, "private", OWNER, "você viu o disco?", old));
    fake.push(click(22, OWNER, OWNER, &callback_data(&card_id, &card_token, Answer::Later)));

    // Segunda vida, com a mesma pasta: o acumulado vira cartão antigo e nada é decidido.
    let kept = std::mem::take(&mut first.dir);
    drop(first);
    let before = fake.calls("getUpdates").len();
    let mut second = World::new(kept, &base, config);
    second.start();
    until("dois antigos", || {
        second.state.nanty().snapshot().cards.iter().filter(|c| c.group.starts_with(STALE_GROUP)).count() == 2
    })
    .await;
    let snapshot = second.state.nanty().snapshot();
    let stale: Vec<&Card> = snapshot.cards.iter().filter(|c| c.group.starts_with(STALE_GROUP)).collect();
    assert_eq!(stale[0].code, "nanty.card.telegram_stale_message");
    assert_eq!(stale[0].params["stale"], true);
    assert!(stale[0].params["age_min"].as_u64().is_some_and(|m| m >= 9), "{:?}", stale[0].params);
    assert_eq!(stale[1].code, "nanty.card.telegram_stale_click");
    assert!(stale.iter().all(|c| c.token.is_none() && c.action.is_none() && c.delivery == Some(Delivery::Digest)));
    // O cartão original segue aberto: o clique acumulado não decidiu nada.
    assert!(snapshot.cards.iter().any(|c| c.id == card_id), "nenhuma ação imediata");
    assert!(snapshot.decided.is_empty());
    until("resposta ao clique antigo", || !fake.calls("answerCallbackQuery").is_empty()).await;
    let answers = fake.calls("answerCallbackQuery");
    assert_eq!(answers.last().map(|(b, _)| b["text"].clone()), Some(json!(tr(Lang::PtBr, "nanty.telegram.stale_click", &[]))));
    let texts: Vec<String> = second.state.nanty().conversation().into_iter().filter_map(|m| m.text).collect();
    assert_eq!(texts, ["antes de fechar", "você viu o disco?"], "a mensagem antiga entra na conversa uma vez");

    // Esvaziada a fila, o clique ao vivo do dono decide pelo Telegram.
    live_after(&fake, before).await;
    fake.push(click(23, OWNER, OWNER, &callback_data(&card_id, &card_token, Answer::Later)));
    until("decidido", || !second.state.nanty().snapshot().decided.is_empty()).await;
    let decided = second.state.nanty().snapshot().decided;
    assert_eq!((decided[0].card_id.as_str(), decided[0].decided_by), (card_id.as_str(), DecidedBy::Telegram));
    until("resposta", || fake.calls("answerCallbackQuery").len() == 2).await;
    let answer = fake.calls("answerCallbackQuery")[1].0["text"].clone();
    assert_eq!(answer, json!(tr(Lang::PtBr, "nanty.outcome.snoozed", &[])));
    second.stop().await;
}

#[test]
fn callback_data_fits_telegram_and_round_trips() {
    let id = "0123456789abcdef";
    let tk = "0123456789abcdef0123456789abcdef";
    for answer in [Answer::Yes, Answer::No, Answer::Later] {
        let data = callback_data(id, tk, answer);
        assert!(data.len() <= 64, "{data}");
        assert_eq!(parse_callback(&data), Some((id.to_string(), tk.to_string(), answer)));
    }
    assert_eq!(parse_callback("c:xyz:abc:y"), None);
    assert_eq!(parse_callback(&format!("c:{id}:{tk}:y:extra")), None);
    assert_eq!(start_code("/start abc_-1"), Some("abc_-1"));
    assert_eq!(start_code("/start"), None);
    assert_eq!(start_code("/startabc"), None);
    assert!(valid_token(&token("forma")));
    assert!(!valid_token("123:curto") && !valid_token("abc:defghijklmnopqrstuvwxyz") && !valid_token("1:a/b"));
}
