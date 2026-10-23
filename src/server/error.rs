//! Erros HTTP no formato JSON `{ "error": "...", "code": "...", "params": {...} }`.
//!
//! Todo erro da API leva um `code` estável e `params` (spec 102; obrigatório desde a spec 115 T02):
//! a interface traduz pelo `code` (`serverError.<code>` no dicionário da web). O texto em `error` é
//! a reserva em pt-BR, tirada do dicionário do servidor (`serverError.<code>` em `src/i18n/pt_br.rs`),
//! para registros e clientes antigos.
//!
//! O único construtor público é [`ApiError::coded`]: o construtor com texto livre é privado deste
//! módulo, então criar um erro sem código fora daqui não compila.
//!
//! ```compile_fail
//! use nants::server::error::ApiError;
//! let _ = ApiError::new(axum::http::StatusCode::BAD_REQUEST, "texto livre");
//! ```
//!
//! ```
//! use nants::server::error::ApiError;
//! let err = ApiError::coded(axum::http::StatusCode::NOT_FOUND, "project_not_found", serde_json::Map::new());
//! assert_eq!(err.code(), Some("project_not_found"));
//! ```
//!
//! Mensagens nunca devem conter chaves de API.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{Map, Value};

use crate::i18n::{self, Lang};

/// Erro de requisição com status HTTP, código estável e texto de reserva seguro para o cliente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    /// Detalhe curto e seguro ao lado do código (`detail`), ex.: motivo da resposta ruim do
    /// provedor (spec 088). Nunca texto remoto.
    detail: Option<String>,
    /// Código estável para a interface traduzir (spec 102), ex.: `host_forbidden`.
    code: Option<&'static str>,
    /// Parâmetros do texto traduzido (textos e números, nunca segredos).
    params: Option<Map<String, Value>>,
    /// Motivo fixo legado (`reason`), lido pela interface em alguns 409 (ex.: `base_dirty`).
    reason: Option<&'static str>,
    /// Campos extras do corpo (ex.: `missing` do 412, `path` do 409 de pasta existente).
    extra: Map<String, Value>,
}

/// Texto de um parâmetro para preencher o texto de reserva (texto sem aspas; número como está).
fn param_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Texto de reserva em pt-BR de `serverError.<code>`, com os `params` preenchidos.
pub fn fallback_text(code: &str, params: &Map<String, Value>) -> String {
    let values: Vec<(String, String)> = params.iter().map(|(k, v)| (k.clone(), param_text(v))).collect();
    let pairs: Vec<(&str, &str)> = values.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    i18n::tr(Lang::PtBr, &format!("serverError.{code}"), &pairs)
}

impl ApiError {
    /// Construtor sem código: privado deste módulo (spec 115 T02). Fora daqui, use [`ApiError::coded`].
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            detail: None,
            code: None,
            params: None,
            reason: None,
            extra: Map::new(),
        }
    }

    /// Erro com código estável (spec 115 T02): o texto de reserva sai do dicionário do servidor
    /// (`serverError.<code>`, em pt-BR) e o JSON leva `code` e `params` (`params` vazio fica de fora).
    pub fn coded(status: StatusCode, code: &'static str, params: Map<String, Value>) -> Self {
        let mut err = Self::new(status, fallback_text(code, &params));
        err.code = Some(code);
        err.params = (!params.is_empty()).then_some(params);
        err
    }

    /// Acrescenta `detail` ao corpo JSON.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Acrescenta o motivo fixo `reason` ao corpo JSON.
    pub fn with_reason(mut self, reason: &'static str) -> Self {
        self.reason = Some(reason);
        self
    }

    /// Acrescenta um campo extra ao corpo JSON (nunca `error`, `code` ou `params`).
    pub fn with_field(mut self, name: &'static str, value: Value) -> Self {
        if !matches!(name, "error" | "code" | "params" | "detail" | "reason") {
            self.extra.insert(name.to_string(), value);
        }
        self
    }

    pub fn reason(&self) -> Option<&'static str> {
        self.reason
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    pub fn code(&self) -> Option<&'static str> {
        self.code
    }

    pub fn params(&self) -> Option<&Map<String, Value>> {
        self.params.as_ref()
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Corpo JSON: `error` e `code` sempre; `detail`, `params`, `reason` e os extras quando houver.
    pub fn body(&self) -> Value {
        let mut body = self.extra.clone();
        body.insert("error".into(), Value::from(self.message.clone()));
        if let Some(detail) = &self.detail {
            body.insert("detail".into(), Value::from(detail.clone()));
        }
        if let Some(code) = self.code {
            body.insert("code".into(), Value::from(code));
        }
        if let Some(params) = &self.params {
            body.insert("params".into(), Value::Object(params.clone()));
        }
        if let Some(reason) = self.reason {
            body.insert("reason".into(), Value::from(reason));
        }
        Value::Object(body)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body())).into_response()
    }
}

/// Parâmetros neutros de idioma de uma falha do provedor (spec 102): `provider` (causa fixa, ex.:
/// `provider_bad_response`), `kind` (tipo estável do motivo) e `provider_code` (código saneado),
/// os dois últimos quando houver. Nunca o rótulo pt-BR de `detail()`/`describe()`.
pub fn provider_failure_params(failure: &crate::agents::llm_client::ProviderFailure) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("provider".into(), Value::from(failure.to_string()));
    if let Some((kind, code)) = failure.reason_kind() {
        out.insert("kind".into(), Value::from(kind));
        if let Some(code) = code {
            out.insert("provider_code".into(), Value::from(code));
        }
    }
    out
}

