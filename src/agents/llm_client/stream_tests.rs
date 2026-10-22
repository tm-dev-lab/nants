//! Testes do streaming SSE do `LlmClient`.

use super::tests::{body_of, spawn_server, KEY, RESP_TOOL};
use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SSE: &str = "text/event-stream";

/// Servidor que responde, em ordem, (status, content-type, corpo) sem Content-Length
/// (fim do corpo = fechamento da conexão). Captura as requisições.
async fn raw_server(
    responses: Vec<(u16, &'static str, String)>,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let cap = Arc::new(Mutex::new(Vec::new()));
    let cap2 = Arc::clone(&cap);
    tokio::spawn(async move {
        for (status, ctype, body) in responses {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let Ok(n) = sock.read(&mut tmp).await else {
                    break;
                };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if rest.len() >= len {
                        break;
                    }
                }
            }
            cap2.lock()
                .expect("lock")
                .push(String::from_utf8_lossy(&buf).to_string());
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nConnection: close\r\n\r\n"
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}/v1"), cap)
}

fn req(effort: Option<Effort>) -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        messages: vec![ChatMessage::text(Role::User, "oi")],
        tools: Vec::new(),
        effort,
        response_json: false,
        web_search: false,
    }
}

fn client(base: String, wire: WireApi) -> LlmClient {
    LlmClient::new(base, KEY.into())
        .expect("client")
        .with_wire_api(wire)
}

fn collector() -> (
    Arc<Mutex<Vec<StreamDelta>>>,
    impl Fn(StreamDelta) + Send + Sync,
) {
    let v = Arc::new(Mutex::new(Vec::new()));
    let v2 = Arc::clone(&v);
    (v, move |d| v2.lock().expect("lock").push(d))
}

fn ev(name: &str, data: &str) -> String {
    format!("event: {name}\r\ndata: {data}\r\n\r\n")
}

