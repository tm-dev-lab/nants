//! Evidência das falhas e das novas tentativas do provedor (spec 133 T01).
//!
//! - [`HttpEvidence`]: o que uma resposta de erro prova sem conteúdo do pedido: o status, o
//!   `error.type`, o `error.code`, o `error.param` e o `request-id` do provedor, mais a mensagem do
//!   provedor redigida e com teto (só para o registro persistente do projeto, nunca para a tela).
//! - [`FailureCategory`]: a categoria do Nants, fechada, tirada do status HTTP e do `error.type`/
//!   `error.code` estruturado (nunca da mensagem livre nem do nome do modelo). A T02 reaproveita.
//! - [`FailureEvidence`]: a falha final de um papel, com o papel, o modelo, a tarefa e a tentativa.
//! - O rastro da chamada: um escopo por task ([`with_trace_scope`]) onde o cliente anota as novas
//!   tentativas, os degraus de relaxamento, o `request-id` da última resposta e a evidência do
//!   último erro HTTP. Sem escopo, as anotações não fazem nada.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use super::{BadKind, PlanError, ProviderCode, ProviderFailure, ProviderLimit};

/// Teto, em caracteres, da mensagem do provedor guardada na evidência.
pub const MESSAGE_MAX_CHARS: usize = 600;
/// Teto, em caracteres, do `error.param` e do `request-id`.
const LABEL_MAX_CHARS: usize = 100;
/// Cabeçalhos com o id do pedido no provedor (OpenAI, Anthropic, Azure, Google).
const REQUEST_ID_HEADERS: [&str; 4] = ["x-request-id", "request-id", "apim-request-id", "x-goog-request-id"];

/// Categoria do Nants para uma falha do provedor. Vocabulário fechado: a tela a traduz pelo
/// dicionário (`provider.category.<valor>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCategory {
    /// Chave ou login recusados, sem permissão.
    Auth,
    /// Cobrança: crédito esgotado, pagamento pendente, teto de gasto.
    Billing,
    /// Cota de uso esgotada (diária ou do plano).
    Quota,
    /// Limite de ritmo (pedidos ou tokens por minuto).
    Rate,
    /// Pedido recusado pelo provedor.
    BadRequest,
    /// Contexto acima da janela do modelo.
    ContextOverflow,
    /// Falha do lado do provedor.
    Server,
    /// Conexão que caiu, tempo esgotado.
    Transport,
}

impl FailureCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Billing => "billing",
            Self::Quota => "quota",
            Self::Rate => "rate",
            Self::BadRequest => "bad_request",
            Self::ContextOverflow => "context_overflow",
            Self::Server => "server",
            Self::Transport => "transport",
        }
    }
}

/// Categoria pelo `error.type`/`error.code` estruturado (já saneados, em minúsculas).
fn category_of_code(code: &str) -> Option<FailureCategory> {
    let has = |parts: &[&str]| parts.iter().any(|p| code.contains(p));
    if has(&["billing", "payment", "insufficient_quota", "credit", "spend_limit"]) {
        return Some(FailureCategory::Billing);
    }
    if has(&["context_length", "context_window", "too_many_tokens", "request_too_large"]) {
        return Some(FailureCategory::ContextOverflow);
    }
    if has(&["rate_limit", "throttl", "too_many_requests", "resource_exhausted"]) {
        return Some(FailureCategory::Rate);
    }
    if has(&["quota", "usage_limit"]) {
        return Some(FailureCategory::Quota);
    }
    if has(&["auth", "api_key", "permission", "forbidden", "unauthenticated", "not_eligible", "login"]) {
        return Some(FailureCategory::Auth);
    }
    if has(&["overloaded", "server_error", "api_error", "internal", "unavailable", "timeout"]) {
        return Some(FailureCategory::Server);
    }
    if has(&["invalid", "bad_request", "not_found", "unsupported", "failed_precondition", "out_of_range"]) {
        return Some(FailureCategory::BadRequest);
    }
    None
}

