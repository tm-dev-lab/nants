//! Testes do adaptador Gemini Interactions (spec 132 T06 e T07), com servidor falso: o corpo exato,
//! o reenvio dos passos `thought` assinados, a assinatura de contorno no turno remontado ou
//! compactado, a escada de relaxamento, o `max_output_tokens`, o uso somado, o stream gravado (que
//! reproduz a resposta sem stream) e o `google_search`. As respostas falsas seguem as gravadas ao
//! vivo em 2026-10-27 e 2026-10-28 (`gemini-3.8-flash`). Os testes ao vivo são `#[ignore]` e só
//! rodam com a variável da chave.

use super::gemini::{self, SKIP_SIGNATURE};
use super::live_tests::{self, Live, Provider};
use super::stream_tests::{ev, raw_server};
use super::tests::{body_of, head_of, spawn_server};
use super::*;
use serde_json::json;

/// Chave falsa montada em tempo de execução (nada com cara de segredo no código).
fn fake_key() -> String {
    ["chave", "gemini", "falsa", "0099"].join("-")
}

/// Endereço do servidor falso sem o `/v1` que ele acrescenta.
fn root(base: &str) -> String {
    base.trim_end_matches("/v1").to_string()
}

/// Cliente Gemini para o servidor falso.
fn client(base: &str) -> LlmClient {
    LlmClient::new(root(base), fake_key()).expect("client").with_kind(ConnectionKind::GoogleGemini)
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
        model: "gemini-teste".into(),
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

const TEXT_REPLY: &str = r#"{"status":"completed","usage":{"total_tokens":6,"total_input_tokens":4,"total_cached_tokens":0,"total_output_tokens":2,"total_tool_use_tokens":0,"total_thought_tokens":0},"steps":[{"signature":"ASSINATURA-texto","type":"thought"},{"content":[{"text":"olá","type":"text"}],"type":"model_output"}],"object":"interaction","model":"gemini-teste"}"#;

/// Resposta com o `thought` assinado (com o resumo) e uma chamada: `requires_action`.
const TOOL_REPLY: &str = r#"{"status":"requires_action","usage":{"total_tokens":275,"total_input_tokens":100,"total_cached_tokens":40,"total_output_tokens":16,"total_tool_use_tokens":0,"total_thought_tokens":159},"steps":[{"signature":"ASSINATURA-opaca-1","summary":[{"text":"preciso ler","type":"text"}],"type":"thought"},{"id":"call_274663","type":"function_call","name":"read_file","arguments":{"path":"a.txt"}}],"object":"interaction","model":"gemini-teste"}"#;

/// 400 do nível de raciocínio recusado (texto visto ao vivo com `thinking_level: minimal`).
const LEVEL_REFUSED: &str = r#"{"error":{"message":"Thinking level THINKING_LEVEL_MINIMAL is not supported for this model. Please retry with other thinking level.","code":"invalid_request"}}"#;

#[tokio::test]
async fn request_goes_to_interactions_with_the_gemini_header() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    let resp = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], None)).await.expect("chat");
    assert_eq!(resp.message.content, "olá");
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    let raw = cap.lock().expect("lock")[0].clone();
    let head = head_of(&raw).to_ascii_lowercase();
    assert!(head.starts_with("post /v1beta/interactions http/1.1"), "{head}");
    assert!(head.contains(&format!("x-goog-api-key: {}", fake_key())));
    assert!(head.contains("content-type: application/json"));
    assert!(!head.contains("authorization:"));
}

/// O corpo exato: `store: false`, o `system_instruction`, a imagem em base64 no `user_input`, o turno
/// remontado com o `thought` de contorno antes da chamada, o `function_result` com o nome e o
/// `call_id`, as ferramentas no formato plano e o esforço em `thinking_level`, sem
/// `max_output_tokens` (limite desconhecido).
#[tokio::test]
async fn exact_body_from_the_chat_request() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    let assistant = ChatMessage {
        tool_calls: vec![ToolCall { id: "c9".into(), name: "read_file".into(), arguments: r#"{"path":"a.txt"}"#.into() }],
        ..ChatMessage::text(Role::Assistant, "vou ler")
    };
    let messages = vec![
        ChatMessage::text(Role::System, "seja breve"),
        ChatMessage::text(Role::System, "responda em português"),
        ChatMessage::with_images("veja", vec![ImageInput::new("image/png", vec![1u8, 2, 3])]),
        assistant,
        tool_result("c9", "conteúdo"),
        ChatMessage::text(Role::User, "continue"),
    ];
    client(&base).chat(&req(messages, Some(Effort::Low))).await.expect("chat");
    let body = body_of(&cap.lock().expect("lock")[0]);
    let expected = json!({
        "model": "gemini-teste",
        "store": false,
        "system_instruction": "seja breve\n\nresponda em português",
        "input": [
            {"type":"user_input","content":[
                {"type":"text","text":"veja"},
                {"type":"image","mime_type":"image/png","data":"AQID"}
            ]},
            {"type":"model_output","content":[{"type":"text","text":"vou ler"}]},
            {"type":"thought","signature":SKIP_SIGNATURE},
            {"type":"function_call","id":"c9","name":"read_file","arguments":{"path":"a.txt"}},
            {"type":"function_result","name":"read_file","call_id":"c9","result":{"content":[{"type":"text","text":"conteúdo"}]}},
            {"type":"user_input","content":[{"type":"text","text":"continue"}]}
        ],
        "tools": [{"type":"function","name":"read_file","description":"Lê um arquivo","parameters":tool().parameters}],
        "generation_config": {"thinking_level":"low","thinking_summaries":"auto"}
    });
    assert_eq!(body, expected);
}

/// Sem esforço, sem `generation_config`; com `response_json`, o `response_format` JSON sem schema.
#[tokio::test]
async fn without_effort_there_is_no_generation_config_and_json_goes_in_the_response_format() {
    let json_reply = r#"{"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"{\"ok\":true}"}]}]}"#;
    let (base, cap) = spawn_server(vec![(200, json_reply)]).await;
    let mut r = req(vec![ChatMessage::text(Role::User, "responda")], None);
    r.response_json = true;
    r.tools.clear();
    let resp = client(&base).chat(&r).await.expect("chat");
    assert_eq!(resp.message.content, r#"{"ok":true}"#);
    assert!(!resp.fallback_used);
    assert!(resp.usage.is_none());
    let body = body_of(&cap.lock().expect("lock")[0]);
    assert_eq!(body["response_format"], json!({"type":"text","mime_type":"application/json"}));
    for absent in ["generation_config", "system_instruction", "tools", "stream", "previous_interaction_id"] {
        assert!(body.get(absent).is_none(), "{absent}: {body}");
    }
    assert_eq!(body["input"].as_array().map(Vec::len), Some(1), "sem a instrução de JSON no texto");
}

/// A segunda volta reenvia os passos do modelo (o `thought` assinado com o resumo e o
/// `function_call`) inalterados, sem o `thought` de contorno; o uso soma o raciocínio à saída e a
/// entrada já inclui o cache.
#[tokio::test]
async fn second_turn_resends_the_signed_steps_unchanged_and_usage_is_summed() {
    let (base, cap) = spawn_server(vec![(200, TOOL_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let mut messages = vec![ChatMessage::text(Role::User, "leia a.txt")];
    let first = c.chat(&req(messages.clone(), Some(Effort::Low))).await.expect("primeira");
    assert_eq!(first.message.content, "");
    assert_eq!(first.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(first.message.tool_calls, vec![ToolCall { id: "call_274663".into(), name: "read_file".into(), arguments: r#"{"path":"a.txt"}"#.into() }]);
    let u = first.usage.expect("uso");
    assert_eq!((u.prompt, u.completion, u.cached, u.reasoning, u.cache_write), (100, 175, Some(40), Some(159), None));
    assert!(!format!("{:?}", first.message).contains("ASSINATURA"), "passos opacos fora do Debug");

    messages.push(first.message.clone());
    messages.push(tool_result("call_274663", "linha"));
    c.chat(&req(messages, Some(Effort::Low))).await.expect("segunda");
    let body = body_of(&cap.lock().expect("lock")[1]);
    let original: Value = serde_json::from_str(TOOL_REPLY).expect("json");
    let input = body["input"].as_array().expect("input");
    assert_eq!(input.len(), 4);
    assert_eq!(input[1], original["steps"][0]);
    assert_eq!(input[2], original["steps"][1]);
    assert_eq!(input[3], json!({"type":"function_result","name":"read_file","call_id":"call_274663","result":{"content":[{"type":"text","text":"linha"}]}}));
}

/// Escada: o `thinking_summaries` recusado sai sozinho; o nível recusado (a mensagem vista ao vivo)
/// tira o `thinking_level`, e a resposta marca o recuo do esforço.
#[tokio::test]
async fn thinking_ladder_drops_the_summaries_then_the_level() {
    let summaries = r#"{"error":{"message":"Unknown name \"thinking_summaries\" at 'generation_config'","code":"invalid_request"}}"#;
    let (base, cap) = spawn_server(vec![(400, summaries), (400, LEVEL_REFUSED), (200, TEXT_REPLY)]).await;
    let resp = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::Medium))).await.expect("chat");
    assert!(resp.fallback_used);
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[0]["generation_config"], json!({"thinking_level":"medium","thinking_summaries":"auto"}));
    assert_eq!(bodies[1]["generation_config"], json!({"thinking_level":"medium"}));
    assert!(bodies[2].get("generation_config").is_none(), "{}", bodies[2]);
}

/// Cada degrau relaxa uma vez: o nível recusado de novo, já sem ele, sobe como erro.
#[tokio::test]
async fn ladder_relaxes_each_step_once() {
    let (base, cap) = spawn_server(vec![(400, LEVEL_REFUSED), (400, LEVEL_REFUSED)]).await;
    let err = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::High))).await.expect_err("400");
    assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), 2);
}

