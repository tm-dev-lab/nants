//! Testes da lista de modelos e do teste de conexão das conexões nativas (spec 132 T08), com
//! servidor falso local. Nenhum teste vai à rede real; o teste ao vivo é ignorado.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::server::config_api::{run_handshake_with, run_models_probe_with, test_connection};
use crate::server::state::AppState;

/// Chave dos testes: só um marcador para conferir que nada a ecoa.
const KEY: &str = "chave-de-teste-nativa-5151";
const TIMEOUT: Duration = Duration::from_secs(5);

/// Requisição capturada: a cabeça (linha e cabeçalhos) e o corpo.
#[derive(Debug, Clone)]
struct Captured {
    head: String,
    body: String,
}

impl Captured {
    fn path(&self) -> &str {
        self.head.lines().next().and_then(|l| l.split(' ').nth(1)).unwrap_or("")
    }

    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }
}

/// Lê a requisição inteira (cabeça e, com `Content-Length`, o corpo).
async fn read_request(sock: &mut TcpStream) -> Captured {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        match sock.read(&mut tmp).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&tmp[..n]),
            _ => break buf.len(),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let len = head
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length")))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        match sock.read(&mut tmp).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&tmp[..n]),
            _ => break,
        }
    }
    let body = String::from_utf8_lossy(&buf[head_end.min(buf.len())..]).into_owned();
    Captured { head, body }
}