/// Categoria pelo status HTTP.
fn category_of_status(status: u16) -> Option<FailureCategory> {
    Some(match status {
        401 | 403 => FailureCategory::Auth,
        402 => FailureCategory::Billing,
        408 => FailureCategory::Transport,
        413 => FailureCategory::ContextOverflow,
        429 => FailureCategory::Rate,
        400..=499 => FailureCategory::BadRequest,
        500..=599 => FailureCategory::Server,
        _ => return None,
    })
}

/// Categoria de uma falha final. O código estruturado vem antes do status (um 400 pode ser de
/// cobrança); `overflow` é o estouro de contexto já reconhecido pelo resumo do histórico.
pub fn classify(failure: &ProviderFailure, http: Option<&HttpEvidence>, overflow: bool) -> Option<FailureCategory> {
    if overflow {
        return Some(FailureCategory::ContextOverflow);
    }
    match failure {
        ProviderFailure::Plan(PlanError::UsageLimit) => return Some(FailureCategory::Quota),
        ProviderFailure::Plan(PlanError::NotEligible | PlanError::NeedsLogin) => return Some(FailureCategory::Auth),
        ProviderFailure::Limit(ProviderLimit::SpendLimit | ProviderLimit::Billing) => return Some(FailureCategory::Billing),
        ProviderFailure::Limit(ProviderLimit::DailyQuota) => return Some(FailureCategory::Quota),
        ProviderFailure::Timeout | ProviderFailure::ConnectFailed => return Some(FailureCategory::Transport),
        ProviderFailure::InvalidConfig => return None,
        _ => {}
    }
    let codes = [
        http.and_then(|h| h.error_code.as_deref()),
        http.and_then(|h| h.error_type.as_deref()),
    ];
    if let Some(category) = codes.into_iter().flatten().find_map(category_of_code) {
        return Some(category);
    }
    match failure {
        ProviderFailure::Http(status, code) => {
            code.as_ref().and_then(|c| category_of_code(c.as_str())).or_else(|| category_of_status(*status))
        }
        ProviderFailure::BadResponse(reason) => match reason.kind {
            BadKind::Throttled => Some(FailureCategory::Rate),
            BadKind::NoFinal | BadKind::Transport => Some(FailureCategory::Transport),
            BadKind::StepsExhausted => None,
            _ => reason.code.as_ref().and_then(|c| category_of_code(c.as_str())).or(Some(FailureCategory::Server)),
        },
        _ => None,
    }
}

/// O que uma resposta de erro do provedor prova. Os códigos passam pelo [`ProviderCode`] (formato
/// fixo, sem cara de chave); a mensagem já vem redigida pela chave da conexão e com teto, e quem a
/// grava passa ainda pela redação dos segredos da configuração.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpEvidence {
    pub status: u16,
    pub error_type: Option<String>,
    pub error_code: Option<String>,
    pub error_param: Option<String>,
    pub request_id: Option<String>,
    pub message: Option<String>,
}

/// Rótulo técnico seguro (`[A-Za-z0-9_.:/\[\]-]`, até [`LABEL_MAX_CHARS`]); outro texto vira `None`.
fn safe_label(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let ok = !raw.is_empty()
        && raw.chars().count() <= LABEL_MAX_CHARS
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-' | '[' | ']'));
    ok.then(|| raw.to_string())
}

/// Código saneado do provedor (texto ou número).
fn code_text(v: &Value) -> Option<String> {
    let raw = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    ProviderCode::parse(&raw).map(|c| c.as_str().to_string())
}

/// Corta `text` em [`MESSAGE_MAX_CHARS`], numa linha só.
fn capped_message(text: &str) -> Option<String> {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = one_line.chars().take(MESSAGE_MAX_CHARS).collect();
    if one_line.chars().count() > MESSAGE_MAX_CHARS {
        out.push('…');
    }
    (!out.is_empty()).then_some(out)
}

