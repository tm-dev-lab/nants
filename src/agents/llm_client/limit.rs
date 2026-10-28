//! Limites por provedor (spec 132 T09 e 133 T02): o que um erro de conta quer dizer.
//!
//! - **Pausa:**
//!   - a cobrança, em qualquer provedor (spec 133 T02): todo 402 e os códigos estruturados de
//!     cobrança no `error.type` ou no `error.code` (`insufficient_quota`, `billing_error`...), no
//!     HTTP e no meio do stream;
//!   - o teto de gasto da conta, da organização ou do workspace na Anthropic, no 429 e no 400, e o
//!     crédito esgotado ("credit balance") no 400, pelo `error.message`;
//!   - a cota diária do Gemini (`quota_exceeded` na Interactions; no formato antigo,
//!     `RESOURCE_EXHAUSTED` com uma cota `PerDay`).
//!
//!   Esperar alguns segundos não resolve: viram [`ProviderLimit`], que pausa a execução com o
//!   motivo, sem repetir e sem ensinar o ritmo (como o `plan_limit` do plano ChatGPT). O texto só é
//!   lido no `error.message` do JSON, no escopo do tipo da conexão (C-10).
//! - **Ritmo:** os demais 429 (`rate_limit_error`, `rate_limit_exceeded`, `too_many_requests`, o
//!   `RESOURCE_EXHAUSTED` por minuto) seguem o limitador de ritmo (spec 089), com o `retry-after`
//!   ou, no Gemini, o `retryDelay` do corpo.
//! - **Tokens de saída:** o 429 da Anthropic que cita os tokens de saída por minuto (o limite é
//!   estimado pelo `max_tokens` do pedido) reduz o `max_tokens` uma vez ([`output_tokens_cited`]).
//!
//! O corpo só é lido para escolher o motivo; nenhum texto do provedor sai daqui.

use std::time::Duration;

use serde_json::Value;

use super::anthropic::MIN_THINKING_BUDGET;
use super::pace::parse_reset;
use super::{BadReason, LlmError};
use crate::config::ConnectionKind;

/// Limite do provedor que pausa a execução (não se repete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderLimit {
    /// Teto de gasto da conta atingido (Anthropic).
    SpendLimit,
    /// Cota diária esgotada (Gemini).
    DailyQuota,
    /// Cobrança (spec 133 T02): crédito esgotado ou pagamento pendente, em qualquer provedor.
    Billing,
}

impl ProviderLimit {
    /// Nome estável (`spend_limit`, `daily_quota`, `billing`).
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderLimit::SpendLimit => "spend_limit",
            ProviderLimit::DailyQuota => "daily_quota",
            ProviderLimit::Billing => "billing",
        }
    }
}

/// Códigos estruturados de cobrança (`error.type` ou `error.code`), de qualquer provedor: o
/// `billing_error` (402 da Anthropic), o `payment_required` (402 do Gemini), o
/// `insufficient_quota` e o `credit_balance_exhausted` (OpenAI e compatíveis) e o
/// `billing_hard_limit_reached` (o teto de cobrança da OpenAI). Comparação sem caixa.
const BILLING_CODES: [&str; 5] = ["billing_error", "payment_required", "insufficient_quota", "credit_balance_exhausted", "billing_hard_limit_reached"];

/// Os campos do erro que a classificação lê: o `type`, o `code` (texto) e a `message`, só do objeto
/// `error` do JSON (spec 133 T02, C-10). O resto do corpo, que pode repetir o que foi enviado, não
/// conta.
#[derive(Debug, Default)]
struct ErrorFields {
    kind: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

impl ErrorFields {
    /// Lê do objeto `error` (ou do próprio objeto, quando já é o `error` de um evento do stream).
    fn of(error: &Value) -> Self {
        let error = error.get("error").filter(|e| e.is_object()).unwrap_or(error);
        let text = |key: &str| error.get(key).and_then(Value::as_str).map(str::to_ascii_lowercase);
        ErrorFields { kind: text("type"), code: text("code"), message: text("message") }
    }

