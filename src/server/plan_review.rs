//! Revisão do plano pelo Revisor antes da aprovação (spec 086 T08).
//!
//! Fluxo, depois de um turno do chat que alterou artefatos (nunca bloqueia o turno):
//! 1. sem o papel `reviewer` configurado, ou sem exclude local garantido (`.git` gitfile ou
//!    symlink: o resultado nunca seria gravado), nada acontece;
//! 2. para cada spec em `AWAITING_APPROVAL`, calcula a assinatura (SHA-256 do `spec.md` e das
//!    `tasks/*.md`); se o `plan-review.json` já é dessa assinatura, ou já há revisão em curso da
//!    spec, não dispara outra;
//! 3. a revisão roda em task própria: o servidor monta o plano (redigido, depois limitado), o
//!    [`PlanReviewer`] responde o JSON e o servidor valida, redige e grava
//!    `.sdd/specs/<spec>/plan-review.json` (`{signature, at, status, issues, rounds, outcome}`; escrita atômica, sem
//!    seguir symlink, local via `.git/info/exclude`);
//! 4. com apontamentos, o ciclo de consenso (spec 128 T02, [`super::plan_consensus`]): o Arquiteto
//!    recebe um turno interno com cada apontamento identificado (`P1`, `P2`…) e responde uma decisão
//!    por apontamento; o Revisor revisa a nova versão com a memória da rodada anterior. Um ponto
//!    recusado que volta sem argumento novo fica "em disputa" e não volta ao Arquiteto. O ciclo
//!    termina sem ponto em aberto, depois de [`MAX_CONSENSUS_ROUNDS`] rodadas de ajuste, numa
//!    pergunta ao usuário, numa falha ou em shutdown; uma mensagem do usuário zera o contador. Se o
//!    plano mudou durante a revisão, revisa de novo a versão nova (uma revisão por vez);
//! 5. ao concluir (com ou sem gravação; só o shutdown não publica), publica `sdd_draft_updated`
//!    para a UI recarregar e sair de "revisando…".
//!
//! JSON inválido, falha do provedor ou tempo esgotado viram `status: "unavailable"`. O estado do
//! projeto expõe `plan_review` só quando a assinatura gravada bate com o conteúdo atual, ou
//! `status: "running"` enquanto a revisão roda. Com o Revisor configurado e sem resultado nem
//! revisão em curso da versão atual (spec 092 T02), expõe `pending` até [`PENDING_WINDOW`] depois
//! da última alteração do plano, e `stale` depois disso (a UI libera a aprovação com aviso).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use super::approvals::ProjectApprover;
use super::chat::load_policy;
use super::events::{ApiEvent, BusActivitySink, EventType, SecretSet};
use super::plan_consensus::{
    infer_adjusted, issue_line, parse_decisions, reviewer_memory, ConsensusRound, Cycle, PlanDecision, PlanPoint, ReviewItem, CONSENSUS_STAGE,
    DECISION_ACCEPT, DECISION_REJECT, MAX_CONSENSUS_ROUNDS, OUTCOME_AWAITING_USER, OUTCOME_CONSENSUS, OUTCOME_DISPUTED,
    OUTCOME_INTERRUPTED, OUTCOME_OPEN, OUTCOME_SHUTDOWN, OUTCOME_UNAVAILABLE,
};
use super::projects::{ensure_local_excludes, format_utc, real_children, scrub_text};
use super::report::{exclude_guaranteed, wait_shutdown};
use super::safe_fs::{self, MAX_READ_BYTES, O_NOFOLLOW};
use super::spec_result::validate_spec_id;
use super::state::AppState;
use crate::agents::architect::truncate_bytes;
use crate::agents::chat::ChatBackend;
use crate::agents::plan_review::PlanReviewer;
use crate::agents::recovery_plan::{count_criteria, criteria_weight, CRITERION_WORDS, MANY_CRITERIA};
use crate::agents::runner::{section_text, ACCEPTANCE};
use crate::config::Effort;
use crate::sdd::capabilities;
use crate::sdd::environment::sha256_hex;
use crate::sdd::{Spec, SpecStatus};

/// Nome do arquivo do resultado, dentro de `.sdd/specs/<id>/`.
pub const PLAN_REVIEW_FILE: &str = "plan-review.json";
/// Linhas do `.git/info/exclude`: o resultado é local (não suja a base nem entra no plano).
pub const PLAN_REVIEW_EXCLUDE_LINE: &str = ".sdd/specs/*/plan-review.json";
pub const PLAN_REVIEW_TMP_EXCLUDE_LINE: &str = ".sdd/specs/*/.plan-review.json.*.tmp";
/// Rótulo da atividade ao vivo da revisão.
pub const PLAN_REVIEW_STAGE: &str = "Revisor revisando o plano";
/// Máximo de apontamentos guardados e expostos.
pub const MAX_ISSUES: usize = 8;
/// Teto do texto de cada apontamento (bytes, depois da redação).
pub const MAX_ISSUE_TEXT_BYTES: usize = 300;
/// Tipos aceitos de apontamento.
pub const ISSUE_KINDS: [&str; 6] = ["contradiction", "untestable", "scope", "uncovered_criterion", "missing_dependency", TOO_LARGE];
/// Tarefa grande demais para uma revisão (spec 124 T06): o modelo aponta as áreas misturadas e o
/// servidor completa, de forma determinística, as de muitos critérios ([`MANY_CRITERIA`]).
pub const TOO_LARGE: &str = "too_large";

pub const STATUS_OK: &str = "ok";
pub const STATUS_UNAVAILABLE: &str = "unavailable";
pub const STATUS_RUNNING: &str = "running";
/// Versão atual sem resultado e sem revisão em curso, alterada há menos de [`PENDING_WINDOW`].
pub const STATUS_PENDING: &str = "pending";
/// Versão atual sem resultado depois de [`PENDING_WINDOW`]: a aprovação libera com aviso.
pub const STATUS_STALE: &str = "stale";
/// Spec em aprovação sem nenhuma tarefa (spec 127 T01: o turno que escrevia o plano caiu): a
/// revisão não roda e a UI oferece pedir ao Arquiteto para terminar. A aprovação segue bloqueada.
pub const STATUS_INCOMPLETE: &str = "incomplete";
/// Quanto a UI espera a revisão da versão atual antes de liberar a aprovação.
pub const PENDING_WINDOW: Duration = Duration::from_secs(180);
/// `detail` do `unavailable` quando a revisão estourou o tempo ou a resposta não era o JSON.
pub const DETAIL_TIMEOUT: &str = "tempo esgotado";
pub const DETAIL_INVALID_JSON: &str = "JSON inválido";

const SPECS_REL: &str = ".sdd/specs";
/// Limites do plano enviado ao modelo.
const MAX_SPEC_BYTES: usize = 24 * 1024;
const MAX_TASK_BYTES: usize = 8 * 1024;
const MAX_PLAN_BYTES: usize = 64 * 1024;
const MAX_TASKS: usize = 50;
/// O registro guarda as rodadas e os pontos do ciclo de consenso (spec 128 T02).
const MAX_RECORD_BYTES: u64 = 256 * 1024;
const MAX_TASK_ID_BYTES: usize = 16;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Revisões em curso, por `<raiz>\0<spec>`. Uma por spec; a vaga sai no [`Claim::finish`] ou no
/// `Drop` do [`Claim`]. Cada vaga guarda o progresso do ciclo (para a tela) e o pedido de nova
/// conferência de quem não conseguiu a vaga.
static RUNNING: Mutex<Option<HashMap<String, Slot>>> = Mutex::new(None);

/// Vaga de uma revisão em curso.
#[derive(Debug, Clone, Default)]
struct Slot {
    progress: Progress,
    /// Um turno pediu a revisão enquanto esta rodava: antes de sair, o ciclo confere de novo.
    recheck: bool,
}

/// Progresso do ciclo de consenso exposto enquanto ele roda (spec 128 T02).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Progress {
    round: u32,
    phase: Option<&'static str>,
    evaluating: usize,
}

/// Fases do ciclo em curso, para a tela.
pub const PHASE_REVIEW: &str = "review";
pub const PHASE_ARCHITECT: &str = "architect";

fn running_key(root: &Path, spec_id: &str) -> String {
    format!("{}\0{spec_id}", root.display())
}

fn with_running<T>(f: impl FnOnce(&mut HashMap<String, Slot>) -> T) -> T {
    let mut guard = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(HashMap::new))
}

/// Vaga de revisão em curso de uma spec (liberada em [`Claim::finish`] ou no `Drop`, inclusive em
/// pânico).
struct Claim {
    key: String,
    released: AtomicBool,
}

impl Claim {
    /// Reserva a vaga. Ocupada, marca o pedido de nova conferência para quem a ocupa e devolve `None`.
    fn take(root: &Path, spec_id: &str) -> Option<Self> {
        let key = running_key(root, spec_id);
        with_running(|map| {
            if let Some(slot) = map.get_mut(&key) {
                slot.recheck = true;
                return None;
            }
            map.insert(key.clone(), Slot::default());
            Some(Self { key, released: AtomicBool::new(false) })
        })
    }

    /// Tenta sair: com pedido de nova conferência pendente, consome o pedido e devolve `false` (o
    /// ciclo confere de novo); senão libera a vaga e devolve `true`. Atômico com [`Claim::take`]:
    /// nenhum pedido se perde entre a última conferência e a saída.
    fn finish(&self) -> bool {
        with_running(|map| match map.get_mut(&self.key) {
            Some(slot) if slot.recheck => {
                slot.recheck = false;
                false
            }
            _ => {
                map.remove(&self.key);
                self.released.store(true, Ordering::SeqCst);
                true
            }
        })
    }

    fn progress(&self, round: u32, phase: &'static str, evaluating: usize) {
        with_running(|map| {
            if let Some(slot) = map.get_mut(&self.key) {
                slot.progress = Progress { round, phase: Some(phase), evaluating };
            }
        });
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if !self.released.load(Ordering::SeqCst) {
            with_running(|map| {
                map.remove(&self.key);
            });
        }
    }
}

/// Progresso da revisão em curso da spec; `None` sem revisão em curso.
fn running_progress(root: &Path, spec_id: &str) -> Option<Progress> {
    with_running(|map| map.get(&running_key(root, spec_id)).map(|s| s.progress))
}

