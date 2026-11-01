//! Observadores do Nanty (spec 138 T02, C-06): determinísticos, sem modelo, cada um ligado à sua
//! fonte:
//! - **plano esperando e execução parada pelo provedor:** `attention::build_attention` (a mesma
//!   visão do sino, por [`super::super::attention::projects_attention`]);
//! - **orçamento estourado e laço de revisão:** o `next_action_code` da tarefa BLOCKED (`budget`,
//!   `kept_recurring`). É só sinal: o runner já pausa sozinho (C-01, opção a);
//! - **custo fora do normal:** `cost_alert`;
//! - **disco e pasta temporária:** espaço livre pelo `statvfs` do `doctor`, nunca por contagem de
//!   entradas;
//! - **recursos Docker** com o rótulo `nants.project`: os de projeto cadastrado e parado (pela
//!   verificação estrita de atividade) são os órfãos que a ação "limpar órfãos" (T03) limpa; os de
//!   projeto que esta instalação não conhece viram só um aviso informativo (podem ser de outra
//!   instalação do Nants no mesmo Docker; o Nanty nunca os remove);
//! - **marcos de progresso:** tarefa entregue, spec concluída e app pronto, pelos eventos.
//!
//! **Fonte única (spec 139 T02, G2):** o que se sabe de um projeto sai de [`project_facts`], que lê
//! a mesma visão do sino ([`crate::server::attention::attention_with`], que vem do `disk_attention`).
//! O observador a chama para cada projeto e a conversa (T03) a usa para dizer o estado: os dois nunca
//! divergem. A fase única ([`Phase`]) tem precedência explícita em [`ProjectFacts::phase`].
//!
//! O disco e o Docker passam por uma sonda ([`NantyProbe`]) que os testes trocam: nenhum teste
//! chama o `docker` de verdade.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::signals::{Signal, SignalKind};
use crate::config::{Config, NantyConfig};
use crate::server::attention::SpecProgress;
use crate::server::events::{ApiEvent, EventType};
use crate::server::state::AppState;

/// Códigos de parada do provedor que significam crédito ou cota (urgente).
pub const CREDIT_STOP_CODES: [&str; 4] = ["run.billing", "run.spend_limit", "run.daily_quota", "run.plan_limit"];
/// `next_action_code` da tarefa BLOCKED por orçamento estourado.
pub const BUDGET_CODE: &str = "budget";
/// `next_action_code` da tarefa BLOCKED por laço de revisão.
pub const REVIEW_LOOP_CODE: &str = "kept_recurring";
/// A cada quantas varreduras o Docker é consultado (a lista é mais cara que o resto).
pub const DOCKER_EVERY: u64 = 10;

/// Sonda do disco e do Docker (produção: [`SystemProbe`]; testes: uma sonda falsa).
pub trait NantyProbe: Send + Sync {
    /// Bytes livres no sistema de arquivos de `path` (ou do primeiro ancestral que existe).
    fn free_bytes(&self, path: &Path) -> Option<u64>;
    /// Projetos (do rótulo `nants.project`) de cada container e rede; `None` sem Docker.
    fn docker_labeled(&self) -> Option<Vec<String>>;
}

/// Sonda real: o `statvfs` do `doctor` e o `docker` com ambiente controlado.
#[derive(Debug, Default)]
pub struct SystemProbe;

impl NantyProbe for SystemProbe {
    fn free_bytes(&self, path: &Path) -> Option<u64> {
        path.ancestors().find(|p| p.exists()).and_then(crate::server::doctor::free_bytes)
    }

    fn docker_labeled(&self) -> Option<Vec<String>> {
        let runner = crate::harness::compose::RealComposeRunner::default();
        crate::server::envs::labeled_resources(&runner).ok().map(|list| list.into_iter().map(|r| r.project).collect())
    }
}

/// Sonda muda: nada medido, sem Docker (o padrão dos testes: o laço nunca toca a máquina).
#[derive(Debug, Default)]
pub struct NullProbe;

impl NantyProbe for NullProbe {
    fn free_bytes(&self, _: &Path) -> Option<u64> {
        None
    }

    fn docker_labeled(&self) -> Option<Vec<String>> {
        None
    }
}

/// A sonda padrão: a real em produção, a muda nos testes.
pub fn default_probe() -> Arc<dyn NantyProbe> {
    if cfg!(test) {
        Arc::new(NullProbe)
    } else {
        Arc::new(SystemProbe)
    }
}