fn completed() -> String {
    format!(r#"{{"type":"response.completed","response":{RESP_TOOL}}}"#)
}

fn responses_sse() -> String {
    let mut s = String::from(": keep-alive\n\n");
    s += &ev(
        "response.reasoning_summary_text.delta",
        r#"{"type":"response.reasoning_summary_text.delta","delta":"pens"}"#,
    );
    s += &ev(
        "response.reasoning_text.delta",
        r#"{"type":"response.reasoning_text.delta","delta":"ando"}"#,
    );
    s += &ev(
        "response.output_text.delta",
        r#"{"type":"response.output_text.delta","delta":"vou ler"}"#,
    );
    // evento multi-linha: o JSON final é dividido em duas linhas data:
    let done = completed();
    let (a, b) = done.split_at(done.find(",\"response\":").unwrap_or(0) + 1);
    s += &format!("event: response.completed\ndata: {a}\ndata: {b}\n\n");
    s
}

#[tokio::test]
async fn responses_stream_emits_deltas_and_final_matches_non_streaming() {
    let (base, cap) = raw_server(vec![(200, SSE, responses_sse())]).await;
    let (seen, cb) = collector();
    let got = client(base, WireApi::Responses)
        .chat_stream(&req(Some(Effort::High)), &cb)
        .await
        .expect("ok");
    assert_eq!(
        *seen.lock().expect("lock"),
        vec![
            StreamDelta::Reasoning("pens".into()),
            StreamDelta::Reasoning("ando".into()),
            StreamDelta::Text("vou ler".into())
        ]
    );
    assert_eq!(got, parse_responses(RESP_TOOL, false).expect("parse"));
    assert_eq!(got.message.tool_calls.len(), 1);
    let body = body_of(&cap.lock().expect("lock")[0]);
    assert_eq!(body["stream"], true);
    assert_eq!(body["reasoning"], json!({"effort":"high","summary":"auto"}));
}

#[tokio::test]
async fn responses_summary_refusal_retries_once_without_summary() {
    let err = r#"{"error":{"message":"Unsupported parameter: reasoning.summary"}}"#.to_string();
    let ok = ev("response.completed", &completed());
    let (base, cap) = raw_server(vec![(400, "application/json", err), (200, SSE, ok)]).await;
    let (_, cb) = collector();
    client(base, WireApi::Responses)
        .chat_stream(&req(Some(Effort::Low)), &cb)
        .await
        .expect("ok");
    let c = cap.lock().expect("lock");
    assert_eq!(c.len(), 2);
    let second = body_of(&c[1]);
    assert_eq!(second["stream"], true);
    assert_eq!(second["reasoning"], json!({"effort":"low"}));
}

#[tokio::test]
async fn stream_refusal_falls_back_to_plain_request_once() {
    let err = r#"{"error":{"message":"stream is not supported"}}"#.to_string();
    let (base, cap) = raw_server(vec![
        (400, "application/json", err.clone()),
        (400, "application/json", err),
    ])
    .await;
    let (_, cb) = collector();
    let e = client(base, WireApi::Chat)
        .chat_stream(&req(None), &cb)
        .await
        .expect_err("falha");
    assert!(matches!(e, LlmError::Http { status: 400, .. }), "{e:?}");
    let c = cap.lock().expect("lock");
    assert_eq!(c.len(), 2);
    assert!(body_of(&c[0]).get("stream").is_some());
    assert!(body_of(&c[1]).get("stream").is_none());
}

#[tokio::test]
async fn plain_json_response_is_accepted() {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":"olá"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#;
    let (base, _) = spawn_server(vec![(200, body)]).await;
    let (seen, cb) = collector();
    let got = client(base, WireApi::Chat)
        .chat_stream(&req(None), &cb)
        .await
        .expect("ok");
    assert_eq!(got.message.content, "olá");
    assert!(seen.lock().expect("lock").is_empty());
}

fn chat_chunks() -> String {
    let mut s = String::new();
    for d in [
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"hm"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"vou "}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":"ler","tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}},{"index":1,"id":"c2","function":{"name":"x","arguments":"{}"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
        "[DONE]",
    ] {
        s += &format!("data: {d}\n\n");
    }
    s
}

#[tokio::test]
async fn chat_stream_assembles_fragmented_tool_calls_equal_to_non_streaming() {
    let (base, cap) = raw_server(vec![(200, SSE, chat_chunks())]).await;
    let (seen, cb) = collector();
    let got = client(base, WireApi::Chat)
        .chat_stream(&req(Some(Effort::Low)), &cb)
        .await
        .expect("ok");
    let plain = r#"{"choices":[{"message":{"role":"assistant","content":"vou ler","tool_calls":[{"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}},{"id":"c2","type":"function","function":{"name":"x","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#;
    assert_eq!(got, parse_response(plain, false).expect("parse"));
    assert_eq!(
        seen.lock().expect("lock")[0],
        StreamDelta::Reasoning("hm".into())
    );
    let body = body_of(&cap.lock().expect("lock")[0]);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
}

#[tokio::test]
async fn truncated_stream_is_an_error() {
    let partial = ev(
        "response.output_text.delta",
        r#"{"type":"response.output_text.delta","delta":"a"}"#,
    );
    // Spec 088: stream sem fim repete (2 novas tentativas); esgotadas, o erro sobe.
    let (base, cap) = raw_server(vec![(200, SSE, partial); 3]).await;
    let (_, cb) = collector();
    let e = client(base, WireApi::Responses)
        .chat_stream(&req(None), &cb)
        .await
        .expect_err("erro");
    assert!(
        matches!(e, LlmError::Parse(r) if r.kind == BadKind::NoFinal),
        "{e:?}"
    );
    assert_eq!(cap.lock().expect("lock").len(), 3);

    let chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n".to_string();
    let (base, _) = raw_server(vec![(200, SSE, chunk); 3]).await;
    let e = client(base, WireApi::Chat)
        .chat_stream(&req(None), &cb)
        .await
        .expect_err("erro");
    assert!(
        matches!(e, LlmError::Parse(r) if r.kind == BadKind::NoFinal),
        "{e:?}"
    );
}

