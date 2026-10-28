//! Ritmo e limites por provedor no cliente (spec 132 T09), com servidor falso e relógio manual:
//! os cabeçalhos `anthropic-ratelimit-*` nos dois caminhos, o `retry-after`, cada tipo de 429 (ritmo,
//! teto de gasto, cota diária, tokens de saída), a pausa sem repetição e o teto do `max_tokens`.

use super::stream_tests::ev;
use super::*;
use serde_json::json;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CONN: &str = "conn-limite";
const ANTHROPIC_MODEL: &str = "claude-teste";
const GEMINI_MODEL: &str = "gemini-teste";

const ANTHROPIC_TEXT: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"olá"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":3}}"#;
const GEMINI_TEXT: &str = r#"{"status":"completed","usage":{"total_input_tokens":4,"total_output_tokens":2},"steps":[{"content":[{"text":"olá","type":"text"}],"type":"model_output"}],"object":"interaction"}"#;
const ANTHROPIC_RATE: &str = r#"{"type":"error","error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"}}"#;
const ANTHROPIC_SPEND: &str = r#"{"type":"error","error":{"type":"rate_limit_error","message":"enforced_spend_limit_reached: raise it in the console"}}"#;
const GEMINI_DAILY: &str = r#"{"error":{"code":"quota_exceeded","message":"daily quota exceeded"}}"#;
const GEMINI_DAILY_OLD: &str = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"quota","details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[{"quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier"}]}]}}"#;

/// Chave falsa montada em tempo de execução (nada com cara de segredo no código).
fn fake_key() -> String {
    ["chave", "limite", "falsa", "0132"].join("-")
}

#[derive(Default)]
struct ManualClock(Mutex<Duration>);

impl PaceClock for ManualClock {
    fn now(&self) -> Duration {
        *self.0.lock().expect("lock")
    }
}

/// Limitador com relógio manual e uma espera que só faz o relógio andar (e registra a duração).
fn paced_parts() -> (Arc<Pacer>, RetrySleep, Arc<Mutex<Vec<Duration>>>) {
    let clock = Arc::new(ManualClock::default());
    let pacer = Pacer::new(Arc::clone(&clock) as Arc<dyn PaceClock>);
    let waits = Arc::new(Mutex::new(Vec::new()));
    let w = Arc::clone(&waits);
    let sleep: RetrySleep = Arc::new(move |d| {
        w.lock().expect("lock").push(d);
        let mut now = clock.0.lock().expect("lock");
        *now = now.saturating_add(d);
        Box::pin(async {})
    });
    (pacer, sleep, waits)
}

/// Resposta HTTP bruta: status, cabeçalhos extras (`Nome: valor\r\n`), tipo e corpo.
fn http(status: u16, extra: &str, ctype: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn json_reply(status: u16, extra: &str, body: &str) -> String {
    http(status, extra, "application/json", body)
}

/// Servidor que responde, em ordem, cada resposta bruta (uma por conexão) e guarda os corpos JSON
/// recebidos.
async fn server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let cap = Arc::clone(&bodies);
    tokio::spawn(async move {
        for raw in responses {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let body = loop {
                let Ok(n) = sock.read(&mut tmp).await else { break String::new() };
                if n == 0 {
                    break String::new();
                }
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
                        .unwrap_or(0);
                    if rest.len() >= len {
                        break rest.to_string();
                    }
                }
            };
            cap.lock().expect("lock").push(serde_json::from_str(&body).unwrap_or(Value::Null));
            let _ = sock.write_all(raw.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), bodies)
}

fn req(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages: vec![ChatMessage::text(Role::User, "oi")],
        tools: Vec::new(),
        effort: None,
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    }
}

fn client(base: &str, kind: ConnectionKind, pacer: &Arc<Pacer>, sleep: RetrySleep) -> LlmClient {
    LlmClient::new(base, fake_key())
        .expect("client")
        .with_kind(kind)
        .with_retry_sleep(sleep)
        .with_pacer(Arc::clone(pacer), CONN)
}

fn anthropic_sse(data: Value) -> String {
    let name = data["type"].as_str().unwrap_or_default().to_string();
    ev(&name, &data.to_string())
}