#[tokio::test]
async fn unrelated_400_is_not_relaxed() {
    let other = r#"{"error":{"message":"Request contains an invalid argument.","code":"invalid_request"}}"#;
    let (base, cap) = spawn_server(vec![(400, other)]).await;
    let err = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::Low))).await.expect_err("400");
    assert!(matches!(err, LlmError::Http { status: 400, .. }));
    assert_eq!(cap.lock().expect("lock").len(), 1);
}

/// O limite conhecido vai em `max_output_tokens`; o 400 que o cita reduz uma vez para o valor
/// indicado, que fica lembrado para o (endereço, modelo).
#[tokio::test]
async fn max_output_tokens_is_known_reduced_once_and_remembered() {
    let too_big = r#"{"error":{"message":"max_output_tokens: 64000 is above the limit of 8192 for gemini-teste","code":"invalid_request"}}"#;
    let (base, cap) = spawn_server(vec![(400, too_big), (200, TEXT_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    gemini::remember_max_tokens(&root(&base), "gemini-teste", 64_000);
    let r = req(vec![ChatMessage::text(Role::User, "oi")], None);
    c.chat(&r).await.expect("chat");
    c.chat(&r).await.expect("chat de novo");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[0]["generation_config"], json!({"max_output_tokens":64_000}));
    assert_eq!(bodies[1]["generation_config"], json!({"max_output_tokens":8_192}));
    assert_eq!(bodies[2]["generation_config"], json!({"max_output_tokens":8_192}));
    assert_eq!(gemini::known_max_tokens(&root(&base), "gemini-teste"), Some(8_192));
}

/// O estouro do contexto não relaxa nada e é reconhecido como estouro (o resumo do histórico
/// dispara).
#[tokio::test]
async fn context_overflow_is_not_relaxed() {
    let overflow = r#"{"error":{"message":"The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).","code":"invalid_request"}}"#;
    let (base, cap) = spawn_server(vec![(400, overflow)]).await;
    let err = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::Low))).await.expect_err("estouro");
    assert!(crate::agents::summary::is_context_overflow(&err), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), 1, "sem relaxamento");
}