/// O que se sabe de um projeto: as entradas dos observadores de atenção, tarefas e custo e o que a
/// fase única mostra (spec 139 T02). Sai de [`project_facts`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectFacts {
    pub project_id: String,
    pub plan_waiting: bool,
    /// A spec do plano que espera aprovação (T03: o cartão de aprovar leva o `spec_id`).
    pub plan_spec_id: Option<String>,
    /// A spec da execução pausada ou parada (T03: retomar e tentar de novo).
    pub run_spec_id: Option<String>,
    /// Tarefa BLOCKED da execução pausada.
    pub blocked: Option<BlockedTask>,
    /// Código da parada atual pelo provedor (`run.billing`...) e o papel.
    pub stop_code: Option<String>,
    pub role: Option<&'static str>,
    /// A execução parou com tarefas por fazer, sem tarefa BLOCKED (o `stalled` do sino).
    pub stalled: bool,
    /// Tarefas em andamento acima do patamar de custo.
    pub cost_alerts: usize,
    /// A spec da execução ativa agora (`None` sem execução).
    pub running_spec_id: Option<String>,
    /// O andamento da spec ativa, pela mesma escolha da atenção.
    pub progress: Option<SpecProgress>,
    /// O que espera a pessoa.
    pub waiting: Waiting,
    /// Hora local (`HH:MM`) em que o observador viu a condição da fase pela primeira vez
    /// (`alerts[].since`); `None` sem condição aberta.
    pub since: Option<String>,
    /// Custo em US$ da spec da fase (só o que tem preço); `None` sem preço ou sem spec.
    pub cost_usd: Option<f64>,
}

/// O que espera a pessoa num projeto, pela visão de atenção do sino.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Waiting {
    /// Pedidos de permissão pendentes.
    pub permissions: usize,
    /// O Arquiteto fez perguntas que ainda não têm resposta.
    pub choices: bool,
    /// O plano pode ser aprovado.
    pub plan: bool,
    /// A tarefa pausada tem opções de escalada para a pessoa escolher.
    pub recovery: bool,
}

/// A fase única de um projeto (spec 139 T02, C-05). A precedência está em [`ProjectFacts::phase`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    /// Execução pausada com uma tarefa BLOCKED.
    Paused,
    /// Execução parada pelo provedor (crédito, cota, falha), sem tarefa BLOCKED.
    ProviderStopped,
    /// Plano esperando a aprovação da pessoa.
    PlanWaiting,
    /// Parada esperando retomar: a execução parou com tarefas por fazer, sem tarefa BLOCKED e sem
    /// código do provedor. O sino a lista com a opção de retomar, então ela espera a pessoa (decisão
    /// da revisão da T02, F-02: fase própria, em vez de um item em `waiting`).
    Stalled,
    /// Execução ativa agora.
    Running,
    /// Parada sem pendência: nada roda e nada do que está acima vale.
    #[default]
    Stopped,
}

impl Phase {
    /// Código estável da fase (para a ferramenta da conversa e os testes).
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Paused => "paused",
            Phase::ProviderStopped => "provider_stopped",
            Phase::PlanWaiting => "plan_waiting",
            Phase::Stalled => "stalled",
            Phase::Running => "running",
            Phase::Stopped => "stopped",
        }
    }
}

/// Tarefa BLOCKED: só o id (código `Txx`), o código do motivo e a tentativa.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockedTask {
    pub task_id: String,
    pub code: Option<String>,
    pub attempts: u32,
    /// Tentativas permitidas à tarefa (spec 139 T12): sem ele, o modelo deduzia um limite atingido.
    pub max_attempts: u32,
}

/// O tipo do sinal de uma tarefa BLOCKED, pelo código do motivo.
fn blocked_kind(task: &BlockedTask) -> SignalKind {
    match task.code.as_deref() {
        Some(BUDGET_CODE) => SignalKind::Budget,
        Some(REVIEW_LOOP_CODE) => SignalKind::ReviewLoop,
        _ => SignalKind::RunBlocked,
    }
}

/// O tipo do sinal de uma parada pelo provedor: sem crédito ou cota é urgente.
fn stop_kind(code: &str) -> SignalKind {
    if CREDIT_STOP_CODES.contains(&code) {
        SignalKind::NoCredit
    } else {
        SignalKind::ProviderStopped
    }
}