/// `true` com uma revisão do plano da spec em curso (spec 128 T04: a aprovação espera por ela).
pub fn review_running(root: &Path, spec_id: &str) -> bool {
    running_progress(root, spec_id).is_some()
}

/// Ocupa a vaga de revisão da spec enquanto o valor viver (testes de outros módulos).
#[cfg(test)]
pub(super) fn hold_review_for_test(root: &Path, spec_id: &str) -> Option<impl Sized> {
    Claim::take(root, spec_id)
}

#[cfg(test)]
fn is_running(root: &Path, spec_id: &str) -> bool {
    running_progress(root, spec_id).is_some()
}

/// Apontamento da revisão do plano.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanIssue {
    /// Id estável do ponto na spec (`P1`, spec 128 T02); ausente nos registros antigos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Tarefa apontada (`T02`), ou `None` quando o problema é da spec.
    pub task: Option<String>,
    pub kind: String,
    pub text: String,
    /// Repete um ponto recusado pelo Arquiteto sem argumento novo: não volta a ele, e o usuário vê
    /// as duas posições.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disputed: bool,
}

impl PlanIssue {
    pub fn new(task: Option<String>, kind: &str, text: &str) -> Self {
        Self { id: None, task, kind: kind.to_string(), text: text.to_string(), disputed: false }
    }
}

/// Conteúdo de `plan-review.json`. Registros antigos (com `split_requested`, da spec 126 T03)
/// continuam lidos: os campos novos têm padrão e os desconhecidos são ignorados.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanReviewRecord {
    pub signature: String,
    pub at: String,
    pub status: String,
    pub issues: Vec<PlanIssue>,
    /// Só com `unavailable` (spec 088): código fixo da falha do provedor com o motivo
    /// (`provider_bad_response (incompleta: content_filter)`), `tempo esgotado` ou `JSON inválido`.
    /// Vocabulário fixo; nunca texto remoto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Só com `unavailable` por falha do provedor (spec 133 T11): os `params` da evidência (a causa
    /// fixa, o papel, a conexão, o modelo, a categoria, o status e os códigos do provedor). Nunca a
    /// mensagem do provedor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<serde_json::Map<String, Value>>,
    /// Rodadas do ciclo de consenso corrente (spec 128 T02): apontamentos, decisões e motivos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rounds: Vec<ConsensusRound>,
    /// Desfecho do ciclo (`consensus`, `disputed`, `open`, `awaiting_user`, `unavailable`,
    /// `interrupted`, `shutdown`); ausente enquanto o ciclo segue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Registro dos pontos da spec, com ids estáveis e a última decisão do Arquiteto. Passa de um
    /// registro para o seguinte, mesmo com outra assinatura.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<PlanPoint>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub next_point: u32,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl PlanReviewRecord {
    /// Registro da versão `signature` com o estado do ciclo.
    fn of_cycle(signature: String, status: &str, issues: Vec<PlanIssue>, detail: Option<String>, outcome: Option<&str>, cycle: &Cycle) -> Self {
        Self {
            signature,
            at: format_utc(unix_now()),
            status: status.to_string(),
            issues,
            detail,
            failure: None,
            rounds: cycle.rounds.clone(),
            outcome: outcome.map(str::to_string),
            points: cycle.points.clone(),
            next_point: cycle.next_point,
        }
    }
}

/// `plan_review` exposto no estado do projeto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanReviewView {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub issues: Vec<PlanIssue>,
    /// Ciclo de consenso (spec 128 T02), para a tela da T05; ausente nos registros antigos.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consensus: Option<ConsensusView>,
    /// Revisão indisponível por falha do provedor (spec 133 T11): os `params` da evidência, para a
    /// tela dizer o papel, a conexão, o modelo e o motivo traduzido.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<serde_json::Map<String, Value>>,
}

/// Estado do ciclo de consenso exposto: rodada, fase, resumo e os pontos em aberto, em disputa e
/// mantidos com o motivo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ConsensusView {
    /// Rodada de ajuste corrente (0 = revisão inicial).
    pub round: u32,
    pub max_rounds: u32,
    /// Enquanto roda: `review` (Revisor conferindo) ou `architect` (Arquiteto avaliando).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<&'static str>,
    /// Apontamentos que o Arquiteto avalia agora (fase `architect`).
    pub evaluating: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Totais do ciclo, por ponto distinto e pela decisão final: ajustados (aceitos, fora dos que
    /// esperam o usuário), mantidos (`kept_points`) e as respostas que faltaram nas rodadas.
    pub adjusted: u32,
    pub kept: u32,
    pub unanswered: u32,
    /// Pontos da última revisão que seguem em aberto (os recusados vão em `kept_points`).
    pub open: Vec<PointView>,
    /// Pontos em disputa: o texto do Revisor e a posição do Arquiteto.
    pub disputed: Vec<PointView>,
    /// Pontos recusados no ciclo fora de disputa, com o motivo.
    pub kept_points: Vec<PointView>,
    /// Recusados que o Revisor reabriu com argumento novo na última revisão, sem nova decisão do
    /// Arquiteto depois dela (ex.: o ciclo parou no teto): em aberto, com as duas posições (o texto
    /// novo do Revisor e o motivo da recusa). Não entram em `kept_points` nem em `open`.
    pub reopened: Vec<PointView>,
    /// Apontamentos do resumo: `adjusted + kept + open + reopened + disputed`.
    pub total: u32,
    /// Tamanho do plano na primeira revisão do ciclo e na última (C-10).
    pub tasks_first: u32,
    pub criteria_first: u32,
    pub tasks: u32,
    pub criteria: u32,
}

/// Um ponto na vista do consenso.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PointView {
    pub id: String,
    pub task: Option<String>,
    pub kind: String,
    /// Texto do Revisor.
    pub text: String,
    /// Última decisão do Arquiteto no ciclo (`accept` ou `reject`); ausente sem decisão.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_kind: Option<String>,
    /// Posição do Arquiteto (motivo).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Configuração do Revisor para a revisão do plano. `None` sem o papel configurado.
struct ReviewerSetup {
    model: String,
    effort: Effort,
    backend: Arc<dyn ChatBackend>,
    /// O papel como foi montado (spec 133 T01), para a evidência de uma falha.
    stage: super::provider_errors::StageSetup,
}

async fn reviewer_setup(state: &AppState) -> Option<ReviewerSetup> {
    let (agent, connection) = {
        let cfg = state.config().read().await;
        let agent = cfg.agents.reviewer.clone();
        let connection = cfg.connections.iter().find(|c| c.id == agent.connection_id).cloned();
        (agent, connection)
    };
    let connection = connection?;
    if agent.model.trim().is_empty() {
        return None;
    }
    let client = super::chatgpt::plan_client::llm_client(state, &connection).ok()?;
    let backend: Arc<dyn ChatBackend> = match state.report_backend() {
        Some(backend) => backend,
        None => Arc::new(client),
    };
    let stage = super::provider_errors::StageSetup {
        role: "reviewer",
        model: agent.model.clone(),
        connection: Some(super::provider_errors::connection_ref(&connection)),
    };
    Some(ReviewerSetup { model: agent.model, effort: agent.effort, backend, stage })
}

/// `true` quando o papel `reviewer` está configurado (a revisão do plano vai rodar).
pub async fn reviewer_configured(state: &AppState) -> bool {
    reviewer_setup(state).await.is_some()
}

