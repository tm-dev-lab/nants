//! Testes do adaptador Anthropic Messages (spec 132 T03 e T04), com servidor falso: o corpo exato,
//! o reenvio do thinking com a assinatura, a escada de relaxamento, a redução do `max_tokens`, o uso
//! somado e o stream gravado, que reproduz a resposta sem stream. O teste ao vivo é `#[ignore]` e só
//! roda com a variável da chave.

use super::anthropic::{self, DEFAULT_MAX_TOKENS};
use super::live_tests::{self, Live, Provider};
use super::stream_tests::{ev, raw_server};
use super::tests::{body_of, head_of, spawn_server};
use super::*;
use serde_json::json;

const SSE: &str = "text/event-stream";

/// Chave falsa montada em tempo de execução (nada com cara de segredo no código).
fn fake_key() -> String {
    ["chave", "anthropic", "falsa", "0077"].join("-")
}

/// Cliente Anthropic para o servidor falso (o endereço sem o `/v1` que o servidor acrescenta).
fn client(base: &str) -> LlmClient {
    let base = base.trim_end_matches("/v1").to_string();
    LlmClient::new(base, fake_key()).expect("client").with_kind(ConnectionKind::Anthropic)
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn tool() -> ToolSpec {
    ToolSpec {
        name: "read_file".into(),
        description: "Lê um arquivo".into(),
        parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
    }
}

fn req(messages: Vec<ChatMessage>, effort: Option<Effort>) -> ChatRequest {
    ChatRequest {
        model: "claude-teste".into(),
        messages,
        tools: vec![tool()],
        effort,
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    }
}

fn tool_result(id: &str, content: &str) -> ChatMessage {
    ChatMessage { tool_call_id: Some(id.into()), ..ChatMessage::text(Role::Tool, content) }
}

const TEXT_REPLY: &str = r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-teste","content":[{"type":"text","text":"olá"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":3}}"#;

/// Resposta com thinking assinado, texto e uma chamada.
const TOOL_REPLY: &str = r#"{"id":"msg_2","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"preciso ler","signature":"ASSINATURA-opaca-1"},{"type":"redacted_thinking","data":"REDIGIDO-opaco-2"},{"type":"text","text":"vou ler"},{"type":"tool_use","id":"toolu_01","name":"read_file","input":{"path":"a.txt"}}],"stop_reason":"tool_use","usage":{"input_tokens":20,"cache_read_input_tokens":100,"cache_creation_input_tokens":30,"output_tokens":15}}"#;

#[tokio::test]
async fn request_goes_to_messages_with_the_anthropic_headers() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    let resp = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], None)).await.expect("chat");
    assert_eq!(resp.message.content, "olá");
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    let raw = cap.lock().expect("lock")[0].clone();
    let head = head_of(&raw).to_ascii_lowercase();
    assert!(head.starts_with("post /v1/messages http/1.1"), "{head}");
    assert!(head.contains(&format!("x-api-key: {}", fake_key())));
    assert!(head.contains("anthropic-version: 2023-06-01"));
    assert!(head.contains("content-type: application/json"));
    assert!(!head.contains("authorization:"));
}

/// O corpo exato: `system` separado, imagem em base64, `tool_use` remontado, `tool_result` numa
/// mensagem `user` (antes do texto que vem junto), `max_tokens`, thinking adaptativo com o esforço e
/// o `cache_control` nas três âncoras.
#[tokio::test]
async fn exact_body_from_the_chat_request() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    let assistant = ChatMessage {
        tool_calls: vec![ToolCall { id: "toolu_9".into(), name: "read_file".into(), arguments: r#"{"path":"a.txt"}"#.into() }],
        ..ChatMessage::text(Role::Assistant, "vou ler")
    };
    let messages = vec![
        ChatMessage::text(Role::System, "seja breve"),
        ChatMessage::with_images("veja", vec![ImageInput::new("image/png", vec![1u8, 2, 3])]),
        assistant,
        tool_result("toolu_9", "conteúdo"),
        ChatMessage::text(Role::User, "continue"),
    ];
    client(&base).chat(&req(messages, Some(Effort::Low))).await.expect("chat");
    let body = body_of(&cap.lock().expect("lock")[0]);
    let eph = json!({"type":"ephemeral"});
    let expected = json!({
        "model": "claude-teste",
        "max_tokens": DEFAULT_MAX_TOKENS,
        "system": [{"type":"text","text":"seja breve","cache_control":eph}],
        "messages": [
            {"role":"user","content":[
                {"type":"text","text":"veja"},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"}}
            ]},
            {"role":"assistant","content":[
                {"type":"text","text":"vou ler"},
                {"type":"tool_use","id":"toolu_9","name":"read_file","input":{"path":"a.txt"}}
            ]},
            {"role":"user","content":[
                {"type":"tool_result","tool_use_id":"toolu_9","content":"conteúdo"},
                {"type":"text","text":"continue","cache_control":eph}
            ]}
        ],
        "tools": [{"name":"read_file","description":"Lê um arquivo","input_schema":tool().parameters,"cache_control":eph}],
        "thinking": {"type":"adaptive","display":"summarized"},
        "output_config": {"effort":"low"}
    });
    assert_eq!(body, expected);
}

#[tokio::test]
async fn without_effort_there_is_no_thinking_nor_output_config() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    let resp = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], None)).await.expect("chat");
    assert!(!resp.fallback_used);
    let body = body_of(&cap.lock().expect("lock")[0]);
    assert!(body.get("thinking").is_none() && body.get("output_config").is_none(), "{body}");
    assert!(body.get("system").is_none());
    assert!(body.get("tool_choice").is_none());
    assert!(body.get("stream").is_none());
}

/// A segunda volta reenvia os blocos de thinking com a assinatura e o `tool_use` inalterados, e o
/// uso soma a entrada com a leitura e a escrita do cache.
#[tokio::test]
async fn second_turn_resends_thinking_with_signature_and_usage_is_summed() {
    let (base, cap) = spawn_server(vec![(200, TOOL_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let mut messages = vec![ChatMessage::text(Role::User, "leia a.txt")];
    let first = c.chat(&req(messages.clone(), Some(Effort::Low))).await.expect("primeira");
    assert_eq!(first.message.content, "vou ler");
    assert_eq!(first.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(first.message.tool_calls, vec![ToolCall { id: "toolu_01".into(), name: "read_file".into(), arguments: r#"{"path":"a.txt"}"#.into() }]);
    let usage = first.usage.expect("uso");
    assert_eq!((usage.prompt, usage.completion, usage.cached, usage.cache_write), (150, 15, Some(100), Some(30)));
    assert!(!format!("{:?}", first.message).contains("ASSINATURA"), "itens opacos fora do Debug");

    messages.push(first.message.clone());
    messages.push(tool_result("toolu_01", "linha"));
    c.chat(&req(messages, Some(Effort::Low))).await.expect("segunda");
    let body = body_of(&cap.lock().expect("lock")[1]);
    let original: Value = serde_json::from_str(TOOL_REPLY).expect("json");
    assert_eq!(body["messages"][1], json!({"role":"assistant","content":original["content"]}));
    assert_eq!(body["messages"][2]["content"][0], json!({"type":"tool_result","tool_use_id":"toolu_01","content":"linha","cache_control":{"type":"ephemeral"}}));
}

/// Escada: `adaptive` recusado vira `enabled` com `budget_tokens` (>= 1024 e menor que o
/// `max_tokens`); recusado de novo, sem thinking. O `output_config` fica.
#[tokio::test]
async fn thinking_ladder_goes_adaptive_enabled_then_off() {
    let adaptive = r#"{"type":"error","error":{"type":"invalid_request_error","message":"thinking.type: Input tag 'adaptive' does not match any of the expected tags"}}"#;
    let enabled = r#"{"type":"error","error":{"type":"invalid_request_error","message":"thinking is not supported for this model"}}"#;
    let (base, cap) = spawn_server(vec![(400, adaptive), (400, enabled), (200, TEXT_REPLY)]).await;
    let resp = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::Medium))).await.expect("chat");
    assert!(resp.fallback_used);
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0]["thinking"], json!({"type":"adaptive","display":"summarized"}));
    assert_eq!(bodies[1]["thinking"], json!({"type":"enabled","budget_tokens":8192,"display":"summarized"}));
    let budget = bodies[1]["thinking"]["budget_tokens"].as_u64().expect("budget");
    assert!(budget >= 1024 && budget < bodies[1]["max_tokens"].as_u64().expect("max"));
    assert!(bodies[2].get("thinking").is_none());
    assert_eq!(bodies[2]["output_config"], json!({"effort":"medium"}));
}

#[tokio::test]
async fn ladder_drops_the_effort_when_output_config_is_refused_and_stops_there() {
    let refused = r#"{"type":"error","error":{"type":"invalid_request_error","message":"output_config: Extra inputs are not permitted"}}"#;
    let (base, cap) = spawn_server(vec![(400, refused), (400, refused)]).await;
    let err = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::High))).await.expect_err("400");
    assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies.len(), 2, "cada recusa relaxa uma vez");
    assert!(bodies[1].get("output_config").is_none());
    assert_eq!(bodies[1]["thinking"], json!({"type":"adaptive","display":"summarized"}));
}