/// Evidência de uma resposta de erro: `status` e o corpo `body` (OpenAI: `error.{type,code,param,
/// message}`; Anthropic: `error.{type,message}` e `request_id`; Gemini: `error.{status,code,message}`,
/// às vezes dentro de uma lista). `secret` (a chave ou o token da conexão) é redigido da mensagem.
/// Corpo que não é JSON: a mensagem é o começo do texto.
pub fn parse_error_body(status: u16, body: &str, secret: &str) -> HttpEvidence {
    let redacted = if secret.is_empty() { body.to_string() } else { body.replace(secret, "***") };
    let parsed: Option<Value> = serde_json::from_str(&redacted).ok();
    let root = match &parsed {
        Some(Value::Array(items)) => items.first().cloned().unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let error = if root["error"].is_object() { &root["error"] } else { &root };
    let mut ev = HttpEvidence { status, ..HttpEvidence::default() };
    ev.error_type = error.get("type").and_then(code_text).or_else(|| error.get("status").and_then(code_text));
    ev.error_code = error.get("code").and_then(code_text);
    ev.error_param = error.get("param").and_then(Value::as_str).and_then(safe_label);
    ev.request_id = root.get("request_id").and_then(Value::as_str).and_then(safe_label);
    ev.message = match error.get("message").and_then(Value::as_str) {
        Some(message) => capped_message(message),
        None if parsed.is_none() => capped_message(&redacted),
        None => None,
    };
    ev
}

/// `request-id` dos cabeçalhos de uma resposta, quando houver.
pub fn request_id_of(headers: &reqwest::header::HeaderMap) -> Option<String> {
    REQUEST_ID_HEADERS
        .iter()
        .find_map(|name| headers.get(*name).and_then(|v| v.to_str().ok()).and_then(safe_label))
}

/// Falha final de um papel (spec 133 T01): quem, com que modelo, em que tarefa e tentativa, a
/// categoria e a evidência HTTP (quando a falha veio de uma resposta de erro). A conexão é
/// preenchida pelo servidor, que conhece a configuração do papel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailureEvidence {
    pub role: String,
    pub model: String,
    pub task: Option<String>,
    pub attempt: Option<u32>,
    pub category: Option<FailureCategory>,
    /// Novas tentativas feitas pelo cliente antes de desistir.
    pub retries: u32,
    pub http: Option<HttpEvidence>,
    /// Conexão do papel (preenchida pelo servidor pela configuração da execução).
    pub connection: Option<ConnectionRef>,
}

/// Conexão de um papel: id, nome dado pela pessoa e tipo (`api_key`, `anthropic`...).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionRef {
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// Tentativa do id de sessão (`developer:T02:a1`, `reviewer:T03:a2r1`): o número depois de `:a`.
pub fn attempt_of_session(session: &str) -> Option<u32> {
    let tail = session.rsplit(':').next()?.strip_prefix('a')?;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Rótulo do motivo de término de uma resposta: `stop`, `length`, `tool_calls` ou o motivo do
/// provedor saneado (`[a-z0-9_]`, até 40).
pub fn finish_label(raw: Option<&str>) -> Option<String> {
    let lower = raw?.trim().to_ascii_lowercase();
    let label = match lower.as_str() {
        "" => return None,
        "stop" | "end_turn" | "stop_sequence" | "completed" => "stop",
        "length" | "max_tokens" | "max_output_tokens" | "model_context_window_exceeded" => "length",
        "tool_calls" | "tool_use" | "function_call" => "tool_calls",
        other => {
            let ok = other.len() <= 40 && other.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            return ok.then(|| other.to_string());
        }
    };
    Some(label.to_string())
}

/// Estado do rastro de uma task.
#[derive(Debug, Default)]
struct TraceState {
    retries: u32,
    relaxed: Vec<&'static str>,
    request_id: Option<String>,
    last_error: Option<HttpEvidence>,
}

/// Rastro de uma chamada: novas tentativas e degraus de relaxamento aplicados.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallTrace {
    pub retries: u32,
    pub relaxed: Vec<&'static str>,
}

tokio::task_local! {
    static TRACE: Arc<Mutex<TraceState>>;
}

