//! O lugar da análise (spec 140 T11, G6 e C-13).
//!
//! A análise (o cenário proativo da T12) tem rodadas e leituras, e na fila única da conversa
//! ([`super::turn::TurnQueue`]) ela deixaria a pessoa esperando. Por isso ela roda aqui, à parte:
//!
//! - **Uma por vez, fora da vez da conversa:** o [`Lane`] tem um lugar só, e a análise nunca entra na
//!   fila da conversa. A conversa, o aviso e a fila de saída do Telegram nunca esperam por ela.
//! - **Segunda análise descartada, não enfileirada:** enquanto uma roda, outra pedida volta com
//!   `busy` (registrado). Na fila, ela sairia atrasada, sobre um caso que pode já ter mudado, e
//!   seguraria um lugar enquanto a pessoa decide pelo cartão. Quem chama ([`super::scenario`], T12)
//!   trata o gatilho como estado, reavaliado a cada passagem do observador: com `busy`, o caso fica
//!   pendente e volta quando o lugar se libera. O caso só é marcado como analisado no `Ok` deste
//!   [`spawn`], então nada se repete e nada se perde (G6).
//! - **Fatia do dia (spec 140 T12, C-19):** as análises gastam no máximo uma fração do orçamento do
//!   dia (`[nanty.analysis] budget_pct`), para nunca derrubar a conversa: sem fatia, não começa
//!   (`share`), e a que esgota a fatia no meio para antes da rodada seguinte.
//! - **Nascença (spec 140 T12, C-11):** o [`spawn`] força o modo de análise, a contaminação com o tipo
//!   e o projeto do aviso e a trava nesse projeto ([`Session::pinned`]), venha a sessão como vier.
//! - **Orçamento compartilhado:** gasta do orçamento diário do Nanty ([`super::agent::account`]). Sem
//!   orçamento, não começa; se ele acabar no meio, a análise para antes da rodada seguinte, sem texto.
//!   Prazo, rodadas e tokens próprios, de `[nanty.analysis]` ([`Limits::analysis`]).
//! - **Avatar:** cada turno conta no "pensando" ([`Nanty::thinking`]); o fim da análise não apaga o
//!   "pensando" da conversa, nem o contrário.
//! - **Cancelamento ([`Cancel`]):** a condição se resolveu (o cartão foi decidido, o sinal fechou), e
//!   a análise para: a espera do modelo termina na hora, e nenhuma ferramenta ou resposta segue
//!   depois disso. Nada se grava pela metade (a análise só devolve o texto; quem chama é que o envia),
//!   e o registro leva o código `cancelled`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::agent::{self, Ended, Limits, Rounds, Session};
use super::provenance::TaintKind;
use super::store::Spend;
use super::turn::{self, BudgetMode, TurnKind};
use super::{tools, Nanty};
use crate::agents::llm_client::ChatMessage;
use crate::config::Config;
use crate::server::state::AppState;

// ---- O lugar ----

/// O lugar da análise: um só.
#[derive(Debug, Default)]
pub struct Lane {
    running: Arc<AtomicBool>,
}

/// A vez da análise: solta em qualquer saída (fim, prazo, cancelamento, falha ou tarefa abortada).
#[derive(Debug)]
struct Slot(Arc<AtomicBool>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Lane {
    fn take(&self) -> Option<Slot> {
        self.running.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).ok()?;
        Some(Slot(Arc::clone(&self.running)))
    }

    /// Há uma análise rodando agora.
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

// ---- O cancelamento ----

type Condition = Box<dyn Fn() -> bool + Send + Sync>;

struct CancelInner {
    fired: tokio::sync::watch::Sender<bool>,
    condition: Option<Condition>,
}

/// A condição de cancelamento de uma análise. Dois jeitos, que valem juntos:
/// - [`Cancel::cancel`], de quem vê o sinal fechar: termina até a espera do modelo, na hora;
/// - a condição de [`Cancel::when`] (o cartão já decidido, por exemplo), conferida antes de cada
///   rodada, depois de cada resposta do modelo e antes de cada ferramenta.
#[derive(Clone)]
pub struct Cancel(Arc<CancelInner>);

impl std::fmt::Debug for Cancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cancel").field("cancelled", &self.is_cancelled()).finish_non_exhaustive()
    }
}

impl Default for Cancel {
    fn default() -> Self {
        Self::new()
    }
}

impl Cancel {
    /// Só pelo [`Cancel::cancel`].
    pub fn new() -> Self {
        Self(Arc::new(CancelInner { fired: tokio::sync::watch::channel(false).0, condition: None }))
    }

    /// Também quando `condition` disser que o caso se resolveu.
    pub fn when(condition: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(CancelInner { fired: tokio::sync::watch::channel(false).0, condition: Some(Box::new(condition)) }))
    }

    /// O caso se resolveu: a análise para.
    pub fn cancel(&self) {
        self.0.fired.send_replace(true);
    }

    /// A análise deve parar.
    pub fn is_cancelled(&self) -> bool {
        *self.0.fired.borrow() || self.0.condition.as_ref().is_some_and(|c| c())
    }

    /// Termina quando [`Cancel::cancel`] for chamado.
    pub async fn cancelled(&self) {
        let mut rx = self.0.fired.subscribe();
        // O emissor vive com o `Cancel`: a espera só termina pelo disparo.
        let _ = rx.wait_for(|fired| *fired).await;
    }
}