/// T04: o `display` recusado sai sozinho, antes de mexer no thinking.
#[tokio::test]
async fn ladder_drops_the_display_first() {
    let refused = r#"{"type":"error","error":{"type":"invalid_request_error","message":"thinking.adaptive.display: Extra inputs are not permitted"}}"#;
    let (base, cap) = spawn_server(vec![(400, refused), (200, TEXT_REPLY)]).await;
    let resp = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::Low))).await.expect("chat");
    assert!(!resp.fallback_used);
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[1]["thinking"], json!({"type":"adaptive"}));
    assert_eq!(bodies[1]["output_config"], json!({"effort":"low"}));
}

#[tokio::test]
async fn unrelated_400_is_not_relaxed() {
    let other = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages: roles must alternate"}}"#;
    let (base, cap) = spawn_server(vec![(400, other)]).await;
    let err = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::Low))).await.expect_err("400");
    assert!(matches!(err, LlmError::Http { status: 400, .. }));
    assert_eq!(cap.lock().expect("lock").len(), 1);
}

/// O 400 que cita `max_tokens` reduz uma vez para o limite indicado, e o valor aprendido vale nas
/// chamadas seguintes do mesmo modelo.
#[tokio::test]
async fn max_tokens_is_reduced_once_and_remembered() {
    let too_big = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 32000 > 8192, which is the maximum allowed number of output tokens for claude-teste"}}"#;
    let (base, cap) = spawn_server(vec![(400, too_big), (200, TEXT_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let r = req(vec![ChatMessage::text(Role::User, "oi")], None);
    c.chat(&r).await.expect("chat");
    c.chat(&r).await.expect("chat de novo");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0]["max_tokens"], 32_000);
    assert_eq!(bodies[1]["max_tokens"], 8_192);
    assert_eq!(bodies[2]["max_tokens"], 8_192, "o limite aprendido vale na próxima chamada");
}

/// F-01: o estouro do contexto cita `max_tokens`, mas não é o limite de saída do modelo: não reduz,
/// não memoriza e é reconhecido como estouro (o resumo do histórico dispara).
#[tokio::test]
async fn context_overflow_is_not_a_max_tokens_reduction() {
    let overflow = r#"{"type":"error","error":{"type":"invalid_request_error","message":"input length and `max_tokens` exceed context limit: 180000 + 32000 > 200000, decrease input length or `max_tokens` and try again"}}"#;
    let (base, cap) = spawn_server(vec![(400, overflow), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let r = req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::Low));
    let err = c.chat(&r).await.expect_err("estouro");
    assert!(crate::agents::summary::is_context_overflow(&err), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), 1, "sem relaxamento");
    assert_eq!(anthropic::known_max_tokens(base.trim_end_matches("/v1"), "claude-teste"), None);
    c.chat(&r).await.expect("chat");
    assert_eq!(body_of(&cap.lock().expect("lock")[1])["max_tokens"], DEFAULT_MAX_TOKENS);
}

/// A redução sem o limite no texto (o valor de reserva) vale só para o pedido: nada é memorizado.
#[tokio::test]
async fn fallback_reduction_is_not_remembered() {
    let vague = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: value too large for this model"}}"#;
    let (base, cap) = spawn_server(vec![(400, vague), (200, TEXT_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let r = req(vec![ChatMessage::text(Role::User, "oi")], None);
    c.chat(&r).await.expect("chat");
    c.chat(&r).await.expect("chat de novo");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[1]["max_tokens"], 8_192);
    assert_eq!(bodies[2]["max_tokens"], DEFAULT_MAX_TOKENS);
    assert_eq!(anthropic::known_max_tokens(base.trim_end_matches("/v1"), "claude-teste"), None);
}

#[tokio::test]
async fn max_tokens_refused_twice_is_an_error() {
    let too_big = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 32000 > 4096, the maximum"}}"#;
    let (base, cap) = spawn_server(vec![(400, too_big), (400, too_big)]).await;
    let err = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], None)).await.expect_err("400");
    assert!(matches!(err, LlmError::Http { status: 400, .. }));
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!((bodies.len(), bodies[1]["max_tokens"].as_u64()), (2, Some(4096)));
}

#[tokio::test]
async fn known_max_tokens_from_the_models_list_is_used() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    let c = client(&base);
    anthropic::remember_max_tokens(&base.trim_end_matches("/v1"), "claude-teste", 64_000);
    c.chat(&req(vec![ChatMessage::text(Role::User, "oi")], None)).await.expect("chat");
    assert_eq!(body_of(&cap.lock().expect("lock")[0])["max_tokens"], 64_000);
}