fn response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Servidor local que responde, em ordem, cada resposta e guarda as requisições. Devolve o
/// endereço (sem caminho, como o de uma conexão nativa).
async fn spawn(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Captured>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("endereço");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let cap = Arc::clone(&captured);
    tokio::spawn(async move {
        for raw in responses {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let req = read_request(&mut sock).await;
            cap.lock().expect("lock").push(req);
            let _ = sock.write_all(raw.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), captured)
}

/// Conexão nativa salva com o endereço avançado `endpoint` (o servidor falso).
fn native(kind: ConnectionKind, endpoint: &str) -> Connection {
    let mut conn = Connection::native("nat", "Nativa", kind, KEY);
    conn.base_url_override = Some(endpoint.to_string());
    conn.normalize();
    assert_eq!(conn.endpoint(), endpoint, "o avançado local é aceito");
    conn
}

fn ids(list: &ModelList) -> Vec<&str> {
    list.models.iter().map(|m| m.id.as_str()).collect()
}

fn state_with(connections: Vec<Connection>) -> AppState {
    let dir = std::env::temp_dir().join(format!(
        "nants-t132-t08-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    AppState::new(dir.join("config.toml"), Config { connections, ..Config::default() }, 3000)
}

/// `POST /api/config/connections/test` pelo handler: o resultado JSON e o texto inteiro.
async fn post_test(state: &AppState, body: Value) -> Result<(Value, String), ApiErrorCode> {
    match test_connection(State(state.clone()), Bytes::from(body.to_string())).await {
        Ok(resp) => {
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("corpo");
            let text = String::from_utf8(bytes.to_vec()).expect("utf8");
            Ok((serde_json::from_str(&text).expect("json"), text))
        }
        Err(err) => Err(ApiErrorCode(err.status(), err.code())),
    }
}

/// Status e código de um erro 4xx do handler.
#[derive(Debug, PartialEq, Eq)]
struct ApiErrorCode(StatusCode, Option<&'static str>);

const ANTHROPIC_PAGE_1: &str = r#"{"data":[
  {"type":"model","id":"claude-b","display_name":"Claude B","created_at":"2026-10-07T18:00:00Z","max_input_tokens":1000000,"max_tokens":128000,
   "capabilities":{"thinking":{"supported":true},"image_input":{"supported":true}}},
  {"type":"model","id":"claude-a","display_name":"Claude A","max_input_tokens":200000,"max_tokens":64000,
   "capabilities":{"thinking":{"supported":false},"image_input":{"supported":false}}}
 ],"has_more":true,"first_id":"claude-b","last_id":"claude-a"}"#;
const ANTHROPIC_PAGE_2: &str = r#"{"data":[
  {"type":"model","id":"claude-velho","display_name":"Claude Velho"},
  {"type":"model","id":"claude-a","display_name":"Claude A repetido"}
 ],"has_more":false,"first_id":"claude-velho","last_id":"claude-a"}"#;

#[tokio::test]
async fn anthropic_pagina_le_os_campos_e_preenche_as_memorias() {
    let (base, cap) = spawn(vec![response(200, ANTHROPIC_PAGE_1), response(200, ANTHROPIC_PAGE_2)]).await;
    let conn = native(ConnectionKind::Anthropic, &base);
    let list = fetch(&conn, &base, TIMEOUT).await.expect("lista");
    assert_eq!(ids(&list), ["claude-b", "claude-a", "claude-velho"], "na ordem do provedor, sem repetir");
    assert!(!list.truncated);
    let b = &list.models[0];
    assert_eq!(
        (b.name.as_deref(), b.input_tokens, b.output_tokens, b.thinking, b.vision),
        (Some("Claude B"), Some(1_000_000), Some(128_000), Some(true), Some(true))
    );
    let velho = &list.models[2];
    assert_eq!((velho.input_tokens, velho.output_tokens, velho.thinking), (None, None, None));

    let reqs = cap.lock().expect("lock").clone();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].path(), "/v1/models?limit=1000");
    assert_eq!(reqs[1].path(), "/v1/models?limit=1000&after_id=claude-a");
    for req in &reqs {
        assert_eq!(req.header("x-api-key").as_deref(), Some(KEY));
        assert_eq!(req.header("anthropic-version").as_deref(), Some("2023-06-01"));
        assert!(req.header("authorization").is_none(), "{}", req.head);
    }

    assert_eq!(anthropic::known_max_tokens(conn.endpoint(), "claude-b"), Some(128_000));
    assert_eq!(anthropic::known_max_tokens(conn.endpoint(), "claude-a"), Some(64_000));
    assert_eq!(anthropic::known_max_tokens(conn.endpoint(), "claude-velho"), None);
    assert_eq!(known_input_window(conn.endpoint(), "claude-b"), Some(1_000_000));
    assert_eq!(known_input_window(&format!("{base}/"), "claude-a"), Some(200_000), "barra final igual");
    assert_eq!(known_input_window("http://outro.example.com", "claude-a"), None, "por endereço");

    let json = serde_json::to_value(&list.models[0]).expect("json");
    assert_eq!(json, json!({"id":"claude-b","name":"Claude B","input_tokens":1000000,"output_tokens":128000,"thinking":true,"vision":true}));
}

const GEMINI_PAGE_1: &str = r#"{"models":[
  {"name":"models/gemini-9-flash","displayName":"Gemini 9 Flash","inputTokenLimit":1048576,"outputTokenLimit":65536,
   "supportedGenerationMethods":["generateContent","countTokens"],"thinking":true},
  {"name":"models/gemini-embedding-9","inputTokenLimit":8192,"outputTokenLimit":1,"supportedGenerationMethods":["embedContent"]},
  {"name":"models/aqa","inputTokenLimit":7168,"outputTokenLimit":1024,"supportedGenerationMethods":["generateAnswer"]},
  {"name":"models/gemini-9-flash-tts","inputTokenLimit":8192,"outputTokenLimit":16384,"supportedGenerationMethods":["generateContent","countTokens"]},
  {"name":"models/gemini-9-flash-image","inputTokenLimit":65536,"outputTokenLimit":65536,"supportedGenerationMethods":["generateContent"]},
  {"name":"models/nano-banana-9","supportedGenerationMethods":["generateContent"]},
  {"name":"models/lyria-9","supportedGenerationMethods":["generateContent"]},
  {"name":"models/gemini-9-live","supportedGenerationMethods":["bidiGenerateContent"]}
 ],"nextPageToken":"Pag/2+x="}"#;
