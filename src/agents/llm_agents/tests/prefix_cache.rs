//! Spec 133 T09 (C-13): estabilidade do prefixo entre as chamadas de um laço, em todos os tipos de
//! conexão. O Desenvolvedor roda duas voltas com ferramentas e uma final contra o servidor falso
//! (`raw_server`); cada pedido n+1 tem de começar com o pedido n: as instruções (ou o `system`), as
//! ferramentas, a chave de cache e o prefixo do `input` (ou de `messages`). A comparação é por hash de
//! cada item, e a falha mostra só o índice e o tipo do primeiro item que diverge, nunca o conteúdo.

use super::*;
use crate::agents::llm_client::stream_tests::{ev, raw_server};
use crate::agents::llm_client::{PlanFuture, PlanTokenError, PlanTokens};
use crate::config::{ConnectionKind, WireApi};

const SSE: &str = "text/event-stream";

/// Os cinco tipos de pedido que o Nants monta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    ApiChat,
    ApiResponses,
    Plan,
    Anthropic,
    Gemini,
}

const WIRES: [Wire; 5] = [Wire::ApiChat, Wire::ApiResponses, Wire::Plan, Wire::Anthropic, Wire::Gemini];

/// Token falso do plano, montado em partes.
struct FakePlan;

impl PlanTokens for FakePlan {
    fn access_token(&self) -> PlanFuture<'_, Result<String, PlanTokenError>> {
        Box::pin(async { Ok(format!("{}-{}", concat!("ac", "cess"), "prefixo")) })
    }
    fn expire(&self) -> PlanFuture<'_, ()> {
        Box::pin(async {})
    }
    fn needs_login(&self) -> PlanFuture<'_, ()> {
        Box::pin(async {})
    }
}

fn fake_key() -> String {
    ["chave", "prefixo", "falsa"].join("-")
}

fn client(wire: Wire, base: &str) -> LlmClient {
    let root = base.trim_end_matches("/v1").to_string();
    let client = match wire {
        Wire::ApiChat => LlmClient::new(base, fake_key()).map(|c| c.with_wire_api(WireApi::Chat)),
        Wire::ApiResponses => LlmClient::new(base, fake_key()).map(|c| c.with_wire_api(WireApi::Responses)),
        Wire::Plan => LlmClient::new(base, String::new()).map(|c| c.with_plan_tokens(Arc::new(FakePlan))),
        Wire::Anthropic => LlmClient::new(root, fake_key()).map(|c| c.with_kind(ConnectionKind::Anthropic)),
        Wire::Gemini => LlmClient::new(root, fake_key()).map(|c| c.with_kind(ConnectionKind::GoogleGemini)),
    };
    client.expect("cliente").without_pacing()
}

fn data(v: Value) -> String {
    format!("data: {v}\n\n")
}

/// Evento SSE com o nome tirado do campo `key` do JSON.
fn named(key: &str, v: Value) -> String {
    let name = v[key].as_str().unwrap_or_default().to_string();
    ev(&name, &v.to_string())
}