/// JSON: sem `output_config.format`; a instrução vai no texto quando nenhuma mensagem cita JSON, e
/// a cerca de código em volta da resposta sai.
#[tokio::test]
async fn json_request_uses_the_text_instruction_and_unwraps_the_fence() {
    let fenced = r#"{"type":"message","content":[{"type":"text","text":"```json\n{\"ok\":true}\n```"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
    let (base, cap) = spawn_server(vec![(200, fenced), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let mut r = req(vec![ChatMessage::text(Role::User, "responda")], Some(Effort::Low));
    r.response_json = true;
    let resp = c.chat(&r).await.expect("chat");
    assert_eq!(resp.message.content, r#"{"ok":true}"#);
    r.messages = vec![ChatMessage::text(Role::System, "Responda só com o objeto JSON."), ChatMessage::text(Role::User, "responda")];
    c.chat(&r).await.expect("chat");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[0]["output_config"], json!({"effort":"low"}), "sem format");
    let user = &bodies[0]["messages"][0]["content"];
    assert_eq!(user[0]["text"], "responda");
    assert_eq!(user[1]["text"], RESPONSES_JSON_HINT);
    assert_eq!(bodies[1]["messages"][0]["content"].as_array().map(Vec::len), Some(1), "já cita JSON");
}

/// Pedido sem ferramentas com `tool_use` no histórico (o resumo do histórico): as ferramentas usadas
/// são declaradas, porque a API recusa os blocos sem elas. O thinking do turno segue inalterado.
#[tokio::test]
async fn request_without_tools_declares_the_tools_of_the_history() {
    let (base, cap) = spawn_server(vec![(200, TOOL_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let first = c.chat(&req(vec![ChatMessage::text(Role::User, "leia")], Some(Effort::Low))).await.expect("primeira");
    let mut r = req(
        vec![ChatMessage::text(Role::User, "leia"), first.message, tool_result("toolu_01", "x"), ChatMessage::text(Role::User, "resuma")],
        Some(Effort::Low),
    );
    r.tools.clear();
    c.chat(&r).await.expect("resumo");
    let body = body_of(&cap.lock().expect("lock")[1]);
    assert_eq!(body["tools"][0]["name"], "read_file");
    assert_eq!(body["tools"][0]["input_schema"], json!({"type":"object"}));
    assert_eq!(body["messages"][1]["content"][0]["signature"], "ASSINATURA-opaca-1");
}

/// Compactação (spec 122): trocar os argumentos de um `tool_use` guardado descarta os blocos de
/// thinking daquele turno; os outros blocos ficam.
#[test]
fn rewriting_a_stored_tool_use_drops_the_thinking_of_that_turn() {
    let reply = anthropic::parse(TOOL_REPLY, false, false).expect("parse");
    let mut msg = reply.message;
    rewrite_call_arguments(&mut msg, "toolu_01", r#"{"path":"[omitido]"}"#);
    let items = msg.provider_items.expect("itens");
    let types: Vec<&str> = items.iter().map(|i| i["type"].as_str().unwrap_or_default()).collect();
    assert_eq!(types, vec!["text", "tool_use"]);
    assert_eq!(items[1]["input"], json!({"path":"[omitido]"}));
    // Outra chamada: nada muda.
    let mut other = anthropic::parse(TOOL_REPLY, false, false).expect("parse").message;
    rewrite_call_arguments(&mut other, "toolu_x", r#"{"path":"b"}"#);
    assert_eq!(other.provider_items.map(|i| i.len()), Some(4));
}

#[test]
fn synthetic_ids_are_mirrored_in_tool_use_blocks() {
    let mut msg = anthropic::parse(TOOL_REPLY, false, false).expect("parse").message;
    msg.tool_calls[0].id = "nants-synth-1".into();
    sync_provider_call_ids(&mut msg);
    assert_eq!(msg.provider_items.expect("itens")[3]["id"], "nants-synth-1");
}

#[test]
fn parse_maps_stop_reasons_errors_and_usage() {
    let max = r#"{"type":"message","content":[{"type":"text","text":"parci"}],"stop_reason":"max_tokens","usage":{"input_tokens":5,"output_tokens":9,"output_tokens_details":{"thinking_tokens":4}}}"#;
    let r = anthropic::parse(max, false, false).expect("parse");
    assert_eq!(r.finish_reason.as_deref(), Some("length"));
    let u = r.usage.expect("uso");
    assert_eq!((u.prompt, u.completion, u.cached, u.cache_write, u.reasoning), (5, 9, None, None, Some(4)));

    let refusal = r#"{"type":"message","content":[],"stop_reason":"refusal"}"#;
    let err = anthropic::parse(refusal, false, false).expect_err("recusa");
    assert_eq!(provider_failure(&err).describe(), "provider_bad_response (incompleta: refusal)");

    let empty = r#"{"type":"message","content":[{"type":"thinking","thinking":"x","signature":"s"}],"stop_reason":"end_turn"}"#;
    assert!(matches!(anthropic::parse(empty, false, false), Err(LlmError::Parse(r)) if r.kind == BadKind::Empty));

    let overloaded = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
    let err = anthropic::parse(overloaded, false, false).expect_err("erro");
    assert!(matches!(err, LlmError::Parse(r) if r.kind == BadKind::ProviderError), "{err:?}");
    assert!(matches!(anthropic::parse("nada", false, false), Err(LlmError::Parse(r)) if r.kind == BadKind::InvalidJson));
}

/// Evento SSE da Anthropic: o nome do evento é o `type` do JSON.
fn sse(data: Value) -> String {
    let name = data["type"].as_str().unwrap_or_default().to_string();
    ev(&name, &data.to_string())
}

fn block_delta(index: u64, delta: Value) -> String {
    sse(json!({"type":"content_block_delta","index":index,"delta":delta}))
}

/// Gravação do stream equivalente a [`TOOL_REPLY`]: thinking em dois pedaços com a assinatura,
/// thinking redigido, texto, `tool_use` com o JSON em pedaços e o uso dividido entre
/// `message_start` e `message_delta`, com `ping` no meio.
fn tool_reply_stream() -> String {
    let mut s = sse(json!({"type":"message_start","message":{"id":"msg_2","type":"message","role":"assistant","model":"claude-teste","content":[],"stop_reason":null,"usage":{"input_tokens":20,"cache_read_input_tokens":100,"cache_creation_input_tokens":30,"output_tokens":1}}}));
    s += &sse(json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}));
    s += &block_delta(0, json!({"type":"thinking_delta","thinking":"preciso "}));
    s += &sse(json!({"type":"ping"}));
    s += &block_delta(0, json!({"type":"thinking_delta","thinking":"ler"}));
    s += &block_delta(0, json!({"type":"signature_delta","signature":"ASSINATURA-opaca-1"}));
    s += &sse(json!({"type":"content_block_stop","index":0}));
    s += &sse(json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"REDIGIDO-opaco-2"}}));
    s += &sse(json!({"type":"content_block_stop","index":1}));
    s += &sse(json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}));
    s += &block_delta(2, json!({"type":"text_delta","text":"vou "}));
    s += &block_delta(2, json!({"type":"text_delta","text":"ler"}));
    s += &sse(json!({"type":"content_block_stop","index":2}));
    s += &sse(json!({"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"toolu_01","name":"read_file","input":{}}}));
    s += &block_delta(3, json!({"type":"input_json_delta","partial_json":""}));
    s += &block_delta(3, json!({"type":"input_json_delta","partial_json":"{\"path\": "}));
    s += &block_delta(3, json!({"type":"input_json_delta","partial_json":"\"a.txt\"}"}));
    s += &sse(json!({"type":"content_block_stop","index":3}));
    s += &sse(json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":15}}));
    s += &sse(json!({"type":"message_stop"}));
    s
}

/// Stream de texto simples equivalente a [`TEXT_REPLY`].
fn text_reply_stream() -> String {
    let mut s = sse(json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":1}}}));
    s += &sse(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}));
    s += &block_delta(0, json!({"type":"text_delta","text":"olá"}));
    s += &sse(json!({"type":"content_block_stop","index":0}));
    s += &sse(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}));
    s += &sse(json!({"type":"message_stop"}));
    s
}

type Seen = std::sync::Arc<std::sync::Mutex<Vec<StreamDelta>>>;

fn collector() -> (Seen, impl Fn(StreamDelta) + Send + Sync) {
    let seen: Seen = Default::default();
    let seen2 = std::sync::Arc::clone(&seen);
    (seen, move |d: StreamDelta| seen2.lock().expect("lock").push(d))
}

/// O stream gravado reproduz o mesmo `ChatResponse` da versão sem stream (os blocos com o thinking
/// completo e a assinatura, o `input` parseado e o uso somado), com o corpo em `stream: true` e os
/// deltas de raciocínio e texto entregues à tela.
#[tokio::test]
async fn recorded_stream_reproduces_the_blocking_response() {
    let (base, cap) = raw_server(vec![(200, SSE, tool_reply_stream())]).await;
    let (seen, on) = collector();
    let r = req(vec![ChatMessage::text(Role::User, "leia a.txt")], Some(Effort::Low));
    let streamed = client(&base).chat_stream(&r, &on).await.expect("stream");
    let blocking = anthropic::parse(TOOL_REPLY, false, false).expect("parse");
    assert_eq!(streamed, blocking);
    let usage = streamed.usage.expect("uso");
    assert_eq!((usage.prompt, usage.completion, usage.cached, usage.cache_write), (150, 15, Some(100), Some(30)));

    let raw = cap.lock().expect("lock")[0].clone();
    assert!(head_of(&raw).to_ascii_lowercase().starts_with("post /v1/messages http/1.1"));
    let body = body_of(&raw);
    assert_eq!(body["stream"], true);
    assert_eq!(body["thinking"], json!({"type":"adaptive","display":"summarized"}));

    let shown: Vec<StreamDelta> = seen.lock().expect("lock").iter().cloned().filter_map(super::not_size).collect();
    assert_eq!(
        shown,
        vec![
            StreamDelta::Reasoning("preciso ".into()),
            StreamDelta::Reasoning("ler".into()),
            StreamDelta::Text("vou ".into()),
            StreamDelta::Text("ler".into()),
        ]
    );
}

/// A volta seguinte (também em stream) reenvia o thinking com a assinatura e o `tool_use`
/// inalterados, como na versão sem stream.
#[tokio::test]
async fn streamed_turn_is_resent_unchanged() {
    let (base, cap) = raw_server(vec![(200, SSE, tool_reply_stream()), (200, SSE, text_reply_stream())]).await;
    let c = client(&base);
    let (_seen, on) = collector();
    let mut messages = vec![ChatMessage::text(Role::User, "leia a.txt")];
    let first = c.chat_stream(&req(messages.clone(), Some(Effort::Low)), &on).await.expect("primeira");
    messages.push(first.message);
    messages.push(tool_result("toolu_01", "linha"));
    let second = c.chat_stream(&req(messages, Some(Effort::Low)), &on).await.expect("segunda");
    assert_eq!(second, anthropic::parse(TEXT_REPLY, false, false).expect("parse"));
    let body = body_of(&cap.lock().expect("lock")[1]);
    let original: Value = serde_json::from_str(TOOL_REPLY).expect("json");
    assert_eq!(body["messages"][1], json!({"role":"assistant","content":original["content"]}));
}

/// O evento `error` no meio do stream sobe com o motivo do provedor, sem diagnóstico de corte.
#[tokio::test]
async fn stream_error_event_is_a_provider_error() {
    let mut failing = sse(json!({"type":"message_start","message":{"usage":{"input_tokens":3}}}));
    failing += &sse(json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}));
    let (base, cap) = raw_server(vec![(200, SSE, failing)]).await;
    let (seen, on) = collector();
    let err = client(&base)
        .with_retry_delays(Vec::new())
        .chat_stream(&req(vec![ChatMessage::text(Role::User, "oi")], None), &on)
        .await
        .expect_err("erro do stream");
    assert!(matches!(&err, LlmError::Parse(r) if r.kind == BadKind::ProviderError), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), 1);
    assert!(!seen.lock().expect("lock").iter().any(|d| matches!(d, StreamDelta::Cut(_))));
}

/// A sobrecarga (529) antes do primeiro delta se repete como falha transitória, pelo stream.
#[tokio::test]
async fn stream_retries_overload() {
    let overloaded = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
    let (base, cap) = raw_server(vec![(529, "application/json", overloaded.to_string()), (200, SSE, text_reply_stream())]).await;
    let (_seen, on) = collector();
    let resp = client(&base)
        .with_retry_delays(vec![std::time::Duration::from_millis(1)])
        .chat_stream(&req(vec![ChatMessage::text(Role::User, "oi")], None), &on)
        .await
        .expect("chat");
    assert_eq!(resp.message.content, "olá");
    let captured = cap.lock().expect("lock").clone();
    assert_eq!(captured.len(), 2);
    assert_eq!(body_of(&captured[1])["stream"], true);
}

/// Stream que acaba sem `message_stop` vira o diagnóstico de corte da spec 114, com o uso informado
/// em `message_start` e os trechos do texto e do thinking.
#[tokio::test]
async fn stream_without_message_stop_is_a_cut_with_the_diagnosis() {
    let mut s = sse(json!({"type":"message_start","message":{"usage":{"input_tokens":7,"cache_read_input_tokens":5,"output_tokens":1}}}));
    s += &sse(json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}));
    s += &block_delta(0, json!({"type":"thinking_delta","thinking":"pensando"}));
    s += &sse(json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}));
    s += &block_delta(1, json!({"type":"text_delta","text":"começo da resposta"}));
    let (base, _) = raw_server(vec![(200, SSE, s.clone()), (200, SSE, s)]).await;
    let (seen, on) = collector();
    let err = client(&base)
        .with_retry_delays(Vec::new())
        .chat_stream(&req(vec![ChatMessage::text(Role::User, "oi")], None), &on)
        .await
        .expect_err("cortada");
    assert!(matches!(&err, LlmError::Parse(r) if r.kind == BadKind::NoFinal), "{err:?}");
    let cuts: Vec<std::sync::Arc<StreamCut>> = seen
        .lock()
        .expect("lock")
        .iter()
        .filter_map(|d| if let StreamDelta::Cut(c) = d { Some(std::sync::Arc::clone(c)) } else { None })
        .collect();
    let cut = cuts.first().expect("diagnóstico");
    assert_eq!(cut.reason, "no_final");
    let reported = cut.reported.expect("uso informado");
    assert_eq!((reported.prompt, reported.cached), (12, Some(5)));
    let (head, _) = cut.snippets(str::to_string);
    assert!(head.contains("pensando") && head.contains("começo da resposta"), "{head}");
    assert!(cut.top_events.iter().any(|(k, _)| k == "content_block_delta"), "{:?}", cut.top_events);
}

