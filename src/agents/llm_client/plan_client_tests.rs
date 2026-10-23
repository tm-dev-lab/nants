//! Testes do modo plano ChatGPT do `LlmClient` (spec 116 T02) contra um provedor simulado local
//! (127.0.0.1, porta livre). Nenhuma rede real; o token é falso e montado em partes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::stream_tests::{ev, raw_server};
use super::tests::{body_of, head_of, RESP_TEXT};
use super::*;

const SSE: &str = "text/event-stream";
const JSON: &str = "application/json";

/// Token falso, montado em partes.
fn token(n: u32) -> String {
    format!("{}-{}-{n}", concat!("ac", "cess"), "plano")
}

/// Fonte de tokens simulada: devolve `token(n)`, conta as chamadas e registra os descartes e as
/// marcas de "precisa entrar de novo".
#[derive(Default)]
struct FakeTokens {
    calls: AtomicUsize,
    expired: AtomicUsize,
    needs_login: AtomicUsize,
    refuse: Option<PlanTokenError>,
}

impl PlanTokens for FakeTokens {
    fn access_token(&self) -> PlanFuture<'_, Result<String, PlanTokenError>> {
        Box::pin(async move {
            if let Some(err) = self.refuse {
                return Err(err);
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Depois de um descarte, o token "renovado" é outro.
            Ok(token(u32::try_from(self.expired.load(Ordering::SeqCst)).unwrap_or(0)))
        })
    }

    fn expire(&self) -> PlanFuture<'_, ()> {
        Box::pin(async move {
            self.expired.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn needs_login(&self) -> PlanFuture<'_, ()> {
        Box::pin(async move {
            self.needs_login.fetch_add(1, Ordering::SeqCst);
        })
    }
}

fn plan_client(base: String, tokens: Arc<FakeTokens>) -> LlmClient {
    LlmClient::new(base, String::new())
        .expect("cliente")
        .with_plan_tokens(tokens)
        .with_retry_delays(vec![Duration::ZERO; 3])
        .with_throttle_delays(vec![Duration::ZERO; 3])
}

fn request(web_search: bool) -> ChatRequest {
    ChatRequest {
        model: "gpt-plano".into(),
        messages: vec![
            ChatMessage::text(Role::System, "regras do papel"),
            ChatMessage::text(Role::User, "oi"),
            ChatMessage::text(Role::Assistant, "olá"),
            ChatMessage::text(Role::User, "de novo"),
        ],
        tools: vec![ToolSpec { name: "read_file".into(), description: "lê".into(), parameters: json!({"type": "object"}) }],
        effort: Some(Effort::Low),
        response_json: false,
        web_search,
    }
}

fn completed_sse() -> String {
    let mut s = ev("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"olá"}"#);
    s += &ev("response.completed", &format!(r#"{{"type":"response.completed","response":{RESP_TEXT}}}"#));
    s
}

fn error_body(code: &str, param: Option<&str>) -> String {
    json!({"error": {"code": code, "param": param, "message": "texto remoto"}}).to_string()
}

fn requests(cap: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    cap.lock().expect("lock").clone()
}

#[tokio::test]
async fn plan_call_streams_without_forbidden_fields_and_with_the_access_token() {
    let (base, cap) = raw_server(vec![(200, SSE, completed_sse())]).await;
    let tokens = Arc::new(FakeTokens::default());
    let client = plan_client(base, Arc::clone(&tokens));
    assert_eq!(client.billing(), Billing::ChatgptPlan);
    // `chat` (sem consumidor de deltas) também vai em streaming no modo plano.
    let resp = client.chat(&request(true)).await.expect("resposta");
    assert_eq!(resp.message.content, "olá");
    assert_eq!(resp.billing, Billing::ChatgptPlan);
    let reqs = requests(&cap);
    assert_eq!(reqs.len(), 1);
    let head = head_of(&reqs[0]).to_ascii_lowercase();
    assert!(head.starts_with("post /v1/responses"), "{head}");
    assert!(head.contains(&format!("authorization: bearer {}", token(0))), "{head}");
    let body = body_of(&reqs[0]);
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["stream"], json!(true));
    for field in PLAN_FORBIDDEN_FIELDS {
        assert!(body.get(field).is_none(), "{field}");
    }
    // `system` vai em `instructions`; nenhum item com `role: system`; o histórico inteiro em `input`.
    assert_eq!(body["instructions"], json!("regras do papel"));
    let input = body["input"].as_array().expect("input");
    assert_eq!(input.len(), 3);
    assert!(input.iter().all(|i| i["role"] != "system"));
    // Function tools e a busca na web seguem; nada de ferramenta hospedada de imagem.
    let kinds: Vec<&str> = body["tools"].as_array().into_iter().flatten().filter_map(|t| t["type"].as_str()).collect();
    assert_eq!(kinds, ["function", "web_search"]);
}

#[tokio::test]
async fn only_response_completed_counts_as_success() {
    // `response.incomplete` (mesmo por tamanho) não é sucesso no plano: repete e, esgotado, falha.
    let incomplete = ev(
        "response.incomplete",
        r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[]}}"#,
    );
    let (base, cap) = raw_server(vec![(200, SSE, incomplete.clone()), (200, SSE, incomplete.clone()), (200, SSE, incomplete)]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let err = client.chat_stream(&request(false), &|_| {}).await.expect_err("incompleta");
    assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::Incomplete), "{err:?}");
    assert_eq!(requests(&cap).len(), 3, "a resposta ruim repete como de costume (spec 088)");

