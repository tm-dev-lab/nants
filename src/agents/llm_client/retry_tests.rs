//! Testes da repetição automática de falhas transitórias do provedor (spec 079, decisão 4).

use super::*;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const OK_CHAT: &str = r#"{"choices":[{"message":{"role":"assistant","content":"olá"},"finish_reason":"stop"}]}"#;
const KEY: &str = "sk-test-RETRY";

fn req() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        messages: vec![ChatMessage::text(Role::User, "oi")],
        tools: Vec::new(),
        effort: None,
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    }
}

/// Resposta HTTP bruta com corpo de tamanho conhecido.
pub(super) fn http(status: u16, extra: &str, ctype: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Lê cabeçalhos e corpo (Content-Length) de uma requisição.
async fn drain_request(sock: &mut TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let Ok(n) = sock.read(&mut tmp).await else { return };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some((head, rest)) = text.split_once("\r\n\r\n") {
            let len = head
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
                .unwrap_or(0);
            if rest.len() >= len {
                return;
            }
        }
    }
}

/// Servidor que responde, em ordem, cada resposta bruta (uma por conexão) e conta as requisições.
async fn serve_on(listener: TcpListener, responses: Vec<String>, count: Arc<Mutex<usize>>) {
    for raw in responses {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        drain_request(&mut sock).await;
        *count.lock().expect("lock") += 1;
        let _ = sock.write_all(raw.as_bytes()).await;
        let _ = sock.shutdown().await;
    }
}

pub(super) async fn server(responses: Vec<String>) -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let count = Arc::new(Mutex::new(0usize));
    tokio::spawn(serve_on(listener, responses, Arc::clone(&count)));
    (format!("http://{addr}/v1"), count)
}

/// Espera que só registra a duração pedida (zero de espera real).
fn recorder() -> (RetrySleep, Arc<Mutex<Vec<Duration>>>) {
    let waits = Arc::new(Mutex::new(Vec::new()));
    let w = Arc::clone(&waits);
    let sleep: RetrySleep = Arc::new(move |d| {
        w.lock().expect("lock").push(d);
        Box::pin(async {})
    });
    (sleep, waits)
}

fn client(base: String, sleep: RetrySleep) -> LlmClient {
    LlmClient::new(base, KEY.into()).expect("client").with_retry_sleep(sleep)
}

fn secs(v: &[u64]) -> Vec<Duration> {
    v.iter().map(|s| Duration::from_secs(*s)).collect()
}

#[tokio::test]
async fn connection_refused_then_success_is_retried() {
    // Porta livre sem ninguém escutando: a primeira tentativa é recusada. A espera sobe o servidor.
    let probe = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = probe.local_addr().expect("addr");
    drop(probe);
    let count = Arc::new(Mutex::new(0usize));
    let waits = Arc::new(Mutex::new(Vec::new()));
    let (c, w) = (Arc::clone(&count), Arc::clone(&waits));
    let sleep: RetrySleep = Arc::new(move |d| {
        w.lock().expect("lock").push(d);
        let c = Arc::clone(&c);
        Box::pin(async move {
            if let Ok(l) = TcpListener::bind(addr).await {
                tokio::spawn(serve_on(l, vec![http(200, "", "application/json", OK_CHAT)], c));
            }
        })
    });
    let got = client(format!("http://{addr}/v1"), sleep).chat(&req()).await.expect("sucesso após repetir");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*waits.lock().expect("lock"), secs(&[2]));
    assert_eq!(*count.lock().expect("lock"), 1);
}

