//! Erros HTTP no formato JSON `{ "error": "..." }`.
//!
//! Erros que a interface mostra levam também um `code` estável e `params` (spec 102): a interface
//! traduz pelo `code`; o texto em pt-BR continua em `error`, como reserva e para registros.
//!
//! Mensagens nunca devem conter chaves de API.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

/// Erro de requisição com status HTTP e mensagem segura para o cliente.
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
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            detail: None,
            code: None,
            params: None,
        }
    }

    /// Acrescenta `detail` ao corpo JSON.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Acrescenta `code` e `params` ao corpo JSON (spec 102). `params` vazio fica de fora.
    pub fn with_code(mut self, code: &'static str, params: Map<String, Value>) -> Self {
        self.code = Some(code);
        self.params = (!params.is_empty()).then_some(params);
        self
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

    /// 422: corpo sintaticamente válido, mas com valores semanticamente inválidos.
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    /// 500: falha interna sem detalhes sensíveis.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Corpo JSON: `error` sempre; `detail`, `code` e `params` quando houver.
    pub fn body(&self) -> Value {
        let mut body = Map::new();
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
        Value::Object(body)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body())).into_response()
    }
}

/// Monta uma resposta JSON `{ "error": message }` com o status dado.
pub fn json_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
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

/// Parâmetros de `with_code` a partir de pares `(nome, valor)`.
pub fn params<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body_of(resp: Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16).await.expect("corpo");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn plain_error_keeps_the_old_shape() {
        let resp = ApiError::new(StatusCode::BAD_REQUEST, "corpo inválido").into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_of(resp).await, json!({"error": "corpo inválido"}));
    }

    #[tokio::test]
    async fn with_code_adds_code_and_params_next_to_the_fallback_text() {
        let err = ApiError::new(StatusCode::CONFLICT, "a porta 3001 está ocupada")
            .with_detail("detalhe")
            .with_code("app_port_in_use", params([("port", json!(3001))]));
        assert_eq!(err.code(), Some("app_port_in_use"));
        assert_eq!(err.params().and_then(|p| p.get("port")), Some(&json!(3001)));
        let body = body_of(err.into_response()).await;
        assert_eq!(
            body,
            json!({"error": "a porta 3001 está ocupada", "detail": "detalhe", "code": "app_port_in_use", "params": {"port": 3001}})
        );
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
        let bare = ApiError::new(StatusCode::FORBIDDEN, "x").with_code("host_forbidden", Map::new());
        assert_eq!(body_of(bare.into_response()).await, json!({"error": "x", "code": "host_forbidden"}));
    }
}