/// Pedido sem ferramentas com `function_call` no histórico (o resumo do histórico): a API aceita
/// sem declarar as ferramentas (conferido ao vivo); os passos assinados seguem inalterados.
#[tokio::test]
async fn request_without_tools_keeps_the_history_steps() {
    let (base, cap) = spawn_server(vec![(200, TOOL_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let first = c.chat(&req(vec![ChatMessage::text(Role::User, "leia")], Some(Effort::Low))).await.expect("primeira");
    let mut r = req(
        vec![ChatMessage::text(Role::User, "leia"), first.message, tool_result("call_274663", "x"), ChatMessage::text(Role::User, "resuma")],
        Some(Effort::Low),
    );
    r.tools.clear();
    c.chat(&r).await.expect("resumo");
    let body = body_of(&cap.lock().expect("lock")[1]);
    assert!(body.get("tools").is_none(), "{body}");
    assert_eq!(body["input"][1]["signature"], "ASSINATURA-opaca-1");
}

/// Compactação (spec 122): trocar os argumentos de um `function_call` guardado descarta os
/// `thought` daquele turno, e o reenvio põe o `thought` de contorno antes da chamada (a API recusa a
/// chamada sem um `thought` antes, conferido ao vivo).
#[tokio::test]
async fn rewriting_a_stored_call_drops_the_thought_and_resends_the_bypass() {
    let mut msg = gemini::parse(TOOL_REPLY, false).expect("parse").message;
    rewrite_call_arguments(&mut msg, "call_274663", r#"{"path":"[omitido]"}"#);
    let items = msg.provider_items.clone().expect("itens");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["arguments"], json!({"path":"[omitido]"}));
    // Outra chamada: nada muda.
    let mut other = gemini::parse(TOOL_REPLY, false).expect("parse").message;
    rewrite_call_arguments(&mut other, "call_x", r#"{"path":"b"}"#);
    assert_eq!(other.provider_items.map(|i| i.len()), Some(2));

    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    msg.tool_calls[0].arguments = r#"{"path":"[omitido]"}"#.into();
    let messages = vec![ChatMessage::text(Role::User, "leia"), msg, tool_result("call_274663", "x")];
    client(&base).chat(&req(messages, Some(Effort::Low))).await.expect("chat");
    let body = body_of(&cap.lock().expect("lock")[0]);
    assert_eq!(body["input"][1], json!({"type":"thought","signature":SKIP_SIGNATURE}));
    assert_eq!(body["input"][2]["arguments"], json!({"path":"[omitido]"}));
}

/// Os itens da Responses (com `call_id`) não são passos da Interactions; os do Gemini têm o id
/// sincronizado no campo `id`.
#[test]
fn synthetic_ids_are_mirrored_in_function_call_steps() {
    let mut msg = gemini::parse(TOOL_REPLY, false).expect("parse").message;
    msg.tool_calls[0].id = "nants-synth-1".into();
    sync_provider_call_ids(&mut msg);
    let items = msg.provider_items.expect("itens");
    assert_eq!(items[1]["id"], "nants-synth-1");
    assert!(items[1].get("call_id").is_none());
    assert!(!gemini::is_step(&json!({"type":"function_call","call_id":"c","arguments":"{}"})));
    assert!(!gemini::is_step(&json!({"type":"reasoning"})));
    assert!(gemini::is_step(&json!({"type":"thought","signature":"s"})));
}

#[test]
fn parse_maps_statuses_errors_and_usage() {
    let incomplete = r#"{"status":"incomplete","usage":{"total_input_tokens":12,"total_output_tokens":16},"steps":[{"type":"model_output","content":[{"type":"text","text":"parci"}]}]}"#;
    let r = gemini::parse(incomplete, false).expect("parse");
    assert_eq!(r.finish_reason.as_deref(), Some("length"));
    let u = r.usage.expect("uso");
    assert_eq!((u.prompt, u.completion, u.cached, u.reasoning), (12, 16, None, None));

    let cut = r#"{"status":"incomplete","steps":[{"type":"thought","signature":"s"}]}"#;
    let err = gemini::parse(cut, false).expect_err("sem texto");
    assert!(matches!(&err, LlmError::Parse(r) if r.kind == BadKind::Incomplete), "{err:?}");

    let empty = r#"{"status":"completed","steps":[{"type":"thought","signature":"s"}]}"#;
    assert!(matches!(gemini::parse(empty, false), Err(LlmError::Parse(r)) if r.kind == BadKind::Empty));

    let failed = r#"{"status":"failed","steps":[]}"#;
    assert!(matches!(gemini::parse(failed, false), Err(LlmError::Parse(r)) if r.kind == BadKind::UnsupportedStatus));

    let error = r#"{"error":{"message":"Blocked","code":"prohibited_content"}}"#;
    let err = gemini::parse(error, false).expect_err("erro");
    assert!(matches!(&err, LlmError::Parse(r) if r.kind == BadKind::ProviderError), "{err:?}");
    assert!(matches!(gemini::parse("nada", false), Err(LlmError::Parse(r)) if r.kind == BadKind::InvalidJson));

    // Dois `model_output` separados viram duas mensagens; as partes de um só, um texto.
    let two = r#"{"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]},{"type":"model_output","content":[{"type":"text","text":"c"}]}]}"#;
    let r = gemini::parse(two, false).expect("parse");
    assert!(r.message.content.starts_with("ab") && r.message.content.ends_with('c'), "{}", r.message.content);
}

/// O 503 (`service_unavailable`) é transitório: a chamada se repete.
#[tokio::test]
async fn unavailable_is_retried() {
    let busy = r#"{"error":{"message":"The model is overloaded.","code":"service_unavailable"}}"#;
    let (base, cap) = spawn_server(vec![(503, busy), (200, TEXT_REPLY)]).await;
    let resp = client(&base)
        .with_retry_delays(vec![std::time::Duration::from_millis(1)])
        .chat(&req(vec![ChatMessage::text(Role::User, "oi")], None))
        .await
        .expect("chat");
    assert_eq!(resp.message.content, "olá");
    assert_eq!(cap.lock().expect("lock").len(), 2);
}

// ---- Várias chamadas num turno (observações da T06) ----

/// Resposta com o `thought` assinado e duas chamadas no mesmo turno.
const TWO_CALLS_REPLY: &str = r#"{"status":"requires_action","usage":{"total_input_tokens":30,"total_output_tokens":20},"steps":[{"signature":"ASSINATURA-dupla","type":"thought"},{"id":"call_1","type":"function_call","name":"read_file","arguments":{"path":"a.txt"}},{"id":"call_2","type":"function_call","name":"read_file","arguments":{"path":"b.txt"}}]}"#;

/// Duas chamadas na resposta: as duas viram `ToolCall`; o reenvio leva os passos inalterados (um
/// só `thought`, sem o de contorno) e um `function_result` para cada chamada, na ordem.
#[tokio::test]
async fn two_calls_in_a_turn_are_resent_with_one_result_each() {
    let (base, cap) = spawn_server(vec![(200, TWO_CALLS_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let mut messages = vec![ChatMessage::text(Role::User, "leia a e b")];
    let first = c.chat(&req(messages.clone(), Some(Effort::Low))).await.expect("primeira");
    let ids: Vec<&str> = first.message.tool_calls.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["call_1", "call_2"]);
    messages.push(first.message.clone());
    messages.push(tool_result("call_1", "A"));
    messages.push(tool_result("call_2", ""));
    c.chat(&req(messages, Some(Effort::Low))).await.expect("segunda");
    let body = body_of(&cap.lock().expect("lock")[1]);
    let original: Value = serde_json::from_str(TWO_CALLS_REPLY).expect("json");
    let input = body["input"].as_array().expect("input");
    assert_eq!(input.len(), 6, "{body}");
    assert_eq!(input[1..4], original["steps"].as_array().expect("passos")[..]);
    assert_eq!(input.iter().filter(|s| s["signature"] == SKIP_SIGNATURE).count(), 0);
    assert_eq!(input[4], json!({"type":"function_result","name":"read_file","call_id":"call_1","result":{"content":[{"type":"text","text":"A"}]}}));
    // O resultado vazio vai como texto vazio (aceito ao vivo em 2026-10-28).
    assert_eq!(input[5], json!({"type":"function_result","name":"read_file","call_id":"call_2","result":{"content":[{"type":"text","text":""}]}}));
}

/// Turno remontado com duas chamadas: o `thought` de contorno vai uma vez só, antes da primeira.
#[tokio::test]
async fn remounted_turn_with_two_calls_gets_the_bypass_once() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY)]).await;
    let assistant = ChatMessage {
        tool_calls: vec![
            ToolCall { id: "c1".into(), name: "read_file".into(), arguments: r#"{"path":"a"}"#.into() },
            ToolCall { id: "c2".into(), name: "read_file".into(), arguments: r#"{"path":"b"}"#.into() },
        ],
        ..ChatMessage::text(Role::Assistant, "")
    };
    let messages = vec![ChatMessage::text(Role::User, "leia"), assistant, tool_result("c1", "x"), tool_result("c2", "y")];
    client(&base).chat(&req(messages, Some(Effort::Low))).await.expect("chat");
    let body = body_of(&cap.lock().expect("lock")[0]);
    let kinds: Vec<&str> = body["input"].as_array().expect("input").iter().filter_map(|s| s["type"].as_str()).collect();
    assert_eq!(kinds, ["user_input", "thought", "function_call", "function_call", "function_result", "function_result"]);
    assert_eq!(body["input"][1], json!({"type":"thought","signature":SKIP_SIGNATURE}));
    assert_eq!(body["input"][5]["call_id"], "c2");
}

// ---- Stream (T07) ----

const SSE: &str = "text/event-stream";

/// Evento SSE da Interactions: o nome do evento é o `event_type` do JSON.
fn sse(data: Value) -> String {
    let name = data["event_type"].as_str().unwrap_or_default().to_string();
    ev(&name, &data.to_string())
}

fn start(index: u64, step: Value) -> String {
    sse(json!({"event_type":"step.start","index":index,"step":step}))
}

fn delta(index: u64, delta: Value) -> String {
    sse(json!({"event_type":"step.delta","index":index,"delta":delta}))
}

fn stop(index: u64) -> String {
    sse(json!({"event_type":"step.stop","index":index}))
}

fn opening() -> String {
    let mut s = sse(json!({"event_type":"interaction.created","interaction":{"object":"interaction","model":"gemini-teste"}}));
    s += &sse(json!({"event_type":"interaction.status_update","status":"in_progress"}));
    s
}

fn completed(reply: &str) -> String {
    let v: Value = serde_json::from_str(reply).expect("json");
    let mut s = sse(json!({"event_type":"interaction.completed","interaction":{"status":v["status"],"usage":v["usage"],"object":"interaction"}}));
    s += "event: done\r\ndata: [DONE]\r\n\r\n";
    s
}

/// Gravação do stream equivalente a [`TOOL_REPLY`], como vista ao vivo em 2026-10-28: o `thought`
/// com o resumo em dois pedaços e a assinatura no último delta, a chamada com `arguments: {}` no
/// start e o JSON em pedaços, e o uso no `interaction.completed`.
fn tool_reply_stream() -> String {
    let mut s = opening();
    s += &start(0, json!({"type":"thought"}));
    s += &delta(0, json!({"type":"thought_summary","content":{"type":"text","text":"preciso "}}));
    s += &delta(0, json!({"type":"thought_summary","content":{"type":"text","text":"ler"}}));
    s += &delta(0, json!({"type":"thought_signature","signature":"ASSINATURA-opaca-1"}));
    s += &stop(0);
    s += &start(1, json!({"id":"call_274663","type":"function_call","name":"read_file","arguments":{}}));
    s += &delta(1, json!({"type":"arguments_delta","arguments":"{\"path\":"}));
    s += &delta(1, json!({"type":"arguments_delta","arguments":"\"a.txt\"}"}));
    s += &stop(1);
    s += &completed(TOOL_REPLY);
    s
}

/// Stream equivalente a [`TEXT_REPLY`].
fn text_reply_stream() -> String {
    let mut s = opening();
    s += &start(0, json!({"type":"thought"}));
    s += &delta(0, json!({"type":"thought_signature","signature":"ASSINATURA-texto"}));
    s += &stop(0);
    s += &start(1, json!({"type":"model_output"}));
    s += &delta(1, json!({"type":"text","text":"ol"}));
    s += &delta(1, json!({"type":"text","text":"á"}));
    s += &stop(1);
    s += &completed(TEXT_REPLY);
    s
}

type Seen = std::sync::Arc<std::sync::Mutex<Vec<StreamDelta>>>;

fn collector() -> (Seen, impl Fn(StreamDelta) + Send + Sync) {
    let seen: Seen = Default::default();
    let seen2 = std::sync::Arc::clone(&seen);
    (seen, move |d: StreamDelta| seen2.lock().expect("lock").push(d))
}

fn cuts(seen: &Seen) -> Vec<std::sync::Arc<StreamCut>> {
    seen.lock()
        .expect("lock")
        .iter()
        .filter_map(|d| if let StreamDelta::Cut(c) = d { Some(std::sync::Arc::clone(c)) } else { None })
        .collect()
}

/// O stream gravado reproduz o mesmo `ChatResponse` da versão sem stream (o `thought` com o resumo
/// inteiro e a assinatura, os argumentos parseados e o uso somado), com `stream: true` no corpo e os
/// deltas de raciocínio entregues à tela.
#[tokio::test]
async fn recorded_stream_reproduces_the_blocking_response() {
    let (base, cap) = raw_server(vec![(200, SSE, tool_reply_stream())]).await;
    let (seen, on) = collector();
    let r = req(vec![ChatMessage::text(Role::User, "leia a.txt")], Some(Effort::Low));
    let streamed = client(&base).chat_stream(&r, &on).await.expect("stream");
    assert_eq!(streamed, gemini::parse(TOOL_REPLY, false).expect("parse"));
    assert_eq!(streamed.finish_reason.as_deref(), Some("tool_calls"), "requires_action é o pedido de ferramenta");

    let raw = cap.lock().expect("lock")[0].clone();
    assert!(head_of(&raw).to_ascii_lowercase().starts_with("post /v1beta/interactions http/1.1"));
    let body = body_of(&raw);
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["generation_config"], json!({"thinking_level":"low","thinking_summaries":"auto"}));

    let shown: Vec<StreamDelta> = seen.lock().expect("lock").iter().cloned().filter_map(super::not_size).collect();
    assert_eq!(shown, vec![StreamDelta::Reasoning("preciso ".into()), StreamDelta::Reasoning("ler".into())]);
}

/// A volta seguinte (também em stream) reenvia os passos remontados inalterados, como na versão sem
/// stream, e o texto chega em deltas.
#[tokio::test]
async fn streamed_turn_is_resent_unchanged() {
    let (base, cap) = raw_server(vec![(200, SSE, tool_reply_stream()), (200, SSE, text_reply_stream())]).await;
    let c = client(&base);
    let (seen, on) = collector();
    let mut messages = vec![ChatMessage::text(Role::User, "leia a.txt")];
    let first = c.chat_stream(&req(messages.clone(), Some(Effort::Low)), &on).await.expect("primeira");
    messages.push(first.message);
    messages.push(tool_result("call_274663", "linha"));
    let second = c.chat_stream(&req(messages, Some(Effort::Low)), &on).await.expect("segunda");
    assert_eq!(second, gemini::parse(TEXT_REPLY, false).expect("parse"));
    let body = body_of(&cap.lock().expect("lock")[1]);
    let original: Value = serde_json::from_str(TOOL_REPLY).expect("json");
    assert_eq!(body["input"][1], original["steps"][0]);
    assert_eq!(body["input"][2], original["steps"][1]);
    let texts: Vec<StreamDelta> =
        seen.lock().expect("lock").iter().filter(|d| matches!(d, StreamDelta::Text(_))).cloned().collect();
    assert_eq!(texts, vec![StreamDelta::Text("ol".into()), StreamDelta::Text("á".into())]);
}

/// O evento `error` no meio do stream sobe com o motivo do provedor, sem diagnóstico de corte.
#[tokio::test]
async fn stream_error_event_is_a_provider_error() {
    let mut failing = opening();
    failing += &sse(json!({"event_type":"error","error":{"code":"prohibited_content","message":"Blocked"}}));
    let (base, cap) = raw_server(vec![(200, SSE, failing)]).await;
    let (seen, on) = collector();
    let err = client(&base)
        .with_retry_delays(Vec::new())
        .chat_stream(&req(vec![ChatMessage::text(Role::User, "oi")], None), &on)
        .await
        .expect_err("erro do stream");
    assert!(matches!(&err, LlmError::Parse(r) if r.kind == BadKind::ProviderError), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), 1);
    assert!(cuts(&seen).is_empty());
}

/// Stream que acaba sem `interaction.completed` (só o `[DONE]`) vira o diagnóstico de corte da spec
/// 114, com os trechos do raciocínio e do texto.
#[tokio::test]
async fn stream_without_completed_is_a_cut_with_the_diagnosis() {
    let mut s = opening();
    s += &start(0, json!({"type":"thought"}));
    s += &delta(0, json!({"type":"thought_summary","content":{"type":"text","text":"pensando"}}));
    s += &start(1, json!({"type":"model_output"}));
    s += &delta(1, json!({"type":"text","text":"começo da resposta"}));
    s += "event: done\r\ndata: [DONE]\r\n\r\n";
    let (base, _) = raw_server(vec![(200, SSE, s.clone()), (200, SSE, s)]).await;
    let (seen, on) = collector();
    let err = client(&base)
        .with_retry_delays(Vec::new())
        .chat_stream(&req(vec![ChatMessage::text(Role::User, "oi")], None), &on)
        .await
        .expect_err("cortada");
    assert!(matches!(&err, LlmError::Parse(r) if r.kind == BadKind::NoFinal), "{err:?}");
    let found = cuts(&seen);
    let cut = found.first().expect("diagnóstico");
    assert_eq!(cut.reason, "no_final");
    assert!(cut.reported.is_none(), "o uso só vem no interaction.completed");
    let (head, _) = cut.snippets(str::to_string);
    assert!(head.contains("pensando") && head.contains("começo da resposta"), "{head}");
    assert!(cut.top_events.iter().any(|(k, _)| k == "step.delta"), "{:?}", cut.top_events);
}

/// Resposta 200 em JSON comum (sem SSE) no caminho do stream é lida como a da versão sem stream.
#[tokio::test]
async fn non_sse_reply_on_the_stream_path_is_parsed_as_blocking() {
    let (base, _) = raw_server(vec![(200, "application/json", TOOL_REPLY.to_string())]).await;
    let (_seen, on) = collector();
    let r = req(vec![ChatMessage::text(Role::User, "leia a.txt")], Some(Effort::Low));
    let resp = client(&base).chat_stream(&r, &on).await.expect("chat");
    assert_eq!(resp, gemini::parse(TOOL_REPLY, false).expect("parse"));
}

/// A escada de relaxamento vale no stream: o nível recusado sai, com `stream: true`.
#[tokio::test]
async fn stream_relaxes_the_thinking_on_400() {
    let (base, cap) = raw_server(vec![(400, "application/json", LEVEL_REFUSED.to_string()), (200, SSE, text_reply_stream())]).await;
    let (_seen, on) = collector();
    let resp = client(&base).chat_stream(&req(vec![ChatMessage::text(Role::User, "oi")], Some(Effort::Medium)), &on).await.expect("chat");
    assert_eq!(resp.message.content, "olá");
    assert!(resp.fallback_used);
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert!(bodies[1].get("generation_config").is_none(), "{}", bodies[1]);
    assert_eq!(bodies[1]["stream"], true);
}

/// O 503 antes do primeiro delta se repete pelo stream.
#[tokio::test]
async fn stream_retries_unavailable() {
    let busy = r#"{"error":{"message":"The model is overloaded.","code":"service_unavailable"}}"#;
    let (base, cap) = raw_server(vec![(503, "application/json", busy.to_string()), (200, SSE, text_reply_stream())]).await;
    let (_seen, on) = collector();
    let resp = client(&base)
        .with_retry_delays(vec![std::time::Duration::from_millis(1)])
        .chat_stream(&req(vec![ChatMessage::text(Role::User, "oi")], None), &on)
        .await
        .expect("chat");
    assert_eq!(resp.message.content, "olá");
    assert_eq!(cap.lock().expect("lock").len(), 2);
}

/// Passa `events` por um [`gemini::StreamAcc`] e devolve a resposta montada.
fn run_acc(events: &[String]) -> ChatResponse {
    let mut acc = gemini::StreamAcc::default();
    let on = |_: StreamDelta| {};
    for e in events {
        let data = e.lines().find_map(|l| l.strip_prefix("data: ")).expect("data");
        acc.event("", data, &on).expect("evento");
    }
    acc.finish(false).expect("resposta")
}

/// Deltas antes do `step.start` (fora de ordem, intercalados com outro passo) são aplicados nele:
/// nada se perde, e o resultado é o da versão sem stream. Delta de um passo cujo start nunca chega
/// não deixa passo sem `type`.
#[test]
fn early_deltas_are_kept_and_orphans_leave_no_untyped_step() {
    let events = vec![
        delta(0, json!({"type":"thought_summary","content":{"type":"text","text":"pen"}})),
        start(1, json!({"id":"c1","type":"function_call","name":"read_file","arguments":{}})),
        delta(1, json!({"type":"arguments_delta","arguments":"{\"path\":"})),
        start(0, json!({"type":"thought"})),
        delta(0, json!({"type":"thought_summary","content":{"type":"text","text":"sando"}})),
        delta(1, json!({"type":"arguments_delta","arguments":"\"a.txt\"}"})),
        delta(0, json!({"type":"thought_signature","signature":"SIG-1"})),
        delta(2, json!({"type":"text","text":"vou "})),
        start(2, json!({"type":"model_output"})),
        delta(2, json!({"type":"text","text":"ler"})),
        delta(3, json!({"type":"text","text":"solto"})),
        stop(1),
        stop(0),
        stop(2),
        sse(json!({"event_type":"interaction.completed","interaction":{"status":"requires_action","usage":{"total_input_tokens":4,"total_output_tokens":9}}})),
    ];
    let expected = json!({
        "status":"requires_action",
        "steps":[
            {"type":"thought","summary":[{"type":"text","text":"pensando"}],"signature":"SIG-1"},
            {"id":"c1","type":"function_call","name":"read_file","arguments":{"path":"a.txt"}},
            {"type":"model_output","content":[{"type":"text","text":"vou ler"}]}
        ],
        "usage":{"total_input_tokens":4,"total_output_tokens":9}
    });
    let r = run_acc(&events);
    assert!(r.message.provider_items.iter().flatten().all(|s| s["type"].is_string()));
    assert_eq!(r, gemini::parse(&expected.to_string(), false).expect("parse"));
}

/// Chamada cortada no meio do JSON dos argumentos: vira objeto vazio, aceito no reenvio.
#[test]
fn truncated_arguments_become_an_empty_object() {
    let events = vec![
        start(0, json!({"id":"c1","type":"function_call","name":"read_file","arguments":{}})),
        delta(0, json!({"type":"arguments_delta","arguments":"{\"path\": \"a"})),
        sse(json!({"event_type":"interaction.completed","interaction":{"status":"incomplete","usage":{"total_output_tokens":4}}})),
    ];
    let r = run_acc(&events);
    assert_eq!(r.message.tool_calls[0].arguments, "{}");
    assert_eq!(r.finish_reason.as_deref(), Some("tool_calls"));
}

// ---- google_search (T07) ----

/// Pedido do Arquiteto com a busca ligada e `left` buscas restantes.
fn web_req(messages: Vec<ChatMessage>, left: u32) -> ChatRequest {
    ChatRequest { web_search: true, web_search_max_uses: Some(left), ..req(messages, Some(Effort::Low)) }
}

/// Resposta com uma busca de duas consultas (como a vista ao vivo em 2026-10-28, com as assinaturas
/// trocadas): a chamada, o resultado, o `thought` e o texto com as citações. O
/// `grounding_tool_count` informa 2, que não é somado às consultas.
const WEB_REPLY: &str = r#"{"status":"completed","usage":{"total_input_tokens":25,"total_output_tokens":38,"total_thought_tokens":0,"grounding_tool_count":[{"type":"google_search","count":2}]},"steps":[{"id":"call_256839","signature":"ASSINATURA-busca","type":"google_search_call","search_type":"web_search","arguments":{"queries":["rust versão estável","rust release"]}},{"call_id":"call_256839","signature":"ASSINATURA-resultado","type":"google_search_result","result":[{"search_suggestions":"<div>sugestões</div>"}],"is_error":false},{"type":"thought","signature":"ASSINATURA-depois"},{"type":"model_output","content":[{"type":"text","text":"A versão estável é a 1.99.1, de outubro.","annotations":[{"start_index":0,"end_index":20,"url":"https://a.example/x","title":"rust-lang.org","type":"url_citation"},{"start_index":21,"end_index":40,"url":"https://b.example/y","title":"blog","type":"url_citation"},{"start_index":0,"end_index":40,"url":"https://a.example/x","title":"rust-lang.org","type":"url_citation"}]}]}]}"#;

/// Stream equivalente a [`WEB_REPLY`], com os eventos vistos ao vivo: a assinatura vazia no start
/// das ferramentas internas e preenchida no delta, e as citações num `text_annotation_delta` depois
/// do texto.
fn web_reply_stream() -> String {
    let v: Value = serde_json::from_str(WEB_REPLY).expect("json");
    let steps = &v["steps"];
    let mut s = opening();
    s += &start(0, json!({"id":"call_256839","signature":"","type":"google_search_call","search_type":"web_search"}));
    s += &delta(0, json!({"type":"google_search_call","signature":"ASSINATURA-busca","arguments":steps[0]["arguments"]}));
    s += &stop(0);
    s += &start(1, json!({"call_id":"call_256839","signature":"","type":"google_search_result"}));
    s += &delta(1, json!({"type":"google_search_result","signature":"ASSINATURA-resultado","result":steps[1]["result"],"is_error":false}));
    s += &stop(1);
    s += &start(2, json!({"type":"thought"}));
    s += &delta(2, json!({"type":"thought_signature","signature":"ASSINATURA-depois"}));
    s += &stop(2);
    s += &start(3, json!({"type":"model_output"}));
    s += &delta(3, json!({"type":"text","text":"A versão estável é a 1.99.1,"}));
    s += &delta(3, json!({"type":"text","text":" de outubro."}));
    s += &delta(3, json!({"type":"text_annotation_delta","annotations":steps[3]["content"][0]["annotations"]}));
    s += &stop(3);
    s += &completed(WEB_REPLY);
    s
}

/// O corpo leva `{"type":"google_search"}` antes das ferramentas do cliente, sem `max_uses` (a
/// Interactions não tem o campo); com o limite esgotado, nada de busca.
#[tokio::test]
async fn google_search_goes_in_the_tools_when_requested() {
    let (base, cap) = spawn_server(vec![(200, TEXT_REPLY), (200, TEXT_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    c.chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 3)).await.expect("com busca");
    c.chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 0)).await.expect("limite esgotado");
    c.chat(&req(vec![ChatMessage::text(Role::User, "oi")], None)).await.expect("sem busca");
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    let tools = bodies[0]["tools"].as_array().expect("tools");
    assert_eq!(tools[0], json!({"type":"google_search"}));
    assert_eq!(tools[1]["name"], "read_file");
    assert_eq!(tools.len(), 2);
    for body in &bodies[1..] {
        assert!(!body["tools"].to_string().contains("google_search"), "{body}");
    }
}

/// A contagem é o número de `queries` (2, sem somar o `grounding_tool_count`), as fontes são as
/// `url_citation` sem repetir, a chamada interna não é do cliente, e a volta seguinte reenvia os
/// passos da busca inalterados.
#[tokio::test]
async fn search_is_counted_cited_and_resent_unchanged() {
    let (base, cap) = spawn_server(vec![(200, WEB_REPLY), (200, TEXT_REPLY)]).await;
    let c = client(&base);
    let mut messages = vec![ChatMessage::text(Role::User, "qual a versão do rust?")];
    let first = c.chat(&web_req(messages.clone(), 3)).await.expect("primeira");
    assert_eq!(first.web_search_calls, 2);
    assert!(!first.web_search_rejected);
    assert_eq!(first.message.content, "A versão estável é a 1.99.1, de outubro.");
    assert_eq!(first.finish_reason.as_deref(), Some("stop"));
    assert!(first.message.tool_calls.is_empty());
    assert_eq!(
        first.citations,
        vec![
            Citation { url: "https://a.example/x".into(), title: "rust-lang.org".into() },
            Citation { url: "https://b.example/y".into(), title: "blog".into() },
        ]
    );
    assert!(!format!("{:?}", first.message).contains("ASSINATURA"), "passos opacos fora do Debug");

    messages.push(first.message.clone());
    messages.push(ChatMessage::text(Role::User, "e a anterior?"));
    c.chat(&req(messages, Some(Effort::Low))).await.expect("segunda, sem a busca");
    let body = body_of(&cap.lock().expect("lock")[1]);
    let original: Value = serde_json::from_str(WEB_REPLY).expect("json");
    let input = body["input"].as_array().expect("input");
    assert_eq!(input[1..5], original["steps"].as_array().expect("passos")[..]);
    assert!(!body["tools"].to_string().contains("google_search"));
}

/// O stream gravado da busca reproduz a versão sem stream, com as consultas contadas e as fontes.
#[tokio::test]
async fn recorded_web_stream_reproduces_the_blocking_response() {
    let (base, _cap) = raw_server(vec![(200, SSE, web_reply_stream())]).await;
    let (seen, on) = collector();
    let streamed = client(&base).chat_stream(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 2), &on).await.expect("stream");
    assert_eq!(streamed, gemini::parse(WEB_REPLY, false).expect("parse"));
    assert_eq!(streamed.web_search_calls, 2);
    assert_eq!(streamed.citations.len(), 2);
    let texts = seen.lock().expect("lock").iter().filter(|d| matches!(d, StreamDelta::Text(_))).count();
    assert_eq!(texts, 2);
}