    // Resposta JSON comum (sem stream) também não conta.
    let (base, _) = raw_server(vec![(200, JSON, RESP_TEXT.to_string()), (200, JSON, RESP_TEXT.to_string()), (200, JSON, RESP_TEXT.to_string())]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let err = client.chat(&request(false)).await.expect_err("sem stream");
    assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::NoFinal), "{err:?}");
}

#[tokio::test]
async fn usage_limit_pauses_without_retry_in_http_and_mid_stream() {
    let (base, cap) = raw_server(vec![(429, JSON, error_body("subscription_sharing_usage_limit_exceeded", None))]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let err = client.chat(&request(false)).await.expect_err("limite");
    assert!(matches!(err, LlmError::Plan(PlanError::UsageLimit)), "{err:?}");
    assert_eq!(requests(&cap).len(), 1, "sem nova tentativa automática");
    assert_eq!(provider_failure(&err).to_string(), "provider_plan_limit");

    let mut failed = ev("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"come"}"#);
    failed += &ev(
        "response.failed",
        r#"{"type":"response.failed","response":{"status":"failed","error":{"code":"subscription_sharing_usage_limit_exceeded","message":"x"}}}"#,
    );
    let (base, cap) = raw_server(vec![(200, SSE, failed)]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let err = client.chat_stream(&request(false), &|_| {}).await.expect_err("limite no stream");
    assert!(matches!(err, LlmError::Plan(PlanError::UsageLimit)), "{err:?}");
    assert_eq!(requests(&cap).len(), 1);
}

#[tokio::test]
async fn not_eligible_pauses_without_retry() {
    let (base, cap) = raw_server(vec![(403, JSON, error_body("subscription_sharing_user_not_eligible", None))]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let err = client.chat(&request(false)).await.expect_err("não elegível");
    assert!(matches!(err, LlmError::Plan(PlanError::NotEligible)), "{err:?}");
    assert_eq!(requests(&cap).len(), 1);
}

#[tokio::test]
async fn unavailable_retries_with_the_throttle_schedule() {
    let (base, cap) = raw_server(vec![
        (503, JSON, error_body("subscription_sharing_usage_unavailable", None)),
        (503, JSON, error_body("subscription_sharing_user_unavailable", None)),
        (200, SSE, completed_sse()),
    ])
    .await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let notices = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&notices);
    let resp = client
        .chat_stream(&request(false), &move |d| {
            if let StreamDelta::Retry(text) = d {
                seen.lock().expect("lock").push(text);
            }
        })
        .await
        .expect("depois da espera");
    assert_eq!(resp.message.content, "olá");
    assert_eq!(requests(&cap).len(), 3);
    assert!(notices.lock().expect("lock").iter().any(|n| n.starts_with("provedor no limite")), "aviso de espera");

    // Espera limitada: esgotado o cronograma, a falha sobe.
    let busy = || (503, JSON, error_body("subscription_sharing_usage_unavailable", None));
    let (base, cap) = raw_server(vec![busy(), busy(), busy(), busy()]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let err = client.chat(&request(false)).await.expect_err("indisponível");
    assert!(matches!(err, LlmError::Parse(r) if r.is_throttled()), "{err:?}");
    assert_eq!(requests(&cap).len(), 4, "1 + as 3 esperas do cronograma");
}

#[tokio::test]
async fn unsupported_capability_drops_the_param_once() {
    let (base, cap) = raw_server(vec![
        (400, JSON, error_body("subscription_sharing_unsupported_capability", Some("tools[1]"))),
        (200, SSE, completed_sse()),
    ])
    .await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    client.chat(&request(true)).await.expect("sem o item recusado");
    let reqs = requests(&cap);
    assert_eq!(reqs.len(), 2);
    let first = body_of(&reqs[0]);
    let second = body_of(&reqs[1]);
    assert_eq!(first["tools"].as_array().map(Vec::len), Some(2));
    assert_eq!(second["tools"].as_array().map(Vec::len), Some(1));
    assert_eq!(second["tools"][0]["type"], "function");
    assert_ne!(first, second, "o mesmo corpo não se repete");

    // Segunda recusa (ou item que não está no corpo): falha, sem repetir o mesmo corpo.
    let (base, cap) = raw_server(vec![
        (400, JSON, error_body("subscription_sharing_unsupported_capability", Some("tools[1]"))),
        (400, JSON, error_body("subscription_sharing_unsupported_capability", Some("tools[0]"))),
    ])
    .await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let err = client.chat(&request(true)).await.expect_err("recusa de novo");
    assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
    assert_eq!(requests(&cap).len(), 2);

    let (base, cap) = raw_server(vec![(400, JSON, error_body("subscription_sharing_unsupported_capability", Some("include")))]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    assert!(client.chat(&request(false)).await.is_err());
    assert_eq!(requests(&cap).len(), 1, "item fora do corpo: nada a retirar");
}

#[tokio::test]
async fn removed_item_stays_out_on_retries_of_the_same_call() {
    let (base, cap) = raw_server(vec![
        (400, JSON, error_body("subscription_sharing_unsupported_capability", Some("tools[1]"))),
        (503, JSON, error_body("subscription_sharing_usage_unavailable", None)),
        (200, SSE, completed_sse()),
    ])
    .await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    client.chat(&request(true)).await.expect("depois da espera");
    let reqs = requests(&cap);
    assert_eq!(reqs.len(), 3);
    assert_eq!(body_of(&reqs[0])["tools"].as_array().map(Vec::len), Some(2));
    for raw in &reqs[1..] {
        let tools = body_of(raw)["tools"].clone();
        assert_eq!(tools.as_array().map(Vec::len), Some(1), "{tools}");
        assert_eq!(tools[0]["type"], "function");
    }
}

#[tokio::test]
async fn invalid_user_marks_needs_login_and_pauses() {
    let (base, cap) = raw_server(vec![(401, JSON, error_body("subscription_sharing_invalid_user", None))]).await;
    let tokens = Arc::new(FakeTokens::default());
    let client = plan_client(base, Arc::clone(&tokens));
    let err = client.chat(&request(false)).await.expect_err("usuário inválido");
    assert!(matches!(err, LlmError::Plan(PlanError::NeedsLogin)), "{err:?}");
    assert_eq!(requests(&cap).len(), 1);
    assert_eq!(tokens.needs_login.load(Ordering::SeqCst), 1, "a conexão fica 'precisa entrar de novo'");
    assert_eq!(tokens.expired.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn plain_401_renews_the_token_once() {
    let (base, cap) = raw_server(vec![(401, JSON, r#"{"error":{"message":"expirado"}}"#.to_string()), (200, SSE, completed_sse())]).await;
    let tokens = Arc::new(FakeTokens::default());
    let client = plan_client(base, Arc::clone(&tokens));
    client.chat(&request(false)).await.expect("depois de renovar");
    let reqs = requests(&cap);
    assert_eq!(reqs.len(), 2);
    assert!(head_of(&reqs[1]).to_ascii_lowercase().contains(&format!("bearer {}", token(1))));
    assert_eq!(tokens.expired.load(Ordering::SeqCst), 1);
    assert_eq!(tokens.needs_login.load(Ordering::SeqCst), 0);

    // 401 de novo depois da renovação: precisa entrar de novo.
    let unauthorized = || (401, JSON, r#"{"error":{"message":"x"}}"#.to_string());
    let (base, cap) = raw_server(vec![unauthorized(), unauthorized()]).await;
    let tokens = Arc::new(FakeTokens::default());
    let client = plan_client(base, Arc::clone(&tokens));
    let err = client.chat(&request(false)).await.expect_err("401 de novo");
    assert!(matches!(err, LlmError::Plan(PlanError::NeedsLogin)), "{err:?}");
    assert_eq!(requests(&cap).len(), 2);
    assert_eq!(tokens.needs_login.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn token_refusals_map_without_any_request() {
    let (base, cap) = raw_server(vec![]).await;
    let tokens = Arc::new(FakeTokens { refuse: Some(PlanTokenError::NeedsLogin), ..FakeTokens::default() });
    let client = plan_client(base.clone(), tokens);
    let err = client.chat(&request(false)).await.expect_err("sem login");
    assert!(matches!(err, LlmError::Plan(PlanError::NeedsLogin)), "{err:?}");
    let tokens = Arc::new(FakeTokens { refuse: Some(PlanTokenError::Failed), ..FakeTokens::default() });
    let err = plan_client(base, tokens).chat(&request(false)).await.expect_err("local");
    assert!(matches!(err, LlmError::Config(_)), "{err:?}");
    assert!(requests(&cap).is_empty());
}

#[tokio::test]
async fn image_tool_is_not_offered_in_plan_mode() {
    let (base, cap) = raw_server(vec![]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let err = client.generate_image("gpt-plano", "um gato", &ImageOptions::default()).await.expect_err("sem imagem");
    assert!(matches!(err, LlmError::Parse(r) if r == IMAGE_UNSUPPORTED), "{err:?}");
    assert!(requests(&cap).is_empty(), "nenhuma requisição");
}

#[tokio::test]
async fn cut_by_size_keeps_the_diagnostic_with_plan_billing() {
    let mut big = ev("response.output_text.delta", r#"{"type":"response.output_text.delta","delta":"aaaa"}"#);
    big += &"x".repeat(4096);
    let (base, _) = raw_server(vec![(200, SSE, big.clone()), (200, SSE, big.clone()), (200, SSE, big)]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default())).with_max_response_bytes(1024);
    let cuts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&cuts);
    let err = client
        .chat_stream(&request(false), &move |d| {
            if let StreamDelta::Cut(cut) = d {
                seen.lock().expect("lock").push(cut);
            }
        })
        .await
        .expect_err("grande demais");
    assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::TooLarge), "{err:?}");
    let cuts = cuts.lock().expect("lock");
    assert_eq!(cuts.len(), 1);
    assert_eq!(cuts[0].reason, "too_large");
    assert_eq!(cuts[0].billing, Billing::ChatgptPlan);
}

#[tokio::test]
async fn models_list_uses_the_access_token() {
    let body = r#"{"models":[{"slug":"gpt-a","display_name":"GPT A","visibility":"list"}]}"#.to_string();
    let (base, cap) = raw_server(vec![(200, JSON, body)]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let value = client.get_json("/models").await.expect("lista");
    assert_eq!(value["models"][0]["slug"], "gpt-a");
    let reqs = requests(&cap);
    let head = head_of(&reqs[0]).to_ascii_lowercase();
    assert!(head.starts_with("get /v1/models"), "{head}");
    assert!(head.contains(&format!("bearer {}", token(0))), "{head}");
}

/// Spec 119: a sequência de eventos medida na API do plano (2026-10-23), para uma resposta só com
/// texto. A API devolve esse stream **sem `Content-Type`**.
fn measured_text_sse() -> String {
    let item = r#"{"type":"message","id":"msg_2","role":"assistant","status":"in_progress","content":[]}"#;
    let part = r#"{"type":"output_text","text":"","annotations":[]}"#;
    let mut s = ev("response.created", r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_2","object":"response","status":"in_progress","output":[]}}"#);
    s += &ev("response.in_progress", r#"{"type":"response.in_progress","sequence_number":1,"response":{"id":"resp_2","object":"response","status":"in_progress","output":[]}}"#);
    s += &ev("response.output_item.added", &format!(r#"{{"type":"response.output_item.added","sequence_number":2,"output_index":0,"item":{item}}}"#));
    s += &ev("response.content_part.added", &format!(r#"{{"type":"response.content_part.added","sequence_number":3,"output_index":0,"content_index":0,"item_id":"msg_2","part":{part}}}"#));
    s += &ev("response.output_text.delta", r#"{"type":"response.output_text.delta","sequence_number":4,"output_index":0,"content_index":0,"item_id":"msg_2","delta":"olá"}"#);
    s += &ev("response.metadata", r#"{"type":"response.metadata","sequence_number":5,"response_id":"resp_2","metadata":{}}"#);
    s += &ev("response.output_text.done", r#"{"type":"response.output_text.done","sequence_number":6,"output_index":0,"content_index":0,"item_id":"msg_2","logprobs":[],"text":"olá"}"#);
    s += &ev("response.content_part.done", r#"{"type":"response.content_part.done","sequence_number":7,"output_index":0,"content_index":0,"item_id":"msg_2","part":{"type":"output_text","text":"olá","annotations":[]}}"#);
    s += &ev("response.output_item.done", r#"{"type":"response.output_item.done","sequence_number":8,"output_index":0,"item":{"type":"message","id":"msg_2","role":"assistant","status":"completed","content":[{"type":"output_text","text":"olá","annotations":[]}]}}"#);
    s += &ev("response.completed", &format!(r#"{{"type":"response.completed","sequence_number":9,"response":{RESP_TEXT}}}"#));
    s
}

/// Spec 119: a mesma forma, com chamada de ferramenta (como as do Arquiteto e do Developer).
fn measured_tool_sse() -> String {
    let call = r#"{"type":"function_call","id":"fc_1","call_id":"call_abc","name":"read_file","arguments":"{\"path\":\"a.txt\"}","status":"completed"}"#;
    let mut s = ev("response.created", r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress","output":[]}}"#);
    s += &ev("response.in_progress", r#"{"type":"response.in_progress","sequence_number":1,"response":{"id":"resp_1","object":"response","status":"in_progress","output":[]}}"#);
    s += &ev("response.output_item.added", r#"{"type":"response.output_item.added","sequence_number":2,"output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_abc","name":"read_file","arguments":"","status":"in_progress"}}"#);
    s += &ev("response.function_call_arguments.delta", r#"{"type":"response.function_call_arguments.delta","sequence_number":3,"output_index":0,"item_id":"fc_1","delta":"{\"path\":\"a.txt\"}"}"#);
    s += &ev("response.metadata", r#"{"type":"response.metadata","sequence_number":4,"response_id":"resp_1","metadata":{}}"#);
    s += &ev("response.function_call_arguments.done", r#"{"type":"response.function_call_arguments.done","sequence_number":5,"output_index":0,"item_id":"fc_1","arguments":"{\"path\":\"a.txt\"}"}"#);
    s += &ev("response.output_item.done", &format!(r#"{{"type":"response.output_item.done","sequence_number":6,"output_index":0,"item":{call}}}"#));
    s += &ev("response.completed", &format!(r#"{{"type":"response.completed","sequence_number":7,"response":{}}}"#, super::tests::RESP_TOOL));
    s
}

/// Sem `Content-Type` na resposta (spec 119).
const NO_CTYPE: &str = "";

#[tokio::test]
async fn plan_stream_without_content_type_completes() {
    // Três cópias: sem a correção, cada uma vira `NoFinal` (repetível) e o erro final é esse.
    let (base, cap) = raw_server(vec![(200, NO_CTYPE, measured_text_sse()); 3]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&deltas);
    let resp = client
        .chat_stream(&request(false), &move |d| {
            if let StreamDelta::Text(t) = d {
                seen.lock().expect("lock").push(t);
            }
        })
        .await
        .expect("o stream sem cabeçalho conclui");
    assert_eq!(requests(&cap).len(), 1, "conclui na primeira resposta");
    assert_eq!(resp.message.content, "olá");
    assert_eq!(*deltas.lock().expect("lock"), ["olá"]);
    assert_eq!(resp.billing, Billing::ChatgptPlan);

    // `chat` (sem consumidor de deltas) também conclui.
    let (base, _) = raw_server(vec![(200, NO_CTYPE, measured_text_sse())]).await;
    let resp = plan_client(base, Arc::new(FakeTokens::default())).chat(&request(false)).await.expect("chat");
    assert_eq!(resp.message.content, "olá");
}

#[tokio::test]
async fn plan_stream_without_content_type_completes_with_tools() {
    let (base, _) = raw_server(vec![(200, NO_CTYPE, measured_tool_sse()); 3]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let resp = client.chat_stream(&request(true), &|_| {}).await.expect("com ferramenta");
    assert_eq!(resp.message.tool_calls.len(), 1);
    assert_eq!(resp.message.tool_calls[0].name, "read_file");
}

#[tokio::test]
async fn plan_failures_mid_stream_are_recognized_without_content_type() {
    let mut failed = ev("response.created", r#"{"type":"response.created","response":{"status":"in_progress"}}"#);
    failed += &ev(
        "response.failed",
        r#"{"type":"response.failed","response":{"status":"failed","error":{"code":"subscription_sharing_usage_limit_exceeded","message":"x"}}}"#,
    );
    let (base, cap) = raw_server(vec![(200, NO_CTYPE, failed)]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let err = client.chat_stream(&request(false), &|_| {}).await.expect_err("limite no stream");
    assert!(matches!(err, LlmError::Plan(PlanError::UsageLimit)), "{err:?}");
    assert_eq!(requests(&cap).len(), 1);
}

#[test]
fn plan_connection_needs_the_token_source() {
    let conn = Connection::chatgpt_plan("c-plano", "ChatGPT");
    assert!(matches!(LlmClient::from_connection(&conn), Err(LlmError::Config(_))));
    let client = LlmClient::from_plan_connection(&conn, Arc::new(FakeTokens::default())).expect("cliente");
    assert!(client.is_plan() && client.is_paced(), "modo plano no ritmo da 089");
}