// ---- A análise ----

/// Uma análise pronta para rodar: o projeto do aviso e o tipo do dado dele que entra (a procedência
/// de nascença, spec 140 T12), o prompt de sistema, a entrada depois dele, a sessão no modo de
/// análise ([`Session::analysis`]), a condição de cancelamento e os limites ([`Limits::analysis`]
/// por padrão).
#[derive(Debug)]
pub struct Job {
    pub project_id: String,
    pub taint: TaintKind,
    pub system: String,
    pub context: Vec<ChatMessage>,
    pub session: Session,
    pub cancel: Cancel,
    pub limits: Limits,
}

/// O projeto do aviso e o tipo do dado dele que entra na análise.
#[derive(Debug, Clone)]
pub struct Origin {
    pub project_id: String,
    pub taint: TaintKind,
}

impl Job {
    /// Com os limites de `[nanty.analysis]`.
    pub fn new(cfg: &Config, origin: Origin, system: String, context: Vec<ChatMessage>, session: Session, cancel: Cancel) -> Self {
        Self { project_id: origin.project_id, taint: origin.taint, system, context, session, cancel, limits: Limits::analysis(cfg) }
    }
}

/// Ainda há fatia do orçamento do dia para as análises (spec 140 T12, C-19): o que elas gastaram fica
/// abaixo de `[nanty.analysis] budget_pct` do orçamento, na mesma medida do orçamento (US$ ou tokens).
pub fn share_left(cfg: &Config, spend: &Spend) -> bool {
    let view = turn::budget_view(cfg, spend);
    let share = cfg.nanty.analysis.budget_share();
    match view.mode {
        BudgetMode::Usd => spend.analysis_usd < view.budget_usd * share,
        BudgetMode::Tokens => (spend.analysis_tokens as f64) < view.budget_tokens as f64 * share,
    }
}

/// O fim de uma análise: como terminou ([`Ended`]: o texto, ou o motivo em código, entre eles
/// `cancelled`, `deadline`, `budget`, `rounds` e `failed`) e a sessão, com o que as ferramentas leram
/// e a procedência do texto.
#[derive(Debug)]
pub struct Done {
    pub ended: Ended,
    pub session: Session,
}

/// Começa a análise no lugar dela, numa tarefa própria, e devolve na hora: quem chama (o aviso) nunca
/// espera. `Err` com o motivo em código quando ela não começa: `no_model`, `budget` (sem orçamento no
/// dia), `share` (as análises já gastaram a fatia delas no dia), `busy` (outra análise rodando: esta é
/// descartada), `cancelled` (o caso já se resolveu) ou `failed` (sem cliente do modelo). Cada recusa
/// fica no registro, fora `no_model`.
pub fn spawn(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, job: Job) -> Result<tokio::task::JoinHandle<Done>, &'static str> {
    let refuse = |reason: &'static str| {
        let _ = turn::skipped(nanty, TurnKind::Analysis, reason);
        reason
    };
    let Some((conn, model)) = turn::model_route(cfg).map(|(c, m)| (c.clone(), m.to_string())) else {
        return Err(refuse("no_model"));
    };
    let spend = nanty.snapshot().spend_on(&nanty.now().day_key());
    if turn::budget_view(cfg, &spend).exhausted {
        return Err(refuse("budget"));
    }
    if !share_left(cfg, &spend) {
        return Err(refuse("share"));
    }
    if job.cancel.is_cancelled() {
        return Err(refuse("cancelled"));
    }
    let Some(slot) = nanty.analysis().take() else {
        return Err(refuse("busy"));
    };
    let Some(backend) = agent::backend(state, nanty, &conn) else {
        return Err(refuse("failed"));
    };
    let (state, nanty, cfg) = (state.clone(), Arc::clone(nanty), cfg.clone());
    Ok(tokio::spawn(async move {
        let _slot = slot;
        let Job { project_id, taint, system, context, mut session, cancel, limits } = job;
        // Só as ferramentas da análise, venha a sessão como vier (spec 140 T04, C-06).
        session.mode = tools::Mode::Analysis;
        // Spec 140 T12 (C-11): nasce contaminada pelo projeto do aviso e presa a ele. O projeto conta
        // como já aberto e visto: outro nunca é admitido (C-16).
        session.tainted.add(taint, Some(&project_id));
        session.opened = vec![project_id.clone()];
        session.opened_seen = true;
        session.pinned = Some(project_id);
        let rounds = Rounds { kind: TurnKind::Analysis, conn: &conn, model: &model, backend, limits, cancel: Some(&cancel) };
        let ended = rounds.run(&state, &nanty, &cfg, system, context, &mut session).await;
        Done { ended, session }
    }))
}

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