/// Stream de texto da Anthropic.
fn anthropic_text_stream() -> String {
    let mut s = anthropic_sse(json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":1}}}));
    s += &anthropic_sse(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}));
    s += &anthropic_sse(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"olá"}}));
    s += &anthropic_sse(json!({"type":"content_block_stop","index":0}));
    s += &anthropic_sse(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}));
    s += &anthropic_sse(json!({"type":"message_stop"}));
    s
}

fn gemini_sse(data: Value) -> String {
    let name = data["event_type"].as_str().unwrap_or_default().to_string();
    ev(&name, &data.to_string())
}

/// Os cabeçalhos de limite da Anthropic, como vêm numa resposta (o reset em RFC 3339).
fn anthropic_quota(requests: u64, tokens: u64, output: u64) -> String {
    format!(
        "anthropic-ratelimit-requests-limit: {requests}\r\nanthropic-ratelimit-requests-remaining: {requests}\r\n\
         anthropic-ratelimit-requests-reset: 2026-10-28T12:00:30Z\r\n\
         anthropic-ratelimit-tokens-limit: {tokens}\r\nanthropic-ratelimit-tokens-remaining: {tokens}\r\n\
         anthropic-ratelimit-tokens-reset: 2026-10-28T12:00:30Z\r\n\
         anthropic-ratelimit-input-tokens-limit: {tokens}\r\nanthropic-ratelimit-output-tokens-limit: {output}\r\n\
         anthropic-ratelimit-output-tokens-remaining: {output}\r\n"
    )
}

fn waits_of(waits: &Mutex<Vec<Duration>>) -> Vec<Duration> {
    waits.lock().expect("lock").clone()
}