/// Gatilho depois de um turno do chat que alterou artefatos. A parte síncrona é curta (lê as
/// specs e reserva a vaga); a revisão roda em segundo plano. Devolve as tasks disparadas (os
/// testes as aguardam; o chat as ignora). Spec com revisão em curso não ganha outra: a que roda
/// confere de novo antes de sair.
pub async fn after_chat_turn(state: &AppState, project_id: &str, root: PathBuf) -> Vec<JoinHandle<()>> {
    let Some(setup) = reviewer_setup(state).await else {
        return Vec::new();
    };
    // Sem exclude local garantido (gitfile de worktree, symlink) o resultado nunca seria gravado:
    // recusa antes do modelo, sem vaga e sem chamada (cada edição pagaria uma revisão descartada).
    let check_root = root.clone();
    if !tokio::task::spawn_blocking(move || exclude_guaranteed(&check_root)).await.unwrap_or(false) {
        return Vec::new();
    }
    let setup = Arc::new(setup);
    let scan_root = root.clone();
    let claims = tokio::task::spawn_blocking(move || {
        awaiting_specs(&scan_root)
            .into_iter()
            .filter(|spec_id| needs_review(&scan_root, spec_id))
            .filter_map(|spec_id| Claim::take(&scan_root, &spec_id).map(|c| (spec_id, c)))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    claims
        .into_iter()
        .map(|(spec_id, claim)| {
            let (state, project_id, root, setup) = (state.clone(), project_id.to_string(), root.clone(), Arc::clone(&setup));
            tokio::spawn(async move {
                let _active = super::runs::ActiveRun::enter();
                let outcome = review_loop(&state, &project_id, &root, &spec_id, &setup, &claim).await;
                drop(claim);
                // Todo desfecho, menos o shutdown, recarrega a UI: ela viu `running` e precisa sair dele.
                if outcome != Stop::Shutdown {
                    state.events().publish(ApiEvent::new(
                        EventType::SddDraftUpdated,
                        Some(project_id.clone()),
                        json!({"project_id": project_id}),
                    ));
                }
            })
        })
        .collect()
}

/// Por que a revisão parou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// A versão atual tem resultado (ou não há mais o que revisar).
    Done,
    /// Plano ilegível ou falha de gravação: nada gravado.
    Failed,
    /// Shutdown do servidor: nada mais é publicado.
    Shutdown,
}

/// O que o ciclo guarda entre as rodadas, além do [`Cycle`], para a memória do Revisor.
#[derive(Debug, Default)]
struct Memory {
    /// Tarefas da revisão anterior (para listar as novas).
    tasks: Option<BTreeSet<String>>,
    /// Linhas do `plan_changes` gravadas pelos turnos desde a revisão anterior.
    changes: Vec<String>,
}

/// Memória entregue a uma revisão: os apontamentos da rodada anterior, o que mudou e as tarefas
/// daquela revisão.
type Memo = (Vec<PlanIssue>, Vec<String>, Option<BTreeSet<String>>);

/// Ciclo de consenso (spec 128 T02): revisa enquanto a versão atual do plano não tem resultado e,
/// com apontamentos em aberto, dá ao Arquiteto uma rodada de ajuste, até o consenso, o teto de
/// [`MAX_CONSENSUS_ROUNDS`], uma pergunta ao usuário, uma falha ou o shutdown. A vaga continua
/// reservada durante o ciclo: a UI segue em "revisando" e a aprovação espera.
async fn review_loop(state: &AppState, project_id: &str, root: &Path, spec_id: &str, setup: &ReviewerSetup, claim: &Claim) -> Stop {
    let runtime = state.plan_consensus();
    let (prev_root, sid) = (root.to_path_buf(), spec_id.to_string());
    let previous = tokio::task::spawn_blocking(move || read_record(&prev_root, &sid)).await.ok().flatten();
    // As rodadas do registro são de um ciclo sem mensagem do usuário desde então: o contador continua.
    let gen = runtime.user_gen(project_id);
    let resume = runtime.cycle_gen(project_id, spec_id) == Some(gen);
    let mut cycle = match previous {
        Some(prev) => {
            let mut cycle = Cycle::start(prev.points, prev.next_point, gen);
            // Sem mensagem do usuário desde o último ciclo da spec, o contador continua.
            if resume {
                cycle.resume(prev.rounds);
            }
            cycle
        }
        None => Cycle::start(Vec::new(), 0, gen),
    };
    let mut memory = Memory::default();
    // Depois de cada turno que respondeu, o Revisor revisa de novo, mesmo sem mudança no plano
    // (todas as recusas): ele vê as decisões e fecha, mantém em disputa ou traz argumento novo.
    let mut force = false;
    // Spec 131 T03: uma rodada de ajuste mudou o plano desde a última lista final.
    let mut adjusted = false;
    loop {
        // Mensagem do usuário durante o ciclo (C-11): o ciclo em curso termina como interrompido e
        // o contador zera.
        let gen = runtime.user_gen(project_id);
        if gen != cycle.gen {
            if !cycle.rounds.is_empty() {
                mark_outcome(root, spec_id, OUTCOME_INTERRUPTED, true).await;
            }
            cycle.reset(gen);
            memory = Memory::default();
            force = false;
            adjusted = false;
        }
        // Mensagem do usuário em andamento: a revisão da versão final roda no fim dela. A marca vem
        // antes da conferência: se o turno acabar entre as duas, o fim dele já a vê.
        runtime.defer(project_id);
        if runtime.user_turn_active(project_id) {
            if claim.finish() {
                return Stop::Done;
            }
            continue;
        }
        runtime.take_deferred(project_id);
        let (check_root, sid, forced) = (root.to_path_buf(), spec_id.to_string(), force);
        let pending = tokio::task::spawn_blocking(move || {
            awaiting(&check_root, &sid) && (needs_review(&check_root, &sid) || (forced && reviewable(&check_root, &sid)))
        })
        .await
        .unwrap_or(false);
        force = false;
        if !pending {
            if claim.finish() {
                return Stop::Done;
            }
            continue;
        }
        let round = cycle.round();
        claim.progress(round, PHASE_REVIEW, 0);
        let memo: Option<Memo> = (round > 0).then(|| {
            let previous = cycle
                .rounds
                .iter()
                .rev()
                .find(|r| r.round < round && r.reviewed_at.is_some())
                .map(|r| r.issues.clone())
                .unwrap_or_default();
            (previous, memory.changes.clone(), memory.tasks.clone())
        });
        let reviewed = match review_once(state, project_id, root, spec_id, setup, round, memo.as_ref(), &cycle).await {
            Ok(reviewed) => reviewed,
            Err(stop) => {
                if stop == Stop::Shutdown {
                    mark_outcome(root, spec_id, OUTCOME_SHUTDOWN, false).await;
                }
                return stop;
            }
        };
        memory.tasks = Some(reviewed.sizes.iter().map(|s| s.id.clone()).collect());
        memory.changes.clear();
        let items = match reviewed.items {
            Ok(items) => items,
            Err(detail) => {
                let record = PlanReviewRecord {
                    failure: reviewed.failure,
                    ..PlanReviewRecord::of_cycle(reviewed.signature, STATUS_UNAVAILABLE, Vec::new(), Some(detail), Some(OUTCOME_UNAVAILABLE), &cycle)
                };
                if !save(root, spec_id, record).await {
                    return Stop::Failed;
                }
                runtime.set_cycle_gen(project_id, spec_id, cycle.gen);
                continue;
            }
        };
        let issues = cycle.classify(items);
        let entry = cycle.round_entry(round);
        entry.reviewed_at = Some(format_utc(unix_now()));
        entry.issues = issues.clone();
        entry.tasks = u32::try_from(reviewed.sizes.len()).unwrap_or(u32::MAX);
        entry.criteria = u32::try_from(reviewed.sizes.iter().map(|s| s.criteria).sum::<usize>()).unwrap_or(u32::MAX);
        let open: Vec<PlanIssue> = issues.iter().filter(|i| !i.disputed).cloned().collect();
        let outcome = if open.is_empty() {
            Some(if issues.is_empty() { OUTCOME_CONSENSUS } else { OUTCOME_DISPUTED })
        } else if let Some(halt) = cycle.halt {
            Some(halt)
        } else if runtime.user_gen(project_id) != cycle.gen || runtime.user_turn_active(project_id) {
            Some(OUTCOME_INTERRUPTED)
        } else if cycle.turns >= MAX_CONSENSUS_ROUNDS {
            Some(OUTCOME_OPEN)
        } else {
            None
        };
        let record = PlanReviewRecord::of_cycle(reviewed.signature, STATUS_OK, issues, None, outcome, &cycle);
        if !save(root, spec_id, record.clone()).await {
            return Stop::Failed;
        }
        // As rodadas gravadas são deste ciclo: um ciclo novo sem mensagem do usuário as retoma.
        runtime.set_cycle_gen(project_id, spec_id, cycle.gen);
        if outcome.is_some() {
            // Spec 131 T03 (critério 3): depois do consenso, a lista final das tarefas na Conversa.
            if adjusted && matches!(outcome, Some(OUTCOME_CONSENSUS | OUTCOME_DISPUTED)) {
                adjusted = false;
                final_plan_line(state, project_id, root, spec_id).await;
            }
            continue;
        }
        let (sig_root, sid) = (root.to_path_buf(), spec_id.to_string());
        let signed = record.signature.clone();
        match adjust(state, project_id, root, spec_id, claim, &mut cycle, &mut memory, open, record).await {
            Ok(review_again) => {
                force = review_again;
                // F-03: a lista final só quando a rodada mudou o plano (só recusas não mudam).
                let now = tokio::task::spawn_blocking(move || plan_signature(&sig_root, &sid)).await.ok().flatten();
                adjusted |= review_again && now.is_some_and(|sig| sig != signed);
            }
            Err(stop) => return stop,
        }
    }
}

/// Como terminou um turno interno do consenso.
enum TurnEnd {
    /// O turno respondeu: as decisões válidas, se terminou numa pergunta ao usuário, se trouxe o
    /// bloco (em qualquer mensagem do turno) e o texto final mostrado.
    Done { decisions: HashMap<String, PlanDecision>, asks_user: bool, block: bool, reply: String },
    /// O turno nem começou: `busy` quando outro turno ocupava o Arquiteto.
    NotStarted { busy: bool },
    /// Falha do provedor ou do turno.
    Failed,
    Shutdown,
}

/// Uma rodada de ajuste: o turno interno do Arquiteto com os apontamentos `open` e, se ele criou
/// ou alterou tarefas que já nascem grandes demais, uma volta a mais na mesma rodada (C-04). Grava
/// as decisões, a linha da Conversa e, quando o ciclo para, o desfecho no registro `record` (da
/// versão revisada). `Ok(true)` quando o turno respondeu e a versão deve ser revisada de novo, mesmo
/// sem mudança no plano; `Err` só no shutdown.
#[allow(clippy::too_many_arguments)]
async fn adjust(
    state: &AppState,
    project_id: &str,
    root: &Path,
    spec_id: &str,
    claim: &Claim,
    cycle: &mut Cycle,
    memory: &mut Memory,
    open: Vec<PlanIssue>,
    record: PlanReviewRecord,
) -> Result<bool, Stop> {
    let runtime = state.plan_consensus();
    // Mensagem do usuário chegou durante a revisão: o turno pendente não é disparado (C-11).
    if runtime.user_gen(project_id) != cycle.gen || runtime.user_turn_active(project_id) {
        halt(root, spec_id, cycle, &record, OUTCOME_INTERRUPTED).await;
        return Ok(false);
    }
    cycle.turns += 1;
    let round = cycle.turns;
    let mut sent = open;
    let mut size_checked = false;
    loop {
        claim.progress(round, PHASE_ARCHITECT, sent.len());
        let (fp_root, sid) = (root.to_path_buf(), spec_id.to_string());
        let (before, before_sig) = tokio::task::spawn_blocking(move || (task_fingerprints(&fp_root, &sid), plan_signature(&fp_root, &sid)))
            .await
            .unwrap_or_default();
        let end = consensus_turn(state, project_id, root, spec_id, round, &sent, size_checked, cycle.gen).await;
        let (decisions, asks_user, block, reply) = match end {
            TurnEnd::Shutdown => {
                save(root, spec_id, outcome_record(&record, Some(OUTCOME_SHUTDOWN), cycle)).await;
                return Err(Stop::Shutdown);
            }
            TurnEnd::NotStarted { busy } => {
                let why = if busy || runtime.user_gen(project_id) != cycle.gen { OUTCOME_INTERRUPTED } else { OUTCOME_UNAVAILABLE };
                halt(root, spec_id, cycle, &record, why).await;
                return Ok(false);
            }
            TurnEnd::Failed => {
                memory.changes.extend(turn_changes(state, project_id, root, spec_id, before_sig.as_deref()).await);
                halt(root, spec_id, cycle, &record, OUTCOME_UNAVAILABLE).await;
                return Ok(false);
            }
            TurnEnd::Done { decisions, asks_user, block, reply } => (decisions, asks_user, block, reply),
        };
        memory.changes.extend(turn_changes(state, project_id, root, spec_id, before_sig.as_deref()).await);
        let (after_root, sid) = (root.to_path_buf(), spec_id.to_string());
        let after = tokio::task::spawn_blocking(move || task_fingerprints(&after_root, &sid)).await.unwrap_or_default();
        let missing = Missing { block, asks_user, reply: &reply };
        let decisions = complete_decisions(state, project_id, root, round, &sent, decisions, missing, (&before, &after)).await;
        cycle.record_decisions(round, &sent, &decisions);
        if asks_user {
            round_line(state, project_id, root, cycle, round).await;
            halt(root, spec_id, cycle, &record, OUTCOME_AWAITING_USER).await;
            return Ok(false);
        }
        if size_checked || runtime.user_gen(project_id) != cycle.gen || runtime.user_turn_active(project_id) {
            break;
        }
        // C-04: tarefas novas ou alteradas que já nascem grandes demais voltam uma única vez, sem
        // contar como rodada.
        size_checked = true;
        let lang = state.project_lang(project_id).get();
        let (size_root, sid) = (root.to_path_buf(), spec_id.to_string());
        let big = tokio::task::spawn_blocking(move || {
            let changed: Vec<TaskSize> = task_sizes(&size_root, &sid)
                .into_iter()
                .filter(|s| after.get(&s.id).is_some_and(|fp| before.get(&s.id) != Some(fp)))
                .collect();
            with_size_issues(Vec::new(), &changed, lang)
        })
        .await
        .unwrap_or_default();
        if big.is_empty() {
            break;
        }
        let classified = cycle.classify(big.into_iter().map(|issue| ReviewItem { issue, reference: None, new_argument: false }).collect());
        cycle.round_entry(round).size_issues = classified.clone();
        sent = classified.into_iter().filter(|i| !i.disputed).collect();
        if sent.is_empty() {
            break;
        }
    }
    round_line(state, project_id, root, cycle, round).await;
    // O registro da versão revisada guarda as decisões; a próxima revisão grava o da versão nova.
    save(root, spec_id, outcome_record(&record, None, cycle)).await;
    Ok(true)
}

/// Para o ciclo com `why`: nenhuma rodada nova; o desfecho vai no registro da versão revisada e no
/// da próxima revisão (se o turno alterou o plano, a versão nova ainda é revisada).
async fn halt(root: &Path, spec_id: &str, cycle: &mut Cycle, record: &PlanReviewRecord, why: &'static str) {
    cycle.halt = Some(why);
    save(root, spec_id, outcome_record(record, Some(why), cycle)).await;
}

/// `record` com o estado do ciclo e o desfecho `outcome`.
fn outcome_record(record: &PlanReviewRecord, outcome: Option<&str>, cycle: &Cycle) -> PlanReviewRecord {
    PlanReviewRecord {
        rounds: cycle.rounds.clone(),
        points: cycle.points.clone(),
        next_point: cycle.next_point,
        outcome: outcome.map(str::to_string),
        ..record.clone()
    }
}

/// Grava o registro no pool bloqueante. `false` se a gravação falhou.
async fn save(root: &Path, spec_id: &str, record: PlanReviewRecord) -> bool {
    let (write_root, sid) = (root.to_path_buf(), spec_id.to_string());
    tokio::task::spawn_blocking(move || persist(&write_root, &sid, &record)).await.ok().flatten().is_some()
}

/// Desfecho no último registro gravado (mesma assinatura; uma versão sem resultado continua sem
/// resultado): o shutdown no meio da revisão, ou a interrupção do ciclo pela mensagem do usuário
/// (`only_if_open`: só se o ciclo ainda não tinha desfecho).
async fn mark_outcome(root: &Path, spec_id: &str, outcome: &'static str, only_if_open: bool) {
    let (write_root, sid) = (root.to_path_buf(), spec_id.to_string());
    let _ = tokio::task::spawn_blocking(move || {
        let record = read_record(&write_root, &sid)?;
        if only_if_open && record.outcome.is_some() {
            return Some(());
        }
        persist(&write_root, &sid, &PlanReviewRecord { outcome: Some(outcome.to_string()), ..record })
    })
    .await;
}

/// Linhas do `plan_changes` gravadas pelo turno que acabou de terminar, no idioma do projeto (vão ao
/// Revisor). Turno que não alterou o plano (assinatura igual à de antes dele, `before`) não
/// acrescenta nada: o registro da última alteração não volta ao Revisor como novidade.
async fn turn_changes(state: &AppState, project_id: &str, root: &Path, spec_id: &str, before: Option<&str>) -> Vec<String> {
    let secrets = state.secret_set().await;
    let lang = state.project_lang(project_id).get();
    let (read_root, sid, before) = (root.to_path_buf(), spec_id.to_string(), before.map(str::to_string));
    tokio::task::spawn_blocking(move || {
        let now = plan_signature(&read_root, &sid);
        if now.is_none() || now == before {
            return Vec::new();
        }
        super::plan_changes::view(&read_root, &sid, &secrets).unwrap_or_default().iter().map(|l| l.render(lang)).collect()
    })
        .await
        .unwrap_or_default()
}

/// Códigos da linha da rodada na Conversa (`system.<código>` nos dicionários).
pub const ROUND_CODE: &str = "plan.consensus_round";
pub const ROUND_OPEN_CODE: &str = "plan.consensus_round_open";
/// Rodada sem resposta em aberto, com ajustes deduzidos da mudança das tarefas (spec 131 T03).
pub const ROUND_INFERRED_CODE: &str = "plan.consensus_round_inferred";
/// Lista final das tarefas depois do consenso (spec 131 T03).
pub const FINAL_PLAN_CODE: &str = "plan.final_plan";
/// Teto do título de cada tarefa na lista final.
const MAX_FINAL_TITLE_BYTES: usize = 160;

/// Linha da rodada na Conversa (critério 5): "Revisor apontou N; Arquiteto ajustou A e manteve M",
/// no idioma do projeto, pelo dicionário.
async fn round_line(state: &AppState, project_id: &str, root: &Path, cycle: &mut Cycle, round: u32) {
    let entry = cycle.round_entry(round).clone();
    let total = entry.accepted + entry.rejected + entry.unanswered;
    let mut params = serde_json::Map::new();
    params.insert("round".into(), json!(round));
    params.insert("issues".into(), json!(total));
    params.insert("accepted".into(), json!(entry.accepted));
    params.insert("kept".into(), json!(entry.rejected));
    let code = if entry.unanswered > 0 {
        params.insert("open".into(), json!(entry.unanswered));
        ROUND_OPEN_CODE
    } else if entry.inferred > 0 {
        // Spec 131 T03: o ajuste deduzido da mudança da tarefa não é alerta.
        params.insert("inferred".into(), json!(entry.inferred));
        ROUND_INFERRED_CODE
    } else {
        ROUND_CODE
    };
    super::chat::system_notice(state, project_id, root.to_path_buf(), code, params).await;
}

/// Turno interno do Arquiteto numa rodada: o pedido com os apontamentos `sent` e a leitura das
/// decisões. `gen`: a geração das mensagens do usuário do ciclo (com outra, o turno ocupado não é
/// esperado).
#[allow(clippy::too_many_arguments)]
async fn consensus_turn(
    state: &AppState,
    project_id: &str,
    root: &Path,
    spec_id: &str,
    round: u32,
    sent: &[PlanIssue],
    size_check: bool,
    gen: u64,
) -> TurnEnd {
    let lines: Vec<String> = sent.iter().map(issue_line).collect();
    // Spec 131 T03 (critério 4): o texto atual das tarefas apontadas vai no pedido.
    let secrets = state.secret_set().await;
    let cited: Vec<String> = sent.iter().filter_map(|i| i.task.clone()).collect();
    let (task_root, sid, task_secrets) = (root.to_path_buf(), spec_id.to_string(), Arc::clone(&secrets));
    let tasks = tokio::task::spawn_blocking(move || cited_task_texts(&task_root, &sid, &cited, &task_secrets)).await.unwrap_or_default();
    let message = crate::agents::prompts::plan_consensus_request(spec_id, round, MAX_CONSENSUS_ROUNDS, &lines, size_check, &tasks);
    let mut shutdown = state.shutdown_signal();
    let turn = super::chat::consensus_turn(state.clone(), project_id.to_string(), message, round, gen);
    let out = tokio::select! {
        biased;
        _ = wait_shutdown(&mut shutdown) => return TurnEnd::Shutdown,
        out = turn => out,
    };
    if out.status.is_success() {
        let ids: Vec<String> = sent.iter().filter_map(|i| i.id.clone()).collect();
        let decisions = out.decisions.as_deref().map(|body| parse_decisions(body, &ids, &secrets)).unwrap_or_default();
        return TurnEnd::Done { decisions, asks_user: out.asks_user, block: out.decisions.is_some(), reply: out.reply };
    }
    if out.status.is_client_error() {
        return TurnEnd::NotStarted { busy: out.status == axum::http::StatusCode::CONFLICT };
    }
    TurnEnd::Failed
}

/// Caminho e texto atual de cada tarefa citada (`cited`, sem repetição, na ordem), redigido antes
/// do corte e limitado como no plano do Revisor (spec 131 T03), e se o texto é o arquivo inteiro
/// (F-01: sem corte nem redação; senão o Arquiteto o lê antes de regravar). Tarefa que não existe
/// fica de fora.
fn cited_task_texts(root: &Path, spec_id: &str, cited: &[String], secrets: &SecretSet) -> Vec<(String, String, bool)> {
    let files: Vec<(String, String, String)> = task_names(root, spec_id)
        .into_iter()
        .filter_map(|name| {
            let rel = format!("{SPECS_REL}/{spec_id}/tasks/{name}");
            let raw = safe_fs::read_regular(root, &rel, MAX_READ_BYTES).ok()?;
            let id = crate::sdd::Task::parse(&raw).ok()?.frontmatter.id;
            Some((id, rel, raw))
        })
        .collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut out: Vec<(String, String, bool)> = Vec::new();
    for id in cited {
        if !seen.insert(id.as_str()) {
            continue;
        }
        if let Some((_, rel, raw)) = files.iter().find(|(tid, _, _)| tid == id) {
            let text = truncate_bytes(&scrub_text(secrets, raw), MAX_TASK_BYTES);
            let whole = text == *raw;
            out.push((rel.clone(), text, whole));
        }
    }
    out
}

/// O que o turno deixou, para completar as decisões.
struct Missing<'a> {
    /// O turno trouxe o bloco (em qualquer mensagem).
    block: bool,
    asks_user: bool,
    /// Texto final mostrado do turno.
    reply: &'a str,
}

