//! O esforço maior do `deep_think` do Nanty (spec 140 T13, C-25) chega ao corpo de cada adaptador
//! que o Nanty usa, no campo de cada um, com o mesmo modelo: Chat Completions, Responses, plano
//! ChatGPT, Anthropic e Gemini. Só monta o corpo: nenhuma chamada de rede.

use serde_json::Value;

use super::{anthropic, build_body, build_responses_body, gemini, plan, ChatMessage, ChatRequest, Role};
use crate::config::Effort;
use crate::server::nanty::think::raise;

fn request(effort: Effort) -> ChatRequest {
    ChatRequest {
        model: "m-nanty".into(),
        messages: vec![ChatMessage::text(Role::System, "sistema"), ChatMessage::text(Role::User, "pensa nisso")],
        tools: Vec::new(),
        effort: Some(effort),
        response_json: false,
        web_search: false,
        web_search_max_uses: None,
        cache_key: Some("chave".into()),
    }
}

/// O nível no corpo de cada adaptador, e o modelo.
fn levels(req: &ChatRequest) -> Vec<(&'static str, Value, Value)> {
    let chat = build_body(req, req.effort);
    let responses = build_responses_body(req, req.effort).expect("responses");
    let mut plan_body = responses.clone();
    plan::plan_body(&mut plan_body);
    let claude = anthropic::build_body(req, &anthropic::Opts::new(req, 4_096, false)).expect("anthropic");
    let gem = gemini::build_body(req, &gemini::Opts::new(req, None)).expect("gemini");
    vec![
        ("chat", chat["reasoning_effort"].clone(), chat["model"].clone()),
        ("responses", responses["reasoning"]["effort"].clone(), responses["model"].clone()),
        ("plan", plan_body["reasoning"]["effort"].clone(), plan_body["model"].clone()),
        ("anthropic", claude["output_config"]["effort"].clone(), claude["model"].clone()),
        ("gemini", gem["generation_config"]["thinking_level"].clone(), gem["model"].clone()),
    ]
}

#[test]
fn the_raised_effort_of_deep_think_reaches_every_adapter_of_the_nanty() {
    for (configured, expected) in [(Effort::Low, "medium"), (Effort::Medium, "high")] {
        let raised = raise(configured).expect("um nível acima");
        for (adapter, level, model) in levels(&request(raised)) {
            assert_eq!(level, Value::from(expected), "{adapter}: {configured:?}");
            assert_eq!(model, Value::from("m-nanty"), "{adapter}: o mesmo modelo");
        }
        // O nível normal, para comparar: o pedido maior difere só no nível.
        for (adapter, level, _) in levels(&request(configured)) {
            assert_ne!(level, Value::from(expected), "{adapter}");
        }
    }
    // No máximo, nada sobe: a ferramenta recusa antes de qualquer corpo.
    assert_eq!(raise(Effort::High), None);
}
