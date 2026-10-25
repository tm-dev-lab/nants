//! Árbitro de produção das contestações (spec 124 T02): o modelo do Arquiteto, em turno somente
//! leitura com o código estruturado `arbitration` na Atividade. O runner monta os FATOS, valida o
//! JSON e grava a decisão; aqui só se chama o turno, com tempo máximo e respeitando a parada.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::agents::activity::ActivitySink;
use crate::agents::architect::LlmArchitect;
use crate::agents::chat::ChatBackend;
use crate::agents::runner::{ArbiterPort, Fut, RunnerError};
use crate::agents::usage::UsageSink;
use crate::config::Effort;
use crate::harness::allowlist::Policy;
use crate::harness::tools::Approver;
use crate::server::events::SecretSet;

/// Tempo máximo do turno de arbitragem.
const ARBITRATION_TIMEOUT: Duration = Duration::from_secs(300);
/// Papel do erro devolvido ao runner.
const ROLE: &str = "architect";

/// Turno de arbitragem pelo modelo do Arquiteto.
pub struct ArchitectArbiter {
    pub backend: Arc<dyn ChatBackend>,
    pub model: String,
    pub effort: Option<Effort>,
    pub policy: Policy,
    pub approver: Arc<dyn Approver>,
    pub activity: Arc<dyn ActivitySink>,
    pub usage: Option<Arc<dyn UsageSink>>,
    pub secrets: Arc<SecretSet>,
    pub lang: crate::i18n::SharedLang,
    /// Parada pedida pelo usuário: abandona o turno em curso.
    pub stop: Option<watch::Receiver<bool>>,
}

impl ArchitectArbiter {
    fn architect(&self, spec_id: &str, task_id: &str) -> LlmArchitect {
        let architect = LlmArchitect::new(Arc::clone(&self.backend), self.model.clone(), self.effort, self.policy.clone(), Arc::clone(&self.approver))
            .with_lang(self.lang.clone())
            .with_activity(Arc::clone(&self.activity), Some(spec_id.to_string()), Some(task_id.to_string()))
            .with_memory(Arc::clone(&self.secrets));
        match &self.usage {
            Some(sink) => architect.with_usage(Arc::clone(sink)),
            None => architect,
        }
    }
}

impl ArbiterPort for ArchitectArbiter {
    fn arbitrate<'a>(&'a self, spec_id: &'a str, task_id: &'a str, root: &'a Path, facts: &'a str) -> Fut<'a, Result<String, RunnerError>> {
        Box::pin(async move {
            let architect = self.architect(spec_id, task_id);
            let turn = architect.arbitrate(root, facts);
            let failed = |message: &str| RunnerError::Port { role: ROLE, message: message.to_string() };
            tokio::select! {
                () = super::approvals::wait_stop(self.stop.clone()) => Err(RunnerError::Cancelled),
                done = tokio::time::timeout(ARBITRATION_TIMEOUT, turn) => match done {
                    Ok(Ok(text)) => Ok(text),
                    Ok(Err(_)) => Err(failed("arbitragem indisponível")),
                    Err(_) => Err(failed("tempo esgotado")),
                },
            }
        })
    }
}