/// Resposta 200 em JSON comum (sem SSE) no caminho do stream é lida como a da versão sem stream.
#[tokio::test]
async fn non_sse_reply_on_the_stream_path_is_parsed_as_blocking() {
    let (base, _) = raw_server(vec![(200, "application/json", TOOL_REPLY.to_string())]).await;
    let (_seen, on) = collector();
    let r = req(vec![ChatMessage::text(Role::User, "leia a.txt")], Some(Effort::Low));
    let resp = client(&base).chat_stream(&r, &on).await.expect("chat");
    assert_eq!(resp, anthropic::parse(TOOL_REPLY, false, false).expect("parse"));
}

/// A escada de relaxamento vale no stream: `adaptive` recusado vira `enabled`, com `stream: true`.
#[tokio::test]
async fn stream_relaxes_the_thinking_on_400() {
    let adaptive = r#"{"type":"error","error":{"type":"invalid_request_error","message":"thinking.type: Input tag 'adaptive' does not match any of the expected tags"}}"#;
    let (base, cap) = raw_server(vec![(400, "application/json", adaptive.to_string()), (200, SSE, text_reply_stream())]).await;
    let (_seen, on) = collector();
    let resp = client(&base).chat_stream(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::Medium)), &on).await.expect("chat");
    assert_eq!(resp.message.content, "olá");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[1]["thinking"], json!({"type":"enabled","budget_tokens":8192,"display":"summarized"}));
    assert_eq!(bodies[1]["stream"], true);
}

/// `tool_use` cortado no meio do JSON (parada por `max_tokens`): o `input` vira objeto vazio, que a
/// API aceita no reenvio, e o motivo é `tool_calls`.
#[test]
fn truncated_tool_json_becomes_an_empty_object() {
    let mut acc = anthropic::StreamAcc::new(false);
    let on = |_: StreamDelta| {};
    let events = [
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"read_file","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"a"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":4}}),
    ];
    for e in events {
        acc.event("", &e.to_string(), &on).expect("evento");
    }
    let r = acc.finish(false).expect("resposta");
    assert_eq!(r.message.tool_calls[0].arguments, "{}");
    assert_eq!(r.message.provider_items.expect("itens")[0]["input"], json!({}));
}

/// Passa `events` por um [`anthropic::StreamAcc`] e devolve a resposta montada.
fn run_acc(events: &[Value]) -> ChatResponse {
    let mut acc = anthropic::StreamAcc::new(false);
    let on = |_: StreamDelta| {};
    for e in events {
        acc.event("", &e.to_string(), &on).expect("evento");
    }
    acc.finish(false).expect("resposta")
}

/// F-01: índices fora de ordem (o start do 1 antes do 0, com deltas intercalados) e deltas antes do
/// start: nada do que chegou se perde, e o resultado é o da versão sem stream.
#[test]
fn out_of_order_blocks_and_early_deltas_are_kept() {
    let delta = |index: u64, d: Value| json!({"type":"content_block_delta","index":index,"delta":d});
    let events = vec![
        json!({"type":"message_start","message":{"usage":{"input_tokens":4,"output_tokens":1}}}),
        // Deltas do 0 antes do start dele.
        delta(0, json!({"type":"thinking_delta","thinking":"pen"})),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"read_file","input":{}}}),
        delta(1, json!({"type":"input_json_delta","partial_json":"{\"path\":"})),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        delta(0, json!({"type":"thinking_delta","thinking":"sando"})),
        delta(1, json!({"type":"input_json_delta","partial_json":"\"a.txt\"}"})),
        delta(0, json!({"type":"signature_delta","signature":"SIG-1"})),
        // Texto do 2 antes do start, com o JSON parcial do 3 também antes do start.
        delta(2, json!({"type":"text_delta","text":"vou "})),
        delta(3, json!({"type":"input_json_delta","partial_json":"{\"path\":\"b.txt\"}"})),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}),
        delta(2, json!({"type":"text_delta","text":"ler"})),
        json!({"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"t2","name":"read_file","input":{}}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"content_block_stop","index":3}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
        json!({"type":"message_stop"}),
    ];
    let expected = json!({
        "type":"message",
        "content":[
            {"type":"thinking","thinking":"pensando","signature":"SIG-1"},
            {"type":"tool_use","id":"t1","name":"read_file","input":{"path":"a.txt"}},
            {"type":"text","text":"vou ler"},
            {"type":"tool_use","id":"t2","name":"read_file","input":{"path":"b.txt"}}
        ],
        "stop_reason":"tool_use",
        "usage":{"input_tokens":4,"output_tokens":9}
    });
    assert_eq!(run_acc(&events), anthropic::parse(&expected.to_string(), false, false).expect("parse"));
}

/// F-01: delta de um índice cujo start nunca chega não deixa bloco sem `type` (a API recusaria o
/// reenvio).
#[test]
fn delta_without_start_leaves_no_untyped_block() {
    let events = vec![
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"solto"}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ];
    let r = run_acc(&events);
    let items = r.message.provider_items.clone().expect("itens");
    assert!(items.iter().all(|b| b["type"].is_string()), "{items:?}");
    let expected = json!({"type":"message","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"output_tokens":2}});
    assert_eq!(r, anthropic::parse(&expected.to_string(), false, false).expect("parse"));
}

#[tokio::test]
async fn http_error_does_not_carry_the_key() {
    let key = fake_key();
    let echo = leak(format!(r#"{{"type":"error","error":{{"type":"authentication_error","message":"chave {key} inválida"}}}}"#));
    let (base, _cap) = spawn_server(vec![(401, echo)]).await;
    let err = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], None)).await.expect_err("401");
    assert!(!format!("{err:?} {err}").contains(&key));
    assert_eq!(provider_failure(&err).to_string(), "provider_http_401");
}

#[test]
fn from_connection_builds_the_anthropic_client_on_the_endpoint() {
    std::env::set_var("NANTS_T132_T03_KEY", fake_key());
    let mut conn = Connection::native("a", "A", ConnectionKind::Anthropic, "env:NANTS_T132_T03_KEY");
    conn.base_url_override = Some("https://proxy.example.com/anthropic".into());
    let c = LlmClient::from_connection(&conn).expect("cliente");
    assert_eq!(c.base_url, "https://proxy.example.com/anthropic");
    assert_eq!(c.kind, ConnectionKind::Anthropic);
    assert!(c.is_paced());
}

// ---- Busca web (T05) ----

/// Pedido do Arquiteto com a busca ligada e `left` buscas restantes.
fn web_req(messages: Vec<ChatMessage>, left: u32) -> ChatRequest {
    ChatRequest { web_search: true, web_search_max_uses: Some(left), ..req(messages, None) }
}

/// Resposta com uma busca: a chamada de servidor, o resultado (com o conteúdo cifrado) e o texto
/// partido pelas citações.
const WEB_REPLY: &str = r#"{"id":"msg_w","type":"message","role":"assistant","content":[{"type":"server_tool_use","id":"srvtoolu_01","name":"web_search","input":{"query":"tokio versão"}},{"type":"web_search_tool_result","tool_use_id":"srvtoolu_01","content":[{"type":"web_search_result","url":"https://a.example/x","title":"A","encrypted_content":"CIFRADO-opaco-1","page_age":"1 day ago"},{"type":"web_search_result","url":"https://b.example/y","title":"B","encrypted_content":"CIFRADO-opaco-2","page_age":null}]},{"type":"text","text":"A versão atual "},{"type":"text","text":"é a 2.0","citations":[{"type":"web_search_result_location","url":"https://a.example/x","title":"A","encrypted_index":"IDX-1","cited_text":"2.0"},{"type":"web_search_result_location","url":"https://a.example/x","title":"A","encrypted_index":"IDX-2","cited_text":"versão 2.0"}]},{"type":"text","text":", segundo ","citations":[{"type":"web_search_result_location","url":"https://b.example/y","title":"B","encrypted_index":"IDX-3","cited_text":"b"}]},{"type":"text","text":"as fontes."}],"stop_reason":"end_turn","usage":{"input_tokens":50,"output_tokens":9,"server_tool_use":{"web_search_requests":1}}}"#;

/// O corpo leva a busca com filtragem dinâmica antes das ferramentas do cliente, com o `max_uses`
/// do que resta do limite, e sem `code_execution` declarado junto.
#[tokio::test]
async fn web_search_goes_in_the_body_with_max_uses() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    client(&base).chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 3)).await.expect("chat");
    let body = body_of(&cap.lock().expect("lock")[0]);
    let tools = body["tools"].as_array().expect("tools");
    assert_eq!(tools[0], json!({"type":"web_search_20260209","name":"web_search","max_uses":3}));
    assert_eq!(tools[1]["name"], "read_file");
    assert_eq!(tools.len(), 2);
    assert!(!body["tools"].to_string().contains("code_execution"));
}