impl ProjectFacts {
    /// A fase única, nesta precedência (a primeira que vale ganha):
    /// 1. **pausada:** há tarefa BLOCKED (mesmo com a parada do provedor ainda valendo, a pessoa
    ///    decide pela tarefa);
    /// 2. **parada pelo provedor:** a execução parou com um código do provedor;
    /// 3. **plano esperando:** o plano pode ser aprovado;
    /// 4. **parada esperando retomar:** a execução parou com tarefas por fazer (o sino oferece
    ///    retomar);
    /// 5. **rodando:** há execução ativa (enquanto roda, a atenção já esconde as quatro de cima);
    /// 6. **parada sem pendência:** o resto.
    pub fn phase(&self) -> Phase {
        if self.blocked.is_some() {
            Phase::Paused
        } else if self.stop_code.is_some() {
            Phase::ProviderStopped
        } else if self.plan_waiting {
            Phase::PlanWaiting
        } else if self.stalled {
            Phase::Stalled
        } else if self.running_spec_id.is_some() {
            Phase::Running
        } else {
            Phase::Stopped
        }
    }

    /// O tipo do sinal que corresponde à fase (o grupo cujo `since` a fase mostra).
    pub fn phase_kind(&self) -> Option<SignalKind> {
        match self.phase() {
            Phase::Paused => self.blocked.as_ref().map(blocked_kind),
            Phase::ProviderStopped => self.stop_code.as_deref().map(stop_kind),
            Phase::PlanWaiting => Some(SignalKind::PlanWaiting),
            Phase::Stalled | Phase::Running | Phase::Stopped => None,
        }
    }

    /// A spec da fase: a da execução (pausada, parada, esperando retomar ou ativa), a do plano ou a ativa.
    pub fn phase_spec_id(&self) -> Option<&str> {
        match self.phase() {
            Phase::Paused | Phase::ProviderStopped | Phase::Stalled => self.run_spec_id.as_deref(),
            Phase::PlanWaiting => self.plan_spec_id.as_deref(),
            Phase::Running => self.running_spec_id.as_deref(),
            Phase::Stopped => self.progress.as_ref().map(|p| p.spec_id.as_str()),
        }
    }

    /// O andamento da spec da fase (só quando é a mesma spec que a atenção escolheu).
    pub fn phase_progress(&self) -> Option<&SpecProgress> {
        let spec = self.phase_spec_id()?;
        self.progress.as_ref().filter(|p| p.spec_id == spec)
    }

    /// A tarefa da fase: a BLOCKED na pausa; a em andamento quando roda.
    pub fn phase_task_id(&self) -> Option<&str> {
        match self.phase() {
            Phase::Paused => self.blocked.as_ref().map(|t| t.task_id.as_str()),
            Phase::Running => self.phase_progress().and_then(|p| p.current_task.as_deref()),
            _ => None,
        }
    }

    /// Os fatos da fase, limpos: zero, falso, vazio e valor padrão ficam de fora (`attempts: 0` não
    /// é fato). Só números, códigos e ids.
    pub fn status_facts(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("phase".into(), json!(self.phase().as_str()));
        if let Some(since) = &self.since {
            out.insert("since".into(), json!(since));
        }
        if let Some(spec) = self.phase_spec_id() {
            out.insert("spec_id".into(), json!(spec));
        }
        if let Some(task) = self.phase_task_id() {
            out.insert("task_id".into(), json!(task));
        }
        if let Some(p) = self.phase_progress().filter(|p| p.total > 0) {
            out.insert("done".into(), json!(p.done));
            out.insert("total".into(), json!(p.total));
        }
        if let (Phase::Paused, Some(task)) = (self.phase(), &self.blocked) {
            if let Some(code) = &task.code {
                out.insert("reason".into(), json!(code));
            }
            if task.attempts > 0 {
                out.insert("attempts".into(), json!(task.attempts));
                if task.max_attempts > 0 {
                    out.insert("max_attempts".into(), json!(task.max_attempts));
                }
            }
        }
        if let Some(code) = &self.stop_code {
            out.insert("stop".into(), json!(code));
            if let Some(role) = self.role {
                out.insert("role".into(), json!(role));
            }
        }
        let w = &self.waiting;
        let waiting: Vec<&str> = [
            (w.plan, "plan_approval"),
            (w.permissions > 0, "permission"),
            (w.choices, "choice"),
            (w.recovery, "recovery_choice"),
        ]
        .into_iter()
        .filter_map(|(on, code)| on.then_some(code))
        .collect();
        if !waiting.is_empty() {
            out.insert("waiting".into(), json!(waiting));
        }
        if w.permissions > 0 {
            out.insert("permissions".into(), json!(w.permissions));
        }
        if let Some(usd) = self.cost_usd.filter(|v| *v > 0.0) {
            out.insert("cost_usd".into(), json!((usd * 10_000.0).round() / 10_000.0));
        }
        if self.cost_alerts > 0 {
            out.insert("cost_alerts".into(), json!(self.cost_alerts));
        }
        out
    }
}