/// Os `anthropic-ratelimit-*` alimentam o limitador nos dois caminhos (sem e com stream), e o
/// `max_tokens` seguinte não passa do limite de tokens de saída lido.
#[tokio::test]
async fn anthropic_headers_feed_the_pacer_on_both_paths() {
    let (base, bodies) = server(vec![
        json_reply(200, &anthropic_quota(50, 40_000, 4_000), ANTHROPIC_TEXT),
        http(200, &anthropic_quota(60, 30_000, 3_000), "text/event-stream", &anthropic_text_stream()),
        json_reply(200, "", ANTHROPIC_TEXT),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(&base, ConnectionKind::Anthropic, &pacer, sleep);
    c.chat(&req(ANTHROPIC_MODEL)).await.expect("chat");
    assert_eq!(pacer.ceilings(CONN, ANTHROPIC_MODEL), (Some(50), Some(40_000)));
    assert_eq!(pacer.output_ceiling(CONN, ANTHROPIC_MODEL), Some(4_000));
    c.chat_stream(&req(ANTHROPIC_MODEL), &|_| {}).await.expect("stream");
    assert_eq!(pacer.ceilings(CONN, ANTHROPIC_MODEL), (Some(60), Some(30_000)));
    assert_eq!(pacer.output_ceiling(CONN, ANTHROPIC_MODEL), Some(3_000));
    c.chat(&req(ANTHROPIC_MODEL)).await.expect("chat de novo");
    let bodies = bodies.lock().expect("lock").clone();
    let max: Vec<u64> = bodies.iter().filter_map(|b| b["max_tokens"].as_u64()).collect();
    assert_eq!(max, vec![u64::from(anthropic::DEFAULT_MAX_TOKENS), 4_000, 3_000]);
    assert!(waits_of(&waits).is_empty(), "restante folgado: nada espera");
}

/// O limite de saída do cabeçalho não aumenta o `max_tokens` conhecido do modelo.
#[tokio::test]
async fn output_header_above_the_model_limit_keeps_the_model_limit() {
    let (base, bodies) = server(vec![
        json_reply(200, &anthropic_quota(50, 400_000, 90_000), ANTHROPIC_TEXT),
        json_reply(200, "", ANTHROPIC_TEXT),
    ])
    .await;
    let (pacer, sleep, _) = paced_parts();
    let c = client(&base, ConnectionKind::Anthropic, &pacer, sleep);
    anthropic::remember_max_tokens(&base, ANTHROPIC_MODEL, 16_000);
    c.chat(&req(ANTHROPIC_MODEL)).await.expect("chat");
    c.chat(&req(ANTHROPIC_MODEL)).await.expect("chat de novo");
    let max: Vec<u64> = bodies.lock().expect("lock").iter().filter_map(|b| b["max_tokens"].as_u64()).collect();
    assert_eq!(max, vec![16_000, 16_000]);
}

/// O 429 de ritmo da Anthropic vai pelo limitador: espera o `retry-after`, nos dois caminhos, e o
/// limite ensina os tetos.
#[tokio::test]
async fn anthropic_rate_limit_waits_for_retry_after_on_both_paths() {
    let (base, _) = server(vec![
        json_reply(429, "retry-after: 7\r\n", ANTHROPIC_RATE),
        json_reply(200, "", ANTHROPIC_TEXT),
        json_reply(429, "retry-after: 4\r\n", ANTHROPIC_RATE),
        http(200, "", "text/event-stream", &anthropic_text_stream()),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(&base, ConnectionKind::Anthropic, &pacer, sleep);
    c.chat(&req(ANTHROPIC_MODEL)).await.expect("chat depois do limite");
    assert!(waits_of(&waits).contains(&Duration::from_secs(7)), "{:?}", waits_of(&waits));
    assert!(pacer.ceilings(CONN, ANTHROPIC_MODEL).0.is_some(), "o limite ensina o teto");
    c.chat_stream(&req(ANTHROPIC_MODEL), &|_| {}).await.expect("stream depois do limite");
    assert!(waits_of(&waits).contains(&Duration::from_secs(4)), "{:?}", waits_of(&waits));
}

/// O 429 que cita os tokens de saída reduz o `max_tokens` uma vez (para o número citado), e a
/// redução vale nos pedidos seguintes; um segundo 429 assim não reduz de novo.
#[tokio::test]
async fn output_tokens_429_reduces_max_tokens_once() {
    let first = r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed the rate limit for your organization (org1234) of 8,000 output tokens per minute."}}"#;
    let second = r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed the rate limit for your organization (org1234) of 2,000 output tokens per minute."}}"#;
    let (base, bodies) = server(vec![
        json_reply(429, "retry-after: 1\r\n", first),
        json_reply(429, "retry-after: 1\r\n", second),
        json_reply(200, "", ANTHROPIC_TEXT),
        http(200, "", "text/event-stream", &anthropic_text_stream()),
    ])
    .await;
    let (pacer, sleep, _) = paced_parts();
    let c = client(&base, ConnectionKind::Anthropic, &pacer, sleep);
    anthropic::remember_max_tokens(&base, ANTHROPIC_MODEL, 64_000);
    c.chat(&req(ANTHROPIC_MODEL)).await.expect("chat");
    c.chat_stream(&req(ANTHROPIC_MODEL), &|_| {}).await.expect("stream");
    let max: Vec<u64> = bodies.lock().expect("lock").iter().filter_map(|b| b["max_tokens"].as_u64()).collect();
    assert_eq!(max, vec![64_000, 8_000, 8_000, 8_000]);
    assert_eq!(pacer.output_ceiling(CONN, ANTHROPIC_MODEL), Some(8_000));
    assert_eq!(anthropic::known_max_tokens(&base, ANTHROPIC_MODEL), Some(64_000), "o limite do modelo continua");
}

/// O teto de gasto da Anthropic pausa: o erro próprio, sem nova tentativa e sem ensinar o ritmo,
/// nos dois caminhos.
#[tokio::test]
async fn anthropic_spend_limit_pauses_without_repeating() {
    let (base, bodies) = server(vec![json_reply(429, "", ANTHROPIC_SPEND), json_reply(429, "", ANTHROPIC_SPEND)]).await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(&base, ConnectionKind::Anthropic, &pacer, sleep);
    let err = c.chat(&req(ANTHROPIC_MODEL)).await.expect_err("teto de gasto");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::SpendLimit)), "{err:?}");
    assert_eq!(provider_failure(&err).to_string(), "provider_spend_limit");
    assert_eq!(bodies.lock().expect("lock").len(), 1, "sem nova tentativa");
    let err = c.chat_stream(&req(ANTHROPIC_MODEL), &|_| {}).await.expect_err("teto de gasto no stream");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::SpendLimit)), "{err:?}");
    assert_eq!(bodies.lock().expect("lock").len(), 2, "sem nova tentativa no stream");
    assert!(waits_of(&waits).is_empty());
    assert_eq!(pacer.ceilings(CONN, ANTHROPIC_MODEL), (None, None), "não é limite de ritmo");
    assert_eq!(pacer.output_ceiling(CONN, ANTHROPIC_MODEL), None);
}