#[tokio::test]
async fn stream_over_byte_limit_is_parse_error() {
    let big = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
        "z".repeat(4096)
    );
    let (base, _) = raw_server(vec![(200, SSE, big)]).await;
    let (_, cb) = collector();
    let e = client(base, WireApi::Chat)
        .with_max_response_bytes(1024)
        .chat_stream(&req(None), &cb)
        .await
        .expect_err("erro");
    assert!(
        matches!(e, LlmError::Parse(r) if r.kind == BadKind::TooLarge),
        "{e:?}"
    );
}

#[tokio::test]
async fn error_events_are_errors_without_remote_text() {
    let failed = ev(
        "response.failed",
        r#"{"type":"response.failed","response":{"error":{"message":"SEGREDO-REMOTO"}}}"#,
    );
    let (base, _) = raw_server(vec![(200, SSE, failed); 3]).await;
    let (_, cb) = collector();
    let e = client(base, WireApi::Responses)
        .chat_stream(&req(None), &cb)
        .await
        .expect_err("erro");
    assert!(
        matches!(e, LlmError::Parse(r) if r.kind == BadKind::ProviderError && !format!("{e:?}{e}").contains("SEGREDO")),
        "{e:?}"
    );

    let chunk = "data: {\"error\":{\"message\":\"x\"}}\n\n".to_string();
    let (base, _) = raw_server(vec![(200, SSE, chunk); 3]).await;
    let e = client(base, WireApi::Chat)
        .chat_stream(&req(None), &cb)
        .await
        .expect_err("erro");
    assert!(matches!(e, LlmError::Parse(_)), "{e:?}");
}

#[test]
fn sse_parser_handles_split_chunks_and_crlf() {
    let mut p = SseParser::default();
    assert!(p.feed(b"event: a\r\nda").is_empty());
    let out = p.feed("ta: olá\r\n\r\n".as_bytes());
    assert_eq!(out, vec![("a".to_string(), "olá".to_string())]);
}

#[tokio::test]
async fn stream_counts_web_search_and_falls_back_on_400() {
    use super::tests::RESP_WEB;
    let rejection = r#"{"error":{"message":"web_search is not supported"}}"#.to_string();
    let done = format!(r#"{{"type":"response.completed","response":{RESP_WEB}}}"#);
    let ok = ev("response.completed", &done);
    let (base, cap) = raw_server(vec![(400, "application/json", rejection), (200, SSE, ok)]).await;
    let mut r = req(None);
    r.web_search = true;
    let got = client(base, WireApi::Responses)
        .chat_stream(&r, &|_| {})
        .await
        .expect("ok");
    assert!(got.web_search_rejected);
    assert_eq!(got.web_search_calls, 2);
    assert_eq!(got.citations.len(), 2);
    let captured = cap.lock().expect("lock").clone();
    assert!(body_of(&captured[0])["tools"].to_string().contains("web_search"));
    assert!(body_of(&captured[1]).get("tools").is_none());
}

#[tokio::test]
async fn stream_with_web_search_accepted_keeps_tool_and_not_rejected() {
    use super::tests::RESP_WEB;
    let done = format!(r#"{{"type":"response.completed","response":{RESP_WEB}}}"#);
    let (base, cap) = raw_server(vec![(200, SSE, ev("response.completed", &done))]).await;
    let mut r = req(None);
    r.web_search = true;
    let got = client(base, WireApi::Responses)
        .chat_stream(&r, &|_| {})
        .await
        .expect("ok");
    assert!(!got.web_search_rejected);
    assert_eq!(got.web_search_calls, 2);
    assert_eq!(body_of(&cap.lock().expect("lock")[0])["tools"], json!([{"type":"web_search"}]));
}