/// Turno que termina numa chamada de função depois de duas buscas (spec 132 T12): sem
/// `url_citation`, as fontes são os itens dos `google_search_result` com `url`, na ordem e sem
/// repetir; o widget `search_suggestions` não vira fonte.
#[test]
fn without_citations_the_sources_are_the_search_results() {
    let reply = r#"{"status":"requires_action","usage":{"total_input_tokens":85,"total_output_tokens":21},"steps":[
        {"id":"c1","signature":"S1","type":"google_search_call","arguments":{"queries":["a"]}},
        {"call_id":"c1","signature":"R1","type":"google_search_result","result":[{"search_suggestions":"<a href=\"https://www.google.com/search?q=a\">a</a>"},{"url":"https://b.example/","title":"B"},{"url":"https://a.example/","title":"A"}],"is_error":false},
        {"id":"c2","signature":"S2","type":"google_search_call","arguments":{"queries":["b"]}},
        {"call_id":"c2","signature":"R2","type":"google_search_result","result":[{"url":"https://a.example/","title":"A de novo"},{"url":"https://c.example/"},{"url":""}],"is_error":false},
        {"type":"thought","signature":"T"},
        {"id":"f1","type":"function_call","name":"save_note","arguments":{"text":"1.99.1"}}]}"#;
    let parsed = gemini::parse(reply, false).expect("parse");
    assert_eq!(parsed.web_search_calls, 2);
    assert_eq!(parsed.message.tool_calls.len(), 1);
    assert_eq!(
        parsed.citations,
        vec![
            Citation { url: "https://b.example/".into(), title: "B".into() },
            Citation { url: "https://a.example/".into(), title: "A".into() },
            Citation { url: "https://c.example/".into(), title: String::new() },
        ]
    );
}

