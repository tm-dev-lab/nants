//! Testes da autenticação por tipo de conexão (spec 132 T02): os cabeçalhos exatos de cada `kind`
//! e a chave fora de erro, trecho, diagnóstico de corte e log.

use super::stream_tests::{ev, raw_server};
use super::tests::{head_of, spawn_server, KEY};
use super::*;
use std::sync::{Arc, Mutex};

/// Chave falsa montada em tempo de execução (nada com cara de segredo no código).
fn fake_key(tag: &str) -> String {
    ["chave", tag, "falsa", "0042"].join("-")
}

/// Corpo de erro que devolve a chave recebida.
fn echo_body(key: &str) -> &'static str {
    Box::leak(format!(r#"{{"error":{{"message":"chave recebida: {key}","type":"authentication_error"}}}}"#).into_boxed_str())
}

/// Linhas de cabeçalho da requisição capturada, com o nome em minúsculas.
fn header_lines(raw: &str) -> Vec<(String, String)> {
    head_of(raw)
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect()
}

fn values<'a>(headers: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    headers.iter().filter(|(n, _)| n == name).map(|(_, v)| v.as_str()).collect()
}

/// Pede `GET /models` com o tipo dado e devolve os cabeçalhos que chegaram ao servidor falso.
async fn sent_headers(kind: ConnectionKind, key: &str) -> Vec<(String, String)> {
    let (base, cap) = spawn_server(vec![(200, r#"{"data":[]}"#)]).await;
    let client = LlmClient::new(base, key.to_string()).expect("client").with_kind(kind);
    client.get_json("/models").await.expect("lista");
    let raw = cap.lock().expect("lock")[0].clone();
    header_lines(&raw)
}

fn request() -> ChatRequest {
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

#[tokio::test]
async fn api_key_and_plan_send_only_bearer() {
    for kind in [ConnectionKind::ApiKey, ConnectionKind::ChatgptPlan] {
        let h = sent_headers(kind, KEY).await;
        let bearer = format!("Bearer {KEY}");
        assert_eq!(values(&h, "authorization"), vec![bearer.as_str()], "{kind:?}");
        for absent in ["x-api-key", "anthropic-version", "x-goog-api-key"] {
            assert!(values(&h, absent).is_empty(), "{kind:?}: {absent}");
        }
    }
}

#[tokio::test]
async fn anthropic_sends_x_api_key_and_version_without_authorization() {
    let key = fake_key("anthropic");
    let h = sent_headers(ConnectionKind::Anthropic, &key).await;
    assert_eq!(values(&h, "x-api-key"), vec![key.as_str()]);
    assert_eq!(values(&h, "anthropic-version"), vec!["2023-06-01"]);
    assert!(values(&h, "authorization").is_empty(), "nativo sem Authorization");
    assert!(values(&h, "x-goog-api-key").is_empty());
}

#[tokio::test]
async fn gemini_sends_x_goog_api_key_without_authorization() {
    let key = fake_key("gemini");
    let h = sent_headers(ConnectionKind::GoogleGemini, &key).await;
    assert_eq!(values(&h, "x-goog-api-key"), vec![key.as_str()]);
    assert!(values(&h, "authorization").is_empty(), "nativo sem Authorization");
    assert!(values(&h, "x-api-key").is_empty());
    assert!(values(&h, "anthropic-version").is_empty());
}

#[test]
fn auth_headers_mark_the_secret_as_sensitive_and_reject_bad_keys_without_echo() {
    let key = fake_key("sensivel");
    for kind in [ConnectionKind::ApiKey, ConnectionKind::ChatgptPlan, ConnectionKind::Anthropic, ConnectionKind::GoogleGemini] {
        let headers = auth_headers(kind, &key).expect("cabeçalhos");
        let secret: Vec<_> = headers.iter().filter(|(_, v)| v.to_str().is_ok_and(|t| t.contains(&key))).collect();
        assert_eq!(secret.len(), 1, "{kind:?}: a chave vai num cabeçalho só");
        assert!(secret[0].1.is_sensitive(), "{kind:?}");
        assert!(!format!("{headers:?}").contains(&key), "{kind:?}: o Debug não mostra a chave");
        let bad = format!("{key}\n");
        let err = auth_headers(kind, &bad).expect_err("quebra de linha não cabe num cabeçalho");
        assert!(matches!(err, LlmError::Config(_)), "{err:?}");
        assert!(!format!("{err} {err:?}").contains(&key), "{kind:?}");
    }
}

/// A chave devolvida no corpo de erro não aparece no erro, no trecho, na falha do provedor nem na
/// linha do log, nos dois provedores e nas duas vias (lista de modelos e chamada do modelo).
#[tokio::test]
async fn native_key_echoed_by_the_server_never_leaks() {
    for (kind, tag) in [(ConnectionKind::Anthropic, "anthropic"), (ConnectionKind::GoogleGemini, "gemini")] {
        let key = fake_key(tag);
        for status in [400u16, 401, 403] {
            let (base, _) = spawn_server(vec![(status, echo_body(&key)), (status, echo_body(&key))]).await;
            let client = LlmClient::new(base, key.clone()).expect("client").with_kind(kind);
            let listed = client.get_json("/models").await.expect_err("lista falha");
            let chat = client.chat(&request()).await.expect_err("chamada falha");
            for err in [listed, chat] {
                let LlmError::Http { body_snippet, .. } = &err else { panic!("esperado Http: {err:?}") };
                assert!(body_snippet.contains("***"), "{body_snippet}");
                let failure = provider_failure(&err);
                let all = format!("{err} {err:?} {failure} {failure:?} {}", failure.describe());
                assert!(!all.contains(&key), "{kind:?} {status}: {all}");
            }
        }
    }
}

/// Stream grande da Messages API (spec 132 T04) com `secret` no texto de cada pedaço.
fn huge_anthropic_stream(deltas: usize, secret: &str) -> String {
    let mut s = ev("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#);
    s += &ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#);
    for i in 0..deltas {
        let delta = serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":format!("parte {i} com {secret} ")}});
        s += &ev("content_block_delta", &delta.to_string());
    }
    s
}

/// Stream grande da Interactions API (spec 132 T07) com `secret` no texto de cada pedaço.
fn huge_gemini_stream(deltas: usize, secret: &str) -> String {
    let mut s = ev("step.start", r#"{"event_type":"step.start","index":0,"step":{"type":"model_output"}}"#);
    for i in 0..deltas {
        let delta = serde_json::json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":format!("parte {i} com {secret} ")}});
        s += &ev("step.delta", &delta.to_string());
    }
    s
}

/// O diagnóstico de corte (spec 114 T05) redige a chave dos provedores nativos: a Anthropic pelo
/// stream da Messages API (spec 132 T04) e o Gemini pelo da Interactions API (T07).
#[tokio::test]
async fn native_cut_diagnosis_redacts_the_key() {
    for (kind, tag) in [(ConnectionKind::Anthropic, "anthropic"), (ConnectionKind::GoogleGemini, "gemini")] {
        let key = fake_key(tag);
        let body = match kind {
            ConnectionKind::GoogleGemini => huge_gemini_stream(2_000, &key),
            _ => huge_anthropic_stream(2_000, &key),
        };
        let (base, _) = raw_server(vec![(200, "text/event-stream", body)]).await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let cb = move |d: StreamDelta| seen2.lock().expect("lock").push(d);
        let err = LlmClient::new(base, key.clone())
            .expect("client")
            .with_kind(kind)
            .with_wire_api(WireApi::Responses)
            .with_max_response_bytes(64 * 1024)
            .chat_stream(&request(), &cb)
            .await
            .expect_err("cortada");
        assert!(!format!("{err} {err:?}").contains(&key));
        let cuts: Vec<Arc<StreamCut>> = seen
            .lock()
            .expect("lock")
            .iter()
            .filter_map(|d| if let StreamDelta::Cut(c) = d { Some(Arc::clone(c)) } else { None })
            .collect();
        assert_eq!(cuts.len(), 1, "{kind:?}");
        let cut = &cuts[0];
        let (head, tail) = cut.snippets(str::to_string);
        assert!(!format!("{cut:?} {head} {tail}").contains(&key), "{kind:?}");
        assert!(format!("{head}{tail}").contains("***"), "{kind:?}: a chave sai como ***");
    }
}