/// Resposta da volta `turn`: as duas primeiras pedem `read_file` de `k<turn>.txt`; a terceira encerra
/// com texto. Cada tipo na forma do seu stream, com raciocínio quando o provedor o devolve.
fn reply(wire: Wire, turn: usize) -> String {
    let tool = turn < 2;
    let args = format!(r#"{{"path":"k{turn}.txt"}}"#);
    match wire {
        Wire::ApiChat => {
            let mut s = data(json!({"choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"pensando"}}]}));
            if tool {
                s += &data(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":format!("call_{turn}"),"type":"function","function":{"name":"read_file","arguments":args}}]}}]}));
                s += &data(json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}));
            } else {
                s += &data(json!({"choices":[{"index":0,"delta":{"content":"pronto"}}]}));
                s += &data(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
            }
            s += &data(json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3}}));
            s + "data: [DONE]\n\n"
        }
        Wire::ApiResponses | Wire::Plan => {
            let mut items = vec![json!({"id":format!("rs_{turn}"),"type":"reasoning","summary":[]})];
            if tool {
                items.push(json!({"id":format!("fc_{turn}"),"type":"function_call","status":"completed","arguments":args,"call_id":format!("call_{turn}"),"name":"read_file"}));
            } else {
                items.push(json!({"id":"msg_f","type":"message","status":"completed","role":"assistant","phase":"final_answer","content":[{"type":"output_text","annotations":[],"logprobs":[],"text":"pronto"}]}));
                let mut s = ev("response.output_text.delta", &json!({"type":"response.output_text.delta","delta":"pronto"}).to_string());
                s += &responses_tail(&items);
                return s;
            }
            responses_tail(&items)
        }
        Wire::Anthropic => {
            let mut s = named("type", json!({"type":"message_start","message":{"id":format!("msg_{turn}"),"type":"message","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":1}}}));
            s += &named("type", json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}));
            s += &named("type", json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"pensando"}}));
            s += &named("type", json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":format!("ASSINATURA-{turn}")}}));
            s += &named("type", json!({"type":"content_block_stop","index":0}));
            let stop = if tool {
                s += &named("type", json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":format!("toolu_{turn}"),"name":"read_file","input":{}}}));
                s += &named("type", json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":args}}));
                "tool_use"
            } else {
                s += &named("type", json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}));
                s += &named("type", json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"pronto"}}));
                "end_turn"
            };
            s += &named("type", json!({"type":"content_block_stop","index":1}));
            s += &named("type", json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}));
            s + &named("type", json!({"type":"message_stop"}))
        }
        Wire::Gemini => {
            let mut s = named("event_type", json!({"event_type":"interaction.created","interaction":{"object":"interaction","model":"gemini-teste"}}));
            s += &named("event_type", json!({"event_type":"step.start","index":0,"step":{"type":"thought"}}));
            s += &named("event_type", json!({"event_type":"step.delta","index":0,"delta":{"type":"thought_signature","signature":format!("ASSINATURA-{turn}")}}));
            s += &named("event_type", json!({"event_type":"step.stop","index":0}));
            let status = if tool {
                s += &named("event_type", json!({"event_type":"step.start","index":1,"step":{"id":format!("call_{turn}"),"type":"function_call","name":"read_file","arguments":{}}}));
                s += &named("event_type", json!({"event_type":"step.delta","index":1,"delta":{"type":"arguments_delta","arguments":args}}));
                "requires_action"
            } else {
                s += &named("event_type", json!({"event_type":"step.start","index":1,"step":{"type":"model_output"}}));
                s += &named("event_type", json!({"event_type":"step.delta","index":1,"delta":{"type":"text","text":"pronto"}}));
                "completed"
            };
            s += &named("event_type", json!({"event_type":"step.stop","index":1}));
            let usage = json!({"total_tokens":15,"total_input_tokens":10,"total_cached_tokens":0,"total_output_tokens":5,"total_tool_use_tokens":0,"total_thought_tokens":0});
            s += &named("event_type", json!({"event_type":"interaction.completed","interaction":{"status":status,"usage":usage,"object":"interaction"}}));
            s + "event: done\r\ndata: [DONE]\r\n\r\n"
        }
    }
}

/// Itens completos (`output_item.done`) e o `response.completed` com `output: []`, na forma medida na
/// API do plano (spec 119), que a Responses por chave também aceita.
fn responses_tail(items: &[Value]) -> String {
    let mut s = String::new();
    for (i, item) in items.iter().enumerate() {
        s += &ev("response.output_item.done", &json!({"type":"response.output_item.done","output_index":i,"item":item}).to_string());
    }
    let usage = json!({"input_tokens":10,"input_tokens_details":{"cached_tokens":0},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":15});
    s + &ev(
        "response.completed",
        &json!({"type":"response.completed","response":{"id":"resp_p","object":"response","status":"completed","error":null,"incomplete_details":null,"output":[],"usage":usage}}).to_string(),
    )
}

/// Tira os marcadores que não fazem parte do texto cacheado (o `cache_control` da Anthropic, que
/// anda com a ponta do histórico de propósito).
fn strip_markers(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter().filter(|(k, _)| k.as_str() != "cache_control").map(|(k, v)| (k.clone(), strip_markers(v))).collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_markers).collect()),
        other => other.clone(),
    }
}

fn hash_of(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    strip_markers(v).to_string().hash(&mut h);
    h.finish()
}