/// Sinais de um projeto (função pura).
pub fn project_signals(f: &ProjectFacts) -> Vec<Signal> {
    let id = Some(f.project_id.as_str());
    let mut out = Vec::new();
    let with_spec = |signal: Signal, spec: &Option<String>| match spec {
        Some(spec) => signal.fact("spec_id", spec.as_str()),
        None => signal,
    };
    if f.plan_waiting {
        out.push(with_spec(Signal::new(SignalKind::PlanWaiting, id), &f.plan_spec_id));
    }
    if let Some(code) = &f.stop_code {
        let mut signal = Signal::new(stop_kind(code), id).fact("stop", code.as_str());
        if let Some(role) = f.role {
            signal = signal.fact("role", role);
        }
        out.push(with_spec(signal, &f.run_spec_id));
    }
    if let Some(task) = &f.blocked {
        let mut signal = Signal::new(blocked_kind(task), id).fact("task_id", task.task_id.as_str());
        // Spec 139 T02: zero tentativas não é fato (o modelo dizia "0 attempts recorded").
        if task.attempts > 0 {
            signal = signal.fact("attempts", task.attempts);
        }
        if let Some(code) = &task.code {
            signal = signal.fact("reason", code.as_str());
        }
        out.push(with_spec(signal, &f.run_spec_id));
    }
    if f.cost_alerts > 0 {
        out.push(Signal::new(SignalKind::CostAlert, id).fact("count", f.cost_alerts));
    }
    out
}

/// Sinal do espaço livre: o menor medido entre `measured`, abaixo do mínimo. `None` = nada medido.
pub fn space_signal(kind: SignalKind, measured: &[u64], min_bytes: u64, min_gb: u64) -> Option<Option<Signal>> {
    let low = measured.iter().copied().min()?;
    let below = measured.iter().filter(|b| **b < min_bytes).count();
    Some((below > 0).then(|| {
        Signal::new(kind, None)
            .fact("min_gb", min_gb)
            .fact("count", below)
            .extra("free_gb", low / crate::config::GB_BYTES)
    }))
}

/// Sinais do Docker pelos projetos do rótulo de cada recurso: órfãos (projeto cadastrado e parado,
/// `busy` = os ativos pela verificação estrita) e estranhos (projeto que esta instalação não conhece,
/// só aviso).
pub fn docker_signals(labeled: &[String], known: &[String], busy: &[String]) -> Vec<Signal> {
    let orphans = labeled.iter().filter(|p| known.contains(p) && !busy.contains(p)).count();
    let foreign = labeled.iter().filter(|p| !known.contains(p)).count();
    let mut out = Vec::new();
    if orphans > 0 {
        out.push(Signal::new(SignalKind::DockerOrphans, None).fact("count", orphans));
    }
    if foreign > 0 {
        out.push(Signal::new(SignalKind::DockerForeign, None).fact("count", foreign));
    }
    out
}

/// Marco de progresso de um evento do barramento, se o projeto é observado. O `spec_id` do evento
/// vai nos fatos: é o que separa a conclusão de uma spec da seguinte e a T01 de uma spec da T01 de
/// outra (o descarte de evento repetido compara os fatos).
pub fn milestone(event: &ApiEvent, cfg: &NantyConfig) -> Option<Signal> {
    let project = event.project_id.as_deref().filter(|p| cfg.observes(p))?;
    let kind = event.event_type();
    let field = |name: &str| event.payload.get(name).and_then(Value::as_str).filter(|v| !v.is_empty());
    let with_spec = |signal: Signal| match field("spec_id") {
        Some(spec) => signal.fact("spec_id", spec),
        None => signal,
    };
    if kind == EventType::TaskUpdated.as_str() && event.payload.get("integrated") == Some(&Value::Bool(true)) {
        let task = field("task_id").unwrap_or_default();
        return Some(with_spec(Signal::new(SignalKind::TaskDelivered, Some(project)).fact("task_id", task)));
    }
    if kind == EventType::FinalizationDone.as_str() {
        return Some(with_spec(Signal::new(SignalKind::SpecDone, Some(project))));
    }
    if kind == EventType::AppStarted.as_str() {
        return Some(with_spec(Signal::new(SignalKind::AppReady, Some(project))));
    }
    None
}

