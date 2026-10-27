//! Agentes LLM: cliente HTTP e tipos compartilhados.

pub mod activity;
pub mod arch_memory;
pub mod architect;
pub mod chat;
pub mod finding_reply;
pub mod finding_track;
pub mod llm_agents;
pub mod llm_client;
pub mod markers;
pub mod plan_review;
pub mod recovery_plan;
pub mod prompts;
pub mod runner;
pub mod spill;
pub mod summary;
pub mod usage;

/// Erros do cliente LLM.
///
/// `body_snippet` nunca contém a chave de API: o cliente a redige antes de armazenar. `code` é o
/// `error.code`/`error.type` saneado do corpo; só o caminho de imagem o preenche (spec 091).
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("HTTP {status}: {body_snippet}")]
    Http { status: u16, body_snippet: String, code: Option<llm_client::ProviderCode> },
    #[error("falha de transporte: {0}")]
    Transport(String),
    /// Tempo limite da requisição excedido (sem detalhes de URL).
    #[error("tempo limite da requisição excedido")]
    Timeout,
    /// Resposta ruim do provedor, com motivo de vocabulário fechado (spec 088).
    #[error("resposta inválida: {0}")]
    Parse(llm_client::BadReason),
    #[error("configuração inválida: {0}")]
    Config(String),
    /// Falha da conexão pelo plano ChatGPT que pausa a execução (spec 116 T02): limite de uso,
    /// conta não elegível ou precisa entrar de novo. Não se repete.
    #[error("plano ChatGPT: {}", .0.as_str())]
    Plan(llm_client::PlanError),
}