/// Como visto ao vivo em 2026-10-28: o `google_search_result` só com o widget `search_suggestions`
/// (links de pesquisa do Google, não fontes) e o turno terminando na chamada de função. A busca é
/// contada, e nenhuma fonte é inventada.
#[test]
fn search_suggestions_alone_yield_no_sources() {
    let reply = r#"{"status":"requires_action","usage":{"total_input_tokens":85,"total_output_tokens":21},"steps":[
        {"id":"c1","signature":"S1","type":"google_search_call","arguments":{"queries":["rust releases stable version","latest stable rust release version"]}},
        {"call_id":"c1","signature":"R1","type":"google_search_result","result":[{"search_suggestions":"<a class=\"chip\" href=\"https://www.google.com/search?q=rust+releases\">rust releases</a>"}],"is_error":false},
        {"type":"thought","signature":"T"},
        {"id":"f1","type":"function_call","name":"save_note","arguments":{"text":"1.99.1"}}]}"#;
    let parsed = gemini::parse(reply, false).expect("parse");
    assert_eq!(parsed.web_search_calls, 2);
    assert_eq!(parsed.message.tool_calls.len(), 1);
    assert!(parsed.citations.is_empty(), "{:?}", parsed.citations);
}

/// Com `url_citation` no texto, os itens do resultado não entram: as fontes são só as citadas.
#[test]
fn citations_in_the_text_win_over_the_search_results() {
    let mut v: Value = serde_json::from_str(WEB_REPLY).expect("json");
    v["steps"][1]["result"] = json!([{"url":"https://z.example/","title":"Z"}]);
    let parsed = gemini::parse(&v.to_string(), false).expect("parse");
    let urls: Vec<&str> = parsed.citations.iter().map(|c| c.url.as_str()).collect();
    assert_eq!(urls, vec!["https://a.example/x", "https://b.example/y"]);
}