/// Completa as decisões do turno (spec 131 T03, critérios 1 e 2): sem decisão no bloco, o ponto de
/// uma tarefa que mudou no turno (`fingerprints`: antes e depois) conta como ajustado por inferência.
/// Sem o bloco no turno inteiro e com pontos ainda sem decisão, um pedido curto, sem ferramentas,
/// pede o bloco uma vez (não quando o turno termina numa pergunta ao usuário).
#[allow(clippy::too_many_arguments)]
async fn complete_decisions(
    state: &AppState,
    project_id: &str,
    root: &Path,
    round: u32,
    sent: &[PlanIssue],
    mut decisions: HashMap<String, PlanDecision>,
    missing: Missing<'_>,
    fingerprints: (&BTreeMap<String, String>, &BTreeMap<String, String>),
) -> HashMap<String, PlanDecision> {
    let (before, after) = fingerprints;
    // Alteradas, apagadas ou renomeadas (o id antigo sumiu). Uma tarefa nova sozinha não conta
    // (F-02): dividir é alterar a original.
    let changed: std::collections::HashSet<String> =
        before.iter().filter(|(id, fp)| after.get(*id) != Some(*fp)).map(|(id, _)| id.clone()).collect();
    let inferred = infer_adjusted(sent, &decisions, &changed);
    decisions.extend(inferred);
    let unresolved: Vec<&PlanIssue> = sent.iter().filter(|i| i.id.as_ref().is_some_and(|id| !decisions.contains_key(id))).collect();
    if missing.block || missing.asks_user || unresolved.is_empty() {
        return decisions;
    }
    let lines: Vec<String> = unresolved.iter().map(|i| issue_line(i)).collect();
    let ids: Vec<String> = unresolved.iter().filter_map(|i| i.id.clone()).collect();
    if let Some(answer) = ask_decisions(state, project_id, root, round, &lines, missing.reply).await {
        let secrets = state.secret_set().await;
        let body = super::plan_consensus::split_decisions(&answer).1.unwrap_or(answer);
        for (id, decision) in parse_decisions(&body, &ids, &secrets) {
            decisions.entry(id).or_insert(decision);
        }
    }
    decisions
}