const GEMINI_PAGE_2: &str = r#"{"models":[
  {"name":"models/gemma-9-it","displayName":"Gemma 9","inputTokenLimit":262144,"outputTokenLimit":32768,
   "supportedGenerationMethods":["generateContent"]},
  {"name":"models/gemini-9-flash","supportedGenerationMethods":["generateContent"]}
 ]}"#;

#[tokio::test]
async fn gemini_filtra_os_de_texto_tira_o_prefixo_e_segue_o_token() {
    let (base, cap) = spawn(vec![response(200, GEMINI_PAGE_1), response(200, GEMINI_PAGE_2)]).await;
    let conn = native(ConnectionKind::GoogleGemini, &base);
    let list = fetch(&conn, &base, TIMEOUT).await.expect("lista");
    assert_eq!(ids(&list), ["gemini-9-flash", "gemma-9-it"], "sem embeddings, AQA, voz, imagem, música e ao vivo");
    let flash = &list.models[0];
    assert_eq!(
        (flash.name.as_deref(), flash.input_tokens, flash.output_tokens, flash.thinking, flash.vision),
        (Some("Gemini 9 Flash"), Some(1_048_576), Some(65_536), Some(true), None)
    );

    let reqs = cap.lock().expect("lock").clone();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].path(), "/v1beta/models?pageSize=1000");
    assert_eq!(reqs[1].path(), "/v1beta/models?pageSize=1000&pageToken=Pag%2F2%2Bx%3D");
    for req in &reqs {
        assert_eq!(req.header("x-goog-api-key").as_deref(), Some(KEY));
        assert!(req.header("authorization").is_none() && req.header("x-api-key").is_none(), "{}", req.head);
    }
    assert_eq!(gemini::known_max_tokens(conn.endpoint(), "gemini-9-flash"), Some(65_536));
    assert_eq!(gemini::known_max_tokens(conn.endpoint(), "gemma-9-it"), Some(32_768));
    assert_eq!(gemini::known_max_tokens(conn.endpoint(), "gemini-9-flash-tts"), None, "filtrado fica fora");
    assert_eq!(known_input_window(conn.endpoint(), "gemma-9-it"), Some(262_144));
}

#[test]
fn paginas_malformadas_sao_resposta_ruim_e_lista_vazia_vale() {
    assert_eq!(anthropic_page(&json!({"models": []}), KEY), None);
    assert_eq!(anthropic_page(&json!({"data": [{"id": 7}]}), KEY), None);
    assert_eq!(anthropic_page(&json!({"data": [], "has_more": false}), KEY), Some(Page::default()));
    assert_eq!(gemini_page(&json!([]), KEY), None);
    assert_eq!(gemini_page(&json!({"models": "x"}), KEY), None);
    assert_eq!(gemini_page(&json!({}), KEY), Some(Page::default()));
    // O provedor nunca devolve a chave como id ou nome.
    let echo = json!({"data": [{"id": format!("x-{KEY}")}, {"id": "ok", "display_name": KEY}]});
    let page = anthropic_page(&echo, KEY).expect("página");
    assert_eq!(page.models.len(), 1);
    assert_eq!((page.models[0].id.as_str(), page.models[0].name.as_deref()), ("ok", None));
}

#[tokio::test]
async fn cursor_repetido_para_a_paginacao() {
    let page = r#"{"data":[{"id":"claude-a"}],"has_more":true,"last_id":"claude-a"}"#;
    let (base, cap) = spawn(vec![response(200, page), response(200, page), response(200, page)]).await;
    let conn = native(ConnectionKind::Anthropic, &base);
    let list = fetch(&conn, &base, TIMEOUT).await.expect("lista");
    assert_eq!(ids(&list), ["claude-a"]);
    assert_eq!(cap.lock().expect("lock").len(), 2, "o mesmo cursor não é pedido de novo");
}