/// O 400 que cita a busca relaxa uma vez, para sem busca (o esforço fica), e marca a recusa; outro
/// 400 que cite a busca, já sem ela, sobe.
#[tokio::test]
async fn search_refused_relaxes_once_to_no_search() {
    let refused = r#"{"error":{"message":"Tool google_search is not supported for this model.","code":"invalid_request"}}"#;
    let (base, cap) = spawn_server(vec![(400, refused), (200, TEXT_REPLY)]).await;
    let resp = client(&base).chat(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 2)).await.expect("chat");
    assert!(resp.web_search_rejected);
    assert!(!resp.fallback_used);
    let bodies: Vec<Value> = cap.lock().expect("lock").iter().map(|r| body_of(r)).collect();
    assert_eq!(bodies[0]["tools"][0], json!({"type":"google_search"}));
    assert!(!bodies[1]["tools"].to_string().contains("google_search"));
    assert_eq!(bodies[1]["generation_config"], json!({"thinking_level":"low","thinking_summaries":"auto"}));

    let (base, cap) = raw_server(vec![(400, "application/json", refused.to_string()), (400, "application/json", refused.to_string())]).await;
    let (_seen, on) = collector();
    let err = client(&base).chat_stream(&web_req(vec![ChatMessage::text(Role::User, "pesquise")], 2), &on).await.expect_err("400");
    assert!(matches!(err, LlmError::Http { status: 400, .. }), "{err:?}");
    assert_eq!(cap.lock().expect("lock").len(), 2, "um relaxamento e o erro");
}

/// A busca aceita não é recusa; sem a busca pedida, também não.
#[tokio::test]
async fn accepted_or_unrequested_search_is_not_a_rejection() {
    let (base, _cap) = raw_server(vec![(200, SSE, web_reply_stream()), (200, SSE, text_reply_stream())]).await;
    let c = client(&base);
    let (_seen, on) = collector();
    assert!(!c.chat_stream(&web_req(vec![ChatMessage::text(Role::User, "a")], 1), &on).await.expect("busca").web_search_rejected);
    assert!(!c.chat_stream(&req(vec![ChatMessage::text(Role::User, "b")], None), &on).await.expect("sem").web_search_rejected);
}

