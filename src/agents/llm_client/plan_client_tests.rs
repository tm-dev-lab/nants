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
        cache_key: None,
        web_search_max_uses: None,
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

// Spec 120: no modo plano, `response.completed` traz `output: []`; a saída só vem nos itens de
// `response.output_item.done` (medido em 2026-10-23, `119-probe/probe2.txt`).

/// `usage` como a sonda mediu (com `output_tokens_details.reasoning_tokens`).
const PLAN_USAGE: &str = r#"{"input_tokens":44,"input_tokens_details":{"cached_tokens":0},"output_tokens":14,"output_tokens_details":{"reasoning_tokens":3},"total_tokens":58}"#;

fn item_done(index: u64, item: &str) -> String {
    ev(
        "response.output_item.done",
        &format!(r#"{{"type":"response.output_item.done","sequence_number":{},"output_index":{index},"item":{item}}}"#, 10 + index),
    )
}

fn message_item(id: &str, phase: &str, text: &str) -> String {
    format!(
        r#"{{"id":"{id}","type":"message","status":"completed","content":[{{"type":"output_text","annotations":[],"logprobs":[],"text":"{text}"}}],"phase":"{phase}","role":"assistant"}}"#
    )
}

const GET_TIME_CALL: &str = r#"{"id":"fc_0ff7","type":"function_call","status":"completed","arguments":"{}","call_id":"call_b5Ag","name":"get_time"}"#;

/// Stream na forma medida: deltas de texto, os itens completos (na ordem dada; o índice vem do
/// `output_index`) e um `response.completed` com `output: []` e o `usage`.
fn plan_items_sse(items: &[(u64, String)], deltas: &[&str]) -> String {
    let mut s = ev("response.created", r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_p","object":"response","status":"in_progress","output":[]}}"#);
    for d in deltas {
        s += &ev("response.output_text.delta", &json!({"type": "response.output_text.delta", "delta": d}).to_string());
    }
    for (i, item) in items {
        s += &item_done(*i, item);
    }
    s += &ev("response.metadata", r#"{"type":"response.metadata","sequence_number":30,"response_id":"resp_p","metadata":{}}"#);
    s += &ev(
        "response.completed",
        &format!(r#"{{"type":"response.completed","sequence_number":31,"response":{{"id":"resp_p","object":"response","status":"completed","error":null,"incomplete_details":null,"output":[],"usage":{PLAN_USAGE}}}}}"#),
    );
    s
}

/// Chama no modo plano com o servidor local sem `Content-Type` e devolve a resposta, os cortes
/// entregues (o diagnóstico do `cuts.jsonl`) e o número de chamadas. Três cópias: sem a correção,
/// cada uma vira `empty`.
async fn plan_items_call(sse: String) -> (Result<ChatResponse, LlmError>, Vec<Arc<StreamCut>>, usize) {
    let (base, cap) = raw_server(vec![(200, NO_CTYPE, sse); 3]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let cuts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&cuts);
    let out = client
        .chat_stream(&request(true), &move |d| {
            if let StreamDelta::Cut(c) = d {
                seen.lock().expect("lock").push(c);
            }
        })
        .await;
    let cuts = cuts.lock().expect("lock").clone();
    (out, cuts, requests(&cap).len())
}

fn assert_plan_usage(resp: &ChatResponse) {
    let usage = resp.usage.expect("usage do completed");
    assert_eq!((usage.prompt, usage.completion, usage.reasoning), (44, 14, Some(3)));
}

fn cut_reasons(cuts: &[Arc<StreamCut>]) -> Vec<String> {
    cuts.iter().map(|c| c.reason.to_string()).collect()
}

#[tokio::test]
async fn plan_text_is_built_from_output_items() {
    let sse = plan_items_sse(&[(0, message_item("msg_1", "final_answer", "OK."))], &["OK."]);
    let (out, cuts, calls) = plan_items_call(sse).await;
    let resp = out.expect("texto montado pelos itens");
    assert_eq!(resp.message.content, "OK.");
    assert!(resp.message.tool_calls.is_empty());
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    assert_plan_usage(&resp);
    assert_eq!(resp.billing, Billing::ChatgptPlan);
    assert_eq!(calls, 1, "conclui na primeira resposta");
    assert!(cuts.is_empty(), "nenhum `empty` no diagnóstico: {:?}", cut_reasons(&cuts));
}

#[tokio::test]
async fn plan_tool_call_is_built_from_output_items() {
    let (out, cuts, calls) = plan_items_call(plan_items_sse(&[(0, GET_TIME_CALL.to_string())], &[])).await;
    let resp = out.expect("ferramenta montada pelos itens");
    assert_eq!(resp.message.tool_calls.len(), 1);
    let call = &resp.message.tool_calls[0];
    assert_eq!((call.id.as_str(), call.name.as_str(), call.arguments.as_str()), ("call_b5Ag", "get_time", "{}"));
    assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
    // O item volta ao histórico como veio (com `call_id`), para a próxima chamada.
    let items = resp.message.provider_items.as_ref().expect("itens");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["call_id"], "call_b5Ag");
    assert_plan_usage(&resp);
    assert_eq!(calls, 1);
    assert!(cuts.is_empty(), "{:?}", cut_reasons(&cuts));
}

#[tokio::test]
async fn plan_text_then_tool_keep_the_output_index_order() {
    // Os eventos chegam fora de ordem; vale o `output_index`.
    let items = [(1, GET_TIME_CALL.to_string()), (0, message_item("msg_1", "commentary", "Vou ver a hora."))];
    let (out, cuts, _) = plan_items_call(plan_items_sse(&items, &["Vou ver a hora."])).await;
    let resp = out.expect("texto e ferramenta");
    assert_eq!(resp.message.content, "Vou ver a hora.");
    assert_eq!(resp.message.tool_calls.len(), 1);
    assert_eq!(resp.message.tool_calls[0].name, "get_time");
    let items = resp.message.provider_items.as_ref().expect("itens");
    let kinds: Vec<&str> = items.iter().filter_map(|i| i["type"].as_str()).collect();
    assert_eq!(kinds, ["message", "function_call"]);
    assert_plan_usage(&resp);
    assert!(cuts.is_empty(), "{:?}", cut_reasons(&cuts));
}

#[tokio::test]
async fn plan_commentary_and_final_answer_join_in_order() {
    // Decisão: as mensagens se juntam na ordem, pela mesma regra do `parse_responses` para um
    // `output` preenchido (sem filtrar `phase`). Spec 126 T04: com uma quebra de parágrafo entre elas.
    let items = [
        (0, message_item("msg_c", "commentary", "Vou pensar. ")),
        (1, message_item("msg_f", "final_answer", "Resposta.")),
    ];
    let (out, cuts, _) = plan_items_call(plan_items_sse(&items, &["Vou pensar. ", "Resposta."])).await;
    let resp = out.expect("as duas mensagens");
    assert_eq!(resp.message.content, "Vou pensar.\n\nResposta.");
    // Os itens guardados mantêm o `phase` de cada mensagem.
    let items = resp.message.provider_items.as_ref().expect("itens");
    let phases: Vec<&str> = items.iter().filter_map(|i| i["phase"].as_str()).collect();
    assert_eq!(phases, ["commentary", "final_answer"]);
    assert!(cuts.is_empty(), "{:?}", cut_reasons(&cuts));
}

#[tokio::test]
async fn plan_without_any_item_is_still_empty() {
    // Sem itens e com `output: []`, continua sendo resposta vazia (e o diagnóstico registra).
    let (out, cuts, calls) = plan_items_call(plan_items_sse(&[], &[])).await;
    let err = out.expect_err("vazia");
    assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::Empty), "{err:?}");
    assert_eq!(calls, 3, "vazia repete como de costume");
    assert!(!cuts.is_empty() && cuts.iter().all(|c| c.reason == "empty"), "{:?}", cut_reasons(&cuts));
}

/// Fonte de token para o teste real: o token lido do arquivo, sem renovar nem marcar nada.
struct LiveToken(String);

impl PlanTokens for LiveToken {
    fn access_token(&self) -> PlanFuture<'_, Result<String, PlanTokenError>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
    fn expire(&self) -> PlanFuture<'_, ()> {
        Box::pin(async {})
    }
    fn needs_login(&self) -> PlanFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// Spec 120: verificação real contra a API do plano, com o cliente do Nants (texto, JSON, ferramenta e o
/// segundo turno com os itens reenviados, sem esforço e com esforço `low`). Só roda à mão:
/// `NANTS_PLAN_LIVE_FILE=<chatgpt-conn-*.json> scripts/test.sh cargo test --lib plan_live_text_and_tool -- --ignored --nocapture`.
/// Lê o `access_token` do arquivo; com menos de 2 minutos de validade, não chama nem renova. Nunca
/// imprime o token. Modelo: `NANTS_PLAN_LIVE_MODEL` (padrão `gpt-6.1-sol`). Sem novas tentativas.
#[tokio::test]
#[ignore = "chama a API real do plano ChatGPT com o token do usuário"]
async fn plan_live_text_and_tool() {
    let Ok(path) = std::env::var("NANTS_PLAN_LIVE_FILE") else {
        panic!("defina NANTS_PLAN_LIVE_FILE com o caminho do chatgpt-conn-*.json");
    };
    let raw = std::fs::read_to_string(&path).expect("leitura do arquivo de credenciais");
    let creds: Value = serde_json::from_str(&raw).expect("credenciais em JSON");
    let access = creds["access_token"].as_str().unwrap_or_default().to_string();
    assert!(!access.is_empty(), "o arquivo não tem access_token (entre de novo pelo Nants)");
    let expires_at = creds["expires_at"].as_u64().unwrap_or(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if expires_at < now.saturating_add(120) {
        eprintln!("token vencido ou a menos de 2 minutos de vencer: nenhuma chamada feita e nada renovado. Use o Nants uma vez para renovar e rode de novo.");
        return;
    }
    let model = std::env::var("NANTS_PLAN_LIVE_MODEL").unwrap_or_else(|_| "gpt-6.1-sol".into());
    let conn = Connection::chatgpt_plan("c-live", "ChatGPT");
    let client = LlmClient::from_plan_connection(&conn, Arc::new(LiveToken(access)))
        .expect("cliente")
        .with_retry_delays(Vec::new())
        .with_throttle_delays(Vec::new());

    // Texto.
    let text_req = ChatRequest {
        model: model.clone(),
        messages: vec![ChatMessage::text(Role::User, "Say OK.")],
        tools: Vec::new(),
        effort: None,
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let resp = client.chat_stream(&text_req, &|_| {}).await.unwrap_or_else(|e| panic!("texto: {e:?}"));
    eprintln!("texto: {:?} usage={:?}", resp.message.content, resp.usage);
    assert!(!resp.message.content.trim().is_empty(), "texto vazio");

    // Spec 129 T03: resposta em JSON com a palavra só no SYSTEM (vai em `instructions`) e o usuário
    // sem ela, como o `ask_architect`. Sem a linha acrescentada pelo cliente, a API dá 400.
    let json_req = ChatRequest {
        model: model.clone(),
        messages: vec![
            ChatMessage::text(Role::System, "Reply ONLY with a JSON object: {\"ok\": true}."),
            ChatMessage::text(Role::User, "Are you there?"),
        ],
        tools: Vec::new(),
        effort: None,
        response_json: true,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let resp = client.chat_stream(&json_req, &|_| {}).await.unwrap_or_else(|e| panic!("json: {e:?}"));
    eprintln!("json: {:?} usage={:?}", resp.message.content, resp.usage);
    let parsed: Value = serde_json::from_str(resp.message.content.trim()).unwrap_or_else(|e| panic!("json inválido: {e}"));
    assert!(parsed.is_object(), "json sem objeto: {parsed}");

    // Ferramenta (`tool_choice` obrigatório) e o segundo turno, que reenvia os itens devolvidos
    // (`provider_items`) e o resultado da ferramenta: sem esforço e com esforço `low` (como o
    // Arquiteto), para aparecerem itens `reasoning`. Se a forma de hoje for recusada, tenta o
    // ajuste preparado (`plan::replay_without_ids`) só para mostrar se ele resolve.
    let mut failures = Vec::new();
    for (label, effort) in [("sem esforço", None), ("esforço low", Some(Effort::Low))] {
        if let Err(e) = live_round_trip(&client, &model, effort, false, label).await {
            eprintln!("{label}: FALHOU na forma de hoje: {e}");
            let fixed_label = format!("{label} + ajuste preparado");
            match live_round_trip(&client, &model, effort, true, &fixed_label).await {
                Ok(()) => eprintln!("{fixed_label}: passou"),
                Err(e) => eprintln!("{fixed_label}: também falhou: {e}"),
            }
            failures.push(label);
        }
    }
    assert!(failures.is_empty(), "segundo turno recusado na forma de hoje: {failures:?}");
}

fn live_get_time() -> ToolSpec {
    ToolSpec {
        name: "get_time".into(),
        description: "Returns the current time.".into(),
        parameters: json!({"type": "object", "properties": {}, "additionalProperties": false}),
    }
}

/// Teste real: chamada com a ferramenta obrigatória e o segundo turno com os itens reenviados.
/// Imprime os `type` dos itens reenviados (e se trazem `id` ou conteúdo cifrado), nunca o token.
async fn live_round_trip(base: &LlmClient, model: &str, effort: Option<Effort>, fix: bool, label: &str) -> Result<(), String> {
    let first = LlmClient { test_tool_choice: Some(json!("required")), test_plan_replay_fix: fix, ..base.clone() };
    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![ChatMessage::text(Role::User, "What time is it?")],
        tools: vec![live_get_time()],
        effort,
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let resp = first.chat_stream(&req, &|_| {}).await.map_err(|e| format!("1º turno: {e:?}"))?;
    let calls: Vec<(&str, &str, &str)> =
        resp.message.tool_calls.iter().map(|c| (c.name.as_str(), c.id.as_str(), c.arguments.as_str())).collect();
    eprintln!("{label}: ferramenta {calls:?} usage={:?}", resp.usage);
    let call = resp.message.tool_calls.first().cloned().ok_or("1º turno sem chamada de ferramenta")?;
    if call.name != "get_time" || call.id.is_empty() {
        return Err(format!("chamada inesperada: {:?} / call_id vazio: {}", call.name, call.id.is_empty()));
    }
    let resent: Vec<String> = resp
        .message
        .provider_items
        .iter()
        .flatten()
        .map(|i| {
            let mut tags = vec![i["type"].as_str().unwrap_or("?").to_string()];
            if i.get("id").is_some() {
                tags.push("id".into());
            }
            if i.get("phase").is_some() {
                tags.push(format!("phase={}", i["phase"].as_str().unwrap_or("?")));
            }
            if i.get("encrypted_content").is_some_and(|c| !c.is_null()) {
                tags.push("encrypted_content".into());
            }
            tags.join("+")
        })
        .collect();
    eprintln!("{label}: itens reenviados no 2º turno = {resent:?}");
    let mut messages = req.messages.clone();
    messages.push(resp.message.clone());
    messages.push(ChatMessage { tool_call_id: Some(call.id.clone()), ..ChatMessage::text(Role::Tool, "2026-10-23T12:00:00Z") });
    let second = LlmClient { test_tool_choice: None, test_plan_replay_fix: fix, ..base.clone() };
    let resp = second
        .chat_stream(&ChatRequest { messages, ..req }, &|_| {})
        .await
        .map_err(|e| format!("2º turno: {e:?}"))?;
    eprintln!("{label}: 2º turno texto={:?} usage={:?}", resp.message.content, resp.usage);
    if resp.message.content.trim().is_empty() {
        return Err("2º turno sem texto".into());
    }
    Ok(())
}

/// Spec 120 (F-01): a forma exata do `input` do segundo turno no modo plano, com os itens montados
/// pelo `output_item.done` (inclusive um `reasoning` sem conteúdo cifrado) reenviados como vieram.
#[tokio::test]
async fn plan_second_turn_resends_the_items_as_they_came() {
    let reasoning = r#"{"id":"rs_1","type":"reasoning","summary":[]}"#.to_string();
    let first = plan_items_sse(
        &[(0, reasoning), (1, message_item("msg_c", "commentary", "Vou ver.")), (2, GET_TIME_CALL.to_string())],
        &["Vou ver."],
    );
    let second = plan_items_sse(&[(0, message_item("msg_f", "final_answer", "São 12h."))], &["São 12h."]);
    let (base, cap) = raw_server(vec![(200, NO_CTYPE, first), (200, NO_CTYPE, second)]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let req = ChatRequest {
        model: "gpt-plano".into(),
        messages: vec![ChatMessage::text(Role::User, "que horas são?")],
        tools: vec![ToolSpec { name: "get_time".into(), description: "hora".into(), parameters: json!({"type": "object"}) }],
        effort: Some(Effort::Low),
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let resp = client.chat_stream(&req, &|_| {}).await.expect("1º turno");
    let mut messages = req.messages.clone();
    messages.push(resp.message.clone());
    messages.push(ChatMessage { tool_call_id: Some("call_b5Ag".into()), ..ChatMessage::text(Role::Tool, "12:00") });
    let resp = client.chat_stream(&ChatRequest { messages, ..req }, &|_| {}).await.expect("2º turno");
    assert_eq!(resp.message.content, "São 12h.");
    let reqs = requests(&cap);
    let body = body_of(&reqs[1]);
    assert_eq!(
        body["input"],
        json!([
            {"role": "user", "content": "que horas são?"},
            {"id": "rs_1", "type": "reasoning", "summary": []},
            {"id": "msg_c", "type": "message", "status": "completed", "content": [{"type": "output_text", "annotations": [], "logprobs": [], "text": "Vou ver."}], "phase": "commentary", "role": "assistant"},
            {"id": "fc_0ff7", "type": "function_call", "status": "completed", "arguments": "{}", "call_id": "call_b5Ag", "name": "get_time"},
            {"type": "function_call_output", "call_id": "call_b5Ag", "output": "12:00"}
        ])
    );
    assert!(body.get("include").is_none(), "o ajuste preparado não está ligado");

    // Com o ajuste preparado (só nos testes até o teste real decidir): sem `id`/`status`, sem o
    // `reasoning` não reenviável e com o pedido do conteúdo cifrado.
    let first = plan_items_sse(&[(0, GET_TIME_CALL.to_string())], &[]);
    let second = plan_items_sse(&[(0, message_item("msg_f", "final_answer", "ok"))], &["ok"]);
    let (base, cap) = raw_server(vec![(200, NO_CTYPE, first), (200, NO_CTYPE, second)]).await;
    let client = LlmClient { test_plan_replay_fix: true, ..plan_client(base, Arc::new(FakeTokens::default())) };
    let req = ChatRequest {
        model: "gpt-plano".into(),
        messages: vec![ChatMessage::text(Role::User, "que horas são?")],
        tools: vec![ToolSpec { name: "get_time".into(), description: "hora".into(), parameters: json!({"type": "object"}) }],
        effort: None,
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let resp = client.chat_stream(&req, &|_| {}).await.expect("1º turno");
    let mut messages = req.messages.clone();
    messages.push(resp.message.clone());
    messages.push(ChatMessage { tool_call_id: Some("call_b5Ag".into()), ..ChatMessage::text(Role::Tool, "12:00") });
    client.chat_stream(&ChatRequest { messages, ..req }, &|_| {}).await.expect("2º turno");
    let body = body_of(&requests(&cap)[1]);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["input"][1], json!({"type": "function_call", "arguments": "{}", "call_id": "call_b5Ag", "name": "get_time"}));
}

/// Spec 122 T05: o plano leva a chave de cache; um 400 comum que a recusa repete uma vez sem ela.
#[tokio::test]
async fn plan_body_carries_the_cache_key_and_drops_it_when_refused() {
    let refusal = json!({"error": {"message": "Unsupported parameter: prompt_cache_key"}}).to_string();
    let (base, cap) = raw_server(vec![(400, JSON, refusal), (200, SSE, completed_sse())]).await;
    let client = plan_client(base, Arc::new(FakeTokens::default()));
    let keyed = ChatRequest { cache_key: Some("nants-plano".into()), ..request(false) };
    client.chat_stream(&keyed, &|_| {}).await.expect("resposta");
    let reqs = requests(&cap);
    assert_eq!(reqs.len(), 2);
    assert_eq!(body_of(&reqs[0])["prompt_cache_key"], json!("nants-plano"));
    let second = body_of(&reqs[1]);
    assert!(second.get("prompt_cache_key").is_none());
    assert_eq!(second["stream"], json!(true));
}