/// Spec 132 T09: a paginação que esgota o `MAX_PAGES` com o provedor ainda indicando outra página
/// marca a lista como truncada; a que termina na última página, não.
#[tokio::test]
async fn paginas_esgotadas_com_mais_por_vir_marcam_truncada() {
    let page = |n: usize| response(200, &format!(r#"{{"data":[{{"id":"claude-{n}"}}],"has_more":true,"last_id":"claude-{n}"}}"#));
    let (base, cap) = spawn((0..MAX_PAGES).map(page).collect()).await;
    let conn = native(ConnectionKind::Anthropic, &base);
    let list = fetch(&conn, &base, TIMEOUT).await.expect("lista");
    assert_eq!(cap.lock().expect("lock").len(), MAX_PAGES);
    assert_eq!(list.models.len(), MAX_PAGES);
    assert!(list.truncated, "o provedor ainda tinha páginas");

    let mut pages: Vec<String> = (0..MAX_PAGES - 1).map(page).collect();
    pages.push(response(200, r#"{"data":[{"id":"claude-fim"}],"has_more":false,"last_id":"claude-fim"}"#));
    let (base, _) = spawn(pages).await;
    let conn = native(ConnectionKind::Anthropic, &base);
    let list = fetch(&conn, &base, TIMEOUT).await.expect("lista");
    assert_eq!(list.models.len(), MAX_PAGES);
    assert!(!list.truncated, "a última página disse que acabou");
}

/// Spec 132 T09: o `max_tokens` aprendido por recusa prevalece sobre o da lista de modelos, que não o
/// aumenta no mesmo processo (só o baixa, se informar um menor).
#[tokio::test]
async fn lista_nao_aumenta_o_max_tokens_aprendido() {
    let (base, _) = spawn(vec![response(200, ANTHROPIC_PAGE_1), response(200, ANTHROPIC_PAGE_2)]).await;
    let conn = native(ConnectionKind::Anthropic, &base);
    anthropic::learn_max_tokens(conn.endpoint(), "claude-b", 8_192);
    anthropic::learn_max_tokens(conn.endpoint(), "claude-a", 100_000);
    fetch(&conn, &base, TIMEOUT).await.expect("lista");
    assert_eq!(anthropic::known_max_tokens(conn.endpoint(), "claude-b"), Some(8_192), "a lista dizia 128000");
    assert_eq!(anthropic::known_max_tokens(conn.endpoint(), "claude-a"), Some(64_000), "a lista diz um menor");
    // Sem recusa, a lista recarregada vale como sempre.
    anthropic::remember_max_tokens(conn.endpoint(), "claude-velho", 4_096);
    anthropic::remember_max_tokens(conn.endpoint(), "claude-velho", 16_384);
    assert_eq!(anthropic::known_max_tokens(conn.endpoint(), "claude-velho"), Some(16_384));
}

/// Corpo de erro da Anthropic, ecoando a chave na mensagem.
fn anthropic_error(kind: &str) -> String {
    format!(r#"{{"type":"error","error":{{"type":"{kind}","message":"recebida {KEY}"}},"request_id":"req_1"}}"#)
}

/// Corpo de erro do Gemini, ecoando a chave na mensagem.
fn gemini_error(code: u16, status: &str, reason: &str) -> String {
    format!(
        r#"{{"error":{{"code":{code},"message":"recebida {KEY}","status":"{status}","details":[{{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"{reason}"}}]}}}}"#
    )
}

/// Cada erro do provedor no teste com modelo e no teste pela lista: o código fixo, sem a chave.
#[tokio::test]
async fn cada_erro_vira_codigo_fixo_sem_a_chave() {
    let spend = format!(
        r#"{{"type":"error","error":{{"type":"rate_limit_error","message":"enforced_spend_limit_reached: {KEY}"}}}}"#
    );
    let gemini_key = r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#;
    let gemini_daily = format!(r#"{{"error":{{"code":"quota_exceeded","message":"cota do dia {KEY}"}}}}"#);
    let gemini_404 = format!(r#"{{"error":{{"message":"Model 'x' not found {KEY}","code":"not_found"}}}}"#);
    let cases: Vec<(ConnectionKind, u16, String, &str, &str)> = vec![
        (ConnectionKind::Anthropic, 401, anthropic_error("authentication_error"), "auth_invalid", "auth_invalid"),
        (ConnectionKind::Anthropic, 403, anthropic_error("permission_error"), "forbidden", "forbidden"),
        (ConnectionKind::Anthropic, 404, anthropic_error("not_found_error"), "model_not_found", "unsupported"),
        (ConnectionKind::Anthropic, 429, anthropic_error("rate_limit_error"), "rate_limited", "rate_limited"),
        (ConnectionKind::Anthropic, 429, spend.clone(), "spend_limit", "spend_limit"),
        (ConnectionKind::GoogleGemini, 400, gemini_key.to_string(), "auth_invalid", "auth_invalid"),
        (ConnectionKind::GoogleGemini, 401, gemini_error(401, "UNAUTHENTICATED", "x"), "auth_invalid", "auth_invalid"),
        (ConnectionKind::GoogleGemini, 403, gemini_error(403, "PERMISSION_DENIED", "x"), "forbidden", "forbidden"),
        (ConnectionKind::GoogleGemini, 404, gemini_404, "model_not_found", "unsupported"),
        (ConnectionKind::GoogleGemini, 429, gemini_error(429, "RESOURCE_EXHAUSTED", "x"), "rate_limited", "rate_limited"),
        // Spec 132 T09: a cota diária do Gemini tem o código próprio.
        (ConnectionKind::GoogleGemini, 429, gemini_daily.clone(), "daily_quota", "daily_quota"),
    ];
    for (kind, status, body, with_model, listing) in cases {
        let (base, cap) = spawn(vec![response(status, &body), response(status, &body)]).await;
        let conn = native(kind, &base);
        let result = run_handshake_with(&conn, "modelo-x".into(), TIMEOUT).await;
        let text = serde_json::to_string(&result).expect("json");
        assert_eq!((result.ok, result.error, result.status), (false, Some(with_model), None), "{kind:?} {status}: {text}");
        let result = run_models_probe_with(&conn, TIMEOUT).await;
        let text2 = serde_json::to_string(&result).expect("json");
        assert_eq!((result.ok, result.error, result.status), (false, Some(listing), None), "{kind:?} {status}: {text2}");
        for t in [text, text2] {
            assert!(!t.contains(KEY) && !t.contains("recebida"), "{t}");
        }
        assert_eq!(cap.lock().expect("lock").len(), 2, "{kind:?} {status}: uma chamada por teste, sem novas tentativas");
    }
}

#[test]
fn classificacao_mantem_o_vocabulario_de_sempre_fora_dos_codigos_novos() {
    let http = |status: u16, body: &str| LlmError::Http { status, body_snippet: body.into(), code: None };
    assert_eq!(classify(&http(400, "campo recusado"), true).code, "http_error");
    assert_eq!(classify(&http(400, "campo recusado"), true).status, Some(400));
    assert_eq!(classify(&http(500, ""), false).code, "http_error");
    assert_eq!(classify(&http(429, "Spend limit reached for the org"), false), SPEND_LIMIT);
    assert_eq!(classify(&LlmError::Timeout, true).code, "timeout");
    assert_eq!(classify(&LlmError::Config("x".into()), true).code, "invalid_config");
}

const ANTHROPIC_PONG: &str = r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-b","content":[{"type":"text","text":"pong"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":1}}"#;
const GEMINI_PONG: &str = r#"{"status":"completed","usage":{"total_input_tokens":3,"total_output_tokens":1},"steps":[{"content":[{"text":"pong","type":"text"}],"type":"model_output"}],"object":"interaction"}"#;

/// O teste de uma conexão nativa ainda não salva: o tipo no corpo, a chave e o endereço avançado
/// (aqui, o servidor falso). Sem modelo, a lista; com modelo, a chamada mínima pelo adaptador.
#[tokio::test]
async fn conexao_nativa_nao_salva_testa_pelo_tipo_do_corpo() {
    let state = state_with(Vec::new());
    for (kind, list_body, pong, path) in [
        ("anthropic", ANTHROPIC_PAGE_2, ANTHROPIC_PONG, "/v1/messages"),
        ("google_gemini", GEMINI_PAGE_2, GEMINI_PONG, "/v1beta/interactions"),
    ] {
        let (base, cap) = spawn(vec![response(200, list_body), response(200, pong)]).await;
        let body = json!({"kind": kind, "api_key": KEY, "base_url_override": base, "base_url": "https://ignorado.example.com"});
        let (result, text) = post_test(&state, body.clone()).await.expect("ok");
        assert_eq!((result["ok"].clone(), result["models_count"].clone()), (json!(true), json!(2)), "{kind}: {text}");
        // Spec 132 T10: a lista vem junto, na ordem do provedor, para a tela antes de salvar.
        let ids: Vec<&str> = result["models"].as_array().expect("models").iter().filter_map(|m| m["id"].as_str()).collect();
        assert_eq!(ids.len(), 2, "{kind}: {text}");
        assert!(!text.contains(KEY), "{kind}: a chave não volta");
        let mut with_model = body;
        with_model["model"] = json!("modelo-x");
        let (result, text) = post_test(&state, with_model).await.expect("ok");
        assert_eq!(result["ok"], true, "{kind}: {text}");
        let reqs = cap.lock().expect("lock").clone();
        assert_eq!(reqs.len(), 2, "{kind}");
        assert_eq!(reqs[1].path(), path, "{kind}");
        assert!(reqs[1].body.contains("modelo-x"), "{kind}: {}", reqs[1].body);
    }
    assert!(state.config().read().await.connections.is_empty(), "o teste não salva nada");

    // Validação local, antes da rede.
    let cases = [
        (json!({"kind": "anthropic"}), "api_key_required"),
        (json!({"kind": "azure", "api_key": KEY}), "connection_kind_invalid"),
        (json!({"kind": "google_gemini", "api_key": KEY, "base_url_override": "ftp://h.example.com"}), "base_url_override_invalid"),
        (json!({"base_url": "https://h.example.com/v1", "api_key": KEY, "base_url_override": "https://p.example.com"}), "base_url_override_not_allowed"),
        (json!({"kind": "anthropic", "api_key": "env:1INVALIDA"}), "api_key_env_invalid"),
    ];
    for (body, code) in cases {
        let err = post_test(&state, body.clone()).await.expect_err("recusa");
        assert_eq!(err, ApiErrorCode(StatusCode::UNPROCESSABLE_ENTITY, Some(code)), "{body}");
    }
}

/// A conexão nativa salva: o teste vai ao endereço avançado dela (o `endpoint()`), com a chave salva;
/// o tipo não muda pelo corpo, e outro endereço exige a chave.
#[tokio::test]
async fn conexao_nativa_salva_usa_o_endpoint_e_a_chave_salva() {
    let (base, cap) = spawn(vec![response(200, GEMINI_PAGE_2)]).await;
    let state = state_with(vec![native(ConnectionKind::GoogleGemini, &base)]);
    let (result, text) = post_test(&state, json!({"id": "nat"})).await.expect("ok");
    assert_eq!(result["models_count"], 2, "{text}");
    assert_eq!(cap.lock().expect("lock")[0].header("x-goog-api-key").as_deref(), Some(KEY));

    let err = post_test(&state, json!({"id": "nat", "kind": "anthropic"})).await.expect_err("tipo fixo");
    assert_eq!(err, ApiErrorCode(StatusCode::UNPROCESSABLE_ENTITY, Some("connection_kind_fixed")));
    let err = post_test(&state, json!({"id": "nat", "base_url_override": "https://outro.example.com"})).await.expect_err("chave");
    assert_eq!(err, ApiErrorCode(StatusCode::UNPROCESSABLE_ENTITY, Some("api_key_required_other_url")));
}

/// A rota da lista de modelos de uma conexão nativa salva: os campos novos e o cache.
#[tokio::test]
async fn rota_da_lista_de_modelos_da_nativa() {
    let (base, cap) = spawn(vec![response(200, ANTHROPIC_PAGE_2)]).await;
    let state = state_with(vec![native(ConnectionKind::Anthropic, &base)]);
    let get = || {
        Request::builder()
            .method(Method::GET)
            .uri("/api/config/connections/nat/models")
            .header(header::HOST, "127.0.0.1:3000")
            .body(Body::empty())
            .expect("req")
    };
    for _ in 0..2 {
        let resp = crate::server::router(state.clone()).oneshot(get()).await.expect("resp");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("corpo");
        let v: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(v["models"], json!([{"id":"claude-velho","name":"Claude Velho"},{"id":"claude-a","name":"Claude A repetido"}]), "{v}");
        assert!(v.get("error").is_none(), "{v}");
    }
    assert_eq!(cap.lock().expect("lock").len(), 1, "a segunda vem do cache");
}

#[test]
fn janela_de_entrada_por_endereco_e_modelo() {
    remember_input_window("https://janela.example.com/", "m", 300_000);
    remember_input_window("https://janela.example.com", "zero", 0);
    assert_eq!(known_input_window("https://janela.example.com", "m"), Some(300_000));
    assert_eq!(known_input_window("https://janela.example.com", "zero"), None);
    assert_eq!(known_input_window("https://janela.example.com", "outro"), None);
}

/// Ao vivo (opcional, sem gasto de tokens): lista os modelos nos dois provedores com as chaves do
/// usuário, lidas dos arquivos em `NANTS_LIVE_ANTHROPIC_KEY_FILE` e `NANTS_LIVE_GEMINI_KEY_FILE` e
/// nunca impressas.
/// `NANTS_LIVE_ANTHROPIC_KEY_FILE=<arq> NANTS_LIVE_GEMINI_KEY_FILE=<arq> scripts/test.sh cargo test --lib native_models_live -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "chama as APIs reais da Anthropic e do Gemini"]
async fn native_models_live() {
    for (var, kind) in [
        ("NANTS_LIVE_ANTHROPIC_KEY_FILE", ConnectionKind::Anthropic),
        ("NANTS_LIVE_GEMINI_KEY_FILE", ConnectionKind::GoogleGemini),
    ] {
        let Ok(path) = std::env::var(var) else {
            panic!("defina {var} com o caminho da chave");
        };
        let key = std::fs::read_to_string(path).expect("chave").trim().to_string();
        let conn = Connection::native("live", "live", kind, key);
        let list = fetch(&conn, conn.endpoint(), Duration::from_secs(30)).await.expect("lista ao vivo");
        assert!(!list.models.is_empty(), "{kind:?}");
        assert!(list.models.iter().all(|m| !m.id.starts_with("models/")), "{kind:?}");
        let with_limits = list.models.iter().filter(|m| m.output_tokens.is_some() && m.input_tokens.is_some()).count();
        let ids: Vec<&str> = list.models.iter().map(|m| m.id.as_str()).collect();
        println!("{kind:?}: {} modelos, {with_limits} com limites: {ids:?}", list.models.len());
        if kind == ConnectionKind::GoogleGemini {
            for bad in ["embedding", "tts", "image", "aqa"] {
                assert!(!ids.iter().any(|id| id.split('-').any(|p| p == bad)), "{bad}: {ids:?}");
            }
        }
        let first = &list.models[0];
        let known = match kind {
            ConnectionKind::GoogleGemini => gemini::known_max_tokens(conn.endpoint(), &first.id),
            _ => anthropic::known_max_tokens(conn.endpoint(), &first.id),
        };
        assert_eq!(known.map(u64::from), first.output_tokens, "{kind:?}");
    }
}
