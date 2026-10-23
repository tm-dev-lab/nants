//! Limitador de ritmo no cliente (spec 089 T02): cabeçalhos de cota, aprendizado com o limite,
//! avisos, parada e quem fica de fora. Relógio manual; as esperas só o fazem andar.

use super::retry_tests::{http, server};
use super::*;
use std::sync::Mutex;

const OK_CHAT: &str = r#"{"choices":[{"message":{"role":"assistant","content":"olá"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#;
const SSE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"olá\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
const CONN: &str = "conn-x";

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

fn req() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        messages: vec![ChatMessage::text(Role::User, "oi")],
        tools: Vec::new(),
        effort: None,
        response_json: false,
        web_search: false,
    }
}

fn client(base: String, pacer: &Arc<Pacer>, sleep: RetrySleep) -> LlmClient {
    LlmClient::new(base, "sk-test-PACE".into())
        .expect("client")
        .with_retry_sleep(sleep)
        .with_pacer(Arc::clone(pacer), CONN)
}

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

fn secs(v: &[u64]) -> Vec<Duration> {
    v.iter().map(|s| Duration::from_secs(*s)).collect()
}

#[tokio::test]
async fn without_quota_headers_or_limits_nothing_waits() {
    let (base, count) = server(vec![http(200, "", "application/json", OK_CHAT); 5]).await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(base, &pacer, sleep);
    for _ in 0..3 {
        c.chat(&req()).await.expect("ok");
    }
    let deltas = Mutex::new(Vec::new());
    for _ in 0..2 {
        c.chat_stream(&req(), &|d| deltas.lock().expect("lock").extend(super::not_size(d))).await.expect("ok");
    }
    assert_eq!(*count.lock().expect("lock"), 5);
    assert!(waits.lock().expect("lock").is_empty());
    assert!(notices(&deltas).is_empty());
    assert_eq!(pacer.ceilings(CONN, "m"), (None, None));
}