#[tokio::test]
async fn http_503_then_success_is_retried_with_growing_delay() {
    let (base, count) = server(vec![
        http(503, "", "application/json", "{}"),
        http(503, "", "application/json", "{}"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (sleep, waits) = recorder();
    let got = client(base, sleep).chat(&req()).await.expect("ok");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 3);
    assert_eq!(*waits.lock().expect("lock"), secs(&[2, 8]));
}

#[tokio::test]
async fn config_errors_are_not_retried() {
    for status in [400u16, 401, 403, 404] {
        let (base, count) = server(vec![
            http(status, "", "application/json", r#"{"error":"x"}"#),
            http(200, "", "application/json", OK_CHAT),
        ])
        .await;
        let (sleep, waits) = recorder();
        let err = client(base, sleep).chat(&req()).await.expect_err("sem repetição");
        assert!(matches!(err, LlmError::Http { status: s, .. } if s == status), "{err:?}");
        assert_eq!(*count.lock().expect("lock"), 1, "{status}");
        assert!(waits.lock().expect("lock").is_empty(), "{status}");
    }
}

#[tokio::test]
async fn http_503_respects_retry_after_with_cap() {
    // 5xx segue o teto de 60 s; o 429 tem o cronograma de limite (spec 089, testes abaixo).
    let (base, _) = server(vec![
        http(503, "Retry-After: 5\r\n", "application/json", "{}"),
        http(503, "Retry-After: 600\r\n", "application/json", "{}"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (sleep, waits) = recorder();
    client(base, sleep).chat(&req()).await.expect("ok");
    assert_eq!(*waits.lock().expect("lock"), secs(&[5, 60]));
}

#[tokio::test]
async fn exhausted_retries_return_the_original_error() {
    let (base, count) = server(vec![
        http(502, "", "application/json", "primeira"),
        http(503, "", "application/json", "{}"),
        http(504, "", "application/json", "{}"),
        http(500, "", "application/json", "{}"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (sleep, waits) = recorder();
    let err = client(base, sleep).chat(&req()).await.expect_err("esgotado");
    assert!(matches!(&err, LlmError::Http { status: 502, body_snippet, .. } if body_snippet == "primeira"), "{err:?}");
    assert_eq!(*count.lock().expect("lock"), 4, "1 + 3 novas tentativas");
    assert_eq!(*waits.lock().expect("lock"), secs(&[2, 8, 30]));
}

#[tokio::test]
async fn retry_keeps_400_fallbacks_working() {
    // 503, depois 400 recusando reasoning_effort, depois sucesso: o fallback continua valendo.
    let (base, count) = server(vec![
        http(503, "", "application/json", "{}"),
        http(400, "", "application/json", r#"{"error":{"message":"Unsupported parameter: reasoning_effort"}}"#),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (sleep, waits) = recorder();
    let mut r = req();
    r.effort = Some(Effort::High);
    let got = client(base, sleep).chat(&r).await.expect("ok");
    assert!(got.fallback_used);
    assert_eq!(*count.lock().expect("lock"), 3);
    assert_eq!(waits.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn stream_503_before_any_delta_is_retried() {
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"olá\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let (base, count) = server(vec![
        http(503, "Retry-After: 1\r\n", "application/json", "{}"),
        http(200, "", "text/event-stream", sse),
    ])
    .await;
    let (sleep, waits) = recorder();
    let deltas = Mutex::new(Vec::new());
    let got = client(base, sleep)
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect("ok");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(*waits.lock().expect("lock"), secs(&[1]));
    assert_eq!(deltas.lock().expect("lock").len(), 1);
}

/// Corpo chunked que entrega `chunk` e cai (sem o pedaço final): erro de transporte no meio.
fn cut_after(chunk: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{chunk}\r\n",
        chunk.len()
    )
}

/// Spec 127 T01: a conexão que cai depois do primeiro delta conta como resposta ruim (`transport`):
/// o mesmo pedido é reenviado, com a espera das respostas ruins e o aviso de nova tentativa.
#[tokio::test]
async fn stream_failure_after_delta_is_retried_as_bad_response() {
    let broken = cut_after("data: {\"choices\":[{\"delta\":{\"content\":\"ol\"}}]}\n\n");
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"olá\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let (base, count) = server(vec![broken, http(200, "", "text/event-stream", sse)]).await;
    let (sleep, waits) = recorder();
    let deltas = Mutex::new(Vec::new());
    let got = client(base, sleep)
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect("sucesso na nova tentativa");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(*waits.lock().expect("lock"), secs(&[2]));
    let notice = retry_notice(&BadReason::new(BadKind::Transport), 1);
    assert_eq!(
        *deltas.lock().expect("lock"),
        vec![StreamDelta::Text("ol".into()), StreamDelta::Retry(notice), StreamDelta::Text("olá".into())]
    );
}

/// Spec 127 T01: o limite é o das respostas ruins ([`BAD_RESPONSE_RETRIES`]); esgotado, o erro de
/// transporte sobe.
#[tokio::test]
async fn stream_failure_after_delta_stops_at_the_bad_response_limit() {
    let broken = cut_after("data: {\"choices\":[{\"delta\":{\"content\":\"ol\"}}]}\n\n");
    let (base, count) = server(vec![broken.clone(), broken.clone(), broken, http(200, "", "application/json", OK_CHAT)]).await;
    let (sleep, waits) = recorder();
    let err = client(base, sleep).chat_stream(&req(), &|_| {}).await.expect_err("esgotado");
    assert!(matches!(err, LlmError::Transport(_)), "{err:?}");
    assert_eq!(*count.lock().expect("lock"), 1 + BAD_RESPONSE_RETRIES);
    assert_eq!(*waits.lock().expect("lock"), secs(&[2, 8]));
}

/// Spec 127 T01: a chamada de ferramenta que veio pela metade na resposta cortada não sobra: a
/// resposta final tem só a da nova tentativa (nada se duplica).
#[tokio::test]
async fn stream_cut_mid_tool_call_does_not_duplicate_the_call() {
    let broken = cut_after(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"vou\",\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"write_sdd_artifacts\",\"arguments\":\"{\\\"a\"}}]}}]}\n\n",
    );
    let sse = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"vou\",\"tool_calls\":[{\"index\":0,\"id\":\"c2\",\"function\":{\"name\":\"write_sdd_artifacts\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (base, count) = server(vec![broken, http(200, "", "text/event-stream", sse)]).await;
    let (sleep, _) = recorder();
    let got = client(base, sleep).chat_stream(&req(), &|_| {}).await.expect("ok");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(got.message.content, "vou");
    assert_eq!(got.message.tool_calls.len(), 1, "sem duplicar a chamada");
    assert_eq!(got.message.tool_calls[0].id, "c2");
    assert_eq!(got.message.tool_calls[0].arguments, "{}");
}

#[tokio::test]
async fn stream_transport_failure_before_delta_is_retried() {
    let broken = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_string();
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"olá\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let (base, count) = server(vec![broken, http(200, "", "text/event-stream", sse)]).await;
    let (sleep, waits) = recorder();
    let got = client(base, sleep).chat_stream(&req(), &|_| {}).await.expect("ok");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(*waits.lock().expect("lock"), secs(&[2]));
}

/// Spec 114 T04: bytes recebidos sem texto (só `Received`) não impedem a nova tentativa.
#[tokio::test]
async fn stream_failure_after_only_received_bytes_is_retried() {
    let chunk = ": keep-alive\n\n";
    let broken = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{chunk}\r\n",
        chunk.len()
    );
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"olá\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let (base, count) = server(vec![broken, http(200, "", "text/event-stream", sse)]).await;
    let (sleep, _) = recorder();
    let sizes = Mutex::new(0usize);
    let got = client(base, sleep)
        .chat_stream(&req(), &|d| {
            if matches!(d, StreamDelta::Received(_)) {
                *sizes.lock().expect("lock") += 1;
            }
        })
        .await
        .expect("ok");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert!(*sizes.lock().expect("lock") >= 2);
}

#[test]
fn transient_classification() {
    assert!(is_transient(&LlmError::Transport("dns".into())));
    assert!(is_transient(&LlmError::Timeout));
    for s in [429u16, 500, 502, 503, 504] {
        assert!(is_transient(&LlmError::Http { status: s, body_snippet: String::new(), code: None }), "{s}");
    }
    for s in [400u16, 401, 403, 404, 422, 501] {
        assert!(!is_transient(&LlmError::Http { status: s, body_snippet: String::new(), code: None }), "{s}");
    }
    assert!(!is_transient(&LlmError::Parse(BadReason::new(BadKind::InvalidJson))));
    assert!(!is_transient(&LlmError::Config("x".into())));
}

#[tokio::test]
async fn default_retry_waits_are_cancelable() {
    // Espera real (tokio::time::sleep) descartada pelo chamador: não bloqueia o runtime.
    let probe = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = probe.local_addr().expect("addr");
    drop(probe);
    let real: RetrySleep = Arc::new(|d| Box::pin(tokio::time::sleep(d)));
    let c = client(format!("http://{addr}/v1"), real);
    let r = tokio::time::timeout(Duration::from_millis(300), c.chat(&req())).await;
    assert!(r.is_err(), "a espera de 2 s foi interrompida pelo timeout do chamador");
}

// ---- Timeout do streaming (spec 080 T01) ----
//
// Causa do `provider_timeout` sem repetições: o timeout do cliente reqwest (120 s) é total e valia
// também para a leitura do corpo SSE. Uma resposta que entregou um delta logo no início (ex.:
// resumo de reasoning) e ficou quieta pensando era cortada aos 120 s do início, ~116 s depois do
// último delta; como já havia delta, a regra da 079 subia o erro sem repetir. Agora, no
// streaming, o timeout vale como ociosidade (até os cabeçalhos e entre pedaços), com teto total
// `STREAM_MAX_DURATION`.

/// Passo do servidor falso: escreve `prefix` e segura a conexão sem mais bytes, ou responde.
enum Step {
    Hold(String),
    Reply(String),
}

async fn scripted_server(steps: Vec<Step>) -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let count = Arc::new(Mutex::new(0usize));
    let c = Arc::clone(&count);
    tokio::spawn(async move {
        for step in steps {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            drain_request(&mut sock).await;
            *c.lock().expect("lock") += 1;
            match step {
                Step::Hold(prefix) => {
                    tokio::spawn(async move {
                        let _ = sock.write_all(prefix.as_bytes()).await;
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        drop(sock);
                    });
                }
                Step::Reply(raw) => {
                    let _ = sock.write_all(raw.as_bytes()).await;
                    let _ = sock.shutdown().await;
                }
            }
        }
    });
    (format!("http://{addr}/v1"), count)
}

const SSE_HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
const SSE_OK: &str =
    "data: {\"choices\":[{\"delta\":{\"content\":\"olá\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

fn short(base: String, sleep: RetrySleep) -> LlmClient {
    client(base, sleep).with_timeout(Duration::from_millis(300)).expect("timeout")
}

fn chunk(text: &str) -> String {
    format!("{:x}\r\n{text}\r\n", text.len())
}

#[tokio::test]
async fn stream_silent_before_headers_times_out_and_is_retried() {
    let (base, count) = scripted_server(vec![
        Step::Hold(String::new()),
        Step::Reply(http(200, "", "text/event-stream", SSE_OK)),
    ])
    .await;
    let (sleep, waits) = recorder();
    let got = short(base, sleep).chat_stream(&req(), &|_| {}).await.expect("repetido");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(*waits.lock().expect("lock"), secs(&[2]));
}

#[tokio::test]
async fn stream_idle_body_without_delta_times_out_and_is_retried() {
    // Cabeçalhos SSE chegam e o corpo fica sem bytes até estourar o timeout curto: sem delta,
    // é `Timeout` transitório e a requisição é repetida.
    let (base, count) = scripted_server(vec![
        Step::Hold(SSE_HEAD.to_string()),
        Step::Reply(http(200, "", "text/event-stream", SSE_OK)),
    ])
    .await;
    let (sleep, waits) = recorder();
    let deltas = Mutex::new(Vec::new());
    let got = short(base, sleep)
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect("repetido");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(*waits.lock().expect("lock"), secs(&[2]));
    assert_eq!(deltas.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn stream_idle_after_delta_times_out_without_retry() {
    let first = "data: {\"choices\":[{\"delta\":{\"content\":\"ol\"}}]}\n\n";
    let (base, count) = scripted_server(vec![
        Step::Hold(format!("{SSE_HEAD}{}", chunk(first))),
        Step::Reply(http(200, "", "text/event-stream", SSE_OK)),
    ])
    .await;
    let (sleep, waits) = recorder();
    let err = short(base, sleep).chat_stream(&req(), &|_| {}).await.expect_err("sobe");
    assert!(matches!(err, LlmError::Timeout), "{err:?}");
    assert_eq!(*count.lock().expect("lock"), 1);
    assert!(waits.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn live_stream_longer_than_timeout_is_not_cut() {
    // Deltas a cada 100 ms por ~700 ms com timeout de 300 ms: antes, o timeout total cortava a
    // resposta viva no meio; agora só a ociosidade conta.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        drain_request(&mut sock).await;
        let _ = sock.write_all(SSE_HEAD.as_bytes()).await;
        for _ in 0..7 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let piece = "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n";
            let _ = sock.write_all(chunk(piece).as_bytes()).await;
        }
        let end = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let _ = sock.write_all(format!("{}0\r\n\r\n", chunk(end)).as_bytes()).await;
        let _ = sock.shutdown().await;
    });
    let (sleep, waits) = recorder();
    let got = short(format!("http://{addr}/v1"), sleep)
        .chat_stream(&req(), &|_| {})
        .await
        .expect("stream vivo termina");
    assert_eq!(got.message.content, "aaaaaaa");
    assert!(waits.lock().expect("lock").is_empty());
}

// ---- Resposta ruim do provedor (spec 088) ----

const RESP_OK: &str = r#"{"status":"completed","error":null,"output":[{"type":"message","content":[{"type":"output_text","text":"olá","annotations":[]}]}]}"#;
const RESP_FILTER: &str = r#"{"status":"incomplete","incomplete_details":{"reason":"content_filter"},"error":null,"output":[]}"#;

fn responses(base: String, sleep: RetrySleep) -> LlmClient {
    client(base, sleep).with_wire_api(WireApi::Responses)
}

/// Evento SSE da Responses.
fn sse(kind: &str, data: &str) -> String {
    format!("event: {kind}\ndata: {data}\n\n")
}

fn completed() -> String {
    sse("response.completed", &format!(r#"{{"type":"response.completed","response":{RESP_OK}}}"#))
}

#[tokio::test]
async fn response_failed_then_success_is_retried() {
    let failed = sse("response.failed", r#"{"type":"response.failed","response":{"error":{"code":"server_error"}}}"#);
    let (base, count) = server(vec![
        http(200, "", "text/event-stream", &failed),
        http(200, "", "text/event-stream", &completed()),
    ])
    .await;
    let (sleep, waits) = recorder();
    let got = responses(base, sleep).chat_stream(&req(), &|_| {}).await.expect("sucesso após repetir");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(*waits.lock().expect("lock"), secs(&[2]));
}

#[tokio::test]
async fn incomplete_content_filter_three_times_fails_after_three_requests() {
    let (base, count) = server(vec![
        http(200, "", "application/json", RESP_FILTER),
        http(200, "", "application/json", RESP_FILTER),
        http(200, "", "application/json", RESP_FILTER),
        http(200, "", "application/json", RESP_OK),
    ])
    .await;
    let (sleep, waits) = recorder();
    let err = responses(base, sleep).chat(&req()).await.expect_err("esgotado");
    let failure = provider_failure(&err);
    assert_eq!(failure.to_string(), "provider_bad_response");
    assert_eq!(failure.describe(), "provider_bad_response (incompleta: content_filter)");
    assert_eq!(*count.lock().expect("lock"), 3, "1 + 2 novas tentativas");
    assert_eq!(*waits.lock().expect("lock"), secs(&[2, 8]));
}

#[tokio::test]
async fn exhausted_bad_response_returns_the_last_failure() {
    let (base, _) = server(vec![
        http(200, "", "application/json", RESP_FILTER),
        http(200, "", "application/json", "não é json"),
        http(200, "", "application/json", r#"{"status":"completed","error":{"code":"server_error"},"output":[]}"#),
    ])
    .await;
    let (sleep, _) = recorder();
    let err = responses(base, sleep).chat(&req()).await.expect_err("esgotado");
    assert_eq!(provider_failure(&err).detail().as_deref(), Some("erro do provedor: server_error"));
}

#[tokio::test]
async fn too_large_and_unsupported_status_are_not_retried() {
    let big = format!(r#"{{"choices":[{{"message":{{"content":"{}"}}}}]}}"#, "x".repeat(200));
    let (base, count) = server(vec![http(200, "", "application/json", &big), http(200, "", "application/json", OK_CHAT)]).await;
    let (sleep, waits) = recorder();
    let err = client(base, sleep).with_max_response_bytes(64).chat(&req()).await.expect_err("grande demais");
    assert_eq!(provider_failure(&err).detail().as_deref(), Some("resposta grande demais"));
    assert_eq!(*count.lock().expect("lock"), 1);
    assert!(waits.lock().expect("lock").is_empty());

    let odd = r#"{"status":"in_progress","error":null,"output":[]}"#;
    let (base, count) = server(vec![http(200, "", "application/json", odd), http(200, "", "application/json", RESP_OK)]).await;
    let (sleep, waits) = recorder();
    let err = responses(base, sleep).chat(&req()).await.expect_err("status");
    assert_eq!(provider_failure(&err).detail().as_deref(), Some("status não suportado: in_progress"));
    assert_eq!(*count.lock().expect("lock"), 1);
    assert!(waits.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn stream_bad_response_after_delta_is_retried_with_notice() {
    let partial = format!(
        "{}{}",
        sse("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"ol"}"#),
        sse("response.failed", r#"{"type":"response.failed","response":{"error":{"code":"server_error"}}}"#),
    );
    let ok = format!(
        "{}{}",
        sse("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"olá"}"#),
        completed(),
    );
    let (base, count) = server(vec![
        http(200, "", "text/event-stream", &partial),
        http(200, "", "text/event-stream", &ok),
    ])
    .await;
    let (sleep, waits) = recorder();
    let deltas = Mutex::new(Vec::new());
    let got = responses(base, sleep)
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect("repetido depois do delta");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(*waits.lock().expect("lock"), secs(&[2]));
    assert_eq!(
        *deltas.lock().expect("lock"),
        vec![
            StreamDelta::Text("ol".into()),
            StreamDelta::Retry("resposta do provedor falhou (erro do provedor: server_error); repetindo (1/2)".into()),
            StreamDelta::Text("olá".into()),
        ]
    );
}

#[tokio::test]
async fn bad_response_never_carries_remote_text() {
    const FAKE: &str = "sk-FAKE-SEGREDO-0123456789";
    let long = "m".repeat(5000);
    let body = format!(
        r#"{{"status":"failed","error":{{"code":"Erro com {FAKE} em https://api.exemplo.test/v1","type":"{long}","message":"{FAKE} {long}"}},"output":[]}}"#
    );
    let failed = sse(
        "response.failed",
        &format!(r#"{{"type":"response.failed","response":{{"error":{{"code":"{FAKE} x","message":"{FAKE} {long}"}}}}}}"#),
    );
    let (base, _) = server(vec![
        http(200, "", "application/json", &body),
        http(200, "", "text/event-stream", &failed),
        http(200, "", "application/json", &body),
    ])
    .await;
    let (sleep, _) = recorder();
    let deltas = Mutex::new(Vec::new());
    let err = responses(base, sleep)
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect_err("falha");
    let failure = provider_failure(&err);
    assert_eq!(failure.describe(), "provider_bad_response (erro do provedor)");
    let text = format!("{err:?} {err} {failure:?} {} {:?}", failure.describe(), deltas.lock().expect("lock"));
    for leak in [FAKE, "api.exemplo", "mmmmmmmmmm", "SEGREDO"] {
        assert!(!text.contains(leak), "vazou {leak}: {text}");
    }
}

// ---- Provedor no limite (spec 089 T01) ----

/// Corpo de erro do provedor (protocolo chat) com o código dado, mais texto remoto livre.
fn chat_error(code: &str) -> String {
    format!(r#"{{"error":{{"code":"{code}","message":"SEGREDO remoto {KEY}"}}}}"#)
}

/// Só os avisos de nova tentativa entregues ao consumidor dos deltas.
fn notices(deltas: &Mutex<Vec<StreamDelta>>) -> Vec<String> {
    deltas
        .lock()
        .expect("lock")
        .iter()
        .filter_map(|d| match d {
            StreamDelta::Retry(n) => Some(n.clone()),
            _ => None,
        })
        .collect()
}

fn total(waits: &Mutex<Vec<Duration>>) -> Duration {
    waits.lock().expect("lock").iter().sum()
}

#[tokio::test]
async fn every_throttle_code_is_retried_and_insufficient_quota_is_not() {
    for code in THROTTLE_CODES {
        let (base, count) = server(vec![
            http(200, "", "application/json", &chat_error(code)),
            http(200, "", "application/json", OK_CHAT),
        ])
        .await;
        let (sleep, waits) = recorder();
        let got = client(base, sleep).chat(&req()).await.expect("sucesso após o limite");
        assert_eq!(got.message.content, "olá", "{code}");
        assert_eq!(*count.lock().expect("lock"), 2, "{code}");
        assert_eq!(*waits.lock().expect("lock"), secs(&[5]), "{code}");
    }
    // Crédito esgotado: falha como erro do provedor da 088 (sem o cronograma de limite).
    let (base, count) = server(vec![
        http(200, "", "application/json", &chat_error("insufficient_quota")),
        http(200, "", "application/json", &chat_error("insufficient_quota")),
        http(200, "", "application/json", &chat_error("insufficient_quota")),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (sleep, waits) = recorder();
    let err = client(base, sleep).chat(&req()).await.expect_err("sem cronograma de limite");
    assert_eq!(provider_failure(&err).describe(), "provider_bad_response (erro do provedor: insufficient_quota)");
    assert_eq!(*count.lock().expect("lock"), 3, "só as 2 novas tentativas da 088");
    assert_eq!(*waits.lock().expect("lock"), secs(&[2, 8]));
}

#[tokio::test]
async fn throttle_sources_in_both_protocols_become_throttled() {
    let failed = sse("response.failed", r#"{"type":"response.failed","response":{"error":{"code":"no_capacity"}}}"#);
    let event = sse("error", r#"{"type":"error","code":"rate_limit_exceeded","message":"x"}"#);
    let incomplete = r#"{"status":"incomplete","incomplete_details":{"reason":"tokens_per_minute_exceeded"},"error":null,"output":[]}"#;
    let body = r#"{"status":"failed","error":{"code":"Model_Overloaded"},"output":[]}"#;
    let cases: [(String, &str, &str); 4] = [
        (failed, "text/event-stream", "no_capacity"),
        (event, "text/event-stream", "rate_limit_exceeded"),
        (incomplete.to_string(), "application/json", "tokens_per_minute_exceeded"),
        (body.to_string(), "application/json", "model_overloaded"),
    ];
    for (raw, ctype, code) in cases {
        let (base, _) = server(vec![http(200, "", ctype, &raw)]).await;
        let err = responses(base, recorder().0)
            .with_throttle_delays(Vec::new())
            .chat_stream(&req(), &|_| {})
            .await
            .expect_err("limite");
        let failure = provider_failure(&err);
        assert_eq!(failure.to_string(), "provider_bad_response", "{code}");
        assert_eq!(failure.describe(), format!("provider_bad_response (provedor no limite: {code})"));
        assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::Throttled), "{code}");
    }
    // `error` no corpo e no stream do protocolo chat.
    let chat_sse = "data: {\"error\":{\"code\":\"overloaded\",\"message\":\"x\"}}\n\n";
    let (base, _) = server(vec![
        http(200, "", "text/event-stream", chat_sse),
        http(200, "", "application/json", &chat_error("rate_limited")),
    ])
    .await;
    let c = client(base, recorder().0).with_throttle_delays(Vec::new());
    let err = c.chat_stream(&req(), &|_| {}).await.expect_err("limite");
    assert_eq!(provider_failure(&err).describe(), "provider_bad_response (provedor no limite: overloaded)");
    let err = c.chat(&req()).await.expect_err("limite");
    assert_eq!(provider_failure(&err).describe(), "provider_bad_response (provedor no limite: rate_limited)");
}

#[tokio::test]
async fn http_429_and_no_capacity_follow_the_throttle_schedule_with_notices() {
    let (base, count) = server(vec![
        http(429, "", "application/json", "{}"),
        http(200, "", "application/json", &chat_error("no_capacity")),
        http(429, "", "application/json", "{}"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (sleep, waits) = recorder();
    let deltas = Mutex::new(Vec::new());
    let got = client(base, sleep)
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect("sucesso após os limites");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 4);
    // 5 s, 15 s e 30 s (este em dois trechos de 15 s, com o aviso renovado).
    assert_eq!(*waits.lock().expect("lock"), secs(&[5, 15, 15, 15]));
    assert_eq!(
        notices(&deltas),
        vec![
            "provedor no limite (HTTP 429); aguardando 5 s (tentativa 1/6)",
            "provedor no limite (no_capacity); aguardando 15 s (tentativa 2/6)",
            "provedor no limite (HTTP 429); aguardando 30 s (tentativa 3/6)",
            "provedor no limite (HTTP 429); aguardando 15 s (tentativa 3/6)",
        ]
    );
}

#[tokio::test]
async fn long_throttle_waits_renew_the_notice_every_15_seconds() {
    let (base, _) = server(vec![
        http(429, "", "application/json", "{}"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (sleep, waits) = recorder();
    let deltas = Mutex::new(Vec::new());
    client(base, sleep)
        .with_throttle_delays(secs(&[40]))
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect("ok");
    assert_eq!(*waits.lock().expect("lock"), secs(&[15, 15, 10]));
    assert_eq!(
        notices(&deltas),
        vec![
            "provedor no limite (HTTP 429); aguardando 40 s (tentativa 1/1)",
            "provedor no limite (HTTP 429); aguardando 25 s (tentativa 1/1)",
            "provedor no limite (HTTP 429); aguardando 10 s (tentativa 1/1)",
        ]
    );
}

#[tokio::test]
async fn throttle_retry_after_seconds_cap_and_http_date() {
    let date = fmt_http_date(SystemTime::now() + Duration::from_secs(30));
    let (base, _) = server(vec![
        http(429, "Retry-After: 7\r\n", "application/json", "{}"),
        http(429, "Retry-After: 500\r\n", "application/json", "{}"),
        http(429, &format!("Retry-After: {date}\r\n"), "application/json", "{}"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (sleep, waits) = recorder();
    let deltas = Mutex::new(Vec::new());
    client(base, sleep)
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect("ok");
    let got = notices(&deltas);
    // Primeira espera: 7 s (substitui os 5 s do cronograma).
    assert_eq!(got[0], "provedor no limite (HTTP 429); aguardando 7 s (tentativa 1/6)");
    assert_eq!(waits.lock().expect("lock")[0], Duration::from_secs(7));
    // Segunda: 500 s com teto de 120 s.
    assert_eq!(got[1], "provedor no limite (HTTP 429); aguardando 120 s (tentativa 2/6)");
    // Terceira: a data HTTP vira ~30 s (a data tem resolução de segundos).
    let third = got.iter().position(|n| n.contains("tentativa 3/6")).expect("terceira espera");
    let expected = |s: u64| format!("provedor no limite (HTTP 429); aguardando {s} s (tentativa 3/6)");
    assert!((28..=30).any(|s| got[third] == expected(s)), "{got:?}");
    let all = total(&waits);
    assert!(all >= Duration::from_secs(7 + 120 + 28) && all <= Duration::from_secs(7 + 120 + 30), "{all:?}");
}

#[tokio::test]
async fn exhausted_throttle_schedule_returns_the_last_failure_after_seven_requests() {
    let mut replies: Vec<String> = (0..6).map(|i| http(429, "", "application/json", &format!("limite {i}"))).collect();
    replies.push(http(429, "", "application/json", "ultima"));
    replies.push(http(200, "", "application/json", OK_CHAT));
    let (base, count) = server(replies).await;
    let (sleep, waits) = recorder();
    let err = client(base, sleep).chat(&req()).await.expect_err("esgotado");
    assert!(matches!(&err, LlmError::Http { status: 429, body_snippet, .. } if body_snippet == "ultima"), "{err:?}");
    assert_eq!(provider_failure(&err).to_string(), "provider_http_429");
    assert_eq!(*count.lock().expect("lock"), 7, "1 + 6 novas tentativas");
    assert_eq!(total(&waits), Duration::from_secs(5 + 15 + 30 + 60 + 60 + 60));
}

#[tokio::test]
async fn stream_throttle_after_delta_is_retried_with_notice() {
    let partial = format!(
        "{}{}",
        sse("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"ol"}"#),
        sse("response.failed", r#"{"type":"response.failed","response":{"error":{"code":"no_capacity"}}}"#),
    );
    let ok = format!(
        "{}{}",
        sse("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"olá"}"#),
        completed(),
    );
    let (base, count) = server(vec![
        http(200, "", "text/event-stream", &partial),
        http(200, "", "text/event-stream", &ok),
    ])
    .await;
    let (sleep, waits) = recorder();
    let deltas = Mutex::new(Vec::new());
    let got = responses(base, sleep)
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect("repetido depois do delta");
    assert_eq!(got.message.content, "olá");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(*waits.lock().expect("lock"), secs(&[5]));
    assert_eq!(
        *deltas.lock().expect("lock"),
        vec![
            StreamDelta::Text("ol".into()),
            StreamDelta::Retry("provedor no limite (no_capacity); aguardando 5 s (tentativa 1/6)".into()),
            StreamDelta::Text("olá".into()),
        ]
    );
}

/// Espera real (`tokio::time::sleep`), para os testes de cancelamento.
fn real_sleep() -> RetrySleep {
    Arc::new(|d| Box::pin(tokio::time::sleep(d)))
}

/// Liga `flag` depois de `ms` milissegundos.
fn stop_after(flag: &Arc<AtomicBool>, ms: u64) {
    let flag = Arc::clone(flag);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        flag.store(true, Ordering::SeqCst);
    });
}

#[tokio::test]
async fn cancel_during_throttle_wait_returns_quickly() {
    let (base, count) = server(vec![
        http(429, "", "application/json", "limite"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let flag = Arc::new(AtomicBool::new(false));
    stop_after(&flag, 100);
    let started = std::time::Instant::now();
    let err = client(base, real_sleep())
        .with_cancel(Arc::clone(&flag))
        .chat(&req())
        .await
        .expect_err("parado durante a espera de 5 s");
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    assert!(matches!(err, LlmError::Http { status: 429, .. }), "{err:?}");
    assert_eq!(*count.lock().expect("lock"), 1, "nenhuma nova tentativa depois da parada");
}

#[tokio::test]
async fn cancel_scope_stops_any_retry_wait() {
    // Sinal vindo do escopo da task (como o laço do agente faz), numa espera de 5xx.
    let (base, count) = server(vec![
        http(503, "", "application/json", "{}"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let flag = Arc::new(AtomicBool::new(false));
    stop_after(&flag, 100);
    let started = std::time::Instant::now();
    let c = client(base, real_sleep()).with_retry_delays(secs(&[30]));
    let err = with_cancel_scope(Some(Arc::clone(&flag)), c.chat_stream(&req(), &|_| {}))
        .await
        .expect_err("parado");
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    assert!(matches!(err, LlmError::Http { status: 503, .. }), "{err:?}");
    assert_eq!(*count.lock().expect("lock"), 1);
    // Já parado antes da espera: nenhuma espera nem nova tentativa.
    let (base, count) = server(vec![http(429, "", "application/json", "{}"), http(200, "", "application/json", OK_CHAT)]).await;
    let (sleep, waits) = recorder();
    let c = client(base, sleep).with_cancel(Arc::new(AtomicBool::new(true)));
    c.chat(&req()).await.expect_err("parado");
    assert_eq!(*count.lock().expect("lock"), 1);
    assert!(waits.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn throttle_notices_and_errors_never_carry_remote_text() {
    const FAKE: &str = "sk-FAKE-SEGREDO-0123456789";
    let failed = sse(
        "response.failed",
        &format!(r#"{{"type":"response.failed","response":{{"error":{{"code":"no_capacity","message":"{FAKE} https://api.exemplo.test/v1"}}}}}}"#),
    );
    let (base, _) = server(vec![
        http(429, "Retry-After: 1\r\n", "application/json", &format!(r#"{{"error":{{"message":"{FAKE}"}}}}"#)),
        http(200, "", "text/event-stream", &failed),
    ])
    .await;
    let (sleep, _) = recorder();
    let deltas = Mutex::new(Vec::new());
    let err = responses(base, sleep)
        .with_throttle_delays(secs(&[5]))
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect_err("esgotado");
    let failure = provider_failure(&err);
    assert_eq!(failure.describe(), "provider_bad_response (provedor no limite: no_capacity)");
    let text = format!("{err:?} {err} {failure:?} {} {:?}", failure.describe(), deltas.lock().expect("lock"));
    for leak in [FAKE, "api.exemplo", "SEGREDO"] {
        assert!(!text.contains(leak), "vazou {leak}: {text}");
    }
}

/// Data HTTP IMF-fixdate (só para os testes).
fn fmt_http_date(at: SystemTime) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let secs = at.duration_since(UNIX_EPOCH).expect("época").as_secs();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // `civil_from_days` (inverso do usado em `http_date_secs`).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    format!(
        "{}, {d:02} {} {y} {:02}:{:02}:{:02} GMT",
        DAYS[usize::try_from(days % 7).expect("dia")],
        MONTHS[usize::try_from(m - 1).expect("mês")],
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[test]
fn retry_after_parses_seconds_and_http_dates() {
    let now = UNIX_EPOCH + Duration::from_secs(784_111_777);
    assert_eq!(http_date_secs("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784_111_777));
    assert_eq!(fmt_http_date(now), "Sun, 06 Nov 1994 08:49:37 GMT");
    assert_eq!(parse_retry_after(" 7 ", now), Some(Duration::from_secs(7)));
    assert_eq!(parse_retry_after("Sun, 06 Nov 1994 08:50:07 GMT", now), Some(Duration::from_secs(30)));
    assert_eq!(
        parse_retry_after("Thu, 29 Feb 2024 23:59:59 GMT", UNIX_EPOCH),
        Some(Duration::from_secs(1_709_251_199))
    );
    // Data passada ou inválida: vale o cronograma.
    for bad in [
        "Sun, 06 Nov 1994 08:49:37 GMT",
        "Sun, 06 Nov 1994 08:49:00 GMT",
        "amanhã",
        "-3",
        "Sun, 06 Foo 1994 08:49:37 GMT",
        "Sun, 06 Nov 1994 25:00:00 GMT",
        "Sun, 06 Nov 1994 08:49:37 UTC",
        // Fora de faixa: sem pânico, vale o cronograma.
        "Sun, 06 Nov 300000000000 08:49:37 GMT",
        "Sun, 06 Nov 18446744073709551615 08:49:37 GMT",
        "Sun, 06 Nov 01994 08:49:37 GMT",
        "Sun, 06 Nov +994 08:49:37 GMT",
        "Sun, 06 Nov 1994 99999999999999999999:00:00 GMT",
    ] {
        assert_eq!(parse_retry_after(bad, now), None, "{bad}");
    }
    // Ano máximo aceito e segundos inteiros gigantes: sem pânico (o teto é de quem usa).
    assert!(parse_retry_after("Fri, 31 Dec 9999 23:59:59 GMT", now).is_some());
    assert_eq!(parse_retry_after("18446744073709551615", now), Some(Duration::from_secs(u64::MAX)));
}

#[tokio::test]
async fn huge_retry_after_headers_fall_back_to_cap_or_schedule() {
    let (base, _) = server(vec![
        http(429, "Retry-After: 18446744073709551615\r\n", "application/json", "{}"),
        http(429, "Retry-After: Sun, 06 Nov 300000000000 08:49:37 GMT\r\n", "application/json", "{}"),
        http(429, "Retry-After: Sun, 06 Nov 18446744073709551615 08:49:37 GMT\r\n", "application/json", "{}"),
        http(503, "Retry-After: 18446744073709551615\r\n", "application/json", "{}"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (sleep, waits) = recorder();
    let deltas = Mutex::new(Vec::new());
    client(base, sleep)
        .chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d)))
        .await
        .expect("ok");
    let got = notices(&deltas);
    // Segundos gigantes: teto de 120 s; datas fora de faixa: espera do cronograma (15 s, 30 s).
    assert!(got.contains(&"provedor no limite (HTTP 429); aguardando 120 s (tentativa 1/6)".to_string()), "{got:?}");
    assert!(got.contains(&"provedor no limite (HTTP 429); aguardando 15 s (tentativa 2/6)".to_string()), "{got:?}");
    assert!(got.contains(&"provedor no limite (HTTP 429); aguardando 30 s (tentativa 3/6)".to_string()), "{got:?}");
    // 503 com segundos gigantes: teto de 60 s.
    assert_eq!(total(&waits), Duration::from_secs(120 + 15 + 30 + 60));
}

#[test]
fn throttle_notice_rounds_up_seconds() {
    assert_eq!(
        throttle_notice("HTTP 429", Duration::from_millis(4500), 2, 6),
        "provedor no limite (HTTP 429); aguardando 5 s (tentativa 2/6)"
    );
    assert_eq!(
        throttle_notice("no_capacity", Duration::ZERO, 1, 6),
        "provedor no limite (no_capacity); aguardando 0 s (tentativa 1/6)"
    );
}

/// Spec 102: as partes do aviso de limite voltam do texto (a atividade o publica com `code`).
#[test]
fn throttle_notice_parts_reads_back_the_notice() {
    let text = throttle_notice("HTTP 429", Duration::from_millis(4500), 2, 6);
    assert_eq!(throttle_notice_parts(&text), Some(("HTTP 429".to_string(), 5, 2, 6)));
    let odd = throttle_notice("x); aguardando (y", Duration::ZERO, 1, 6);
    assert_eq!(throttle_notice_parts(&odd), Some(("x); aguardando (y".to_string(), 0, 1, 6)));
    assert_eq!(throttle_notice_parts("resposta do provedor falhou (x); repetindo (1/2)"), None);
}