/// Eventos que pedem uma nova olhada nos projetos (com espera curta, para juntar uma rajada).
pub fn wants_scan(event: &ApiEvent) -> bool {
    [
        EventType::ProjectStatus,
        EventType::TaskUpdated,
        EventType::SddDraftUpdated,
        EventType::RecoveryDecided,
        EventType::ChatTurnEnded,
        EventType::FinalizationDone,
        EventType::AppStarted,
        EventType::AppExited,
        EventType::ProjectRemoved,
    ]
    .iter()
    .any(|t| t.as_str() == event.event_type())
}

/// Resultado de uma varredura: os sinais e os tipos que ela cobriu por inteiro.
#[derive(Debug, Default)]
pub struct Scan {
    pub signals: Vec<Signal>,
    pub covered: Vec<SignalKind>,
}

/// Tarefa BLOCKED lida do disco (`next_action_code` e tentativas).
fn blocked_task(root: &Path, spec_id: &str, task_id: &str) -> BlockedTask {
    let rel = format!(".sdd/specs/{spec_id}/tasks/{task_id}.md");
    let meta = crate::server::projects::read_task(root, &rel).ok().map(|t| t.frontmatter);
    BlockedTask {
        task_id: task_id.to_string(),
        code: meta.as_ref().and_then(|m| m.next_action_code.clone()),
        attempts: meta.as_ref().map_or(0, |m| m.attempts),
        max_attempts: meta.map_or(0, |m| m.max_attempts),
    }
}

/// O quanto [`facts_with`] lê: só o que os sinais usam (a varredura) ou tudo (a conversa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// Só as entradas dos sinais: sem o custo (que lê o `usage.jsonl`) e sem o `since`.
    Signals,
    /// Tudo o que a fase mostra.
    Full,
}

/// Os fatos de um projeto cadastrado (spec 139 T02, G2): a visão de atenção do sino
/// ([`crate::server::attention::attention_with`]), a tarefa BLOCKED, a parada pelo provedor, a
/// execução ativa, o custo e o `since` da condição aberta. É a fonte única do observador e da
/// conversa (a conversa pede esta, com tudo). `None` sem o projeto ou se a leitura do disco falhou
/// (nada é dado por resolvido).
pub async fn project_facts(state: &AppState, cfg: &Config, id: &str) -> Option<ProjectFacts> {
    let project = cfg.projects.iter().find(|p| p.id == id)?;
    let ctx = crate::server::attention::attention_context(state).await;
    facts_with(state, cfg, &ctx, project, Reading::Full).await
}

