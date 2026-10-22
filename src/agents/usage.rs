//! Registro de uso (tokens) das chamadas ao modelo, por agente/tarefa/spec.
//!
//! Um [`UsageBackend`] envolve um [`ChatBackend`] e entrega cada `Usage` reportado a um
//! [`UsageSink`] injetável; a persistência fica fora dos agentes.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::chat::ChatBackend;
use super::llm_client::{ChatRequest, ChatResponse, StreamDelta, Usage};
use super::LlmError;

/// Uso de uma chamada ao modelo, com o contexto do agente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEvent {
    pub agent: String,
    pub model: String,
    pub spec: Option<String>,
    pub task: Option<String>,
    pub usage: Usage,
    /// Chamadas de busca na web da resposta (cobradas por chamada).
    pub web_search_calls: u32,
    /// Imagens geradas pela chamada (spec 090).
    pub images: u32,
    /// Qualidade das imagens geradas (`low|medium|high`, spec 092 T03); `None` sem imagem.
    pub image_quality: Option<String>,
}

/// Destino dos registros de uso. Nunca deve falhar para o chamador.
pub trait UsageSink: Send + Sync {
    fn record(&self, event: UsageEvent);
}

/// Orçamento da spec, consultado antes de cada chamada de agente.
pub trait BudgetGuard: Send + Sync {
    /// `Some(motivo)` quando o limite da spec foi atingido.
    fn exceeded(&self, spec: &str) -> Option<String>;
}

/// Contexto de registro de um agente.
#[derive(Clone)]
pub struct UsageScope {
    pub sink: Arc<dyn UsageSink>,
    pub agent: String,
    pub spec: Option<String>,
    pub task: Option<String>,
}

/// Decorador de [`ChatBackend`]: repassa as chamadas e registra o `Usage` das respostas.
/// Sem escopo, é transparente.
pub struct UsageBackend<'a> {
    inner: &'a dyn ChatBackend,
    scope: Option<UsageScope>,
}

impl<'a> UsageBackend<'a> {
    pub fn new(inner: &'a dyn ChatBackend, scope: Option<UsageScope>) -> Self {
        Self { inner, scope }
    }

    fn note(&self, model: &str, resp: &ChatResponse) {
        let usage = resp.usage.or_else(|| (resp.web_search_calls > 0).then(|| Usage::new(0, 0)));
        if let (Some(scope), Some(usage)) = (&self.scope, usage) {
            scope.sink.record(UsageEvent {
                agent: scope.agent.clone(),
                model: model.to_string(),
                spec: scope.spec.clone(),
                task: scope.task.clone(),
                usage,
                web_search_calls: resp.web_search_calls,
                images: 0,
                image_quality: None,
            });
        }
    }
}

impl ChatBackend for UsageBackend<'_> {
    fn complete<'a>(
        &'a self,
        req: &'a ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let resp = self.inner.complete(req).await?;
            self.note(&req.model, &resp);
            Ok(resp)
        })
    }

    fn complete_stream<'a>(
        &'a self,
        req: &'a ChatRequest,
        on_delta: &'a (dyn Fn(StreamDelta) + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let resp = self.inner.complete_stream(req, on_delta).await?;
            self.note(&req.model, &resp);
            Ok(resp)
        })
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    /// Sink que guarda os eventos em memória.
    #[derive(Default)]
    pub struct MemSink(pub Mutex<Vec<UsageEvent>>);

    impl UsageSink for MemSink {
        fn record(&self, event: UsageEvent) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MemSink;
    use super::*;
    use crate::agents::llm_client::{ChatMessage, Role};

    struct Fake(Option<Usage>);

    impl ChatBackend for Fake {
        fn complete<'a>(
            &'a self,
            _req: &'a ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
            let usage = self.0;
            Box::pin(async move {
                Ok(ChatResponse {
                    message: ChatMessage::text(Role::Assistant, "ok"),
                    finish_reason: None,
                    usage,
                    fallback_used: false,
                    web_search_calls: 0,
                    citations: Vec::new(),
                    web_search_rejected: false,
                })
            })
        }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: "m1".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            effort: None,
            response_json: false,
            web_search: false,
        }
    }

    fn scope(sink: &Arc<MemSink>) -> UsageScope {
        UsageScope {
            sink: sink.clone(),
            agent: "developer".into(),
            spec: Some("s".into()),
            task: Some("T01".into()),
        }
    }

    #[tokio::test]
    async fn records_complete_and_stream() {
        let sink = Arc::new(MemSink::default());
        let fake = Fake(Some(Usage { prompt: 3, completion: 4, cached: Some(1), reasoning: Some(2) }));
        let b = UsageBackend::new(&fake, Some(scope(&sink)));
        b.complete(&req()).await.expect("ok");
        b.complete_stream(&req(), &|_| {}).await.expect("ok");
        let ev = sink.0.lock().expect("lock");
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].model, "m1");
        assert_eq!(ev[0].task.as_deref(), Some("T01"));
        assert_eq!(ev[0].usage.cached, Some(1));
    }

    #[tokio::test]
    async fn skips_without_usage_or_scope() {
        let sink = Arc::new(MemSink::default());
        let none = Fake(None);
        UsageBackend::new(&none, Some(scope(&sink))).complete(&req()).await.expect("ok");
        let some = Fake(Some(Usage::new(1, 1)));
        UsageBackend::new(&some, None).complete(&req()).await.expect("ok");
        assert!(sink.0.lock().expect("lock").is_empty());
    }
}