#[tokio::test]
async fn http_error_does_not_carry_the_key() {
    let key = fake_key();
    let echo = leak(format!(r#"{{"error":{{"message":"API key {key} not valid","code":"invalid_api_key"}}}}"#));
    let (base, _cap) = spawn_server(vec![(403, echo)]).await;
    let err = client(&base).chat(&req(vec![ChatMessage::text(Role::User, "oi")], None)).await.expect_err("403");
    assert!(!format!("{err:?} {err}").contains(&key));
    assert_eq!(provider_failure(&err).to_string(), "provider_http_403");
}

#[test]
fn from_connection_builds_the_gemini_client_on_the_endpoint() {
    std::env::set_var("NANTS_T132_T06_KEY", fake_key());
    let conn = Connection::native("g", "G", ConnectionKind::GoogleGemini, "env:NANTS_T132_T06_KEY");
    let c = LlmClient::from_connection(&conn).expect("cliente");
    assert_eq!(c.base_url, crate::config::GOOGLE_GEMINI_BASE_URL);
    assert_eq!(c.kind, ConnectionKind::GoogleGemini);
    assert!(c.is_paced());
    let mut conn = conn;
    conn.base_url_override = Some("https://proxy.example.com/gemini".into());
    assert_eq!(LlmClient::from_connection(&conn).expect("cliente").base_url, "https://proxy.example.com/gemini");
}

/// PNG 8x8 vermelho (a imagem pequena do teste ao vivo).
const RED_PNG: [u8; 75] = [
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 8, 0, 0, 0, 8, 8, 2, 0, 0, 0, 75, 109, 41,
    220, 0, 0, 0, 18, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 128, 21, 97, 23, 29, 180, 18, 0, 40, 255, 63, 193,
    110, 236, 223, 97, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// Teste ao vivo, ignorado por padrão (spec 132 T06, T07 e T11): o Flash mais recente com
/// raciocínio pela Models API, `thinking_level: low` e `max_output_tokens` pequeno. Ping (com os
/// limites do ritmo, T09); uma ferramenta com a segunda volta reenviando os `thought` assinados, sem
/// estado; o pedido sem ferramentas sobre o histórico (como o resumo); o turno compactado com o
/// `thought` de contorno; JSON; uma imagem pequena; e (T07) a ferramenta pelo stream, com os deltas
/// de raciocínio, o mesmo resultado da versão sem stream e a segunda volta com o resultado de
/// ferramenta vazio. A chave, o modelo, o preço e o teto de gasto vêm de [`live_tests`]; o uso e o
/// custo de cada pedido saem no relatório.
///
/// Só roda com `--ignored` e a chave em `NANTS_LIVE_GEMINI_KEY_FILE`, por `scripts/test.sh`.
#[tokio::test]
#[ignore = "chama a API real do Gemini com a chave do usuário"]
async fn gemini_live_text_and_tool() {
    let live = Live::open(Provider::Gemini).await;
    let (c, model, base) = (&live.client, live.model.clone(), live.base.clone());
    // Teto pequeno de saída (o raciocínio conta nele).
    gemini::remember_max_tokens(&base, &model, 1_024);
    let show = |label: &str, r: &ChatResponse| {
        live.record(label, r);
        eprintln!(
            "  {label}: finish={:?} fallback={} passos={:?}",
            r.finish_reason,
            r.fallback_used,
            r.message
                .provider_items
                .iter()
                .flatten()
                .map(|i| match i["signature"].as_str() {
                    // Tamanho da assinatura (o conteúdo não é impresso).
                    Some(s) => format!(
                        "{}(assinatura {} chars, resumo {} partes)",
                        i["type"].as_str().unwrap_or_default(),
                        s.len(),
                        i["summary"].as_array().map_or(0, Vec::len)
                    ),
                    None => i["type"].as_str().unwrap_or_default().to_string(),
                })
                .collect::<Vec<_>>()
        );
    };
    let base_req = |messages: Vec<ChatMessage>, tools: Vec<ToolSpec>| ChatRequest {
        model: model.clone(),
        messages,
        tools,
        effort: Some(Effort::Low),
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };

    // Texto.
    let text = c.chat(&base_req(vec![ChatMessage::text(Role::User, "Say OK.")], Vec::new())).await.expect("ping");
    show("ping", &text);
    assert!(!text.message.content.is_empty());
    assert!(!text.fallback_used, "thinking_level low aceito");
    // T09: o Gemini não manda cabeçalhos de limite (conferido ao vivo); valem o `Retry-After` e o
    // `retryDelay` do 429. Só o registro.
    eprintln!("  limites no ritmo: {:?}", live.pacer.ceilings(live.conn, &model));

    // Ferramenta e a segunda volta com os `thought` assinados reenviados, sem estado.
    let weather = ToolSpec {
        name: "get_weather".into(),
        description: "Returns the current weather for a city.".into(),
        parameters: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
    };
    let mut messages = vec![
        ChatMessage::text(Role::System, "Use the get_weather tool to answer weather questions."),
        ChatMessage::text(
            Role::User,
            "First work out privately which is larger, 17*23 or 19*21. If the first is larger, ask the weather of Paris; otherwise of Rome. Use the tool.",
        ),
    ];
    let first = c.chat(&base_req(messages.clone(), vec![weather.clone()])).await.expect("ferramenta");
    show("ferramenta", &first);
    let call = first.message.tool_calls.first().cloned().expect("chamada da ferramenta");
    assert_eq!(call.name, "get_weather");
    assert_eq!(first.finish_reason.as_deref(), Some("tool_calls"));
    let signed = first.message.provider_items.iter().flatten().filter(|s| s["type"] == "thought" && s["signature"].is_string()).count();
    assert!(signed > 0, "thought assinado na resposta");
    messages.push(first.message.clone());
    messages.push(tool_result(&call.id, "Sunny, 21 C."));
    let second = c.chat(&base_req(messages.clone(), vec![weather.clone()])).await.expect("segunda volta");
    show("segunda volta", &second);
    assert!(!second.message.content.is_empty());

    // Pedido sem ferramentas sobre o mesmo histórico (como o resumo).
    let mut summary_msgs = messages.clone();
    summary_msgs.push(second.message.clone());
    summary_msgs.push(ChatMessage::text(Role::User, "Summarize this conversation in one sentence."));
    let summary = c.chat(&base_req(summary_msgs, Vec::new())).await.expect("pedido sem ferramentas");
    show("sem ferramentas", &summary);
    assert!(!summary.message.content.is_empty());
    assert!(summary.message.tool_calls.is_empty());

    // Turno compactado: os `thought` da chamada caem e o reenvio leva o de contorno.
    let mut compacted = messages.clone();
    let at = compacted.len() - 2;
    rewrite_call_arguments(&mut compacted[at], &call.id, r#"{"city":"[omitted]"}"#);
    compacted[at].tool_calls[0].arguments = r#"{"city":"[omitted]"}"#.into();
    assert!(compacted[at].provider_items.iter().flatten().all(|s| s["type"] != "thought"));
    let after = c.chat(&base_req(compacted, vec![weather])).await.expect("turno compactado");
    show("compactado", &after);
    assert!(!after.message.content.is_empty());

    // JSON.
    let mut json_req = base_req(vec![ChatMessage::text(Role::User, "Give an object with keys a=1 and b='x'.")], Vec::new());
    json_req.response_json = true;
    let reply = c.chat(&json_req).await.expect("json");
    show("json", &reply);
    let parsed: Value = serde_json::from_str(&reply.message.content).expect("resposta em JSON");
    assert_eq!(parsed["a"], 1);

    // Imagem pequena.
    let image = ChatMessage::with_images("What color is this image? One word.", vec![ImageInput::new("image/png", RED_PNG.to_vec())]);
    let seen = c.chat(&base_req(vec![image], Vec::new())).await.expect("imagem");
    show("imagem", &seen);
    assert!(seen.message.content.to_ascii_lowercase().contains("red"), "{}", seen.message.content);

    // T07: a mesma ferramenta pelo stream, com os deltas de raciocínio, e a segunda volta (também
    // em stream) com os `thought` reenviados e o resultado de ferramenta vazio.
    let reasoning = std::sync::atomic::AtomicUsize::new(0);
    let texts = std::sync::atomic::AtomicUsize::new(0);
    let on = |d: StreamDelta| match d {
        StreamDelta::Reasoning(_) => {
            reasoning.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        StreamDelta::Text(_) => {
            texts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        _ => {}
    };
    let mut streamed_msgs = messages[..2].to_vec();
    let weather_tool = base_req(Vec::new(), Vec::new());
    let tools = vec![ToolSpec {
        name: "get_weather".into(),
        description: "Returns the current weather for a city.".into(),
        parameters: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
    }];
    let sfirst = c
        .chat_stream(&ChatRequest { messages: streamed_msgs.clone(), tools: tools.clone(), ..weather_tool.clone() }, &on)
        .await
        .expect("ferramenta em stream");
    show("stream ferramenta", &sfirst);
    // O resumo é `auto`: visto ao vivo em 2026-10-28, o stream às vezes o omite mesmo com raciocínio.
    // Quando vem, chega em deltas e fica no `thought`.
    let reasoning_deltas = reasoning.load(std::sync::atomic::Ordering::SeqCst);
    let summarized = sfirst.message.provider_items.iter().flatten().any(|s| s["summary"].as_array().is_some_and(|p| !p.is_empty()));
    eprintln!("deltas de raciocínio: {reasoning_deltas}; resumo no thought: {summarized}");
    assert_eq!(reasoning_deltas > 0, summarized, "os deltas são o resumo guardado");
    // O mesmo resultado da versão sem stream: a chamada, o motivo e um `thought` assinado.
    let scall = sfirst.message.tool_calls.first().cloned().expect("chamada em stream");
    assert_eq!((scall.name.as_str(), sfirst.finish_reason.as_deref()), (call.name.as_str(), first.finish_reason.as_deref()));
    assert_eq!(scall.arguments, call.arguments, "os mesmos argumentos");
    let ssigned = sfirst.message.provider_items.iter().flatten().filter(|s| s["type"] == "thought" && s["signature"].is_string()).count();
    assert!(ssigned > 0, "thought assinado no stream");
    streamed_msgs.push(sfirst.message.clone());
    streamed_msgs.push(tool_result(&scall.id, ""));
    let ssecond = c
        .chat_stream(&ChatRequest { messages: streamed_msgs, tools, ..weather_tool }, &on)
        .await
        .expect("segunda volta em stream com o resultado vazio");
    show("stream segunda volta (resultado vazio)", &ssecond);
    assert!(!ssecond.message.content.is_empty() || !ssecond.message.tool_calls.is_empty());
    eprintln!("deltas de texto: {}", texts.load(std::sync::atomic::Ordering::SeqCst));

    // Raciocínio mais longo pelo stream (esforço médio, teto maior): o resumo chega em deltas.
    gemini::remember_max_tokens(&base, &model, 4_096);
    reasoning.store(0, std::sync::atomic::Ordering::SeqCst);
    let think = ChatRequest {
        effort: Some(Effort::Medium),
        ..base_req(
            vec![ChatMessage::text(Role::User, "Think step by step: is 2^31-1 prime? Then answer only yes or no.")],
            Vec::new(),
        )
    };
    let thought = c.chat_stream(&think, &on).await.expect("raciocínio em stream");
    show("stream raciocínio", &thought);
    let reasoning_deltas = reasoning.load(std::sync::atomic::Ordering::SeqCst);
    eprintln!("deltas de raciocínio: {reasoning_deltas}");
    assert!(reasoning_deltas > 0, "resumo do raciocínio em deltas");

    live.report();
}

/// Teste ao vivo do cache implícito (T11), ignorado por padrão: o mesmo prefixo longo no
/// `system_instruction` (o mesmo entre execuções), com perguntas diferentes, em dois pedidos com uma
/// pausa entre eles. O Gemini guarda o prefixo sozinho (sem `cache_control`) e informa a leitura no
/// `total_cached_tokens`, que o custo cobra pelo preço do cache.
///
/// O cache implícito é o melhor esforço do Google, sem garantia (a documentação pede ao menos 4.096
/// tokens no Gemini 3.8 Flash e pedidos com o mesmo prefixo em pouco tempo). Visto ao vivo em
/// 2026-10-28: dentro de uma execução, nenhum prefixo novo foi lido (4.116 tokens sem pausa, com 8 s
/// e com 20 s e 60 s entre os pedidos; 8.916 tokens com 10 s); o prefixo estável, numa execução
/// seguinte, leu 2.056 dos 4.094 tokens já no primeiro pedido, e de novo uns 20 minutos depois. Por
/// isso o prefixo é o mesmo entre execuções, e a falta de leitura sai como aviso no relatório, não
/// como falha; `NANTS_LIVE_GEMINI_REQUIRE_CACHE=1` a torna falha (útil numa segunda execução).
///
/// Só roda com `--ignored` e a chave em `NANTS_LIVE_GEMINI_KEY_FILE`, por `scripts/test.sh`.
#[tokio::test]
#[ignore = "chama a API real do Gemini com a chave do usuário"]
async fn gemini_live_cache() {
    let live = Live::open(Provider::Gemini).await;
    gemini::remember_max_tokens(&live.base, &live.model, 256);
    let prefix = live_tests::long_prefix(110, false);
    let mut read = 0;
    for (i, question) in ["Say OK.", "Say YES."].into_iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
        let req = ChatRequest {
            model: live.model.clone(),
            messages: vec![ChatMessage::text(Role::System, &prefix), ChatMessage::text(Role::User, question)],
            tools: Vec::new(),
            effort: Some(Effort::Low),
            response_json: false,
            web_search: false,
            cache_key: None,
            web_search_max_uses: None,
        };
        let reply = live.client.chat(&req).await.expect("pedido com o prefixo");
        live.record(&format!("cache {}", i + 1), &reply);
        read = reply.usage.and_then(|u| u.cached).unwrap_or(0);
        if read > 0 {
            break;
        }
    }
    if read == 0 {
        eprintln!("  AVISO: o cache implícito não leu o prefixo nesta execução (melhor esforço do Google)");
        assert!(std::env::var_os("NANTS_LIVE_GEMINI_REQUIRE_CACHE").is_none(), "o cache implícito lê o prefixo repetido");
    } else {
        eprintln!("  cache implícito: {read} tokens lidos");
    }
    live.report();
}

/// Teste ao vivo da busca (T07), ignorado por padrão: o mesmo Flash em `thinking_level: low`, uma
/// pergunta que exige a busca, pelo stream; confere a contagem pelas `queries`, as citações e uma
/// segunda volta, sem stream e sem a busca, que reenvia o histórico com os passos da busca. A
/// pergunta pede uma só consulta (a Interactions não tem `max_uses`; o limite de buscas do
/// [`live_tests`] confere o que veio).
///
/// Só roda com `--ignored` e a chave em `NANTS_LIVE_GEMINI_KEY_FILE`, por `scripts/test.sh`.
#[tokio::test]
#[ignore = "chama a API real do Gemini com a chave do usuário e faz uma busca"]
async fn gemini_live_web_search() {
    let live = Live::open(Provider::Gemini).await;
    let (c, model) = (&live.client, live.model.clone());
    gemini::remember_max_tokens(&live.base, &model, 2_048);
    let show = |label: &str, r: &ChatResponse| {
        live.record(label, r);
        eprintln!(
            "  {label}: finish={:?} recusada={} fontes={:?} passos={:?}",
            r.finish_reason,
            r.web_search_rejected,
            r.citations.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            r.message.provider_items.iter().flatten().map(|i| i["type"].as_str().unwrap_or_default().to_string()).collect::<Vec<_>>()
        );
    };
    let mut messages = vec![ChatMessage::text(
        Role::User,
        "Use exactly one Google search query: what is the latest stable release version of the Rust programming language? Answer in one sentence.",
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
    let on = |_: StreamDelta| {};
    let first = c.chat_stream(&first_req, &on).await.expect("busca em stream");
    show("busca", &first);
    let items = first.message.provider_items.clone().expect("passos");
    let calls: Vec<&Value> = items.iter().filter(|s| s["type"] == "google_search_call").collect();
    let queries: usize = calls.iter().map(|s| s["arguments"]["queries"].as_array().map_or(1, Vec::len)).sum();
    eprintln!("chamadas de busca: {}; consultas: {queries}", calls.len());
    assert!(!first.web_search_rejected);
    assert!(first.web_search_calls >= 1, "ao menos uma consulta");
    assert_eq!(first.web_search_calls as usize, queries);
    assert!(calls.iter().all(|s| s["signature"].as_str().is_some_and(|x| !x.is_empty())), "assinatura da chamada");
    assert!(items.iter().any(|s| s["type"] == "google_search_result" && s["signature"].as_str().is_some_and(|x| !x.is_empty())));
    assert!(!first.citations.is_empty(), "citações url_citation");
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
