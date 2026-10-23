//! Registro de uso (tokens) das chamadas ao modelo, por agente/tarefa/spec.
//!
//! Um [`UsageBackend`] envolve um [`ChatBackend`] e entrega cada `Usage` reportado a um
//! [`UsageSink`] injetável; a persistência fica fora dos agentes.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::chat::ChatBackend;
use super::llm_client::{Billing, ChatRequest, ChatResponse, StreamCut, StreamDelta, Usage};
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
    /// Tokens estimados pelo Nants, não informados pelo provedor (spec 114 T05: resposta cortada).
    pub estimated: bool,
    /// Motivo do corte da resposta (`too_large`, `timeout`...), quando a chamada foi cortada.
    pub cut: Option<String>,
    /// Como a chamada é cobrada (spec 116 T02): pelo plano ChatGPT, tokens sem US$.
    pub billing: Billing,
}

/// Diagnóstico de uma resposta cortada, com o contexto do agente (spec 114 T05).
#[derive(Debug, Clone)]
pub struct CutEvent {
    pub agent: String,
    pub model: String,
    pub spec: Option<String>,
    pub task: Option<String>,
    pub cut: Arc<StreamCut>,
}

/// Destino dos registros de uso. Nunca deve falhar para o chamador.
pub trait UsageSink: Send + Sync {
    fn record(&self, event: UsageEvent);
    /// Grava o diagnóstico de uma resposta cortada. Padrão: nada (o custo vai por [`Self::record`]).
    fn record_cut(&self, _event: CutEvent) {}
}

/// Linha do log do servidor para uma resposta cortada: papel, modelo e só números. Os trechos do
/// texto ficam no registro do projeto, nunca no log.
pub fn cut_log_line(agent: &str, model: &str, cut: &StreamCut) -> String {
    let safe = |t: &str| -> String {
        t.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-')).take(80).collect()
    };
    let tokens = match cut.usage() {
        Some((u, true)) => format!("saida_estimada={}", u.completion),
        Some((u, false)) => format!("saida={} raciocinio={}", u.completion, u.reasoning.unwrap_or(0)),
        None => "saida=0".to_string(),
    };
    let top = cut.top_events.first().map(|(k, n)| format!("{k}:{n}")).unwrap_or_default();
    format!(
        "nants: resposta cortada papel={} modelo={} motivo={} tempo_ms={} bytes={} eventos={} {tokens} evento_mais_comum={top}",
        safe(agent),
        safe(model),
        cut.reason,
        cut.elapsed_ms,
        cut.bytes,
        cut.events,
    )
}

/// Orçamento da spec, consultado antes de cada chamada de agente.
pub trait BudgetGuard: Send + Sync {
    /// `Some(motivo)` quando o limite da spec foi atingido, com o motivo no idioma `lang`.
    fn exceeded(&self, spec: &str, lang: crate::i18n::Lang) -> Option<String>;
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
                estimated: false,
                cut: None,
                billing: resp.billing,
            });
        }
    }

    /// Resposta cortada (spec 114 T05): a linha no log, o diagnóstico e o custo conhecido ou
    /// estimado, mesmo sem a resposta final.
    fn note_cut(&self, model: &str, cut: &Arc<StreamCut>) {
        let agent = self.scope.as_ref().map_or("?", |s| s.agent.as_str());
        eprintln!("{}", cut_log_line(agent, model, cut));
        let Some(scope) = &self.scope else {
            return;
        };
        scope.sink.record_cut(CutEvent {
            agent: scope.agent.clone(),
            model: model.to_string(),
            spec: scope.spec.clone(),
            task: scope.task.clone(),
            cut: Arc::clone(cut),
        });
        if let Some((usage, estimated)) = cut.usage() {
            scope.sink.record(UsageEvent {
                agent: scope.agent.clone(),
                model: model.to_string(),
                spec: scope.spec.clone(),
                task: scope.task.clone(),
                usage,
                web_search_calls: 0,
                images: 0,
                image_quality: None,
                estimated,
                cut: Some(cut.reason.to_string()),
                billing: cut.billing,
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
            let wrapped = |d: StreamDelta| {
                if let StreamDelta::Cut(cut) = &d {
                    self.note_cut(&req.model, cut);
                }
                on_delta(d);
            };
            let resp = self.inner.complete_stream(req, &wrapped).await?;
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
                    billing: Default::default(),
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

    /// Spec 116 T02: a cobrança da resposta (plano ChatGPT) chega ao registro.
    #[tokio::test]
    async fn billing_of_the_response_reaches_the_record() {
        struct Plan;
        impl ChatBackend for Plan {
            fn complete<'a>(
                &'a self,
                req: &'a ChatRequest,
            ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
                Box::pin(async move {
                    let mut resp = Fake(Some(Usage::new(5, 6))).complete(req).await?;
                    resp.billing = Billing::ChatgptPlan;
                    Ok(resp)
                })
            }
        }
        let sink = Arc::new(MemSink::default());
        UsageBackend::new(&Plan, Some(scope(&sink))).complete(&req()).await.expect("ok");
        UsageBackend::new(&Fake(Some(Usage::new(1, 1))), Some(scope(&sink))).complete(&req()).await.expect("ok");
        let ev = sink.0.lock().expect("lock");
        assert_eq!(ev[0].billing, Billing::ChatgptPlan);
        assert_eq!(ev[1].billing, Billing::Metered);
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