/// Pedido curto do bloco ao Arquiteto (spec 131 T03): uma chamada sem ferramentas, com só os
/// apontamentos sem decisão e o texto final do turno; o uso vai com a etapa do consenso. `None` sem
/// o Arquiteto configurado, em falha do provedor, tempo esgotado ou shutdown.
async fn ask_decisions(state: &AppState, project_id: &str, root: &Path, round: u32, lines: &[String], reply: &str) -> Option<String> {
    use crate::agents::llm_client::{ChatMessage, ChatRequest, Role as ChatRole};
    use crate::agents::usage::{UsageBackend, UsageScope, UsageSink};
    let (model, effort, client) = super::chat::architect_client(state).await.ok()?;
    let backend: Arc<dyn ChatBackend> = match state.llm_backend() {
        Some(backend) => backend,
        None => Arc::new(client),
    };
    let secrets = state.secret_set().await;
    let sink: Arc<dyn UsageSink> =
        super::usage::FileUsageSink::redacting_staged(root.to_path_buf(), project_id, Arc::clone(&secrets), CONSENSUS_STAGE, round);
    let metered = UsageBackend::new(backend.as_ref(), Some(UsageScope::new(sink, "architect", None, None)));
    let request = ChatRequest {
        model,
        messages: vec![
            ChatMessage::text(ChatRole::System, crate::agents::prompts::PLAN_DECISIONS_ONLY_SYSTEM),
            ChatMessage::text(ChatRole::User, scrub_text(&secrets, &crate::agents::prompts::plan_decisions_request(lines, reply))),
        ],
        tools: Vec::new(),
        effort: Some(effort),
        response_json: false,
        web_search: false,
        cache_key: None,
        web_search_max_uses: None,
    };
    let mut shutdown = state.shutdown_signal();
    tokio::select! {
        biased;
        _ = wait_shutdown(&mut shutdown) => None,
        out = tokio::time::timeout(state.report_timeout(), metered.complete(&request)) => match out {
            Ok(Ok(resp)) => Some(resp.message.content),
            _ => None,
        },
    }
}

/// Lista final das tarefas na Conversa depois do consenso (spec 131 T03, critério 3): a frase fixa
/// pelo dicionário e, depois dela, a lista (id e título, na ordem de execução) como conteúdo.
async fn final_plan_line(state: &AppState, project_id: &str, root: &Path, spec_id: &str) {
    let secrets = state.secret_set().await;
    let (list_root, sid) = (root.to_path_buf(), spec_id.to_string());
    let tasks = tokio::task::spawn_blocking(move || final_task_list(&list_root, &sid)).await.unwrap_or_default();
    if tasks.is_empty() {
        return;
    }
    let tail: Vec<String> = tasks
        .iter()
        .map(|(id, title)| {
            let title = truncate_bytes(&clean_text(&secrets, title), MAX_FINAL_TITLE_BYTES);
            if title.is_empty() {
                format!("- {id}")
            } else {
                format!("- {id}: {title}")
            }
        })
        .collect();
    let mut params = serde_json::Map::new();
    params.insert("spec".into(), json!(spec_id));
    super::chat::system_notice_with_tail(state, project_id, root.to_path_buf(), FINAL_PLAN_CODE, params, Some(&tail.join("\n"))).await;
}

/// Tarefas do plano (id e título) na ordem de execução; com ciclo, na ordem dos ids.
pub fn final_task_list(root: &Path, spec_id: &str) -> Vec<(String, String)> {
    let tasks: Vec<crate::sdd::Task> = task_names(root, spec_id)
        .iter()
        .filter_map(|name| safe_fs::read_regular(root, &format!("{SPECS_REL}/{spec_id}/tasks/{name}"), MAX_READ_BYTES).ok())
        .filter_map(|raw| crate::sdd::Task::parse(&raw).ok())
        .filter(|t| valid_task_id(&t.frontmatter.id))
        .collect();
    let deps: Vec<(String, Vec<String>)> = tasks.iter().map(|t| (t.frontmatter.id.clone(), t.frontmatter.depends_on.clone())).collect();
    let order: Vec<String> = crate::sdd::execution_order::execution_order(&deps).unwrap_or_else(|_| {
        let mut ids: Vec<String> = deps.iter().map(|(id, _)| id.clone()).collect();
        ids.sort();
        ids
    });
    let mut seen: BTreeSet<String> = BTreeSet::new();
    order
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .filter_map(|id| tasks.iter().find(|t| t.frontmatter.id == id).map(|t| (id, t.frontmatter.title.clone())))
        .collect()
}

/// Impressão digital de cada tarefa do plano (id → SHA-256 do arquivo), para achar as tarefas
/// novas ou alteradas num turno (C-04).
fn task_fingerprints(root: &Path, spec_id: &str) -> BTreeMap<String, String> {
    task_names(root, spec_id)
        .iter()
        .filter_map(|name| safe_fs::read_regular(root, &format!("{SPECS_REL}/{spec_id}/tasks/{name}"), MAX_READ_BYTES).ok())
        .filter_map(|raw| {
            let id = crate::sdd::Task::parse(&raw).ok()?.frontmatter.id;
            valid_task_id(&id).then(|| (id, sha256_hex(raw.as_bytes())))
        })
        .collect()
}

/// Resultado de uma revisão: a assinatura revisada, o tamanho das tarefas e os apontamentos (ou o
/// `detail` da falha).
struct Reviewed {
    signature: String,
    sizes: Vec<TaskSize>,
    items: Result<Vec<ReviewItem>, String>,
    /// Falha do provedor (spec 133 T11): os `params` da evidência.
    failure: Option<serde_json::Map<String, Value>>,
}