/// Sem a busca pedida, nada de ferramenta de servidor.
#[tokio::test]
async fn without_web_search_there_is_no_server_tool() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    let resp = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], None)).await.expect("chat");
    assert!(!resp.web_search_rejected);
    assert!(!body_of(&cap.lock().expect("lock")[0])["tools"].to_string().contains("web_search"));
}

/// A escada desce da dinâmica para a básica e depois para sem busca, uma vez por degrau, e só o
/// último degrau marca a busca como recusada. O esforço fica.
#[tokio::test]
async fn web_search_ladder_goes_dynamic_basic_then_off() {
    let dynamic = r#"{"type":"error","error":{"type":"invalid_request_error","message":"'claude-teste' does not support programmatic tool calling. The following tools have `allowed_callers` that require it: web_search."}}"#;
    let disabled = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Web search is not enabled for your organization."}}"#;
    let (base, cap) = spawn_server(vec![(400, dynamic), (400, disabled), (200, TEXT_REPLY)]).await;
    let r = ChatRequest { effort: Some(Effort::Low), ..web_req(vec![ChatMessage::text(Role::User, "pesquise")], 2) };
    let resp = client(&base).chat(&r).await.expect("chat");
    assert!(resp.web_search_rejected);
    assert!(!resp.fallback_used);
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0]["tools"][0]["type"], "web_search_20260209");
    assert_eq!(bodies[1]["tools"][0], json!({"type":"web_search_20250305","name":"web_search","max_uses":2}));
    assert!(!bodies[2]["tools"].to_string().contains("web_search"));
    assert_eq!(bodies[2]["thinking"], json!({"type":"adaptive","display":"summarized"}));
}

/// A básica aceita: a busca não foi recusada. O 400 que segue depois do último degrau sobe.
#[tokio::test]
async fn basic_web_search_accepted_is_not_a_rejection_and_the_ladder_ends() {
    let refused = r#"{"type":"error","error":{"type":"invalid_request_error","message":"tools.0: Input tag 'web_search_20260209' found using 'type' does not match any of the expected tags"}}"#;
    let (base, _cap) = spawn_server(vec![(400, refused), (200, WEB_REPLY)]).await;
    let resp = client(&base).chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 1)).await.expect("chat");
    assert!(!resp.web_search_rejected);
    assert_eq!(resp.web_search_calls, 1);

    let (base, cap) = spawn_server(vec![(400, refused), (400, refused), (400, refused)]).await;
    let err = client(&base).chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 1)).await.expect_err("400");
    assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), 3, "dois degraus e o erro");
}

/// Dois turnos: a contagem, as fontes sem repetir e o texto partido pelas citações num texto só; na
/// volta seguinte, os blocos da busca (com o conteúdo cifrado e as citações) voltam inalterados.
#[tokio::test]
async fn two_turns_count_cite_and_resend_the_search_blocks() {
    let (base, cap) = spawn_server(vec![(200, WEB_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let mut messages = vec![ChatMessage::text(Role::User, "qual a versão do tokio?")];
    let first = c.chat(&web_req(messages.clone(), 2)).await.expect("primeira");
    assert_eq!(first.web_search_calls, 1);
    assert_eq!(first.message.content, "A versão atual é a 2.0, segundo as fontes.");
    assert_eq!(first.finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        first.citations,
        vec![
            Citation { url: "https://a.example/x".into(), title: "A".into() },
            Citation { url: "https://b.example/y".into(), title: "B".into() },
        ]
    );
    assert!(first.message.tool_calls.is_empty(), "a chamada de servidor não é do cliente");
    assert!(!format!("{:?}", first.message).contains("CIFRADO"), "itens opacos fora do Debug");

    messages.push(first.message.clone());
    messages.push(ChatMessage::text(Role::User, "e a anterior?"));
    c.chat(&web_req(messages, 1)).await.expect("segunda");
    let body = body_of(&cap.lock().expect("lock")[1]);
    let original: Value = serde_json::from_str(WEB_REPLY).expect("json");
    assert_eq!(body["messages"][1], json!({"role":"assistant","content":original["content"]}));
    assert_eq!(body["tools"][0]["max_uses"], 1);
}

/// O histórico com a busca vai sem a ferramenta quando o pedido não a pede (a API aceita, conferido
/// ao vivo); se um 400 citar a busca, ela é declarada uma vez, a básica com uma busca.
#[tokio::test]
async fn history_with_search_declares_the_tool_only_after_a_refusal() {
    let refused = r#"{"type":"error","error":{"type":"invalid_request_error","message":"web_search_tool_result requires the web_search tool"}}"#;
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY), (400, refused), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let first = anthropic::parse(WEB_REPLY, false, false).expect("parse");
    let messages = vec![ChatMessage::text(Role::User, "pesquise"), first.message, ChatMessage::text(Role::User, "resuma")];
    let r = ChatRequest { tools: Vec::new(), ..req(messages, None) };
    c.chat(&r).await.expect("aceito sem a ferramenta");
    let resp = c.chat(&r).await.expect("reserva");
    assert!(!resp.web_search_rejected, "o pedido não queria a busca");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert!(bodies[0].get("tools").is_none());
    assert!(bodies[1].get("tools").is_none());
    assert_eq!(
        bodies[2]["tools"],
        json!([{"type":"web_search_20250305","name":"web_search","max_uses":1,"cache_control":{"type":"ephemeral"}}])
    );
}

/// O erro da busca chega com HTTP 200 como objeto no `content` do resultado: volta inalterado.
#[tokio::test]
async fn search_error_result_is_kept_and_resent() {
    let reply = r#"{"type":"message","role":"assistant","content":[{"type":"server_tool_use","id":"srvtoolu_9","name":"web_search","input":{"query":"x"}},{"type":"web_search_tool_result","tool_use_id":"srvtoolu_9","content":{"type":"web_search_tool_result_error","error_code":"max_uses_exceeded"}},{"type":"text","text":"Sem resultados."}],"stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":3,"server_tool_use":{"web_search_requests":0}}}"#;
    let (base, cap) = spawn_server(vec![(200, reply), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let first = c.chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 1)).await.expect("primeira");
    assert_eq!(first.web_search_calls, 0);
    assert_eq!(first.message.content, "Sem resultados.");
    let messages = vec![ChatMessage::text(Role::User, "pesquise"), first.message, ChatMessage::text(Role::User, "ok")];
    c.chat(&web_req(messages, 1)).await.expect("segunda");
    let body = body_of(&cap.lock().expect("lock")[1]);
    let original: Value = serde_json::from_str(reply).expect("json");
    assert_eq!(body["messages"][1]["content"], original["content"]);
}

/// Resposta parada no `pause_turn`: só a chamada de servidor e o resultado, sem texto.
const PAUSED_REPLY: &str = r#"{"type":"message","role":"assistant","content":[{"type":"server_tool_use","id":"srvtoolu_p","name":"web_search","input":{"query":"q"}},{"type":"web_search_tool_result","tool_use_id":"srvtoolu_p","content":[{"type":"web_search_result","url":"https://p.example/","title":"P","encrypted_content":"CIFRADO-p"}]}],"stop_reason":"pause_turn","usage":{"input_tokens":40,"cache_read_input_tokens":10,"output_tokens":5,"server_tool_use":{"web_search_requests":1}}}"#;

/// O `pause_turn` continua com o mesmo pedido e o conteúdo recebido como a mensagem `assistant` do
/// fim, sem mensagem nova do usuário. O resultado junta os blocos e soma o uso e as buscas.
#[tokio::test]
async fn pause_turn_continues_with_the_assistant_content_and_sums_usage() {
    let (base, cap) = spawn_server(vec![(200, PAUSED_REPLY), (200, WEB_REPLY)]).await;
    let resp = client(&base).chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 3)).await.expect("chat");
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    assert_eq!(resp.web_search_calls, 2);
    assert_eq!(resp.message.content, "A versão atual é a 2.0, segundo as fontes.");
    let usage = resp.usage.expect("uso");
    assert_eq!((usage.prompt, usage.completion, usage.cached), (100, 14, Some(10)));
    let paused: Value = serde_json::from_str(PAUSED_REPLY).expect("json");
    let web: Value = serde_json::from_str(WEB_REPLY).expect("json");
    let mut all = paused["content"].as_array().expect("blocos").clone();
    all.extend(web["content"].as_array().expect("blocos").iter().cloned());
    assert_eq!(resp.message.provider_items, Some(all));

    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies.len(), 2);
    let messages = bodies[1]["messages"].as_array().expect("mensagens");
    assert_eq!(messages.len(), 2, "sem mensagem nova do usuário");
    assert_eq!(messages[0], bodies[0]["messages"][0]);
    assert_eq!(messages[1], json!({"role":"assistant","content":paused["content"]}));
    assert_eq!(bodies[1]["tools"][0]["max_uses"], 2, "a continuação leva o que resta");
    assert_eq!(bodies[1]["tools"][1], bodies[0]["tools"][1]);
}