/// Rótulo de um item sem conteúdo: o `type` ou o `role`.
fn kind_of(v: &Value) -> String {
    v.get("type").or_else(|| v.get("role")).and_then(Value::as_str).unwrap_or("?").to_string()
}

/// Um item do pedido: o lugar (`tools[1]`, `input[3]`), o tipo e o hash.
#[derive(Debug, Clone)]
struct Item {
    place: String,
    kind: String,
    hash: u64,
}

/// Itens do pedido na ordem em que o provedor os lê: as partes fixas (instruções, ferramentas e a
/// chave de cache) e depois o histórico.
fn prefix_items(body: &Value) -> Vec<Item> {
    let mut out = Vec::new();
    let mut push = |place: String, v: &Value| out.push(Item { place, kind: kind_of(v), hash: hash_of(v) });
    for field in ["instructions", "system_instruction", "prompt_cache_key"] {
        if let Some(v) = body.get(field) {
            push(field.to_string(), v);
        }
    }
    for list in ["system", "tools", "input", "messages"] {
        for (i, v) in body[list].as_array().into_iter().flatten().enumerate() {
            push(format!("{list}[{i}]"), v);
        }
    }
    out
}

/// O pedido `next` começa com `prev`? `None` quando sim; senão, o lugar e o tipo do primeiro item
/// que diverge (ou que falta).
fn first_divergence(prev: &Value, next: &Value) -> Option<String> {
    let next = prefix_items(next);
    for (i, a) in prefix_items(prev).iter().enumerate() {
        match next.get(i) {
            Some(b) if b.place == a.place && b.hash == a.hash => {}
            Some(b) => return Some(format!("{} ({}) virou {} ({})", a.place, a.kind, b.place, b.kind)),
            None => return Some(format!("{} ({}) sumiu", a.place, a.kind)),
        }
    }
    None
}

/// Corpos JSON dos pedidos capturados.
fn bodies(cap: &Arc<Mutex<Vec<String>>>) -> Vec<Value> {
    cap.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|raw| raw.split_once("\r\n\r\n").and_then(|(_, b)| serde_json::from_str(b).ok()).unwrap_or(Value::Null))
        .collect()
}

/// Roda o Desenvolvedor contra o servidor falso do tipo `wire` e devolve os corpos dos pedidos.
async fn developer_requests(wire: Wire) -> Vec<Value> {
    let root = tmp_root(&format!("prefix-{wire:?}"));
    for i in 0..2 {
        std::fs::write(root.join(format!("k{i}.txt")), format!("conteúdo {i}")).expect("arquivo");
    }
    let ctype = if wire == Wire::Plan { "" } else { SSE };
    let (base, cap) = raw_server((0..3).map(|turn| (200, ctype, reply(wire, turn))).collect()).await;
    let dev = LlmDeveloper::new(
        Arc::new(client(wire, &base)),
        "modelo-teste",
        Some(Effort::Low),
        Policy::new(Vec::new(), Vec::new()),
        Arc::new(Deny),
    )
    .without_final_check()
    .with_usage(Arc::new(crate::agents::usage::testing::MemSink::default()));
    dev.implement(&brief(&root)).await.unwrap_or_else(|e| panic!("{wire:?}: {e}"));
    let _ = std::fs::remove_dir_all(&root);
    bodies(&cap)
}

/// O pedido n+1 começa com o pedido n, em todos os tipos de conexão (instruções, ferramentas, chave
/// de cache e histórico).
#[tokio::test]
async fn each_request_starts_with_the_previous_one_in_every_wire() {
    let mut broken = Vec::new();
    for wire in WIRES {
        let reqs = developer_requests(wire).await;
        assert_eq!(reqs.len(), 3, "{wire:?}: duas voltas com ferramenta e a final");
        assert!(reqs.iter().all(Value::is_object), "{wire:?}: corpo JSON");
        let places: Vec<String> = prefix_items(&reqs[0]).into_iter().map(|i| i.place).collect();
        assert!(places.iter().any(|p| p == "tools[0]"), "{wire:?}: {places:?}");
        if matches!(wire, Wire::ApiResponses | Wire::Plan) {
            assert!(places.iter().any(|p| p == "prompt_cache_key"), "{wire:?}: {places:?}");
        }
        for (n, pair) in reqs.windows(2).enumerate() {
            if let Some(at) = first_divergence(&pair[0], &pair[1]) {
                broken.push(format!("{wire:?} pedido {} -> {}: {at}", n + 1, n + 2));
            }
            assert!(prefix_items(&pair[1]).len() > prefix_items(&pair[0]).len(), "{wire:?}: o histórico cresce");
        }
    }
    assert!(broken.is_empty(), "prefixo instável: {broken:#?}");
}