/// Uma revisão da versão atual (rodada `round`, com a memória `memo` a partir da rodada 1). `Err` em
/// shutdown ou plano ilegível (nada gravado).
#[allow(clippy::too_many_arguments)]
async fn review_once(
    state: &AppState,
    project_id: &str,
    root: &Path,
    spec_id: &str,
    setup: &ReviewerSetup,
    round: u32,
    memo: Option<&Memo>,
    cycle: &Cycle,
) -> Result<Reviewed, Stop> {
    let secrets = state.secret_set().await;
    let (plan_root, sid, plan_secrets) = (root.to_path_buf(), spec_id.to_string(), Arc::clone(&secrets));
    let (signature, plan, sizes, dep_cycle) = tokio::task::spawn_blocking(move || {
        Some((
            plan_signature(&plan_root, &sid)?,
            build_plan(&plan_root, &sid, &plan_secrets)?,
            task_sizes(&plan_root, &sid),
            plan_cycle(&plan_root, &sid),
        ))
    })
    .await
    .ok()
    .flatten()
    .ok_or(Stop::Failed)?;
    let memory_text = memo.map(|(previous, changes, tasks)| {
        let new_tasks: Vec<String> = match tasks {
            Some(before) => sizes.iter().map(|s| s.id.clone()).filter(|id| !before.contains(id)).collect(),
            None => Vec::new(),
        };
        scrub_text(&secrets, &reviewer_memory(round, previous, cycle, changes, &new_tasks))
    });

    let policy = load_policy(root.to_path_buf()).await;
    let approver = Arc::new(ProjectApprover::new(
        project_id,
        root.to_path_buf(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::clone(&secrets),
    )
    .with_lang(state.project_lang(project_id)));
    let activity = Arc::new(
        BusActivitySink::new(state.events().clone(), project_id, Arc::clone(&secrets)).with_stage("plan_review", PLAN_REVIEW_STAGE),
    );
    let usage = super::usage::FileUsageSink::redacting_staged(root.to_path_buf(), project_id, Arc::clone(&secrets), CONSENSUS_STAGE, round);
    let reviewer = PlanReviewer::new(Arc::clone(&setup.backend), setup.model.clone(), Some(setup.effort), policy, approver)
        .with_activity(activity, Some(spec_id.to_string()))
        .with_usage(usage)
        .with_lang(state.project_lang(project_id));
    let mut shutdown = state.shutdown_signal();
    // Spec 133 T01: o rastro das chamadas da revisão dá a evidência da falha final do provedor.
    let turn = Box::pin(crate::agents::llm_client::evidence::with_trace_scope(async {
        let out = reviewer.review_with_memory(root, &plan, memory_text.as_deref()).await;
        let trace = out.is_err().then(super::provider_errors::StageTrace::current).unwrap_or_default();
        (out, trace)
    }));
    let mut failure_params = None;
    let answer = tokio::select! {
        biased;
        _ = wait_shutdown(&mut shutdown) => return Err(Stop::Shutdown),
        joined = tokio::time::timeout(state.report_timeout(), turn) => match joined {
            Ok((Ok(text), _)) => Ok(text),
            Ok((Err(failure), trace)) => {
                let evidence = super::provider_errors::record_stage_failure(
                    &secrets, project_id, root, Some(spec_id), "plan_review", &setup.stage, &failure, trace,
                )
                .await;
                failure_params = Some(super::provider_errors::stage_failure_params(&secrets, &failure, &evidence));
                Err(failure.describe())
            }
            Err(_) => Err(DETAIL_TIMEOUT.to_string()),
        },
    };
    let lang = state.project_lang(project_id).get();
    let items = match answer {
        Ok(raw) => match parse_review(&raw, &secrets) {
            Some(found) => Ok(with_cycle_items(with_size_items(found, &sizes, lang), &dep_cycle, lang)),
            None => Err(DETAIL_INVALID_JSON.to_string()),
        },
        Err(why) => Err(why),
    };
    Ok(Reviewed { signature, sizes, items, failure: failure_params })
}

/// Tarefas do plano que formam ciclo pelo `depends_on` (spec 128 T04); vazio sem ciclo. Com ciclo
/// não há ordem de execução.
pub fn plan_cycle(root: &Path, spec_id: &str) -> Vec<String> {
    let tasks: Vec<(String, Vec<String>)> = task_names(root, spec_id)
        .iter()
        .filter_map(|name| safe_fs::read_regular(root, &format!("{SPECS_REL}/{spec_id}/tasks/{name}"), MAX_READ_BYTES).ok())
        .filter_map(|raw| crate::sdd::Task::parse(&raw).ok())
        .map(|t| (t.frontmatter.id, t.frontmatter.depends_on))
        .collect();
    crate::sdd::execution_order::execution_order(&tasks).err().unwrap_or_default()
}

/// Tipo do apontamento do ciclo de dependências.
pub const CYCLE_KIND: &str = "contradiction";

/// Completa os apontamentos com um `contradiction` do servidor quando há ciclo (spec 128 T04): o
/// ciclo passa pelo julgamento do Arquiteto como os demais. O do servidor nunca é cortado: o último
/// apontamento do modelo cede a vaga dentro de [`MAX_ISSUES`]. Texto no idioma do projeto.
pub fn with_cycle_issue(found: Vec<PlanIssue>, cycle: &[String], lang: crate::i18n::Lang) -> Vec<PlanIssue> {
    with_cycle_items(found.into_iter().map(|issue| ReviewItem { issue, reference: None, new_argument: false }).collect(), cycle, lang)
        .into_iter()
        .map(|i| i.issue)
        .collect()
}

fn with_cycle_items(mut items: Vec<ReviewItem>, cycle: &[String], lang: crate::i18n::Lang) -> Vec<ReviewItem> {
    let Some(first) = cycle.first() else {
        return items;
    };
    let text = crate::i18n::tr(lang, "plan_review.dependency_cycle", &[("tasks", &cycle.join(", "))]);
    if items.len() >= MAX_ISSUES {
        // Cede a vaga o último apontamento do modelo (os `too_large` do servidor ficam).
        let at = items.iter().rposition(|i| i.issue.kind != TOO_LARGE).unwrap_or(items.len() - 1);
        items.remove(at);
    }
    items.push(ReviewItem { issue: PlanIssue::new(Some(first.clone()), CYCLE_KIND, &text), reference: None, new_argument: false });
    items
}

/// [`with_size_issues`] sobre os itens do modelo, mantendo o `ref` e o `new_argument` de cada um
/// (os do servidor não têm).
fn with_size_items(found: Vec<ReviewItem>, sizes: &[TaskSize], lang: crate::i18n::Lang) -> Vec<ReviewItem> {
    let merged = with_size_issues(found.iter().map(|i| i.issue.clone()).collect(), sizes, lang);
    merged
        .into_iter()
        .enumerate()
        .map(|(k, issue)| match found.get(k) {
            Some(item) if item.issue == issue => item.clone(),
            _ => ReviewItem { issue, reference: None, new_argument: false },
        })
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Specs em `AWAITING_APPROVAL` (pastas reais e não ocultas de `.sdd/specs`).
fn awaiting_specs(root: &Path) -> Vec<String> {
    if safe_fs::check_chain(root, SPECS_REL).is_err() {
        return Vec::new();
    }
    real_children(&root.join(SPECS_REL), true)
        .into_iter()
        .filter(|name| !name.starts_with('.') && validate_spec_id(name).is_ok())
        .filter(|name| awaiting(root, name))
        .collect()
}

/// `true` quando a spec está aguardando aprovação.
fn awaiting(root: &Path, spec_id: &str) -> bool {
    safe_fs::read_regular(root, &format!("{SPECS_REL}/{spec_id}/spec.md"), MAX_READ_BYTES)
        .ok()
        .and_then(|raw| Spec::parse(&raw).ok())
        .is_some_and(|spec| spec.frontmatter.status == SpecStatus::AwaitingApproval)
}

/// `true` quando o resultado gravado não é da versão atual do plano. Registro com caminho
/// inseguro (symlink na cadeia, ou não é arquivo regular) devolve `false` (spec 087 T06): o
/// resultado nunca seria gravado, e cada turno pagaria uma revisão descartada.
fn needs_review(root: &Path, spec_id: &str) -> bool {
    if !reviewable(root, spec_id) {
        return false;
    }
    match plan_signature(root, spec_id) {
        Some(sig) => read_record(root, spec_id).is_none_or(|r| r.signature != sig),
        None => false,
    }
}

/// `true` quando a revisão da spec pode rodar e ser gravada: registro em caminho seguro e plano com
/// tarefas (spec 127 T01: sem tarefas, nada a revisar; a vista diz `incomplete`).
fn reviewable(root: &Path, spec_id: &str) -> bool {
    record_path_safe(root, spec_id) && !task_names(root, spec_id).is_empty()
}

/// Nomes das tarefas `tasks/*.md` (arquivos reais, não ocultos), em ordem.
pub(super) fn task_names(root: &Path, spec_id: &str) -> Vec<String> {
    let tasks_rel = format!("{SPECS_REL}/{spec_id}/tasks");
    if safe_fs::check_chain(root, &tasks_rel).is_err() {
        return Vec::new();
    }
    real_children(&root.join(&tasks_rel), false)
        .into_iter()
        .filter(|n| n.ends_with(".md") && !n.starts_with('.'))
        .take(MAX_TASKS)
        .collect()
}

/// Assinatura do plano: SHA-256 do `spec.md` e de cada `tasks/*.md` (nome e conteúdo, em ordem).
/// `None` se o `spec.md` não pode ser lido com segurança.
pub fn plan_signature(root: &Path, spec_id: &str) -> Option<String> {
    let spec_rel = format!("{SPECS_REL}/{spec_id}");
    let spec = safe_fs::read_regular(root, &format!("{spec_rel}/spec.md"), MAX_READ_BYTES).ok()?;
    let mut data = Vec::with_capacity(spec.len() + 64);
    data.extend_from_slice(b"spec.md\0");
    data.extend_from_slice(spec.as_bytes());
    for name in task_names(root, spec_id) {
        let content = safe_fs::read_regular(root, &format!("{spec_rel}/tasks/{name}"), MAX_READ_BYTES).unwrap_or_default();
        data.extend_from_slice(b"\0tasks/");
        data.extend_from_slice(name.as_bytes());
        data.push(0);
        data.extend_from_slice(content.as_bytes());
    }
    Some(sha256_hex(&data))
}

/// Plano enviado ao modelo: `spec.md` e tarefas, cada um redigido ANTES do corte e limitado.
fn build_plan(root: &Path, spec_id: &str, secrets: &SecretSet) -> Option<String> {
    let spec_rel = format!("{SPECS_REL}/{spec_id}");
    let spec = safe_fs::read_regular(root, &format!("{spec_rel}/spec.md"), MAX_READ_BYTES).ok()?;
    let mut out = format!(
        "### spec.md ({spec_id})\n{}\n",
        truncate_bytes(&scrub_text(secrets, &spec), MAX_SPEC_BYTES).trim_end()
    );
    for name in task_names(root, spec_id) {
        let Ok(content) = safe_fs::read_regular(root, &format!("{spec_rel}/tasks/{name}"), MAX_READ_BYTES) else {
            continue;
        };
        let block = format!(
            "\n### tasks/{name}\n{}\n",
            truncate_bytes(&scrub_text(secrets, &content), MAX_TASK_BYTES).trim_end()
        );
        if out.len() + block.len() > MAX_PLAN_BYTES {
            out.push_str("\n(demais tarefas omitidas por tamanho)\n");
            break;
        }
        out.push_str(&block);
    }
    // Spec 124 T06: o tamanho de cada tarefa, para o apontamento `too_large`.
    let sizes = task_sizes(root, spec_id);
    if !sizes.is_empty() {
        out.push_str("\n### Tamanho das tarefas (contado pelo servidor)\n");
        for size in &sizes {
            let dense = if size.weight > size.criteria {
                format!(" (valem {} pela densidade: critério com mais de {CRITERION_WORDS} palavras conta como mais de um)", size.weight)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "- {}: {} critérios de aceitação{dense}; allowed_scope: {}\n",
                size.id,
                size.criteria,
                truncate_bytes(&scrub_text(secrets, &size.scope.join(", ")), 300)
            ));
        }
    }
    // Spec 124 T04: o que o ambiente de testes prova (critério que depende de capacidade ausente
    // vira verificação manual ou é apontado).
    let host = safe_fs::read_regular(root, ".sdd/settings.yaml", MAX_READ_BYTES)
        .ok()
        .and_then(|t| crate::sdd::Settings::parse(&t).ok())
        .is_some_and(|s| s.environment.is_host());
    let caps = capabilities::current(root, host, || format_utc(unix_now()));
    let section = capabilities::prompt_section(&caps, capabilities::Audience::Plan);
    if !section.is_empty() {
        out.push_str(&format!("\n{}", scrub_text(secrets, &section)));
    }
    Some(out)
}

/// Tamanho de uma tarefa do plano: critérios de aceitação contados, o peso deles com a densidade
/// (spec 128 T03, [`criteria_weight`]) e o `allowed_scope`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSize {
    pub id: String,
    pub criteria: usize,
    /// Critérios com o peso da densidade: um critério longo conta como mais de um. Nunca menor que
    /// `criteria`.
    pub weight: usize,
    pub scope: Vec<String>,
}

