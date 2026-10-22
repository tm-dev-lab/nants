//! Abstração de backend de chat, para que os agentes possam ser testados com um fake.

use std::future::Future;
use std::pin::Pin;

use super::llm_client::{ChatRequest, ChatResponse, LlmClient, StreamDelta};
use super::LlmError;

/// Origem de completions de chat (implementada por [`LlmClient`] e por fakes em testes).
pub trait ChatBackend: Send + Sync {
    fn complete<'a>(
        &'a self,
        req: &'a ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>>;

    /// Como `complete`, entregando deltas de reasoning/texto em `on_delta`. A implementação
    /// padrão não faz streaming: delega a `complete` sem emitir deltas.
    fn complete_stream<'a>(
        &'a self,
        req: &'a ChatRequest,
        on_delta: &'a (dyn Fn(StreamDelta) + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        let _ = on_delta;
        self.complete(req)
    }
}

impl ChatBackend for LlmClient {
    fn complete<'a>(
        &'a self,
        req: &'a ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(self.chat(req))
    }

    fn complete_stream<'a>(
        &'a self,
        req: &'a ChatRequest,
        on_delta: &'a (dyn Fn(StreamDelta) + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(self.chat_stream(req, on_delta))
    }
}