#[tokio::test]
async fn exhausted_remaining_on_a_stream_waits_for_the_reset_with_notices() {
    // Cabeçalhos numa resposta SSE de sucesso: o pedido seguinte espera o reset, avisando.
    let quota = "x-ratelimit-remaining-requests: 0\r\nx-ratelimit-reset-requests: 20s\r\n";
    let (base, count) = server(vec![
        http(200, quota, "text/event-stream", SSE),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(base, &pacer, sleep);
    let deltas = Mutex::new(Vec::new());
    let on = |d| deltas.lock().expect("lock").extend(super::not_size(d));
    c.chat_stream(&req(), &on).await.expect("1º");
    assert!(notices(&deltas).is_empty());
    c.chat_stream(&req(), &on).await.expect("2º");
    assert_eq!(*count.lock().expect("lock"), 2);
    assert_eq!(*waits.lock().expect("lock"), secs(&[15, 5]));
    assert_eq!(
        notices(&deltas),
        vec![
            "ajustando o ritmo ao limite do provedor: 20 s".to_string(),
            "ajustando o ritmo ao limite do provedor: 5 s".to_string(),
        ]
    );
}

#[tokio::test]
async fn usage_corrects_the_cost_of_the_request() {
    let (base, _) = server(vec![http(200, "", "application/json", OK_CHAT)]).await;
    let (pacer, sleep, _) = paced_parts();
    client(base, &pacer, sleep).chat(&req()).await.expect("ok");
    assert_eq!(pacer.window(CONN, "m"), vec![(Duration::ZERO, 10)], "prompt 7 + saída 3");
}

#[tokio::test]
async fn a_limit_teaches_the_pace_and_the_retry_follows_it() {
    let (base, count) = server(vec![
        http(200, "", "application/json", OK_CHAT),
        http(200, "", "application/json", OK_CHAT),
        http(429, "", "application/json", "limite"),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(base, &pacer, sleep).with_throttle_delays(secs(&[5]));
    let deltas = Mutex::new(Vec::new());
    let on = |d| deltas.lock().expect("lock").extend(super::not_size(d));
    c.chat_stream(&req(), &on).await.expect("1º");
    c.chat_stream(&req(), &on).await.expect("2º");
    c.chat_stream(&req(), &on).await.expect("3º, depois do limite");
    assert_eq!(*count.lock().expect("lock"), 4);
    // 3 pedidos na janela quando o limite chegou: teto de 2/min. Espera do limite (5 s) e, na nova
    // tentativa, a do ritmo até o 2º dos três sair da janela (60 s depois de 0): 55 s.
    assert_eq!(pacer.ceilings(CONN, "m").0, Some(2));
    assert_eq!(*waits.lock().expect("lock"), secs(&[5, 15, 15, 15, 10]));
    let got = notices(&deltas);
    assert_eq!(got.len(), 5, "{got:?}");
    assert!(got[0].starts_with("provedor no limite (HTTP 429)"), "{got:?}");
    assert_eq!(got[1], "ajustando o ritmo ao limite do provedor: 55 s");
    assert_eq!(got[4], "ajustando o ritmo ao limite do provedor: 10 s");
}

#[tokio::test]
async fn throttled_bad_response_also_teaches_the_pace() {
    let body = r#"{"error":{"code":"no_capacity","message":"sem capacidade"}}"#;
    let (base, _) = server(vec![http(200, "", "application/json", body)]).await;
    let (pacer, sleep, _) = paced_parts();
    let err = client(base, &pacer, sleep).with_throttle_delays(Vec::new()).chat(&req()).await.expect_err("limite");
    assert!(is_throttle(&err), "{err:?}");
    assert_eq!(pacer.ceilings(CONN, "m").0, Some(1));
}

#[tokio::test]
async fn short_waits_have_no_notice() {
    let quota = "x-ratelimit-remaining-requests: 1\r\nx-ratelimit-reset-requests: 800ms\r\n";
    let (base, _) = server(vec![
        http(200, quota, "application/json", OK_CHAT),
        http(200, "", "application/json", OK_CHAT),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(base, &pacer, sleep);
    let deltas = Mutex::new(Vec::new());
    let on = |d| deltas.lock().expect("lock").extend(super::not_size(d));
    c.chat_stream(&req(), &on).await.expect("1º");
    c.chat_stream(&req(), &on).await.expect("2º");
    assert_eq!(*waits.lock().expect("lock"), vec![Duration::from_millis(800)]);
    assert!(notices(&deltas).is_empty());
}

#[tokio::test]
async fn stop_during_the_pace_wait_returns_quickly_without_sending() {
    let (base, count) = server(vec![http(200, "", "application/json", OK_CHAT)]).await;
    let clock = Arc::new(ManualClock::default());
    let pacer = Pacer::new(Arc::clone(&clock) as Arc<dyn PaceClock>);
    let quota = Quota { remaining_requests: Some(0), reset_requests: Some(Duration::from_secs(30)), ..Quota::default() };
    pacer.observe(CONN, "m", &quota);
    let flag = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&flag);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(80)).await;
        f.store(true, Ordering::SeqCst);
    });
    let real: RetrySleep = Arc::new(|d| Box::pin(tokio::time::sleep(d)));
    let c = client(base, &pacer, real);
    let started = std::time::Instant::now();
    let err = with_cancel_scope(Some(flag), c.chat_stream(&req(), &|_| {})).await.expect_err("parado");
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    assert!(matches!(err, LlmError::Config(_)), "{err:?}");
    assert_eq!(*count.lock().expect("lock"), 0, "o pedido não saiu");
    assert!(pacer.window(CONN, "m").is_empty(), "a vaga foi devolvida");
}

#[test]
fn only_connection_clients_are_paced() {
    let conn = Connection {
        id: "c-pace".into(),
        name: "x".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        api_key: "sk-literal".into(),
        wire_api: WireApi::Chat,
        kind: Default::default(),
    };
    let from_conn = LlmClient::from_connection(&conn).expect("cliente");
    assert!(from_conn.is_paced());
    assert!(!from_conn.without_pacing().is_paced(), "teste de conexão");
    assert!(!LlmClient::new("http://x/v1", "k".into()).expect("cliente").is_paced(), "lista de modelos");
}

#[tokio::test]
async fn connection_clients_share_the_process_registry() {
    let quota = "x-ratelimit-limit-requests: 40\r\n";
    let (base, _) = server(vec![http(200, quota, "application/json", OK_CHAT)]).await;
    let conn = Connection {
        id: "c-shared".into(),
        name: "x".into(),
        base_url: base,
        api_key: "sk-literal".into(),
        wire_api: WireApi::Chat,
        kind: Default::default(),
    };
    LlmClient::from_connection(&conn).expect("cliente").chat(&req()).await.expect("ok");
    // Outro cliente da mesma conexão (outro papel, outro turno) vê o mesmo par.
    assert_eq!(Pacer::global().ceilings("c-shared", "m"), (Some(40), None));
}

#[test]
fn text_estimate_stays_close_to_body_bytes_over_four() {
    let c = LlmClient::new("http://x/v1", "k".into()).expect("cliente");
    let tool = ToolSpec {
        name: "read_file".into(),
        description: "Lê um arquivo do projeto".into(),
        parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
    };
    let r = ChatRequest {
        messages: vec![
            ChatMessage::text(Role::System, "s".repeat(2000)),
            ChatMessage::text(Role::User, "a".repeat(4000)),
        ],
        tools: vec![tool],
        ..req()
    };
    let est = c.estimate_tokens(&r);
    let body = serde_json::to_vec(&build_body(&r, None)).expect("json").len() as u64 / 4;
    // Só texto: a estimativa fica perto de bytes do corpo / 4 (sem o excesso de estrutura do JSON).
    assert!(est * 100 >= body * 90 && est <= body, "estimativa {est}, corpo/4 {body}");
}

#[test]
fn images_cost_a_fixed_amount_and_are_not_encoded() {
    let c = LlmClient::new("http://x/v1", "k".into()).expect("cliente");
    let image = ImageInput::new("image/png", vec![0u8; 1024 * 1024]);
    let r = ChatRequest { messages: vec![ChatMessage::with_images("veja", vec![image.clone(), image])], ..req() };
    let est = c.estimate_tokens(&r);
    assert!((3000..3100).contains(&est), "{est}");
}

#[tokio::test]
async fn a_large_screenshot_does_not_stall_the_next_request_under_token_quota() {
    // Provedor com cota de tokens em toda resposta (como a OpenAI): 30 mil/min, quase todo livre.
    let quota = "x-ratelimit-limit-tokens: 30000\r\nx-ratelimit-remaining-tokens: 28000\r\nx-ratelimit-reset-tokens: 4s\r\n";
    let (base, count) = server(vec![
        http(200, quota, "application/json", OK_CHAT),
        http(200, quota, "application/json", OK_CHAT),
        http(200, quota, "application/json", OK_CHAT),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(base, &pacer, sleep);
    let shot = ImageInput::new("image/png", vec![0u8; 1024 * 1024]);
    let with_shot = ChatRequest { messages: vec![ChatMessage::with_images("captura", vec![shot])], ..req() };
    c.chat(&req()).await.expect("1º");
    c.chat(&with_shot).await.expect("com a captura");
    c.chat(&with_shot).await.expect("o seguinte");
    assert_eq!(*count.lock().expect("lock"), 3);
    assert!(waits.lock().expect("lock").is_empty(), "{:?}", waits.lock().expect("lock"));
}

#[tokio::test]
async fn internal_resends_count_as_requests_in_the_window() {
    let rejected = r#"{"error":{"message":"Unsupported parameter: reasoning_effort"}}"#;
    let (base, count) = server(vec![
        http(400, "", "application/json", rejected),
        http(200, "", "application/json", OK_CHAT),
        http(400, "", "application/json", rejected),
        http(200, "", "text/event-stream", SSE),
    ])
    .await;
    let (pacer, sleep, waits) = paced_parts();
    let c = client(base, &pacer, sleep);
    let r = ChatRequest { effort: Some(Effort::High), ..req() };
    assert!(c.chat(&r).await.expect("chat").fallback_used);
    c.chat_stream(&r, &|_| {}).await.expect("stream");
    assert_eq!(*count.lock().expect("lock"), 4);
    assert_eq!(pacer.window(CONN, "m").len(), 4, "cada reenvio é um pedido na janela");
    assert!(waits.lock().expect("lock").is_empty(), "sem teto, sem espera");
}