    fn from_body(body: &str) -> Self {
        error_of(body).map(|e| Self::of(&e)).unwrap_or_default()
    }

    /// O `type` ou o `code` é um dos [`BILLING_CODES`].
    fn has_billing_code(&self) -> bool {
        [&self.kind, &self.code].into_iter().flatten().any(|c| BILLING_CODES.contains(&c.as_str()))
    }

    /// A `message` contém algum dos trechos (já em minúsculas).
    fn message_has(&self, needles: &[&str]) -> bool {
        self.message.as_deref().is_some_and(|m| needles.iter().any(|n| m.contains(n)))
    }

    /// O teto de gasto da conta, da organização ou do workspace (o texto que a Anthropic usa).
    fn spend_limit(&self) -> bool {
        self.message_has(&["enforced_spend_limit_reached", "spend limit", "spend_limit", "api usage limit"])
            || self.code.as_deref() == Some("enforced_spend_limit_reached")
    }

    /// O crédito acabou ("credit balance", na `message`).
    fn credit_exhausted(&self) -> bool {
        self.message_has(&["credit balance"])
    }
}

/// O erro pelo teto de gasto da conta, no `error.message` do JSON (o texto que a Anthropic usa;
/// qualquer caixa).
pub fn spend_limit_reached(body: &str) -> bool {
    ErrorFields::from_body(body).spend_limit()
}

/// Objeto `error` do corpo, quando for JSON.
fn error_of(body: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("error").filter(|e| e.is_object()).cloned()
}

/// O 429 do Gemini pela cota diária: `error.code` `quota_exceeded` (Interactions) ou, no formato
/// antigo, `error.status` `RESOURCE_EXHAUSTED` com uma violação de cota por dia (`...PerDay...`).
pub fn daily_quota_exceeded(body: &str) -> bool {
    let Some(error) = error_of(body) else {
        return false;
    };
    if error["code"].as_str().is_some_and(|c| c.eq_ignore_ascii_case("quota_exceeded")) {
        return true;
    }
    if error["status"].as_str() != Some("RESOURCE_EXHAUSTED") {
        return false;
    }
    let details = error["details"].as_array().map(Vec::as_slice).unwrap_or_default();
    details
        .iter()
        .flat_map(|d| d["violations"].as_array().map(Vec::as_slice).unwrap_or_default())
        .filter_map(|v| v["quotaId"].as_str())
        .any(|id| id.contains("PerDay"))
}

/// O limite que pausa num erro HTTP `status` de uma conexão `kind`, quando for o caso.
///
/// - **Cobrança, em qualquer provedor:** todo 402; um 400, 403 ou 429 com um código de cobrança
///   no `error.type` ou no `error.code` ([`BILLING_CODES`]).
/// - **Anthropic:** o teto de gasto (da conta, da organização ou do workspace) no 400 ou no 429, e o
///   crédito esgotado ("credit balance") no 400, pelo `error.message`.
/// - **Gemini:** a cota diária no 429.
///
/// O texto só é lido no `error.message` do JSON e no escopo do tipo da conexão; o resto do corpo
/// não conta.
pub(super) fn classify(kind: ConnectionKind, status: u16, body: &str) -> Option<ProviderLimit> {
    if status == 402 {
        return Some(ProviderLimit::Billing);
    }
    let fields = ErrorFields::from_body(body);
    if matches!(status, 400 | 403 | 429) && fields.has_billing_code() {
        return Some(ProviderLimit::Billing);
    }
    match (kind, status) {
        (ConnectionKind::Anthropic, 400 | 429) if fields.spend_limit() => Some(ProviderLimit::SpendLimit),
        (ConnectionKind::Anthropic, 400) if fields.credit_exhausted() => Some(ProviderLimit::Billing),
        (ConnectionKind::GoogleGemini, 429) if daily_quota_exceeded(body) => Some(ProviderLimit::DailyQuota),
        _ => None,
    }
}

/// O erro que chega no meio da resposta (o `error` de um evento do stream ou do corpo de uma
/// resposta 200) é de cobrança, em qualquer provedor: um código de [`BILLING_CODES`] no `type` ou
/// no `code` (o `insufficient_quota` da OpenAI no `response.failed`, por exemplo).
pub(super) fn is_billing_event(error: &Value) -> bool {
    ErrorFields::of(error).has_billing_code()
}

/// O erro de um evento do stream (ou do corpo de uma resposta 200): a pausa por cobrança, quando
/// for o caso; senão, a resposta ruim com o código do provedor (spec 088).
pub(super) fn event_error(error: &Value) -> LlmError {
    if is_billing_event(error) {
        return LlmError::Limit(ProviderLimit::Billing);
    }
    LlmError::Parse(BadReason::provider_error(error))
}

/// O código de um evento `error` do stream do Gemini é a cota diária.
pub(super) fn is_daily_quota_code(error: &Value) -> bool {
    let error = error.get("error").filter(|e| e.is_object()).unwrap_or(error);
    error["code"].as_str().is_some_and(|c| c.eq_ignore_ascii_case("quota_exceeded"))
}

/// Espera sugerida no corpo de um 429 do Gemini: o `retryDelay` (`"30s"`, `"1.5s"`) de um item de
/// `error.details` (o `RetryInfo` do formato antigo) ou o `retry_delay` do próprio `error`.
pub(super) fn gemini_retry_delay(body: &str) -> Option<Duration> {
    let error = error_of(body)?;
    let own = error["retry_delay"].as_str().or_else(|| error["retryDelay"].as_str());
    let details = error["details"].as_array().map(Vec::as_slice).unwrap_or_default();
    own.into_iter()
        .chain(details.iter().filter_map(|d| d["retryDelay"].as_str()))
        .find_map(parse_reset)
        .filter(|d| !d.is_zero())
}

/// O 429 cita os tokens de saída por minuto (`output tokens`, `output_tokens`)?
pub(super) fn cites_output_tokens(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("output tokens") || lower.contains("output_tokens")
}

/// O `max_tokens` reduzido por um 429 que cita os tokens de saída, a partir do `current`, com o piso
/// [`MIN_THINKING_BUDGET`] (o mesmo da redução por 400):
/// - o número logo antes de "output tokens" (com separador de milhar, ex.: `of 8,000 output tokens
///   per minute`), quando está em `MIN_THINKING_BUDGET..current`;
/// - citado um número maior ou igual ao `current`, o `max_tokens` não foi a causa: `None` (só a
///   espera resolve, e a redução única do par não é gasta);
/// - sem número útil (ausente ou abaixo do piso), a metade do atual, nunca abaixo do piso.
///
/// `None` também quando o 429 não cita os tokens de saída ou o resultado não fica menor que o atual.
pub(super) fn output_tokens_cited(body: &str, current: u32) -> Option<u32> {
    if !cites_output_tokens(body) {
        return None;
    }
    let lower = body.to_ascii_lowercase();
    let cited = lower
        .find("output tokens")
        .or_else(|| lower.find("output_tokens"))
        .and_then(|at| number_before(&lower[..at]));
    if cited.is_some_and(|n| n >= current) {
        return None;
    }
    let reduced = cited
        .filter(|n| (MIN_THINKING_BUDGET..current).contains(n))
        .unwrap_or_else(|| (current / 2).max(MIN_THINKING_BUDGET));
    (reduced < current).then_some(reduced)
}

/// O número inteiro no fim de `text` (descontados os espaços), aceitando `,` e `_` entre os
/// dígitos.
fn number_before(text: &str) -> Option<u32> {
    let text = text.trim_end();
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit() || matches!(c, ',' | '_'))
        .last()
        .map(|(i, _)| i)?;
    let digits: String = text[start..].chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_limit_is_recognised_in_the_anthropic_429_only() {
        let spend = r#"{"type":"error","error":{"type":"rate_limit_error","message":"enforced_spend_limit_reached"}}"#;
        let rate = r#"{"type":"error","error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"}}"#;
        let caps = r#"{"type":"error","error":{"type":"rate_limit_error","message":"You have reached your Spend Limit"}}"#;
        assert_eq!(classify(ConnectionKind::Anthropic, 429, spend), Some(ProviderLimit::SpendLimit));
        assert_eq!(classify(ConnectionKind::Anthropic, 429, caps), Some(ProviderLimit::SpendLimit));
        assert_eq!(classify(ConnectionKind::Anthropic, 429, rate), None);
        assert_eq!(classify(ConnectionKind::Anthropic, 400, spend), Some(ProviderLimit::SpendLimit), "o 400 do teto da organização ou do workspace");
        assert_eq!(classify(ConnectionKind::Anthropic, 500, spend), None);
        assert_eq!(classify(ConnectionKind::ApiKey, 429, spend), None);
        // Só o `error.message` do JSON conta (C-10): texto solto ou fora do `error` não classifica.
        assert_eq!(classify(ConnectionKind::Anthropic, 429, "You have reached your Spend Limit"), None);
        let outside = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"},"echo":"spend limit"}"#;
        assert_eq!(classify(ConnectionKind::Anthropic, 429, outside), None);
    }