/// A continuação sem buscas restantes vai sem a ferramenta, e isso não é uma recusa.
#[tokio::test]
async fn pause_turn_with_the_searches_spent_continues_without_the_tool() {
    let (base, cap) = spawn_server(vec![(200, PAUSED_REPLY), (200, TEXT_REPLY)]).await;
    let resp = client(&base).chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 1)).await.expect("chat");
    assert!(!resp.web_search_rejected);
    assert_eq!(resp.web_search_calls, 1);
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[0]["tools"][0]["max_uses"], 1);
    assert!(!bodies[1]["tools"].to_string().contains("web_search"));
}

/// O `pause_turn` tem teto de continuações: sem texto depois dele, a resposta ficou incompleta.
#[tokio::test]
async fn pause_turn_stops_after_the_continuation_cap() {
    let replies = vec![(200, PAUSED_REPLY); anthropic::MAX_CONTINUATIONS + 2];
    let (base, cap) = spawn_server(replies).await;
    let c = client(&base).with_retry_delays(Vec::new());
    let err = c.chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 9)).await.expect_err("incompleta");
    assert!(matches!(&err, LlmError::Parse(r) if r.kind == BadKind::Incomplete), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), anthropic::MAX_CONTINUATIONS + 1);
}

/// Stream da busca: a chamada de servidor com o `input_json_delta`, o resultado inteiro no
/// `content_block_start`, o texto com `citations_delta` e a contagem no `message_delta`.
fn web_reply_stream() -> String {
    let web: Value = serde_json::from_str(WEB_REPLY).expect("json");
    let mut s = sse(json!({"type":"message_start","message":{"id":"msg_w","type":"message","role":"assistant","content":[],"usage":{"input_tokens":50,"output_tokens":1}}}));
    s += &sse(json!({"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_01","name":"web_search","input":{}}}));
    s += &block_delta(0, json!({"type":"input_json_delta","partial_json":"{\"query\": \"tokio "}));
    s += &block_delta(0, json!({"type":"input_json_delta","partial_json":"versão\"}"}));
    s += &sse(json!({"type":"content_block_stop","index":0}));
    s += &sse(json!({"type":"content_block_start","index":1,"content_block":web["content"][1]}));
    s += &sse(json!({"type":"content_block_stop","index":1}));
    s += &sse(json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}));
    s += &block_delta(2, json!({"type":"text_delta","text":"A versão atual "}));
    s += &sse(json!({"type":"content_block_stop","index":2}));
    s += &sse(json!({"type":"content_block_start","index":3,"content_block":{"type":"text","text":"","citations":[]}}));
    for c in web["content"][3]["citations"].as_array().expect("citações") {
        s += &block_delta(3, json!({"type":"citations_delta","citation":c}));
    }
    s += &block_delta(3, json!({"type":"text_delta","text":"é a 2.0"}));
    s += &sse(json!({"type":"content_block_stop","index":3}));
    s += &sse(json!({"type":"content_block_start","index":4,"content_block":{"type":"text","text":"","citations":[]}}));
    s += &block_delta(4, json!({"type":"citations_delta","citation":web["content"][4]["citations"][0]}));
    s += &block_delta(4, json!({"type":"text_delta","text":", segundo "}));
    s += &sse(json!({"type":"content_block_stop","index":4}));
    s += &sse(json!({"type":"content_block_start","index":5,"content_block":{"type":"text","text":""}}));
    s += &block_delta(5, json!({"type":"text_delta","text":"as fontes."}));
    s += &sse(json!({"type":"content_block_stop","index":5}));
    s += &sse(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9,"server_tool_use":{"web_search_requests":1}}}));
    s += &sse(json!({"type":"message_stop"}));
    s
}

/// O stream da busca reproduz a resposta sem stream: os mesmos blocos, a contagem e as fontes.
#[tokio::test]
async fn recorded_web_stream_reproduces_the_blocking_response() {
    let (base, _cap) = raw_server(vec![(200, SSE, web_reply_stream())]).await;
    let (_seen, on) = collector();
    let r = web_req(vec![ChatMessage::text(Role::User, "pesquise")], 2);
    let streamed = client(&base).chat_stream(&r, &on).await.expect("stream");
    let blocking = anthropic::parse(WEB_REPLY, false, false).expect("parse");
    assert_eq!(streamed, blocking);
    assert_eq!(streamed.web_search_calls, 1);
    assert_eq!(streamed.citations.len(), 2);
}

/// O `pause_turn` também continua pelo stream.
#[tokio::test]
async fn stream_pause_turn_continues() {
    let p: Value = serde_json::from_str(PAUSED_REPLY).expect("json");
    let mut paused = sse(json!({"type":"message_start","message":{"usage":{"input_tokens":40,"output_tokens":1}}}));
    for i in 0..2u64 {
        paused += &sse(json!({"type":"content_block_start","index":i,"content_block":p["content"][i as usize]}));
        paused += &sse(json!({"type":"content_block_stop","index":i}));
    }
    paused += &sse(json!({"type":"message_delta","delta":{"stop_reason":"pause_turn"},"usage":{"output_tokens":5,"server_tool_use":{"web_search_requests":1}}}));
    paused += &sse(json!({"type":"message_stop"}));
    let (base, cap) = raw_server(vec![(200, SSE, paused), (200, SSE, web_reply_stream())]).await;
    let (_seen, on) = collector();
    let r = web_req(vec![ChatMessage::text(Role::User, "pesquise")], 2);
    let resp = client(&base).chat_stream(&r, &on).await.expect("stream");
    assert_eq!(resp.web_search_calls, 2);
    assert_eq!(resp.message.content, "A versão atual é a 2.0, segundo as fontes.");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[1]["messages"][1], json!({"role":"assistant","content":p["content"]}));
    assert_eq!(bodies[1]["stream"], true);
}

/// Resposta da dinâmica em que o modelo chamou a busca com a entrada errada (visto ao vivo com
/// `claude-haiku-5-5`): HTTP 200, sem nenhuma busca com resultado.
const INVALID_INPUT_REPLY: &str = r#"{"type":"message","role":"assistant","content":[{"type":"server_tool_use","id":"srvtoolu_i","name":"web_search","input":{"params":{"query":"q"}}},{"type":"web_search_tool_result","tool_use_id":"srvtoolu_i","content":{"type":"web_search_tool_result_error","error_code":"invalid_tool_input"}},{"type":"text","text":"Não consegui buscar."}],"stop_reason":"end_turn","usage":{"input_tokens":30,"output_tokens":6,"server_tool_use":{"web_search_requests":0}}}"#;

/// A entrada inválida na dinâmica desce para a básica (uma vez) e o modelo fica lembrado: o pedido
/// seguinte já começa pela básica.
#[tokio::test]
async fn invalid_search_input_on_dynamic_goes_basic_and_is_remembered() {
    let (base, cap) = spawn_server(vec![(200, INVALID_INPUT_REPLY), (200, WEB_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let resp = c.chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 1)).await.expect("chat");
    assert_eq!(resp.web_search_calls, 1);
    assert!(!resp.web_search_rejected);
    let usage = resp.usage.expect("uso");
    assert_eq!((usage.prompt, usage.completion), (80, 15), "o uso do pedido descartado entra na conta");
    c.chat(&web_req(vec![ChatMessage::text(Role::User, "de novo")], 1)).await.expect("seguinte");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[0]["tools"][0]["type"], "web_search_20260209");
    assert_eq!(bodies[1]["tools"][0]["type"], "web_search_20250305");
    assert_eq!(bodies[2]["tools"][0]["type"], "web_search_20250305", "lembrado para o modelo");
}

/// Na básica, a entrada inválida é só um resultado: nada é repetido.
#[tokio::test]
async fn invalid_search_input_on_basic_is_kept() {
    let refused = r#"{"type":"error","error":{"type":"invalid_request_error","message":"web_search_20260209 is not supported"}}"#;
    let (base, cap) = spawn_server(vec![(400, refused), (200, INVALID_INPUT_REPLY)]).await;
    let resp = client(&base).chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 1)).await.expect("chat");
    assert_eq!(resp.message.content, "Não consegui buscar.");
    assert_eq!(cap.lock().expect("lock").len(), 2);
}

