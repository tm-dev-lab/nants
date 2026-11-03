//! O histórico normalizado da conversa do Nanty em cada adaptador (spec 140 T06, C-18), sem rede: o
//! corpo de Chat Completions, de Responses, do plano ChatGPT, da Anthropic e do Gemini. Em todos, os
//! papéis alternam e a lista começa pela pessoa, nada de raciocínio nem de chamada de ferramenta vai
//! junto, e o envelope do texto de terceiro chega no conteúdo da mensagem histórica.

use super::*;
use crate::server::nanty::history::{normalize, Entry, EMPTY_SUMMARY};
use crate::server::nanty::tools::{UNTRUSTED_CLOSE, UNTRUSTED_OPEN};
use serde_json::json;

/// A conversa difícil da C-18: dois avisos seguidos do Nanty abrindo a janela (um deles fixo, de duas
/// linhas), um comando, a pessoa que encaminhou um texto e ficou sem resposta, e a mensagem atual.
fn history() -> Vec<ChatMessage> {
    let forwarded = format!("{UNTRUSTED_OPEN} \"texto de terceiro\" {UNTRUSTED_CLOSE}");
    let person = json!({ "at": "2026-11-03 10:00", "text": "", "forwarded": forwarded }).to_string();
    let entries = vec![
        Entry { fixed: true, ..Entry::new(Role::Assistant, "[2026-11-03 09:00] Aviso fixo.\nSegunda linha do aviso.") },
        Entry::new(Role::Assistant, "[2026-11-03 09:01] O modelo comentou o aviso."),
        Entry { control: true, ..Entry::new(Role::User, "/status") },
        Entry::new(Role::User, person),
    ];
    normalize(Vec::new(), entries, json!({ "message": "e agora?" }).to_string())
}

fn request(messages: Vec<ChatMessage>) -> ChatRequest {
    let system = ChatMessage::text(Role::System, "sistema");
    ChatRequest {
        model: "m".into(),
        messages: std::iter::once(system).chain(messages).collect(),
        tools: Vec::new(),
        effort: None,
        response_json: false,
        web_search: false,
        web_search_max_uses: None,
        cache_key: None,
    }
}

/// Os papéis de uma lista de mensagens do corpo (`role`), na ordem.
fn roles(items: &[Value]) -> Vec<String> {
    items.iter().map(|i| i["role"].as_str().unwrap_or_default().to_string()).collect()
}

fn assert_clean(body: &Value, what: &str) {
    let text = body.to_string();
    assert!(text.contains("Aviso fixo.") && !text.contains("Segunda linha"), "{what}: o aviso fixo vira uma linha: {text}");
    assert!(!text.contains("/status"), "{what}: a mensagem só de controle sai: {text}");
    assert!(text.contains("untrusted_data") && text.contains("texto de terceiro"), "{what}: o envelope fica no conteúdo: {text}");
    for absent in ["reasoning", "encrypted_content", "function_call", "tool_use", "thought", "tool_calls"] {
        assert!(!text.contains(absent), "{what}: {absent} no histórico: {text}");
    }
}

#[test]
fn the_pure_function_alternates_and_opens_with_the_person() {
    let messages = history();
    let roles: Vec<Role> = messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::User, Role::Assistant, Role::User]);
    assert_eq!(messages[0].content, EMPTY_SUMMARY, "a janela que abre pelo Nanty ganha o resumo vazio");
    assert!(messages[1].content.contains("Aviso fixo.\n\n[2026-11-03 09:01]"), "as falas seguidas se juntam: {}", messages[1].content);
    assert!(messages[2].content.ends_with("{\"message\":\"e agora?\"}"), "a mensagem sem resposta se junta à atual");
    assert!(messages.iter().all(|m| m.provider_items.is_none() && m.tool_calls.is_empty() && m.tool_call_id.is_none()));
    // Com dado antes do histórico, ele abre a lista, e nada é inventado.
    let led = normalize(vec!["{\"conversation_summary\":[1]}".into()], vec![Entry::new(Role::Assistant, "[x] a")], "{}".into());
    assert_eq!(led.iter().map(|m| m.role).collect::<Vec<_>>(), [Role::User, Role::Assistant, Role::User]);
    assert_eq!(led[0].content, "{\"conversation_summary\":[1]}");
    // Só a mensagem atual: uma mensagem só.
    assert_eq!(normalize(Vec::new(), Vec::new(), "{}".into()).len(), 1);
}

#[test]
fn chat_completions_and_responses_get_the_alternated_history() {
    let req = request(history());
    let chat = chat_body(&req, None).expect("corpo");
    let messages = chat["messages"].as_array().expect("mensagens");
    assert_eq!(roles(messages), ["system", "user", "assistant", "user"]);
    assert_clean(&chat, "chat completions");

    let responses = build_responses_body(&req, None).expect("corpo");
    let input = responses["input"].as_array().expect("input");
    assert_eq!(roles(input), ["user", "assistant", "user"]);
    assert_clean(&responses, "responses");
}

#[test]
fn the_chatgpt_plan_gets_the_alternated_history() {
    let req = request(history());
    let mut body = build_responses_body(&req, None).expect("corpo");
    plan::plan_body(&mut body);
    let input = body["input"].as_array().expect("input");
    assert_eq!(roles(input), ["user", "assistant", "user"]);
    assert!(input.iter().all(|i| i.get("type").is_none_or(|t| t == "message")), "{input:?}");
    assert_clean(&body, "plano ChatGPT");
}

#[test]
fn anthropic_gets_the_alternated_history() {
    let req = request(history());
    let body = anthropic::build_body(&req, &anthropic::Opts::new(&req, 1024, false)).expect("corpo");
    let messages = body["messages"].as_array().expect("mensagens");
    assert_eq!(roles(messages), ["user", "assistant", "user"]);
    assert!(messages.iter().all(|m| m["content"].as_array().is_some_and(|c| c.iter().all(|b| b["type"] == "text"))));
    assert_clean(&body, "anthropic");
}

#[test]
fn gemini_gets_the_alternated_history() {
    let req = request(history());
    let body = gemini::build_body(&req, &gemini::Opts::new(&req, None)).expect("corpo");
    let steps: Vec<&str> = body["input"].as_array().expect("passos").iter().map(|s| s["type"].as_str().unwrap_or_default()).collect();
    assert_eq!(steps, ["user_input", "model_output", "user_input"]);
    assert_clean(&body, "gemini");
}