/// Parâmetros de [`ApiError::coded`] a partir de pares `(nome, valor)`.
pub fn params<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn body_of(resp: Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16).await.expect("corpo");
        serde_json::from_slice(&bytes).expect("json")
    }

    /// Spec 115 T02: o texto de reserva vem do dicionário do servidor (pt-BR), com os `params`
    /// preenchidos; o corpo leva `code` e `params`.
    #[tokio::test]
    async fn coded_takes_the_fallback_from_the_dictionary_and_sends_code_and_params() {
        let err = ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "model_required", params([("role", json!("developer"))]));
        assert_eq!(err.message(), "developer: model é obrigatório");
        assert_eq!(err.code(), Some("model_required"));
        assert_eq!(err.params().and_then(|p| p.get("role")), Some(&json!("developer")));
        let body = body_of(err.into_response()).await;
        assert_eq!(
            body,
            json!({"error": "developer: model é obrigatório", "code": "model_required", "params": {"role": "developer"}})
        );
        // Número entra no texto como está; `params` vazio fica de fora.
        let busy = ApiError::coded(StatusCode::BAD_REQUEST, "device_code_invalid", params([("attempts_left", json!(3))]));
        assert_eq!(busy.message(), "código errado");
        let bare = ApiError::coded(StatusCode::FORBIDDEN, "host_forbidden", Map::new());
        assert_eq!(body_of(bare.into_response()).await, json!({"error": "Host não permitido", "code": "host_forbidden"}));
        let range = ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "max_steps_range", params([("role", json!("reviewer")), ("min", json!(10)), ("max", json!(1000))]));
        assert_eq!(range.message(), "reviewer: limite de passos deve estar entre 10 e 1000");
    }

    #[tokio::test]
    async fn detail_reason_and_extra_fields_go_next_to_the_code() {
        let err = ApiError::coded(StatusCode::CONFLICT, "app_port_in_use", Map::new())
            .with_detail("detalhe")
            .with_reason("docker_port_in_use")
            .with_field("path", json!("/a"))
            .with_field("code", json!("outro"));
        assert_eq!(err.reason(), Some("docker_port_in_use"));
        assert_eq!(err.detail(), Some("detalhe"));
        let body = body_of(err.into_response()).await;
        assert_eq!(
            body,
            json!({
                "error": "uma porta publicada pelo ambiente Docker já está em uso no host",
                "detail": "detalhe",
                "reason": "docker_port_in_use",
                "code": "app_port_in_use",
                "path": "/a",
            })
        );
    }

    /// O construtor sem código é privado deste módulo: fora daqui, só `coded` compila (o doctest
    /// `compile_fail` do módulo documenta). Aqui dentro, ele monta o corpo antigo, sem `code`.
    #[tokio::test]
    async fn the_free_text_constructor_stays_inside_this_module() {
        let resp = ApiError::new(StatusCode::BAD_REQUEST, "texto").into_response();
        assert_eq!(body_of(resp).await, json!({"error": "texto"}));
        // Só o código de produção (antes deste módulo de testes), que não cita os nomes abaixo.
        let source = include_str!("error.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(production.contains("\n    fn new(status: StatusCode"), "`new` precisa continuar privado");
        for visibility in ["pub", "pub(crate)", "pub(super)"] {
            for name in ["new", "unprocessable", "internal", "with_code", "json_response"] {
                let public = format!("{visibility} fn {name}(");
                assert!(!production.contains(&public), "construtor sem código público: {public}");
            }
        }
    }

    #[test]
    fn provider_params_are_language_free() {
        use crate::agents::llm_client::{BadKind, BadReason, ProviderCode, ProviderFailure};
        let bad = ProviderFailure::BadResponse(BadReason::with_code(BadKind::Incomplete, Some("content_filter")));
        assert_eq!(
            Value::Object(provider_failure_params(&bad)),
            json!({"provider": "provider_bad_response", "kind": "incomplete", "provider_code": "content_filter"}),
            "spec 102: params sem rótulo pt-BR"
        );
        let unreadable = ProviderFailure::BadResponse(BadReason::new(BadKind::ImageUnreadable));
        assert_eq!(Value::Object(provider_failure_params(&unreadable)), json!({"provider": "provider_bad_response", "kind": "image_unreadable"}));
        let http = ProviderFailure::Http(400, ProviderCode::parse("invalid_value"));
        assert_eq!(Value::Object(provider_failure_params(&http)), json!({"provider": "provider_http_400", "kind": "http_code", "provider_code": "invalid_value"}));
        assert_eq!(Value::Object(provider_failure_params(&ProviderFailure::Timeout)), json!({"provider": "provider_timeout"}));
        let err = ApiError::coded(StatusCode::BAD_GATEWAY, "provider_failed", provider_failure_params(&ProviderFailure::Timeout));
        assert_eq!(err.message(), "o provedor do modelo falhou (provider_timeout)");
    }
}
