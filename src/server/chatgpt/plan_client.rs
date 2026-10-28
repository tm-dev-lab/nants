//! Cliente das conexões pelo plano ChatGPT (spec 116 T02): liga o `LlmClient` em modo plano ao
//! login da T01 e lê a lista de modelos da conta.
//!
//! - [`llm_client`]: o cliente de qualquer conexão. Por chave, o de sempre; pelo plano, o modo plano
//!   com a fonte de tokens [`PlanTokenSource`] (renovação, descarte do token recusado e "precisa
//!   entrar de novo" pela [`ChatgptAuth`]).
//! - [`normalize_plan_models`]: `GET /v1/models` da conta, só os itens com `visibility == "list"`,
//!   na ordem do servidor; o `slug` vai no pedido e o `display_name` é o nome mostrado.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use super::{ChatgptAuth, TokenError};
use crate::agents::llm_client::{LlmClient, PlanFuture, PlanTokenError, PlanTokens};
use crate::agents::LlmError;
use crate::config::Connection;
use crate::server::models_api::{ModelInfo, ModelList, MODELS_MAX, MODEL_ID_MAX_CHARS};
use crate::server::state::AppState;

/// Fonte do access token de uma conexão pelo plano, sobre o login da T01.
pub struct PlanTokenSource {
    auth: Arc<ChatgptAuth>,
    dir: Option<PathBuf>,
    connection_id: String,
}

impl PlanTokenSource {
    pub fn new(auth: Arc<ChatgptAuth>, dir: Option<PathBuf>, connection_id: impl Into<String>) -> Self {
        Self { auth, dir, connection_id: connection_id.into() }
    }
}

impl PlanTokens for PlanTokenSource {
    fn access_token(&self) -> PlanFuture<'_, Result<String, PlanTokenError>> {
        Box::pin(async move {
            let Some(dir) = &self.dir else {
                return Err(PlanTokenError::NeedsLogin);
            };
            self.auth.access_token(dir, &self.connection_id).await.map_err(|e| match e {
                TokenError::NotConnected | TokenError::NeedsLogin => PlanTokenError::NeedsLogin,
                TokenError::Unavailable => PlanTokenError::Unavailable,
                TokenError::Store => PlanTokenError::Failed,
            })
        })
    }

    fn expire(&self) -> PlanFuture<'_, ()> {
        Box::pin(async move {
            if let Some(dir) = &self.dir {
                self.auth.expire_access_token(dir, &self.connection_id).await;
            }
        })
    }

    fn needs_login(&self) -> PlanFuture<'_, ()> {
        Box::pin(async move {
            if let Some(dir) = &self.dir {
                self.auth.require_login(dir, &self.connection_id).await;
            }
        })
    }
}

/// Cliente da conexão: pelo plano ChatGPT, o modo plano com a fonte de tokens do servidor; por
/// chave, [`LlmClient::from_connection`].
pub fn llm_client(state: &AppState, conn: &Connection) -> Result<LlmClient, LlmError> {
    if !conn.is_chatgpt_plan() {
        return LlmClient::from_connection(conn);
    }
    let source = PlanTokenSource::new(Arc::clone(state.chatgpt()), state.data_dir().map(PathBuf::from), conn.id.clone());
    LlmClient::from_plan_connection(conn, Arc::new(source))
}

/// Itens da listagem do plano: `models`, `data` ou o próprio array.
fn plan_items(body: &Value) -> Option<&Vec<Value>> {
    match body {
        Value::Object(map) => map.get("models").or_else(|| map.get("data")).and_then(Value::as_array),
        Value::Array(items) => Some(items),
        _ => None,
    }
}

/// Texto aceito como id ou nome de modelo: não vazio, curto, sem controle, sem cara de URL e sem o
/// token (o provedor não pode ecoar segredo na listagem).
fn valid_text(text: &str, secret: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= MODEL_ID_MAX_CHARS
        && !text.chars().any(char::is_control)
        && !text.contains("://")
        && (secret.is_empty() || !text.contains(secret))
}

/// Lista de modelos da conta pelo plano: só `visibility == "list"`, na ordem do servidor, sem
/// repetir o `slug` (ou o `id`, na falta dele), até [`MODELS_MAX`]. O nome mostrado é o
/// `display_name` (válido) ou o próprio `slug`. `None` se o corpo não tem lista ou se a lista não
/// está vazia e nenhum item visível é válido.
pub fn normalize_plan_models(body: &Value, secret: &str) -> Option<ModelList> {
    let items = plan_items(body)?;
    let visible: Vec<&Value> = items
        .iter()
        .filter(|item| item.get("visibility").and_then(Value::as_str) == Some("list"))
        .collect();
    let mut models: Vec<ModelInfo> = Vec::new();
    for item in &visible {
        let slug = ["slug", "id"].iter().find_map(|k| item.get(*k).and_then(Value::as_str));
        let Some(slug) = slug.filter(|s| valid_text(s, secret)) else {
            continue;
        };
        if models.iter().any(|m| m.id == slug) {
            continue;
        }
        let name = item
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|n| valid_text(n, secret))
            .map(|n| n.trim().to_string());
        models.push(ModelInfo { id: slug.to_string(), name, ..ModelInfo::default() });
    }
    if !visible.is_empty() && models.is_empty() {
        return None;
    }
    let truncated = models.len() > MODELS_MAX;
    models.truncate(MODELS_MAX);
    Some(ModelList { models, truncated })
}

/// Modelos da conta pela conexão do plano (`GET {base_url}/models` com o token).
pub async fn fetch_plan_models(client: &LlmClient) -> Result<ModelList, LlmError> {
    let body = client.get_json("/models").await?;
    normalize_plan_models(&body, "").ok_or(LlmError::Parse(crate::agents::llm_client::BadReason::new(
        crate::agents::llm_client::BadKind::InvalidJson,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_models_keep_server_order_and_only_listed() {
        let body = json!({"models": [
            {"slug": "gpt-z", "display_name": "GPT Z", "visibility": "list"},
            {"slug": "gpt-hidden", "display_name": "Oculto", "visibility": "hide"},
            {"slug": "gpt-a", "display_name": "GPT A", "visibility": "list"},
            {"slug": "gpt-sem-nome", "visibility": "list"},
            {"slug": "gpt-z", "display_name": "Repetido", "visibility": "list"},
            {"slug": "gpt-sem-visibilidade", "display_name": "X"},
        ]});
        let list = normalize_plan_models(&body, "").expect("lista");
        let ids: Vec<&str> = list.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["gpt-z", "gpt-a", "gpt-sem-nome"]);
        assert_eq!(list.models[0].name.as_deref(), Some("GPT Z"));
        assert_eq!(list.models[2].name, None);
        assert!(!list.truncated);
    }

    #[test]
    fn plan_models_reject_bad_bodies() {
        assert!(normalize_plan_models(&json!({"x": 1}), "").is_none());
        let bad = json!({"data": [{"slug": "https://x", "visibility": "list"}]});
        assert!(normalize_plan_models(&bad, "").is_none());
        let empty = normalize_plan_models(&json!({"models": []}), "").expect("vazia");
        assert!(empty.models.is_empty());
        // Só itens ocultos: lista vazia, não erro.
        let hidden = normalize_plan_models(&json!([{"slug": "a", "visibility": "hide"}]), "").expect("oculta");
        assert!(hidden.models.is_empty());
    }
}
