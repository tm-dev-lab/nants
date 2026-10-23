//! Modo plano ChatGPT do cliente Responses (spec 116 T02, decisão 4).
//!
//! Numa conexão `chatgpt_plan`, o mesmo cliente Responses:
//! - autentica com o access token do login (fonte [`PlanTokens`], que renova sozinha);
//! - manda sempre `store:false` e `stream:true`, retira os campos proibidos
//!   ([`PLAN_FORBIDDEN_FIELDS`]) e as ferramentas hospedadas que o plano não aceita
//!   ([`PLAN_UNSUPPORTED_TOOLS`]), e troca qualquer `role:system` em `input` por `developer`
//!   (as mensagens `system` já vão em `instructions`);
//! - só o evento `response.completed` conta como sucesso: `response.incomplete` vira resposta
//!   incompleta, e `response.failed` passa pelo mapa de erros do plano;
//! - mapa de erros ([`classify`]): limite de uso (pausa, sem nova tentativa), conta não elegível
//!   (pausa), uso ou conta indisponível (nova tentativa pelo ritmo da spec 089), capacidade não
//!   suportada (retira o item de `error.param` e tenta uma vez com o corpo diferente) e usuário
//!   inválido (precisa entrar de novo).

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use super::{BadKind, BadReason, LlmError};

/// Campos que a inferência pelo plano recusa (spec 116, "Situação"). O histórico vai inteiro em
/// `input`, sem `previous_response_id`.
pub const PLAN_FORBIDDEN_FIELDS: [&str; 16] = [
    "background",
    "conversation",
    "max_output_tokens",
    "max_tool_calls",
    "metadata",
    "moderation",
    "multi_agent",
    "prompt",
    "prompt_cache_retention",
    "safety_identifier",
    "temperature",
    "top_logprobs",
    "top_p",
    "truncation",
    "user",
    "previous_response_id",
];

/// Tipos de ferramenta hospedada que não funcionam pelo plano: não vão no pedido. As function e
/// custom tools e a `web_search` seguem.
pub const PLAN_UNSUPPORTED_TOOLS: [&str; 8] = [
    "image_generation",
    "file_search",
    "code_interpreter",
    "computer_use",
    "computer_use_preview",
    "computer",
    "mcp",
    "tool_search",
];

/// Página do ChatGPT onde a pessoa confere o uso do plano ("Manage usage").
pub const PLAN_USAGE_URL: &str = "https://chatgpt.com/settings/usage";

/// Falha do plano que pausa a execução (não se repete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    /// Limite de uso do plano atingido (`subscription_sharing_usage_limit_exceeded`).
    UsageLimit,
    /// A conta não é elegível para usar o plano por aqui (`..._user_not_eligible`).
    NotEligible,
    /// Precisa entrar de novo: usuário inválido (401), renovação recusada de vez ou sem login.
    NeedsLogin,
}

impl PlanError {
    /// Nome estável (`plan_limit`, `plan_not_eligible`, `plan_needs_login`).
    pub fn as_str(self) -> &'static str {
        match self {
            PlanError::UsageLimit => "plan_limit",
            PlanError::NotEligible => "plan_not_eligible",
            PlanError::NeedsLogin => "plan_needs_login",
        }
    }
}

/// Por que a fonte de tokens não entregou um access token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanTokenError {
    /// Sem login ou login recusado de vez: precisa entrar de novo.
    NeedsLogin,
    /// Servidor de autenticação indisponível: vale tentar de novo.
    Unavailable,
    /// Falha local (pasta de dados, gravação): não adianta repetir.
    Failed,
}

/// Futuro devolvido pela fonte de tokens.
pub type PlanFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Fonte do access token de UMA conexão pelo plano (o servidor liga ao login da T01).
pub trait PlanTokens: Send + Sync {
    /// Access token utilizável; renova antes, se preciso.
    fn access_token(&self) -> PlanFuture<'_, Result<String, PlanTokenError>>;
    /// O provedor recusou o token atual (401 sem motivo): descarta-o para a próxima chamada renovar.
    fn expire(&self) -> PlanFuture<'_, ()>;
    /// Conta recusada de vez: a conexão passa a "precisa entrar de novo".
    fn needs_login(&self) -> PlanFuture<'_, ()>;
}

