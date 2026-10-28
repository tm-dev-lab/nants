//! Limites por provedor (spec 132 T09): o que um HTTP 429 da Anthropic ou do Gemini quer dizer.
//!
//! - **Pausa:** o teto de gasto da conta na Anthropic (`enforced_spend_limit_reached` ou "spend
//!   limit" na mensagem, sem `retry-after`) e a cota diária do Gemini (`quota_exceeded` na
//!   Interactions; no formato antigo, `RESOURCE_EXHAUSTED` com uma cota `PerDay`). Esperar alguns
//!   segundos não resolve: viram [`ProviderLimit`], que pausa a execução com o motivo, sem repetir
//!   (como o `plan_limit` do plano ChatGPT).
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
use crate::config::ConnectionKind;

/// Limite do provedor que pausa a execução (não se repete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderLimit {
    /// Teto de gasto da conta atingido (Anthropic).
    SpendLimit,
    /// Cota diária esgotada (Gemini).
    DailyQuota,
}

impl ProviderLimit {
    /// Nome estável (`spend_limit`, `daily_quota`).
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderLimit::SpendLimit => "spend_limit",
            ProviderLimit::DailyQuota => "daily_quota",
        }
    }
}

/// O 429 pelo teto de gasto da conta (o texto que a Anthropic usa; qualquer caixa).
pub fn spend_limit_reached(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("enforced_spend_limit_reached") || lower.contains("spend limit") || lower.contains("spend_limit")
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
pub(super) fn classify(kind: ConnectionKind, status: u16, body: &str) -> Option<ProviderLimit> {
    if status != 429 {
        return None;
    }
    match kind {
        ConnectionKind::Anthropic if spend_limit_reached(body) => Some(ProviderLimit::SpendLimit),
        ConnectionKind::GoogleGemini if daily_quota_exceeded(body) => Some(ProviderLimit::DailyQuota),
        _ => None,
    }
}

/// O código de um evento `error` do stream do Gemini é a cota diária.
pub(super) fn is_daily_quota_code(error: &Value) -> bool {
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
        assert_eq!(classify(ConnectionKind::Anthropic, 429, spend), Some(ProviderLimit::SpendLimit));
        assert_eq!(classify(ConnectionKind::Anthropic, 429, "You have reached your Spend Limit"), Some(ProviderLimit::SpendLimit));
        assert_eq!(classify(ConnectionKind::Anthropic, 429, rate), None);
        assert_eq!(classify(ConnectionKind::Anthropic, 400, spend), None);
        assert_eq!(classify(ConnectionKind::ApiKey, 429, spend), None);
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