/// A cota diária do Gemini pausa, nos dois formatos e nos dois caminhos, e também quando chega
/// como evento `error` no meio do stream.
#[tokio::test]
async fn gemini_daily_quota_pauses_without_repeating() {
    let mid_stream = gemini_sse(json!({"event_type":"interaction.created","interaction":{"object":"interaction"}}))
        + &gemini_sse(json!({"event_type":"error","error":{"code":"quota_exceeded","message":"daily quota exceeded"}}));
    let (base, bodies) = server(vec![
        json_reply(429, "", GEMINI_DAILY),
        json_reply(429, "retry-after: 30\r\n", GEMINI_DAILY_OLD),
        http(200, "", "text/event-stream", &mid_stream),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(&base, ConnectionKind::GoogleGemini, &pacer, sleep);
    let err = c.chat(&req(GEMINI_MODEL)).await.expect_err("cota diária");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::DailyQuota)), "{err:?}");
    assert_eq!(provider_failure(&err).to_string(), "provider_daily_quota");
    let err = c.chat_stream(&req(GEMINI_MODEL), &|_| {}).await.expect_err("cota diária, formato antigo");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::DailyQuota)), "{err:?}");
    let err = c.chat_stream(&req(GEMINI_MODEL), &|_| {}).await.expect_err("cota diária no meio do stream");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::DailyQuota)), "{err:?}");
    assert_eq!(bodies.lock().expect("lock").len(), 3, "uma chamada cada, sem repetir");
    assert!(waits_of(&waits).is_empty());
    assert_eq!(pacer.ceilings(CONN, GEMINI_MODEL), (None, None));
}