/// Como a chamada é cobrada (gravado no `usage.jsonl`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Billing {
    /// Por chave de API: tokens com preço em US$ quando configurado.
    #[default]
    Metered,
    /// Pelo plano ChatGPT: tokens sem US$, fora do total e do orçamento em dólar.
    ChatgptPlan,
}

impl Billing {
    /// Valor do campo `billing` do registro; a cobrança por chave não grava o campo.
    pub fn as_record(self) -> Option<&'static str> {
        match self {
            Billing::Metered => None,
            Billing::ChatgptPlan => Some("chatgpt_plan"),
        }
    }
}

/// Classe de um código de erro do plano.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanClass {
    /// 429 (ou `response.failed`): limite de uso do plano.
    Limit,
    /// 403: conta não elegível.
    NotEligible,
    /// 503: uso ou conta indisponível no momento.
    Unavailable,
    /// 400: capacidade não suportada (o item vem em `error.param`).
    Unsupported,
    /// 401: usuário inválido.
    InvalidUser,
}

/// `code` igual a `suffix` ou terminado em `_<suffix>`.
fn ends_with_code(code: &str, suffix: &str) -> bool {
    code == suffix || code.strip_suffix(suffix).is_some_and(|rest| rest.ends_with('_'))
}

/// Classe do código de erro do plano; `None` para os demais códigos.
pub fn classify(code: &str) -> Option<PlanClass> {
    let code = code.to_ascii_lowercase();
    if ends_with_code(&code, "usage_limit_exceeded") {
        Some(PlanClass::Limit)
    } else if ends_with_code(&code, "user_not_eligible") {
        Some(PlanClass::NotEligible)
    } else if ends_with_code(&code, "usage_unavailable") || ends_with_code(&code, "user_unavailable") {
        Some(PlanClass::Unavailable)
    } else if ends_with_code(&code, "unsupported_capability") {
        Some(PlanClass::Unsupported)
    } else if ends_with_code(&code, "invalid_user") {
        Some(PlanClass::InvalidUser)
    } else {
        None
    }
}

/// Código e `param` crus do erro: `{"error": {"code"|"type", "param"}}`, o próprio objeto `error`
/// ou um evento SSE (`response.failed` com `response.error`). Ficam só no cliente: nunca vão à
/// pessoa nem ao log sem saneamento.
pub fn error_parts(v: &Value) -> (Option<String>, Option<String>) {
    let text = |obj: &Value, key: &str| obj.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let candidates = [v.pointer("/response/error"), v.get("error"), Some(v)];
    for obj in candidates.into_iter().flatten().filter(|o| o.is_object()) {
        let code = text(obj, "code").or_else(|| {
            // O `type` de um evento SSE (`error`, `response.failed`) não é código.
            text(obj, "type").filter(|t| t != "error" && !t.starts_with("response."))
        });
        if code.is_some() {
            return (code, text(obj, "param"));
        }
    }
    (None, None)
}

/// Código e `param` crus do corpo de erro (JSON); corpo ilegível não tem código.
pub fn body_error_parts(body: &str) -> (Option<String>, Option<String>) {
    serde_json::from_str::<Value>(body).map_or((None, None), |v| error_parts(&v))
}