/// Roda `fut` com um escopo de rastro, quando a task ainda não tem um (o de fora vale).
pub async fn with_trace_scope<F: Future>(fut: F) -> F::Output {
    if TRACE.try_with(|_| ()).is_ok() {
        fut.await
    } else {
        TRACE.scope(Arc::default(), fut).await
    }
}

fn with_state<R>(f: impl FnOnce(&mut TraceState) -> R) -> Option<R> {
    TRACE.try_with(|state| f(&mut state.lock().unwrap_or_else(PoisonError::into_inner))).ok()
}

/// Começo de uma chamada ao modelo: zera o rastro da anterior.
pub fn begin_call() {
    with_state(|s| *s = TraceState::default());
}

/// Rastro da chamada em curso (ou da última), quando há escopo.
pub fn call_trace() -> Option<CallTrace> {
    with_state(|s| CallTrace { retries: s.retries, relaxed: s.relaxed.clone() })
}

/// Evidência do último erro HTTP da chamada, com o `request-id` dela.
pub fn last_http_error() -> Option<HttpEvidence> {
    with_state(|s| s.last_error.clone()).flatten()
}

/// Nova tentativa do mesmo pedido.
pub(super) fn note_retry() {
    with_state(|s| s.retries = s.retries.saturating_add(1));
}

/// Degrau de relaxamento aplicado (um campo opcional recusado e retirado).
pub(super) fn note_relax(step: &'static str) {
    with_state(|s| s.relaxed.push(step));
}

/// `request-id` da resposta que acabou de chegar.
pub(super) fn note_headers(headers: &reqwest::header::HeaderMap) {
    let id = request_id_of(headers);
    with_state(|s| s.request_id = id);
}