/// Pelo stream, o primeiro resultado com a entrada inválida encerra a leitura antes do texto e o
/// pedido volta com a básica.
#[tokio::test]
async fn stream_invalid_search_input_on_dynamic_goes_basic() {
    let bad: Value = serde_json::from_str(INVALID_INPUT_REPLY).expect("json");
    let mut s = sse(json!({"type":"message_start","message":{"usage":{"input_tokens":30,"output_tokens":1}}}));
    // Um texto antes da busca: chega à tela antes de a tentativa ser descartada.
    s += &sse(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}));
    s += &block_delta(0, json!({"type":"text_delta","text":"JA-MOSTRADO"}));
    s += &sse(json!({"type":"content_block_stop","index":0}));
    for i in 0..2u64 {
        s += &sse(json!({"type":"content_block_start","index":i + 1,"content_block":bad["content"][i as usize]}));
        s += &sse(json!({"type":"content_block_stop","index":i + 1}));
    }
    s += &sse(json!({"type":"content_block_start","index":3,"content_block":{"type":"text","text":""}}));
    s += &block_delta(3, json!({"type":"text_delta","text":"NÃO-MOSTRAR"}));
    s += &sse(json!({"type":"content_block_stop","index":3}));
    s += &sse(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}}));
    s += &sse(json!({"type":"message_stop"}));
    let (base, cap) = raw_server(vec![(200, SSE, s), (200, SSE, web_reply_stream())]).await;
    let (seen, on) = collector();
    let r = web_req(vec![ChatMessage::text(Role::User, "pesquise")], 1);
    let resp = client(&base).chat_stream(&r, &on).await.expect("stream");
    let expected = anthropic::parse(WEB_REPLY, false, false).expect("parse");
    assert_eq!(resp.message, expected.message);
    assert_eq!((resp.web_search_calls, resp.citations.clone()), (1, expected.citations));
    let usage = resp.usage.expect("uso");
    assert_eq!((usage.prompt, usage.completion), (80, 10), "o uso do pedido descartado entra na conta");
    let shown = seen.lock().expect("lock").clone();
    assert!(!shown.iter().any(|d| matches!(d, StreamDelta::Text(t) if t.contains("NÃO-MOSTRAR"))));
    assert!(!shown.iter().any(|d| matches!(d, StreamDelta::Cut(_))), "não é um corte");
    // Spec 132 T11: o aviso de nova tentativa vem depois do que já foi mostrado e antes da nova
    // tentativa, para a tela descartar os deltas da descartada.
    let at = |pred: &dyn Fn(&StreamDelta) -> bool| shown.iter().position(|d| pred(d));
    let shown_first = at(&|d| matches!(d, StreamDelta::Text(t) if t.contains("JA-MOSTRADO"))).expect("texto mostrado");
    let retry = at(&|d| matches!(d, StreamDelta::Retry(n) if n == anthropic::WEB_SEARCH_RETRY_NOTICE)).expect("aviso");
    assert_eq!(shown.iter().filter(|d| matches!(d, StreamDelta::Retry(_))).count(), 1, "{shown:?}");
    let next_text = shown.iter().skip(retry + 1).position(|d| matches!(d, StreamDelta::Text(_)));
    assert!(shown_first < retry && next_text.is_some(), "{shown:?}");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[1]["tools"][0]["type"], "web_search_20250305");
}

/// Sem `citations` no texto, as fontes são os resultados das buscas, na ordem, sem repetir.
#[tokio::test]
async fn without_citations_the_sources_are_the_search_results() {
    let reply = r#"{"type":"message","role":"assistant","content":[{"type":"server_tool_use","id":"s1","name":"web_search","input":{"query":"a"}},{"type":"web_search_tool_result","tool_use_id":"s1","content":[{"type":"web_search_result","url":"https://b.example/","title":"B","encrypted_content":"x"},{"type":"web_search_result","url":"https://a.example/","title":"A","encrypted_content":"y"}]},{"type":"server_tool_use","id":"s2","name":"web_search","input":{"query":"b"}},{"type":"web_search_tool_result","tool_use_id":"s2","content":[{"type":"web_search_result","url":"https://a.example/","title":"A","encrypted_content":"z"},{"type":"web_search_result","url":"https://c.example/","title":"C","encrypted_content":"w"}]},{"type":"text","text":"Resposta sem citações."}],"stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":3,"server_tool_use":{"web_search_requests":2}}}"#;
    let (base, _cap) = spawn_server(vec![(200, reply)]).await;
    let resp = client(&base).chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 2)).await.expect("chat");
    let urls: Vec<(&str, &str)> = resp.citations.iter().map(|c| (c.url.as_str(), c.title.as_str())).collect();
    assert_eq!(urls, vec![("https://b.example/", "B"), ("https://a.example/", "A"), ("https://c.example/", "C")]);
    // Com citações no texto, só elas valem.
    let cited = anthropic::parse(WEB_REPLY, false, false).expect("parse");
    assert_eq!(cited.citations.len(), 2);
}

/// Um 400 na continuação de um `pause_turn` sobe como erro HTTP, sem laço.
#[tokio::test]
async fn http_400_on_a_pause_turn_continuation_is_an_error() {
    let other = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages: invalid"}}"#;
    let (base, cap) = spawn_server(vec![(200, PAUSED_REPLY), (400, other), (200, TEXT_REPLY)]).await;
    let err = client(&base).chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 3)).await.expect_err("400");
    assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), 2);
}

/// Os blocos das ferramentas de servidor (inclusive o resultado da execução de código usada pela
/// filtragem dinâmica) são blocos da Anthropic, que voltam inalterados.
#[test]
fn server_tool_blocks_are_anthropic_blocks() {
    for t in ["server_tool_use", "web_search_tool_result", "code_execution_tool_result"] {
        assert!(anthropic::is_block(&json!({ "type": t })), "{t}");
    }
    assert!(!anthropic::is_block(&json!({"type":"function_call"})));
    assert!(!anthropic::is_block(&json!({"type":"web_search_call"})));
}