/// Erro do cliente para a classe do plano. `Unsupported` não vira erro aqui (quem envia tenta sem o
/// item); fora do vocabulário, `None`.
pub fn class_error(class: &PlanClass, code: &str) -> Option<LlmError> {
    match class {
        PlanClass::Limit => Some(LlmError::Plan(PlanError::UsageLimit)),
        PlanClass::NotEligible => Some(LlmError::Plan(PlanError::NotEligible)),
        PlanClass::InvalidUser => Some(LlmError::Plan(PlanError::NeedsLogin)),
        // Nova tentativa pelo cronograma de limite (spec 089), com aviso e espera limitada.
        PlanClass::Unavailable => Some(LlmError::Parse(BadReason::with_code(BadKind::Throttled, Some(code)))),
        PlanClass::Unsupported => None,
    }
}

/// Falha do stream no modo plano, antes do tratamento comum: `response.incomplete` não é sucesso
/// (vira resposta incompleta com o motivo) e `response.failed`/`error` com código do plano vira o
/// erro do plano. `None` deixa o evento seguir o caminho comum.
pub fn stream_failure(event: &str, data: &str) -> Option<LlmError> {
    let v: Value = serde_json::from_str(data).ok()?;
    let kind = v.get("type").and_then(Value::as_str).unwrap_or(event);
    match kind {
        "response.incomplete" => {
            let reason = v.pointer("/response/incomplete_details/reason").and_then(Value::as_str);
            Some(LlmError::Parse(BadReason::with_code(BadKind::Incomplete, reason)))
        }
        "response.failed" | "error" => {
            let (code, _) = error_parts(&v);
            let code = code?;
            class_error(&classify(&code)?, &code)
        }
        _ => None,
    }
}

/// Ajusta o corpo do Responses para o plano: retira os campos proibidos, força `store:false` e
/// `stream:true`, tira as ferramentas hospedadas não suportadas e troca `role:system` por
/// `developer` nos itens de `input`.
pub fn plan_body(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    for field in PLAN_FORBIDDEN_FIELDS {
        obj.remove(field);
    }
    obj.insert("store".into(), Value::Bool(false));
    obj.insert("stream".into(), Value::Bool(true));
    if let Some(Value::Array(tools)) = obj.get_mut("tools") {
        tools.retain(|t| {
            let kind = t.get("type").and_then(Value::as_str).unwrap_or_default();
            !PLAN_UNSUPPORTED_TOOLS.contains(&kind)
        });
        if tools.is_empty() {
            obj.remove("tools");
        }
    }
    if let Some(Value::Array(items)) = obj.get_mut("input") {
        for item in items.iter_mut() {
            if item.get("role").and_then(Value::as_str) == Some("system") {
                item["role"] = Value::from("developer");
            }
        }
    }
}

/// Um passo do caminho de `error.param`: campo ou índice.
enum Step {
    Key(String),
    Index(usize),
}

/// Passos de `a.b[2].c` (também `a[2]`, `tools[0].type`). Caminho vazio ou malformado: `None`.
fn parse_param(param: &str) -> Option<Vec<Step>> {
    let mut steps = Vec::new();
    for part in param.split('.') {
        let (name, mut rest) = match part.find('[') {
            Some(at) => (&part[..at], &part[at..]),
            None => (part, ""),
        };
        if !name.is_empty() {
            steps.push(Step::Key(name.to_string()));
        } else if rest.is_empty() {
            return None;
        }
        while let Some(inner) = rest.strip_prefix('[') {
            let close = inner.find(']')?;
            steps.push(Step::Index(inner[..close].parse().ok()?));
            rest = &inner[close + 1..];
        }
        if !rest.is_empty() {
            return None;
        }
    }
    (!steps.is_empty()).then_some(steps)
}

/// Retira do corpo o item apontado por `param` (`tools[2]`, `reasoning.summary`, `include`...).
/// `true` quando algo saiu (o corpo ficou diferente).
pub fn remove_param(body: &mut Value, param: &str) -> bool {
    let Some(steps) = parse_param(param.trim()) else {
        return false;
    };
    let Some((last, path)) = steps.split_last() else {
        return false;
    };
    let mut node = body;
    for step in path {
        let next = match step {
            Step::Key(k) => node.get_mut(k.as_str()),
            Step::Index(i) => node.get_mut(*i),
        };
        let Some(next) = next else {
            return false;
        };
        node = next;
    }
    match (last, node) {
        (Step::Key(k), Value::Object(map)) => map.remove(k.as_str()).is_some(),
        (Step::Index(i), Value::Array(items)) if *i < items.len() => {
            items.remove(*i);
            true
        }
        _ => false,
    }
}