/// A comparação acha o primeiro item que diverge e diz só o lugar e o tipo.
#[test]
fn divergence_names_only_the_place_and_the_kind() {
    let a = json!({"instructions": "regras", "tools": [{"type": "function", "name": "f"}], "input": [{"role": "user", "content": "segredo"}]});
    let mut b = a.clone();
    b["input"].as_array_mut().expect("lista").push(json!({"type": "function_call_output", "output": "x"}));
    assert_eq!(first_divergence(&a, &b), None);
    let mut c = b.clone();
    c["input"][0]["content"] = json!("outro segredo");
    let found = first_divergence(&a, &c).expect("diverge");
    assert_eq!(found, "input[0] (user) virou input[0] (user)");
    assert!(!found.contains("segredo"));
    let mut d = b.clone();
    d["tools"] = json!([{"type": "web_search"}]);
    assert_eq!(first_divergence(&a, &d).as_deref(), Some("tools[0] (function) virou tools[0] (web_search)"));
    // O `cache_control` anda com a ponta do histórico: não conta.
    let mut e = b.clone();
    e["input"][0]["cache_control"] = json!({"type": "ephemeral"});
    assert_eq!(first_divergence(&e, &b), None);
}

/// Resposta do Arquiteto pelo plano: na volta 0, uma busca nativa e `list_dir`; na 1, só `list_dir`;
/// na 2, o texto final.
fn architect_reply(turn: usize) -> String {
    let call = json!({"id":format!("fc_{turn}"),"type":"function_call","status":"completed","arguments":r#"{"path":"."}"#,"call_id":format!("call_{turn}"),"name":"list_dir"});
    let items = match turn {
        0 => vec![json!({"id":"ws_0","type":"web_search_call","status":"completed","action":{"type":"search","query":"x"}}), call],
        1 => vec![call],
        _ => vec![json!({"id":"msg_f","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","annotations":[],"text":"pronto"}]})],
    };
    responses_tail(&items)
}

/// A quebra que o Nants causa hoje, a mesma do registro do Test 10 (o Arquiteto do plano em 0 de
/// cache a partir do passo em que a 5ª busca da fase foi feita): atingido o limite de buscas da
/// internet (spec 076), a ferramenta hospedada `web_search` sai do pedido seguinte, e o prefixo muda
/// nas ferramentas, antes de todo o histórico. Fora esse passo, o pedido seguinte começa com o
/// anterior.
#[tokio::test]
async fn architect_search_limit_changes_the_tools_once() {
    use crate::agents::architect::{InternetSetup, LlmArchitect};
    let root = tmp_root("prefix-arch-net");
    let (base, cap) = raw_server((0..3).map(|turn| (200, "", architect_reply(turn))).collect()).await;
    let net = InternetSetup {
        web_search: true,
        plan: (1, 8, 10),
        recovery: (1, 5, 10),
        secrets: Arc::new(SecretSet::new([])),
        on_web_rejected: Arc::new(|| {}),
    };
    let backend = Arc::new(client(Wire::Plan, &base));
    LlmArchitect::new(backend, "modelo-teste", Some(Effort::Low), Policy::new(Vec::new(), Vec::new()), Arc::new(Deny))
        .with_internet(net)
        .reply(&root, &[], "oi", "Projeto: teste")
        .await
        .expect("resposta");
    let _ = std::fs::remove_dir_all(&root);
    let reqs = bodies(&cap);
    assert_eq!(reqs.len(), 3);
    let first = first_divergence(&reqs[0], &reqs[1]).unwrap_or_default();
    assert!(first.starts_with("tools[") && first.contains("(web_search)"), "{first}");
    assert_eq!(first_divergence(&reqs[1], &reqs[2]), None, "depois do limite, estável de novo");
}