    #[test]
    fn every_402_is_billing() {
        for kind in [ConnectionKind::ApiKey, ConnectionKind::ChatgptPlan, ConnectionKind::Anthropic, ConnectionKind::GoogleGemini] {
            assert_eq!(classify(kind, 402, ""), Some(ProviderLimit::Billing), "{kind:?}");
            assert_eq!(classify(kind, 402, r#"{"error":{"code":"payment_required"}}"#), Some(ProviderLimit::Billing), "{kind:?}");
        }
    }

    #[test]
    fn billing_codes_pause_in_any_provider() {
        let quota = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#;
        let credit = r#"{"error":{"message":"x","type":"invalid_request_error","code":"CREDIT_BALANCE_EXHAUSTED"}}"#;
        let anthropic = r#"{"type":"error","error":{"type":"billing_error","message":"x"}}"#;
        for kind in [ConnectionKind::ApiKey, ConnectionKind::Anthropic, ConnectionKind::GoogleGemini] {
            for status in [400, 403, 429] {
                for body in [quota, credit, anthropic] {
                    assert_eq!(classify(kind, status, body), Some(ProviderLimit::Billing), "{kind:?} {status} {body}");
                }
            }
            assert_eq!(classify(kind, 500, quota), None, "{kind:?}: o 5xx segue como falha do servidor");
        }
        // O código só conta no objeto `error`: o mesmo texto em outro lugar do corpo não classifica.
        let echoed = r#"{"error":{"type":"invalid_request_error","message":"bad"},"input":"insufficient_quota"}"#;
        assert_eq!(classify(ConnectionKind::ApiKey, 400, echoed), None);
    }

    #[test]
    fn anthropic_credit_balance_400_is_billing_only_in_the_message() {
        let credit = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#;
        assert_eq!(classify(ConnectionKind::Anthropic, 400, credit), Some(ProviderLimit::Billing));
        // Negativos: o 400 comum, o "credit" fora do `error.message`, um "credit" que não é saldo e
        // o mesmo texto noutro tipo de conexão (o escopo é o da Anthropic).
        let plain = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: must be positive"}}"#;
        let echoed = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long"},"credit balance":"is too low"}"#;
        let other_credit = r#"{"type":"error","error":{"type":"invalid_request_error","message":"credit card field is invalid"}}"#;
        for body in [plain, echoed, other_credit, "Your credit balance is too low"] {
            assert_eq!(classify(ConnectionKind::Anthropic, 400, body), None, "{body}");
        }
        assert_eq!(classify(ConnectionKind::ApiKey, 400, credit), None);
        assert_eq!(classify(ConnectionKind::GoogleGemini, 400, credit), None);
    }

    #[test]
    fn stream_error_events_with_billing_codes_pause() {
        let openai = serde_json::json!({"type": "insufficient_quota", "code": "insufficient_quota", "message": "x"});
        let nested = serde_json::json!({"type": "error", "error": {"code": "credit_balance_exhausted"}});
        assert!(matches!(event_error(&openai), LlmError::Limit(ProviderLimit::Billing)));
        assert!(matches!(event_error(&nested), LlmError::Limit(ProviderLimit::Billing)));
        let server = serde_json::json!({"code": "server_error", "message": "insufficient_quota"});
        assert!(matches!(event_error(&server), LlmError::Parse(_)), "o texto da mensagem não conta no evento");
    }

    #[test]
    fn daily_quota_is_recognised_in_both_gemini_formats() {
        let new = r#"{"error":{"code":"quota_exceeded","message":"daily quota"}}"#;
        let minute = r#"{"error":{"code":"rate_limit_exceeded","message":"per minute"}}"#;
        let many = r#"{"error":{"code":"too_many_requests","message":"slow down"}}"#;
        let old_day = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"quota","details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[{"quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier"}]},{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"43s"}]}}"#;
        let old_minute = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"quota","details":[{"violations":[{"quotaId":"GenerateRequestsPerMinutePerProjectPerModel-FreeTier"}]}]}}"#;
        let g = ConnectionKind::GoogleGemini;
        assert_eq!(classify(g, 429, new), Some(ProviderLimit::DailyQuota));
        assert_eq!(classify(g, 429, old_day), Some(ProviderLimit::DailyQuota));
        for body in [minute, many, old_minute, "não é json"] {
            assert_eq!(classify(g, 429, body), None, "{body}");
        }
        assert_eq!(classify(ConnectionKind::Anthropic, 429, new), None);
    }