/// Spec 120, **preparado e não ligado**: ajuste do histórico reenviado no modo plano, para o caso
/// de o backend recusar os itens como hoje saem (`store:false` não guarda nada no servidor, então
/// um item reenviado só com `id` pode não ser encontrado). Segue a forma do Codex CLI:
/// - pede `include: ["reasoning.encrypted_content"]`, para o `reasoning` voltar com o conteúdo
///   cifrado e poder ser reenviado inteiro;
/// - tira `id` e `status` dos itens do modelo reenviados em `input` (`message`, `function_call`,
///   `reasoning`);
/// - não reenvia `reasoning` sem `encrypted_content`.
///
/// Só os testes o usam até o teste real (`plan_live_text_and_tool`) mostrar a recusa.
#[cfg_attr(not(test), allow(dead_code))]
pub fn replay_without_ids(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    const ENCRYPTED: &str = "reasoning.encrypted_content";
    let include = obj.entry("include").or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(list) = include {
        if !list.iter().any(|v| v.as_str() == Some(ENCRYPTED)) {
            list.push(Value::from(ENCRYPTED));
        }
    }
    if let Some(Value::Array(items)) = obj.get_mut("input") {
        items.retain(|i| {
            i.get("type").and_then(Value::as_str) != Some("reasoning")
                || i.get("encrypted_content").and_then(Value::as_str).is_some_and(|c| !c.is_empty())
        });
        for item in items.iter_mut() {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
            if matches!(kind, "message" | "function_call" | "reasoning") {
                if let Some(o) = item.as_object_mut() {
                    o.remove("id");
                    o.remove("status");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codes_are_classified_by_suffix() {
        assert_eq!(classify("subscription_sharing_usage_limit_exceeded"), Some(PlanClass::Limit));
        assert_eq!(classify("subscription_sharing_user_not_eligible"), Some(PlanClass::NotEligible));
        assert_eq!(classify("subscription_sharing_usage_unavailable"), Some(PlanClass::Unavailable));
        assert_eq!(classify("subscription_sharing_user_unavailable"), Some(PlanClass::Unavailable));
        assert_eq!(classify("subscription_sharing_unsupported_capability"), Some(PlanClass::Unsupported));
        assert_eq!(classify("subscription_sharing_invalid_user"), Some(PlanClass::InvalidUser));
        assert_eq!(classify("rate_limit_exceeded"), None);
        assert_eq!(classify("xusage_limit_exceeded"), None);
        assert_eq!(classify("server_error"), None);
    }

    #[test]
    fn error_parts_read_body_event_and_nested_error() {
        let body = r#"{"error":{"code":"x_unsupported_capability","param":"tools[1]","message":"m"}}"#;
        assert_eq!(body_error_parts(body), (Some("x_unsupported_capability".into()), Some("tools[1]".into())));
        let failed = json!({"type": "response.failed", "response": {"error": {"code": "a_usage_limit_exceeded"}}});
        assert_eq!(error_parts(&failed).0.as_deref(), Some("a_usage_limit_exceeded"));
        let event = json!({"type": "error", "code": "b_invalid_user"});
        assert_eq!(error_parts(&event).0.as_deref(), Some("b_invalid_user"));
        assert_eq!(error_parts(&json!({"type": "error"})).0, None);
        assert_eq!(body_error_parts("texto"), (None, None));
    }

    #[test]
    fn body_drops_forbidden_fields_and_unsupported_tools() {
        let mut body = json!({
            "model": "m", "input": [{"role": "system", "content": "a"}, {"role": "user", "content": "b"}],
            "store": true, "stream": false, "temperature": 0.2, "max_output_tokens": 10,
            "previous_response_id": "resp_1", "metadata": {}, "user": "u", "truncation": "auto",
            "tools": [{"type": "function", "name": "f"}, {"type": "image_generation"}, {"type": "web_search"}, {"type": "mcp"}],
        });
        plan_body(&mut body);
        for field in PLAN_FORBIDDEN_FIELDS {
            assert!(body.get(field).is_none(), "{field}");
        }
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["input"][0]["role"], "developer");
        assert_eq!(body["input"][1]["role"], "user");
        let kinds: Vec<&str> = body["tools"].as_array().into_iter().flatten().filter_map(|t| t["type"].as_str()).collect();
        assert_eq!(kinds, ["function", "web_search"]);
        let mut only_hosted = json!({"tools": [{"type": "image_generation"}]});
        plan_body(&mut only_hosted);
        assert!(only_hosted.get("tools").is_none());
    }

    #[test]
    fn param_paths_remove_one_item() {
        let mut body = json!({"tools": [{"type": "a"}, {"type": "b"}], "reasoning": {"effort": "low", "summary": "auto"}, "include": ["x"]});
        assert!(remove_param(&mut body, "tools[0]"));
        assert_eq!(body["tools"], json!([{"type": "b"}]));
        assert!(remove_param(&mut body, "reasoning.summary"));
        assert_eq!(body["reasoning"], json!({"effort": "low"}));
        assert!(remove_param(&mut body, "include"));
        assert!(body.get("include").is_none());
        for bad in ["", "tools[9]", "nada", "tools[x]", "a..b", "tools[0", "[0]"] {
            assert!(!remove_param(&mut body, bad), "{bad}");
        }
    }

    #[test]
    fn stream_failures_follow_the_plan_map() {
        let incomplete = r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#;
        assert!(matches!(stream_failure("", incomplete), Some(LlmError::Parse(r)) if r.kind == BadKind::Incomplete));
        let limit = r#"{"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded"}}}"#;
        assert!(matches!(stream_failure("", limit), Some(LlmError::Plan(PlanError::UsageLimit))));
        let busy = r#"{"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_unavailable"}}}"#;
        assert!(matches!(stream_failure("", busy), Some(LlmError::Parse(r)) if r.is_throttled()));
        let other = r#"{"type":"response.failed","response":{"error":{"code":"server_error"}}}"#;
        assert!(stream_failure("", other).is_none());
        assert!(stream_failure("", r#"{"type":"response.completed","response":{}}"#).is_none());
    }

    #[test]
    fn replay_without_ids_strips_ids_and_unreplayable_reasoning() {
        let mut body = json!({
            "include": ["x"],
            "input": [
                {"role": "user", "content": "oi"},
                {"type": "reasoning", "id": "rs_1", "summary": []},
                {"type": "reasoning", "id": "rs_2", "summary": [], "encrypted_content": "ENC"},
                {"type": "message", "id": "msg_1", "status": "completed", "role": "assistant", "phase": "commentary", "content": [{"type": "output_text", "text": "a"}]},
                {"type": "function_call", "id": "fc_1", "status": "completed", "call_id": "call_1", "name": "get_time", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "12:00"}
            ]
        });
        replay_without_ids(&mut body);
        assert_eq!(body["include"], json!(["x", "reasoning.encrypted_content"]));
        assert_eq!(
            body["input"],
            json!([
                {"role": "user", "content": "oi"},
                {"type": "reasoning", "summary": [], "encrypted_content": "ENC"},
                {"type": "message", "role": "assistant", "phase": "commentary", "content": [{"type": "output_text", "text": "a"}]},
                {"type": "function_call", "call_id": "call_1", "name": "get_time", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "12:00"}
            ])
        );
        // Idempotente: não repete o `include`.
        replay_without_ids(&mut body);
        assert_eq!(body["include"], json!(["x", "reasoning.encrypted_content"]));
    }
}