/// [`project_facts`] com o contexto da atenção já montado e o tanto de leitura pedido.
async fn facts_with(
    state: &AppState,
    cfg: &Config,
    ctx: &crate::server::attention::AttentionContext,
    project: &crate::config::Project,
    reading: Reading,
) -> Option<ProjectFacts> {
    let id = project.id.as_str();
    let root = PathBuf::from(&project.path);
    let attention = crate::server::attention::attention_with(state, ctx, project).await;
    let running_spec_id = state.runs().active(id).map(|run| run.spec_id);
    let pricing = (reading == Reading::Full).then(|| cfg.pricing.clone());
    let mut facts = ProjectFacts { project_id: id.to_string(), running_spec_id, ..ProjectFacts::default() };
    let job = move || {
        if let Some(item) = attention {
            facts.plan_waiting = item.plan_awaiting_approval;
            facts.plan_spec_id = item.plan_spec_id.clone();
            facts.progress = item.progress.clone();
            facts.waiting = Waiting {
                permissions: item.pending_permissions.len(),
                choices: item.pending_choices,
                plan: item.plan_awaiting_approval,
                recovery: item.paused_or_stalled.as_ref().is_some_and(|r| r.escalation_options.is_some()),
            };
            if let Some(run) = &item.paused_or_stalled {
                (facts.stop_code, facts.role) = (run.stop_code.clone(), run.role);
                facts.run_spec_id = Some(run.spec_id.clone());
                facts.stalled = run.kind == "stalled";
                if let (Some(task), true) = (&run.task_id, run.kind == "paused") {
                    facts.blocked = Some(blocked_task(&root, &run.spec_id, task));
                    // Pausada com a parada do provedor ainda valendo (crédito no meio da tarefa).
                    if facts.stop_code.is_none() {
                        (facts.stop_code, facts.role) = crate::server::attention::provider_stop(&root, &run.spec_id);
                    }
                }
            }
        }
        facts.cost_alerts = crate::server::cost_alert::open_alert_count(&root);
        if let (Some(pricing), Some(spec)) = (pricing, facts.phase_spec_id()) {
            let records = crate::server::usage::load_records(&root, Some(spec));
            facts.cost_usd = crate::server::usage::aggregate(&records, &pricing).total.cost_usd;
        }
        facts
    };
    let mut facts = tokio::task::spawn_blocking(job).await.ok()?;
    if let (Reading::Full, Some(kind)) = (reading, facts.phase_kind()) {
        let group = super::signals::group_key(Some(id), kind);
        facts.since = state.nanty().snapshot().alerts.get(&group).map(|a| a.since.clone());
    }
    Some(facts)
}

/// Os projetos observados, cada um pela mesma leitura de [`project_facts`], só com o que os sinais
/// usam e com o contexto da atenção montado uma vez por varredura. `None` se alguma leitura falhou
/// (nada é dado por resolvido).
pub async fn observe_projects(state: &AppState, cfg: &Config) -> Option<Vec<Signal>> {
    let ctx = crate::server::attention::attention_context(state).await;
    let mut out = Vec::new();
    for project in cfg.projects.iter().filter(|p| cfg.nanty.observes(&p.id)) {
        let facts = facts_with(state, cfg, &ctx, project, Reading::Signals).await?;
        out.extend(project_signals(&facts));
    }
    Some(out)
}

/// A fase única de cada projeto observado (spec 139 T03: a lista de projetos da conversa), pela
/// mesma leitura de [`project_facts`], com o contexto da atenção montado uma vez. A fase de um
/// projeto cuja leitura falhou fica `None` (nada é dado por resolvido).
pub async fn observed_phases(state: &AppState, cfg: &Config) -> Vec<(crate::config::Project, Option<Phase>)> {
    let ctx = crate::server::attention::attention_context(state).await;
    let mut out = Vec::new();
    for project in cfg.projects.iter().filter(|p| cfg.nanty.observes(&p.id)) {
        let phase = facts_with(state, cfg, &ctx, project, Reading::Signals).await.map(|f| f.phase());
        out.push((project.clone(), phase));
    }
    out
}

/// Disco, pasta temporária e (com `docker`) os recursos do Docker, pela sonda. `busy`: os projetos
/// ativos pela verificação estrita (`AppState::project_busy_strict`).
pub fn observe_machine(probe: &dyn NantyProbe, cfg: &Config, docker: bool, busy: &[String]) -> Scan {
    let filter = &cfg.nanty.filter;
    let mut scan = Scan::default();
    let disk: Vec<u64> = crate::server::doctor::disk_paths(cfg).iter().filter_map(|p| probe.free_bytes(p)).collect();
    let tmp: Vec<u64> = probe.free_bytes(&std::env::temp_dir()).into_iter().collect();
    for (kind, measured, min_bytes, min_gb) in [
        (SignalKind::DiskLow, disk, filter.disk_min_free_bytes(), filter.disk_min_free_gb),
        (SignalKind::TmpLow, tmp, filter.tmp_min_free_bytes(), filter.tmp_min_free_gb),
    ] {
        if let Some(found) = space_signal(kind, &measured, min_bytes, min_gb) {
            scan.covered.push(kind);
            scan.signals.extend(found);
        }
    }
    if docker {
        if let Some(labeled) = probe.docker_labeled() {
            let known: Vec<String> = cfg.projects.iter().map(|p| p.id.clone()).collect();
            scan.covered.extend([SignalKind::DockerOrphans, SignalKind::DockerForeign]);
            scan.signals.extend(docker_signals(&labeled, &known, busy));
        }
    }
    scan
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod facts_tests;