    #[test]
    fn gemini_retry_delay_comes_from_the_body() {
        let old = r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"43s"}]}}"#;
        let own = r#"{"error":{"code":"rate_limit_exceeded","retry_delay":"1.5s"}}"#;
        assert_eq!(gemini_retry_delay(old), Some(Duration::from_secs(43)));
        assert_eq!(gemini_retry_delay(own), Some(Duration::from_millis(1500)));
        for body in [r#"{"error":{"code":"rate_limit_exceeded"}}"#, r#"{"error":{"retry_delay":"0s"}}"#, "x"] {
            assert_eq!(gemini_retry_delay(body), None, "{body}");
        }
    }

    #[test]
    fn output_tokens_reduction_reads_the_cited_limit() {
        let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed the rate limit for your organization (a1b2c3) of 8,000 output tokens per minute."}}"#;
        assert_eq!(output_tokens_cited(body, 64_000), Some(8_000));
        assert_eq!(output_tokens_cited(body, 8_000), None, "citado igual ao atual: o max_tokens não foi a causa");
        assert_eq!(output_tokens_cited(body, 4_000), None, "citado acima do atual: não reduz");
        let vague = r#"{"error":{"type":"rate_limit_error","message":"output_tokens per minute exceeded"}}"#;
        assert_eq!(output_tokens_cited(vague, 32_000), Some(16_000));
        assert_eq!(output_tokens_cited(vague, 1_500), Some(1_024), "a metade tem o piso");
        assert_eq!(output_tokens_cited(vague, 1_024), None, "já no piso: nada a reduzir");
        assert_eq!(output_tokens_cited(vague, 1), None);
        let tiny = r#"{"error":{"type":"rate_limit_error","message":"of 300 output tokens per minute"}}"#;
        assert_eq!(output_tokens_cited(tiny, 32_000), Some(16_000), "citado abaixo do piso: a metade");
        assert_eq!(output_tokens_cited(tiny, 1_500), Some(1_024));
        let input = r#"{"error":{"type":"rate_limit_error","message":"of 50,000 input tokens per minute"}}"#;
        assert_eq!(output_tokens_cited(input, 32_000), None);
    }
}