/// Tamanho das tarefas legíveis do plano, em ordem.
pub fn task_sizes(root: &Path, spec_id: &str) -> Vec<TaskSize> {
    task_names(root, spec_id)
        .iter()
        .filter_map(|name| safe_fs::read_regular(root, &format!("{SPECS_REL}/{spec_id}/tasks/{name}"), MAX_READ_BYTES).ok())
        .filter_map(|raw| crate::sdd::Task::parse(&raw).ok())
        .filter(|t| valid_task_id(&t.frontmatter.id))
        .map(|t| {
            let acceptance = section_text(&t.body, ACCEPTANCE).unwrap_or_default();
            TaskSize {
                criteria: count_criteria(&acceptance),
                weight: criteria_weight(&acceptance),
                scope: t.frontmatter.allowed_scope.clone(),
                id: t.frontmatter.id,
            }
        })
        .collect()
}

/// Completa os apontamentos do modelo com `too_large` para cada tarefa com [`MANY_CRITERIA`] ou
/// mais critérios que o modelo não apontou como grande. Spec 128 T03: conta o peso com a densidade
/// (critério longo vale mais de um); quando é a densidade que passa do limite, o texto diz isso e
/// pede critérios curtos. Não barra nada: o apontamento passa pelo julgamento do Arquiteto no
/// consenso. O texto fica gravado no idioma do projeto.
/// Os do servidor nunca são cortados: os do modelo cedem a vaga dentro de [`MAX_ISSUES`].
pub fn with_size_issues(found: Vec<PlanIssue>, sizes: &[TaskSize], lang: crate::i18n::Lang) -> Vec<PlanIssue> {
    let flagged = |id: &str| found.iter().any(|i| i.kind == TOO_LARGE && i.task.as_deref() == Some(id));
    let extra: Vec<PlanIssue> = sizes
        .iter()
        .filter(|s| s.criteria.max(s.weight) >= MANY_CRITERIA && !flagged(&s.id))
        .take(MAX_ISSUES)
        .map(|s| {
            let count = u64::try_from(s.criteria).unwrap_or(u64::MAX);
            let text = if s.weight > s.criteria {
                let weight = s.weight.to_string();
                let words = CRITERION_WORDS.to_string();
                crate::i18n::tr_plural(lang, "plan_review.too_large_dense", count, &[("weight", weight.as_str()), ("words", words.as_str())])
            } else {
                crate::i18n::tr_plural(lang, "plan_review.too_large", count, &[])
            };
            PlanIssue::new(Some(s.id.clone()), TOO_LARGE, &text)
        })
        .collect();
    let room = MAX_ISSUES.saturating_sub(extra.len());
    found.into_iter().take(room).chain(extra).collect()
}

/// Valida a resposta do modelo: o objeto JSON (entre o primeiro `{` e o último `}`) com `issues`
/// em lista. Itens com `kind` desconhecido ou sem texto são descartados; texto redigido, numa linha
/// e cortado em [`MAX_ISSUE_TEXT_BYTES`]; no máximo [`MAX_ISSUES`]. `None` = revisão indisponível
/// (JSON inválido, sem `issues`, ou só itens inválidos).
pub fn parse_issues(raw: &str, secrets: &SecretSet) -> Option<Vec<PlanIssue>> {
    parse_review(raw, secrets).map(|items| items.into_iter().map(|i| i.issue).collect())
}

/// [`parse_issues`] com o que o Revisor informa a partir da rodada 1 (spec 128 T02, C-01): `ref`
/// (id `P<n>` do mesmo ponto numa rodada anterior) e `new_argument` (ausente = `false`).
pub fn parse_review(raw: &str, secrets: &SecretSet) -> Option<Vec<ReviewItem>> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end < start {
        return None;
    }
    let value: Value = serde_json::from_str(&raw[start..=end]).ok()?;
    let list = value.get("issues")?.as_array()?;
    let items: Vec<ReviewItem> = list.iter().filter_map(|item| item_of(item, secrets)).take(MAX_ISSUES).collect();
    if items.is_empty() && !list.is_empty() {
        return None;
    }
    Some(items)
}

fn item_of(item: &Value, secrets: &SecretSet) -> Option<ReviewItem> {
    let kind = item.get("kind")?.as_str()?.trim();
    if !ISSUE_KINDS.contains(&kind) {
        return None;
    }
    let text = clean_text(secrets, item.get("text")?.as_str()?);
    if text.is_empty() {
        return None;
    }
    let task = item
        .get("task")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| valid_task_id(t))
        .map(str::to_string);
    let reference = item
        .get("ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| r.len() <= MAX_TASK_ID_BYTES && r.strip_prefix('P').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
        .map(str::to_string);
    let new_argument = item.get("new_argument").and_then(Value::as_bool).unwrap_or(false);
    Some(ReviewItem { issue: PlanIssue::new(task, kind, &text), reference, new_argument })
}

/// `T` seguido de letras/dígitos, curto (ex.: `T02`).
fn valid_task_id(id: &str) -> bool {
    id.len() >= 2
        && id.len() <= MAX_TASK_ID_BYTES
        && id.starts_with('T')
        && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Redige, junta em uma linha e corta (nessa ordem: segredo nunca vaza pelo corte).
pub(super) fn clean_text(secrets: &SecretSet, text: &str) -> String {
    let scrubbed = scrub_text(secrets, text);
    let line = scrubbed.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.len() <= MAX_ISSUE_TEXT_BYTES {
        return line;
    }
    format!("{}…", truncate_bytes(&line, MAX_ISSUE_TEXT_BYTES - '…'.len_utf8()).trim_end())
}

fn record_rel(spec_id: &str) -> String {
    format!("{SPECS_REL}/{spec_id}/{PLAN_REVIEW_FILE}")
}

/// Resultado gravado (arquivo regular, sem symlink). `None` se ausente, inseguro ou inválido.
/// Caminho do `plan-review.json` gravável com segurança: sem symlink na cadeia e, se existir,
/// arquivo regular. Ausente é seguro (será criado).
fn record_path_safe(root: &Path, spec_id: &str) -> bool {
    match safe_fs::check_chain(root, &record_rel(spec_id)) {
        Ok(path) => match fs::symlink_metadata(&path) {
            Ok(meta) => meta.file_type().is_file(),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        },
        Err(_) => false,
    }
}

fn read_record(root: &Path, spec_id: &str) -> Option<PlanReviewRecord> {
    let raw = safe_fs::read_regular(root, &record_rel(spec_id), MAX_RECORD_BYTES).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Grava o resultado de forma atômica (temporário exclusivo, `fsync`, `rename`), depois de
/// garantir as entradas locais do `.git/info/exclude`. Nunca segue symlink.
fn persist(root: &Path, spec_id: &str, record: &PlanReviewRecord) -> Option<()> {
    let body = serde_json::to_string_pretty(record).ok()?;
    write_local_file(root, &record_rel(spec_id), PLAN_REVIEW_FILE, &body)
}

/// Escrita atômica de um arquivo local do Nants em `rel` (nome `file_name`): exige exclude local
/// garantido, garante as entradas do `.git/info/exclude`, grava num temporário exclusivo
/// (`.<file_name>.<pid>.<seq>.tmp`, `O_NOFOLLOW`, `fsync`) e faz `rename`. Nunca segue symlink.
pub(super) fn write_local_file(root: &Path, rel: &str, file_name: &str, body: &str) -> Option<()> {
    if !exclude_guaranteed(root) {
        return None;
    }
    ensure_local_excludes(root).ok()?;
    let target = safe_fs::check_chain(root, rel).ok()?;
    if fs::symlink_metadata(&target).is_ok_and(|m| !m.file_type().is_file()) {
        return None;
    }
    let dir = target.parent()?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{file_name}.{}.{seq}.tmp", process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(body.as_bytes())?;
            file.sync_all()
        });
    if written.is_err() || safe_fs::check_chain(root, rel).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    fs::rename(&tmp, &target)
        .map_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
        .ok()
}

/// `plan_review` de uma spec aguardando aprovação: `incomplete` sem nenhuma tarefa (spec 127
/// T01); `running` enquanto a revisão roda (com a rodada e a fase do consenso); senão o resultado
/// gravado, só se a assinatura bate com o conteúdo atual (redigido de novo na leitura).
pub fn view(root: &Path, spec_id: &str, secrets: &SecretSet) -> Option<PlanReviewView> {
    if task_names(root, spec_id).is_empty() {
        return Some(PlanReviewView { status: STATUS_INCOMPLETE, at: None, issues: Vec::new(), consensus: None, failure: None });
    }
    if let Some(progress) = running_progress(root, spec_id) {
        let consensus = ConsensusView {
            round: progress.round,
            max_rounds: MAX_CONSENSUS_ROUNDS,
            phase: progress.phase,
            evaluating: progress.evaluating,
            ..ConsensusView::default()
        };
        return Some(PlanReviewView { status: STATUS_RUNNING, at: None, issues: Vec::new(), consensus: Some(consensus), failure: None });
    }
    let record = read_record(root, spec_id)?;
    if plan_signature(root, spec_id)? != record.signature {
        return None;
    }
    let status = match record.status.as_str() {
        STATUS_OK => STATUS_OK,
        STATUS_UNAVAILABLE => STATUS_UNAVAILABLE,
        _ => return None,
    };
    let issues: Vec<PlanIssue> = record
        .issues
        .iter()
        .filter(|i| ISSUE_KINDS.contains(&i.kind.as_str()))
        .take(MAX_ISSUES)
        .map(|i| clean_issue(secrets, i))
        .collect();
    let consensus = consensus_view(&record, &issues, secrets);
    // Spec 133 T11: a evidência da falha, saneada como os `params` das mensagens.
    let failure = record
        .failure
        .as_ref()
        .filter(|_| status == STATUS_UNAVAILABLE)
        .map(|params| super::events::message_params(secrets, params));
    Some(PlanReviewView { status, at: Some(scrub_text(secrets, &record.at)), issues, consensus, failure })
}