/// Resposta de erro: a evidência dela, com o `request-id` do cabeçalho quando o corpo não o traz.
pub(super) fn note_http_error(status: u16, body: &str, secret: &str) {
    if TRACE.try_with(|_| ()).is_err() {
        return;
    }
    let mut ev = parse_error_body(status, body, secret);
    with_state(|s| {
        if ev.request_id.is_none() {
            ev.request_id = s.request_id.clone();
        }
        s.last_error = Some(ev);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::llm_client::BadReason;

    #[test]
    fn parses_openai_anthropic_and_gemini_bodies() {
        let openai = r#"{"error":{"message":"Invalid value for x","type":"invalid_request_error","param":"messages[2].content","code":"invalid_value"}}"#;
        let ev = parse_error_body(400, openai, "");
        assert_eq!(ev.error_type.as_deref(), Some("invalid_request_error"));
        assert_eq!(ev.error_code.as_deref(), Some("invalid_value"));
        assert_eq!(ev.error_param.as_deref(), Some("messages[2].content"));
        assert_eq!(ev.message.as_deref(), Some("Invalid value for x"));
        let anthropic = r#"{"type":"error","error":{"type":"billing_error","message":"credit low"},"request_id":"req_011abc"}"#;
        let ev = parse_error_body(402, anthropic, "");
        assert_eq!((ev.error_type.as_deref(), ev.request_id.as_deref()), (Some("billing_error"), Some("req_011abc")));
        let gemini = r#"[{"error":{"code":400,"message":"bad","status":"INVALID_ARGUMENT"}}]"#;
        let ev = parse_error_body(400, gemini, "");
        assert_eq!((ev.error_type.as_deref(), ev.error_code.as_deref()), (Some("invalid_argument"), Some("400")));
    }

    #[test]
    fn message_is_redacted_capped_and_codes_are_sanitized() {
        let key = "chave-de-teste-123";
        let long = "x".repeat(MESSAGE_MAX_CHARS * 2);
        let body = format!(r#"{{"error":{{"message":"usa {key} {long}","type":"tem espaço","code":"ok_code"}}}}"#);
        let ev = parse_error_body(400, &body, key);
        let message = ev.message.unwrap_or_default();
        assert!(!message.contains(key) && message.contains("***"), "{message}");
        assert!(message.chars().count() <= MESSAGE_MAX_CHARS + 1);
        assert_eq!(ev.error_type, None);
        assert_eq!(ev.error_code.as_deref(), Some("ok_code"));
        let plain = parse_error_body(502, "<html>gateway</html>", "");
        assert_eq!(plain.message.as_deref(), Some("<html>gateway</html>"));
    }

    #[test]
    fn classify_prefers_structured_code_then_status() {
        let http = |status, ty: Option<&str>, code: Option<&str>| HttpEvidence {
            status,
            error_type: ty.map(str::to_string),
            error_code: code.map(str::to_string),
            ..HttpEvidence::default()
        };
        let f400 = ProviderFailure::Http(400, None);
        assert_eq!(classify(&f400, Some(&http(400, Some("billing_error"), None)), false), Some(FailureCategory::Billing));
        assert_eq!(classify(&f400, Some(&http(400, Some("invalid_request_error"), None)), false), Some(FailureCategory::BadRequest));
        assert_eq!(classify(&f400, None, false), Some(FailureCategory::BadRequest));
        assert_eq!(classify(&f400, None, true), Some(FailureCategory::ContextOverflow));
        let f429 = ProviderFailure::Http(429, None);
        assert_eq!(classify(&f429, Some(&http(429, None, Some("insufficient_quota"))), false), Some(FailureCategory::Billing));
        assert_eq!(classify(&f429, None, false), Some(FailureCategory::Rate));
        assert_eq!(classify(&ProviderFailure::Http(402, None), None, false), Some(FailureCategory::Billing));
        assert_eq!(classify(&ProviderFailure::Http(401, None), None, false), Some(FailureCategory::Auth));
        assert_eq!(classify(&ProviderFailure::Http(529, None), None, false), Some(FailureCategory::Server));
        assert_eq!(classify(&ProviderFailure::Timeout, None, false), Some(FailureCategory::Transport));
        assert_eq!(classify(&ProviderFailure::Limit(ProviderLimit::SpendLimit), None, false), Some(FailureCategory::Billing));
        assert_eq!(classify(&ProviderFailure::Limit(ProviderLimit::DailyQuota), None, false), Some(FailureCategory::Quota));
        assert_eq!(classify(&ProviderFailure::Plan(PlanError::UsageLimit), None, false), Some(FailureCategory::Quota));
        let throttled = ProviderFailure::BadResponse(BadReason::new(BadKind::Throttled));
        assert_eq!(classify(&throttled, None, false), Some(FailureCategory::Rate));
        assert_eq!(classify(&ProviderFailure::InvalidConfig, None, false), None);
    }

    #[test]
    fn attempt_and_finish_labels() {
        assert_eq!(attempt_of_session("developer:T02:a1"), Some(1));
        assert_eq!(attempt_of_session("reviewer:T03:a2r1"), Some(2));
        assert_eq!(attempt_of_session("architect:-"), None);
        assert_eq!(finish_label(Some("end_turn")).as_deref(), Some("stop"));
        assert_eq!(finish_label(Some("max_tokens")).as_deref(), Some("length"));
        assert_eq!(finish_label(Some("tool_use")).as_deref(), Some("tool_calls"));
        assert_eq!(finish_label(Some("content_filter")).as_deref(), Some("content_filter"));
        assert_eq!(finish_label(Some("Com Espaço")), None);
        assert_eq!(finish_label(None), None);
    }

    #[tokio::test]
    async fn trace_scope_records_and_resets() {
        note_retry();
        assert_eq!(call_trace(), None, "sem escopo, nada");
        with_trace_scope(async {
            note_retry();
            note_relax("effort");
            note_http_error(400, r#"{"error":{"type":"invalid_request_error"}}"#, "");
            with_trace_scope(async { note_retry() }).await;
            assert_eq!(call_trace(), Some(CallTrace { retries: 2, relaxed: vec!["effort"] }));
            assert_eq!(last_http_error().map(|e| e.status), Some(400));
            begin_call();
            assert_eq!(call_trace(), Some(CallTrace::default()));
            assert_eq!(last_http_error(), None);
        })
        .await;
    }
}