/// O 429 de ritmo do Gemini espera o `Retry-After` ou, sem ele, o `retryDelay` do corpo.
#[tokio::test]
async fn gemini_rate_limit_waits_for_retry_after_or_retry_delay() {
    let old_minute = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"quota","details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[{"quotaId":"GenerateRequestsPerMinutePerProjectPerModel-FreeTier"}]},{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"12s"}]}}"#;
    let minute = r#"{"error":{"code":"rate_limit_exceeded","message":"per minute"}}"#;
    let (base, _) = server(vec![
        json_reply(429, "", old_minute),
        json_reply(200, "", GEMINI_TEXT),
        json_reply(429, "retry-after: 5\r\n", minute),
        json_reply(200, "", GEMINI_TEXT),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(&base, ConnectionKind::GoogleGemini, &pacer, sleep);
    c.chat(&req(GEMINI_MODEL)).await.expect("chat depois do retryDelay");
    assert!(waits_of(&waits).contains(&Duration::from_secs(12)), "{:?}", waits_of(&waits));
    c.chat(&req(GEMINI_MODEL)).await.expect("chat depois do Retry-After");
    assert!(waits_of(&waits).contains(&Duration::from_secs(5)), "{:?}", waits_of(&waits));
    assert!(pacer.ceilings(CONN, GEMINI_MODEL).0.is_some(), "o limite ensina o teto");
}

/// O teto de tokens de saída também limita o `max_output_tokens` do Gemini (quando algum cabeçalho o
/// informar), sem passar do conhecido do modelo.
#[tokio::test]
async fn gemini_max_output_tokens_follows_the_output_ceiling() {
    let (base, bodies) = server(vec![json_reply(200, "", GEMINI_TEXT), json_reply(200, "", GEMINI_TEXT)]).await;
    let (pacer, sleep, _) = paced_parts();
    let c = client(&base, ConnectionKind::GoogleGemini, &pacer, sleep);
    gemini::remember_max_tokens(&base, GEMINI_MODEL, 65_536);
    c.chat(&req(GEMINI_MODEL)).await.expect("chat");
    pacer.observe(CONN, GEMINI_MODEL, &Quota { limit_output_tokens: Some(6_000), ..Quota::default() });
    c.chat(&req(GEMINI_MODEL)).await.expect("chat de novo");
    let max: Vec<Value> = bodies.lock().expect("lock").iter().map(|b| b["generation_config"]["max_output_tokens"].clone()).collect();
    assert_eq!(max, vec![json!(65_536), json!(6_000)]);
}

/// F-01: o 429 que cita um limite de saída maior ou igual ao `max_tokens` enviado não reduz (o
/// `max_tokens` não foi a causa) nem gasta a redução única do par; o seguinte, abaixo, reduz.
#[tokio::test]
async fn output_tokens_429_citing_a_larger_limit_keeps_the_single_reduction() {
    let larger = r#"{"type":"error","error":{"type":"rate_limit_error","message":"would exceed the rate limit of 8,000 output tokens per minute."}}"#;
    let smaller = r#"{"type":"error","error":{"type":"rate_limit_error","message":"would exceed the rate limit of 2,000 output tokens per minute."}}"#;
    let (base, bodies) = server(vec![
        json_reply(429, "retry-after: 1\r\n", larger),
        json_reply(200, "", ANTHROPIC_TEXT),
        json_reply(429, "retry-after: 1\r\n", smaller),
        json_reply(200, "", ANTHROPIC_TEXT),
    ])
    .await;
    let (pacer, sleep, _) = paced_parts();
    let c = client(&base, ConnectionKind::Anthropic, &pacer, sleep);
    anthropic::remember_max_tokens(&base, ANTHROPIC_MODEL, 4_000);
    c.chat(&req(ANTHROPIC_MODEL)).await.expect("chat");
    assert_eq!(pacer.output_ceiling(CONN, ANTHROPIC_MODEL), None, "nada reduzido");
    c.chat(&req(ANTHROPIC_MODEL)).await.expect("chat de novo");
    let max: Vec<u64> = bodies.lock().expect("lock").iter().filter_map(|b| b["max_tokens"].as_u64()).collect();
    assert_eq!(max, vec![4_000, 4_000, 4_000, 2_000]);
    assert_eq!(pacer.output_ceiling(CONN, ANTHROPIC_MODEL), Some(2_000));
}

// --- Spec 133 T02: cobrança e crédito esgotado pausam, em qualquer provedor ---

const ANTHROPIC_BILLING_402: &str = r#"{"type":"error","error":{"type":"billing_error","message":"Your account has an outstanding payment."}}"#;
const ANTHROPIC_CREDIT_400: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."}}"#;
const ANTHROPIC_WORKSPACE_SPEND_400: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"This workspace has reached its spend limit for the month."}}"#;
const GEMINI_PAYMENT_402: &str = r#"{"error":{"code":"payment_required","message":"billing account required"}}"#;
const OPENAI_INSUFFICIENT_QUOTA: &str = r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#;

/// Todo 402, de qualquer tipo de conexão, pausa por cobrança: sem nova tentativa e sem ensinar o
/// ritmo.
#[tokio::test]
async fn http_402_pauses_as_billing_for_every_kind() {
    for (kind, model, body) in [
        (ConnectionKind::Anthropic, ANTHROPIC_MODEL, ANTHROPIC_BILLING_402),
        (ConnectionKind::GoogleGemini, GEMINI_MODEL, GEMINI_PAYMENT_402),
        (ConnectionKind::ApiKey, "modelo-x", "corpo que não é json"),
    ] {
        let (base, bodies) = server(vec![json_reply(402, "", body), json_reply(402, "", body)]).await;
        let (pacer, sleep, waits) = paced_parts();
        let c = client(&base, kind, &pacer, sleep);
        let err = c.chat(&req(model)).await.expect_err("402");
        assert!(matches!(err, LlmError::Limit(ProviderLimit::Billing)), "{kind:?}: {err:?}");
        assert_eq!(provider_failure(&err).to_string(), "provider_billing");
        let err = c.chat_stream(&req(model), &|_| {}).await.expect_err("402 no stream");
        assert!(matches!(err, LlmError::Limit(ProviderLimit::Billing)), "{kind:?}: {err:?}");
        assert_eq!(bodies.lock().expect("lock").len(), 2, "{kind:?}: sem nova tentativa");
        assert!(waits_of(&waits).is_empty());
        assert_eq!(pacer.ceilings(CONN, model), (None, None), "{kind:?}: não é limite de ritmo");
    }
}