/// Apontamento lido de novo para a vista: tarefa e id válidos, texto redigido.
fn clean_issue(secrets: &SecretSet, issue: &PlanIssue) -> PlanIssue {
    PlanIssue {
        id: issue.id.as_deref().filter(|id| valid_point_id(id)).map(str::to_string),
        task: issue.task.as_deref().filter(|t| valid_task_id(t)).map(str::to_string),
        kind: issue.kind.clone(),
        text: clean_text(secrets, &issue.text),
        disputed: issue.disputed,
    }
}

/// `P` seguido de dígitos, curto (ex.: `P12`).
fn valid_point_id(id: &str) -> bool {
    id.len() <= MAX_TASK_ID_BYTES && id.strip_prefix('P').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Vista do ciclo de um registro com rodadas ou desfecho (registros antigos: `None`).
fn consensus_view(record: &PlanReviewRecord, issues: &[PlanIssue], secrets: &SecretSet) -> Option<ConsensusView> {
    if record.rounds.is_empty() && record.outcome.is_none() {
        return None;
    }
    let point = |id: &str| record.points.iter().find(|p| p.id == id);
    let reason_of = |id: &str| {
        point(id).map_or((None, None), |p| {
            let kind = p.reason_kind.as_deref().filter(|k| super::plan_consensus::REASON_KINDS.contains(k)).map(str::to_string);
            (kind, p.reason.as_deref().map(|r| clean_text(secrets, r)))
        })
    };
    // Pontos com decisão registrada no ciclo: a vista leva a decisão e o motivo do Arquiteto.
    let decided: BTreeSet<&str> = record
        .rounds
        .iter()
        .flat_map(|r| &r.decisions)
        .filter(|d| d.decision == DECISION_ACCEPT || d.decision == DECISION_REJECT)
        .map(|d| d.id.as_str())
        .collect();
    let decision_of = |id: &str| {
        point(id)
            .filter(|_| decided.contains(id))
            .and_then(|p| p.decision.as_deref())
            .filter(|d| *d == DECISION_ACCEPT || *d == DECISION_REJECT)
            .map(str::to_string)
    };
    let rejected = |id: &str| decision_of(id).as_deref() == Some(DECISION_REJECT);
    // Reaberto (spec 128 T05, critério 11): recusado, fora de disputa (o Revisor trouxe argumento
    // novo) e a última decisão sobre ele é de uma rodada anterior ou igual à da última revisão (a
    // decisão de uma rodada precede a revisão dessa rodada). Uma recusa que responde à última
    // revisão (rodada posterior, ex.: o ciclo parou numa pergunta) continua "mantida".
    let last_review = record.rounds.iter().filter(|r| r.reviewed_at.is_some()).map(|r| r.round).max();
    let decided_in = |id: &str| {
        record
            .rounds
            .iter()
            .filter(|r| r.decisions.iter().any(|d| d.id == id && (d.decision == DECISION_ACCEPT || d.decision == DECISION_REJECT)))
            .map(|r| r.round)
            .max()
    };
    let reopened_ids: BTreeSet<&str> = issues
        .iter()
        .filter(|i| !i.disputed)
        .filter_map(|i| i.id.as_deref())
        .filter(|id| rejected(id) && matches!((decided_in(id), last_review), (Some(d), Some(r)) if d <= r))
        .collect();
    let to_view = |issue: &PlanIssue| {
        let id = issue.id.clone().unwrap_or_default();
        let decision = decision_of(&id);
        let (reason_kind, reason) = if decision.is_some() || issue.disputed { reason_of(&id) } else { (None, None) };
        PointView { id, task: issue.task.clone(), kind: issue.kind.clone(), text: issue.text.clone(), decision, reason_kind, reason }
    };
    // Mantidos: os recusados do ciclo que não estão em disputa, inclusive os da última revisão (um
    // recusado nunca aparece como "sem resposta").
    let disputed_ids: BTreeSet<&str> = issues.iter().filter(|i| i.disputed).filter_map(|i| i.id.as_deref()).collect();
    let mut kept_ids: Vec<&str> = Vec::new();
    for decision in record.rounds.iter().flat_map(|r| &r.decisions) {
        let id = decision.id.as_str();
        if rejected(id) && valid_point_id(id) && !disputed_ids.contains(id) && !reopened_ids.contains(id) && !kept_ids.contains(&id) {
            kept_ids.push(id);
        }
    }
    let kept_points: Vec<PointView> = kept_ids
        .into_iter()
        .filter_map(|id| match issues.iter().find(|i| i.id.as_deref() == Some(id)) {
            Some(issue) => Some(to_view(issue)),
            None => point(id).map(|p| {
                let (reason_kind, reason) = reason_of(&p.id);
                PointView {
                    id: p.id.clone(),
                    task: p.task.as_deref().filter(|t| valid_task_id(t)).map(str::to_string),
                    kind: p.kind.clone(),
                    text: clean_text(secrets, &p.text),
                    decision: Some(DECISION_REJECT.to_string()),
                    reason_kind,
                    reason,
                }
            }),
        })
        .collect();
    let reopened: Vec<PointView> = issues.iter().filter(|i| i.id.as_deref().is_some_and(|id| reopened_ids.contains(id))).map(to_view).collect();
    let open: Vec<PointView> = issues.iter().filter(|i| !i.disputed && !rejected(i.id.as_deref().unwrap_or_default())).map(to_view).collect();
    let disputed: Vec<PointView> = issues.iter().filter(|i| i.disputed).map(to_view).collect();
    // Totais por ponto distinto e pela decisão final (F-01 da revisão): ajustados são os aceitos
    // que não voltaram a ficar em aberto; mantidos são os `kept_points`; o total fecha a conta
    // (ajustados + mantidos + em aberto, reabertos e em disputa).
    let waiting: BTreeSet<&str> = open.iter().chain(&reopened).chain(&disputed).map(|p| p.id.as_str()).collect();
    let adjusted = decided
        .iter()
        .filter(|&&id| valid_point_id(id) && !waiting.contains(id) && decision_of(id).as_deref() == Some(DECISION_ACCEPT))
        .count();
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let kept = count(kept_points.len());
    let total = [count(adjusted), kept, count(open.len()), count(reopened.len()), count(disputed.len())]
        .into_iter()
        .fold(0u32, u32::saturating_add);
    let sum = |f: fn(&ConsensusRound) -> u32| record.rounds.iter().map(f).fold(0u32, u32::saturating_add);
    let first = record.rounds.iter().find(|r| r.reviewed_at.is_some());
    let last = record.rounds.iter().rev().find(|r| r.reviewed_at.is_some());
    Some(ConsensusView {
        round: record.rounds.iter().map(|r| r.round).max().unwrap_or(0),
        max_rounds: MAX_CONSENSUS_ROUNDS,
        phase: None,
        evaluating: 0,
        outcome: record.outcome.as_deref().filter(|o| OUTCOMES.contains(o)).map(str::to_string),
        adjusted: count(adjusted),
        kept,
        unanswered: sum(|r| r.unanswered),
        open,
        disputed,
        kept_points,
        reopened,
        total,
        tasks_first: first.map_or(0, |r| r.tasks),
        criteria_first: first.map_or(0, |r| r.criteria),
        tasks: last.map_or(0, |r| r.tasks),
        criteria: last.map_or(0, |r| r.criteria),
    })
}

/// Desfechos aceitos na leitura do registro.
const OUTCOMES: [&str; 7] = [
    OUTCOME_CONSENSUS,
    OUTCOME_DISPUTED,
    OUTCOME_OPEN,
    OUTCOME_AWAITING_USER,
    OUTCOME_UNAVAILABLE,
    OUTCOME_INTERRUPTED,
    OUTCOME_SHUTDOWN,
];

/// [`view`] com a espera da versão atual (spec 092 T02): sem resultado nem revisão em curso, e com
/// o Revisor configurado (`reviewer`) e a revisão possível (exclude local garantido, registro
/// gravável), devolve `pending` se o plano mudou há menos de [`PENDING_WINDOW`] em relação a `now`,
/// e `stale` depois. Sem Revisor, nada (como antes).
pub fn view_at(root: &Path, spec_id: &str, secrets: &SecretSet, reviewer: bool, now: SystemTime) -> Option<PlanReviewView> {
    if let Some(found) = view(root, spec_id, secrets) {
        return Some(found);
    }
    if !reviewer || !exclude_guaranteed(root) || !needs_review(root, spec_id) {
        return None;
    }
    let changed = plan_modified(root, spec_id)?;
    // mtime no futuro (relógio defasado, cópia que preserva mtime): uma diferença pequena conta
    // como "agora"; além da janela, vira `stale` (a espera nunca passa de ~3 min).
    let age = match now.duration_since(changed) {
        Ok(age) => age,
        Err(e) if e.duration() <= PENDING_WINDOW => Duration::ZERO,
        Err(_) => PENDING_WINDOW,
    };
    let status = if age < PENDING_WINDOW { STATUS_PENDING } else { STATUS_STALE };
    Some(PlanReviewView { status, at: None, issues: Vec::new(), consensus: None, failure: None })
}

/// Última alteração do plano: maior `mtime` entre o `spec.md` e as `tasks/*.md` (sem symlink).
fn plan_modified(root: &Path, spec_id: &str) -> Option<SystemTime> {
    let spec_rel = format!("{SPECS_REL}/{spec_id}");
    let mtime = |rel: &str| {
        safe_fs::check_chain(root, rel)
            .ok()
            .and_then(|path| fs::symlink_metadata(path).ok())
            .filter(|meta| meta.file_type().is_file())
            .and_then(|meta| meta.modified().ok())
    };
    let spec = mtime(&format!("{spec_rel}/spec.md"))?;
    Some(
        task_names(root, spec_id)
            .iter()
            .filter_map(|name| mtime(&format!("{spec_rel}/tasks/{name}")))
            .fold(spec, SystemTime::max),
    )
}

#[cfg(test)]
#[path = "plan_review_tests.rs"]
mod tests;
