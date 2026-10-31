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
//! O disco e o Docker passam por uma sonda ([`NantyProbe`]) que os testes trocam: nenhum teste
//! chama o `docker` de verdade.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use super::signals::{Signal, SignalKind};
use crate::config::{Config, NantyConfig};
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

/// O que se sabe de um projeto para os observadores de atenção, tarefas e custo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// Tarefas em andamento acima do patamar de custo.
    pub cost_alerts: usize,
}

/// Tarefa BLOCKED: só o id (código `Txx`), o código do motivo e a tentativa.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockedTask {
    pub task_id: String,
    pub code: Option<String>,
    pub attempts: u32,
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
        let kind =
            if CREDIT_STOP_CODES.contains(&code.as_str()) { SignalKind::NoCredit } else { SignalKind::ProviderStopped };
        let mut signal = Signal::new(kind, id).fact("stop", code.as_str());
        if let Some(role) = f.role {
            signal = signal.fact("role", role);
        }
        out.push(with_spec(signal, &f.run_spec_id));
    }
    if let Some(task) = &f.blocked {
        let kind = match task.code.as_deref() {
            Some(BUDGET_CODE) => SignalKind::Budget,
            Some(REVIEW_LOOP_CODE) => SignalKind::ReviewLoop,
            _ => SignalKind::RunBlocked,
        };
        let mut signal = Signal::new(kind, id).fact("task_id", task.task_id.as_str()).fact("attempts", task.attempts);
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
        attempts: meta.map_or(0, |m| m.attempts),
    }
}

/// Os projetos observados: a visão de atenção, a tarefa BLOCKED e o custo. `None` se a leitura
/// falhou (nada é dado por resolvido).
pub async fn observe_projects(state: &AppState, cfg: &Config) -> Option<Vec<Signal>> {
    let attention = crate::server::attention::projects_attention(state).await;
    let projects: Vec<(String, PathBuf)> = cfg
        .projects
        .iter()
        .filter(|p| cfg.nanty.observes(&p.id))
        .map(|p| (p.id.clone(), PathBuf::from(&p.path)))
        .collect();
    let job = move || {
        let mut out = Vec::new();
        for (id, root) in projects {
            let mut facts = ProjectFacts { project_id: id.clone(), ..ProjectFacts::default() };
            if let Some((_, _, item)) = attention.iter().find(|(pid, _, _)| *pid == id) {
                facts.plan_waiting = item.plan_awaiting_approval;
                facts.plan_spec_id = item.plan_spec_id.clone();
                if let Some(run) = &item.paused_or_stalled {
                    (facts.stop_code, facts.role) = (run.stop_code.clone(), run.role);
                    facts.run_spec_id = Some(run.spec_id.clone());
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
            out.extend(project_signals(&facts));
        }
        out
    };
    tokio::task::spawn_blocking(job).await.ok()
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