/// Teste ao vivo, ignorado por padrão (spec 132 T03, T04 e T11): o Haiku mais recente pela Models
/// API, em esforço baixo. Ping (com os limites dos cabeçalhos no ritmo, T09); uma ferramenta com o
/// reenvio do thinking na segunda volta, sem stream e pelo stream; o pedido sem ferramentas sobre o
/// histórico; e o JSON. A chave, o modelo, o preço e o teto de gasto vêm de [`live_tests`]; o uso e
/// o custo de cada pedido saem no relatório. Sem novas tentativas.
///
/// Só roda com `--ignored` e a chave em `NANTS_LIVE_ANTHROPIC_KEY_FILE`, por `scripts/test.sh`.
#[tokio::test]
#[ignore = "chama a API real da Anthropic com a chave do usuário"]
async fn anthropic_live_text_and_tool() {
    let live = Live::open(Provider::Anthropic).await;
    let (c, model) = (&live.client, live.model.clone());
    // Teto pequeno de saída (o pedido mínimo do teste).
    anthropic::remember_max_tokens(&live.base, &model, 2_048);
    let show = |label: &str, r: &ChatResponse| {
        live.record(label, r);
        eprintln!(
            "  {label}: finish={:?} fallback={} blocos={:?}",
            r.finish_reason,
            r.fallback_used,
            r.message
                .provider_items
                .iter()
                .flatten()
                .map(|i| match i["thinking"].as_str() {
                    // Tamanho do thinking (o texto não é impresso).
                    Some(t) => format!("thinking({} chars)", t.chars().count()),
                    None => i["type"].as_str().unwrap_or_default().to_string(),
                })
                .collect::<Vec<_>>()
        );
    };

    // Texto.
    let text_req = ChatRequest {
        model: model.clone(),
        messages: vec![ChatMessage::text(Role::User, "Say OK.")],
        tools: Vec::new(),
        effort: Some(Effort::Low),
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let text = c.chat(&text_req).await.expect("ping");
    show("ping", &text);
    assert!(!text.message.content.is_empty());
    // T09: os cabeçalhos `anthropic-ratelimit-*` do ping ensinam o ritmo do par.
    let ceilings = live.pacer.ceilings(live.conn, &model);
    eprintln!("  limites no ritmo (pedidos, tokens): {ceilings:?}; teto de saída: {:?}", live.pacer.output_ceiling(live.conn, &model));
    assert!(live.pacer.knows(live.conn, &model), "a Anthropic informa os limites nos cabeçalhos");

    // Ferramenta e a segunda volta com o thinking reenviado.
    let weather = ToolSpec {
        name: "get_weather".into(),
        description: "Returns the current weather for a city.".into(),
        parameters: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
    };
    let start = vec![
        ChatMessage::text(Role::System, "Use the get_weather tool to answer weather questions."),
        // Pede uma conta antes da chamada, para o thinking adaptativo aparecer mesmo no esforço baixo.
        ChatMessage::text(
            Role::User,
            "First work out privately which is larger, 17*23 or 19*21. If the first is larger, ask the weather of Paris; otherwise of Rome. Use the tool.",
        ),
    ];
    let mut messages = start.clone();
    let tool_req = |messages: Vec<ChatMessage>| ChatRequest {
        model: model.clone(),
        messages,
        tools: vec![weather.clone()],
        effort: Some(Effort::Low),
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let first = c.chat(&tool_req(messages.clone())).await.expect("ferramenta");
    show("ferramenta", &first);
    let call = first.message.tool_calls.first().cloned().expect("chamada da ferramenta");
    assert_eq!(call.name, "get_weather");
    messages.push(first.message.clone());
    messages.push(tool_result(&call.id, "Sunny, 21 C."));
    let second = c.chat(&tool_req(messages.clone())).await.expect("segunda volta");
    show("segunda volta", &second);
    assert!(!second.message.content.is_empty());

    // Pedido sem ferramentas sobre o mesmo histórico (como o resumo): as ferramentas usadas são
    // declaradas pelo adaptador, e a API aceita o `tool_use`/`tool_result` com o thinking original.
    messages.push(second.message.clone());
    messages.push(ChatMessage::text(Role::User, "Summarize this conversation in one sentence."));
    let summary_req = ChatRequest { tools: Vec::new(), ..tool_req(messages) };
    let summary = c.chat(&summary_req).await.expect("pedido sem ferramentas");
    show("sem ferramentas", &summary);
    assert!(!summary.message.content.is_empty());
    assert!(summary.message.tool_calls.is_empty());

    // Spec 132 T04: a mesma ferramenta pelo stream, com o reenvio do thinking na segunda volta
    // também em stream. O resultado tem a forma da versão sem stream: a mesma chamada, os blocos
    // de thinking com a assinatura completa e o uso informado.
    let deltas = std::sync::Mutex::new((0usize, 0usize));
    let on = |d: StreamDelta| {
        let mut n = deltas.lock().expect("lock");
        match d {
            StreamDelta::Reasoning(_) => n.0 += 1,
            StreamDelta::Text(_) => n.1 += 1,
            _ => {}
        }
    };
    let mut messages = start;
    let first_s = c.chat_stream(&tool_req(messages.clone()), &on).await.expect("ferramenta em stream");
    show("stream ferramenta", &first_s);
    let call_s = first_s.message.tool_calls.first().cloned().expect("chamada da ferramenta em stream");
    assert_eq!(call_s.name, call.name);
    assert_eq!(
        serde_json::from_str::<Value>(&call_s.arguments).expect("argumentos"),
        serde_json::from_str::<Value>(&call.arguments).expect("argumentos"),
        "a mesma chamada da versão sem stream"
    );
    assert_eq!(first_s.finish_reason, first.finish_reason);
    let items = first_s.message.provider_items.clone().expect("blocos");
    for block in items.iter().filter(|b| b["type"] == "thinking") {
        assert!(!block["signature"].as_str().unwrap_or_default().is_empty(), "assinatura completa");
    }
    let thinking_blocks = items.iter().filter(|b| b["type"] == "thinking").count();
    let use_block = items.iter().find(|b| b["type"] == "tool_use").expect("bloco tool_use");
    assert!(use_block["input"].is_object(), "input parseado do JSON acumulado");
    assert!(first_s.usage.is_some_and(|u| u.prompt > 0 && u.completion > 0));
    messages.push(first_s.message.clone());
    messages.push(tool_result(&call_s.id, "Sunny, 21 C."));
    let second_s = c.chat_stream(&tool_req(messages), &on).await.expect("segunda volta em stream");
    show("stream segunda volta", &second_s);
    assert!(!second_s.message.content.is_empty());
    let (reasoning, text) = *deltas.lock().expect("lock");
    eprintln!("stream: deltas de raciocínio={reasoning} de texto={text} blocos thinking={thinking_blocks}");
    assert!(text > 0, "o texto chega em deltas");
    if thinking_blocks > 0 {
        assert!(reasoning > 0, "o thinking chega em deltas");
    }

    // JSON (T11): a resposta é um objeto, sem a cerca de código.
    let json_req = ChatRequest {
        messages: vec![ChatMessage::text(Role::User, "Give an object with keys a=1 and b='x'.")],
        response_json: true,
        ..text_req
    };
    let reply = c.chat(&json_req).await.expect("json");
    show("json", &reply);
    let parsed: Value = serde_json::from_str(&reply.message.content).expect("resposta em JSON");
    assert_eq!(parsed["a"], 1);
    live.report();
}

/// Teste ao vivo do cache (T11), ignorado por padrão: dois pedidos com o mesmo prefixo longo no
/// `system` (acima do mínimo cacheável do modelo e novo a cada execução), com perguntas diferentes. O primeiro grava o
/// cache (`cache_control` do adaptador) e o segundo o lê; o custo usa o preço da leitura e o da
/// escrita.
///
/// Só roda com `--ignored` e a chave em `NANTS_LIVE_ANTHROPIC_KEY_FILE`, por `scripts/test.sh`.
#[tokio::test]
#[ignore = "chama a API real da Anthropic com a chave do usuário"]
async fn anthropic_live_cache() {
    let live = Live::open(Provider::Anthropic).await;
    anthropic::remember_max_tokens(&live.base, &live.model, 256);
    let prefix = live_tests::long_prefix(240, true);
    let ask = |question: &str| ChatRequest {
        model: live.model.clone(),
        messages: vec![ChatMessage::text(Role::System, &prefix), ChatMessage::text(Role::User, question)],
        tools: Vec::new(),
        effort: Some(Effort::Low),
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let first = live.client.chat(&ask("Say OK.")).await.expect("primeiro pedido");
    live.record("cache 1", &first);
    let second = live.client.chat(&ask("Say YES.")).await.expect("segundo pedido");
    live.record("cache 2", &second);
    let read = |r: &ChatResponse| r.usage.and_then(|u| u.cached).unwrap_or(0);
    let written = first.usage.and_then(|u| u.cache_write).unwrap_or(0);
    eprintln!("  gravado no 1º: {written}; lido: {} e {}", read(&first), read(&second));
    assert!(read(&second) > 0, "o segundo pedido lê o prefixo do cache");
    live.report();
}

/// Teste ao vivo da busca web (T05), ignorado por padrão: o mesmo Haiku em esforço baixo com
/// `max_uses: 1`, uma pergunta que exige a busca, pelo stream; depois uma segunda volta, sem stream e
/// sem a busca, que reenvia o histórico com os blocos da busca. No máximo uma busca (US$ 0,01). Se a
/// busca dinâmica for descartada pela entrada inválida, o aviso de nova tentativa (T11) chega antes
/// da busca básica.
///
/// Só roda com `--ignored` e a chave em `NANTS_LIVE_ANTHROPIC_KEY_FILE`, por `scripts/test.sh`.
#[tokio::test]
#[ignore = "chama a API real da Anthropic com a chave do usuário e faz uma busca paga"]
async fn anthropic_live_web_search() {
    let live = Live::open(Provider::Anthropic).await;
    let (c, model) = (&live.client, live.model.clone());
    anthropic::remember_max_tokens(&live.base, &model, 4_096);
    let show = |label: &str, r: &ChatResponse| {
        live.record(label, r);
        eprintln!(
            "  {label}: finish={:?} recusada={} fontes={:?} blocos={:?}",
            r.finish_reason,
            r.web_search_rejected,
            r.citations.iter().map(|c| c.url.as_str()).collect::<Vec<_>>(),
            r.message
                .provider_items
                .iter()
                .flatten()
                .map(|i| match i["name"].as_str() {
                    Some(n) => format!("{}({n})", i["type"].as_str().unwrap_or_default()),
                    None => i["type"].as_str().unwrap_or_default().to_string(),
                })
                .collect::<Vec<_>>()
        );
    };
    let mut messages = vec![ChatMessage::text(
        Role::User,
        "Search the web: what is the latest stable release version of the Rust programming language? Answer in one sentence and cite the source.",
    )];
    let first_req = ChatRequest {
        model: model.clone(),
        messages: messages.clone(),
        tools: Vec::new(),
        effort: Some(Effort::Low),
        response_json: false,
        web_search: true,
        web_search_max_uses: Some(1),
        cache_key: None,
    };
    let retries = std::sync::atomic::AtomicUsize::new(0);
    let on = |d: StreamDelta| {
        if matches!(&d, StreamDelta::Retry(n) if n == anthropic::WEB_SEARCH_RETRY_NOTICE) {
            retries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    };
    let first = c.chat_stream(&first_req, &on).await.expect("busca em stream");
    show("busca", &first);
    let items = first.message.provider_items.clone().expect("blocos");
    let dynamic = items.iter().any(|b| b["type"] == "server_tool_use" && b["name"] == "code_execution");
    let basic = anthropic::uses_basic_web(&c.base_url, &model);
    let retries = retries.load(std::sync::atomic::Ordering::SeqCst);
    eprintln!("desceu para web_search_20250305: {basic}; execução de código na resposta: {dynamic}; avisos de nova tentativa: {retries}");
    assert!(retries <= 1, "no máximo uma descida");
    eprintln!("fontes: {:?}", first.citations);
    assert_eq!(first.web_search_calls, 1, "uma busca");
    assert!(!first.web_search_rejected);
    assert!(items.iter().any(|b| b["type"] == "web_search_tool_result"));
    assert!(!first.message.content.is_empty());

    messages.push(first.message.clone());
    messages.push(ChatMessage::text(Role::User, "Repeat the version number only, without searching."));
    let second_req = ChatRequest { messages, web_search: false, web_search_max_uses: None, ..first_req };
    let second = c.chat(&second_req).await.expect("segunda volta com o histórico da busca");
    show("segunda volta", &second);
    assert_eq!(second.web_search_calls, 0);
    assert!(!second.message.content.is_empty());
    live.report();
}

/// Spec 133 T04 (C-03): o turno cujo prefixo mudou volta sem os blocos de raciocínio; o resto dos
/// blocos (as chamadas) segue inalterado.
#[test]
fn released_turn_goes_without_thinking() {
    let tool_use = json!({ "type": "tool_use", "id": "t1", "name": "read_file", "input": { "path": "a" } });
    let mut msg = ChatMessage::text(Role::Assistant, "");
    msg.tool_calls = vec![ToolCall { id: "t1".into(), name: "read_file".into(), arguments: r#"{"path":"a"}"#.into() }];
    msg.provider_items = Some(vec![
        json!({ "type": "thinking", "thinking": "pensando", "signature": "SIG" }),
        json!({ "type": "redacted_thinking", "data": "X" }),
        tool_use.clone(),
    ]);
    let mut items = msg.provider_items.clone().expect("itens");
    assert!(anthropic::drop_thinking(&mut items));
    assert!(!anthropic::drop_thinking(&mut items), "nada mais a tirar");
    assert_eq!(items, vec![tool_use]);
    release_prefix_reasoning(&mut msg);
    assert_eq!(msg.provider_items.as_ref(), Some(&items));
}