/// O 400 da Anthropic pelo crédito esgotado pausa por cobrança; o 400 pelo teto de gasto do
/// workspace pausa pelo teto. Nenhum dos dois se repete.
#[tokio::test]
async fn anthropic_400_credit_and_spend_limit_pause() {
    let (base, bodies) = server(vec![json_reply(400, "", ANTHROPIC_CREDIT_400), json_reply(400, "", ANTHROPIC_WORKSPACE_SPEND_400)]).await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(&base, ConnectionKind::Anthropic, &pacer, sleep);
    let err = c.chat(&req(ANTHROPIC_MODEL)).await.expect_err("crédito");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::Billing)), "{err:?}");
    let err = c.chat_stream(&req(ANTHROPIC_MODEL), &|_| {}).await.expect_err("teto do workspace");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::SpendLimit)), "{err:?}");
    assert_eq!(bodies.lock().expect("lock").len(), 2);
    assert!(waits_of(&waits).is_empty());
}

/// Negativo (C-10): um 400 comum que fala em crédito fora do `error.message` (repetindo o que foi
/// enviado) não é limite; nem o texto de crédito numa conexão de outro tipo.
#[tokio::test]
async fn plain_400_mentioning_credit_outside_the_message_is_not_a_limit() {
    let echoed = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.0.content: prompt is too long"},"request":{"echo":"Your credit balance is too low"},"credit balance":"too low"}"#;
    let other_kind = r#"{"error":{"type":"invalid_request_error","message":"Your credit balance is too low"}}"#;
    let (base, _) = server(vec![json_reply(400, "", echoed), json_reply(400, "", other_kind)]).await;
    let (pacer, sleep, _) = paced_parts();
    let c = client(&base, ConnectionKind::Anthropic, &pacer, Arc::clone(&sleep));
    let err = c.chat(&req(ANTHROPIC_MODEL)).await.expect_err("400 comum");
    assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
    let c = client(&base, ConnectionKind::GoogleGemini, &pacer, sleep);
    let err = c.chat(&req(GEMINI_MODEL)).await.expect_err("texto de crédito fora do escopo");
    assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
}

/// O `insufficient_quota` da OpenAI pausa por cobrança no HTTP 429 (sem esperar nem repetir) e no
/// meio do stream, no Chat Completions e no `response.failed` do Responses.
#[tokio::test]
async fn openai_insufficient_quota_pauses_on_http_and_stream() {
    let chat_stream = "data: {\"error\":{\"type\":\"insufficient_quota\",\"code\":\"insufficient_quota\",\"message\":\"quota\"}}\n\n";
    let failed = ev(
        "response.failed",
        &json!({"type":"response.failed","response":{"status":"failed","error":{"code":"credit_balance_exhausted","message":"no credit"}}}).to_string(),
    );
    let (base, bodies) = server(vec![
        json_reply(429, "retry-after: 5\r\n", OPENAI_INSUFFICIENT_QUOTA),
        http(200, "", "text/event-stream", chat_stream),
        http(200, "", "text/event-stream", &failed),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(&base, ConnectionKind::ApiKey, &pacer, Arc::clone(&sleep));
    let err = c.chat(&req("gpt-x")).await.expect_err("429 insufficient_quota");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::Billing)), "{err:?}");
    let err = c.chat_stream(&req("gpt-x"), &|_| {}).await.expect_err("erro no stream do chat");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::Billing)), "{err:?}");
    let c = client(&base, ConnectionKind::ApiKey, &pacer, sleep).with_wire_api(WireApi::Responses);
    let err = c.chat_stream(&req("gpt-x"), &|_| {}).await.expect_err("response.failed");
    assert!(matches!(err, LlmError::Limit(ProviderLimit::Billing)), "{err:?}");
    assert_eq!(bodies.lock().expect("lock").len(), 3, "sem nova tentativa");
    assert!(waits_of(&waits).is_empty(), "não espera o retry-after");
    assert_eq!(pacer.ceilings(CONN, "gpt-x"), (None, None), "não ensina o ritmo");
}
