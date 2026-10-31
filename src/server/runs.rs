//! `POST /api/projects/:id/specs/:spec_id/approve`: aprovação de spec e execução em background.
//!
//! Fluxo: validar ids e corpo; checar o projeto (404); reservar a execução do projeto
//! (uma por projeto, 409 se ocupada); ler spec, tarefas e settings (409 com `reason` fixo);
//! checar o Git da base (sem I/O sob lock, com timeout de [`GIT_TIMEOUT`]); montar Desenvolvedor
//! e Revisor a partir da configuração (422 antes de qualquer escrita); gravar `IN_PROGRESS` com
//! escrita atômica; publicar `project_status: executing` e disparar o [`TaskRunner`] em segundo
//! plano. Ao terminar, publica o status final (`integrated`, `blocked`, `idle`).
//!
//! Retomada (`POST /api/projects/:id/specs/:spec_id/resume`): spec `IN_PROGRESS` sem execução ativa
//! e com ao menos uma tarefa BLOCKED. Usa as mesmas fases A e B ([`Mode::Resume`]): a fase B reabre
//! as tarefas BLOCKED (TODO, `attempts` 0, `next_action` limpo, corpo preservado) com escrita atômica
//! e compensação, sem regravar a spec, e dispara o runner. A base não precisa estar limpa além do que
//! a aprovação exige: `.sdd/settings.yaml` nunca conta como sujeira.
//!
//! Motivos (`reason`) de `project_status: idle` publicados: `error` (falha de montagem ou
//! panic, com spec restaurada), `shutdown` (cancelamento ou shutdown antes do disparo, com spec
//! restaurada), `shutdown_forced` (runner abandonado após o prazo), `stalled`, `restore_failed`,
//! `stopped` (parada pedida pelo usuário, spec 086 T04).
//!
//! Parada (`POST /api/projects/:id/specs/:spec_id/stop`, spec 086 T04): aciona a flag de cancelamento
//! cooperativo da execução ativa daquela spec (a mesma do shutdown, guardada na vaga do projeto) e
//! responde 202 `{"stopping": true}`, idempotente; sem execução ativa daquela spec, 409. O runner e as
//! sessões de agente checam a flag entre etapas e entre chamadas ao modelo/ferramentas; o
//! `test_command` em curso e a etapa de integração terminam antes. A spec fica `IN_PROGRESS` sem
//! BLOCKED, e o "Continuar" da UI retoma pelo `resume`.
//!
//! Recuperação manual (`restore_failed`, com `needs_manual_recovery: true`): a regravação do
//! `spec.md` original falhou (IO ou permissão). Nesse caso a spec permanece `IN_PROGRESS` sem
//! runner e o novo `approve` responde 409. A recuperação é manual: editar `spec.md` para
//! `AWAITING_APPROVAL` ou reexecutar o runner pelo CLI ou pela sessão. Não há endpoint novo.
//!
//! Limitações documentadas:
//! - `waiting_approval` não é publicado: o pedido HITL já gera `permission_requested`, e publicar
//!   o status exigiria acoplar o aprovador ao registro de execução.
//! - No cancelamento por shutdown, a chamada em andamento (LLM ou `test_command`) é abandonada.
//!   O estado persistido pelo runner continua retomável; esta camada não força estado.
//! - Erros do runner nunca são ecoados: o evento leva texto fixo.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::watch;

use super::approvals::{replace_atomic, ApprovalRegistry, ProjectApprover, RunScopeGuard, PERSIST_LOCK};
use super::error::{params, ApiError};
use crate::i18n::Lang;
use super::dialog::{deliver, DialogTarget};
use crate::agents::activity::ActivitySink;
use super::events::{validate_project_id, ApiEvent, BusActivitySink, BusSink, EventBus, EventType, SecretSet};
use super::projects::{check_local_excludes, ensure_local_excludes, is_git_repo, scrub_text};
use super::{recovery, report};
use super::safe_fs::{self, SafeFsError, MAX_READ_BYTES, O_NOFOLLOW};
use super::state::{AppState, ReserveError, INTERNAL_RESERVATION_PREFIX, REPORT_BUSY_CODE, REPORT_RESERVATION};
use crate::agents::chat::ChatBackend;
use crate::agents::llm_agents::{LlmDeveloper, LlmReviewer};
use crate::agents::llm_client::{ConnectionRef, FailureEvidence, LlmClient, ProviderFailure};
use crate::agents::runner::{
    is_integrated, BlockKind, Fut, RunnerError, ShellTestExecutor, SpecOutcome, TaskRunner,
};
use crate::config::{AgentConfig, Config, Effort, Project};
use crate::harness::allowlist::Policy;
use crate::harness::git::Git;
use crate::sdd::{Settings, Spec, SpecStatus, Task, TaskStatus};

/// Tempo máximo das consultas ao Git antes da aprovação (10 s).
pub const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Tempo que o shutdown espera o runner terminar a etapa corrente (cancelamento cooperativo).
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);
/// Tamanho máximo de `spec_id`.
const MAX_SPEC_ID_LEN: usize = 64;
/// Diretórios e arquivos do projeto usados pela aprovação (relativos à raiz).
const SPECS_DIR: &str = ".sdd/specs";
const SPECS_PREFIX: &str = ".sdd/specs/";
/// Perfis de especialista (spec 063): sujos não bloqueiam a base, como `.sdd/specs/`.
const SKILLS_PREFIX: &str = ".sdd/skills/";
const SETTINGS_REL: &str = ".sdd/settings.yaml";
const SPEC_FILE: &str = "spec.md";
const TASKS_DIR: &str = "tasks";
/// Worktrees das tarefas, excluídas da checagem de limpeza da base (como no runner).
const WORKTREES_EXCLUDE: &str = ".sdd/worktrees";
/// Status exposto em `run.status` enquanto o runner executa.
const EXECUTING: &str = "executing";

const SPEC_ID_MSG: &str = "spec_id_invalid";
const PROJECT_NOT_FOUND: &str = "project_not_found";
const SPEC_NOT_FOUND: &str = "spec_not_found";
const INVALID_SPEC: &str = "spec_invalid";
const SPEC_DRAFT: &str = "spec_draft";
const SPEC_NOT_AWAITING: &str = "spec_not_awaiting";
const SPEC_NOT_IN_PROGRESS: &str = "spec_not_in_progress";
const NO_BLOCKED_TASK: &str = "no_blocked_task";
const SPEC_CHANGED: &str = "spec_changed_approval";
const TASKS_CHANGED: &str = "tasks_changed_resume";
const BUSY: &str = "run_active";
const SHUTTING_DOWN_MSG: &str = "shutting_down";
/// Motivo do `idle` quando o shutdown interrompe a aprovação antes do runner começar.
const SHUTDOWN_REASON: &str = "shutdown";
/// Motivo do `idle` quando o usuário para a execução (spec 086 T04).
const STOPPED_REASON: &str = "stopped";
/// 409 da parada sem execução ativa daquela spec.
const NOT_RUNNING_MSG: &str = "no_run_for_spec";
const GIT_TIMEOUT_MSG: &str = "git_timeout";
const WRITE_FAILED: &str = "spec_write_failed";
const INTERNAL: &str = "internal";
const SETTINGS_MSG: &str = "settings_invalid";
const TEST_COMMAND_MSG: &str = "test_command_missing";
const BASE_BRANCH_MSG: &str = "base_branch_missing";
const NO_TASKS_MSG: &str = "no_tasks";
const INVALID_TASK_MSG: &str = "invalid_task";
const DEP_MISSING_MSG: &str = "dependency_missing";
const DEP_CYCLE_MSG: &str = "dependency_cycle";
const NOT_GIT_MSG: &str = "not_git_repo";
const BASE_DIRTY_MSG: &str = "base_dirty";
/// `reason` fixo das respostas 409 de validação.
const REASON_NO_TASKS: &str = "no_tasks";
/// Aprovação de plano sem nenhuma tarefa (spec 080 T01). A retomada mantém `no_tasks`.
const REASON_PLAN_WITHOUT_TASKS: &str = "plan_without_tasks";
/// Aprovação recusada enquanto o Arquiteto ainda elabora o plano (spec 080 T01).
const REASON_ARCHITECT_TURN: &str = "architect_turn_active";
const ARCHITECT_TURN_MSG: &str = "architect_turn_active";
/// Aprovação recusada enquanto o Revisor revisa o plano (spec 128 T04: a renumeração nunca corre
/// junto com a revisão).
const REASON_PLAN_REVIEW_RUNNING: &str = "plan_review_running";
const PLAN_REVIEW_RUNNING_MSG: &str = "plan_review_running";
/// Falha ao gravar a renumeração das tarefas (spec 128 T04): tudo é desfeito.
const RENUMBER_FAILED: &str = "plan_renumber_failed";
/// Código da linha da renumeração na Conversa (`system.<código>` nos dicionários).
pub const RENUMBERED_CODE: &str = "plan.renumbered";
const REASON_INVALID_TASK: &str = "invalid_task";
const REASON_DEP_MISSING: &str = "dependency_missing";
const REASON_DEP_CYCLE: &str = "dependency_cycle";
const REASON_TEST_COMMAND: &str = "test_command_missing";
const REASON_BASE_BRANCH: &str = "base_branch_missing";
const REASON_SETTINGS: &str = "settings_invalid";
const REASON_ENV_INVALID: &str = "environment_invalid";
const REASON_ENV_CHANGED: &str = "environment_changed";
const ENV_CHANGED_MSG: &str = "environment_changed";
const ARCH_INVALID_MSG: &str = "architecture_invalid";
const ARCH_COMMIT_MSG: &str = "architecture_commit_failed";
const ARCH_FILE: &str = crate::agents::arch_memory::ARCH_FILE;
const SKILLS_INVALID_MSG: &str = "skills_invalid";
const SKILLS_COMMIT_MSG: &str = "skills_commit_failed";
pub(crate) const REASON_ENV_NOT_DOCKER: &str = "environment_not_docker";
const REASON_DOCKER_UNAVAILABLE: &str = "docker_unavailable";
const PLAN_NOT_DOCKER_MSG: &str = "plan_not_docker";
pub(crate) const PROJECT_NOT_DOCKER_CODE: &str = "project_not_docker";
const ENV_DENIED_MSG: &str = "environment_denied";
const ENV_COMMIT_MSG: &str = "environment_commit_failed";
const ENV_TRACKED_MSG: &str = "environment_settings_tracked";
const REASON_NOT_GIT: &str = "not_git_repo";
const REASON_BASE_DIRTY: &str = "base_dirty";
/// Motivo do 409 enquanto o relatório final da spec anterior é gerado.
const REASON_BUDGET: &str = "budget_exceeded";
const REASON_REPORT_IN_PROGRESS: &str = "report_in_progress";
/// `reason` do evento `blocked` (o motivo do runner não é ecoado).
const BLOCKED_REASON: &str = "task_blocked";
/// Teto (bytes) da causa exibida na mensagem do sistema de falha (spec 081 T02).
const RUN_FAILED_CAUSE_MAX_BYTES: usize = 300;
/// Agente usado nas mensagens emitidas pelo próprio sistema.
const SYSTEM_AGENT: &str = "system";

/// Sequência para nomes temporários únicos da escrita atômica.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
/// Serializa a leitura-comparação-escrita de `spec.md` entre aprovações. Sem I/O sob o lock
/// de configuração; o lock é segurado só dentro de `blocking`.
static SPEC_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Entrada do corpo de aprovação. Só `{}` é aceito (ou corpo vazio).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveInput {}

/// Recusa das rotas de execução: um [`ApiError`] com código (spec 115 T02) e, às vezes, `reason`.
type Rejection = ApiError;

fn conflict(code: &'static str) -> Rejection {
    ApiError::coded(StatusCode::CONFLICT, code, Map::new())
}

fn conflict_reason(code: &'static str, reason: &'static str) -> Rejection {
    conflict(code).with_reason(reason)
}

fn invalid_spec() -> Rejection {
    conflict(INVALID_SPEC)
}

fn internal(code: &'static str) -> Rejection {
    ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, code, Map::new())
}

/// Recusa com o status dado e sem parâmetros.
fn rejection(status: StatusCode, code: &'static str) -> Rejection {
    ApiError::coded(status, code, Map::new())
}

/// Papel de agente da execução: backend pronto, modelo e esforço.
pub struct AgentSetup {
    pub backend: Arc<dyn ChatBackend>,
    pub model: String,
    pub effort: Effort,
    /// Teto de passos configurado (spec 081 T01), já ajustado à faixa; `None` = padrão do papel.
    pub max_steps: Option<usize>,
    /// Janela de entrada do modelo, base do limiar do resumo do histórico: a da lista de modelos do
    /// provedor nativo (spec 132 T08), senão a do catálogo de preços em cache (spec 122 T04);
    /// `None` = o padrão declarado.
    pub context_window: Option<usize>,
    /// Tipo da conexão do papel: escolhe o preço do modelo no catálogo (spec 133 T04).
    pub kind: crate::config::ConnectionKind,
    /// Preços de cache do modelo (spec 133 T04, C-01): os da tabela de preços, senão os do catálogo
    /// em cache ([`resolve_cache_prices`]); pesam o custo de
    /// compactar o histórico. `None` = a regra só pelo tamanho.
    pub cache_prices: Option<crate::agents::cache_cost::CachePrices>,
}

/// Entradas para montar o runner de um projeto. Já validadas pelo handler.
pub struct RunJob {
    pub project_id: String,
    pub spec_id: String,
    pub root: PathBuf,
    pub secrets: Arc<SecretSet>,
    pub policy: Policy,
    pub developer: AgentSetup,
    pub reviewer: AgentSetup,
    /// Modelo do Arquiteto para `ask_architect` (spec 055); ausente = ferramenta sem conselheiro.
    pub architect: Option<AgentSetup>,
    pub approvals: ApprovalRegistry,
    pub events: EventBus,
    /// Escopo do run: concessões `run` e `test_command` valem enquanto ele existir.
    pub run: Arc<RunScopeGuard>,
    /// Sinal de cancelamento cooperativo, repassado ao `TaskRunner`.
    pub cancel: Arc<AtomicBool>,
    /// Parada pedida pelo usuário (spec 087 T06): libera as esperas de permissão e do `ask_architect`.
    pub stop: watch::Receiver<bool>,
    /// Preços por modelo (snapshot da config), para o orçamento em US$.
    pub pricing: std::collections::BTreeMap<String, crate::config::Pricing>,
    /// Ambiente docker do projeto (`-test`): testes, checks e `run_command` rodam no workspace.
    pub compose: Option<Arc<crate::harness::compose::ComposeEnv>>,
    /// Gerador de imagens do Desenvolvedor (spec 090 T02); ausente = sem `generate_image`.
    pub images: Option<super::image_gen::LlmImageGenerator>,
    /// Idioma ativo da interface no projeto (spec 102): Desenvolvedor e Revisor o leem a cada sessão.
    pub lang: crate::i18n::SharedLang,
}

/// Execução de uma spec. Implementada por [`TaskRunner`] e por fakes em testes.
pub trait SpecRunner: Send + Sync {
    fn run<'a>(&'a self, spec_id: &'a str) -> Fut<'a, Result<SpecOutcome, RunnerError>>;

    /// Registra a análise de recuperação do Arquiteto no corpo da tarefa bloqueada (com `retry`,
    /// zera `attempts` e volta a `TODO`). Pelo mesmo caminho de escrita do runner. Devolve o ciclo.
    fn record_recovery(
        &self,
        _spec_id: &str,
        _task_id: &str,
        _retry: bool,
        _text: &str,
    ) -> Result<usize, RunnerError> {
        Err(RunnerError::Invalid("recuperação não suportada por este runner".into()))
    }

    /// Registra o diagnóstico de ambiente do Arquiteto no corpo da tarefa bloqueada (sem mudar o
    /// status). Devolve o número da seção.
    fn record_diagnosis(&self, _spec_id: &str, _task_id: &str, _text: &str) -> Result<usize, RunnerError> {
        Err(RunnerError::Invalid("diagnóstico não suportado por este runner".into()))
    }

    /// REVISE_TASK: reescreve a definição da tarefa bloqueada e a reabre (ver
    /// `TaskRunner::record_revision`). Devolve o ciclo.
    fn record_revision(
        &self,
        _spec_id: &str,
        _task_id: &str,
        _revision: &crate::agents::runner::TaskRevision,
        _text: &str,
    ) -> Result<usize, RunnerError> {
        Err(RunnerError::Invalid("recuperação não suportada por este runner".into()))
    }

    /// SPLIT (spec 124 T03): grava as tarefas novas e a definição menor da atual, que é reaberta
    /// (ver `TaskRunner::record_split`). Devolve o ciclo e o texto registrado.
    fn record_split(
        &self,
        _spec_id: &str,
        _task_id: &str,
        _split: &crate::agents::recovery_plan::TaskSplit,
        _text: &str,
    ) -> Result<(usize, String), RunnerError> {
        Err(RunnerError::Invalid("recuperação não suportada por este runner".into()))
    }

    /// ADJUST (spec 124 T03): ajusta os critérios pelo caminho da arbitragem e reabre a tarefa (ver
    /// `TaskRunner::record_adjust`). Devolve o ciclo e o texto registrado.
    fn record_adjust(
        &self,
        _spec_id: &str,
        _task_id: &str,
        _adjustment: &crate::agents::recovery_plan::CriteriaAdjustment,
        _text: &str,
    ) -> Result<(usize, String), RunnerError> {
        Err(RunnerError::Invalid("recuperação não suportada por este runner".into()))
    }
}

impl SpecRunner for TaskRunner {
    fn run<'a>(&'a self, spec_id: &'a str) -> Fut<'a, Result<SpecOutcome, RunnerError>> {
        Box::pin(self.run_spec(spec_id))
    }

    fn record_recovery(
        &self,
        spec_id: &str,
        task_id: &str,
        retry: bool,
        text: &str,
    ) -> Result<usize, RunnerError> {
        TaskRunner::record_recovery(self, spec_id, task_id, retry, text)
    }

    fn record_diagnosis(&self, spec_id: &str, task_id: &str, text: &str) -> Result<usize, RunnerError> {
        TaskRunner::record_diagnosis(self, spec_id, task_id, text)
    }

    fn record_revision(
        &self,
        spec_id: &str,
        task_id: &str,
        revision: &crate::agents::runner::TaskRevision,
        text: &str,
    ) -> Result<usize, RunnerError> {
        TaskRunner::record_revision(self, spec_id, task_id, revision, text)
    }

    fn record_split(
        &self,
        spec_id: &str,
        task_id: &str,
        split: &crate::agents::recovery_plan::TaskSplit,
        text: &str,
    ) -> Result<(usize, String), RunnerError> {
        TaskRunner::record_split(self, spec_id, task_id, split, text)
    }

    fn record_adjust(
        &self,
        spec_id: &str,
        task_id: &str,
        adjustment: &crate::agents::recovery_plan::CriteriaAdjustment,
        text: &str,
    ) -> Result<(usize, String), RunnerError> {
        TaskRunner::record_adjust(self, spec_id, task_id, adjustment, text)
    }
}

/// Monta o runner de uma execução. A produção usa [`ConfiguredRunnerFactory`].
pub trait RunnerFactory: Send + Sync + std::fmt::Debug {
    /// Erro de montagem: a aprovação é desfeita (spec volta a `AWAITING_APPROVAL`).
    fn build(&self, job: RunJob) -> Result<Box<dyn SpecRunner>, RunnerError>;
}

/// Fábrica de produção: ports reais (LLM por conexão, `test_command` real, eventos no barramento).
#[derive(Debug, Default)]
pub struct ConfiguredRunnerFactory;

impl RunnerFactory for ConfiguredRunnerFactory {
    fn build(&self, job: RunJob) -> Result<Box<dyn SpecRunner>, RunnerError> {
        let usage = super::usage::FileUsageSink::redacting(job.root.clone(), job.project_id.clone(), Arc::clone(&job.secrets));
        let mut approver = ProjectApprover::new(
            job.project_id.clone(),
            job.root.clone(),
            job.approvals,
            job.events.clone(),
            Arc::clone(&job.secrets),
        )
        .with_run(job.run)
        .with_dialog(DialogTarget::new(job.root.clone(), job.spec_id.clone()))
        .with_stop(job.stop.clone())
        .with_lang(job.lang.clone());
        // Juiz do YOLO (spec 062): o modelo do Arquiteto, custo contabilizado como `architect`.
        if let Some(a) = &job.architect {
            approver = approver.with_judge(Arc::new(
                super::judge::ArchitectJudge::new(Arc::clone(&a.backend), a.model.clone(), Some(a.effort), Arc::clone(&job.secrets))
                    .with_usage(usage.clone()),
            ));
        }
        let approver = Arc::new(approver);
        // Trilha de ferramentas da spec (085): rótulos de ferramenta e comando em `tools.jsonl`.
        let activity: Arc<dyn ActivitySink> = Arc::new(
            BusActivitySink::new(job.events.clone(), job.project_id.clone(), Arc::clone(&job.secrets))
                .with_trail(job.root.clone(), &job.spec_id),
        );
        // Spec 124 T02: o Arquiteto julga as contestações do Desenvolvedor antes da próxima revisão.
        let arbiter: Option<Arc<dyn crate::agents::runner::ArbiterPort>> = job.architect.as_ref().map(|a| {
            Arc::new(super::arbiter::ArchitectArbiter {
                backend: Arc::clone(&a.backend),
                model: a.model.clone(),
                effort: Some(a.effort),
                policy: job.policy.clone(),
                approver: approver.clone(),
                activity: activity.clone(),
                usage: Some(usage.clone()),
                secrets: Arc::clone(&job.secrets),
                lang: job.lang.clone(),
                stop: Some(job.stop.clone()),
                project_root: job.root.clone(),
            }) as Arc<dyn crate::agents::runner::ArbiterPort>
        });
        // Modo docker (único): testes, checks e run_command no Compose `-test`.
        let compose = job.compose.clone();
        let baselines: crate::agents::runner::SharedBaselines = Default::default();
        let ask_limit = crate::sdd::read_settings(&crate::sdd::sdd_dir(&job.root).join("settings.yaml"))
            .ok()
            .and_then(|s| s.ask_architect_limit)
            .unwrap_or(crate::harness::tools::DEFAULT_ASK_LIMIT);
        let advisor: Option<Arc<dyn crate::harness::tools::Advisor>> = job.architect.map(|a| {
            let events: Arc<dyn crate::agents::runner::EventSink> = Arc::new(
                BusSink::new(job.events.clone(), job.project_id.clone(), Arc::clone(&job.secrets))
                    .with_dialog(DialogTarget::new(job.root.clone(), job.spec_id.clone())),
            );
            Arc::new(
                super::advisor::ArchitectAdvisor::new(
                    a.backend,
                    a.model,
                    Some(a.effort),
                    job.root.clone(),
                    Arc::clone(&job.secrets),
                    crate::agents::runner::TaskEditor::new(job.root.clone(), Arc::clone(&baselines)),
                    events,
                )
                .with_usage(usage.clone())
                .with_stop(job.stop.clone())
                .with_lang(job.lang.clone()),
            ) as Arc<dyn crate::harness::tools::Advisor>
        });
        let developer = LlmDeveloper::new(
            job.developer.backend,
            job.developer.model,
            Some(job.developer.effort),
            job.policy.clone(),
            approver.clone(),
        )
        .with_activity(activity.clone())
        .with_usage(usage.clone())
        .with_memory(Arc::clone(&job.secrets))
        .with_cancel(Arc::clone(&job.cancel))
        .with_lang(job.lang.clone())
        .with_context_window(job.developer.context_window)
        .with_cache_prices(job.developer.cache_prices)
        .with_browser(Arc::new(crate::harness::browser::BrowserTests::real(job.project_id.clone())));
        let developer = match job.developer.max_steps {
            Some(n) => developer.with_max_steps(n),
            None => developer,
        };
        let developer = match &compose {
            Some(c) => developer.with_compose(Arc::clone(c)),
            None => developer,
        };
        let developer = match &advisor {
            Some(a) => developer.with_advisor(Arc::clone(a), ask_limit),
            None => developer,
        };
        // Spec 090 T02: a recusa do provedor vira mensagem de sistema única nesta execução.
        let developer = match job.images {
            Some(generator) => {
                let notice: Arc<dyn crate::agents::runner::EventSink> = Arc::new(
                    BusSink::new(job.events.clone(), job.project_id.clone(), Arc::clone(&job.secrets))
                        .with_dialog(DialogTarget::new(job.root.clone(), job.spec_id.clone())),
                );
                developer.with_image_generator(Arc::new(generator.with_notice(notice).with_lang(job.lang.clone())))
            }
            None => developer,
        };
        let final_gate: Arc<dyn crate::agents::runner::FinalGate> = approver.clone();
        let reviewer = LlmReviewer::new(
            job.reviewer.backend,
            job.reviewer.model,
            Some(job.reviewer.effort),
            job.policy,
            approver,
        )
        .with_activity(activity)
        .with_usage(usage)
        .with_memory(Arc::clone(&job.secrets))
        .with_cancel(Arc::clone(&job.cancel))
        .with_lang(job.lang.clone())
        .with_context_window(job.reviewer.context_window)
        .with_cache_prices(job.reviewer.cache_prices);
        let reviewer = match job.reviewer.max_steps {
            Some(n) => reviewer.with_max_steps(n),
            None => reviewer,
        };
        let reviewer = match &advisor {
            Some(a) => reviewer.with_advisor(Arc::clone(a), ask_limit),
            None => reviewer,
        };
        let dialog = DialogTarget::new(job.root.clone(), job.spec_id.clone());
        let budget_root = job.root.clone();
        let trail_root = job.root.clone();
        let runner_secrets = Arc::clone(&job.secrets);
        let (events_for_final, project_for_final, runner_secrets_final, spec_for_final) = (
            job.events.clone(),
            job.project_id.clone(),
            Arc::clone(&job.secrets),
            job.spec_id.clone(),
        );
        let sink = BusSink::new(job.events, job.project_id, job.secrets).with_dialog(dialog);
        let runner = TaskRunner::new(
            job.root,
            Box::new(developer),
            Box::new(reviewer),
            Box::new(ShellTestExecutor::new().with_lang(job.lang.clone())),
            Box::new(sink),
        )
        .with_cancel(job.cancel)
        .with_baselines(baselines)
        .with_secrets(runner_secrets)
        .with_final_gate(final_gate)
        .with_final_activity(
            Arc::new(
                BusActivitySink::new(events_for_final, project_for_final, Arc::clone(&runner_secrets_final))
                    .with_stage(crate::agents::runner::FINAL_VERIFICATION_STAGE, crate::agents::runner::FINAL_VERIFICATION_STAGE)
                    .with_trail(trail_root, &spec_for_final),
            ),
            &spec_for_final,
        )
        .with_budget(super::usage::FileBudgetGuard::new(budget_root, job.pricing))
        .with_lang(job.lang);
        let runner = match compose {
            Some(c) => runner.with_compose(c),
            None => runner,
        };
        let runner = match arbiter {
            Some(a) => runner.with_arbiter(a),
            None => runner,
        };
        Ok(Box::new(runner))
    }
}

/// Grava o hash do ambiente aprovado pelo humano durante a execução (mudança de ambiente) em
/// `.sdd/settings.yaml` da raiz do projeto, sob o `PERSIST_LOCK`. Os arquivos `.nants/*` mudam na
/// worktree e entram na base com o squash da tarefa, quando o hash passa a bater.
pub(super) fn persist_environment_hash(root: &Path, hash: &str) -> Result<(), String> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut settings = read_settings_checked(root).map_err(|_| SETTINGS_SAVE_MSG.to_string())?;
    settings.approve_environment_hash(hash);
    let rendered = settings.render().map_err(|_| SETTINGS_SAVE_MSG.to_string())?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| SETTINGS_SAVE_MSG.to_string())
}

/// Execução de um projeto, para `GET /api/projects/:id/state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunView {
    pub spec_id: String,
    pub status: &'static str,
}

/// Pedido de parada de uma execução (spec 086 T04). `cancel` é a flag de cancelamento cooperativo
/// repassada ao runner e às sessões de agente (a mesma que o shutdown aciona); `user` marca que a
/// parada veio do usuário (desfecho `stopped`, não `shutdown`) e acorda as esperas da recuperação.
/// Vive na vaga do projeto: o relançamento do mesmo job (mesma reserva) enxerga o mesmo pedido.
#[derive(Debug, Clone)]
pub struct StopSignal {
    cancel: Arc<AtomicBool>,
    user: Arc<watch::Sender<bool>>,
}

impl Default for StopSignal {
    fn default() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            user: Arc::new(watch::channel(false).0),
        }
    }
}

impl StopSignal {
    /// Parada pedida pelo usuário: aciona o cancelamento cooperativo. Idempotente.
    pub fn request(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.user.send_replace(true);
    }

    /// `true` depois de [`StopSignal::request`].
    pub fn requested(&self) -> bool {
        *self.user.borrow()
    }

    /// Flag de cancelamento cooperativo (runner e sessões de agente).
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// Receptor que muda para `true` quando o usuário pede a parada.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.user.subscribe()
    }
}

/// Vaga de um projeto: nome da reserva (spec ou sentinela `__…`) e pedido de parada da execução.
#[derive(Debug)]
struct Slot {
    name: String,
    stop: StopSignal,
}

/// Registro de execuções: no máximo uma por projeto. Clones compartilham o mapa.
/// Mapa protegido por `Mutex` std; nenhum I/O ocorre sob o lock.
#[derive(Debug, Clone, Default)]
pub struct RunRegistry {
    inner: Arc<Mutex<HashMap<String, Slot>>>,
}

/// Projeto já tem uma execução em andamento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

impl RunRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserva a execução do projeto. A reserva é liberada ao soltar o [`RunGuard`].
    pub fn try_start(&self, project_id: &str, spec_id: &str) -> Result<RunGuard, Busy> {
        let mut map = self.lock();
        if map.contains_key(project_id) {
            return Err(Busy);
        }
        map.insert(
            project_id.to_string(),
            Slot { name: spec_id.to_string(), stop: StopSignal::default() },
        );
        Ok(RunGuard {
            registry: self.clone(),
            project_id: project_id.to_string(),
        })
    }

    /// `true` com qualquer vaga do projeto ocupada, inclusive as reservas internas (`__app__`,
    /// `__survey__`, `__report__`): a verificação estrita de atividade do Nanty (spec 138 T03, C-16).
    pub fn holds(&self, project_id: &str) -> bool {
        self.lock().contains_key(project_id)
    }

    /// `true` quando a reserva do projeto é a geração do relatório final (sem execução ativa).
    pub fn is_report(&self, project_id: &str) -> bool {
        self.lock().get(project_id).is_some_and(|slot| slot.name == REPORT_RESERVATION)
    }

    /// Troca o nome da reserva mantendo a vaga: a execução concluída passa a ser o relatório.
    fn retag(&self, project_id: &str, name: &str) {
        if let Some(slot) = self.lock().get_mut(project_id) {
            slot.name = name.to_string();
        }
    }

    /// Execução em andamento do projeto, se houver.
    /// Reservas internas (sentinelas `__…`, de remoção e de configuração) não são execuções.
    pub fn active(&self, project_id: &str) -> Option<RunView> {
        self.lock()
            .get(project_id)
            .filter(|slot| !slot.name.starts_with(INTERNAL_RESERVATION_PREFIX))
            .map(|slot| RunView {
                spec_id: slot.name.clone(),
                status: EXECUTING,
            })
    }

    /// Pede a parada da execução ativa de `spec_id` no projeto (idempotente). `false` quando não há
    /// execução ativa dessa spec (vaga livre, de outra spec ou reserva interna, como o relatório).
    pub fn request_stop(&self, project_id: &str, spec_id: &str) -> bool {
        let map = self.lock();
        match map.get(project_id) {
            Some(slot) if slot.name == spec_id && !slot.name.starts_with(INTERNAL_RESERVATION_PREFIX) => {
                slot.stop.request();
                true
            }
            _ => false,
        }
    }

    /// Pedido de parada da vaga do projeto; vaga ausente devolve um sinal novo (nunca acionado).
    fn stop_signal(&self, project_id: &str) -> StopSignal {
        self.lock().get(project_id).map(|slot| slot.stop.clone()).unwrap_or_default()
    }

    fn release(&self, project_id: &str) {
        self.lock().remove(project_id);
    }
}

/// Reserva de execução; libera a vaga do projeto ao ser descartada (inclusive em panic).
#[derive(Debug)]
pub struct RunGuard {
    registry: RunRegistry,
    project_id: String,
}

impl RunGuard {
    /// Pedido de parada desta reserva (o mesmo para o run e o relançamento que a herdam).
    pub fn stop_signal(&self) -> StopSignal {
        self.registry.stop_signal(&self.project_id)
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.registry.release(&self.project_id);
    }
}

/// Modo da execução: aprovação de spec `AWAITING_APPROVAL` ou retomada de spec `IN_PROGRESS` com
/// tarefas BLOCKED. As fases A e B são as mesmas; só a validação inicial e a escrita da fase B mudam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Approve,
    Resume,
}

/// Congela as fontes da base citadas na spec e nas tarefas (spec 134 T05, ver
/// [`crate::kb::cite::freeze`]). Sem documento pronto e sem citação, nada é gravado.
fn freeze_kb_sources(root: &Path, spec_id: &str, secrets: &SecretSet) -> Result<bool, crate::kb::KbError> {
    let spec_dir = format!(".sdd/specs/{spec_id}");
    let mut texts: Vec<String> = Vec::new();
    texts.extend(safe_fs::read_regular(root, &format!("{spec_dir}/spec.md"), MAX_READ_BYTES).ok());
    for name in super::plan_review::task_names(root, spec_id) {
        texts.extend(safe_fs::read_regular(root, &format!("{spec_dir}/tasks/{name}"), MAX_READ_BYTES).ok());
    }
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let now = super::projects::now_rfc3339();
    Ok(crate::kb::cite::freeze(root, spec_id, &refs, secrets, &now)?.is_some())
}

/// `POST /api/projects/:id/specs/:spec_id/approve`
pub async fn approve_spec(
    State(state): State<AppState>,
    AxPath((id, spec_id)): AxPath<(String, String)>,
    body: Bytes,
) -> Response {
    match start(&state, &id, &spec_id, &body, Mode::Approve).await {
        Ok(resp) => resp,
        Err(rejection) => rejection.into_response(),
    }
}

/// `POST /api/projects/:id/specs/:spec_id/stop` (spec 086 T04): pede a parada da execução ativa da spec.
/// 202 `{"stopping": true}` (também num segundo pedido enquanto para); 409 sem execução ativa dessa spec;
/// 404 projeto ou spec desconhecidos; 422 ids inválidos.
pub async fn stop_spec(
    State(state): State<AppState>,
    AxPath((id, spec_id)): AxPath<(String, String)>,
    body: Bytes,
) -> Response {
    match stop(&state, &id, &spec_id, &body).await {
        Ok(resp) => resp,
        Err(rejection) => rejection.into_response(),
    }
}

/// Parada de uma execução; também chamada por dentro, sem HTTP (spec 138 T03, C-03).
pub(crate) async fn stop(state: &AppState, id: &str, spec_id: &str, body: &[u8]) -> Result<Response, Rejection> {
    validate_project_id(id)?;
    validate_spec_id(spec_id)?;
    parse_body(body)?;
    let project = {
        let cfg = state.config().read().await;
        cfg.projects.iter().find(|p| p.id == id).cloned()
    }
    .ok_or_else(|| rejection(StatusCode::NOT_FOUND, PROJECT_NOT_FOUND))?;
    if state.runs().request_stop(id, spec_id) {
        return Ok((StatusCode::ACCEPTED, Json(json!({"stopping": true}))).into_response());
    }
    // Sem execução ativa dessa spec: spec desconhecida é 404; conhecida, 409.
    let (root, rel) = (PathBuf::from(&project.path), spec_rel(spec_id));
    let missing = blocking(move || {
        Ok(matches!(safe_fs::read_regular(&root, &rel, MAX_READ_BYTES), Err(SafeFsError::Missing)))
    })
    .await?;
    if missing {
        return Err(rejection(StatusCode::NOT_FOUND, SPEC_NOT_FOUND));
    }
    Err(conflict(NOT_RUNNING_MSG))
}

/// `POST /api/projects/:id/specs/:spec_id/resume`: retoma uma spec `IN_PROGRESS` cuja execução
/// parou em tarefa BLOCKED. Mesmas validações e mesma resposta da aprovação.
pub async fn resume_spec(
    State(state): State<AppState>,
    AxPath((id, spec_id)): AxPath<(String, String)>,
    body: Bytes,
) -> Response {
    match start(&state, &id, &spec_id, &body, Mode::Resume).await {
        Ok(resp) => resp,
        Err(rejection) => rejection.into_response(),
    }
}

/// Corpo de `recovery-choice`: exatamente um entre `option_id` (opção da última análise) e
/// `guidance` (texto livre do humano).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChoiceInput {
    option_id: Option<String>,
    /// Número da análise que ofereceu a opção (obrigatório com `option_id`).
    analysis: Option<usize>,
    guidance: Option<String>,
}

const TASK_ID_MSG: &str = "task_id_invalid";
const TASK_NOT_FOUND: &str = "task_not_found";
const TASK_NOT_BLOCKED: &str = "task_not_blocked";
const OPTION_STALE: &str = "option_stale";
const CHOICE_INVALID: &str = "choice_invalid";
const CHOICE_FAILED: &str = "choice_failed";
const MAX_OPTION_ID_LEN: usize = 64;

fn validate_task_id(id: &str) -> Result<(), Rejection> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(rejection(StatusCode::UNPROCESSABLE_ENTITY, TASK_ID_MSG))
    }
}

fn parse_choice(body: &[u8]) -> Result<recovery::Choice, Rejection> {
    let invalid = || rejection(StatusCode::UNPROCESSABLE_ENTITY, CHOICE_INVALID);
    let input: ChoiceInput = serde_json::from_slice(body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            invalid()
        } else {
            rejection(StatusCode::BAD_REQUEST, "json_malformed")
        }
    })?;
    match (input.option_id, input.analysis, input.guidance) {
        (Some(id), Some(n), None) if !id.is_empty() && id.len() <= MAX_OPTION_ID_LEN => {
            Ok(recovery::Choice::Option(id, n))
        }
        (None, None, Some(text))
            if !text.trim().is_empty() && text.len() <= recovery::MAX_OPTION_TEXT_BYTES =>
        {
            Ok(recovery::Choice::Guidance(text.trim().to_string()))
        }
        _ => Err(invalid()),
    }
}

/// `POST /api/projects/:id/specs/:spec_id/tasks/:task_id/recovery-choice` (spec 073): o humano
/// escolhe uma opção da última análise (`{option_id}`) ou escreve uma orientação (`{guidance}`).
/// Aplica a escolha à tarefa BLOCKED e retoma a spec como o `resume` (que grava a marca de retomada
/// humana). A escolha referencia só o `id`: o conteúdo vem do que o servidor persistiu.
///
/// 404 projeto/spec/tarefa desconhecidos; 409 ocupado, tarefa não bloqueada ou opção vencida; 422
/// payload inválido. Se a retomada for recusada depois de aplicada a escolha (ex.: orçamento), o
/// efeito permanece e o humano pode usar Retomar.
pub async fn recovery_choice(
    State(state): State<AppState>,
    AxPath((id, spec_id, task_id)): AxPath<(String, String, String)>,
    body: Bytes,
) -> Response {
    match choose(&state, &id, &spec_id, &task_id, &body).await {
        Ok(resp) => resp,
        Err(rejection) => rejection.into_response(),
    }
}

async fn choose(
    state: &AppState,
    id: &str,
    spec_id: &str,
    task_id: &str,
    body: &[u8],
) -> Result<Response, Rejection> {
    validate_project_id(id)?;
    validate_spec_id(spec_id)?;
    validate_task_id(task_id)?;
    let choice = parse_choice(body)?;
    // A escolha retoma via `launch(Mode::Resume)`: mesma prontidão `execute` da retomada.
    if let Err(resp) = super::readiness::require(state, super::readiness::Gate::Execute).await {
        return Ok(resp);
    }
    let (project, guard) = state.reserve_run(id, spec_id).await.map_err(|e| match e {
        ReserveError::NotFound => rejection(StatusCode::NOT_FOUND, PROJECT_NOT_FOUND),
        ReserveError::Busy => conflict(BUSY),
        ReserveError::ReportInProgress => conflict_reason(REPORT_BUSY_CODE, REASON_REPORT_IN_PROGRESS),
    })?;
    if stopping(state) {
        return Err(shutting_down());
    }
    let secrets = state.secret_set().await;
    let (state_job, id_job, spec_job, task_job) =
        (state.clone(), id.to_string(), spec_id.to_string(), task_id.to_string());
    // Aplicar e retomar na mesma task: cancelar este future não deixa a escolha pela metade.
    let job = tokio::spawn(async move {
        let root = PathBuf::from(&project.path);
        // Todo projeto é Docker (spec 074): projeto sem ambiente docker é recusado antes de qualquer
        // efeito, com a mesma mensagem da retomada.
        let settings_root = root.clone();
        let is_docker = blocking(move || {
            Ok(read_settings_checked(&settings_root)
                .is_ok_and(|s| s.environment == crate::sdd::Environment::Docker))
        })
        .await?;
        if !is_docker {
            return Err(conflict_reason(PROJECT_NOT_DOCKER_CODE, REASON_ENV_NOT_DOCKER));
        }
        let (apply_root, apply_spec, apply_task) = (root.clone(), spec_job.clone(), task_job.clone());
        let snap_root = root.clone();
        let (snap_spec, snap_task) = (spec_job.clone(), task_job.clone());
        let snapshot = blocking(move || Ok(recovery::snapshot(&snap_root, &snap_spec, &snap_task))).await?;
        // Opção `environment` com `files` (spec 074): aprovação do ambiente + adoção na base ANTES de
        // registrar a escolha; negada ou falha, nada foi alterado e a opção continua utilizável.
        let probe = state_job.prereqs().clone();
        let (pre_root, pre_spec, pre_task, pre_choice, pre_secrets) =
            (root.clone(), spec_job.clone(), task_job.clone(), choice.clone(), Arc::clone(&secrets));
        let map_choice_error = |e: recovery::ChoiceError| match e {
            recovery::ChoiceError::NotFound => rejection(StatusCode::NOT_FOUND, TASK_NOT_FOUND),
            recovery::ChoiceError::NotBlocked => conflict(TASK_NOT_BLOCKED),
            recovery::ChoiceError::Stale => conflict(OPTION_STALE),
            recovery::ChoiceError::Io => internal(CHOICE_FAILED),
        };
        let env_option = blocking(move || {
            recovery::env_files_option(&pre_root, &pre_spec, &pre_task, &pre_choice, &pre_secrets, &|| {
                probe.docker_available()
            })
            .map_err(map_choice_error)
        })
        .await?;
        let mut adopted = false;
        if let Some(option) = env_option {
            let dialog = DialogTarget::new(root.clone(), spec_job.clone());
            let reason = env_choice_reason(state_job.project_lang(&id_job).get(), &option.label);
            recovery::adjust_environment(
                &state_job, &id_job, &dialog, &root, &secrets, &spec_job, &task_job, &option, &reason,
            )
            .await
            .map_err(|e| match e {
                recovery::MigrateError::Denied => conflict(ENV_DENIED_MSG),
                recovery::MigrateError::Shutdown => shutting_down(),
                recovery::MigrateError::Failed(m) => conflict_reason("environment_adjust_failed", REASON_ENV_INVALID).with_detail(m),
            })?;
            adopted = true;
        }
        let probe = state_job.prereqs().clone();
        let apply_secrets = Arc::clone(&secrets);
        let apply_lang = state_job.project_lang(&id_job).get();
        let applied = blocking(move || {
            recovery::apply_choice(
                &apply_root,
                &apply_spec,
                &apply_task,
                &choice,
                &apply_secrets,
                &|| probe.docker_available(),
                adopted,
                apply_lang,
            )
            .map_err(map_choice_error)
        })
        .await?;
        // Escolha atômica: se a retomada for recusada (orçamento, git sujo, spec parada...), o efeito
        // é desfeito e a opção continua utilizável.
        if let Err(rejection) = launch(state_job, id_job, spec_job, project, guard, Mode::Resume).await {
            if let Some(snap) = snapshot {
                let with_settings = applied == "config";
                let _ = blocking(move || {
                    recovery::restore(&root, &snap, with_settings);
                    Ok(())
                })
                .await;
            }
            return Err(rejection);
        }
        Ok::<&'static str, Rejection>(applied)
    });
    match job.await {
        Ok(Ok(kind)) => Ok((
            StatusCode::ACCEPTED,
            Json(json!({"ok": true, "spec_id": spec_id, "task_id": task_id, "applied": kind})),
        )
            .into_response()),
        Ok(Err(rejection)) => Err(rejection),
        Err(_) => Err(internal(INTERNAL)),
    }
}

/// Motivo do pedido de ajuste do ambiente quando a pessoa escolhe a opção (spec 115 T03), no
/// idioma do projeto: é o texto do pedido de aprovação.
fn env_choice_reason(lang: crate::i18n::Lang, label: &str) -> String {
    crate::i18n::tr(lang, "runs.env_choice_reason", &[("label", label)])
}

/// Validações rápidas e reserva da execução. Todo o restante roda em task própria ([`launch`]):
/// cancelar este future (desconexão do cliente) não interrompe gravação nem execução. Também chamada
/// por dentro, sem HTTP, pelo Nanty (spec 138 T03, C-03): 202 é sucesso; a recusa traz o `code`.
pub(crate) async fn start(
    state: &AppState,
    id: &str,
    spec_id: &str,
    body: &[u8],
    mode: Mode,
) -> Result<Response, Rejection> {
    validate_project_id(id)?;
    validate_spec_id(spec_id)?;
    parse_body(body)?;
    // Prontidão `execute` (spec 093 T02), com a máquina conferida na hora: 412 antes do 422 de
    // agente não configurado e antes de reservar.
    if let Err(resp) = super::readiness::require(state, super::readiness::Gate::Execute).await {
        return Ok(resp);
    }
    // Projeto e reserva da execução na mesma seção sob o read lock da configuração: a reserva
    // precede qualquer efeito, e a remoção não se intercala entre a busca e a reserva.
    // Sem await entre a reserva e o spawn: o guard sempre chega à task de execução.
    let (project, guard) = state
        .reserve_run(id, spec_id)
        .await
        .map_err(|e| match e {
            ReserveError::NotFound => rejection(StatusCode::NOT_FOUND, PROJECT_NOT_FOUND),
            ReserveError::Busy => conflict(BUSY),
            ReserveError::ReportInProgress => {
                conflict_reason(REPORT_BUSY_CODE, REASON_REPORT_IN_PROGRESS)
            }
        })?;
    let task = tokio::spawn(launch(
        state.clone(),
        id.to_string(),
        spec_id.to_string(),
        project,
        guard,
        mode,
    ));
    match task.await {
        Ok(Ok(())) => Ok((
            StatusCode::ACCEPTED,
            Json(json!({"ok": true, "spec_id": spec_id})),
        )
            .into_response()),
        Ok(Err(rejection)) => Err(rejection),
        Err(_) => Err(internal(INTERNAL)),
    }
}

/// Sequência completa da aprovação: inspeção, Git, agentes, gravação de `IN_PROGRESS`,
/// montagem do runner e disparo. Depois da gravação, [`Restore`] desfaz a aprovação se algo
/// falhar (erro ou panic) antes de o runner assumir a spec. A reserva (`guard`) só é solta
/// depois de o runner ser disparado, ou no erro.
async fn launch(
    state: AppState,
    project_id: String,
    spec_id: String,
    project: Project,
    guard: RunGuard,
    mode: Mode,
) -> Result<(), Rejection> {
    // ---- FASE A: validação pura. Nenhum arquivo, commit, evento ou worktree é tocado aqui, e toda
    // recusa previsível acontece nesta fase.
    if stopping(&state) {
        return Err(shutting_down());
    }
    // Plano ainda em elaboração (spec 080 T01): `spec.md` é gravado antes das tarefas, então
    // aprovar durante o turno do Arquiteto poderia iniciar um plano incompleto.
    if mode == Mode::Approve && state.architect_turn(&project_id).is_some() {
        return Err(conflict_reason(ARCHITECT_TURN_MSG, REASON_ARCHITECT_TURN));
    }
    let root = PathBuf::from(&project.path);
    // Spec 128 T04: nem a aprovação nem a renumeração correm junto com a revisão do plano.
    if mode == Mode::Approve && super::plan_review::review_running(&root, &spec_id) {
        return Err(conflict_reason(PLAN_REVIEW_RUNNING_MSG, REASON_PLAN_REVIEW_RUNNING));
    }
    let scan_root = root.clone();
    let sid = spec_id.clone();
    let inspected = blocking(move || inspect(&scan_root, &sid, mode)).await?;
    // Docker é o único modo (spec 074): plano sem `environment: docker` e projeto sem ambiente docker
    // são recusados; sem Docker no host, a recusa traz o diagnóstico (o Nants nunca instala Docker).
    match mode {
        Mode::Approve if inspected.spec.frontmatter.environment.as_deref() != Some("docker") => {
            return Err(conflict_reason(PLAN_NOT_DOCKER_MSG, REASON_ENV_NOT_DOCKER));
        }
        Mode::Resume if inspected.settings.environment != crate::sdd::Environment::Docker => {
            return Err(conflict_reason(PROJECT_NOT_DOCKER_CODE, REASON_ENV_NOT_DOCKER));
        }
        _ => {}
    }
    let prereqs = state.prereqs().clone();
    if let Some(item) = blocking(move || Ok(docker_prereq_missing(&prereqs))).await? {
        return Err(ApiError::coded(StatusCode::CONFLICT, "docker_unavailable", params([("item", json!(item))]))
            .with_reason(REASON_DOCKER_UNAVAILABLE));
    }
    let cfg = {
        let current = state.config().read().await;
        current.clone()
    };
    let mut developer = agent_setup(&cfg, &cfg.agents.developer, "developer", state.llm_backend(), Some(&state))?;
    let mut reviewer = agent_setup(&cfg, &cfg.agents.reviewer, "reviewer", state.llm_backend(), Some(&state))?;
    let architect = agent_setup(&cfg, &cfg.agents.architect, "architect", state.llm_backend(), Some(&state)).ok();
    // Spec 122 T04: a janela de cada modelo pelo catálogo já em cache (sem rede).
    let catalog_dir = state.config_path().parent().map(Path::to_path_buf).unwrap_or_default();
    let window_models = [developer.model.clone(), reviewer.model.clone()];
    let windows = blocking(move || {
        let names: Vec<&str> = window_models.iter().map(String::as_str).collect();
        Ok(super::price_catalog::cached_context_windows(&catalog_dir, &names))
    })
    .await
    .unwrap_or_default();
    let window = |i: usize| windows.get(i).copied().flatten().and_then(|w| usize::try_from(w).ok());
    // Spec 132 T08: a janela informada pela lista de modelos do provedor nativo vem antes.
    developer.context_window = developer.context_window.or(window(0));
    reviewer.context_window = reviewer.context_window.or(window(1));
    // Spec 133 T04 (C-01): os preços de cache pelo modelo e pelo tipo da conexão, do mesmo cache.
    let price_dir = state.config_path().parent().map(Path::to_path_buf).unwrap_or_default();
    let price_models = [(developer.model.clone(), developer.kind), (reviewer.model.clone(), reviewer.kind)];
    let prices = blocking(move || {
        let models: Vec<(&str, crate::config::ConnectionKind)> = price_models.iter().map(|(m, k)| (m.as_str(), *k)).collect();
        Ok(super::price_catalog::cached_cache_prices(&price_dir, &models))
    })
    .await
    .unwrap_or_default();
    // A mesma fonte do custo da tela: o `cfg.pricing` do modelo (estimativa ou edição manual)
    // vem antes; o catálogo em cache só sem ele.
    developer.cache_prices = resolve_cache_prices(cfg.pricing.get(&developer.model), developer.kind, prices.first().copied().flatten());
    reviewer.cache_prices = resolve_cache_prices(cfg.pricing.get(&reviewer.model), reviewer.kind, prices.get(1).copied().flatten());
    // Spec 090 T02: com backend substituto (testes), nenhum gerador real chega à rede.
    let images = match state.llm_backend() {
        Some(_) => None,
        None => super::image_gen::LlmImageGenerator::for_developer(&cfg, &state),
    };
    let secrets = state.secret_set().await;
    // Idioma dos commits do Nants na base (spec 115 T04, C-09): só o texto depois do prefixo.
    let commit_lang = state.project_lang(&project_id).get();
    // Spec 128 T04: renumeração na ordem de execução, só calculada aqui (nada gravado ainda).
    let renumbering = if mode == Mode::Approve {
        let (plan_root, sid, spec, files) = (root.clone(), spec_id.clone(), inspected.spec.clone(), inspected.files.clone());
        blocking(move || plan_renumbering(&plan_root, &sid, &spec, &files, commit_lang)).await?
    } else {
        None
    };
    if mode == Mode::Resume {
        // Retomar com o limite ainda atingido só pausaria de novo: exige aumentar ou remover.
        let (budget_root, budget_spec, pricing) = (root.clone(), spec_id.clone(), cfg.pricing.clone());
        let lang = state.project_lang(&project_id).get();
        let hit = blocking(move || Ok(super::usage::budget_status(&budget_root, &budget_spec, &pricing, lang))).await?;
        if let Some(reason) = hit {
            return Err(ApiError::coded(StatusCode::CONFLICT, "budget_exceeded", params([("reason", json!(reason))]))
                .with_reason(REASON_BUDGET));
        }
    }
    let check_root = root.clone();
    let env_spec_docker = mode == Mode::Approve
        && inspected.spec.frontmatter.environment.as_deref() == Some("docker");
    let approving = mode == Mode::Approve;
    let preflight =
        git_job(root.clone(), move || check_git_for(&check_root, env_spec_docker, approving)).await?;
    let excl_check = root.clone();
    blocking(move || check_local_excludes(&excl_check).map_err(Rejection::from)).await?;
    // Settings vazio: o comando da spec é adotado na fase B, salvo se o settings for versionado
    // (alterá-lo sujaria a base). Nesse caso o humano define e commita o comando.
    let adopt = if inspected.settings.test_command.trim().is_empty() {
        if preflight.settings_tracked {
            return Err(conflict_reason(TEST_COMMAND_MSG, REASON_TEST_COMMAND));
        }
        Some(
            inspected
                .spec
                .frontmatter
                .valid_test_command()
                .ok_or_else(|| conflict_reason(TEST_COMMAND_MSG, REASON_TEST_COMMAND))?,
        )
    } else {
        None
    };
    // Modo docker (spec 039): test_command/checks rodam DENTRO do workspace; sem docker/podman.
    let (adopt, spec_checks, spec_formatters) = {
        let docker_err = |_: &str| conflict_reason("test_command_docker", REASON_TEST_COMMAND);
        let mut current = inspected.settings.clone();
        current.environment = crate::sdd::Environment::Docker;
        let would_migrate = crate::sdd::docker_command::migrate_wrapped(&mut current);
        if would_migrate && preflight.settings_tracked {
            return Err(docker_err(crate::sdd::docker_command::DOCKER_COMMAND_MSG));
        }
        crate::sdd::docker_command::normalize_all(&current.test_command, &current.checks).map_err(docker_err)?;
        let adopt = match adopt {
            Some(c) => Some(crate::sdd::docker_command::normalize(&c).map_err(docker_err)?),
            None => None,
        };
        let spec_checks = match inspected.spec.frontmatter.checks.clone() {
            Some(c) => Some(crate::sdd::docker_command::normalize_all("", &c).map_err(docker_err)?.1),
            None => None,
        };
        let spec_formatters = match inspected.spec.frontmatter.formatters.clone() {
            Some(f) => Some(crate::sdd::docker_command::normalize_formatters(&f).map_err(docker_err)?),
            None => None,
        };
        (adopt, spec_checks, spec_formatters)
    };
    // Spec 124 T05: níveis de teste e instalação propostos pela spec, com a mesma regra do docker.
    let spec_levels = {
        let fm = &inspected.spec.frontmatter;
        let docker_err = |_: &str| conflict_reason("test_command_docker", REASON_TEST_COMMAND);
        let full = match fm.full_test_command.as_deref() {
            Some(c) => Some(crate::sdd::docker_command::normalize(c).map_err(docker_err)?),
            None => None,
        };
        let install = match fm.install_command.as_deref() {
            Some(c) => Some(crate::sdd::docker_command::normalize(c).map_err(docker_err)?),
            None => None,
        };
        SpecTestLevels::new(full, install, fm.lockfiles.clone())
    };
    let spec_failure_artifacts = inspected.spec.frontmatter.failure_artifacts.clone();
    let spec_screenshots = inspected.spec.frontmatter.screenshots.clone();
    let spec_references = inspected.spec.frontmatter.references.clone();
    // Ambiente docker: a spec que o declara aprova os arquivos (hash registrado na fase B); sem
    // declaração, o modo docker do projeto exige os mesmos arquivos aprovados antes.
    let env_root = root.clone();
    let env_settings = inspected.settings.clone();
    // Fecha a escrita de ambiente pelo Arquiteto até o fim da adoção.
    let _env_guard = (env_spec_docker || approving).then(|| crate::sdd::environment::begin_approval(&root));
    let env_adopt = blocking(move || {
        plan_environment(&env_root, &env_settings, env_spec_docker, preflight.settings_tracked)
    })
    .await?;
    if stopping(&state) {
        return Err(shutting_down());
    }

    // Iniciar um run encerra o app de teste do projeto (evento `app_exited`); a reserva do run
    // já impede nova partida. Só depois da validação: recusa previsível não derruba o app.
    state.apps().stop(&project_id, super::app::REASON_RUN_STARTED).await;

    // ---- FASE B: efeitos, nesta ordem. Só falhas inesperadas de I/O ou Git chegam aqui; elas
    // desfazem a aprovação pelo `Restore`. Exclude e commit inicial são idempotentes e inofensivos.
    // (1) Aprovação: spec vai a IN_PROGRESS. Retomada: tarefas BLOCKED voltam a TODO. A compensação
    // ([`Restore`]) é armada com o texto original e o gravado de cada arquivo.
    let mut restore = match mode {
        Mode::Approve => {
            // Spec 128 T04: com renumeração, a spec já vem com as referências trocadas e a seção.
            let mut marked = renumbering.as_ref().map_or_else(|| inspected.spec.clone(), |r| r.spec.clone());
            marked.frontmatter.status = SpecStatus::InProgress;
            let marked_text = marked.render().map_err(|_| internal(INTERNAL))?;
            let renumbered = renumbering.as_ref().map(|r| r.entries.clone()).unwrap_or_default();
            if !renumbered.is_empty() {
                // Conferência sem efeito: tarefa ou registro alterado desde a inspeção recusa antes.
                let (check_root, checked) = (root.clone(), renumbered.clone());
                blocking(move || check_unchanged_as(&check_root, &checked, SPEC_CHANGED)).await?;
            }
            let entry = Entry {
                rel: spec_rel(&spec_id),
                original: inspected.raw.clone(),
                marked: marked_text.clone(),
            };
            let mut entries = vec![entry];
            entries.extend(renumbered.iter().cloned());
            // Spec 128 T05 (critério 12): com renumeração, o diário com os originais vai para o disco
            // antes da primeira escrita; uma queda abrupta no meio é desfeita na subida do servidor.
            let journal = renumbering.is_some();
            if journal {
                let (journal_root, sid, journaled) = (root.clone(), spec_id.clone(), entries.clone());
                blocking(move || write_renumber_journal(&journal_root, &sid, &journaled)).await?;
            }
            let original = inspected.raw.clone();
            let write_root = root.clone();
            let sid = spec_id.clone();
            let written = marked_text;
            // SPEC_CHANGED aqui ainda não tem efeito a desfazer (só o diário, apagado aqui).
            let marked_result = blocking(move || mark_in_progress(&write_root, &sid, &original, &written)).await;
            if let Err(rejection) = marked_result {
                if journal {
                    let _ = safe_fs::remove_regular(&root, &renumber_journal_rel(&spec_id));
                }
                return Err(rejection);
            }
            // Armada antes das escritas da renumeração: a falha em qualquer uma desfaz todas, e a spec.
            let mut armed = Restore::new(&root, entries, state.events(), &project_id);
            if journal {
                armed.journal = Some(renumber_journal_rel(&spec_id));
                let write_root = root.clone();
                let sid = spec_id.clone();
                // O diário só sai depois da última escrita; sem conseguir apagá-lo, a aprovação é
                // desfeita (um diário esquecido desfaria na próxima subida uma spec em execução).
                blocking(move || {
                    write_entries(&write_root, &renumbered, RENUMBER_FAILED)?;
                    remove_renumber_journal(&write_root, &sid)
                })
                .await?;
            }
            armed
        }
        Mode::Resume => {
            let entries = inspected.resets.clone();
            let check_root = root.clone();
            let checked = entries.clone();
            // Conferência sem efeito: arquivo alterado desde a inspeção recusa antes de qualquer escrita.
            blocking(move || check_unchanged(&check_root, &checked)).await?;
            // Armada antes das escritas: se uma falhar, as já gravadas são desfeitas.
            let armed = Restore::new(&root, entries.clone(), state.events(), &project_id);
            let write_root = root.clone();
            blocking(move || write_resets(&write_root, &entries)).await?;
            armed
        }
    };
    // Shutdown logo após a gravação: a compensação (Restore) devolve a spec a AWAITING_APPROVAL.
    if stopping(&state) {
        restore.reason = SHUTDOWN_REASON;
        return Err(shutting_down());
    }
    // (2) exclusões locais do Nants em .git/info/exclude.
    let excl_root = root.clone();
    blocking(move || ensure_local_excludes(&excl_root).map_err(Rejection::from)).await?;
    // (3) commit inicial vazio, só para repositório sem commits.
    if preflight.needs_initial_commit {
        let commit_root = root.clone();
        git_job(root.clone(), move || initial_commit(&Git::new(commit_root), commit_lang)).await?;
    }
    // (3b) ARCHITECTURE.md inicial/atualizado no planejamento: commitado na base antes das worktrees.
    if approving {
        let (arch_root, sid, arch_secrets) = (root.clone(), spec_id.clone(), Arc::clone(&secrets));
        git_job(root.clone(), move || adopt_architecture(&arch_root, &sid, &arch_secrets, commit_lang)).await?;
        // Perfis de especialista (spec 063): `.sdd/skills/` commitado junto, antes das worktrees.
        let (skills_root, sid, skills_secrets) = (root.clone(), spec_id.clone(), Arc::clone(&secrets));
        git_job(root.clone(), move || adopt_skills(&skills_root, &sid, &skills_secrets, commit_lang)).await?;
    }
    // (4) comando da spec entra no settings do projeto quando o humano ainda não definiu um.
    let settings = match adopt {
        Some(command) => {
            let adopt_root = root.clone();
            blocking(move || adopt_spec_test_command(&adopt_root, &command)).await?
        }
        None => inspected.settings.clone(),
    };
    // (4b) checks da spec entram no settings só se o projeto ainda não tem checks.
    let settings = match spec_checks {
        Some(checks) if settings.checks.is_empty() && !preflight.settings_tracked => {
            let adopt_root = root.clone();
            blocking(move || adopt_spec_checks(&adopt_root, &checks)).await?
        }
        _ => settings,
    };
    // (4b2) formatadores da spec (spec 078) substituem os do settings; spec sem o campo não mexe.
    let settings = match spec_formatters {
        Some(formatters) if formatters != settings.formatters && !preflight.settings_tracked => {
            let adopt_root = root.clone();
            blocking(move || adopt_spec_formatters(&adopt_root, &formatters)).await?
        }
        _ => settings,
    };
    // (4b3) artefatos de falha da spec (spec 079): mesmo caminho dos formatadores.
    let settings = match spec_failure_artifacts {
        Some(globs) if globs != settings.failure_artifacts && !preflight.settings_tracked => {
            let adopt_root = root.clone();
            blocking(move || adopt_spec_failure_artifacts(&adopt_root, &globs)).await?
        }
        _ => settings,
    };
    // (4b4) capturas de tela da spec (spec 082 T03): mesmo caminho dos artefatos de falha.
    let settings = match spec_screenshots {
        Some(globs) if globs != settings.screenshots && !preflight.settings_tracked => {
            let adopt_root = root.clone();
            blocking(move || adopt_spec_screenshots(&adopt_root, &globs)).await?
        }
        _ => settings,
    };
    // (4b4b) níveis de teste e instalação da spec (spec 124 T05): o campo presente substitui o do
    // settings; spec sem os campos não mexe.
    let settings = match spec_levels {
        Some(levels) if levels.changes(&settings) && !preflight.settings_tracked => {
            let adopt_root = root.clone();
            blocking(move || adopt_spec_test_levels(&adopt_root, &levels)).await?
        }
        _ => settings,
    };
    // (4b5) referências visuais da spec (spec 084 T02): os anexos do chat listados em `references` são
    // copiados para `.sdd/specs/<spec>/references/`. Melhor esforço e idempotente: nunca falha a aprovação.
    if let Some(names) = spec_references {
        let (ref_root, sid) = (root.clone(), spec_id.clone());
        let _ = tokio::task::spawn_blocking(move || crate::sdd::references::copy_to_spec(&ref_root, &sid, &names)).await;
    }
    // (4b6) Spec 134 T05: o SHA-256 de cada fonte da base citada no plano e o catálogo
    // congelados em `.sdd/kb/frozen/<spec>.json` (fora do Git). Melhor esforço: a citação já foi
    // validada na gravação, e a falha aqui só tira o aviso de fonte alterada.
    if approving {
        let (kb_root, sid, kb_secrets) = (root.clone(), spec_id.clone(), Arc::clone(&secrets));
        let frozen = tokio::task::spawn_blocking(move || freeze_kb_sources(&kb_root, &sid, &kb_secrets)).await;
        if !matches!(frozen, Ok(Ok(_))) {
            eprintln!("{}", crate::i18n::tr(crate::i18n::Lang::from_env(), "cli.kb_freeze_failed", &[("spec", &spec_id)]));
        }
    }
    // (4c) ambiente aprovado junto com o plano: modo docker e hash dos arquivos no settings.
    let settings = match env_adopt {
        Some(hash) => {
            let adopt_root = root.clone();
            let sid = spec_id.clone();
            git_job(root.clone(), move || adopt_environment(&adopt_root, &hash, &sid, commit_lang)).await?
        }
        None => settings,
    };
    // (4d) migração (spec 039): projeto docker com comandos no formato embrulhado é desembrulhado.
    let mut notes = Vec::new();
    let settings = {
        let migrate_root = root.clone();
        let (migrated, changed) = blocking(move || migrate_docker_settings(&migrate_root)).await?;
        if changed {
            notes.push(CODE_DOCKER_MIGRATION.to_string());
        }
        migrated.unwrap_or(settings)
    };
    let policy = Policy::from_settings(&settings);
    // Abre o escopo do run com o test_command do settings (recarregado a cada início de run).
    let run = state
        .approvals()
        .begin_run(&project_id, Some(settings.test_command.as_str()));

    // Flag da vaga do projeto (spec 086 T04): a parada pela UI e o relançamento do mesmo job a enxergam.
    let stop = guard.stop_signal();
    let cancel = stop.cancel_flag();
    let job = RunJob {
        project_id: project_id.clone(),
        spec_id: spec_id.clone(),
        root: root.clone(),
        secrets: Arc::clone(&secrets),
        policy,
        developer,
        reviewer,
        architect,
        approvals: state.approvals().clone(),
        events: state.events().clone(),
        run,
        cancel: Arc::clone(&cancel),
        stop: stop.subscribe(),
        pricing: cfg.pricing.clone(),
        compose: state.compose().for_project(&root, &project_id, crate::harness::compose::ComposeKind::Test),
        images,
        lang: state.project_lang(&project_id),
    };
    let runner = state
        .runner_factory()
        .build(job)
        .map_err(|_| internal(INTERNAL))?;
    state.events().publish(status_event(
        &project_id,
        json!({"status": "executing", "spec_id": spec_id}),
    ));
    // (5) dispara o runner.
    let started = spawn_run(Background {
        guard,
        runner,
        spec_id: spec_id.clone(),
        project_id: project_id.clone(),
        root: root.clone(),
        events: state.events().clone(),
        secrets,
        shutdown: state.shutdown_signal(),
        cancel,
        stop,
        grace: state.shutdown_grace(),
        notes,
        state: state.clone(),
        roles: role_connections(&cfg),
    });
    if !started {
        // Shutdown acionado antes do disparo: o runner não começou; a compensação desfaz IN_PROGRESS.
        restore.reason = SHUTDOWN_REASON;
        return Err(shutting_down());
    }
    restore.disarm();
    // Spec 136 T05: a aprovação concluída fecha a sessão do plano das menções à base; o próximo
    // plano conta as menções a partir da mensagem seguinte do usuário. Melhor esforço.
    if approving {
        let (closed_root, now) = (root.clone(), SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
        let _closed = tokio::task::spawn_blocking(move || crate::kb::mention::record_plan_closed(&closed_root, now)).await;
    }
    // Spec 128 T04: a linha da renumeração na Conversa, só com a aprovação concluída.
    if let Some(r) = renumbering {
        let mut params = Map::new();
        params.insert("map".into(), json!(r.map_text));
        super::chat::system_notice(&state, &project_id, root.clone(), RENUMBERED_CODE, params).await;
    }
    Ok(())
}

/// Renumeração calculada na aprovação (spec 128 T04): a spec com as referências trocadas e a
/// seção "Renumeração", e cada arquivo a regravar (tarefas e registros locais do plano) com o texto
/// lido e o novo.
struct Renumbering {
    spec: Spec,
    entries: Vec<Entry>,
    map_text: String,
}

/// Registros locais do plano traduzidos junto: `issues`, `rounds` e `points` da revisão e as linhas
/// do resumo de mudanças.
const RENUMBER_RECORDS: [&str; 2] = [super::plan_review::PLAN_REVIEW_FILE, super::plan_changes::PLAN_CHANGES_FILE];

/// Calcula a renumeração sem gravar nada. `None` quando a ordem de execução já coincide com os ids
/// (ou os ids não são `T` + 2 dígitos). As tarefas trocam de conteúdo entre os mesmos arquivos (o
/// conjunto de ids não muda), então "renomear" é regravar cada arquivo com a tarefa que passa a ter
/// aquele id. Registro local ausente ou com JSON inválido fica como está; registro ilegível
/// (symlink, não regular) recusa a aprovação.
fn plan_renumbering(
    root: &Path,
    spec_id: &str,
    spec: &Spec,
    files: &[TaskFile],
    lang: crate::i18n::Lang,
) -> Result<Option<Renumbering>, Rejection> {
    use crate::sdd::execution_order as order;
    let plan: Vec<(String, Vec<String>)> =
        files.iter().map(|f| (f.task.frontmatter.id.clone(), f.task.frontmatter.depends_on.clone())).collect();
    let Ok(ids) = order::execution_order(&plan) else {
        return Ok(None);
    };
    let Some(map) = order::renumber_map(&ids) else {
        return Ok(None);
    };
    let failed = || internal(RENUMBER_FAILED);
    let tasks_rel = format!("{SPECS_DIR}/{spec_id}/{TASKS_DIR}");
    let mut entries = Vec::new();
    for file in files {
        let renumbered = order::renumber_task(&file.task, &map);
        let rel = format!("{tasks_rel}/{}.md", renumbered.frontmatter.id);
        let target = files.iter().find(|f| f.rel == rel).ok_or_else(failed)?;
        let marked = renumbered.render().map_err(|_| failed())?;
        if marked != target.raw {
            entries.push(Entry { rel, original: target.raw.clone(), marked });
        }
    }
    for name in RENUMBER_RECORDS {
        let rel = format!("{SPECS_DIR}/{spec_id}/{name}");
        let raw = match safe_fs::read_regular(root, &rel, MAX_READ_BYTES) {
            Ok(raw) => raw,
            Err(SafeFsError::Missing) => continue,
            Err(_) => return Err(failed()),
        };
        let Ok(mut value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        order::renumber_json(&mut value, &map);
        let marked = serde_json::to_string_pretty(&value).map_err(|_| failed())?;
        if marked != raw {
            entries.push(Entry { rel, original: raw, marked });
        }
    }
    let map_text = order::map_text(&map);
    let section = format!(
        "## {}\n\n{}",
        crate::i18n::tr(lang, "runs.renumber.heading", &[]),
        crate::i18n::tr(lang, "runs.renumber.body", &[("map", &map_text)])
    );
    Ok(Some(Renumbering { spec: order::renumber_spec(spec, &map, &section), entries, map_text }))
}

/// `true` quando o shutdown já foi acionado.
fn stopping(state: &AppState) -> bool {
    let rx = state.shutdown_signal();
    let value = *rx.borrow();
    value
}

fn shutting_down() -> Rejection {
    rejection(StatusCode::SERVICE_UNAVAILABLE, SHUTTING_DOWN_MSG)
}

/// `reason` padrão do `idle` de compensação (falha de montagem ou panic).
const RESTORE_ERROR_REASON: &str = "error";
/// `reason` do `idle` quando a compensação não conseguiu regravar o original.
const RESTORE_FAILED_REASON: &str = "restore_failed";

/// Arquivo gravado na fase B: texto lido na inspeção (`original`) e texto que a execução grava.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    rel: String,
    original: String,
    marked: String,
}

/// Diário da renumeração na aprovação (spec 128 T05, critério 12), na pasta da spec. Local (pelo
/// exclude do Git) e gravado de forma atômica antes da primeira escrita; apagado depois da última.
pub const RENUMBER_JOURNAL_FILE: &str = ".renumber-journal.json";
pub const RENUMBER_JOURNAL_EXCLUDE_LINE: &str = ".sdd/specs/*/.renumber-journal.json";
/// Temporário da escrita atômica (`.<nome>.<pid>.<seq>.tmp`).
pub const RENUMBER_JOURNAL_TMP_EXCLUDE_LINE: &str = ".sdd/specs/*/..renumber-journal.json.*.tmp";
/// Teto da leitura do diário: os originais e os textos novos da spec, das tarefas e dos registros.
const MAX_JOURNAL_BYTES: u64 = 64 * MAX_READ_BYTES;

/// Conteúdo do diário: cada arquivo da renumeração com o texto lido e o que a aprovação grava.
#[derive(Debug, Serialize, Deserialize)]
struct RenumberJournal {
    spec_id: String,
    entries: Vec<Entry>,
}

fn renumber_journal_rel(spec_id: &str) -> String {
    format!("{SPECS_DIR}/{spec_id}/{RENUMBER_JOURNAL_FILE}")
}

/// Grava o diário (escrita atômica, com o exclude local garantido). Falha recusa a aprovação
/// antes de qualquer escrita do plano.
fn write_renumber_journal(root: &Path, spec_id: &str, entries: &[Entry]) -> Result<(), Rejection> {
    let journal = RenumberJournal { spec_id: spec_id.to_string(), entries: entries.to_vec() };
    let body = serde_json::to_string(&journal).map_err(|_| internal(RENUMBER_FAILED))?;
    super::plan_review::write_local_file(root, &renumber_journal_rel(spec_id), RENUMBER_JOURNAL_FILE, &body)
        .ok_or_else(|| internal(RENUMBER_FAILED))
}

fn remove_renumber_journal(root: &Path, spec_id: &str) -> Result<(), Rejection> {
    if forced_journal_remove_failure(root) {
        return Err(internal(RENUMBER_FAILED));
    }
    safe_fs::remove_regular(root, &renumber_journal_rel(spec_id)).map_err(|_| internal(RENUMBER_FAILED))
}

/// Caminho do diário aceito: dentro da pasta da spec, sem `.`/`..` nem componente vazio.
fn journal_entry_ok(spec_id: &str, rel: &str) -> bool {
    rel.strip_prefix(&format!("{SPECS_DIR}/{spec_id}/"))
        .is_some_and(|rest| rest.split('/').all(|part| !part.is_empty() && part != "." && part != ".."))
}

/// Desfaz uma renumeração interrompida (spec 128 T05, critério 12): com o diário presente, cada
/// arquivo que ainda tem o texto novo volta ao original (a spec volta a `AWAITING_APPROVAL`, que é
/// o texto lido na aprovação) e o diário é apagado. Arquivo já original fica como está. Diário
/// ilegível ou de outra spec não é tocado. `true` quando um diário foi desfeito.
pub fn recover_renumber_journal(root: &Path, spec_id: &str) -> bool {
    let rel = renumber_journal_rel(spec_id);
    let Ok(raw) = safe_fs::read_regular(root, &rel, MAX_JOURNAL_BYTES) else {
        return false;
    };
    let Ok(journal) = serde_json::from_str::<RenumberJournal>(&raw) else {
        return false;
    };
    if journal.spec_id != spec_id || !journal.entries.iter().all(|e| journal_entry_ok(spec_id, &e.rel)) {
        return false;
    }
    let _lock = SPEC_WRITE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    // Só restaura quando TODA entrada tem hoje o texto original ou o novo. Um arquivo num terceiro
    // estado (alterado depois, ilegível ou ausente) indica que o plano seguiu adiante: nada é
    // restaurado, o diário fica e o terminal registra o motivo.
    if let Some(changed) = journal_unknown_entry(root, &journal.entries) {
        eprintln!(
            "{}",
            crate::i18n::tr(
                crate::i18n::Lang::from_env(),
                "cli.renumber_journal_kept",
                &[("spec", spec_id), ("root", &root.display().to_string()), ("file", changed), ("journal", &rel)],
            )
        );
        return false;
    }
    let mut restored = true;
    for entry in &journal.entries {
        restored &= restore_entry(root, entry);
    }
    restored && journal_unknown_entry(root, &journal.entries).is_none() && safe_fs::remove_regular(root, &rel).is_ok()
}

/// Primeira entrada do diário cujo arquivo não tem hoje nem o texto original nem o novo.
fn journal_unknown_entry<'a>(root: &Path, entries: &'a [Entry]) -> Option<&'a str> {
    entries
        .iter()
        .find(|entry| match safe_fs::read_regular(root, &entry.rel, MAX_READ_BYTES) {
            Ok(text) => text != entry.original && text != entry.marked,
            Err(_) => true,
        })
        .map(|entry| entry.rel.as_str())
}

/// `true` quando toda entrada tem hoje o texto original (compensação completa).
fn all_original(root: &Path, entries: &[Entry]) -> bool {
    entries
        .iter()
        .all(|entry| safe_fs::read_regular(root, &entry.rel, MAX_READ_BYTES).is_ok_and(|text| text == entry.original))
}

/// Na subida do servidor, antes de servir qualquer leitura do plano: desfaz os diários de
/// renumeração deixados por uma queda abrupta em todos os projetos registrados.
pub async fn recover_renumber_journals(state: &AppState) {
    let projects = super::envs::registered_projects(state).await;
    let job = tokio::task::spawn_blocking(move || {
        for (_, root) in projects {
            let Ok(dirs) = fs::read_dir(root.join(SPECS_DIR)) else {
                continue;
            };
            for dir in dirs.flatten() {
                let Some(spec_id) = dir.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if validate_spec_id(&spec_id).is_ok() && dir.path().join(RENUMBER_JOURNAL_FILE).exists() {
                    recover_renumber_journal(&root, &spec_id);
                }
            }
        }
    });
    let _ = job.await;
}

/// Desfaz a fase B quando a execução falha depois da primeira gravação. Para cada arquivo,
/// restaura o original somente se ele ainda contém exatamente o texto gravado, e publica `idle`
/// com motivo fixo. Vale para a aprovação (`IN_PROGRESS` na spec) e para a retomada (tarefas).
struct Restore {
    root: PathBuf,
    entries: Vec<Entry>,
    events: EventBus,
    project_id: String,
    /// `reason` do `idle` publicado ao compensar.
    reason: &'static str,
    armed: bool,
    /// Diário da renumeração (spec 128 T05): apagado depois de uma compensação completa.
    journal: Option<String>,
}

impl Restore {
    fn new(root: &Path, entries: Vec<Entry>, events: &EventBus, project_id: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            entries,
            events: events.clone(),
            project_id: project_id.to_string(),
            reason: RESTORE_ERROR_REASON,
            armed: true,
            journal: None,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

/// Desfaz uma entrada. Arquivo sem o texto gravado não tem o que restaurar; leitura falha não
/// permite confirmar, então conta como falha.
fn restore_entry(root: &Path, entry: &Entry) -> bool {
    match safe_fs::read_regular(root, &entry.rel, MAX_READ_BYTES) {
        Ok(text) if text == entry.marked => {
            !forced_restore_failure(root)
                && write_atomic(root, &entry.rel, &entry.original).is_ok()
        }
        Ok(_) => true,
        Err(_) => false,
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Todas as entradas são tentadas, mesmo depois de uma falha.
        let mut restored = true;
        for entry in &self.entries {
            restored &= restore_entry(&self.root, entry);
        }
        // Com tudo desfeito, o diário não tem mais o que restaurar; senão ele fica para a subida.
        // Só com cada arquivo de volta ao original: um texto diferente fica para a conferência da subida.
        if let (true, Some(journal)) = (restored, &self.journal) {
            if all_original(&self.root, &self.entries) {
                let _ = safe_fs::remove_regular(&self.root, journal);
            }
        }
        let payload = if restored {
            json!({"status": "idle", "reason": self.reason})
        } else {
            json!({
                "status": "idle",
                "reason": RESTORE_FAILED_REASON,
                "needs_manual_recovery": true,
            })
        };
        self.events
            .publish(status_event(&self.project_id, payload));
    }
}

/// Vaga `__report__` da finalização pós-integração. Ao sair por QUALQUER caminho (sucesso, falha,
/// timeout, shutdown, panic) libera a vaga e só então publica `finalization_done`, para que o
/// cliente, ao recarregar o estado, veja `report_pending: false`.
struct FinalizationGuard {
    slot: Option<RunGuard>,
    events: EventBus,
    project_id: String,
    spec_id: String,
}

impl Drop for FinalizationGuard {
    fn drop(&mut self) {
        drop(self.slot.take());
        self.events.publish(ApiEvent::new(
            EventType::FinalizationDone,
            Some(self.project_id.clone()),
            json!({"spec_id": self.spec_id}),
        ));
    }
}

/// Dados da execução em segundo plano.
struct Background {
    guard: RunGuard,
    runner: Box<dyn SpecRunner>,
    spec_id: String,
    project_id: String,
    root: PathBuf,
    events: EventBus,
    secrets: Arc<SecretSet>,
    shutdown: watch::Receiver<bool>,
    cancel: Arc<AtomicBool>,
    /// Pedido de parada pelo usuário (spec 086 T04); `cancel` é a flag dele.
    stop: StopSignal,
    grace: Duration,
    /// Linhas do sistema (migração de settings etc.) entregues ao diálogo antes do run.
    notes: Vec<String>,
    /// Estado do processo: o relatório final da spec (após `integrated`) usa reservas e configuração.
    state: AppState,
    /// Conexão de cada papel na configuração desta execução (spec 133 T01), para a evidência das
    /// falhas do provedor.
    roles: Vec<(&'static str, ConnectionRef)>,
}

/// Conexão de cada papel na configuração `cfg` (id, nome e tipo).
fn role_connections(cfg: &Config) -> Vec<(&'static str, ConnectionRef)> {
    [("architect", &cfg.agents.architect), ("developer", &cfg.agents.developer), ("reviewer", &cfg.agents.reviewer)]
        .into_iter()
        .filter_map(|(role, agent)| {
            let conn = cfg.connections.iter().find(|c| c.id == agent.connection_id)?;
            Some((role, ConnectionRef { id: conn.id.clone(), name: conn.name.clone(), kind: conn.kind.as_str().to_string() }))
        })
        .collect()
}

/// Falha do provedor (spec 133 T01): completa a evidência com a conexão do papel e grava a linha
/// no registro persistente da spec, fora do Git.
async fn record_provider_end(end: &mut RunEnd, roles: &[(&'static str, ConnectionRef)], root: &Path, project_id: &str, spec_id: &str, secrets: &Arc<SecretSet>) {
    let RunEnd::Provider(failure, Some(evidence)) = end else {
        return;
    };
    if evidence.connection.is_none() {
        evidence.connection = roles.iter().find(|(role, _)| *role == evidence.role).map(|(_, conn)| conn.clone());
    }
    let record = super::provider_errors::record_of(project_id, Some(spec_id), None, failure, evidence, secrets);
    let root = root.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || super::provider_errors::append(&root, &record)).await;
}

/// Desfecho da execução em segundo plano, antes de virar eventos.
enum RunEnd {
    Outcome(SpecOutcome),
    /// Erro do runner; carrega a causa (`Display` do `RunnerError`, ainda sem scrub) quando conhecida.
    Failed(Option<String>),
    /// Falha do provedor LLM de um papel: a causa fixa vai como mensagem do sistema, com a
    /// evidência (spec 133 T01) quando houver.
    Provider(ProviderFailure, Option<Box<FailureEvidence>>),
    Panicked,
    Cancelled,
    /// Cancelamento pedido pelo usuário (botão "Parar", spec 086 T04), sem shutdown.
    Stopped,
    /// Shutdown com runner que não respondeu no prazo: abandonado.
    Forced,
}

/// Dispara o runner. Com shutdown já acionado, o runner nem começa. Caso contrário, o worker roda
/// em task própria; o shutdown (com prioridade no `select!`) pede cancelamento cooperativo e
/// espera até `grace`. Só depois disso o worker é abandonado. A reserva é solta antes do status final.
fn spawn_run(bg: Background) -> bool {
    // Checagem síncrona: com shutdown já acionado nada é disparado e `bg` (inclusive a reserva)
    // é descartado aqui. O chamador compensa a aprovação.
    if *bg.shutdown.borrow() {
        return false;
    }
    // Marca de run ativo: só é solta depois das mensagens finais (ver `wait_active_runs`).
    let active = ActiveRun::enter();
    tokio::spawn(async move {
        let _active = active;
        let Background {
            guard,
            runner,
            spec_id,
            project_id,
            root,
            events,
            secrets,
            mut shutdown,
            cancel,
            stop,
            grace,
            notes,
            state,
            roles,
        } = bg;
        // Recuperação e diagnóstico do Arquiteto param no shutdown OU na parada pelo usuário.
        let mut halt = halt_signal(shutdown.clone(), stop.subscribe());
        // Semeia o seq com o que já está gravado (reinício), antes de qualquer mensagem do run.
        let seed_root = root.clone();
        let seed_spec = spec_id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            // Run de projeto existente: readmite o projeto no diálogo (limpa lápide, se houver).
            crate::server::dialog::revive_project(&seed_root);
            crate::server::dialog::seed_seq(&seed_root, &seed_spec)
        })
        .await;
        let mut guard = Some(guard);
        let runner: Arc<dyn SpecRunner> = Arc::from(runner);
        let dialog = DialogTarget::new(root.clone(), spec_id.clone());
        // Spec 137 T12: o texto de reserva gravado no diálogo sai no idioma do projeto.
        let lang = state.project_lang(&project_id).get();
        for note in &notes {
            let event = if note == CODE_DOCKER_MIGRATION {
                system_message_coded(&project_id, &coded_text(lang, CODE_DOCKER_MIGRATION, &[]), CODE_DOCKER_MIGRATION, Map::new())
            } else {
                system_message(&project_id, note)
            };
            deliver(&events, Some(&dialog), event);
        }
        let mut end = run_once(&state.project_lang(&project_id), &runner, &spec_id, &mut shutdown, &cancel, grace).await;
        // Bloqueio por qualidade: o Arquiteto analisa dentro do mesmo run (mesma reserva). RETRY
        // reabre a tarefa e o run continua; ESCALATE deixa BLOCKED. Ambiental nunca passa por aqui.
        let mut relaunch = false;
        loop {
            let RunEnd::Outcome(SpecOutcome::Blocked { task_id, kind, reason, .. }) = &end else {
                break;
            };
            // Ambiental só entra aqui quando a causa é a configuração de testes do plano (docker no
            // comando dentro do workspace): o Arquiteto pode corrigi-la via REVISE_CONFIG.
            let eligible = match kind {
                BlockKind::Quality => true,
                BlockKind::Environmental => {
                    // Config suspeita, ou (YOLO) falha de build/up do compose / `.nants/*` (1 rodada).
                    let probe = root.clone();
                    let reason_probe = reason.clone();
                    tokio::task::spawn_blocking(move || {
                        recovery::config_suspect(&probe)
                            || (recovery::is_env_fixable(&reason_probe) && recovery::yolo_enabled(&probe))
                    })
                    .await
                    .unwrap_or(false)
                }
                _ => false,
            };
            if !eligible {
                break;
            }
            let scope = recovery::Scope {
                state: &state,
                project_id: &project_id,
                spec_id: &spec_id,
                root: &root,
                secrets: &secrets,
                events: &events,
                dialog: &dialog,
            };
            let env_reason = (*kind == BlockKind::Environmental).then_some(reason.as_str());
            match recovery::recover(&scope, &runner, task_id, env_reason, &mut halt).await {
                recovery::Next::Retry => {
                    end = run_once(&state.project_lang(&project_id), &runner, &spec_id, &mut shutdown, &cancel, grace).await;
                }
                // Ambiente migrado para docker (spec 074): este run (host) termina limpo e o launch
                // relança a spec, detectando o modo docker.
                recovery::Next::Relaunch => {
                    relaunch = true;
                    break;
                }
                recovery::Next::Stay => break,
                recovery::Next::Shutdown => {
                    end = RunEnd::Cancelled;
                    break;
                }
            }
        }
        if relaunch {
            // Solta o runner host (e o escopo de aprovações dele) antes de montar o novo.
            drop(runner);
            let project = {
                let cfg = state.config().read().await;
                cfg.projects.iter().find(|p| p.id == project_id).cloned()
            };
            let relaunched = match (project, guard.take()) {
                (Some(project), Some(slot)) => {
                    launch(state.clone(), project_id.clone(), spec_id.clone(), project, slot, Mode::Resume).await.map_err(Some)
                }
                _ => Err(None),
            };
            match relaunched {
                // O novo run assumiu a spec e a reserva; este termina sem eventos finais.
                Ok(()) => return,
                Err(refusal) => {
                    let lang = state.project_lang(&project_id).get();
                    let (text, params) = relaunch_failure(lang, &secrets, refusal.as_ref());
                    deliver(&events, Some(&dialog), system_message_coded(&project_id, &text, CODE_RELAUNCH_FAILED, params));
                    for ev in terminal_events(lang, &project_id, &secrets, &spec_id, RunEnd::Failed(None)) {
                        deliver(&events, Some(&dialog), ev);
                    }
                    return;
                }
            }
        }
        // Bloqueio de ambiente: diagnóstico do Arquiteto (somente leitura; nunca reabre a tarefa).
        if let RunEnd::Outcome(SpecOutcome::Blocked { task_id, reason, kind: BlockKind::Environmental, .. }) = &end {
            let scope = recovery::Scope {
                state: &state,
                project_id: &project_id,
                spec_id: &spec_id,
                root: &root,
                secrets: &secrets,
                events: &events,
                dialog: &dialog,
            };
            if let recovery::Next::Shutdown = recovery::diagnose(&scope, &runner, task_id, reason, &mut halt).await {
                end = RunEnd::Cancelled;
            }
        }
        // Orçamento atingido: o Arquiteto avisa uma vez na conversa (sem recuperação nem diagnóstico).
        if let RunEnd::Outcome(SpecOutcome::Blocked { task_id, reason, kind: BlockKind::Budget, .. }) = &end {
            let text = crate::i18n::tr(state.project_lang(&project_id).get(), "runs.budget_paused", &[("reason", reason)]);
            deliver(
                &events,
                Some(&dialog),
                ApiEvent::new(
                    EventType::AgentMessage,
                    Some(project_id.clone()),
                    json!({
                        "agent": "architect",
                        "text": scrub_text(&secrets, &text),
                        "task_id": task_id,
                        "attempt": Value::Null,
                        "seq": Value::Null,
                    }),
                ),
            );
        }
        // Cancelamento sem shutdown, depois do pedido do usuário: desfecho `stopped` (o shutdown vence).
        if matches!(end, RunEnd::Cancelled) && stop.requested() && !*shutdown.borrow() {
            end = RunEnd::Stopped;
        }
        let integrated = matches!(end, RunEnd::Outcome(SpecOutcome::Completed));
        // Concluída: a vaga do projeto passa ao relatório final (`__report__`) antes do evento
        // `integrated`, sem janela para outra aprovação. Sem relatório, a vaga é solta já aqui.
        let hold = if integrated {
            state.runs().retag(&project_id, REPORT_RESERVATION);
            Some(FinalizationGuard {
                slot: guard.take(),
                events: events.clone(),
                project_id: project_id.clone(),
                spec_id: spec_id.clone(),
            })
        } else {
            drop(guard.take());
            None
        };
        record_provider_end(&mut end, &roles, &root, &project_id, &spec_id, &secrets).await;
        // Mensagens do sistema vão ao diálogo da spec; o status segue na mesma ordem.
        let lang = state.project_lang(&project_id).get();
        for ev in terminal_events(lang, &project_id, &secrets, &spec_id, end) {
            deliver(&events, Some(&dialog), ev);
        }
        // Relatório final da spec: só depois de `integrated`; falhas viram evento próprio.
        if integrated {
            report::after_integration(&state, &project_id, &spec_id, root, shutdown).await;
        }
        drop(hold);
    });
    true
}

/// Uma execução do runner em task própria, com shutdown cooperativo (prazo `grace`, depois abandono).
async fn run_once(
    lang: &crate::i18n::SharedLang,
    runner: &Arc<dyn SpecRunner>,
    spec_id: &str,
    shutdown: &mut watch::Receiver<bool>,
    cancel: &AtomicBool,
    grace: Duration,
) -> RunEnd {
    let worker_runner = Arc::clone(runner);
    let worker_spec = spec_id.to_string();
    // Spec 133 T01: o rastro das chamadas vale para a execução inteira (a evidência da falha final).
    let mut worker = tokio::spawn(crate::agents::llm_client::evidence::with_trace_scope(async move { worker_runner.run(&worker_spec).await }));
    tokio::select! {
        biased;
        _ = wait_shutdown(shutdown) => {
            cancel.store(true, Ordering::SeqCst);
            match tokio::time::timeout(grace, &mut worker).await {
                Ok(joined) => run_end(lang.get(), joined),
                Err(_) => {
                    worker.abort();
                    RunEnd::Forced
                }
            }
        }
        joined = &mut worker => run_end(lang.get(), joined),
    }
}

/// Converte o resultado do worker em desfecho. A causa da falha sai no idioma `lang` do projeto
/// (spec 137 T12): vai ao texto gravado e ao `params.cause`.
fn run_end(
    lang: Lang,
    joined: Result<Result<SpecOutcome, RunnerError>, tokio::task::JoinError>,
) -> RunEnd {
    match joined {
        Ok(Ok(outcome)) => RunEnd::Outcome(outcome),
        Ok(Err(RunnerError::Cancelled)) => RunEnd::Cancelled,
        Ok(Err(RunnerError::Provider { failure, evidence, .. })) => RunEnd::Provider(failure, evidence),
        Ok(Err(err)) => RunEnd::Failed(Some(err.describe_in(lang))),
        Err(_) => RunEnd::Panicked,
    }
}

/// Runs em andamento: contador global, incrementado ao disparar e decrementado quando a tarefa
/// do run termina (inclusive as mensagens finais). Usado pelo encerramento para não perdê-las.
static ACTIVE_RUNS: AtomicUsize = AtomicUsize::new(0);

/// Marca uma execução ativa até ser descartada.
#[derive(Debug)]
pub struct ActiveRun;

impl ActiveRun {
    pub fn enter() -> Self {
        ACTIVE_RUNS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        ACTIVE_RUNS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Espera, no máximo `timeout`, até não haver execução ativa. `true` se não há mais nenhuma.
pub async fn wait_active_runs(timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while ACTIVE_RUNS.load(Ordering::SeqCst) > 0 {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

/// Receptor que vira `true` no shutdown ou na parada pelo usuário (o que vier primeiro). O repasse
/// termina quando o receptor é descartado.
fn halt_signal(mut shutdown: watch::Receiver<bool>, mut user: watch::Receiver<bool>) -> watch::Receiver<bool> {
    let initial = *shutdown.borrow() || *user.borrow();
    let (tx, rx) = watch::channel(initial);
    if !initial {
        tokio::spawn(async move {
            tokio::select! {
                _ = wait_shutdown(&mut shutdown) => {}
                Ok(_) = user.wait_for(|stop| *stop) => {}
                _ = tx.closed() => return,
            }
            let _ = tx.send(true);
        });
    }
    rx
}

/// Espera o sinal de shutdown. Canal fechado também conta como shutdown.
async fn wait_shutdown(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

/// Mensagem do sistema de falha no idioma do projeto: com causa, `system.run.failed_cause` ("A
/// execução foi interrompida: <causa>; confira o estado do projeto.", causa com scrub, numa linha e
/// limitada a [`RUN_FAILED_CAUSE_MAX_BYTES`] sem quebrar caracteres); sem causa, `system.run.failed`.
#[cfg(test)]
fn run_failed_text(lang: Lang, secrets: &SecretSet, cause: Option<&str>) -> String {
    run_failed_parts(lang, secrets, cause).0
}

/// Texto de reserva no idioma do projeto (`system.<code>` do dicionário, o mesmo texto que a web
/// monta pelo `code`; spec 137 T12), com os parâmetros de texto.
fn coded_text(lang: Lang, code: &str, params: &[(&str, &str)]) -> String {
    crate::i18n::tr(lang, &format!("system.{code}"), params)
}

/// Texto de reserva, `code` e `params` (spec 102) da falha do run.
fn run_failed_parts(lang: Lang, secrets: &SecretSet, cause: Option<&str>) -> (String, &'static str, Map<String, Value>) {
    let cause = cause
        .map(|c| scrub_text(secrets, c).split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|c| !c.is_empty());
    let Some(mut cause) = cause else {
        return (coded_text(lang, CODE_RUN_FAILED, &[]), CODE_RUN_FAILED, Map::new());
    };
    if cause.len() > RUN_FAILED_CAUSE_MAX_BYTES {
        let mut end = RUN_FAILED_CAUSE_MAX_BYTES;
        while !cause.is_char_boundary(end) {
            end -= 1;
        }
        cause.truncate(end);
        cause.push('…');
    }
    let text = coded_text(lang, CODE_RUN_FAILED_CAUSE, &[("cause", &cause)]);
    (text, CODE_RUN_FAILED_CAUSE, Map::from_iter([("cause".to_string(), Value::from(cause))]))
}

/// Eventos finais de uma execução. Textos fixos no idioma `lang` do projeto (a falha com causa leva a
/// causa com scrub e a registra no log do servidor); o id da tarefa passa por redação.
fn terminal_events(
    lang: Lang,
    project_id: &str,
    secrets: &SecretSet,
    spec_id: &str,
    end: RunEnd,
) -> Vec<ApiEvent> {
    match end {
        RunEnd::Outcome(SpecOutcome::Completed) => vec![status_event(
            project_id,
            json!({"status": "integrated", "spec_id": spec_id}),
        )],
        RunEnd::Outcome(SpecOutcome::Blocked { task_id, kind, .. }) => vec![status_event(
            project_id,
            json!({
                "status": "blocked",
                "task_id": scrub_text(secrets, &task_id),
                "reason": BLOCKED_REASON,
                "kind": kind.as_str(),
            }),
        )],
        RunEnd::Outcome(SpecOutcome::Stalled { .. }) => vec![status_event(
            project_id,
            json!({"status": "idle", "reason": "stalled"}),
        )],
        RunEnd::Failed(cause) => {
            if let Some(cause) = &cause {
                eprintln!(
                    "execução interrompida (projeto {}, spec {}): {}",
                    scrub_text(secrets, project_id),
                    scrub_text(secrets, spec_id),
                    scrub_text(secrets, cause)
                );
            }
            vec![
                {
                    let (text, code, params) = run_failed_parts(lang, secrets, cause.as_deref());
                    system_message_coded(project_id, &text, code, params)
                },
                status_event(project_id, json!({"status": "idle", "reason": "error"})),
            ]
        }
        RunEnd::Panicked => vec![
            system_message_coded(project_id, &coded_text(lang, CODE_RUN_FAILED, &[]), CODE_RUN_FAILED, Map::new()),
            status_event(project_id, json!({"status": "idle", "reason": "error"})),
        ],
        // Código fixo do provedor (ex.: `provider_http_400`), com o motivo fechado da resposta
        // ruim quando houver (`provider_bad_response (incompleta: content_filter)`, spec 088);
        // nunca texto remoto.
        // Plano ChatGPT (spec 116 T02): limite de uso, conta não elegível ou precisa entrar de novo
        // pausam a execução com o código próprio; o "Continuar" retoma. Sem nova tentativa
        // automática e sem horário de liberação.
        RunEnd::Provider(ProviderFailure::Plan(plan), evidence) => vec![
            system_message_coded(
                project_id,
                &ProviderFailure::Plan(plan).describe(),
                plan_pause_code(plan),
                evidence_params(secrets, evidence.as_deref(), Map::new()),
            ),
            status_event(project_id, json!({"status": "idle", "reason": "error"})),
        ],
        // Spec 132 T09: o teto de gasto da Anthropic e a cota diária do Gemini pausam do mesmo
        // jeito, com o código próprio: esperar alguns segundos não resolve.
        RunEnd::Provider(ProviderFailure::Limit(limit), evidence) => vec![
            system_message_coded(
                project_id,
                &ProviderFailure::Limit(limit).describe(),
                limit_pause_code(limit),
                evidence_params(secrets, evidence.as_deref(), Map::new()),
            ),
            status_event(project_id, json!({"status": "idle", "reason": "error"})),
        ],
        RunEnd::Provider(failure, evidence) => vec![
            {
                // Params neutros de idioma (spec 102): a interface monta o motivo pelo dicionário.
                let params = super::error::provider_failure_params(&failure);
                let code = if params.contains_key("kind") { CODE_PROVIDER_FAILED_DETAIL } else { CODE_PROVIDER_FAILED };
                system_message_coded(project_id, &failure.describe_in(lang), code, evidence_params(secrets, evidence.as_deref(), params))
            },
            status_event(project_id, json!({"status": "idle", "reason": "error"})),
        ],
        RunEnd::Cancelled => vec![status_event(
            project_id,
            json!({"status": "idle", "reason": "shutdown"}),
        )],
        RunEnd::Stopped => vec![
            system_message_coded(project_id, &coded_text(lang, CODE_STOPPED, &[]), CODE_STOPPED, Map::new()),
            status_event(project_id, json!({"status": "idle", "reason": STOPPED_REASON})),
        ],
        RunEnd::Forced => vec![status_event(
            project_id,
            json!({"status": "idle", "reason": "shutdown_forced"}),
        )],
    }
}

/// `params` da falha do provedor com a evidência (spec 133 T01), quando houver: papel, conexão,
/// modelo, tarefa, tentativa, categoria e os códigos do provedor, somados aos `base`.
fn evidence_params(secrets: &SecretSet, evidence: Option<&FailureEvidence>, mut base: Map<String, Value>) -> Map<String, Value> {
    if let Some(evidence) = evidence {
        base.extend(super::provider_errors::evidence_params(secrets, evidence));
    }
    base
}

fn status_event(project_id: &str, payload: Value) -> ApiEvent {
    ApiEvent::new(
        EventType::ProjectStatus,
        Some(project_id.to_string()),
        payload,
    )
}

/// Códigos (spec 102) das mensagens do sistema do run; a interface as mostra pelo `code`.
pub const CODE_STOPPED: &str = "run.stopped";
pub const CODE_RUN_FAILED: &str = "run.failed";
/// Falha com causa (`params.cause`).
pub const CODE_RUN_FAILED_CAUSE: &str = "run.failed_cause";
/// Falha do provedor (`params.provider`: causa fixa, ex.: `provider_http_400`).
pub const CODE_PROVIDER_FAILED: &str = "run.provider_failed";
/// Falha do provedor com motivo (`params.provider`, `params.kind` e, quando houver, `params.provider_code`).
pub const CODE_PROVIDER_FAILED_DETAIL: &str = "run.provider_failed_detail";
/// Execução pausada pelo limite de uso do plano ChatGPT (spec 116 T02).
pub const CODE_PLAN_LIMIT: &str = "run.plan_limit";
/// Execução pausada: a conta não é elegível para usar o plano ChatGPT por aqui.
pub const CODE_PLAN_NOT_ELIGIBLE: &str = "run.plan_not_eligible";
/// Execução pausada: a conexão pelo plano ChatGPT precisa entrar de novo.
pub const CODE_PLAN_NEEDS_LOGIN: &str = "run.plan_needs_login";

/// Código da pausa por uma falha do plano ChatGPT.
fn plan_pause_code(plan: crate::agents::llm_client::PlanError) -> &'static str {
    use crate::agents::llm_client::PlanError;
    match plan {
        PlanError::UsageLimit => CODE_PLAN_LIMIT,
        PlanError::NotEligible => CODE_PLAN_NOT_ELIGIBLE,
        PlanError::NeedsLogin => CODE_PLAN_NEEDS_LOGIN,
    }
}
/// Execução pausada pelo teto de gasto da conta na Anthropic (spec 132 T09).
pub const CODE_SPEND_LIMIT: &str = "run.spend_limit";
/// Execução pausada pela cota diária do Gemini esgotada (spec 132 T09).
pub const CODE_DAILY_QUOTA: &str = "run.daily_quota";
/// Execução pausada pela cobrança do provedor: crédito esgotado ou pagamento pendente (spec 133 T02).
pub const CODE_BILLING: &str = "run.billing";

/// Código da pausa por um limite do provedor.
fn limit_pause_code(limit: crate::agents::llm_client::ProviderLimit) -> &'static str {
    use crate::agents::llm_client::ProviderLimit;
    match limit {
        ProviderLimit::SpendLimit => CODE_SPEND_LIMIT,
        ProviderLimit::DailyQuota => CODE_DAILY_QUOTA,
        ProviderLimit::Billing => CODE_BILLING,
    }
}
/// Relançamento em modo docker recusado (`params.message`).
pub const CODE_RELAUNCH_FAILED: &str = "run.relaunch_failed";
/// Ajuste do test_command/checks para rodar dentro do workspace.
pub const CODE_DOCKER_MIGRATION: &str = "run.docker_migration";

/// Mensagem do sistema com `code`/`params` (spec 102), além do texto de reserva em pt-BR.
/// Texto (reserva em pt-BR) e `params` da mensagem `run.relaunch_failed` (spec 115 T02). Com a
/// recusa do relançamento (um [`ApiError`]), os `params` levam o código dela (`error_code`), os
/// `params` dela e o `detail` técnico, para a interface traduzir pelo `serverError.<code>`;
/// `message` (o texto de reserva da recusa) fica só para quem não conhece o código. Tudo redigido
/// e cortado em [`super::events::MAX_PARAM_BYTES`].
fn relaunch_failure(lang: Lang, secrets: &SecretSet, refusal: Option<&ApiError>) -> (String, Map<String, Value>) {
    let cut = |s: &str| {
        let clean = scrub_text(secrets, s);
        let mut end = clean.len().min(super::events::MAX_PARAM_BYTES);
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean[..end].to_string()
    };
    // Spec 137 T12: o motivo (a recusa pelo código dela) e o texto saem no idioma do projeto.
    let Some(err) = refusal else {
        let message = crate::i18n::tr(lang, "runs.relaunch_unavailable", &[]);
        let text = coded_text(lang, CODE_RELAUNCH_FAILED, &[("message", &message)]);
        return (scrub_text(secrets, &text), Map::from_iter([("message".to_string(), Value::from(cut(&message)))]));
    };
    let message = match (err.code(), err.params()) {
        (Some(code), Some(params)) => super::error::fallback_text_in(lang, code, params),
        (Some(code), None) => super::error::fallback_text_in(lang, code, &Map::new()),
        (None, _) => err.message().to_string(),
    };
    let mut params = Map::new();
    params.insert("message".into(), Value::from(cut(&message)));
    if let Some(code) = err.code() {
        params.insert("error_code".into(), Value::from(code));
    }
    for (name, value) in err.params().into_iter().flatten() {
        if matches!(name.as_str(), "message" | "error_code" | "detail") {
            continue;
        }
        let value = match value {
            Value::String(s) => Value::from(cut(s)),
            Value::Number(_) => value.clone(),
            _ => continue,
        };
        params.insert(name.clone(), value);
    }
    let mut text = coded_text(lang, CODE_RELAUNCH_FAILED, &[("message", &message)]);
    if let Some(detail) = err.detail() {
        params.insert("detail".into(), Value::from(cut(detail)));
        text.push('\n');
        text.push_str(&coded_text(lang, "run.relaunch_failed_detail", &[("detail", detail)]));
    }
    (scrub_text(secrets, &text), params)
}

fn system_message_coded(project_id: &str, text: &str, code: &str, params: Map<String, Value>) -> ApiEvent {
    let mut event = system_message(project_id, text);
    if let Some(map) = event.payload.as_object_mut() {
        map.insert("code".into(), Value::from(code));
        map.insert("params".into(), Value::Object(params));
    }
    event
}

/// Mensagem do sistema do run. Sem tarefa; `seq` é preenchido ao gravar em `dialog.jsonl`.
fn system_message(project_id: &str, text: &str) -> ApiEvent {
    ApiEvent::new(
        EventType::AgentMessage,
        Some(project_id.to_string()),
        json!({
            "agent": SYSTEM_AGENT,
            "text": text,
            "task_id": Value::Null,
            "attempt": Value::Null,
            "seq": Value::Null,
        }),
    )
}

/// Valida `spec_id`: `[a-zA-Z0-9_-]{1,64}`.
fn validate_spec_id(id: &str) -> Result<(), Rejection> {
    let ok = !id.is_empty()
        && id.len() <= MAX_SPEC_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(rejection(StatusCode::UNPROCESSABLE_ENTITY, SPEC_ID_MSG))
    }
}

/// Corpo vazio ou `{}`. JSON malformado vira 400; campos desconhecidos, 422.
fn parse_body(body: &[u8]) -> Result<(), Rejection> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    serde_json::from_slice::<ApproveInput>(body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            rejection(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields")
        } else {
            rejection(StatusCode::BAD_REQUEST, "json_malformed")
        }
    })?;
    Ok(())
}


/// Executa `job` em thread bloqueante. Falha da thread vira 500 estático.
async fn blocking<T, F>(job: F) -> Result<T, Rejection>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Rejection> + Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| internal(INTERNAL))?
}

fn spec_rel(spec_id: &str) -> String {
    format!("{SPECS_DIR}/{spec_id}/{SPEC_FILE}")
}

/// Estado lido e validado da spec, usado na escrita de `IN_PROGRESS`.
struct Inspected {
    /// Tarefas lidas e validadas (texto exato e interpretado), em ordem de arquivo.
    files: Vec<TaskFile>,
    /// Texto exato lido; a escrita só ocorre se o arquivo não mudou desde então.
    raw: String,
    spec: Spec,
    settings: Settings,
    /// Retomada: tarefas BLOCKED a reabrir, com o texto lido e o reaberto. Vazio na aprovação.
    resets: Vec<Entry>,
}

/// Lê e valida spec, tarefas e settings. Tudo com `safe_fs`.
fn inspect(root: &Path, spec_id: &str, mode: Mode) -> Result<Inspected, Rejection> {
    let raw = match safe_fs::read_regular(root, &spec_rel(spec_id), MAX_READ_BYTES) {
        Ok(text) => text,
        Err(SafeFsError::Missing) => {
            return Err(rejection(StatusCode::NOT_FOUND, SPEC_NOT_FOUND))
        }
        Err(_) => return Err(invalid_spec()),
    };
    let spec = Spec::parse(&raw).map_err(|_| invalid_spec())?;
    if spec.frontmatter.id != spec_id {
        return Err(invalid_spec());
    }
    match (mode, spec.frontmatter.status) {
        (Mode::Approve, SpecStatus::AwaitingApproval) => {}
        (Mode::Approve, SpecStatus::Draft) => return Err(conflict(SPEC_DRAFT)),
        (Mode::Approve, _) => return Err(conflict(SPEC_NOT_AWAITING)),
        (Mode::Resume, SpecStatus::InProgress) => {}
        (Mode::Resume, _) => return Err(conflict(SPEC_NOT_IN_PROGRESS)),
    }
    let files = check_tasks(root, spec_id, mode)?;
    if mode == Mode::Resume && files.iter().all(|f| f.task.frontmatter.status == TaskStatus::Done)
    {
        return Err(conflict(NO_BLOCKED_TASK));
    }
    let settings = read_settings_checked(root)?;
    // Sem comando no settings nem na spec: recusa. Com comando só na spec, a fase de efeitos
    // grava (em `launch`); se o settings for versionado, `launch` recusa antes de qualquer efeito.
    if settings.test_command.trim().is_empty() && spec.frontmatter.valid_test_command().is_none() {
        return Err(conflict_reason(TEST_COMMAND_MSG, REASON_TEST_COMMAND));
    }
    if settings.base_branch.trim().is_empty() {
        return Err(conflict_reason(BASE_BRANCH_MSG, REASON_BASE_BRANCH));
    }
    let resets = match mode {
        Mode::Approve => Vec::new(),
        Mode::Resume => reset_plan(root, spec_id, &settings, &files)?,
    };
    Ok(Inspected {
        files,
        raw,
        spec,
        settings,
        resets,
    })
}

/// Plano da retomada: cada tarefa BLOCKED volta a TODO com `attempts` zerado e `next_action` limpo.
/// O corpo (findings incluídos) fica como contexto. Tarefa cuja integração já consta na base não é
/// reaberta: o runner a finaliza como antes.
fn reset_plan(
    root: &Path,
    spec_id: &str,
    settings: &Settings,
    files: &[TaskFile],
) -> Result<Vec<Entry>, Rejection> {
    let git = Git::new(root.to_path_buf());
    let mut entries = Vec::new();
    for file in files {
        let meta = &file.task.frontmatter;
        if meta.status != TaskStatus::Blocked {
            continue;
        }
        let integrated = is_integrated(&git, &settings.base_branch, spec_id, &meta.id)
            .map_err(|_| internal(INTERNAL))?;
        if integrated {
            continue;
        }
        let mut reopened = file.task.clone();
        if crate::agents::runner::is_revised_block(meta) {
            // Definição revisada = nova chance: os ciclos anteriores deixam de contar.
            reopened.body = crate::agents::runner::mark_revised(&reopened.body);
        }
        // Retomar é intervenção humana (spec 073): zera o orçamento de ciclos; o histórico fica.
        reopened.body = crate::agents::runner::mark_human_resumed(&reopened.body);
        reopened.frontmatter.status = TaskStatus::Todo;
        reopened.frontmatter.attempts = 0;
        reopened.frontmatter.rejected_sha = None;
        reopened.frontmatter.tested_sha = None;
        reopened.frontmatter.tested_signature = None;
        reopened.frontmatter.tested_at = None;
        reopened.frontmatter.next_action = None;
        reopened.frontmatter.next_action_code = None;
        let marked = reopened.render().map_err(|_| internal(INTERNAL))?;
        entries.push(Entry {
            rel: file.rel.clone(),
            original: file.raw.clone(),
            marked,
        });
    }
    Ok(entries)
}

/// Confere, antes de qualquer escrita da retomada, que cada tarefa a reabrir ainda tem o texto lido
/// na inspeção. Divergência recusa sem efeito.
fn check_unchanged(root: &Path, entries: &[Entry]) -> Result<(), Rejection> {
    check_unchanged_as(root, entries, TASKS_CHANGED).map_err(|_| conflict(TASKS_CHANGED))
}

/// Como [`check_unchanged`], com o código da recusa (`code`).
fn check_unchanged_as(root: &Path, entries: &[Entry], code: &'static str) -> Result<(), Rejection> {
    for entry in entries {
        let current = safe_fs::read_regular(root, &entry.rel, MAX_READ_BYTES)
            .map_err(|_| conflict(code))?;
        if current != entry.original {
            return Err(conflict(code));
        }
    }
    Ok(())
}

/// Grava as tarefas reabertas, em ordem. Falha no meio deixa as já gravadas para o [`Restore`].
fn write_resets(root: &Path, entries: &[Entry]) -> Result<(), Rejection> {
    write_entries(root, entries, WRITE_FAILED)
}

/// Grava cada entrada (escrita atômica, sem seguir symlink), em ordem. A falha devolve `code` e
/// deixa as já gravadas para o [`Restore`]. Usada na retomada e na renumeração da aprovação.
fn write_entries(root: &Path, entries: &[Entry], code: &'static str) -> Result<(), Rejection> {
    for (index, entry) in entries.iter().enumerate() {
        if forced_reset_failure(root, index) {
            return Err(internal(code));
        }
        write_atomic(root, &entry.rel, &entry.marked).map_err(|_| internal(code))?;
    }
    Ok(())
}

/// Índices de escrita da retomada que devem falhar (teste de estado coerente após falha).
#[cfg(test)]
static FORCED_RESET_WRITES: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

/// Agenda a falha da escrita de índice `index` na retomada da raiz `root` (uma vez).
#[cfg(test)]
fn fail_reset_write(root: &Path, index: usize) {
    FORCED_RESET_WRITES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((root.to_path_buf(), index));
}

/// `true` (uma vez) quando a escrita `index` da retomada desta raiz foi marcada para falhar.
#[cfg(test)]
fn forced_reset_failure(root: &Path, index: usize) -> bool {
    let mut list = FORCED_RESET_WRITES
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match list.iter().position(|(r, i)| r == root && *i == index) {
        Some(pos) => {
            list.remove(pos);
            true
        }
        None => false,
    }
}

/// Sem falha forçada fora de testes.
#[cfg(not(test))]
fn forced_reset_failure(_root: &Path, _index: usize) -> bool {
    false
}

/// Mensagem (500) quando o comando de testes da spec não pode ser gravado no projeto.
const SETTINGS_SAVE_MSG: &str = "falha ao salvar o comando de testes";
/// Código (spec 115 T02) do 500 ao gravar o `settings.yaml`.
const SETTINGS_SAVE_CODE: &str = "test_command_save_failed";

/// Grava `command` (da spec) em `settings.yaml` se o `test_command` do projeto estiver vazio.
/// Relê sob o `PERSIST_LOCK` (o mesmo de PUT e `always`) e preserva os demais campos: se o humano
/// já definiu um comando, nada é alterado. Devolve as configurações resultantes.
fn adopt_spec_test_command(root: &Path, command: &str) -> Result<Settings, Rejection> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut settings = read_settings_checked(root)?;
    if !settings.test_command.trim().is_empty() {
        return Ok(settings);
    }
    settings.test_command = command.to_string();
    let rendered = settings
        .render()
        .map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes())
        .map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    Ok(settings)
}

/// Grava `checks` (da spec) em `settings.yaml` se o projeto ainda não tiver checks, sob o mesmo
/// `PERSIST_LOCK`. Os do humano vencem. Devolve as configurações resultantes.
fn adopt_spec_checks(root: &Path, checks: &[crate::sdd::Check]) -> Result<Settings, Rejection> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut settings = read_settings_checked(root)?;
    let Some(valid) = crate::sdd::validate_checks(checks) else {
        return Ok(settings);
    };
    if !settings.checks.is_empty() || valid.is_empty() {
        return Ok(settings);
    }
    settings.checks = valid;
    let rendered = settings.render().map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    Ok(settings)
}

/// Grava `formatters` (da spec) em `settings.yaml`, substituindo a lista anterior (spec 078), sob o
/// mesmo `PERSIST_LOCK`. Lista vazia ou inválida não grava. Devolve as configurações resultantes.
fn adopt_spec_formatters(root: &Path, formatters: &[crate::sdd::Formatter]) -> Result<Settings, Rejection> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut settings = read_settings_checked(root)?;
    let valid = crate::sdd::validate_formatters(formatters);
    if valid.is_empty() || valid == settings.formatters {
        return Ok(settings);
    }
    settings.formatters = valid;
    let rendered = settings.render().map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    Ok(settings)
}

/// Níveis de teste e instalação propostos pela spec (spec 124 T05): cada campo presente substitui o
/// do settings; ausente não mexe.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecTestLevels {
    full_test_command: Option<String>,
    install_command: Option<String>,
    lockfiles: Option<Vec<String>>,
}

impl SpecTestLevels {
    /// `None` quando a spec não traz nenhum dos campos.
    fn new(full_test_command: Option<String>, install_command: Option<String>, lockfiles: Option<Vec<String>>) -> Option<Self> {
        let levels = Self { full_test_command, install_command, lockfiles };
        (levels.full_test_command.is_some() || levels.install_command.is_some() || levels.lockfiles.is_some()).then_some(levels)
    }

    fn apply(&self, settings: &mut Settings) {
        if let Some(full) = &self.full_test_command {
            settings.full_test_command = crate::sdd::valid_optional_command(Some(full));
        }
        if let Some(install) = &self.install_command {
            settings.install_command = crate::sdd::valid_optional_command(Some(install));
        }
        if let Some(locks) = &self.lockfiles {
            settings.lockfiles = crate::sdd::validate_lockfiles(locks);
        }
    }

    /// Aplicar muda o settings?
    fn changes(&self, settings: &Settings) -> bool {
        let mut next = settings.clone();
        self.apply(&mut next);
        next != *settings
    }
}

/// Grava os níveis de teste e a instalação da spec em `settings.yaml` (spec 124 T05), sob o
/// `PERSIST_LOCK`. Devolve as configurações resultantes.
fn adopt_spec_test_levels(root: &Path, levels: &SpecTestLevels) -> Result<Settings, Rejection> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut settings = read_settings_checked(root)?;
    if !levels.changes(&settings) {
        return Ok(settings);
    }
    levels.apply(&mut settings);
    let rendered = settings.render().map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    Ok(settings)
}

/// Grava `failure_artifacts` (da spec) em `settings.yaml`, substituindo a lista anterior (spec 079),
/// sob o mesmo `PERSIST_LOCK`. Lista vazia ou toda inválida não grava.
fn adopt_spec_failure_artifacts(root: &Path, globs: &[String]) -> Result<Settings, Rejection> {
    adopt_spec_glob_list(root, globs, |s| &mut s.failure_artifacts)
}

/// Grava `screenshots` (da spec) em `settings.yaml` (spec 082 T03), pelo mesmo caminho e com a
/// mesma validação de [`adopt_spec_failure_artifacts`].
fn adopt_spec_screenshots(root: &Path, globs: &[String]) -> Result<Settings, Rejection> {
    adopt_spec_glob_list(root, globs, |s| &mut s.screenshots)
}

/// Substitui a lista de globs `field` do settings pela da spec, validada por
/// [`crate::sdd::validate_failure_artifacts`], sob o `PERSIST_LOCK`. Vazia ou toda inválida não grava.
fn adopt_spec_glob_list(
    root: &Path,
    globs: &[String],
    field: fn(&mut Settings) -> &mut Vec<String>,
) -> Result<Settings, Rejection> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut settings = read_settings_checked(root)?;
    let valid = crate::sdd::validate_failure_artifacts(globs);
    if valid.is_empty() || valid == *field(&mut settings) {
        return Ok(settings);
    }
    *field(&mut settings) = valid;
    let rendered = settings.render().map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    Ok(settings)
}


/// Migração idempotente (spec 039): lê sob o `PERSIST_LOCK`, desembrulha `test_command`/`checks` no
/// formato embrulhado exato e grava de forma atômica. `(settings, alterou)`; `None` se o projeto
/// não é docker.
fn migrate_docker_settings(root: &Path) -> Result<(Option<Settings>, bool), Rejection> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut settings = read_settings_checked(root)?;
    if settings.environment != crate::sdd::Environment::Docker {
        return Ok((None, false));
    }
    if !crate::sdd::docker_command::migrate_wrapped(&mut settings) {
        return Ok((Some(settings), false));
    }
    let rendered = settings.render().map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    Ok((Some(settings), true))
}

/// Decide o tratamento do ambiente na fase A (sem efeitos). `Some(hash)`: a aprovação registra
/// o modo docker e o hash (arquivos validados). `None`: nada a gravar; em modo docker, os
/// arquivos atuais precisam bater com o hash aprovado.
/// Mensagem de pré-requisito quando o Docker não está pronto (CLI, daemon, permissão, Compose v2):
/// o primeiro item ausente, com o detalhe e a correção do diagnóstico. `None` com o Docker pronto.
/// Primeiro item do Docker que falta no host (`docker_cli`, `docker_daemon`, `docker_permission` ou
/// `docker_compose`), ou `None` com o Docker disponível. A interface mostra o item pelo código.
fn docker_prereq_missing(prereqs: &super::prereqs::PrereqChecker) -> Option<&'static str> {
    let items = prereqs.check();
    ["docker_cli", "docker_daemon", "docker_permission", "docker_compose"]
        .into_iter()
        .find(|id| !items.iter().any(|i| i.id == *id && i.ok))
}

fn plan_environment(
    root: &Path,
    settings: &Settings,
    spec_docker: bool,
    settings_tracked: bool,
) -> Result<Option<String>, Rejection> {
    use crate::sdd::environment as env;
    if spec_docker {
        let hash = env::validate_and_hash(root)
            .map_err(|m| conflict_reason("environment_invalid", REASON_ENV_INVALID).with_detail(m))?;
        let same = settings.environment == crate::sdd::Environment::Docker
            && settings.approved_hashes().contains(&hash);
        if same {
            return Ok(None);
        }
        if settings_tracked {
            return Err(conflict_reason(ENV_TRACKED_MSG, REASON_ENV_INVALID));
        }
        return Ok(Some(hash));
    }
    if settings.environment == crate::sdd::Environment::Docker {
        let current = env::env_hash(root);
        if current.as_ref().is_none_or(|c| !settings.approved_hashes().contains(c)) {
            return Err(conflict_reason(ENV_CHANGED_MSG, REASON_ENV_CHANGED));
        }
    }
    Ok(None)
}

/// Grava `environment: docker` e o hash aprovado no settings, sob o `PERSIST_LOCK`.
/// Antes, commita na base só os arquivos de ambiente (sem commit se nada mudou); falha no commit
/// recusa com 409 e nada é adotado.
fn adopt_environment(root: &Path, hash: &str, spec_id: &str, lang: crate::i18n::Lang) -> Result<Settings, Rejection> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut settings = read_settings_checked(root)?;
    // Revalida sob o lock: arquivos diferentes dos validados na fase A não são commitados.
    match crate::sdd::environment::validate_and_hash(root) {
        Ok(now) if now == hash => {}
        _ => return Err(conflict_reason(ENV_CHANGED_MSG, REASON_ENV_CHANGED)),
    }
    let paths: Vec<String> = crate::sdd::environment::read_env_files(root)
        .into_iter()
        .map(|(rel, _)| rel)
        .collect();
    commit_environment(root, &paths, spec_id, lang)?;
    mark_environment_docker(root, &mut settings, hash)?;
    Ok(settings)
}

/// Commita na base só os `paths` de ambiente (sem commit se nada mudou). Falha: 409 e nada fica
/// commitado nem preparado (o índice é desfeito por `commit_paths`).
fn commit_environment(root: &Path, paths: &[String], spec_id: &str, lang: crate::i18n::Lang) -> Result<(), Rejection> {
    let msg = crate::i18n::commit_subject(lang, &format!("chore({spec_id}): "), "commit.environment");
    Git::new(root.to_path_buf())
        .commit_paths(paths, &msg)
        .map(|_| ())
        .map_err(|_| conflict(ENV_COMMIT_MSG))
}

/// Marca `environment: docker` e o hash aprovado no settings (escrita atômica). Exige o `PERSIST_LOCK`.
fn mark_environment_docker(root: &Path, settings: &mut Settings, hash: &str) -> Result<(), Rejection> {
    settings.environment = crate::sdd::Environment::Docker;
    settings.approve_environment_hash(hash);
    let rendered = settings.render().map_err(|_| internal(SETTINGS_SAVE_CODE))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| internal(SETTINGS_SAVE_CODE))
}

/// Adoção de arquivos de ambiente APROVADOS propostos pela recuperação (spec 074): grava `files`
/// (caminho, conteúdo) em `.nants/`, marca `environment: docker` + hash aprovado e commita só esses
/// caminhos (`chore(<spec>): ambiente aprovado (Nants)`), tudo sob o `PERSIST_LOCK`. Qualquer falha
/// restaura os arquivos e o settings anteriores; o commit é o último passo, então falha dele não
/// adota nada. O chamador já obteve a aprovação (nunca chamar sem ela).
pub(super) fn adopt_environment_files(
    root: &Path,
    spec_id: &str,
    files: &[(String, String)],
    lang: crate::i18n::Lang,
) -> Result<Settings, String> {
    use crate::sdd::environment as env;
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let settings_before = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES)
        .map_err(|_| SETTINGS_SAVE_MSG.to_string())?;
    let mut settings = read_settings_checked(root).map_err(|r| r.message().to_string())?;
    // Estado anterior de cada arquivo (None = não existia); symlink ou especial recusa antes de gravar.
    let mut before: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    for (rel, _) in files {
        if !env::is_env_file(rel) {
            return Err(format!("{rel}: arquivo fora do ambiente permitido"));
        }
        let target = safe_fs::check_chain(root, rel).map_err(|_| format!("{rel}: caminho inseguro"))?;
        let old = match fs::symlink_metadata(&target) {
            Ok(m) if m.is_file() => Some(fs::read(&target).map_err(|_| format!("{rel}: ilegível"))?),
            Ok(_) => return Err(format!("{rel}: não é arquivo regular")),
            Err(_) => None,
        };
        before.push((rel.clone(), old));
    }
    let dir_existed = root.join(env::ENV_DIR).is_dir();
    let rollback = |settings_changed: bool| {
        for (rel, old) in &before {
            match old {
                Some(bytes) => {
                    let _ = fs::write(root.join(rel), bytes);
                }
                None => {
                    let _ = fs::remove_file(root.join(rel));
                }
            }
        }
        if !dir_existed {
            let _ = fs::remove_dir(root.join(env::ENV_DIR));
        }
        if settings_changed {
            let _ = replace_atomic(root, SETTINGS_REL, settings_before.as_bytes());
        }
    };
    let written = (|| -> Result<(), String> {
        safe_fs::ensure_dirs(root, env::ENV_DIR).map_err(|_| "não foi possível criar .nants/".to_string())?;
        for (rel, content) in files {
            write_atomic(root, rel, content).map_err(|_| format!("{rel}: falha ao gravar"))?;
        }
        Ok(())
    })();
    if let Err(e) = written {
        rollback(false);
        return Err(e);
    }
    let hash = match env::validate_and_hash(root) {
        Ok(hash) => hash,
        Err(m) => {
            rollback(false);
            return Err(format!("ambiente inválido após a gravação: {m}"));
        }
    };
    if let Err(r) = mark_environment_docker(root, &mut settings, &hash) {
        rollback(true);
        return Err(r.message().to_string());
    }
    let paths: Vec<String> = files.iter().map(|(rel, _)| rel.clone()).collect();
    if let Err(r) = commit_environment(root, &paths, spec_id, lang) {
        rollback(true);
        return Err(r.message().to_string());
    }
    Ok(settings)
}

/// Commita na base o `ARCHITECTURE.md` da raiz quando está novo/alterado (rascunho do plano),
/// sob o `PERSIST_LOCK` e revalidado. Inválido → 409. Sem mudança, nada a fazer.
fn adopt_architecture(root: &Path, spec_id: &str, secrets: &super::events::SecretSet, lang: crate::i18n::Lang) -> Result<(), Rejection> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let git = Git::new(root.to_path_buf());
    let dirty = git
        .porcelain_paths(&[WORKTREES_EXCLUDE])
        .map_err(|_| conflict_reason(BASE_DIRTY_MSG, REASON_BASE_DIRTY))?;
    if !dirty.iter().any(|p| p == ARCH_FILE) {
        return Ok(());
    }
    check_arch_valid(root)?;
    // Segredos nunca vão para o commit: regrava o arquivo redigido (atômico, sem symlink).
    if let Some(raw) = crate::agents::arch_memory::read_raw(root) {
        let clean = super::projects::scrub_text(secrets, &raw);
        if clean != raw {
            super::architecture::write_atomic(root, &clean)
                .ok_or_else(|| conflict(ARCH_COMMIT_MSG))?;
        }
    }
    let key = if git.is_tracked(ARCH_FILE) { "commit.architecture_update" } else { "commit.architecture_initial" };
    let msg = crate::i18n::commit_subject(lang, &format!("docs({spec_id}): "), key);
    git.commit_paths(&[ARCH_FILE.to_string()], &msg)
        .map_err(|_| conflict(ARCH_COMMIT_MSG))?;
    Ok(())
}

/// Perfis de especialista (`.sdd/skills/{developer,reviewer}.md`) novos/alterados no plano:
/// validados, redigidos e commitados na base (spec 063). Sem mudança, nada a fazer. Os perfis
/// ficam fora do squash das tarefas (os agentes nunca escrevem em `.sdd/`).
fn adopt_skills(root: &Path, spec_id: &str, secrets: &super::events::SecretSet, lang: crate::i18n::Lang) -> Result<(), Rejection> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let git = Git::new(root.to_path_buf());
    let dirty = git
        .porcelain_paths(&[WORKTREES_EXCLUDE])
        .map_err(|_| conflict_reason(BASE_DIRTY_MSG, REASON_BASE_DIRTY))?;
    let mut paths = Vec::new();
    for role in crate::sdd::skills::ROLES {
        let rel = crate::sdd::skills::rel_path(role);
        // Perfil apagado no disco (` D`) não é erro: nada a validar nem a commitar.
        if !dirty.iter().any(|p| *p == rel) || fs::symlink_metadata(root.join(&rel)).is_err() {
            continue;
        }
        let raw = crate::server::safe_fs::read_regular(root, &rel, ((crate::sdd::skills::MAX_PROFILE_BYTES + crate::sdd::skills::MAX_LESSONS_SECTION_BYTES) * 2) as u64)
            .map_err(|_| conflict_reason(SKILLS_INVALID_MSG, REASON_ENV_INVALID))?;
        // Segredos nunca vão para o commit: regrava o arquivo redigido antes de validar.
        let clean = super::projects::scrub_text(secrets, &raw);
        if clean != raw {
            replace_atomic(root, &rel, clean.as_bytes())
                .map_err(|_| conflict(SKILLS_COMMIT_MSG))?;
        }
        crate::sdd::skills::validate(role, &clean, None)
            .map_err(|_| conflict_reason(SKILLS_INVALID_MSG, REASON_ENV_INVALID))?;
        paths.push(rel);
    }
    if paths.is_empty() {
        return Ok(());
    }
    git.commit_paths(&paths, &crate::i18n::commit_subject(lang, &format!("docs({spec_id}): "), "commit.skills"))
        .map_err(|_| conflict(SKILLS_COMMIT_MSG))?;
    Ok(())
}

/// Perfis sujos do plano passam no formato (usado no preflight da aprovação).
fn check_skills_valid(root: &Path, dirty: &[String]) -> Result<(), Rejection> {
    for role in crate::sdd::skills::ROLES {
        let rel = crate::sdd::skills::rel_path(role);
        if !dirty.iter().any(|p| *p == rel) || fs::symlink_metadata(root.join(&rel)).is_err() {
            continue;
        }
        let ok = crate::server::safe_fs::read_regular(root, &rel, ((crate::sdd::skills::MAX_PROFILE_BYTES + crate::sdd::skills::MAX_LESSONS_SECTION_BYTES) * 2) as u64)
            .is_ok_and(|t| crate::sdd::skills::validate(role, &t, None).is_ok());
        if !ok {
            return Err(conflict_reason(SKILLS_INVALID_MSG, REASON_ENV_INVALID));
        }
    }
    Ok(())
}

/// O `ARCHITECTURE.md` da raiz é regular, seguro e passa no validador da spec 028.
fn check_arch_valid(root: &Path) -> Result<(), Rejection> {
    let ok = crate::agents::arch_memory::read_raw(root)
        .is_some_and(|text| super::architecture::validate_content(&text).is_ok());
    if ok {
        Ok(())
    } else {
        Err(conflict_reason(ARCH_INVALID_MSG, REASON_ENV_INVALID))
    }
}

fn read_settings_checked(root: &Path) -> Result<Settings, Rejection> {
    safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES)
        .ok()
        .and_then(|text| Settings::parse(&text).ok())
        .ok_or_else(|| conflict_reason(SETTINGS_MSG, REASON_SETTINGS))
}

/// Tarefa lida na inspeção: caminho relativo, texto exato e interpretação.
#[derive(Clone)]
struct TaskFile {
    rel: String,
    raw: String,
    task: Task,
}

/// Tarefas `*.md` de `tasks/`: todas parseáveis, `spec_id` e nome de arquivo coerentes com o
/// frontmatter, ids únicos, dependências existentes e sem ciclo. Aprovação exige status TODO; a
/// retomada aceita qualquer status válido (o runner retoma de onde a tarefa parou).
fn check_tasks(root: &Path, spec_id: &str, mode: Mode) -> Result<Vec<TaskFile>, Rejection> {
    let tasks_rel = format!("{SPECS_DIR}/{spec_id}/{TASKS_DIR}");
    let dir = safe_fs::check_chain(root, &tasks_rel).map_err(|_| invalid_task())?;
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(invalid_task()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(no_tasks(mode)),
        Err(_) => return Err(invalid_task()),
    }
    let entries = fs::read_dir(&dir).map_err(|_| invalid_task())?;
    let mut files: Vec<TaskFile> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for entry in entries {
        let entry = entry.map_err(|_| invalid_task())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid_task())?;
        let Some(stem) = name.strip_suffix(".md") else {
            continue;
        };
        let rel = format!("{tasks_rel}/{name}");
        let raw =
            safe_fs::read_regular(root, &rel, MAX_READ_BYTES).map_err(|_| invalid_task())?;
        let task = Task::parse(&raw).map_err(|_| invalid_task())?;
        let meta = &task.frontmatter;
        let status_ok = mode == Mode::Resume || meta.status == TaskStatus::Todo;
        let coherent = meta.spec_id == spec_id
            && meta.id == stem
            && status_ok
            && seen.insert(meta.id.clone());
        if !coherent {
            return Err(invalid_task());
        }
        files.push(TaskFile { rel, raw, task });
    }
    if files.is_empty() {
        return Err(no_tasks(mode));
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    let plain: Vec<Task> = files.iter().map(|f| f.task.clone()).collect();
    check_dependencies(&plain)?;
    Ok(files)
}

fn no_tasks(mode: Mode) -> Rejection {
    match mode {
        Mode::Approve => conflict_reason(NO_TASKS_MSG, REASON_PLAN_WITHOUT_TASKS),
        Mode::Resume => conflict_reason(NO_TASKS_MSG, REASON_NO_TASKS),
    }
}

fn invalid_task() -> Rejection {
    conflict_reason(INVALID_TASK_MSG, REASON_INVALID_TASK)
}

/// Dependências existentes e grafo acíclico (ordenação topológica de Kahn).
fn check_dependencies(tasks: &[Task]) -> Result<(), Rejection> {
    let ids: HashSet<&str> = tasks.iter().map(|t| t.frontmatter.id.as_str()).collect();
    let mut deps: HashMap<&str, HashSet<&str>> = HashMap::new();
    for task in tasks {
        let entry = deps.entry(task.frontmatter.id.as_str()).or_default();
        for dep in &task.frontmatter.depends_on {
            if !ids.contains(dep.as_str()) {
                return Err(conflict_reason(DEP_MISSING_MSG, REASON_DEP_MISSING));
            }
            entry.insert(dep.as_str());
        }
    }
    let mut remaining: HashMap<&str, usize> =
        deps.iter().map(|(id, d)| (*id, d.len())).collect();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for (id, ds) in &deps {
        for dep in ds {
            dependents.entry(*dep).or_default().push(*id);
        }
    }
    let mut ready: Vec<&str> = remaining
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut resolved = 0usize;
    while let Some(id) = ready.pop() {
        resolved += 1;
        for dependent in dependents.get(id).into_iter().flatten() {
            if let Some(n) = remaining.get_mut(dependent) {
                *n -= 1;
                if *n == 0 {
                    ready.push(*dependent);
                }
            }
        }
    }
    if resolved == deps.len() {
        Ok(())
    } else {
        Err(conflict_reason(DEP_CYCLE_MSG, REASON_DEP_CYCLE))
    }
}

/// Mensagem (500) quando o commit inicial da base não pode ser criado. Sem eco da saída do Git.
const INITIAL_COMMIT_ERROR: &str = "initial_commit_failed";
/// Prefixo do commit inicial (vazio, sem arquivos, criado só para existir HEAD); o texto depois
/// dele sai no idioma do projeto (`commit.initial`).
const INITIAL_COMMIT_PREFIX: &str = "chore: ";

/// Commit inicial vazio para repositório sem commits (fase B). Só age se HEAD continua sem commits.
/// Índice com entradas aqui é falha inesperada (a fase A já recusou staged), então vira 500.
fn initial_commit(git: &Git, lang: crate::i18n::Lang) -> Result<(), Rejection> {
    if !git.is_unborn() {
        return Ok(());
    }
    if git
        .index_has_entries()
        .map_err(|_| internal(INITIAL_COMMIT_ERROR))?
    {
        return Err(internal(INITIAL_COMMIT_ERROR));
    }
    // Identidade de reserva por chave e só nesta invocação (`commit_empty`): nunca altera a config.
    git.commit_empty(&crate::i18n::commit_subject(lang, INITIAL_COMMIT_PREFIX, "commit.initial"))
        .map_err(|_| internal(INITIAL_COMMIT_ERROR))
}

/// Resultado da checagem do Git feita na fase A.
struct Preflight {
    /// Repositório sem commits e índice vazio: a fase B cria o commit inicial vazio.
    needs_initial_commit: bool,
    /// `.sdd/settings.yaml` versionado: alterá-lo sujaria a base, então a aprovação não o adota.
    settings_tracked: bool,
}

/// Checagens do Git da base, sem efeitos: repositório, staged e sujeira. Funciona sem HEAD
/// (repositório recém-criado). Alteração não staged em `.sdd/specs/**`, `.sdd/worktrees/**` ou
/// `.sdd/settings.yaml` não bloqueia (o settings é configuração local, versionado ou não); qualquer
/// outro caminho bloqueia. Staged sempre bloqueia: o índice só pode conter o squash da integração.
#[cfg(test)]
fn check_git(root: &Path) -> Result<Preflight, Rejection> {
    check_git_for(root, false, false)
}

/// `allow_env`: na aprovação, arquivos de ambiente válidos (`.nants/compose.yaml`,
/// `.nants/Dockerfile*`) ainda não commitados são o rascunho do plano e não sujam a base; a
/// aprovação os commita. Fora disso continuam sendo base suja.
fn check_git_for(root: &Path, allow_env: bool, allow_arch: bool) -> Result<Preflight, Rejection> {
    #[cfg(test)]
    run_hook("git", root);
    if !is_git_repo(root) {
        return Err(conflict_reason(NOT_GIT_MSG, REASON_NOT_GIT));
    }
    let git = Git::new(root.to_path_buf());
    let settings_tracked = git.is_tracked(SETTINGS_REL);
    let unborn = git.is_unborn();
    if unborn {
        let index = git
            .index_has_entries()
            .map_err(|_| conflict_reason(BASE_DIRTY_MSG, REASON_BASE_DIRTY))?;
        if index {
            return Err(conflict_reason(BASE_DIRTY_MSG, REASON_BASE_DIRTY));
        }
    } else {
        git.rev_parse("HEAD")
            .map_err(|_| conflict_reason(NOT_GIT_MSG, REASON_NOT_GIT))?;
        let staged = git
            .has_staged_changes()
            .map_err(|_| conflict_reason(BASE_DIRTY_MSG, REASON_BASE_DIRTY))?;
        if staged {
            return Err(conflict_reason(BASE_DIRTY_MSG, REASON_BASE_DIRTY));
        }
    }
    let dirty = git
        .porcelain_paths(&[WORKTREES_EXCLUDE])
        .map_err(|_| conflict_reason(BASE_DIRTY_MSG, REASON_BASE_DIRTY))?;
    let blocked = dirty.iter().any(|p| {
        !p.starts_with(SPECS_PREFIX)
            && p.as_str() != SETTINGS_REL
            && !(allow_env && crate::sdd::environment::is_env_file(p))
            && !(allow_arch && p.as_str() == ARCH_FILE)
            && !p.starts_with(SKILLS_PREFIX)
    });
    if blocked {
        return Err(conflict_reason(BASE_DIRTY_MSG, REASON_BASE_DIRTY));
    }
    // Exceção de base suja só para um ARCHITECTURE.md válido.
    if allow_arch && dirty.iter().any(|p| p == ARCH_FILE) {
        check_arch_valid(root)?;
    }
    if allow_arch {
        check_skills_valid(root, &dirty)?;
    }
    Ok(Preflight {
        needs_initial_commit: unborn,
        settings_tracked,
    })
}

/// Executa uma operação Git (fase A ou B) em thread bloqueante, sob o mesmo tempo limite.
/// Estouro → 504 estático. Os efeitos da fase B que falharem por isso são desfeitos pelo `Restore`.
async fn git_job<T, F>(root: PathBuf, job: F) -> Result<T, Rejection>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Rejection> + Send + 'static,
{
    let timeout = git_timeout_for(&root);
    let handle = tokio::task::spawn_blocking(job);
    match tokio::time::timeout(timeout, handle).await {
        Err(_) => Err(rejection(StatusCode::GATEWAY_TIMEOUT, GIT_TIMEOUT_MSG)),
        Ok(Err(_)) => Err(internal(INTERNAL)),
        Ok(Ok(result)) => result,
    }
}

/// Tempo limite das operações Git de um projeto. Testes podem reduzi-lo para o projeto.
fn git_timeout_for(root: &Path) -> Duration {
    #[cfg(test)]
    if let Some(timeout) = test_git_timeout(root) {
        return timeout;
    }
    let _ = root;
    GIT_TIMEOUT
}

/// Limites de Git por raiz, usados só em testes (tempo limite simulado).
#[cfg(test)]
static GIT_TIMEOUT_OVERRIDES: Mutex<Vec<(PathBuf, Duration)>> = Mutex::new(Vec::new());

#[cfg(test)]
fn test_git_timeout(root: &Path) -> Option<Duration> {
    let overrides = GIT_TIMEOUT_OVERRIDES
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    overrides.iter().find(|(r, _)| r == root).map(|(_, d)| *d)
}

/// Monta Desenvolvedor ou Revisor. Qualquer falha de configuração vira 422 estático. Com o estado,
/// a conexão pelo plano ChatGPT usa o token do login (spec 116 T02).
fn agent_setup(
    cfg: &Config,
    agent: &AgentConfig,
    role: &str,
    injected: Option<Arc<dyn ChatBackend>>,
    state: Option<&AppState>,
) -> Result<AgentSetup, Rejection> {
    let not_configured = || match role {
        "developer" => rejection(StatusCode::UNPROCESSABLE_ENTITY, "developer_not_configured"),
        "reviewer" => rejection(StatusCode::UNPROCESSABLE_ENTITY, "reviewer_not_configured"),
        _ => rejection(StatusCode::UNPROCESSABLE_ENTITY, "architect_not_configured"),
    };
    let connection = cfg
        .connections
        .iter()
        .find(|c| c.id == agent.connection_id)
        .ok_or_else(not_configured)?;
    if agent.model.trim().is_empty() {
        return Err(not_configured());
    }
    let client = match state {
        Some(state) => super::chatgpt::plan_client::llm_client(state, connection),
        None => LlmClient::from_connection(connection),
    }
    .map_err(|_| not_configured())?;
    let backend: Arc<dyn ChatBackend> = injected.unwrap_or_else(|| Arc::new(client));
    Ok(AgentSetup {
        backend,
        model: agent.model.clone(),
        effort: agent.effort,
        max_steps: agent.max_steps_clamped(),
        // Spec 132 T08: a janela da última lista de modelos da conexão nativa, quando houver.
        context_window: connection
            .kind
            .is_native()
            .then(|| super::models_native::known_input_window(connection.endpoint(), &agent.model))
            .flatten()
            .and_then(|w| usize::try_from(w).ok()),
        kind: connection.kind,
        cache_prices: None,
    })
}

/// Preços de cache da regra de compactação (spec 133 T04, C-01): os de `pricing` (a tabela que
/// também dá o custo da tela, com as estimativas do catálogo e as edições do usuário), senão os do
/// catálogo em cache (`catalog`). O plano ChatGPT não cobra por token: `None`.
fn resolve_cache_prices(
    pricing: Option<&crate::config::Pricing>,
    kind: crate::config::ConnectionKind,
    catalog: Option<crate::agents::cache_cost::CachePrices>,
) -> Option<crate::agents::cache_cost::CachePrices> {
    if kind == crate::config::ConnectionKind::ChatgptPlan {
        return None;
    }
    pricing.and_then(crate::agents::cache_cost::CachePrices::from_pricing).or(catalog)
}

/// Grava `IN_PROGRESS` somente se `spec.md` ainda contém `original` (lido na inspeção).
fn mark_in_progress(
    root: &Path,
    spec_id: &str,
    original: &str,
    marked: &str,
) -> Result<(), Rejection> {
    {
        let _lock = SPEC_WRITE_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let rel = spec_rel(spec_id);
        let current = safe_fs::read_regular(root, &rel, MAX_READ_BYTES)
            .map_err(|_| conflict(SPEC_CHANGED))?;
        if current != original {
            return Err(conflict(SPEC_CHANGED));
        }
        write_atomic(root, &rel, marked).map_err(|_| internal(WRITE_FAILED))?;
    }
    #[cfg(test)]
    run_hook("mark", root);
    Ok(())
}

/// Raízes cuja próxima regravação do `Restore` deve falhar (teste de `restore_failed`).
#[cfg(test)]
static FORCED_RESTORE_FAILURES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// `true` (uma vez) quando a regravação do `Restore` deste projeto foi marcada para falhar.
#[cfg(test)]
fn forced_restore_failure(root: &Path) -> bool {
    let mut roots = FORCED_RESTORE_FAILURES
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match roots.iter().position(|r| r == root) {
        Some(i) => {
            roots.remove(i);
            true
        }
        None => false,
    }
}

/// Sem falha forçada fora de testes.
#[cfg(not(test))]
fn forced_restore_failure(_root: &Path) -> bool {
    false
}

/// Raízes cuja próxima remoção do diário da renumeração deve falhar (spec 128 T05).
#[cfg(test)]
static FORCED_JOURNAL_REMOVE_FAILURES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// `true` (uma vez) quando a remoção do diário deste projeto foi marcada para falhar.
#[cfg(test)]
fn forced_journal_remove_failure(root: &Path) -> bool {
    let mut roots = FORCED_JOURNAL_REMOVE_FAILURES.lock().unwrap_or_else(PoisonError::into_inner);
    match roots.iter().position(|r| r == root) {
        Some(i) => {
            roots.remove(i);
            true
        }
        None => false,
    }
}

/// Sem falha forçada fora de testes.
#[cfg(not(test))]
fn forced_journal_remove_failure(_root: &Path) -> bool {
    false
}

/// Gancho de teste: executa uma vez, na etapa indicada, quando a raiz do projeto coincide.
#[cfg(test)]
type Hook = Box<dyn FnOnce() + Send>;

#[cfg(test)]
static HOOKS: Mutex<Vec<(PathBuf, &'static str, Hook)>> = Mutex::new(Vec::new());

#[cfg(test)]
fn run_hook(stage: &'static str, root: &Path) {
    let hook = {
        let mut hooks = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
        hooks
            .iter()
            .position(|(r, s, _)| r == root && *s == stage)
            .map(|i| hooks.remove(i).2)
    };
    if let Some(hook) = hook {
        hook();
    }
}

/// Escrita atômica: arquivo temporário exclusivo no mesmo diretório (sem seguir symlink),
/// `fsync`, revalidação do destino e `rename`. Falhas removem o temporário.
fn write_atomic(root: &Path, rel: &str, contents: &str) -> Result<(), SafeFsError> {
    let target = safe_fs::check_chain(root, rel)?;
    let parent = target.parent().ok_or(SafeFsError::Unsafe)?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(SafeFsError::Unsafe)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}.{}.{seq}.tmp", process::id()));
    let perms = fs::metadata(&target).ok().map(|m| m.permissions());
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(SafeFsError::Io);
    }
    if let Some(perms) = perms {
        let _ = fs::set_permissions(&tmp, perms);
    }
    // Revalida o destino logo antes do rename (rename não segue symlink no último componente).
    if let Err(e) = safe_fs::check_chain(root, rel) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, &target).map_err(|_| {
        let _ = fs::remove_file(&tmp);
        SafeFsError::Io
    })
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::panic::{self, AssertUnwindSafe};
    use std::pin::Pin;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use axum::body::Body;
    use axum::http::{header, Method, Request, StatusCode};
    use serde_json::{json, Value};
    use tokio::sync::broadcast;
    use tower::ServiceExt;

    use super::*;
    use crate::agents::runner::{EventSink, RunnerEvent};

    /// Textos de reserva em pt-BR das mensagens fixas do run (`system.run.*`, spec 137 T12).
    const STOPPED_TEXT: &str = "Execução parada pelo usuário. Use Continuar para retomar.";
    const RUN_FAILED_TEXT: &str = "A execução foi interrompida por erro interno; confira o estado do projeto.";
    use crate::i18n::Lang;

    /// Spec 115 T03: o motivo do ajuste escolhido pela pessoa sai no idioma do projeto.
    #[test]
    fn env_choice_reason_follows_the_project_language() {
        assert_eq!(env_choice_reason(Lang::PtBr, "curl"), "Ajustar o ambiente docker (opção «curl» escolhida por você)");
        assert_eq!(env_choice_reason(Lang::En, "curl"), "Adjust the docker environment (option \"curl\" chosen by you)");
    }
    use crate::server::approvals::persist_always;
    use crate::config::Connection;
    use crate::server::{router, AppState};
    use crate::harness::tools::{ApprovalDecision, ApprovalRequest, Approver, Role, ToolHost, ToolName};

    const PORT: u16 = 3000;
    const HOST: &str = "127.0.0.1:3000";
    const SECRET: &str = "sk-SEGREDO-RUNS-9876";
    const PROJECT: &str = "proj";
    const SPEC: &str = "s1";
    const WAIT: Duration = Duration::from_secs(5);
    /// Ambiente docker aprovado: hash do `.nants/compose.yaml` que `write_sdd` grava.
    const SETTINGS_PLAIN: &str = "base_branch: main\nmerge_strategy: squash\ntest_command: \"true\"\n";
    const DOCKER_COMPOSE: &str = "services:\n  workspace:\n    image: node:20\n";
    const SETTINGS: &str = concat!("base_branch: main\nmerge_strategy: squash\ntest_command: \"true\"\n", "environment: docker\nenvironment_hash: 956ec7f74f0b60d1d28f0f623ad234c11881f2438c985c66dc61aec409368168\n");

    /// Diretório temporário único por processo e teste.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("nants-runs-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn spec_md(id: &str, status: &str) -> String {
        format!(
            "---\nid: \"{id}\"\ntitle: \"Spec de teste\"\nstatus: \"{status}\"\nbase_branch: \"main\"\nenvironment: \"docker\"\ncreated_at: \"2026-10-13\"\n---\n\n## Contexto\nTexto da spec.\n"
        )
    }

    /// Tarefa TODO; `deps` separadas por vírgula.
    fn task_md(id: &str, deps: &str, status: &str) -> String {
        let list: Vec<String> = deps
            .split(',')
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(|d| format!("\"{d}\""))
            .collect();
        format!(
            "---\nid: \"{id}\"\nspec_id: \"{SPEC}\"\ntitle: \"Tarefa {id}\"\nstatus: \"{status}\"\ndepends_on: [{}]\nbranch: \"sdd/{SPEC}/{id}\"\nattempts: 0\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope: [\"src/lib.rs\"]\n---\n\n## Objetivo\nFazer {id}.\n\n## Critérios de Aceitação\n- compila\n",
            list.join(", ")
        )
    }

    /// Reescreve `.sdd` com settings, a spec `SPEC` e as tarefas (`(id, deps)`, todas TODO).
    fn write_sdd(root: &Path, settings: &str, spec_status: &str, tasks: &[(&str, &str)]) {
        // Todo projeto é docker (spec 074): ambiente aprovado e registrado no settings.
        fs::create_dir_all(root.join(".nants")).expect("nants");
        fs::write(root.join(".nants/compose.yaml"), DOCKER_COMPOSE).expect("compose");
        write_sdd_files(root, settings, spec_status, tasks);
    }

    /// Como [`write_sdd`], sem criar `.nants/` (testes de baixo nível da checagem de Git).
    fn write_sdd_files(root: &Path, settings: &str, spec_status: &str, tasks: &[(&str, &str)]) {
        let sdd = root.join(".sdd");
        let _ = fs::remove_dir_all(&sdd);
        let spec_dir = sdd.join("specs").join(SPEC);
        fs::create_dir_all(spec_dir.join("tasks")).expect("tasks");
        fs::write(sdd.join("settings.yaml"), settings).expect("settings");
        fs::write(spec_dir.join("spec.md"), spec_md(SPEC, spec_status)).expect("spec");
        for (id, deps) in tasks {
            fs::write(
                spec_dir.join("tasks").join(format!("{id}.md")),
                task_md(id, deps, "TODO"),
            )
            .expect("task");
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Repositório com a configuração commitada (o `.sdd/specs` fica não rastreado, como no uso real).
    fn git_repo(root: &Path) {
        git(root, &["init", "-q"]);
        git(root, &["add", ".sdd/settings.yaml"]);
        if root.join(".nants").exists() {
            git(root, &["add", ".nants"]);
        }
        git(
            root,
            &[
                "-c",
                "user.name=Teste",
                "-c",
                "user.email=teste@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
    }

    /// Raiz de projeto de teste com spec `AWAITING_APPROVAL` e uma tarefa `T01`.
    fn setup(tag: &str, with_git: bool) -> PathBuf {
        let root = temp_dir(tag).join("repo");
        fs::create_dir_all(&root).expect("repo");
        write_sdd(&root, SETTINGS, "AWAITING_APPROVAL", &[("T01", "")]);
        if with_git {
            git_repo(&root);
        }
        root
    }

    /// Como [`setup`], mas sem ambiente docker no projeto (testes de baixo nível da adoção do ambiente).
    fn setup_plain(tag: &str, with_git: bool) -> PathBuf {
        let root = temp_dir(tag).join("repo");
        fs::create_dir_all(&root).expect("repo");
        write_sdd_files(&root, SETTINGS_PLAIN, "AWAITING_APPROVAL", &[("T01", "")]);
        if with_git {
            git_repo(&root);
        }
        root
    }

    fn connection(id: &str, key: &str) -> Connection {
        Connection {
            id: id.into(),
            name: format!("Conexão {id}"),
            base_url: "https://api.example.com/v1".into(),
            api_key: key.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        }
    }

    fn config_for(root: &Path) -> Config {
        let mut cfg = Config::default();
        cfg.connections.push(connection("literal", SECRET));
        cfg.agents.developer = AgentConfig {
            connection_id: "literal".into(),
            model: "modelo-dev".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        cfg.agents.reviewer = AgentConfig {
            connection_id: "literal".into(),
            model: "modelo-rev".into(),
            effort: Effort::Medium,
            max_steps: None,
        };
        cfg.projects.push(Project {
            id: PROJECT.into(),
            name: "Projeto".into(),
            path: root.display().to_string(),
            created_at: "2026-10-13T00:00:00Z".into(),
        });
        cfg
    }

    /// Spec 132 T08: na conexão nativa, a janela vem da última lista de modelos do provedor (por
    /// endereço e modelo); na conexão por chave, fica para o catálogo.
    /// Spec 133 T04 (C-01): a regra de compactação usa a mesma tabela do custo da tela; o preço
    /// manual vem antes do catálogo e muda a decisão. O plano ChatGPT fica sem preço.
    #[test]
    fn manual_price_drives_the_compaction_decision() {
        use crate::agents::cache_cost::CachePrices;
        use crate::config::{ConnectionKind, PriceSource, Pricing};
        let catalog = Some(CachePrices { read: 0.1, rewrite: 1.25 });
        // Tirar 500 tokens invalidando 25 mil, num horizonte de 10 chamadas.
        let decide = |p: Option<CachePrices>| p.map(|p| p.worth(500, 25_000, 10, true));
        let by_catalog = resolve_cache_prices(None, ConnectionKind::Anthropic, catalog);
        assert_eq!(by_catalog, catalog, "sem preço na tabela: o catálogo");
        assert_eq!(decide(by_catalog), Some(false));
        // O usuário editou o preço do modelo: sem desconto de cache, compactar sempre compensa.
        let manual = Pricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cached_input_per_mtok: None,
            cache_write_per_mtok: None,
            web_search_per_call: None,
            image_per_unit: None,
            source: PriceSource::Manual,
            estimated_at: None,
        };
        let by_manual = resolve_cache_prices(Some(&manual), ConnectionKind::Anthropic, catalog);
        assert_eq!(by_manual, Some(CachePrices { read: 1.0, rewrite: 1.0 }));
        assert_eq!(decide(by_manual), Some(true), "o preço manual muda a decisão");
        // Com os campos de cache na tabela, eles valem.
        let cached = Pricing { cached_input_per_mtok: Some(0.2), cache_write_per_mtok: Some(2.0), ..manual.clone() };
        assert_eq!(resolve_cache_prices(Some(&cached), ConnectionKind::Anthropic, None), Some(CachePrices { read: 0.2, rewrite: 2.0 }));
        assert_eq!(resolve_cache_prices(Some(&manual), ConnectionKind::ChatgptPlan, catalog), None, "plano: sem preço");
        assert_eq!(resolve_cache_prices(None, ConnectionKind::ApiKey, None), None);
    }

    #[test]
    fn agent_setup_takes_the_native_input_window_from_the_models_list() {
        let mut cfg = config_for(Path::new("/tmp/x"));
        let mut native = crate::config::Connection::native("nat", "Nativa", crate::config::ConnectionKind::Anthropic, "chave-literal");
        native.base_url_override = Some("https://janela-t08.example.com".into());
        native.normalize();
        cfg.connections.push(native);
        cfg.agents.developer.connection_id = "nat".into();
        super::super::models_native::remember_input_window("https://janela-t08.example.com", "modelo-dev", 400_000);
        super::super::models_native::remember_input_window(&cfg.connections[0].base_url, "modelo-rev", 123_456);
        let dev = agent_setup(&cfg, &cfg.agents.developer, "developer", None, None).expect("dev");
        assert_eq!(dev.context_window, Some(400_000));
        let rev = agent_setup(&cfg, &cfg.agents.reviewer, "reviewer", None, None).expect("rev");
        assert_eq!(rev.context_window, None, "por chave: o catálogo decide");
        cfg.agents.developer.model = "outro".into();
        assert_eq!(agent_setup(&cfg, &cfg.agents.developer, "developer", None, None).expect("dev").context_window, None);
    }

    #[test]
    fn agent_setup_carries_configured_max_steps_clamped() {
        let mut cfg = config_for(Path::new("/tmp/x"));
        let dev = agent_setup(&cfg, &cfg.agents.developer, "developer", None, None).expect("dev");
        assert_eq!(dev.max_steps, None, "sem o campo: padrão do papel");
        cfg.agents.developer.max_steps = Some(220);
        cfg.agents.reviewer.max_steps = Some(5);
        let dev = agent_setup(&cfg, &cfg.agents.developer, "developer", None, None).expect("dev");
        let rev = agent_setup(&cfg, &cfg.agents.reviewer, "reviewer", None, None).expect("rev");
        assert_eq!(dev.max_steps, Some(220));
        assert_eq!(rev.max_steps, Some(10), "fora da faixa: ajustado no uso");
    }

    /// Comportamento do runner fake.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Script {
        /// Emite mensagem com o segredo (deve sair redigida) e completa.
        Complete,
        Block,
        Stall,
        /// Erro com o segredo na mensagem (não deve ser ecoado).
        Fail,
        Panic,
        /// Nunca termina e ignora o cancelamento (para testes de busy e shutdown forçado).
        Hang,
        /// Respeita o cancelamento: espera o pedido e termina a etapa corrente com `Cancelled`.
        Slow,
        /// Falha na montagem do runner (aprovação deve ser desfeita).
        BuildFail,
        /// Panic na montagem do runner (aprovação deve ser desfeita pelo compensador).
        BuildPanic,
        /// Bloqueia a T01 por qualidade (arquivo real, com evidência de `../evidence.txt`); quando
        /// a tarefa volta a TODO (RETRY registrado), conclui.
        RecoverFixes,
        /// Como `RecoverFixes`, mas a tarefa volta sempre a BLOCKED (a estratégia nunca resolve).
        RecoverStuck,
        /// Bloqueio ambiental: nunca deve chamar o modelo.
        EnvBlock,
        /// Falha de build do compose (ambiental); conclui quando a tarefa volta a TODO.
        EnvComposeFixes,
        /// Falha de build do compose (ambiental) que se repete sempre.
        EnvComposeStuck,
        /// Bloqueia a T01 por qualidade; conclui quando a tarefa volta a TODO e o ambiente docker já
        /// foi ajustado pela recuperação (spec 074: imagem node:22 adotada na base).
        RecoverAdjustsEnv,
    }

    struct NoDev;
    impl crate::agents::runner::DeveloperPort for NoDev {
        fn implement<'a>(
            &'a self,
            _brief: &'a crate::agents::runner::DevBrief,
        ) -> Fut<'a, Result<crate::agents::runner::DevReport, RunnerError>> {
            Box::pin(async { Err(RunnerError::Invalid("não usado".into())) })
        }
    }
    struct NoRev;
    impl crate::agents::runner::ReviewerPort for NoRev {
        fn review<'a>(
            &'a self,
            _input: &'a crate::agents::runner::ReviewInput,
        ) -> Fut<'a, Result<crate::agents::runner::ReviewResult, RunnerError>> {
            Box::pin(async { Err(RunnerError::Invalid("não usado".into())) })
        }
    }

    /// Escreve a T01 como BLOCKED (3 reprovações), com findings e a evidência de `../evidence.txt`.
    fn write_blocked_t01(root: &Path) {
        let evidence = fs::read_to_string(root.with_file_name("evidence.txt")).unwrap_or_default();
        let path = root.join(".sdd/specs").join(SPEC).join("tasks/T01.md");
        let mut task = Task::parse(&fs::read_to_string(&path).expect("T01")).expect("parse T01");
        task.frontmatter.status = TaskStatus::Blocked;
        task.frontmatter.attempts = 3;
        task.frontmatter.next_action = Some("3 reprovações".into());
        task.frontmatter.test_evidence = Some(evidence);
        if !task.body.contains("## Findings Abertos") {
            task.body.push_str("\n## Findings Abertos\n- id: F-01\n  severity: HIGH\n  location: src/lib.rs:1\n  description: falta validar a entrada\n  recommendation: validar\n");
        }
        fs::write(&path, task.render().expect("render")).expect("gravar T01");
    }

    fn t01_path(root: &Path) -> PathBuf {
        root.join(".sdd/specs").join(SPEC).join("tasks/T01.md")
    }

    fn read_t01(root: &Path) -> Task {
        Task::parse(&fs::read_to_string(t01_path(root)).expect("T01")).expect("parse T01")
    }

    #[derive(Debug)]
    struct Scripted(Script);

    impl RunnerFactory for Scripted {
        fn build(&self, job: RunJob) -> Result<Box<dyn SpecRunner>, RunnerError> {
            match self.0 {
                Script::BuildFail => Err(RunnerError::Config("falha simulada".into())),
                Script::BuildPanic => panic!("montagem simulada"),
                script => {
                    BUILDS
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((job.root.clone(), built_adjusted(&job.root)));
                    Ok(Box::new(FakeRunner {
                        script,
                        root: job.root.clone(),
                        calls: std::sync::atomic::AtomicUsize::new(0),
                        cancel: job.cancel,
                        sink: BusSink::new(job.events, job.project_id, job.secrets)
                            .with_dialog(DialogTarget::new(job.root.clone(), job.spec_id.clone())),
                    }))
                }
            }
        }
    }

    /// Runners montados pelo factory fake: (raiz, ambiente docker já ajustado no momento da montagem).
    static BUILDS: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());

    fn built_adjusted(root: &Path) -> bool {
        fs::read_to_string(root.join(".nants/compose.yaml")).is_ok_and(|c| c.contains("node:22"))
    }

    fn builds_of(root: &Path) -> Vec<bool> {
        let log = BUILDS.lock().unwrap_or_else(PoisonError::into_inner);
        log.iter().filter(|(r, _)| r == root).map(|(_, docker)| *docker).collect()
    }

    /// Prereqs com Docker disponível (ou não), sem rodar nenhum comando real.
    fn prereqs(docker: bool) -> super::super::prereqs::PrereqChecker {
        use super::super::prereqs::{CmdOutput, PrereqChecker};
        PrereqChecker::new(Arc::new(move |program: &str, _args: &[&str]| {
            (program == "git" || docker).then(|| CmdOutput { success: true, stdout: "ok".into(), stderr: String::new() })
        }))
    }

    struct FakeRunner {
        script: Script,
        cancel: Arc<AtomicBool>,
        sink: BusSink,
        root: PathBuf,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeRunner {
        /// Runner real (novo a cada chamada), com os eventos no barramento: o Quadro os recebe.
        fn real(&self) -> TaskRunner {
            TaskRunner::new(
                self.root.clone(),
                Box::new(NoDev),
                Box::new(NoRev),
                Box::new(ShellTestExecutor::new()),
                Box::new(self.sink.clone()),
            )
        }
    }

    impl SpecRunner for FakeRunner {
        fn record_diagnosis(&self, spec_id: &str, task_id: &str, text: &str) -> Result<usize, RunnerError> {
            TaskRunner::new(
                self.root.clone(),
                Box::new(NoDev),
                Box::new(NoRev),
                Box::new(ShellTestExecutor::new()),
                Box::new(crate::agents::runner::NullSink),
            )
            .record_diagnosis(spec_id, task_id, text)
        }

        fn record_recovery(&self, spec_id: &str, task_id: &str, retry: bool, text: &str) -> Result<usize, RunnerError> {
            // Runner real (novo a cada chamada: o fake reescreve o arquivo por fora do runner).
            TaskRunner::new(
                self.root.clone(),
                Box::new(NoDev),
                Box::new(NoRev),
                Box::new(ShellTestExecutor::new()),
                Box::new(crate::agents::runner::NullSink),
            )
            .record_recovery(spec_id, task_id, retry, text)
        }

        fn record_revision(
            &self,
            spec_id: &str,
            task_id: &str,
            revision: &crate::agents::runner::TaskRevision,
            text: &str,
        ) -> Result<usize, RunnerError> {
            TaskRunner::new(
                self.root.clone(),
                Box::new(NoDev),
                Box::new(NoRev),
                Box::new(ShellTestExecutor::new()),
                Box::new(crate::agents::runner::NullSink),
            )
            .record_revision(spec_id, task_id, revision, text)
        }

        fn record_split(
            &self,
            spec_id: &str,
            task_id: &str,
            split: &crate::agents::recovery_plan::TaskSplit,
            text: &str,
        ) -> Result<(usize, String), RunnerError> {
            self.real().record_split(spec_id, task_id, split, text)
        }

        fn record_adjust(
            &self,
            spec_id: &str,
            task_id: &str,
            adjustment: &crate::agents::recovery_plan::CriteriaAdjustment,
            text: &str,
        ) -> Result<(usize, String), RunnerError> {
            self.real().record_adjust(spec_id, task_id, adjustment, text)
        }

        fn run<'a>(&'a self, _spec_id: &'a str) -> Fut<'a, Result<SpecOutcome, RunnerError>> {
            Box::pin(async move {
                match self.script {
                    Script::Slow => {
                        while !self.cancel.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Err(RunnerError::Cancelled)
                    }
                    Script::BuildFail | Script::BuildPanic => {
                        Err(RunnerError::Config("montagem não executa".into()))
                    }
                    Script::Complete => {
                        self.sink.emit(RunnerEvent::AgentMessage {
                            agent: "developer".into(),
                            text: format!("usei a chave {SECRET}"),
                            task_id: None,
                            attempt: None,
                        });
                        Ok(SpecOutcome::Completed)
                    }
                    Script::RecoverFixes
                    | Script::RecoverStuck
                    | Script::EnvBlock
                    | Script::EnvComposeFixes
                    | Script::EnvComposeStuck
                    | Script::RecoverAdjustsEnv => {
                        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
                        let todo = read_t01(&self.root).frontmatter.status == TaskStatus::Todo;
                        if matches!(self.script, Script::RecoverFixes | Script::EnvComposeFixes) && todo && !first {
                            return Ok(SpecOutcome::Completed);
                        }
                        if self.script == Script::RecoverAdjustsEnv && todo && built_adjusted(&self.root) {
                            return Ok(SpecOutcome::Completed);
                        }
                        write_blocked_t01(&self.root);
                        let compose = matches!(self.script, Script::EnvComposeFixes | Script::EnvComposeStuck);
                        let kind = if self.script == Script::EnvBlock || compose {
                            crate::agents::runner::BlockKind::Environmental
                        } else {
                            crate::agents::runner::BlockKind::Quality
                        };
                        let reason = if compose {
                            "ambiente docker: docker compose build falhou: unknown instruction"
                        } else {
                            "3 reprovações"
                        };
                        Ok(SpecOutcome::Blocked {
                            task_id: "T01".into(),
                            reason: reason.into(),
                            kind,
                            open_findings: vec![],
                        })
                    }
                    Script::Block => Ok(SpecOutcome::Blocked {
                        task_id: "T01".into(),
                        reason: "motivo interno".into(),
                        kind: crate::agents::runner::BlockKind::Quality,
                        open_findings: vec![],
                    }),
                    Script::Stall => Ok(SpecOutcome::Stalled {
                        pending: vec!["T02".into()],
                    }),
                    Script::Fail => Err(RunnerError::Config(format!("falha com {SECRET}"))),
                    Script::Panic => panic!("falha simulada no runner"),
                    Script::Hang => {
                        std::future::pending::<Result<SpecOutcome, RunnerError>>().await
                    }
                }
            })
        }
    }

    /// Prazo curto de shutdown nos testes (o padrão de 30 s só vale em produção).
    const TEST_GRACE: Duration = Duration::from_millis(200);

    fn state_with(root: &Path, cfg: Config, script: Script) -> AppState {
        state_with_compose(root, cfg, script, &Arc::new(crate::server::envs::tests::Rec::default()))
    }

    /// Como [`state_with`], com o executor Compose falso `rec` (o teste confere as chamadas).
    /// Nenhum teste daqui chama o `docker` real: a limpeza do `-test` depois do `integrated`
    /// fica local e instantânea, e os overrides vão para a pasta do próprio teste.
    fn state_with_compose(root: &Path, cfg: Config, script: Script, rec: &Arc<crate::server::envs::tests::Rec>) -> AppState {
        let compose = crate::server::envs::ComposeProvider::with_runner(rec.clone(), root.with_file_name("compose-overrides"));
        AppState::new(root.with_file_name("config.toml"), crate::server::readiness::chat_ready(cfg), PORT)
            .with_runner_factory(Arc::new(Scripted(script)))
            .with_shutdown_grace(TEST_GRACE)
            .with_compose_provider(compose)
    }

    /// Apaga a pasta do teste ao sair do escopo, passe ou falhe.
    struct TempGuard(PathBuf);

    impl Drop for TempGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn state(root: &Path, script: Script) -> AppState {
        state_with(root, config_for(root), script)
    }

    fn request(method: Method, uri: &str, body: &str, content_type: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, HOST);
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        builder.body(Body::from(body.to_string())).expect("requisição")
    }

    fn approve_req(project: &str, spec: &str, body: &str) -> Request<Body> {
        request(
            Method::POST,
            &format!("/api/projects/{project}/specs/{spec}/approve"),
            body,
            Some("application/json"),
        )
    }

    async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, String) {
        let resp = router(state.clone()).oneshot(req).await.expect("resposta");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("corpo");
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    /// Agenda `hook` para a etapa `stage` ("git" ou "mark") deste projeto.
    fn set_hook(root: &Path, stage: &'static str, hook: impl FnOnce() + Send + 'static) {
        HOOKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((root.to_path_buf(), stage, Box::new(hook)));
    }

    /// Eventos já publicados e ainda não lidos.
    fn drain(rx: &mut broadcast::Receiver<ApiEvent>) -> Vec<ApiEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn json_of(text: &str) -> Value {
        serde_json::from_str(text).expect("json")
    }

    fn spec_status(root: &Path, spec: &str) -> SpecStatus {
        let text = fs::read_to_string(root.join(".sdd/specs").join(spec).join("spec.md"))
            .expect("spec.md");
        Spec::parse(&text).expect("spec").frontmatter.status
    }

    /// Consome eventos até `project_status` com `status`; devolve todos (inclusive o último).
    async fn collect_until(
        rx: &mut broadcast::Receiver<ApiEvent>,
        status: &str,
    ) -> Vec<ApiEvent> {
        let mut out = Vec::new();
        loop {
            let ev = tokio::time::timeout(WAIT, rx.recv())
                .await
                .expect("evento dentro do prazo")
                .expect("canal aberto");
            let done = ev.event_type() == "project_status"
                && ev.payload["status"].as_str() == Some(status);
            out.push(ev);
            if done {
                return out;
            }
        }
    }

    /// Espera (até 2 s) o registro de execuções ficar vazio.
    async fn wait_idle(state: &AppState) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if state.runs().active(PROJECT).is_none() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    async fn expect_conflict(tag: &str, settings: &str, tasks: &[(&str, &str)], reason: &str) {
        let root = setup(tag, false);
        write_sdd(&root, settings, "AWAITING_APPROVAL", tasks);
        let state = state(&root, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], reason, "{body}");
    }

    #[tokio::test]
    async fn starting_a_run_stops_the_projects_test_app_with_event() {
        let root = setup("approve-stops-app", true);
        let state = state(&root, Script::Complete).with_app_observe_window(Duration::ZERO);
        let mut rx = state.events().subscribe();
        // App no ar no `-run` (todo app é Docker, spec 074), com executor Compose falso.
        let rec = crate::server::envs::tests::Rec::with_ports();
        let compose = crate::server::envs::tests::provider(&rec)
            .for_project(&root, PROJECT, crate::harness::compose::ComposeKind::Run)
            .expect("projeto docker");
        state
            .apps()
            .launch_docker(PROJECT, &root, "outra", compose, "npm run dev", 3000)
            .await
            .expect("app de teste");
        assert!(state.apps().is_running(PROJECT));
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let events = collect_until(&mut rx, "integrated").await;
        assert!(!state.apps().is_running(PROJECT), "app deve ter sido encerrado");
        let exited = events
            .iter()
            .find(|e| e.event_type() == "app_exited")
            .expect("evento app_exited");
        assert_eq!(exited.payload["reason"], "run_started");
    }

    #[tokio::test]
    async fn approve_marks_in_progress_and_runs_in_background() {
        let root = setup("approve-ok", true);
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(json_of(&body), json!({"ok": true, "spec_id": SPEC}));
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);

        let events = collect_until(&mut rx, "integrated").await;
        assert_eq!(events[0].event_type(), "project_status");
        assert_eq!(events[0].payload["status"], "executing");
        assert_eq!(events[0].payload["spec_id"], SPEC);
        let text: String = events
            .iter()
            .map(|e| e.payload.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains(SECRET), "segredo vazou: {text}");
        assert!(text.contains("usei a chave ***"), "{text}");
        assert!(state.runs().active(PROJECT).is_none(), "reserva liberada");
    }

    /// Spec 134 T05: a aprovação congela o SHA-256 de cada fonte da base citada no plano (com o
    /// catálogo) em `.sdd/kb/frozen/<spec>.json`, fora do Git.
    #[tokio::test]
    async fn approve_freezes_the_cited_kb_sources() {
        let root = setup("approve-kb-freeze", true);
        // Como a API da base faz antes de gravar: `.sdd/kb/` fora do Git.
        safe_fs::append_line_once(&root, ".git/info/exclude", crate::kb::KB_EXCLUDE_LINE).expect("exclude");
        let none = SecretSet::new(Vec::<String>::new());
        let doc = crate::kb::add_file(&root, "padroes.md", b"# Filas\nUse filas.\n", &none, "2026-10-28T12:00:00Z").expect("md");
        let task = root.join(format!(".sdd/specs/{SPEC}/tasks/T01.md"));
        let text = fs::read_to_string(&task).expect("tarefa");
        fs::write(&task, format!("{}\n- Filas (kb:{}#filas)\n", text.trim_end(), doc.id)).expect("citar");
        git(&root, &["add", ".sdd/specs"]);
        git(&root, &["commit", "-qm", "chore: cita a base"]);
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        // O instante antes da aprovação: o fim do plano tem de cair entre ele e o fim do teste, sem
        // um teto fixo de duração (sob carga a execução até o "integrated" pode passar de minutos).
        let before = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        collect_until(&mut rx, "integrated").await;
        let frozen = crate::kb::cite::load_frozen(&root, SPEC).expect("congelado");
        assert_eq!(frozen.sources.len(), 1);
        assert_eq!(frozen.sources[0].citation, format!("kb:{}#filas", doc.id));
        assert_eq!(frozen.sources[0].sha256.len(), 64);
        assert!(frozen.catalog.contains(&format!("- kb:{} |", doc.id)));
        let exclude = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
        assert!(exclude.lines().any(|l| l == crate::kb::KB_EXCLUDE_LINE), "fora do Git: {exclude}");
        // Spec 136 T05: a aprovação concluída fecha a sessão do plano das menções.
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let closed = crate::kb::mention::plan_closed_ts(&root).expect("fim do plano gravado");
        assert!(before <= closed && closed <= now, "{before} <= {closed} <= {now}");
        assert!(root.join(".sdd/kb/mentions/.closed.json").is_file());
    }

    /// Backend do relatório que só responde quando o teste libera (F-02).
    struct Gated(Arc<tokio::sync::Notify>, std::sync::atomic::AtomicBool);

    impl ChatBackend for Gated {
        fn complete<'a>(
            &'a self,
            _req: &'a crate::agents::llm_client::ChatRequest,
        ) -> Pin<Box<dyn std::future::Future<Output = crate::agents::architect::testing::Reply> + Send + 'a>> {
            let gate = Arc::clone(&self.0);
            // Só a 1ª chamada (relatório) espera a liberação; a atualização do ARCHITECTURE.md
            // que vem depois (`.sdd/` não conta mais como base suja) responde na hora.
            let first = !self.1.swap(true, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if first {
                    gate.notified().await;
                }
                crate::agents::architect::testing::text("# Relatório\n\n## Como executar\n1. `cargo run`")
            })
        }
    }

    #[tokio::test]
    async fn report_in_progress_refuses_approval_with_own_reason_and_keeps_report() {
        let root = setup("report-busy", true);
        let mut cfg = config_for(&root);
        cfg.agents.architect = AgentConfig {
            connection_id: "literal".into(),
            model: "modelo-arq".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        let gate = Arc::new(tokio::sync::Notify::new());
        let state = state_with(&root, cfg, Script::Complete).with_report_backend(Arc::new(Gated(Arc::clone(&gate), std::sync::atomic::AtomicBool::new(false))));
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        collect_until(&mut rx, "integrated").await;

        // Enquanto o relatório é gerado, aprovação recusa com motivo próprio; o estado mostra o relatório.
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], "report_in_progress", "{body}");
        assert_eq!(error_of(&body), "Gerando o relatório final do Arquiteto; tente de novo em instantes.", "{body}");
        let (status, body) = call(&state, get_req(&format!("/api/projects/{PROJECT}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["report_pending"], true, "{body}");
        assert_eq!(json_of(&body)["running"], false, "{body}");

        // Liberado o modelo, o relatório é gravado (não foi perdido para a aprovação concorrente).
        gate.notify_one();
        let ready = tokio::time::timeout(WAIT, async {
            loop {
                let ev = rx.recv().await.expect("canal aberto");
                if ev.event_type() == "report_ready" {
                    return ev;
                }
            }
        })
        .await
        .expect("report_ready dentro do prazo");
        assert_eq!(ready.payload["spec_id"], SPEC);
        assert!(root.join(".sdd/specs").join(SPEC).join("report.md").exists());
        // A reserva sai logo depois do evento: espera curta até o estado mostrar ocioso.
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let (_, body) = call(&state, get_req(&format!("/api/projects/{PROJECT}/state"))).await;
            if json_of(&body)["report_pending"] == false {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "report_pending preso: {body}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(state.runs().active(PROJECT).is_none(), "reserva liberada");
    }

    #[tokio::test]
    async fn completed_run_publishes_report_ready_after_integrated() {
        let root = setup("report-trigger", true);
        let _cleanup = TempGuard(root.parent().expect("pasta do teste").to_path_buf());
        let mut cfg = config_for(&root);
        cfg.agents.architect = AgentConfig {
            connection_id: "literal".into(),
            model: "modelo-arq".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        let backend = crate::agents::architect::testing::Scripted::new(vec![
            crate::agents::architect::testing::text("# Relatório\n\n## Como executar\n1. `cargo run`"),
        ]);
        let rec = Arc::new(crate::server::envs::tests::Rec::default());
        let state = state_with_compose(&root, cfg, Script::Complete, &rec).with_report_backend(backend);
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let events = collect_until(&mut rx, "integrated").await;
        assert!(events.iter().all(|e| e.event_type() != "report_failed"));
        let ready = tokio::time::timeout(WAIT, async {
            loop {
                let ev = rx.recv().await.expect("canal aberto");
                if ev.event_type() == "report_ready" {
                    return ev;
                }
            }
        })
        .await
        .expect("report_ready dentro do prazo");
        assert_eq!(ready.payload["spec_id"], SPEC);
        // A limpeza do `-test` antes do relatório passou pelo executor falso, não pelo docker real.
        let downs: Vec<_> = rec.compose_calls().into_iter().filter(|(_, args)| args.first().is_some_and(|a| a == "down")).collect();
        assert_eq!(downs.len(), 1, "{downs:?}");
        assert!(downs[0].1.iter().any(|a| a == "-v"), "{downs:?}");
        let report = root.join(".sdd/specs").join(SPEC).join("report.md");
        assert!(fs::read_to_string(report).expect("relatório").contains("cargo run"));
        // O fim da finalização é sinalizado depois da atualização de arquitetura e da vaga liberada.
        let done = tokio::time::timeout(WAIT, async {
            loop {
                let ev = rx.recv().await.expect("canal aberto");
                if ev.event_type() == "finalization_done" {
                    return ev;
                }
            }
        })
        .await
        .expect("finalization_done dentro do prazo");
        assert_eq!(done.payload["spec_id"], SPEC);
        assert!(!state.runs().is_report(PROJECT), "vaga já liberada quando o evento sai");
    }

    #[tokio::test]
    async fn finalization_guard_publishes_after_release_even_on_panic() {
        let state = state(&setup("final-guard", true), Script::Complete);
        let mut rx = state.events().subscribe();
        let guard = state.runs().try_start(PROJECT, REPORT_RESERVATION).expect("vaga");
        let events = state.events().clone();
        let probe = state.clone();
        let handle = tokio::spawn(async move {
            let _hold = FinalizationGuard {
                slot: Some(guard),
                events,
                project_id: PROJECT.to_string(),
                spec_id: SPEC.to_string(),
            };
            assert!(probe.runs().is_report(PROJECT));
            panic!("falha simulada");
        });
        assert!(handle.await.is_err());
        let ev = rx.recv().await.expect("evento");
        assert_eq!(ev.event_type(), "finalization_done");
        assert!(!state.runs().is_report(PROJECT));
    }

    #[tokio::test]
    async fn second_approve_while_running_is_409_and_shutdown_releases_run() {
        let root = setup("busy", true);
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");

        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["code"], BUSY);

        let other = root.join(".sdd/specs/s2/tasks");
        fs::create_dir_all(&other).expect("s2");
        fs::write(root.join(".sdd/specs/s2/spec.md"), spec_md("s2", "AWAITING_APPROVAL"))
            .expect("spec s2");
        fs::write(other.join("T01.md"), task_md("T01", "", "TODO")).expect("task s2");
        let (status, body) = call(&state, approve_req(PROJECT, "s2", "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["code"], BUSY);

        state.trigger_shutdown();
        assert!(wait_idle(&state).await, "shutdown deve liberar a execução");
    }

    #[tokio::test]
    async fn unresponsive_runner_is_abandoned_after_grace_and_guard_released() {
        let root = setup("shutdown-forced", true);
        let state = state(&root, Script::Hang);
        let mut rx = state.events().subscribe();
        let (status, _) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let started = Instant::now();
        state.trigger_shutdown();
        assert!(wait_idle(&state).await, "guard não liberado");
        assert!(started.elapsed() < Duration::from_secs(2));
        let events = collect_until(&mut rx, "idle").await;
        let last = events.last().expect("evento");
        assert_eq!(last.payload["reason"], "shutdown_forced");
    }

    #[tokio::test]
    async fn cooperative_runner_finishes_cleanly_on_shutdown() {
        let root = setup("shutdown-coop", true);
        let state = state_with(&root, config_for(&root), Script::Slow)
            .with_shutdown_grace(Duration::from_secs(10));
        let mut rx = state.events().subscribe();
        let (status, _) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        collect_until(&mut rx, "executing").await;
        state.trigger_shutdown();
        let events = collect_until(&mut rx, "idle").await;
        assert_eq!(events.last().expect("evento").payload["reason"], "shutdown");
        assert!(!events.iter().any(|e| e.payload["reason"] == "shutdown_forced"));
        assert!(wait_idle(&state).await);
    }

    fn stop_req(project: &str, spec: &str) -> Request<Body> {
        request(
            Method::POST,
            &format!("/api/projects/{project}/specs/{spec}/stop"),
            "{}",
            Some("application/json"),
        )
    }

    /// Spec 086 T04: parar aciona o cancelamento da execução, responde 202 (também no 2º pedido) e o
    /// desfecho é `idle` com `reason: "stopped"` e a mensagem do sistema; a spec fica IN_PROGRESS.
    #[tokio::test]
    async fn stop_route_cancels_the_run_and_ends_idle_stopped() {
        let root = setup("stop-ok", true);
        let state = state_with(&root, config_for(&root), Script::Slow)
            .with_shutdown_grace(Duration::from_secs(10));
        let mut rx = state.events().subscribe();
        let (status, _) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        collect_until(&mut rx, "executing").await;
        let (status, body) = call(&state, stop_req(PROJECT, SPEC)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(json_of(&body), json!({"stopping": true}));
        let (status, body) = call(&state, stop_req(PROJECT, SPEC)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "idempotente: {body}");
        let events = collect_until(&mut rx, "idle").await;
        assert_eq!(events.last().expect("evento").payload["reason"], STOPPED_REASON);
        assert!(!events.iter().any(|e| e.payload["reason"] == "shutdown"));
        let said = events
            .iter()
            .any(|e| e.payload["agent"] == SYSTEM_AGENT && e.payload["text"] == STOPPED_TEXT);
        assert!(said, "mensagem do sistema no diálogo");
        assert!(wait_idle(&state).await, "a vaga é liberada");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress, "spec fica em andamento");
        let (status, body) = call(&state, stop_req(PROJECT, SPEC)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["code"], NOT_RUNNING_MSG);
        state.trigger_shutdown();
    }

    #[tokio::test]
    async fn stop_route_refuses_without_run_unknown_ids_and_other_spec() {
        let root = setup("stop-refuse", true);
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, stop_req(PROJECT, SPEC)).await;
        assert_eq!(status, StatusCode::CONFLICT, "sem execução: {body}");
        assert_eq!(json_of(&body)["code"], NOT_RUNNING_MSG);
        let (status, _) = call(&state, stop_req(PROJECT, "nao-existe")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&state, stop_req("outro", SPEC)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&state, stop_req(PROJECT, "s!1")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = call(&state, stop_req("p!x", SPEC)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        // Execução de outra spec do projeto: esta não está ativa (409) e a outra segue.
        let other = root.join(".sdd/specs/s2/tasks");
        fs::create_dir_all(&other).expect("s2");
        fs::write(root.join(".sdd/specs/s2/spec.md"), spec_md("s2", "AWAITING_APPROVAL")).expect("spec s2");
        fs::write(other.join("T01.md"), task_md("T01", "", "TODO")).expect("task s2");
        let (status, _) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let (status, _) = call(&state, stop_req(PROJECT, "s2")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(state.runs().active(PROJECT).is_some(), "a execução ativa não é tocada");
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    /// A flag fica na vaga do projeto: o relançamento do mesmo job (mesma reserva) herda o pedido.
    #[test]
    fn stop_signal_lives_in_the_slot_and_is_shared_by_relaunch() {
        let registry = RunRegistry::new();
        assert!(!registry.request_stop(PROJECT, SPEC), "sem vaga");
        let guard = registry.try_start(PROJECT, SPEC).expect("vaga");
        let first = guard.stop_signal();
        assert!(!registry.request_stop(PROJECT, "s2"), "outra spec");
        assert!(registry.request_stop(PROJECT, SPEC));
        assert!(registry.request_stop(PROJECT, SPEC), "idempotente");
        let relaunch = guard.stop_signal();
        assert!(first.requested() && relaunch.requested());
        assert!(relaunch.cancel_flag().load(Ordering::SeqCst));
        assert!(Arc::ptr_eq(&first.cancel_flag(), &relaunch.cancel_flag()));
        registry.retag(PROJECT, REPORT_RESERVATION);
        assert!(!registry.request_stop(PROJECT, REPORT_RESERVATION), "relatório não é execução");
        drop(guard);
        let fresh = registry.try_start(PROJECT, SPEC).expect("vaga nova");
        assert!(!fresh.stop_signal().requested(), "novo run começa sem pedido");
    }

    #[tokio::test]
    async fn shutdown_refuses_new_approval_without_touching_spec() {
        let root = setup("shutdown-refuse", true);
        let state = state(&root, Script::Complete);
        state.trigger_shutdown();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
    }

    #[tokio::test]
    async fn spawn_run_does_not_start_runner_when_shutdown_already_set() {
        #[derive(Debug)]
        struct Probe(Arc<AtomicBool>);
        impl SpecRunner for Probe {
            fn run<'a>(&'a self, _spec_id: &'a str) -> Fut<'a, Result<SpecOutcome, RunnerError>> {
                self.0.store(true, Ordering::SeqCst);
                Box::pin(async { Ok(SpecOutcome::Completed) })
            }
        }
        let started = Arc::new(AtomicBool::new(false));
        let registry = RunRegistry::new();
        let guard = registry.try_start(PROJECT, SPEC).expect("reserva");
        let events = EventBus::new();
        let mut rx = events.subscribe();
        let (tx, shutdown) = watch::channel(false);
        tx.send_replace(true);
        spawn_run(Background {
            guard,
            runner: Box::new(Probe(Arc::clone(&started))),
            spec_id: SPEC.into(),
            project_id: PROJECT.into(),
            root: PathBuf::from("/nonexistent-nants-root"),
            events,
            secrets: Arc::new(SecretSet::default()),
            shutdown,
            cancel: Arc::new(AtomicBool::new(false)),
            stop: StopSignal::default(),
            grace: TEST_GRACE,
            notes: Vec::new(),
            state: AppState::new(PathBuf::from("/nonexistent-nants-root/config.toml"), Config::default(), PORT),
            roles: Vec::new(),
        });
        assert!(!started.load(Ordering::SeqCst), "runner não deve iniciar");
        assert!(registry.active(PROJECT).is_none(), "reserva liberada");
        assert!(drain(&mut rx).is_empty(), "spawn_run não publica status");
    }

    #[tokio::test]
    async fn aborting_handler_after_mark_still_launches_runner() {
        let root = setup("abort-handler", true);
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        set_hook(&root, "mark", move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        });
        let handler = tokio::spawn(router(state.clone()).oneshot(approve_req(PROJECT, SPEC, "{}")));
        let reached = tokio::task::spawn_blocking(move || started_rx.recv())
            .await
            .expect("join");
        assert!(reached.is_ok(), "gravação não alcançada");
        handler.abort();
        let _ = release_tx.send(());

        let events = collect_until(&mut rx, "integrated").await;
        assert!(!events.is_empty());
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        assert!(wait_idle(&state).await, "reserva liberada");
    }

    /// Falha na montagem do runner depois de `IN_PROGRESS`: a spec volta a `AWAITING_APPROVAL`.
    async fn expect_restored_after_build_failure(tag: &str, script: Script) {
        let root = setup(tag, true);
        let original = fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("spec");
        let state = state(&root, script);
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(
            fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("spec"),
            original,
            "spec deve voltar ao conteúdo original"
        );
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        let events = collect_until(&mut rx, "idle").await;
        let last = events.last().expect("evento");
        assert_eq!(last.payload["reason"], "error");
        assert!(last.payload.get("needs_manual_recovery").is_none(), "caminho feliz");
        assert!(wait_idle(&state).await, "reserva liberada após falha");
    }

    #[tokio::test]
    async fn build_failure_after_mark_restores_awaiting_approval() {
        expect_restored_after_build_failure("build-fail", Script::BuildFail).await;
    }

    #[tokio::test]
    async fn build_panic_after_mark_restores_awaiting_approval() {
        expect_restored_after_build_failure("build-panic", Script::BuildPanic).await;
    }

    /// Shutdown acionado depois da gravação: 503, spec volta ao original, nenhum runner,
    /// reserva liberada e um único evento `idle` (reason=shutdown).
    #[tokio::test]
    async fn shutdown_during_mark_restores_spec_and_refuses() {
        let root = setup("shutdown-mark", true);
        let original = fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("spec");
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        let stopper = state.clone();
        set_hook(&root, "mark", move || stopper.trigger_shutdown());
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(
            fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("spec"),
            original,
            "spec deve voltar ao conteúdo original"
        );
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(wait_idle(&state).await, "reserva liberada");
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "um único evento: {events:?}");
        assert_eq!(events[0].payload["status"], "idle");
        assert_eq!(events[0].payload["reason"], "shutdown");
    }

    /// Falha da regravação do original: `idle` com `restore_failed` e `needs_manual_recovery`,
    /// spec permanece IN_PROGRESS, reserva liberada e a resposta não ecoa o erro.
    #[tokio::test]
    async fn failed_restore_reports_restore_failed_with_manual_recovery() {
        let root = setup("restore-fail", true);
        let state = state(&root, Script::BuildFail);
        let mut rx = state.events().subscribe();
        FORCED_RESTORE_FAILURES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(root.clone());
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(!body.contains("falha simulada"), "{body}");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        assert!(wait_idle(&state).await, "reserva liberada");
        let events = drain(&mut rx);
        let idle: Vec<&ApiEvent> = events
            .iter()
            .filter(|e| e.payload["status"] == "idle")
            .collect();
        assert_eq!(idle.len(), 1, "um único idle: {events:?}");
        assert_eq!(idle[0].payload["reason"], "restore_failed");
        assert_eq!(idle[0].payload["needs_manual_recovery"], true);
    }

    /// Shutdown acionado durante o Git da base: 503 sem gravar nada e sem runner.
    #[tokio::test]
    async fn shutdown_during_git_check_refuses_without_writing() {
        let root = setup("shutdown-git", true);
        let original = fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("spec");
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        let stopper = state.clone();
        set_hook(&root, "git", move || stopper.trigger_shutdown());
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(
            fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("spec"),
            original
        );
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(wait_idle(&state).await, "reserva liberada");
        assert!(drain(&mut rx).is_empty(), "nenhum evento esperado");
    }

    #[tokio::test]
    async fn blocked_run_publishes_blocked_with_fixed_reason() {
        let root = setup("blocked", true);
        let state = state(&root, Script::Block);
        let mut rx = state.events().subscribe();
        let (status, _) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let events = collect_until(&mut rx, "blocked").await;
        let last = events.last().expect("evento");
        assert_eq!(last.payload["task_id"], "T01");
        assert_eq!(last.payload["reason"], BLOCKED_REASON);
        assert!(!last.payload.to_string().contains("motivo interno"));
    }

    #[tokio::test]
    async fn stalled_run_publishes_idle_stalled() {
        let root = setup("stalled", true);
        let state = state(&root, Script::Stall);
        let mut rx = state.events().subscribe();
        let (status, _) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let events = collect_until(&mut rx, "idle").await;
        assert_eq!(events.last().expect("evento").payload["reason"], "stalled");
    }

    /// Erro do runner ou panic: mensagem do sistema (`expected`; a causa do erro vem com scrub), sem
    /// vazar segredo, e liberação da reserva.
    async fn expect_error_end(tag: &str, script: Script, expected: &str) {
        let root = setup(tag, true);
        let state = state(&root, script);
        let mut rx = state.events().subscribe();
        let (status, _) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let events = collect_until(&mut rx, "idle").await;
        let text: String = events
            .iter()
            .map(|e| e.payload.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains(SECRET), "segredo vazou: {text}");
        assert!(text.contains(expected), "{text}");
        assert!(text.contains("\"reason\":\"error\""), "{text}");
        assert!(wait_idle(&state).await, "reserva não liberada após erro");
    }

    #[tokio::test]
    async fn system_message_of_failed_run_is_persisted_to_dialog() {
        let root = setup("dialog-system", true);
        let state = state(&root, Script::Fail);
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        collect_until(&mut rx, "idle").await;
        let path = root.join(".sdd/specs").join(SPEC).join("dialog.jsonl");
        crate::server::dialog::flush_project(&root, Duration::from_secs(5));
        let text = fs::read_to_string(&path).expect("diálogo gravado");
        let line: Value = serde_json::from_str(text.lines().last().expect("linha")).expect("json");
        assert_eq!(line["agent"], "system");
        let shown = line["text"].as_str().expect("texto");
        assert!(shown.starts_with(FAILED_WITH_CAUSE), "{shown}");
        assert!(!shown.contains(SECRET), "segredo vazou: {shown}");
        assert!(line["task_id"].is_null());
        assert!(line["attempt"].is_null());
        assert!(line["seq"].as_u64().is_some(), "{line}");
    }

    /// Mensagens do sistema gravadas no `dialog.jsonl` da spec.
    fn system_lines(root: &Path) -> Vec<Value> {
        crate::server::dialog::flush_project(root, Duration::from_secs(5));
        let text = fs::read_to_string(root.join(".sdd/specs").join(SPEC).join("dialog.jsonl")).unwrap_or_default();
        text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|l| l["agent"] == SYSTEM_AGENT).collect()
    }

    /// Spec 137 T12 (trava, 135 P4): num projeto en, o fluxo grava no `dialog.jsonl` as mensagens do
    /// sistema da parada e da falha em inglês, com o `code` para a web; nenhuma sai em pt.
    #[tokio::test]
    async fn english_project_records_stop_and_failure_in_english() {
        use crate::i18n::portuguese_in;
        let root = setup("en-stop", true);
        let stop_state = state_with(&root, config_for(&root), Script::Slow).with_shutdown_grace(Duration::from_secs(10));
        stop_state.project_lang(PROJECT).set(Lang::En);
        let mut rx = stop_state.events().subscribe();
        let (status, _) = call(&stop_state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        collect_until(&mut rx, "executing").await;
        let (status, _) = call(&stop_state, stop_req(PROJECT, SPEC)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        collect_until(&mut rx, "idle").await;
        assert!(wait_idle(&stop_state).await);
        let lines = system_lines(&root);
        let stopped = lines.iter().find(|l| l["code"] == CODE_STOPPED).expect("parada gravada");
        assert_eq!(stopped["text"], "Run stopped by the user. Use Continue to resume.");
        stop_state.trigger_shutdown();

        let root = setup("en-panic", true);
        let state = state(&root, Script::Panic);
        state.project_lang(PROJECT).set(Lang::En);
        let mut rx = state.events().subscribe();
        let (status, _) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        collect_until(&mut rx, "idle").await;
        assert!(wait_idle(&state).await);
        let lines = system_lines(&root);
        let failed = lines.iter().find(|l| l["code"] == CODE_RUN_FAILED).expect("falha gravada");
        assert_eq!(failed["text"], "The run was interrupted by an internal error; check the project state.");
        for line in lines.iter().chain([stopped]) {
            let text = line["text"].as_str().unwrap_or_default();
            assert_eq!(portuguese_in(text), None, "pt gravado num projeto en: {text}");
        }
    }

    /// Spec 137 T12 (trava, 135 P4 e P6): todo texto fixo que o servidor grava no idioma do projeto sai
    /// sem pt num projeto en: as mensagens finais do run, o relançamento recusado, o ajuste do docker,
    /// os avisos de imagem, o perfil de especialista (linha de papel, seções e motivo das lições) e o
    /// prompt do Arquiteto que pede o perfil.
    #[test]
    fn english_project_records_no_portuguese_text() {
        use crate::agents::llm_client::{BadKind, BadReason};
        use crate::i18n::portuguese_in;
        use crate::sdd::skills;
        let secrets = SecretSet::new(vec![SECRET.to_string()]);
        let en = Lang::En;
        let mut texts: Vec<String> = Vec::new();
        let bad = ProviderFailure::BadResponse(BadReason::with_code(BadKind::Incomplete, Some("content_filter")));
        for end in [
            RunEnd::Stopped,
            RunEnd::Failed(None),
            RunEnd::Failed(Some("boom".into())),
            RunEnd::Panicked,
            RunEnd::Provider(bad, None),
            RunEnd::Provider(ProviderFailure::Http(429, None), None),
        ] {
            for ev in terminal_events(en, PROJECT, &secrets, SPEC, end) {
                if ev.payload["agent"] == SYSTEM_AGENT {
                    assert!(ev.payload["code"].is_string(), "{}", ev.payload);
                    texts.push(ev.payload["text"].as_str().unwrap_or_default().to_string());
                }
            }
        }
        assert!(texts.len() >= 6, "{texts:?}");
        // Causa do `RunnerError` no idioma do projeto, no texto e no `params.cause`.
        let failures = [
            RunnerError::Config("x".into()),
            RunnerError::Invalid("y".into()),
            RunnerError::Docker("z".into()),
            RunnerError::Tests("t".into()),
            RunnerError::Port { role: "developer", message: "m".into() },
            RunnerError::ExternalEdit { path: "spec.md".into() },
            RunnerError::DependenciesPending { task: "T02".into(), pending: vec!["T01".into()] },
            RunnerError::Io { path: "a.txt".into(), source: std::io::Error::other("e") },
            RunnerError::InvalidId { name: "spec", value: "v".into() },
        ];
        for err in failures {
            let end = run_end(en, Ok(Err(err)));
            let RunEnd::Failed(Some(cause)) = &end else { panic!("falha esperada") };
            assert!(!cause.is_empty());
            let ev = terminal_events(en, PROJECT, &secrets, SPEC, end);
            assert_eq!(ev[0].payload["code"], CODE_RUN_FAILED_CAUSE);
            texts.push(ev[0].payload["params"]["cause"].as_str().unwrap_or_default().to_string());
            texts.push(ev[0].payload["text"].as_str().unwrap_or_default().to_string());
        }
        let config = terminal_events(en, PROJECT, &secrets, SPEC, run_end(en, Ok(Err(RunnerError::Config("x".into())))));
        assert_eq!(config[0].payload["text"], "The run was interrupted: invalid configuration: x; check the project state.");
        texts.push(crate::server::image_gen::image_suspended_text(en, 1));
        assert!(texts.last().is_some_and(|t| t.contains("(1 attempt)")), "{texts:?}");
        let docker = ApiError::coded(StatusCode::CONFLICT, "docker_unavailable", params([("item", json!("docker_cli"))]));
        let env = conflict_reason("environment_invalid", REASON_ENV_INVALID).with_detail("compose.yaml");
        for refusal in [Some(&docker), Some(&env), None] {
            let (text, params) = relaunch_failure(en, &secrets, refusal);
            texts.push(text);
            texts.push(params["message"].as_str().unwrap_or_default().to_string());
        }
        texts.push(coded_text(en, CODE_DOCKER_MIGRATION, &[]));
        texts.push(crate::i18n::tr(en, "system.image.unavailable", &[]));
        texts.push(crate::server::image_gen::image_failure_notice(en, "web/img/a.webp", &bad));
        texts.push(crate::server::image_gen::image_suspended_text(en, 2));
        // Perfil de especialista.
        texts.push(skills::role_line(en, "developer", "Rust"));
        texts.push(skills::role_line(en, "reviewer", "Rust"));
        let profile = "---\nrole: developer\nstack: [rust]\nupdated_at: 2026-10-30\nsource_spec: 001-x\n---\n\
                       You are an expert developer in Rust for this project.\n## Pitfalls\n- `cargo test` runs the suite.\n";
        let updated = skills::with_lessons(en, profile, &["- run clippy (T01)".to_string()], "001-x", "2026-10-30").expect("lições");
        texts.push(updated);
        let prompt = crate::agents::prompts::architect_system_for(en);
        let start = prompt.find(".sdd/skills/developer.md").expect("trecho do perfil");
        let end = start + prompt[start..].find("critério de aprovação").expect("fim do trecho");
        let roles = &prompt[prompt[..end].rfind("PRIMEIRA linha").expect("linha de papel")..end];
        for marker in ["You are an expert developer in <stack> for this project.", "## User notes", "## Project lessons"] {
            assert!(roles.contains(marker), "{marker}: {roles}");
        }
        for (k, text) in texts.iter().enumerate() {
            assert!(!text.trim().is_empty(), "texto {k} vazio");
            assert_eq!(portuguese_in(text), None, "pt gravado num projeto en ({k}): {text}");
        }
    }

    #[tokio::test]
    async fn runner_error_is_reported_with_its_cause() {
        // Spec 081 T02: a causa do `RunnerError` aparece (com scrub); o pânico mantém o texto fixo.
        expect_error_end("err", Script::Fail, FAILED_WITH_CAUSE).await;
    }

    /// Início da mensagem de falha do `Script::Fail` (o segredo da causa sai redigido em seguida).
    const FAILED_WITH_CAUSE: &str = "A execução foi interrompida: configuração inválida: falha com ";

    #[tokio::test]
    async fn runner_panic_releases_run_and_reports_idle() {
        expect_error_end("panic", Script::Panic, RUN_FAILED_TEXT).await;
    }

    #[test]
    fn run_guard_releases_on_panic_and_is_exclusive_per_project() {
        let registry = RunRegistry::new();
        let first = registry.try_start("p", "s1").expect("primeira");
        assert_eq!(registry.try_start("p", "s2").err(), Some(Busy));
        assert!(registry.try_start("q", "s1").is_ok(), "outro projeto livre");
        drop(first);
        assert!(registry.active("p").is_none());

        let reg = registry.clone();
        let outcome = panic::catch_unwind(AssertUnwindSafe(move || {
            let _guard = reg.try_start("p", "s1").expect("reserva");
            panic!("interrompido");
        }));
        assert!(outcome.is_err());
        assert!(registry.try_start("p", "s1").is_ok(), "panic liberou a vaga");
    }

    #[tokio::test]
    async fn state_reports_running_spec_and_clears_after_shutdown() {
        let root = setup("state-run", true);
        let state = state(&root, Script::Hang);
        let (_, before) = call(&state, get_req(&format!("/api/projects/{PROJECT}/state"))).await;
        assert_eq!(json_of(&before)["running"], false);
        assert!(json_of(&before)["run"].is_null());

        let (status, _) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let (_, during) = call(&state, get_req(&format!("/api/projects/{PROJECT}/state"))).await;
        let v = json_of(&during);
        assert_eq!(v["running"], true);
        assert_eq!(v["run"]["spec_id"], SPEC);
        assert_eq!(v["run"]["status"], "executing");

        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    fn get_req(uri: &str) -> Request<Body> {
        request(Method::GET, uri, "", None)
    }

    #[tokio::test]
    async fn non_approvable_status_is_409() {
        let root = setup("status", true);
        let state = state(&root, Script::Complete);
        for (status, expected) in [
            ("DRAFT", SPEC_DRAFT),
            ("COMPLETED", SPEC_NOT_AWAITING),
            ("IN_PROGRESS", SPEC_NOT_AWAITING),
            ("BLOCKED", SPEC_NOT_AWAITING),
        ] {
            write_sdd(&root, SETTINGS, status, &[("T01", "")]);
            let (code, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
            assert_eq!(code, StatusCode::CONFLICT, "{status}: {body}");
            assert_eq!(json_of(&body)["code"], expected, "{status}");
        }
    }

    #[tokio::test]
    async fn spec_without_tasks_is_409_plan_without_tasks() {
        expect_conflict("no-tasks", SETTINGS, &[], REASON_PLAN_WITHOUT_TASKS).await;
        assert_eq!(REASON_PLAN_WITHOUT_TASKS, "plan_without_tasks");
    }

    #[tokio::test]
    async fn approve_during_architect_turn_is_409_and_changes_nothing() {
        let root = setup("arch-turn", true);
        let state = state(&root, Script::Complete);
        let turn = super::super::state::ArchitectTurn {
            turn_id: "turno-1".into(),
            user_text: "planeje".into(),
            started_at: 1,
        };
        let scope = state.begin_architect_turn(PROJECT, turn, false);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let v = json_of(&body);
        assert_eq!(v["reason"], "architect_turn_active", "{body}");
        assert!(v["error"].as_str().is_some_and(|e| !e.is_empty()));
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(state.runs().active(PROJECT).is_none());
        // Fim do turno: a mesma aprovação passa.
        drop(scope);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn corrupt_task_is_409_invalid_task() {
        let root = setup("corrupt", false);
        write_sdd(&root, SETTINGS, "AWAITING_APPROVAL", &[]);
        let tasks = root.join(".sdd/specs").join(SPEC).join("tasks");
        fs::write(tasks.join("T01.md"), "---\nid: [\n---\nlixo\n").expect("corrompida");
        let state = state(&root, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_INVALID_TASK);
    }

    #[tokio::test]
    async fn task_not_todo_or_incoherent_is_409_invalid_task() {
        // Status diferente de TODO.
        let root = setup("not-todo", false);
        write_sdd(&root, SETTINGS, "AWAITING_APPROVAL", &[]);
        let tasks = root.join(".sdd/specs").join(SPEC).join("tasks");
        fs::write(tasks.join("T01.md"), task_md("T01", "", "IN_PROGRESS")).expect("t");
        let state = state(&root, Script::Complete);
        let (_, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(json_of(&body)["reason"], REASON_INVALID_TASK, "{body}");

        // Nome de arquivo diferente do id.
        fs::remove_file(tasks.join("T01.md")).expect("rm");
        fs::write(tasks.join("X.md"), task_md("T01", "", "TODO")).expect("t");
        let (_, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(json_of(&body)["reason"], REASON_INVALID_TASK, "{body}");

        // Ids duplicados.
        fs::remove_file(tasks.join("X.md")).expect("rm");
        fs::write(tasks.join("T01.md"), task_md("T01", "", "TODO")).expect("t");
        fs::write(tasks.join("T01b.md"), task_md("T01", "", "TODO")).expect("t");
        let (_, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(json_of(&body)["reason"], REASON_INVALID_TASK, "{body}");
    }

    #[tokio::test]
    async fn missing_dependency_is_409() {
        expect_conflict("dep-missing", SETTINGS, &[("T01", "T99")], REASON_DEP_MISSING).await;
    }

    #[tokio::test]
    async fn dependency_cycle_and_self_dependency_are_409() {
        expect_conflict(
            "cycle",
            SETTINGS,
            &[("T01", "T02"), ("T02", "T01")],
            REASON_DEP_CYCLE,
        )
        .await;
        expect_conflict("self-dep", SETTINGS, &[("T01", "T01")], REASON_DEP_CYCLE).await;
    }

    #[tokio::test]
    async fn valid_dependency_chain_is_accepted_by_validation() {
        let root = setup("chain", false);
        write_sdd(
            &root,
            SETTINGS,
            "AWAITING_APPROVAL",
            &[("T01", ""), ("T02", "T01"), ("T03", "T01,T02")],
        );
        git_repo(&root);
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn empty_test_command_is_409() {
        expect_conflict(
            "tc-empty",
            "base_branch: main\nmerge_strategy: squash\ntest_command: \"  \"\n",
            &[("T01", "")],
            REASON_TEST_COMMAND,
        )
        .await;
    }

    /// `settings.yaml` com `test_command` vazio e allowlist/denylist preenchidas.
    const SETTINGS_EMPTY_TC: &str = "base_branch: main\nmerge_strategy: squash\ntest_command: \"\"\n\
        allowed_commands:\n  - \"cargo check*\"\ndenied_commands:\n  - \"sudo *\"\nenvironment: docker\nenvironment_hash: 956ec7f74f0b60d1d28f0f623ad234c11881f2438c985c66dc61aec409368168\n";

    /// Declara `test_command` na spec `SPEC` (inserido após `created_at`).
    fn set_spec_command(root: &Path, command: &str) {
        let path = root.join(".sdd/specs").join(SPEC).join("spec.md");
        let text = fs::read_to_string(&path).expect("spec.md");
        let text = text.replace(
            "created_at: \"2026-10-13\"\n",
            &format!("created_at: \"2026-10-13\"\ntest_command: \"{command}\"\n"),
        );
        fs::write(&path, text).expect("spec.md");
    }

    fn settings_file(root: &Path) -> crate::sdd::Settings {
        crate::sdd::read_settings(&root.join(".sdd/settings.yaml")).expect("settings")
    }

    #[test]
    fn inspect_accepts_spec_command_without_writing_and_adoption_writes() {
        let root = setup("tc-adopt", false);
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
        set_spec_command(&root, "node --test");
        match inspect(&root, SPEC, Mode::Approve) {
            Ok(inspected) => assert_eq!(inspected.settings.test_command, "", "inspect não grava"),
            Err(_) => panic!("spec com comando válido deve ser aceita"),
        }
        assert_eq!(settings_file(&root).test_command, "", "nenhuma escrita antes do git");
        adopt_spec_test_command(&root, "node --test").expect("adoção");
        let s = settings_file(&root);
        assert_eq!(s.test_command, "node --test");
        assert_eq!(s.base_branch, "main", "demais campos preservados");
        assert_eq!(s.allowed_commands, vec!["cargo check*".to_string()]);
        assert_eq!(s.denied_commands, vec!["sudo *".to_string()]);
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval, "spec não muda aqui");
    }

    #[test]
    fn approving_environment_commits_only_env_files_and_worktrees_carry_them() {
        let root = setup_plain("env-commit", true);
        fs::create_dir_all(root.join(".nants")).expect("mkdir");
        fs::write(root.join(".nants/compose.yaml"), "services:\n  workspace:\n    image: node:20\n").expect("w");
        fs::write(root.join(".nants/Dockerfile"), "FROM node:20\n").expect("w");
        fs::write(root.join("outro.txt"), "sujo").expect("w");
        // Draft não suja a base na aprovação; fora dela suja.
        assert!(check_git_for(&root, false, false).is_err());
        let hash = env_hash_of(&root);
        let log = |r: &Path| {
            let out = std::process::Command::new("git")
                .args(["log", "--format=%s"])
                .current_dir(r)
                .output()
                .expect("git log");
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let before = log(&root).lines().count();
        adopt_environment(&root, &hash, "s1", Lang::PtBr).expect("adoção");
        let after = log(&root);
        assert_eq!(after.lines().count(), before + 1);
        assert!(after.starts_with("chore(s1): ambiente aprovado (Nants)"), "{after}");
        // Só os arquivos de ambiente entraram no commit.
        let shown = std::process::Command::new("git")
            .args(["show", "--name-only", "--format=", "HEAD"])
            .current_dir(&root)
            .output()
            .expect("show");
        let names = String::from_utf8_lossy(&shown.stdout).to_string();
        assert!(names.contains(".nants/compose.yaml") && names.contains(".nants/Dockerfile"), "{names}");
        assert!(!names.contains("outro.txt"), "{names}");
        // Worktree nova a partir da base traz o ambiente.
        let wt = root.parent().expect("pai").join("wt-env");
        crate::harness::git::Git::new(root.clone())
            .worktree_add("env-branch", &wt, "HEAD")
            .expect("worktree");
        assert!(wt.join(".nants/compose.yaml").exists());
        // Reaprovar com os mesmos arquivos não cria commit.
        adopt_environment(&root, &hash, "s1", Lang::PtBr).expect("reaprovação");
        assert_eq!(log(&root).lines().count(), before + 1);
    }

    #[test]
    fn check_git_for_allow_env_tolerates_only_env_files() {
        let root = setup_plain("env-dirty", true);
        fs::create_dir_all(root.join(".nants")).expect("mkdir");
        fs::write(root.join(".nants/compose.yaml"), "services:\n  workspace:\n    image: x\n").expect("w");
        fs::write(root.join(".nants/Dockerfile"), "FROM x\n").expect("w");
        assert!(check_git_for(&root, true, false).is_ok(), "rascunho do ambiente na aprovação");
        assert!(check_git_for(&root, false, false).is_err(), "fora da aprovação suja a base");
        fs::write(root.join("outro.txt"), "sujo").expect("w");
        assert!(check_git_for(&root, true, false).is_err(), "outro arquivo sujo bloqueia");
    }

    const ARCH_OK: &str = "# P\n\n## Visão geral\nx\n\n## Stack\nx\n\n## Estrutura\nx\n\n## Comandos\nx\n\n## Padrões e convenções\nx\n\n## Decisões\nx\n\n## Restrições\nx\n";

    #[test]
    fn architecture_draft_is_committed_on_approval_and_invalid_is_409() {
        let root = setup("arch-adopt", true);
        // inválido: 409 já no preflight.
        fs::write(root.join("ARCHITECTURE.md"), "sem titulo").expect("w");
        assert!(check_git_for(&root, false, true).is_err());
        assert!(adopt_architecture(&root, "s1", &SecretSet::new(Vec::new()), Lang::PtBr).is_err());
        // válido: tolerado só na aprovação e commitado.
        fs::write(root.join("ARCHITECTURE.md"), ARCH_OK).expect("w");
        assert!(check_git_for(&root, false, false).is_err(), "fora da aprovação suja a base");
        assert!(check_git_for(&root, false, true).is_ok());
        adopt_architecture(&root, "s1", &SecretSet::new(Vec::new()), Lang::PtBr).expect("commit");
        let git = Git::new(root.clone());
        assert!(git.is_tracked("ARCHITECTURE.md"));
        assert!(git.log_subjects("HEAD", 10).unwrap_or_default().iter().any(|s| s == "docs(s1): arquitetura inicial"));
        // nova versão: atualiza.
        fs::write(root.join("ARCHITECTURE.md"), format!("{ARCH_OK}\nmais\n")).expect("w");
        adopt_architecture(&root, "s2", &SecretSet::new(Vec::new()), Lang::PtBr).expect("update");
        assert!(git.log_subjects("HEAD", 10).unwrap_or_default().iter().any(|s| s == "docs(s2): atualiza arquitetura"));
        // sem mudança: no-op.
        adopt_architecture(&root, "s3", &SecretSet::new(Vec::new()), Lang::PtBr).expect("noop");
    }

    const SKILL_OK: &str = "---\nrole: developer\nstack: [clojure]\nupdated_at: 2026-10-16\nsource_spec: s1\n---\n## Papel\n- Clojure com deps.edn.\n";

    #[test]
    fn skill_profiles_are_committed_on_approval_and_invalid_is_409() {
        let root = setup("skills-adopt", true);
        fs::create_dir_all(root.join(".sdd/skills")).expect("mkdir");
        // inválido (URL): 409 no preflight e na adoção.
        fs::write(root.join(".sdd/skills/developer.md"), SKILL_OK.replace("deps.edn.", "https://x.dev")).expect("w");
        assert!(check_git_for(&root, false, true).is_err());
        assert!(adopt_skills(&root, "s1", &SecretSet::new(Vec::new()), Lang::PtBr).is_err());
        // válido: tolerado só na aprovação; commitado com segredo redigido.
        let secret = "sk-segredo-12345678";
        fs::write(root.join(".sdd/skills/developer.md"), SKILL_OK.replace("deps.edn.", &format!("chave {secret}"))).expect("w");
        assert!(check_git_for(&root, false, false).is_ok(), "perfil sujo não bloqueia (como .sdd/specs)");
        assert!(check_git_for(&root, false, true).is_ok());
        adopt_skills(&root, "s1", &SecretSet::new(vec![secret.to_string()]), Lang::PtBr).expect("commit");
        let git = Git::new(root.clone());
        assert!(git.is_tracked(".sdd/skills/developer.md"));
        assert!(git.log_subjects("HEAD", 10).unwrap_or_default().iter().any(|s| s == "docs(s1): perfis de especialista"));
        let shown = Command::new("git").args(["show", "HEAD:.sdd/skills/developer.md"]).current_dir(&root).output().expect("show");
        assert!(!String::from_utf8_lossy(&shown.stdout).contains(secret));
        assert!(check_git_for(&root, false, false).is_ok(), "base limpa");
        // sem mudança: no-op.
        adopt_skills(&root, "s2", &SecretSet::new(Vec::new()), Lang::PtBr).expect("noop");
        // perfil versionado apagado no disco: sem 409.
        fs::remove_file(root.join(".sdd/skills/developer.md")).expect("rm");
        assert!(check_git_for(&root, false, true).is_ok());
        adopt_skills(&root, "s3", &SecretSet::new(Vec::new()), Lang::PtBr).expect("remoção ignorada");
    }

    #[test]
    fn architecture_secrets_are_scrubbed_before_commit() {
        let root = setup("arch-scrub", true);
        let secret = "sk-segredo-12345678";
        let doc = ARCH_OK.replace("## Stack\nx", &format!("## Stack\nchave {secret}"));
        fs::write(root.join("ARCHITECTURE.md"), doc).expect("w");
        adopt_architecture(&root, "s1", &SecretSet::new(vec![secret.to_string()]), Lang::PtBr).expect("commit");
        let shown = Command::new("git").args(["show", "HEAD:ARCHITECTURE.md"]).current_dir(&root).output().expect("show");
        let text = String::from_utf8_lossy(&shown.stdout);
        assert!(text.contains("## Stack") && !text.contains(secret), "{text}");
        assert!(!fs::read_to_string(root.join("ARCHITECTURE.md")).expect("r").contains(secret));
    }

    fn env_hash_of(root: &Path) -> String {
        crate::sdd::environment::validate_and_hash(root).expect("hash")
    }

    #[test]
    fn discarded_hitl_approval_keeps_base_runnable_and_unknown_hash_is_refused() {
        let root = setup_plain("env-set", true);
        let compose = "services:\n  workspace:\n    image: node:20\n";
        fs::create_dir_all(root.join(".nants")).expect("mkdir");
        fs::write(root.join(".nants/compose.yaml"), compose).expect("w");
        let settings = settings_file(&root);
        let h1 = plan_environment(&root, &settings, true, false).expect("plano").expect("hash");
        adopt_environment(&root, &h1, "s1", Lang::PtBr).expect("adoção");
        // Aprovação HITL de um ambiente novo (tarefa depois descartada): o hash entra no conjunto.
        persist_environment_hash(&root, "hitl-hash").expect("persist");
        let after = settings_file(&root);
        assert_eq!(after.approved_hashes(), vec![h1.clone(), "hitl-hash".to_string()]);
        // A base mantém os arquivos antigos e segue executável.
        assert_eq!(plan_environment(&root, &after, false, false).expect("base antiga"), None);
        // Arquivo fora do conjunto continua recusado.
        fs::write(root.join(".nants/compose.yaml"), format!("{compose}  db:\n    image: postgres:16\n")).expect("w");
        assert!(plan_environment(&root, &after, false, false).is_err());
    }

    #[test]
    fn legacy_settings_with_only_environment_hash_still_work() {
        let text = "base_branch: main\nmerge_strategy: squash\ntest_command: cargo test\nenvironment: docker\nenvironment_hash: abc\n";
        let s = Settings::parse(text).expect("parse");
        assert_eq!(s.approved_hashes(), vec!["abc".to_string()]);
        let mut s = s;
        s.approve_environment_hash("def");
        assert_eq!(s.approved_hashes(), vec!["abc".to_string(), "def".to_string()]);
        for i in 0..20 {
            s.approve_environment_hash(&format!("h{i}"));
        }
        assert_eq!(s.approved_environment_hashes.len(), 16);
        assert_eq!(s.environment_hash.as_deref(), Some("h19"));
        s.approve_environment_hash("h19");
        assert_eq!(s.approved_environment_hashes.len(), 16);
    }

    #[test]
    fn plan_environment_registers_hash_and_blocks_changes() {
        use crate::sdd::Environment;
        let root = setup_plain("env-plan", true);
        let compose = "services:\n  workspace:\n    image: node:20\n";
        fs::create_dir_all(root.join(".nants")).expect("mkdir");
        fs::write(root.join(".nants/compose.yaml"), compose).expect("w");
        let settings = settings_file(&root);
        // Host sem declaração: nada a fazer.
        assert_eq!(plan_environment(&root, &settings, false, false).expect("host"), None);
        // Spec docker: aprova e registra o hash.
        let hash = plan_environment(&root, &settings, true, false).expect("plano").expect("hash");
        let adopted = adopt_environment(&root, &hash, "s1", Lang::PtBr).expect("adoção");
        assert_eq!(adopted.environment, Environment::Docker);
        assert_eq!(settings_file(&root).environment_hash.as_deref(), Some(hash.as_str()));
        // Execução sem nova aprovação: arquivos iguais passam; alterados são recusados.
        let settings = settings_file(&root);
        assert_eq!(plan_environment(&root, &settings, false, false).expect("igual"), None);
        fs::write(root.join(".nants/compose.yaml"), format!("{compose}  db:\n    image: postgres:16\n")).expect("w");
        assert!(plan_environment(&root, &settings, false, false).is_err());
        // Nova aprovação (spec docker) re-registra.
        assert!(plan_environment(&root, &settings, true, false).expect("reaprova").is_some());
        // Compose inválido não é aprovado.
        fs::write(root.join(".nants/compose.yaml"), "services:\n  workspace:\n    privileged: true\n").expect("w");
        assert!(plan_environment(&root, &settings, true, false).is_err());
    }

    #[test]
    fn adopt_spec_checks_writes_only_when_settings_has_none() {
        let root = setup("checks-adopt", false);
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
        let checks = vec![crate::sdd::Check { name: "build".into(), command: "npm run build".into(), blocking: true }];
        let s = adopt_spec_checks(&root, &checks).expect("adoção");
        assert_eq!(s.checks, checks);
        assert_eq!(settings_file(&root).checks, checks);
        assert_eq!(settings_file(&root).allowed_commands, vec!["cargo check*".to_string()]);
        let other = vec![crate::sdd::Check { name: "lint".into(), command: "x".into(), blocking: false }];
        let s = adopt_spec_checks(&root, &other).expect("humano vence");
        assert_eq!(s.checks, checks, "checks já definidos não são sobrescritos");
        assert_eq!(settings_file(&root).checks, checks);
    }

    #[test]
    fn adopt_spec_formatters_replaces_the_settings_list() {
        let root = setup("fmt-adopt", false);
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
        let fmt = |name: &str, cmd: &str| crate::sdd::Formatter {
            name: name.into(),
            command: cmd.into(),
            workdir: ".".into(),
            paths: vec!["**".into()],
        };
        let first = vec![fmt("rust", "cargo fmt")];
        let s = adopt_spec_formatters(&root, &first).expect("adoção");
        assert_eq!(s.formatters, first);
        assert_eq!(settings_file(&root).formatters, first);
        assert_eq!(settings_file(&root).allowed_commands, vec!["cargo check*".to_string()]);
        // A spec posterior substitui a lista (não só quando vazia).
        let second = vec![fmt("web", "npx prettier --write .")];
        let s = adopt_spec_formatters(&root, &second).expect("substitui");
        assert_eq!(s.formatters, second);
        assert_eq!(settings_file(&root).formatters, second);
        // Lista vazia ou toda inválida não apaga o que existe.
        let s = adopt_spec_formatters(&root, &[fmt("Nome Ruim", "x")]).expect("inválida");
        assert_eq!(s.formatters, second);
        assert_eq!(settings_file(&root).formatters, second);
    }

    /// Spec 124 T05: os níveis de teste e a instalação da spec entram no settings; o campo ausente
    /// não mexe, e a spec sem nenhum deles deixa tudo como antes.
    #[test]
    fn adopt_spec_test_levels_sets_only_the_fields_present() {
        let root = setup("levels-adopt", false);
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
        assert!(SpecTestLevels::new(None, None, None).is_none(), "sem os campos, nada muda");
        let levels = SpecTestLevels::new(
            Some("make full".into()),
            Some("make deps".into()),
            Some(vec!["deps.lock".into(), "../fora".into()]),
        )
        .expect("níveis");
        let s = adopt_spec_test_levels(&root, &levels).expect("adoção");
        assert_eq!(s.full_test_command.as_deref(), Some("make full"));
        assert_eq!(s.install_command.as_deref(), Some("make deps"));
        assert_eq!(s.lockfiles, vec!["deps.lock".to_string()]);
        assert_eq!(settings_file(&root), s);
        let only_full = SpecTestLevels::new(Some("make all".into()), None, None).expect("um campo");
        let s = adopt_spec_test_levels(&root, &only_full).expect("substitui");
        assert_eq!(s.full_test_command.as_deref(), Some("make all"));
        assert_eq!(s.install_command.as_deref(), Some("make deps"), "ausente não mexe");
        assert!(!only_full.changes(&s));
    }

    #[test]
    fn adopt_spec_failure_artifacts_replaces_the_settings_list() {
        let root = setup("fa-adopt", false);
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
        let first = vec!["test-results/**/error-context.md".to_string()];
        let s = adopt_spec_failure_artifacts(&root, &first).expect("adoção");
        assert_eq!(s.failure_artifacts, first);
        assert_eq!(settings_file(&root).failure_artifacts, first);
        let second = vec!["./reports//*.log".to_string(), "../fora".to_string()];
        let s = adopt_spec_failure_artifacts(&root, &second).expect("substitui");
        assert_eq!(s.failure_artifacts, vec!["reports/*.log".to_string()]);
        assert_eq!(settings_file(&root).failure_artifacts, vec!["reports/*.log".to_string()]);
        let s = adopt_spec_failure_artifacts(&root, &["/abs".to_string()]).expect("inválida");
        assert_eq!(s.failure_artifacts, vec!["reports/*.log".to_string()]);
    }

    #[test]
    fn adopt_spec_screenshots_replaces_the_settings_list_and_keeps_failure_artifacts() {
        let root = setup("shots-adopt", false);
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
        adopt_spec_failure_artifacts(&root, &["test-results/**/error-context.md".to_string()]).expect("fa");
        let first = vec!["test-results/screens/**/*.png".to_string()];
        let s = adopt_spec_screenshots(&root, &first).expect("adoção");
        assert_eq!(s.screenshots, first);
        assert_eq!(settings_file(&root).screenshots, first);
        assert_eq!(settings_file(&root).failure_artifacts, vec!["test-results/**/error-context.md".to_string()]);
        let second = vec!["./shots//*.jpg".to_string(), "../fora/*.png".to_string()];
        let s = adopt_spec_screenshots(&root, &second).expect("substitui");
        assert_eq!(s.screenshots, vec!["shots/*.jpg".to_string()]);
        assert_eq!(settings_file(&root).screenshots, vec!["shots/*.jpg".to_string()]);
        let s = adopt_spec_screenshots(&root, &["/abs/*.png".to_string()]).expect("inválida");
        assert_eq!(s.screenshots, vec!["shots/*.jpg".to_string()]);
    }

    #[test]
    fn inspect_keeps_human_test_command_over_spec() {
        let root = setup("tc-human-wins", false);
        let human = SETTINGS_EMPTY_TC.replace("test_command: \"\"", "test_command: \"make\"");
        fs::write(root.join(".sdd/settings.yaml"), human).expect("settings");
        set_spec_command(&root, "node --test");
        let settings = match inspect(&root, SPEC, Mode::Approve) {
            Ok(inspected) => inspected.settings,
            Err(_) => panic!("settings do humano basta"),
        };
        assert_eq!(settings.test_command, "make");
        assert_eq!(settings_file(&root).test_command, "make");
    }

    #[tokio::test]
    async fn blank_spec_command_and_empty_settings_is_409_and_untouched() {
        let root = setup("tc-both-empty", true);
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
        set_spec_command(&root, "   ");
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_TEST_COMMAND);
        assert_eq!(settings_file(&root).test_command, "");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(!root.join(".sdd/worktrees").exists(), "nenhuma worktree criada");
        assert!(
            drain(&mut rx).iter().all(|ev| ev.event_type() != "project_status"),
            "nenhum evento de execução"
        );
        assert!(state.runs().active(PROJECT).is_none());
    }

    #[test]
    fn adopt_is_serialized_with_always_and_put_without_losses() {
        for i in 0..20 {
            // Sem PUT: a spec preenche o comando vazio e o `always` preserva a sua entrada.
            let root = setup(&format!("tc-race-a-{i}"), false);
            fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
            let (r1, r2) = (root.clone(), root.clone());
            let adopt = std::thread::spawn(move || adopt_spec_test_command(&r1, "node --test"));
            let always = std::thread::spawn(move || persist_always(&r2, "echo alfa"));
            assert!(adopt.join().expect("adopt").is_ok(), "iteração {i}");
            assert!(always.join().expect("always"), "iteração {i}");
            let s = settings_file(&root);
            assert_eq!(s.test_command, "node --test", "iteração {i}");
            assert!(s.allowed_commands.contains(&"echo alfa".to_string()), "iteração {i}");

            // Com PUT concorrente: o comando do humano sempre vence sobre o da spec.
            let root = setup(&format!("tc-race-p-{i}"), false);
            fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
            let (r1, r2, r3) = (root.clone(), root.clone(), root.clone());
            let adopt = std::thread::spawn(move || adopt_spec_test_command(&r1, "node --test"));
            let always = std::thread::spawn(move || persist_always(&r2, "echo alfa"));
            let put = std::thread::spawn(move || {
                crate::server::projects::write_test_command(&r3, "cargo test", &SecretSet::new(Vec::new()))
            });
            assert!(adopt.join().expect("adopt").is_ok(), "iteração {i}");
            assert!(always.join().expect("always"), "iteração {i}");
            assert!(put.join().expect("put").is_ok(), "iteração {i}");
            let s = settings_file(&root);
            assert_eq!(s.test_command, "cargo test", "iteração {i}");
            assert!(s.allowed_commands.contains(&"echo alfa".to_string()), "iteração {i}");
        }
    }

    /// Commit de base com identidade fixa (só para preparar o repositório dos testes).
    fn commit_base(root: &Path) {
        git(
            root,
            &[
                "-c",
                "user.name=Teste",
                "-c",
                "user.email=teste@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
    }

    /// Saída de `git` (stdout), sem exigir sucesso.
    fn git_out(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Repositório Git real criado com `git init`; com `with_commit`, traz um commit com README.
    fn real_repo(tag: &str, with_commit: bool) -> PathBuf {
        let root = temp_dir(tag).join("repo");
        fs::create_dir_all(&root).expect("repo");
        git(&root, &["init", "-q"]);
        if with_commit {
            fs::write(root.join("README.md"), "projeto\n").expect("readme");
            git(&root, &["add", "README.md"]);
            commit_base(&root);
        }
        root
    }

    /// Projeto criado pelo fluxo de produção (`POST /api/projects`) sobre `root`.
    async fn created_project(root: &Path) -> (AppState, String) {
        let mut cfg = config_for(root);
        cfg.projects.clear();
        created_project_with(root, cfg).await
    }

    /// Como [`created_project`], com a configuração dada.
    async fn created_project_with(root: &Path, cfg: Config) -> (AppState, String) {
        let state = state_with(root, cfg, Script::Complete);
        let body = json!({"name": "Projeto E2E", "path": root.to_str().expect("utf8")}).to_string();
        let (status, text) = call(
            &state,
            request(Method::POST, "/api/projects", &body, Some("application/json")),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        (state, json_of(&text)["id"].as_str().expect("id").to_string())
    }

    /// Tarefa T01 como o Arquiteto grava: TODO, sem campos de execução.
    fn plan_task() -> String {
        format!(
            "---\nid: \"T01\"\nspec_id: \"{SPEC}\"\ntitle: \"Tarefa T01\"\nstatus: \"TODO\"\n\
             depends_on: []\nbranch: \"\"\nattempts: 0\nmax_attempts: 3\n\
             last_review_verdict: \"NONE\"\nallowed_scope: [\"src/lib.rs\"]\n---\n\n\
             ## Objetivo\nFazer T01.\n\n## Critérios de Aceitação\n- compila\n"
        )
    }

    /// Ferramentas do Arquiteto (produção): grava a spec `SPEC` com `command` e a tarefa T01.
    async fn architect_writes_plan(root: &Path, command: &str) {
        fs::create_dir_all(root.join(".sdd/specs").join(SPEC).join("tasks")).expect("dirs");
        let host = ToolHost::new(
            Role::Architect,
            root.to_path_buf(),
            Vec::new(),
            Policy::new(Vec::new(), Vec::new()),
            Arc::new(NoApprover),
        );
        let spec = format!(
            "---\nid: \"{SPEC}\"\ntitle: \"Plano E2E\"\nstatus: \"AWAITING_APPROVAL\"\n\
             base_branch: \"main\"\nenvironment: \"docker\"\ntest_command: \"{command}\"\ncreated_at: \"2026-10-14\"\n---\n\n\
             ## Contexto\nE2E.\n"
        );
        fs::create_dir_all(root.join(".nants")).expect("nants");
        fs::write(root.join(".nants/compose.yaml"), DOCKER_COMPOSE).expect("compose");
        host.call(
            ToolName::WriteSddArtifacts,
            json!({"path": format!(".sdd/specs/{SPEC}/spec.md"), "content": spec}),
        )
        .await
        .expect("spec");
        host.call(
            ToolName::WriteSddArtifacts,
            json!({"path": format!(".sdd/specs/{SPEC}/tasks/T01.md"), "content": plan_task()}),
        )
        .await
        .expect("tarefa");
    }

    /// O cadastro agora cria o commit inicial; os testes de repositório sem commits o desfazem.
    fn make_unborn(root: &Path) {
        git(root, &["update-ref", "-d", "HEAD"]);
    }

    /// Aguarda a execução do projeto `id` terminar (até 2 s).
    async fn wait_project_idle(state: &AppState, id: &str) {
        for _ in 0..200 {
            if state.runs().active(id).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("execução não terminou");
    }

    #[tokio::test]
    async fn e2e_unborn_repo_gets_one_empty_commit_and_approves() {
        let root = real_repo("e2e-unborn", false);
        let (state, id) = created_project(&root).await;
        make_unborn(&root);
        architect_writes_plan(&root, "node --test").await;
        let (status, body) = call(&state, approve_req(&id, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        // Commit inicial vazio + commit do ambiente docker aprovado (só `.nants/`).
        assert_eq!(git_out(&root, &["rev-list", "--count", "HEAD"]).trim(), "2");
        assert_eq!(git_out(&root, &["log", "-1", "--format=%s", "HEAD~1"]).trim(), "chore: commit inicial (Nants)");
        assert!(git_out(&root, &["show", "--stat", "--format=", "HEAD~1"]).trim().is_empty());
        assert_eq!(git_out(&root, &["ls-files"]).trim(), ".nants/compose.yaml", "só o ambiente versionado");
        let status_text = git_out(&root, &["status", "--porcelain", "--untracked-files=all"]);
        assert!(!status_text.contains(".sdd/settings.yaml"), "{status_text}");
        wait_project_idle(&state, &id).await;
    }

    #[tokio::test]
    async fn e2e_unborn_repo_with_staged_change_is_base_dirty_without_commit() {
        let root = real_repo("e2e-staged", false);
        let (state, id) = created_project(&root).await;
        make_unborn(&root);
        fs::write(root.join("novo.txt"), "x\n").expect("arquivo");
        git(&root, &["add", "novo.txt"]);
        architect_writes_plan(&root, "node --test").await;
        let (status, body) = call(&state, approve_req(&id, SPEC, "")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_BASE_DIRTY);
        assert!(Git::new(&root).is_unborn(), "nenhum commit criado");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
    }

    #[test]
    fn initial_commit_is_idempotent_and_adds_no_files() {
        let root = real_repo("ci-idem", false);
        let git_w = Git::new(&root);
        assert!(initial_commit(&git_w, Lang::PtBr).is_ok(), "primeiro commit");
        assert!(initial_commit(&git_w, Lang::PtBr).is_ok(), "segunda chamada não cria outro");
        assert_eq!(git_out(&root, &["rev-list", "--count", "HEAD"]).trim(), "1");
        assert!(git_out(&root, &["ls-files"]).trim().is_empty());
    }

    #[test]
    fn initial_commit_uses_fallback_identity_and_keeps_repo_config() {
        let root = real_repo("ci-identity", false);
        let home = temp_dir("ci-identity-home");
        fs::create_dir_all(&home).expect("home vazio");
        let home_text = home.to_str().expect("utf8").to_string();
        let git_w = Git::new(&root)
            .with_env("HOME", &home_text)
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
            .with_env("GIT_CONFIG_NOSYSTEM", "1");
        assert_eq!(git_w.missing_identity(), vec!["user.name", "user.email"]);
        assert!(initial_commit(&git_w, Lang::PtBr).is_ok(), "commit com identidade de fallback");
        assert_eq!(git_out(&root, &["log", "-1", "--format=%an <%ae>"]).trim(), "Nants <nants@localhost>");
        let local = git_out(&root, &["config", "--local", "--list"]);
        assert!(!local.contains("user.name") && !local.contains("user.email"), "{local}");
    }

    #[tokio::test]
    async fn e2e_unborn_repo_with_untracked_file_is_base_dirty_without_commit() {
        let root = real_repo("e2e-untracked", false);
        let (state, id) = created_project(&root).await;
        make_unborn(&root);
        fs::write(root.join("novo.txt"), "x\n").expect("arquivo");
        architect_writes_plan(&root, "node --test").await;
        let (status, body) = call(&state, approve_req(&id, SPEC, "")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_BASE_DIRTY);
        assert!(Git::new(&root).is_unborn(), "nenhum commit criado");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
    }

    #[tokio::test]
    async fn approval_without_agent_is_412_and_nothing_changes() {
        let root = real_repo("e2e-no-agent", false);
        let mut cfg = config_for(&root);
        cfg.projects.clear();
        cfg.connections.clear();
        let (state, id) = created_project_with(&root, cfg).await;
        make_unborn(&root);
        architect_writes_plan(&root, "node --test").await;
        let settings_path = root.join(".sdd/settings.yaml");
        let before = fs::read_to_string(&settings_path).expect("settings");
        let (status, body) = call(&state, approve_req(&id, SPEC, "")).await;
        // Spec 093 T02: a prontidão `execute` vem antes do 422 de agente não configurado.
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
        assert_eq!(json_of(&body), json!({"error": "prereq_missing", "missing": ["agent_developer", "agent_reviewer"], "code": "prereq_missing", "params": {"count": 2}}));
        assert_eq!(fs::read_to_string(&settings_path).expect("settings"), before);
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(Git::new(&root).is_unborn(), "sem commit");
    }

    #[tokio::test]
    async fn spec_changed_before_effects_leaves_everything_unchanged() {
        let root = real_repo("e2e-spec-changed", false);
        let (state, id) = created_project(&root).await;
        make_unborn(&root);
        architect_writes_plan(&root, "node --test").await;
        let spec_path = root.join(".sdd/specs").join(SPEC).join("spec.md");
        let settings_before = fs::read_to_string(root.join(".sdd/settings.yaml")).expect("settings");
        let exclude_before = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
        let altered = fs::read_to_string(&spec_path)
            .expect("spec")
            .replace("## Contexto\nE2E.", "## Contexto\nMudou.");
        let (hook_path, hook_text) = (spec_path.clone(), altered.clone());
        set_hook(&root, "git", move || fs::write(&hook_path, hook_text).expect("alterar"));
        let (status, body) = call(&state, approve_req(&id, SPEC, "")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["code"], SPEC_CHANGED);
        assert_eq!(fs::read_to_string(&spec_path).expect("spec"), altered);
        assert_eq!(fs::read_to_string(root.join(".sdd/settings.yaml")).expect("settings"), settings_before);
        assert_eq!(fs::read_to_string(root.join(".git/info/exclude")).expect("exclude"), exclude_before);
        assert!(Git::new(&root).is_unborn(), "sem commit");
        assert!(state.runs().active(&id).is_none());
    }

    #[tokio::test]
    async fn git_timeout_is_504_without_effects() {
        let root = real_repo("e2e-timeout", false);
        let (state, id) = created_project(&root).await;
        make_unborn(&root);
        architect_writes_plan(&root, "node --test").await;
        let settings_before = fs::read_to_string(root.join(".sdd/settings.yaml")).expect("settings");
        GIT_TIMEOUT_OVERRIDES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((root.clone(), Duration::from_millis(50)));
        set_hook(&root, "git", || std::thread::sleep(Duration::from_millis(400)));
        let (status, body) = call(&state, approve_req(&id, SPEC, "")).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
        assert_eq!(json_of(&body)["code"], GIT_TIMEOUT_MSG);
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert_eq!(fs::read_to_string(root.join(".sdd/settings.yaml")).expect("settings"), settings_before);
        assert!(Git::new(&root).is_unborn(), "sem commit");
    }

    #[test]
    fn initial_commit_fills_only_the_missing_identity_key() {
        let root = real_repo("ci-partial", false);
        git(&root, &["config", "user.name", "Dev Real"]);
        let home = temp_dir("ci-partial-home");
        fs::create_dir_all(&home).expect("home vazio");
        let home_text = home.to_str().expect("utf8").to_string();
        let git_w = Git::new(&root)
            .with_env("HOME", &home_text)
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
            .with_env("GIT_CONFIG_NOSYSTEM", "1");
        assert_eq!(git_w.missing_identity(), vec!["user.email"]);
        assert!(initial_commit(&git_w, Lang::PtBr).is_ok(), "commit inicial");
        assert_eq!(
            git_out(&root, &["log", "-1", "--format=%an <%ae>"]).trim(),
            "Dev Real <nants@localhost>"
        );
    }

    /// Spec 132 T12 (A1): instalação nova, sem identidade Git no repositório nem no HOME (isolado).
    /// O cadastro, o commit inicial, o do ambiente, o do `ARCHITECTURE.md` e o dos perfis usam a
    /// identidade de reserva, e o plano é aprovado (antes: 409 `architecture_commit_failed`).
    #[tokio::test]
    async fn e2e_approval_without_git_identity_commits_with_the_fallback() {
        let root = real_repo("e2e-sem-identidade", false);
        let home = root.parent().expect("base").join("home");
        fs::create_dir_all(&home).expect("home");
        let home_text = home.to_str().expect("utf8").to_string();
        crate::harness::git::isolate_env_for_tests(
            &root,
            &[("HOME", &home_text), ("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")],
        );
        assert_eq!(Git::new(&root).missing_identity(), vec!["user.name", "user.email"]);
        let (state, id) = created_project(&root).await;
        make_unborn(&root);
        architect_writes_plan(&root, "node --test").await;
        fs::write(root.join("ARCHITECTURE.md"), ARCH_OK).expect("arquitetura");
        fs::create_dir_all(root.join(".sdd/skills")).expect("skills");
        fs::write(root.join(".sdd/skills/developer.md"), SKILL_OK).expect("perfil");
        let (status, body) = call(&state, approve_req(&id, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let authors = git_out(&root, &["log", "--format=%s|%an <%ae>"]);
        let lines: Vec<&str> = authors.lines().collect();
        assert_eq!(lines.len(), 4, "{authors}");
        for prefix in ["chore: commit inicial", &format!("chore({SPEC}): "), &format!("docs({SPEC}): ")] {
            assert!(lines.iter().any(|l| l.starts_with(prefix)), "{prefix}: {authors}");
        }
        assert!(lines.iter().all(|l| l.ends_with("|Nants <nants@localhost>")), "{authors}");
        let local = git_out(&root, &["config", "--local", "--list"]);
        assert!(!local.contains("user."), "{local}");
        wait_project_idle(&state, &id).await;
    }

    /// Falha no commit do `ARCHITECTURE.md` (aqui, um hook `pre-commit` que recusa) tem o código
    /// próprio, sem o `reason` `environment_invalid` do ambiente docker.
    #[test]
    fn architecture_commit_failure_has_its_own_code_without_the_environment_reason() {
        let root = setup("arch-commit-falha", true);
        fs::write(root.join("ARCHITECTURE.md"), ARCH_OK).expect("w");
        let hooks = root.join(".git/hooks");
        fs::create_dir_all(&hooks).expect("hooks");
        let hook = hooks.join("pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").expect("hook");
        let mut perms = fs::metadata(&hook).expect("meta").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&hook, perms).expect("chmod");
        let err = adopt_architecture(&root, "s1", &SecretSet::new(Vec::new()), Lang::PtBr).expect_err("hook recusa");
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.code(), Some(ARCH_COMMIT_MSG));
        assert_eq!(err.reason(), None, "{}", err.body());
        assert!(err.body().get("reason").is_none(), "{}", err.body());
    }

    #[tokio::test]
    async fn e2e_plain_directory_is_not_git() {
        let root = temp_dir("e2e-nogit").join("dir");
        fs::create_dir_all(&root).expect("dir");
        let (state, id) = created_project(&root).await;
        fs::remove_dir_all(root.join(".git")).expect("remove .git criado no cadastro");
        architect_writes_plan(&root, "node --test").await;
        let (status, body) = call(&state, approve_req(&id, SPEC, "")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_NOT_GIT);
        assert!(!root.join(".git").exists(), "nenhum repositório criado");
    }

    #[tokio::test]
    async fn e2e_legacy_project_without_settings_exclude_migrates_and_approves() {
        let root = real_repo("e2e-legacy", true);
        let (state, id) = created_project(&root).await;
        // Projeto criado antes desta correção: o exclude não tem a linha do settings.
        fs::write(root.join(".git/info/exclude"), ".sdd/worktrees/\n").expect("exclude");
        architect_writes_plan(&root, "node --test").await;
        let (status, body) = call(&state, approve_req(&id, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        let exclude = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
        assert!(exclude.lines().any(|l| l == ".sdd/settings.yaml"), "{exclude}");
        let status_text = git_out(&root, &["status", "--porcelain", "--untracked-files=all"]);
        assert!(!status_text.contains(".sdd/settings.yaml"), "{status_text}");
        wait_project_idle(&state, &id).await;
    }

    #[tokio::test]
    async fn tracked_empty_settings_is_409_before_any_effect() {
        let root = setup("tc-tracked-empty", false);
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
        set_spec_command(&root, "node --test");
        git_repo(&root);
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_TEST_COMMAND);
        assert_eq!(settings_file(&root).test_command, "");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(!root.join(".sdd/worktrees").exists(), "nenhuma worktree");
        assert!(drain(&mut rx).is_empty(), "nenhum evento");
        assert!(state.runs().active(PROJECT).is_none());
        assert_eq!(git_out(&root, &["rev-list", "--count", "HEAD"]).trim(), "1", "sem commit");
    }

    #[tokio::test]
    async fn tracked_settings_with_value_approves() {
        let root = setup("tc-tracked-ok", false);
        let human = SETTINGS_EMPTY_TC.replace("test_command: \"\"", "test_command: \"make\"");
        fs::write(root.join(".sdd/settings.yaml"), human).expect("settings");
        git_repo(&root);
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        collect_until(&mut rx, "integrated").await;
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn put_settings_and_always_keep_git_status_clean_with_exclude() {
        let root = real_repo("put-excl", true);
        let (state, id) = created_project(&root).await;
        let body = json!({"test_command": "cargo test"}).to_string();
        let (status, text) = call(
            &state,
            request(Method::PUT, &format!("/api/projects/{id}/settings"), &body, Some("application/json")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(persist_always(&root, "echo alfa"));
        let s = settings_file(&root);
        assert_eq!(s.test_command, "cargo test");
        assert!(s.allowed_commands.contains(&"echo alfa".to_string()));
        let status_text = git_out(&root, &["status", "--porcelain", "--untracked-files=all"]);
        assert!(!status_text.contains(".sdd/settings.yaml"), "{status_text}");
        assert!(check_git(&root).is_ok(), "base continua limpa para o runner");
    }

    #[tokio::test]
    async fn tracked_settings_modified_does_not_block_approval() {
        // `.sdd/settings.yaml` versionado e alterado (caso teste-web): configuração local, não base suja.
        let root = setup("tc-tracked", true);
        fs::write(
            root.join(".sdd/settings.yaml"),
            format!("{SETTINGS}allowed_commands:\n  - \"cargo check*\"\n"),
        )
        .expect("settings");
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        collect_until(&mut rx, "integrated").await;
        assert!(wait_idle(&state).await);
    }

    #[test]
    fn excluded_settings_keep_base_clean_for_runner_after_adoption() {
        let root = setup_plain("tc-excluded", false);
        git(&root, &["init", "-q"]);
        fs::write(root.join("README.md"), "x\n").expect("readme");
        git(&root, &["add", "README.md"]);
        commit_base(&root);
        ensure_local_excludes_for_test(&root);
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_EMPTY_TC).expect("settings");
        let settings = adopt_spec_test_command(&root, "node --test");
        assert!(settings.is_ok(), "adoção grava o comando");
        let status_text = git_out(&root, &["status", "--porcelain", "--untracked-files=all"]);
        assert!(!status_text.contains(".sdd/settings.yaml"), "{status_text}");
        assert!(check_git(&root).is_ok(), "base continua limpa para o runner");
    }

    fn ensure_local_excludes_for_test(root: &Path) {
        assert!(ensure_local_excludes(root).is_ok(), "exclude");
    }

    struct NoApprover;

    impl Approver for NoApprover {
        fn approve<'a>(
            &'a self,
            _req: &'a ApprovalRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ApprovalDecision> + Send + 'a>> {
            Box::pin(async { ApprovalDecision::Deny })
        }
    }

    #[tokio::test]
    async fn unreadable_settings_is_409() {
        expect_conflict("settings", "não é yaml: [", &[("T01", "")], REASON_SETTINGS).await;
    }

    #[tokio::test]
    async fn not_a_git_repo_is_409() {
        let root = setup("not-git", false);
        let state = state(&root, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_NOT_GIT);
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
    }

    #[tokio::test]
    async fn dirty_base_outside_specs_is_409() {
        let root = setup("dirty-untracked", true);
        fs::write(root.join("README.md"), "alteração do humano").expect("readme");
        let state = state(&root, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_BASE_DIRTY);
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
    }

    #[tokio::test]
    async fn untracked_settings_modified_does_not_block_approval() {
        let root = real_repo("settings-untracked", true);
        write_sdd(&root, SETTINGS, "AWAITING_APPROVAL", &[("T01", "")]);
        fs::write(
            root.join(".sdd/settings.yaml"),
            format!("{SETTINGS}allowed_commands:\n  - \"cargo check*\"\n"),
        )
        .expect("settings");
        let state = state(&root, Script::Complete);
        let mut rx = state.events().subscribe();
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        collect_until(&mut rx, "integrated").await;
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn other_sdd_file_dirty_still_blocks_approval() {
        let root = setup("sdd-other", true);
        fs::write(root.join(".sdd/notas.md"), "nota do humano\n").expect("nota");
        let state = state(&root, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_BASE_DIRTY);
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
    }

    #[test]
    fn check_git_tolerates_only_settings_yaml() {
        let root = setup("check-git-settings", true);
        fs::write(root.join(".sdd/settings.yaml"), format!("{SETTINGS}# nota\n")).expect("settings");
        assert!(check_git(&root).is_ok(), "settings versionado alterado é tolerado");

        let root = real_repo("check-git-untracked", true);
        write_sdd_files(&root, SETTINGS, "AWAITING_APPROVAL", &[("T01", "")]);
        assert!(check_git(&root).is_ok(), "settings não versionado é tolerado");

        fs::write(root.join(".sdd/notas.md"), "x").expect("nota");
        assert!(check_git(&root).is_err(), "outro arquivo em .sdd bloqueia");
        fs::remove_file(root.join(".sdd/notas.md")).expect("remove");
        fs::write(root.join("src.txt"), "x").expect("src");
        assert!(check_git(&root).is_err(), "arquivo fora de .sdd bloqueia");
    }

    #[test]
    fn unborn_repo_with_untracked_settings_is_tolerated() {
        let root = real_repo("check-git-unborn", false);
        write_sdd_files(&root, SETTINGS, "AWAITING_APPROVAL", &[("T01", "")]);
        let pre = check_git(&root).expect("base sem commits com settings não versionado");
        assert!(pre.needs_initial_commit);
    }

    #[tokio::test]
    async fn agent_not_configured_or_unusable_key_is_412_and_spec_untouched() {
        let root = setup("agent-missing", true);
        let before = fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("spec");
        // Sem Developer: a prontidão `execute` recusa com 412 antes do 422 de "não configurado".
        let mut cfg = config_for(&root);
        cfg.agents.developer.connection_id = String::new();
        let state = state_with(&root, cfg, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
        assert_eq!(json_of(&body), json!({"error": "prereq_missing", "missing": ["agent_developer"], "code": "prereq_missing", "params": {"count": 1}}));
        let after = fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("spec");
        assert_eq!(before, after, "spec não pode mudar");
        assert!(state.runs().active(PROJECT).is_none());
        // Developer com conexão e modelo, mas chave não resolvível: a prontidão também recusa com
        // 412 (spec 093 T12), com o item da chave.
        let mut cfg = config_for(&root);
        cfg.connections.push(connection("envdev", "env:NANTS_RUNS_TEST_UNSET_KEY_4712"));
        cfg.agents.developer.connection_id = "envdev".into();
        let state = state_with(&root, cfg, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
        assert_eq!(json_of(&body), json!({"error": "prereq_missing", "missing": ["connection_key:envdev"], "code": "prereq_missing", "params": {"count": 1}}));
        assert!(state.runs().active(PROJECT).is_none());
        assert_eq!(before, fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("spec"));
    }

    #[tokio::test]
    async fn unresolved_env_key_is_412_for_reviewer() {
        let root = setup("env-missing", true);
        let mut cfg = config_for(&root);
        cfg.connections.push(connection("envkey", "env:NANTS_RUNS_TEST_UNSET_KEY_4711"));
        cfg.agents.reviewer.connection_id = "envkey".into();
        let state = state_with(&root, cfg, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
        assert_eq!(json_of(&body), json!({"error": "prereq_missing", "missing": ["connection_key:envkey"], "code": "prereq_missing", "params": {"count": 1}}));
        assert!(!body.contains("NANTS_RUNS_TEST_UNSET_KEY_4711"), "{body}");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
    }

    #[tokio::test]
    async fn invalid_ids_are_422_and_missing_entities_are_404() {
        let root = setup("ids", true);
        let state = state(&root, Script::Complete);
        let (status, _) = call(&state, approve_req(PROJECT, "bad.id", "{}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = call(&state, approve_req("p.x", SPEC, "{}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let long = "a".repeat(MAX_SPEC_ID_LEN + 1);
        let (status, _) = call(&state, approve_req(PROJECT, &long, "{}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = call(&state, approve_req("outro", SPEC, "{}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&state, approve_req(PROJECT, "nao-existe", "{}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{\"x\":1}")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(state.runs().active(PROJECT).is_none());
    }

    #[tokio::test]
    async fn corrupt_spec_is_409_invalid_spec() {
        let root = setup("corrupt-spec", true);
        fs::write(root.join(".sdd/specs/s1/spec.md"), "lixo sem frontmatter").expect("spec");
        let state = state(&root, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["code"], INVALID_SPEC);
    }

    /// Tarefa BLOCKED como o runner grava: motivo em `next_action` e tentativas gastas.
    fn blocked_task_md(id: &str, reason: &str) -> String {
        format!(
            "---\nid: \"{id}\"\nspec_id: \"{SPEC}\"\ntitle: \"Tarefa {id}\"\nstatus: \"BLOCKED\"\n\
             depends_on: []\nbranch: \"sdd/{SPEC}/{id}\"\nattempts: 2\nmax_attempts: 3\n\
             last_review_verdict: \"NONE\"\nallowed_scope: [\"src/lib.rs\"]\nnext_action: \"{reason}\"\n---\n\n\
             ## Objetivo\nFazer {id}.\n\n## Critérios de Aceitação\n- compila\n"
        )
    }

    /// Motivo que o runner antigo gravou no teste-web.
    const TESTE_WEB_REASON: &str = "alterações não commitadas em .sdd: .sdd/settings.yaml";
    const TASK_T01: &str = ".sdd/specs/s1/tasks/T01.md";
    const TASK_T02: &str = ".sdd/specs/s1/tasks/T02.md";

    /// Projeto como no teste-web: `.sdd` criado pelo fluxo de produção, commit manual de TUDO
    /// (`.sdd/settings.yaml` versionado), settings modificado depois (comando e `allowed_commands`),
    /// spec `IN_PROGRESS` com `T01` (e `T02`, se `two`) BLOCKED e a worktree/branch de `T01` existente.
    async fn blocked_project(tag: &str, two: bool) -> PathBuf {
        let root = real_repo(tag, false);
        let _ = created_project(&root).await;
        architect_writes_plan(&root, "node --test").await;
        if two {
            let t2 = root.join(TASK_T02);
            fs::write(&t2, plan_task().replace("T01", "T02")).expect("T02");
        }
        git(&root, &["add", "-A"]);
        commit_base(&root);
        git(&root, &["branch", "-M", "main"]);
        fs::write(
            root.join(".sdd/settings.yaml"),
            concat!(
                "base_branch: main\nmerge_strategy: squash\ntest_command: \"npm test\"\n",
                "allowed_commands:\n  - \"npm test\"\n",
                "environment: docker\nenvironment_hash: 956ec7f74f0b60d1d28f0f623ad234c11881f2438c985c66dc61aec409368168\n"
            ),
        )
        .expect("settings");
        let spec_path = root.join(format!(".sdd/specs/{SPEC}/spec.md"));
        let spec = fs::read_to_string(&spec_path)
            .expect("spec")
            .replace("AWAITING_APPROVAL", "IN_PROGRESS");
        fs::write(&spec_path, spec).expect("spec");
        fs::write(root.join(TASK_T01), blocked_task_md("T01", TESTE_WEB_REASON)).expect("T01");
        if two {
            fs::write(root.join(TASK_T02), blocked_task_md("T02", TESTE_WEB_REASON)).expect("T02");
        }
        let wt = root.join(format!(".sdd/worktrees/{SPEC}/T01"));
        git(
            &root,
            &["worktree", "add", "-q", "-B", "sdd/s1/T01", wt.to_str().expect("utf8"), "HEAD"],
        );
        fs::write(wt.join("out-T01-1.txt"), "trabalho do dev\n").expect("dev");
        git(&wt, &["add", "-A"]);
        commit_base(&wt);
        root
    }

    fn resume_req(project: &str, spec: &str, body: &str) -> Request<Body> {
        request(
            Method::POST,
            &format!("/api/projects/{project}/specs/{spec}/resume"),
            body,
            Some("application/json"),
        )
    }

    fn task_of(root: &Path, rel: &str) -> Task {
        Task::parse(&fs::read_to_string(root.join(rel)).expect("tarefa")).expect("tarefa válida")
    }

    /// Mensagem de erro de uma resposta 409/4xx.
    fn error_of(body: &str) -> String {
        json_of(body)["error"].as_str().expect("error").to_string()
    }

    #[tokio::test]
    async fn e2e_resume_reopens_blocked_task_on_teste_web_repo() {
        let root = blocked_project("e2e-resume", false).await;
        assert!(check_git(&root).is_ok(), "settings versionado modificado não bloqueia a base");
        let state = state(&root, Script::Hang);
        let mut rx = state.events().subscribe();

        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(json_of(&body), json!({"ok": true, "spec_id": SPEC}));
        let t1 = task_of(&root, TASK_T01);
        assert_eq!(t1.frontmatter.status, TaskStatus::Todo);
        assert_eq!(t1.frontmatter.attempts, 0);
        assert!(t1.frontmatter.next_action.is_none());
        assert!(t1.body.contains("Fazer T01."), "corpo preservado");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        assert!(state.runs().active(PROJECT).is_some(), "execução disparada");
        assert!(drain(&mut rx).iter().any(|e| {
            e.event_type() == "project_status" && e.payload["status"].as_str() == Some("executing")
        }));
        assert!(check_git(&root).is_ok(), "base continua sem bloqueio");

        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    /// Fábrica que anota a conexão de cada papel (o modelo montado) e segue como a [`Scripted`].
    #[derive(Debug)]
    struct RoleRecorder(Arc<Mutex<Vec<(String, String, Option<String>)>>>);

    impl RunnerFactory for RoleRecorder {
        fn build(&self, job: RunJob) -> Result<Box<dyn SpecRunner>, RunnerError> {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).push((
                job.developer.model.clone(),
                job.reviewer.model.clone(),
                job.architect.as_ref().map(|a| a.model.clone()),
            ));
            Scripted(Script::Hang).build(job)
        }
    }

    /// Spec 133 T02: a troca no cartão da pausa grava só a conexão daquele papel (o mesmo
    /// `PUT /api/config/agents` da tela de Agentes, com os outros papéis como estavam) e o
    /// Continuar monta a execução com a nova conexão desse papel; os outros papéis seguem iguais.
    #[tokio::test]
    async fn switch_in_the_pause_card_applies_on_continue_only_for_that_role() {
        let root = blocked_project("troca-papel", false).await;
        let mut cfg = config_for(&root);
        cfg.connections.push(connection("conta-b", "chave-b-literal"));
        cfg.agents.architect = AgentConfig { connection_id: "literal".into(), model: "modelo-arq".into(), effort: Effort::Low, max_steps: None };
        let built = Arc::new(Mutex::new(Vec::new()));
        let compose = crate::server::envs::ComposeProvider::with_runner(
            Arc::new(crate::server::envs::tests::Rec::default()),
            root.with_file_name("compose-overrides"),
        );
        let state = AppState::new(root.with_file_name("config.toml"), crate::server::readiness::chat_ready(cfg), PORT)
            .with_runner_factory(Arc::new(RoleRecorder(Arc::clone(&built))))
            .with_shutdown_grace(TEST_GRACE)
            .with_compose_provider(compose);

        // O que o cartão envia: os papéis de hoje com só o Desenvolvedor na nova conexão.
        let agents = json!({
            "architect": {"connection_id": "literal", "model": "modelo-arq", "effort": "low"},
            "developer": {"connection_id": "conta-b", "model": "modelo-b", "effort": "low"},
            "reviewer": {"connection_id": "literal", "model": "modelo-rev", "effort": "medium"},
        });
        let (status, body) = call(&state, request(Method::PUT, "/api/config/agents", &agents.to_string(), Some("application/json"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        {
            let cfg = state.config().read().await;
            assert_eq!((cfg.agents.developer.connection_id.as_str(), cfg.agents.developer.model.as_str()), ("conta-b", "modelo-b"));
            assert_eq!((cfg.agents.reviewer.connection_id.as_str(), cfg.agents.reviewer.model.as_str()), ("literal", "modelo-rev"));
            assert_eq!((cfg.agents.architect.connection_id.as_str(), cfg.agents.architect.model.as_str()), ("literal", "modelo-arq"));
        }

        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let runs = built.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(runs, vec![("modelo-b".to_string(), "modelo-rev".to_string(), Some("modelo-arq".to_string()))]);

        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    /// Spec 073: o resume simples (sem revisão) também é intervenção humana e zera a contagem; antes
    /// só a tarefa revisada zerava (esperado 3 no caso simples). O histórico segue preservado.
    #[tokio::test]
    async fn resume_resets_recovery_cycles_but_keeps_history() {
        let sections = "\n## Análise de recuperação #1\n- a\n\n## Análise de recuperação #2\n- b\n\n## Análise de recuperação #3\n- c\n";
        for revised in [true, false] {
            let root = blocked_project(if revised { "resume-rev" } else { "resume-plain" }, false).await;
            let mut md = blocked_task_md("T01", "x");
            md.push_str(sections);
            if revised {
                md = md.replace("next_action: \"x\"", "next_action: \"definição revisada; retomar\"");
            }
            fs::write(root.join(TASK_T01), md).expect("T01");
            let state = state(&root, Script::Hang);
            let (status, body) = call(&state, resume_req(PROJECT, SPEC, "")).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            let t1 = task_of(&root, TASK_T01);
            assert_eq!(t1.frontmatter.attempts, 0);
            assert_eq!(crate::agents::runner::recovery_cycles(&t1.body), 0, "{revised}");
            assert!(t1.body.contains("## Análise de recuperação #3"), "histórico preservado");
            assert!(t1.body.contains("## Retomada pelo humano"), "marca de retomada gravada");
            state.trigger_shutdown();
            assert!(wait_idle(&state).await);
        }
    }

    /// Spec 114 T02: a retomada reconhece a definição revisada pelo código estável (texto em
    /// qualquer idioma) e, nas tarefas antigas sem código, pelo texto pt-BR; outro código não conta,
    /// mesmo com o texto antigo. O código sai junto com o motivo.
    #[tokio::test]
    async fn resume_reads_revised_code_and_legacy_pt_text() {
        let cases = [
            ("rev-legado", "next_action: \"definição revisada; retomar\"", true),
            ("rev-codigo-en", "next_action: \"definition revised; resume\"\nnext_action_code: \"revised\"", true),
            ("rev-outro-codigo", "next_action: \"definição revisada; retomar\"\nnext_action_code: \"rejections\"", false),
        ];
        for (tag, fields, revised) in cases {
            let root = blocked_project(tag, false).await;
            let md = blocked_task_md("T01", "x").replace("next_action: \"x\"", fields);
            fs::write(root.join(TASK_T01), md).expect("T01");
            let state = state(&root, Script::Hang);
            let (status, body) = call(&state, resume_req(PROJECT, SPEC, "")).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            let t1 = task_of(&root, TASK_T01);
            let marked = t1.body.contains(&format!("## {}", crate::agents::runner::REVISED_MARK));
            assert_eq!(marked, revised, "{tag}: {}", t1.body);
            assert_eq!(t1.frontmatter.next_action_code, None, "{tag}");
            state.trigger_shutdown();
            assert!(wait_idle(&state).await);
        }
    }

    /// Caso real: T01 INTEGRATING (com ou sem commit na base), T02 TODO, nenhuma BLOCKED.
    async fn integrating_project(tag: &str, commit: bool) -> PathBuf {
        let root = blocked_project(tag, true).await;
        let t1 = blocked_task_md("T01", "x")
            .replace("BLOCKED", "INTEGRATING")
            .replace("attempts: 2", "attempts: 1");
        fs::write(root.join(TASK_T01), t1).expect("T01");
        let t2 = blocked_task_md("T02", "x")
            .replace("BLOCKED", "TODO")
            .replace("depends_on: []", "depends_on: [\"T01\"]");
        fs::write(root.join(TASK_T02), t2).expect("T02");
        if commit {
            fs::write(root.join("t01-feito.txt"), "ok\n").expect("arquivo");
            git(&root, &["add", "t01-feito.txt"]);
            git(
                &root,
                &[
                    "-c", "user.name=Teste", "-c", "user.email=teste@example.invalid",
                    "-c", "commit.gpgsign=false", "commit", "-q", "-m",
                    &format!("feat({SPEC}): T01 - Tarefa T01"),
                ],
            );
        }
        root
    }

    #[tokio::test]
    async fn resume_without_blocked_accepts_integrating_with_commit() {
        let root = integrating_project("resume-integrating-commit", true).await;
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert!(state.runs().active(PROJECT).is_some(), "execução disparada");
        // O servidor não toca nas tarefas; a finalização contra o Git é do runner.
        assert_eq!(task_of(&root, TASK_T01).frontmatter.status, TaskStatus::Integrating);
        assert_eq!(task_of(&root, TASK_T02).frontmatter.status, TaskStatus::Todo);
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn resume_without_blocked_and_integrating_without_commit_is_safe() {
        let root = integrating_project("resume-integrating-nocommit", false).await;
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(task_of(&root, TASK_T01).frontmatter.status, TaskStatus::Integrating, "sem commit não vira DONE");
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn resume_is_refused_statically_without_effects() {
        // Spec aguardando aprovação (não está em andamento).
        let root = blocked_project("resume-awaiting", false).await;
        let spec_path = root.join(format!(".sdd/specs/{SPEC}/spec.md"));
        let spec = fs::read_to_string(&spec_path).expect("spec").replace("IN_PROGRESS", "AWAITING_APPROVAL");
        fs::write(&spec_path, spec).expect("spec");
        let before = fs::read_to_string(root.join(TASK_T01)).expect("T01");
        let st = state(&root, Script::Hang);
        let (status, body) = call(&st, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(error_of(&body), "spec não está em andamento");
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), before);
        assert!(st.runs().active(PROJECT).is_none());

        // Nada a fazer (T01 DONE).
        let root = blocked_project("resume-no-blocked", false).await;
        fs::write(root.join(TASK_T01), blocked_task_md("T01", "x").replace("BLOCKED", "DONE")).expect("T01 DONE");
        let before = fs::read_to_string(root.join(TASK_T01)).expect("T01");
        let st = state(&root, Script::Hang);
        let (status, body) = call(&st, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(error_of(&body), "não há tarefa bloqueada para retomar");
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), before);
        assert!(st.runs().active(PROJECT).is_none());

        // Base suja fora de .sdd: mesma recusa da aprovação, sem efeito.
        let root = blocked_project("resume-dirty", false).await;
        fs::write(root.join("src.txt"), "humano\n").expect("src");
        let before = fs::read_to_string(root.join(TASK_T01)).expect("T01");
        let st = state(&root, Script::Hang);
        let (status, body) = call(&st, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_BASE_DIRTY);
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), before);
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
    }

    #[tokio::test]
    async fn resume_is_409_while_budget_is_hit_and_accepted_once_raised() {
        use crate::agents::llm_client::Usage;
        use crate::agents::usage::{UsageEvent, UsageSink};
        let root = blocked_project("resume-budget", false).await;
        super::super::usage::FileUsageSink::new(&root, PROJECT).record(UsageEvent {
            agent: "developer".into(),
            model: "m".into(),
            spec: Some(SPEC.into()),
            task: Some("T01".into()),
            usage: Usage::new(600, 500),
            web_search_calls: 0,
            images: 0,
            image_quality: None,
            estimated: false,
            cut: None,
            billing: Default::default(),
            context: None,
            call: Default::default(),
        });
        fs::write(root.join(format!(".sdd/specs/{SPEC}/budget.json")), r#"{"max_tokens":1000}"#).expect("budget");
        let before = fs::read_to_string(root.join(TASK_T01)).expect("T01");
        let st = state(&root, Script::Hang);
        let (status, body) = call(&st, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_BUDGET);
        assert!(error_of(&body).contains("1100 de 1000 tokens"), "{body}");
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), before, "sem efeitos");
        assert!(st.runs().active(PROJECT).is_none());

        let put = request(
            Method::PUT,
            &format!("/api/projects/{PROJECT}/specs/{SPEC}/budget"),
            r#"{"max_tokens":5000}"#,
            Some("application/json"),
        );
        let (status, body) = call(&st, put).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = call(&st, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        st.trigger_shutdown();
        assert!(wait_idle(&st).await);
    }

    #[tokio::test]
    async fn resume_does_not_reopen_blocked_task_already_integrated_in_base() {
        // Integração já na base (commit de feat da tarefa): a tarefa não volta a TODO; o runner a
        // finaliza. Só a tarefa não integrada (T02) é reaberta.
        let root = blocked_project("resume-integrated", true).await;
        git(
            &root,
            &[
                "-c",
                "user.name=Teste",
                "-c",
                "user.email=teste@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "feat(s1): T01 - Tarefa T01",
            ],
        );
        let before_t1 = fs::read_to_string(root.join(TASK_T01)).expect("T01");
        let st = state(&root, Script::Hang);
        let (status, body) = call(&st, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), before_t1);
        assert_eq!(task_of(&root, TASK_T02).frontmatter.status, TaskStatus::Todo);
        st.trigger_shutdown();
        assert!(wait_idle(&st).await);
    }

    #[tokio::test]
    async fn resume_with_active_run_is_409_busy_and_keeps_first_run() {
        let root = blocked_project("resume-busy", false).await;
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["code"], BUSY);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["code"], BUSY);
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    // ---- Docker como padrão único (spec 074) ----

    /// Plano sem `environment: docker` é recusado na aprovação (409), sem efeitos.
    #[tokio::test]
    async fn approving_a_plan_without_docker_environment_is_409_without_effects() {
        let root = setup("plan-no-docker", true);
        let spec_path = root.join(".sdd/specs").join(SPEC).join("spec.md");
        let plain = fs::read_to_string(&spec_path).expect("spec").replace("environment: \"docker\"\n", "");
        fs::write(&spec_path, &plain).expect("spec");
        let state = state(&root, Script::Complete);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_ENV_NOT_DOCKER);
        assert!(error_of(&body).contains("environment: docker"), "{body}");
        assert_eq!(fs::read_to_string(&spec_path).expect("spec"), plain, "spec intocada");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(state.runs().active(PROJECT).is_none());
    }

    /// Sem Docker no host, aprovar é recusado pela prontidão (spec 093 T02): 412 com os ids.
    #[tokio::test]
    async fn approving_without_docker_on_the_host_is_412_prereq_missing() {
        let root = setup("approve-no-host-docker", true);
        let state = state(&root, Script::Complete).with_prereqs(prereqs(false));
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
        assert_eq!(
            json_of(&body),
            json!({"error": "prereq_missing", "missing": ["docker_cli", "docker_daemon", "docker_permission", "docker_compose"], "code": "prereq_missing", "params": {"count": 4}})
        );
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(state.runs().active(PROJECT).is_none());
    }

    /// Sem Docker no host, retomar também é recusado (e nada é reaberto).
    #[tokio::test]
    async fn resuming_without_docker_on_the_host_is_412_and_keeps_blocked_tasks() {
        let root = blocked_project("resume-no-host-docker", true).await;
        let before = fs::read_to_string(root.join(TASK_T01)).expect("T01");
        let state = state(&root, Script::Hang).with_prereqs(prereqs(false));
        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
        assert_eq!(json_of(&body)["error"], "prereq_missing");
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), before);
        assert!(state.runs().active(PROJECT).is_none());
    }

    /// Projeto em modo host (settings sem `environment: docker`) é recusado na retomada, sem migração.
    #[tokio::test]
    async fn resuming_a_host_project_is_409_replan_and_nothing_changes() {
        let root = blocked_project("resume-host-project", true).await;
        let host_settings = "base_branch: main\nmerge_strategy: squash\ntest_command: \"npm test\"\nallowed_commands:\n  - \"npm test\"\n";
        fs::write(root.join(".sdd/settings.yaml"), host_settings).expect("settings");
        let before = fs::read_to_string(root.join(TASK_T01)).expect("T01");
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(error_of(&body), "este projeto não tem ambiente docker; replaneje a spec");
        assert_eq!(json_of(&body)["reason"], REASON_ENV_NOT_DOCKER);
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), before);
        assert_eq!(settings_text(&root), host_settings, "sem migração automática");
        assert!(!root.join(".nants/Dockerfile").exists());
        assert!(state.runs().active(PROJECT).is_none());
    }

    /// A opção `environment` com `files` não adota nada em projeto host: recusa com o mesmo 409.
    #[tokio::test]
    async fn environment_option_is_refused_in_a_host_project_without_adopting() {
        let (root, _) = blocked_with_options("env-opt-host", ENV_FILES_JSON).await;
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_PLAIN).expect("settings");
        let state = resume_state(&root);
        let (code, body) = call(&state, choice_req("T01", r#"{"option_id":"mig","analysis":1}"#)).await;
        assert_eq!(code, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_ENV_NOT_DOCKER);
        assert_eq!(settings_text(&root), SETTINGS_PLAIN);
        assert!(!built_adjusted(&root), "ambiente intocado");
    }

    #[test]
    fn relaunch_refusal_carries_the_code_params_and_detail() {
        let secret = ["sk", "-RELAUNCH", "-0123456789"].concat();
        let secrets = SecretSet::new([secret.clone()]);
        let docker = ApiError::coded(StatusCode::CONFLICT, "docker_unavailable", params([("item", json!("docker_cli"))]))
            .with_reason(REASON_DOCKER_UNAVAILABLE);
        let (text, p) = relaunch_failure(Lang::PtBr, &secrets, Some(&docker));
        assert_eq!(p["error_code"], "docker_unavailable");
        assert_eq!(p["item"], "docker_cli");
        assert!(p["message"].as_str().is_some_and(|m| m.starts_with("O Docker é pré-requisito")), "{p:?}");
        assert!(p.get("detail").is_none());
        assert!(text.starts_with("O relançamento em modo docker foi recusado; use Retomar. Motivo: O Docker é pré-requisito"), "{text}");
        let env = conflict_reason("environment_invalid", REASON_ENV_INVALID)
            .with_detail(format!("compose.yaml não pode expor o docker.sock {secret}"));
        let (text, p) = relaunch_failure(Lang::PtBr, &secrets, Some(&env));
        assert_eq!(p["error_code"], "environment_invalid");
        let detail = p["detail"].as_str().unwrap_or_default();
        assert!(detail.starts_with("compose.yaml não pode expor o docker.sock"), "{detail}");
        assert!(!detail.contains(&secret) && !text.contains(&secret), "segredo redigido");
        assert!(text.contains("docker.sock"), "{text}");
        assert!(text.contains("\nDetalhe técnico: compose.yaml"), "{text}");
        let (text, p) = relaunch_failure(Lang::PtBr, &secrets, None);
        assert!(text.starts_with("O relançamento em modo docker foi recusado; use Retomar. Motivo: o projeto"), "{text}");
        assert_eq!(p.len(), 1);
        // Spec 137 T12: num projeto en, o texto gravado e o motivo da recusa saem em inglês.
        let (text, p) = relaunch_failure(Lang::En, &secrets, Some(&docker));
        assert!(text.starts_with("The relaunch in docker mode was refused; use Resume. Reason: "), "{text}");
        assert_eq!(p["error_code"], "docker_unavailable");
        assert!(!p["message"].as_str().unwrap_or_default().contains("Docker é"), "{p:?}");
        let (text, _) = relaunch_failure(Lang::En, &secrets, Some(&env));
        assert!(text.contains("\nTechnical detail: "), "{text}");
    }

    #[test]
    fn docker_prereq_missing_names_the_first_missing_item_only_when_unavailable() {
        assert_eq!(docker_prereq_missing(&prereqs(true)), None);
        assert_eq!(docker_prereq_missing(&prereqs(false)), Some("docker_cli"));
        let err = ApiError::coded(StatusCode::CONFLICT, "docker_unavailable", params([("item", json!("docker_cli"))]));
        assert!(err.message().starts_with("O Docker é pré-requisito do Nants"), "{}", err.message());
        assert!(err.message().contains("docker_cli"), "{}", err.message());
    }

    #[tokio::test]
    async fn resume_without_agent_is_412_and_blocked_tasks_stay() {
        let root = blocked_project("resume-no-agent", true).await;
        let before = fs::read_to_string(root.join(TASK_T01)).expect("T01");
        let mut cfg = config_for(&root);
        cfg.agents.developer.connection_id = String::new();
        let state = state_with(&root, cfg, Script::Hang);
        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
        assert_eq!(json_of(&body), json!({"error": "prereq_missing", "missing": ["agent_developer"], "code": "prereq_missing", "params": {"count": 1}}));
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), before);
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        assert!(state.runs().active(PROJECT).is_none());
    }

    #[tokio::test]
    async fn failed_reset_write_restores_blocked_tasks_and_retry_works() {
        let root = blocked_project("resume-write-fail", true).await;
        let before_t1 = fs::read_to_string(root.join(TASK_T01)).expect("T01");
        let before_t2 = fs::read_to_string(root.join(TASK_T02)).expect("T02");
        fail_reset_write(&root, 1);
        let state = state(&root, Script::Hang);
        let mut rx = state.events().subscribe();

        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), before_t1, "T01 restaurada");
        assert_eq!(fs::read_to_string(root.join(TASK_T02)).expect("T02"), before_t2);
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        assert!(state.runs().active(PROJECT).is_none(), "sem execução");
        assert!(drain(&mut rx).iter().any(|e| {
            e.event_type() == "project_status"
                && e.payload["status"].as_str() == Some("idle")
                && e.payload["reason"].as_str() == Some("error")
        }));

        let (status, body) = call(&state, resume_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(task_of(&root, TASK_T01).frontmatter.status, TaskStatus::Todo);
        assert_eq!(task_of(&root, TASK_T02).frontmatter.status, TaskStatus::Todo);
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    // ---- Spec 128 T04: ordem de execução e renumeração na aprovação.

    /// Pasta da spec de teste.
    fn spec_dir_of(root: &Path) -> PathBuf {
        root.join(".sdd/specs").join(SPEC)
    }

    /// Corpo da spec com referências fora e dentro de código.
    const RENUMBER_BODY: &str = "\n## Tarefas\n- T08: contas. Depois T10 e T02.\n\n```\nT08 no bloco\n```\n\nVeja `T08` no código.\n";

    /// Plano do Test 6 (cópias das tarefas, TODO e com o `spec_id` da spec de teste), a spec com
    /// referências e os registros locais do plano.
    fn write_test6(root: &Path) {
        use crate::sdd::execution_order::test6::TEST6;
        let dir = spec_dir_of(root);
        for entry in fs::read_dir(dir.join("tasks")).expect("tasks") {
            fs::remove_file(entry.expect("entrada").path()).expect("apagar");
        }
        for raw in TEST6 {
            let mut task = Task::parse(raw).expect("tarefa");
            let meta = &mut task.frontmatter;
            meta.spec_id = SPEC.into();
            meta.status = TaskStatus::Todo;
            meta.attempts = 0;
            meta.last_verdict = crate::sdd::Verdict::None;
            meta.rejected_sha = None;
            meta.test_evidence = None;
            let id = meta.id.clone();
            fs::write(dir.join("tasks").join(format!("{id}.md")), task.render().expect("render")).expect("gravar");
        }
        fs::write(dir.join("spec.md"), format!("{}{RENUMBER_BODY}", spec_md(SPEC, "AWAITING_APPROVAL"))).expect("spec");
        let review = json!({
            "signature": "abc", "at": "2026-10-26T12:00:00Z", "status": "ok",
            "issues": [{"id": "P1", "task": "T08", "kind": "scope", "text": "T08 e T10 misturam áreas"}],
            "rounds": [{"round": 0, "issues": [{"id": "P1", "task": "T08", "kind": "scope", "text": "T08 grande"}]}],
            "points": [{"id": "P1", "task": "T08", "kind": "scope", "text": "T08 grande"}],
            "next_point": 2
        });
        fs::write(dir.join("plan-review.json"), review.to_string()).expect("revisão");
        fs::write(dir.join("plan-changes.json"), json!({"signature": "abc", "lines": ["T10 nova, depende da T08"]}).to_string()).expect("mudanças");
        fs::write(dir.join("usage.jsonl"), "{\"task_id\":\"T08\"}\n").expect("usage");
    }

    /// Conteúdo de cada arquivo da pasta da spec (e das tarefas).
    fn snapshot_spec_dir(root: &Path) -> Vec<(PathBuf, String)> {
        let dir = spec_dir_of(root);
        let mut out = Vec::new();
        for base in [dir.clone(), dir.join("tasks")] {
            for entry in fs::read_dir(&base).expect("pasta") {
                let path = entry.expect("entrada").path();
                if path.is_file() {
                    out.push((path.clone(), fs::read_to_string(&path).expect("ler")));
                }
            }
        }
        out.sort();
        out
    }

    fn chat_log(root: &Path) -> String {
        fs::read_to_string(root.join(".sdd/specs/.chat.jsonl")).unwrap_or_default()
    }

    #[tokio::test]
    async fn approval_renumbers_the_test6_plan_once_with_every_reference() {
        use crate::sdd::execution_order::test6::{TEST6, TEST6_ORDER};
        let root = setup("renumber-test6", true);
        write_test6(&root);
        let state = state(&root, Script::Hang);
        let lang = state.project_lang(PROJECT).get();

        // Antes da aprovação: o estado expõe a ordem de execução, com os ids como estão.
        let (status, body) = call(&state, get_req(&format!("/api/projects/{PROJECT}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let spec = json_of(&body)["specs"][0].clone();
        assert_eq!(spec["execution_order"], json!(TEST6_ORDER), "{spec}");
        let ids: Vec<String> = spec["tasks"].as_array().expect("tarefas").iter().map(|t| t["id"].as_str().unwrap_or("").to_string()).collect();
        assert_eq!(ids, (1..=15).map(|n| format!("T{n:02}")).collect::<Vec<_>>());

        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);

        // Tarefas: cada arquivo tem a tarefa que passou a ter aquele id, com o depends_on traduzido.
        let dir = spec_dir_of(&root);
        let old = |id: &str| Task::parse(TEST6[id[1..].parse::<usize>().expect("número") - 1]).expect("tarefa");
        let t02 = task_of(&root, &format!(".sdd/specs/{SPEC}/tasks/T02.md"));
        assert_eq!(t02.frontmatter.id, "T02");
        assert_eq!(t02.frontmatter.title, old("T08").frontmatter.title);
        assert_eq!(t02.frontmatter.depends_on, ["T01"]);
        let t04 = task_of(&root, &format!(".sdd/specs/{SPEC}/tasks/T04.md"));
        assert_eq!(t04.frontmatter.title, old("T02").frontmatter.title);
        assert_eq!(t04.frontmatter.depends_on, ["T02", "T03"]);
        assert!(t04.body.contains("before T07."), "{}", t04.body);
        // A ordem agora coincide com os ids.
        let now: Vec<(String, Vec<String>)> = (1..=15)
            .map(|n| task_of(&root, &format!(".sdd/specs/{SPEC}/tasks/T{n:02}.md")).frontmatter)
            .map(|m| (m.id, m.depends_on))
            .collect();
        let order = crate::sdd::execution_order::execution_order(&now).expect("sem ciclo");
        assert_eq!(order, (1..=15).map(|n| format!("T{n:02}")).collect::<Vec<_>>());

        // spec.md: referências trocadas fora do código, código intacto e a seção com o mapa.
        let spec_text = fs::read_to_string(dir.join("spec.md")).expect("spec");
        assert!(spec_text.contains("- T02: contas. Depois T03 e T04."), "{spec_text}");
        assert!(spec_text.contains("```\nT08 no bloco\n```"), "{spec_text}");
        assert!(spec_text.contains("Veja `T08` no código."), "{spec_text}");
        let heading = format!("## {}", crate::i18n::tr(lang, "runs.renumber.heading", &[]));
        assert!(spec_text.contains(&heading), "{spec_text}");
        assert!(spec_text.contains("T08→T02, T10→T03, T02→T04"), "{spec_text}");

        // Registros locais traduzidos; os históricos ficam como estão.
        let review: Value = serde_json::from_str(&fs::read_to_string(dir.join("plan-review.json")).expect("revisão")).expect("json");
        assert_eq!(review["issues"][0]["task"], "T02");
        assert_eq!(review["issues"][0]["text"], "T02 e T03 misturam áreas");
        assert_eq!(review["rounds"][0]["issues"][0]["task"], "T02");
        assert_eq!(review["points"][0]["task"], "T02");
        let changes: Value = serde_json::from_str(&fs::read_to_string(dir.join("plan-changes.json")).expect("mudanças")).expect("json");
        assert_eq!(changes["lines"][0], "T03 nova, depende da T02");
        assert_eq!(fs::read_to_string(dir.join("usage.jsonl")).expect("usage"), "{\"task_id\":\"T08\"}\n");

        // Uma linha na Conversa, pelo dicionário.
        let chat = chat_log(&root);
        assert!(chat.contains(RENUMBERED_CODE), "{chat}");
        assert!(chat.contains("T08→T02"), "{chat}");
        // O diário da renumeração sai depois da última escrita, e é local (exclude do Git).
        assert!(!dir.join(RENUMBER_JOURNAL_FILE).exists());
        let exclude = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
        assert!(exclude.contains(RENUMBER_JOURNAL_EXCLUDE_LINE) && exclude.contains(RENUMBER_JOURNAL_TMP_EXCLUDE_LINE), "{exclude}");
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    /// Spec 128 T05 (critério 12): os arquivos que a aprovação grava com a renumeração (a spec em
    /// `IN_PROGRESS` primeiro), calculados como em [`launch`].
    fn renumber_entries(root: &Path) -> Vec<Entry> {
        let inspected = inspect(root, SPEC, Mode::Approve).expect("inspeção");
        let renumbering = plan_renumbering(root, SPEC, &inspected.spec, &inspected.files, crate::i18n::Lang::PtBr)
            .expect("cálculo")
            .expect("renumera");
        let mut marked = renumbering.spec.clone();
        marked.frontmatter.status = SpecStatus::InProgress;
        let mut entries = vec![Entry { rel: spec_rel(SPEC), original: inspected.raw.clone(), marked: marked.render().expect("render") }];
        entries.extend(renumbering.entries.iter().cloned());
        entries
    }

    #[tokio::test]
    async fn a_journal_left_mid_renumbering_is_restored_on_startup() {
        let root = setup("renumber-journal", true);
        write_test6(&root);
        let before = snapshot_spec_dir(&root);
        let entries = renumber_entries(&root);
        assert!(entries.len() > 4, "{}", entries.len());
        // Queda abrupta no meio: o diário, a spec e as três primeiras tarefas gravados; o resto não.
        write_renumber_journal(&root, SPEC, &entries).expect("diário");
        for entry in entries.iter().take(4) {
            write_atomic(&root, &entry.rel, &entry.marked).expect("escrita");
        }
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        assert_ne!(snapshot_spec_dir(&root), before);

        // A subida do servidor desfaz tudo antes de qualquer leitura do plano.
        let state = state(&root, Script::Hang);
        recover_renumber_journals(&state).await;
        assert_eq!(snapshot_spec_dir(&root), before, "originais de volta e o diário apagado");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        // Sem diário, a subida não mexe em nada; e a aprovação seguinte renumera normalmente.
        recover_renumber_journals(&state).await;
        assert_eq!(snapshot_spec_dir(&root), before);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(task_of(&root, &format!(".sdd/specs/{SPEC}/tasks/T02.md")).frontmatter.depends_on, ["T01"]);
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn a_journal_that_cannot_be_removed_undoes_the_approval() {
        let root = setup("renumber-journal-stuck", true);
        write_test6(&root);
        let before = snapshot_spec_dir(&root);
        FORCED_JOURNAL_REMOVE_FAILURES.lock().unwrap_or_else(PoisonError::into_inner).push(root.clone());
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(json_of(&body)["code"], RENUMBER_FAILED, "{body}");
        assert_eq!(snapshot_spec_dir(&root), before, "tudo desfeito, inclusive o diário");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(state.runs().active(PROJECT).is_none());
    }

    #[test]
    fn a_journal_with_a_file_in_a_third_state_leaves_everything_as_is() {
        let root = setup("renumber-journal-third", true);
        write_test6(&root);
        let entries = renumber_entries(&root);
        write_renumber_journal(&root, SPEC, &entries).expect("diário");
        for entry in &entries {
            write_atomic(&root, &entry.rel, &entry.marked).expect("escrita");
        }
        // A spec seguiu adiante: uma tarefa já tem outro texto (nem o original, nem o da renumeração).
        let third = &entries[1];
        write_atomic(&root, &third.rel, &format!("{}\n<!-- seguiu adiante -->\n", third.marked)).expect("terceiro texto");
        let before = snapshot_spec_dir(&root);
        assert!(!recover_renumber_journal(&root, SPEC));
        assert_eq!(snapshot_spec_dir(&root), before, "nada restaurado e o diário mantido");
        assert!(spec_dir_of(&root).join(RENUMBER_JOURNAL_FILE).exists());
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        // Arquivo ausente também é um terceiro estado.
        fs::remove_file(root.join(&third.rel)).expect("apagar");
        assert!(!recover_renumber_journal(&root, SPEC));
        assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
    }

    #[test]
    fn a_journal_from_elsewhere_is_not_touched() {
        let root = setup("renumber-journal-foreign", true);
        write_test6(&root);
        let entries = renumber_entries(&root);
        for (spec_id, rel) in [("outra", entries[0].rel.clone()), (SPEC, "../fora.md".to_string()), (SPEC, format!(".sdd/specs/{SPEC}/../x.md"))] {
            let mut forged = entries.clone();
            forged[0].rel = rel;
            let journal = RenumberJournal { spec_id: spec_id.to_string(), entries: forged };
            fs::write(spec_dir_of(&root).join(RENUMBER_JOURNAL_FILE), serde_json::to_string(&journal).expect("json")).expect("diário");
            fs::write(spec_dir_of(&root).join("spec.md"), &entries[0].marked).expect("spec");
            assert!(!recover_renumber_journal(&root, SPEC));
            assert!(spec_dir_of(&root).join(RENUMBER_JOURNAL_FILE).exists());
            assert_eq!(spec_status(&root, SPEC), SpecStatus::InProgress);
        }
        assert!(journal_entry_ok(SPEC, &format!(".sdd/specs/{SPEC}/tasks/T01.md")));
        assert!(!journal_entry_ok(SPEC, &format!(".sdd/specs/{SPEC}//T01.md")));
    }

    #[tokio::test]
    async fn approval_rewrites_nothing_when_the_plan_is_already_in_order() {
        let root = setup("renumber-noop", true);
        write_sdd(&root, SETTINGS, "AWAITING_APPROVAL", &[("T01", ""), ("T02", "T01"), ("T03", "T01")]);
        let before = snapshot_spec_dir(&root);
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let after = snapshot_spec_dir(&root);
        let changed: Vec<&PathBuf> = after.iter().zip(&before).filter(|(a, b)| a != b).map(|(a, _)| &a.0).collect();
        assert_eq!(changed, [&spec_dir_of(&root).join("spec.md")], "só o status da spec muda");
        let spec_text = fs::read_to_string(spec_dir_of(&root).join("spec.md")).expect("spec");
        assert_eq!(spec_text.matches("## ").count(), 1, "sem seção de renumeração: {spec_text}");
        assert!(!chat_log(&root).contains(RENUMBERED_CODE));
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn failed_renumbering_changes_nothing_and_returns_a_code() {
        let root = setup("renumber-fail", true);
        write_test6(&root);
        let before = snapshot_spec_dir(&root);
        fail_reset_write(&root, 3);
        let state = state(&root, Script::Hang);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(json_of(&body)["code"], RENUMBER_FAILED, "{body}");
        assert_eq!(snapshot_spec_dir(&root), before, "tudo ou nada");
        assert_eq!(spec_status(&root, SPEC), SpecStatus::AwaitingApproval);
        assert!(state.runs().active(PROJECT).is_none());
        assert!(!chat_log(&root).contains(RENUMBERED_CODE));
        // Sem a falha, a mesma aprovação renumera.
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(task_of(&root, &format!(".sdd/specs/{SPEC}/tasks/T02.md")).frontmatter.depends_on, ["T01"]);
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn approval_waits_for_a_running_plan_review_and_changes_nothing() {
        let root = setup("renumber-review", true);
        write_test6(&root);
        let before = snapshot_spec_dir(&root);
        let state = state(&root, Script::Hang);
        let hold = super::super::plan_review::hold_review_for_test(&root, SPEC).expect("vaga");
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["reason"], REASON_PLAN_REVIEW_RUNNING, "{body}");
        assert_eq!(json_of(&body)["code"], PLAN_REVIEW_RUNNING_MSG, "{body}");
        assert_eq!(snapshot_spec_dir(&root), before);
        drop(hold);
        let (status, body) = call(&state, approve_req(PROJECT, SPEC, "{}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        state.trigger_shutdown();
        assert!(wait_idle(&state).await);
    }

    #[tokio::test]
    async fn resume_applies_origin_guard_and_id_validation_like_approve() {
        let root = blocked_project("resume-guard", false).await;
        let state = state(&root, Script::Hang);
        let mut cross = resume_req(PROJECT, SPEC, "{}");
        cross
            .headers_mut()
            .insert(header::ORIGIN, "http://evil.example".parse().expect("hv"));
        let (status, _) = call(&state, cross).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(fs::read_to_string(root.join(TASK_T01)).expect("T01"), blocked_task_md("T01", TESTE_WEB_REASON));
        assert!(state.runs().active(PROJECT).is_none());

        for (project, spec) in [(PROJECT, "bad.id"), ("nao-existe", SPEC)] {
            let (approve_status, _) = call(&state, approve_req(project, spec, "{}")).await;
            let (resume_status, _) = call(&state, resume_req(project, spec, "{}")).await;
            assert_eq!(resume_status, approve_status, "{project}/{spec}");
        }
    }

    #[test]
    fn spec_id_validation_matches_contract() {
        assert!(validate_spec_id("s1_ok-2").is_ok());
        assert!(validate_spec_id("").is_err());
        assert!(validate_spec_id("a.b").is_err());
        assert!(validate_spec_id("a/b").is_err());
        assert!(validate_spec_id(&"a".repeat(MAX_SPEC_ID_LEN)).is_ok());
        assert!(validate_spec_id(&"a".repeat(MAX_SPEC_ID_LEN + 1)).is_err());
    }

    #[test]
    fn write_atomic_refuses_symlink_target() {
        let root = temp_dir("atomic-symlink");
        let outside = temp_dir("atomic-symlink-out");
        fs::create_dir_all(root.join(".sdd/specs/s1")).expect("dir");
        fs::write(outside.join("alvo.md"), "intacto").expect("alvo");
        std::os::unix::fs::symlink(outside.join("alvo.md"), root.join(".sdd/specs/s1/spec.md"))
            .expect("ln");
        assert!(write_atomic(&root, ".sdd/specs/s1/spec.md", "novo").is_err());
        assert_eq!(fs::read_to_string(outside.join("alvo.md")).expect("ler"), "intacto");
    }

    #[test]
    fn write_atomic_replaces_regular_file_without_temp_leftovers() {
        let root = temp_dir("atomic-ok");
        fs::create_dir_all(root.join(".sdd/specs/s1")).expect("dir");
        fs::write(root.join(".sdd/specs/s1/spec.md"), "antigo").expect("spec");
        write_atomic(&root, ".sdd/specs/s1/spec.md", "novo").expect("escrita");
        assert_eq!(
            fs::read_to_string(root.join(".sdd/specs/s1/spec.md")).expect("ler"),
            "novo"
        );
        let leftovers: Vec<_> = fs::read_dir(root.join(".sdd/specs/s1"))
            .expect("ls")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporário sobrou");
    }

    #[test]
    fn provider_runner_error_becomes_fixed_provider_end() {
        let err = RunnerError::Provider {
            role: "developer",
            failure: ProviderFailure::Http(400, None),
            evidence: None,
        };
        let end = run_end(Lang::PtBr, Ok(Err(err)));
        assert!(matches!(end, RunEnd::Provider(ProviderFailure::Http(400, None), None)));
        let other = run_end(Lang::PtBr, Ok(Err(RunnerError::Config("x".into()))));
        assert!(matches!(other, RunEnd::Failed(Some(ref c)) if c == "configuração inválida: x"));
    }

    /// Spec 102: as mensagens do sistema levam `code` e `params` junto do texto de reserva pt-BR.
    #[test]
    fn system_messages_carry_code_and_params() {
        let secrets = SecretSet::new(vec![SECRET.to_string()]);
        let code_of = |ev: &ApiEvent| (ev.payload["code"].clone(), ev.payload["params"].clone());
        let stopped = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Stopped);
        assert_eq!(stopped[0].payload["text"], STOPPED_TEXT);
        assert_eq!(code_of(&stopped[0]), (json!(CODE_STOPPED), json!({})));
        let failed = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Failed(None));
        assert_eq!(code_of(&failed[0]), (json!(CODE_RUN_FAILED), json!({})));
        let cause = format!("boom {SECRET}");
        let failed = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Failed(Some(cause)));
        let (code, params) = code_of(&failed[0]);
        assert_eq!(code, json!(CODE_RUN_FAILED_CAUSE));
        let cause = params["cause"].as_str().unwrap_or_default();
        assert!(cause.starts_with("boom") && !cause.contains(SECRET), "{params}");
        assert!(failed[0].payload["text"].as_str().unwrap_or_default().contains(cause));
        let panicked = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Panicked);
        assert_eq!(code_of(&panicked[0]).0, json!(CODE_RUN_FAILED));
        let provider = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Provider(ProviderFailure::Http(400, None), None));
        assert_eq!(code_of(&provider[0]), (json!(CODE_PROVIDER_FAILED), json!({"provider": "provider_http_400"})));
        let reason = crate::agents::llm_client::BadReason::with_code(
            crate::agents::llm_client::BadKind::Incomplete,
            Some("content_filter"),
        );
        let bad = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Provider(ProviderFailure::BadResponse(reason), None));
        assert_eq!(
            code_of(&bad[0]),
            (json!(CODE_PROVIDER_FAILED_DETAIL), json!({"provider": "provider_bad_response", "kind": "incomplete", "provider_code": "content_filter"}))
        );
        // Status sem mensagem do sistema não ganha `code`.
        let integrated = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Outcome(SpecOutcome::Completed));
        assert!(integrated[0].payload.get("code").is_none());
    }

    /// Spec 116 T02: as falhas do plano ChatGPT pausam a execução com o código próprio, sem
    /// parâmetros (nem horário de liberação); o "Continuar" retoma.
    #[test]
    fn plan_failures_pause_with_their_own_code() {
        use crate::agents::llm_client::PlanError;
        let secrets = SecretSet::new(vec![SECRET.to_string()]);
        for (plan, code, text) in [
            (PlanError::UsageLimit, CODE_PLAN_LIMIT, "provider_plan_limit"),
            (PlanError::NotEligible, CODE_PLAN_NOT_ELIGIBLE, "provider_plan_not_eligible"),
            (PlanError::NeedsLogin, CODE_PLAN_NEEDS_LOGIN, "provider_plan_needs_login"),
        ] {
            let events = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Provider(ProviderFailure::Plan(plan), None));
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].payload["code"], json!(code));
            assert_eq!(events[0].payload["params"], json!({}));
            assert_eq!(events[0].payload["text"], json!(text));
            assert_eq!(events[1].payload["status"], "idle");
        }
        let err = RunnerError::Provider { role: "developer", failure: ProviderFailure::Plan(PlanError::UsageLimit), evidence: None };
        assert!(matches!(run_end(Lang::PtBr, Ok(Err(err))), RunEnd::Provider(ProviderFailure::Plan(PlanError::UsageLimit), None)));
    }

    /// Spec 133 T01: a falha do provedor com evidência leva os `params` completos (papel, conexão,
    /// modelo, tarefa, tentativa, status, códigos, `request-id` e categoria) à mensagem do sistema e
    /// ao `dialog.jsonl`, e grava o registro persistente da spec sem a chave; a pausa do limite
    /// também leva a evidência.
    #[tokio::test]
    async fn provider_failure_with_evidence_has_full_params_and_persistent_record() {
        use crate::agents::llm_client::{FailureCategory, HttpEvidence, ProviderLimit};
        let root = setup("provider-evidence", true);
        let secrets = Arc::new(SecretSet::new(vec![SECRET.to_string()]));
        let evidence = FailureEvidence {
            role: "reviewer".into(),
            model: "gemini-x".into(),
            task: Some("T03".into()),
            attempt: Some(1),
            category: Some(FailureCategory::BadRequest),
            retries: 0,
            http: Some(HttpEvidence {
                status: 400,
                error_type: Some("invalid_argument".into()),
                error_code: Some("400".into()),
                error_param: None,
                request_id: Some("req_77".into()),
                message: Some(format!("chave {SECRET} inválida")),
            }),
            connection: None,
        };
        let roles = vec![("reviewer", ConnectionRef { id: "gem".into(), name: "Gemini do Ana".into(), kind: "google_gemini".into() })];
        let mut end = RunEnd::Provider(ProviderFailure::Http(400, None), Some(Box::new(evidence.clone())));
        record_provider_end(&mut end, &roles, &root, PROJECT, SPEC, &secrets).await;
        let events = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, end);
        let dialog = DialogTarget::new(root.clone(), SPEC.to_string());
        let bus = EventBus::new();
        for ev in events.iter().cloned() {
            deliver(&bus, Some(&dialog), ev);
        }
        crate::server::dialog::flush_project(&root, Duration::from_secs(5));
        let params = events[0].payload["params"].clone();
        assert_eq!(events[0].payload["code"], json!(CODE_PROVIDER_FAILED));
        let expected = json!({
            "provider": "provider_http_400",
            "role": "reviewer",
            "model": "gemini-x",
            "connection_id": "gem",
            "connection_name": "Gemini do Ana",
            "connection_kind": "google_gemini",
            "task": "T03",
            "attempt": 1,
            "category": "bad_request",
            "status": 400,
            "error_type": "invalid_argument",
            "error_code": "400",
            "request_id": "req_77",
        });
        assert_eq!(params, expected);
        let line = fs::read_to_string(root.join(format!(".sdd/specs/{SPEC}/dialog.jsonl"))).expect("diálogo");
        let saved: Value = serde_json::from_str(line.lines().find(|l| l.contains("provider_http_400")).expect("linha")).expect("json");
        assert_eq!(saved["params"], expected, "o diálogo guarda os params inteiros");
        let rows = super::super::provider_errors::load(&root, Some(SPEC));
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].connection_name.as_deref(), rows[0].request_id.as_deref()), (Some("Gemini do Ana"), Some("req_77")));
        let raw = fs::read_to_string(root.join(format!(".sdd/specs/{SPEC}/provider-errors.jsonl"))).expect("registro");
        assert!(raw.contains("inválida") && !raw.contains(SECRET), "{raw}");
        let exclude = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
        assert!(exclude.contains(super::super::provider_errors::PROVIDER_ERRORS_EXCLUDE_LINE), "{exclude}");

        let mut limit = RunEnd::Provider(ProviderFailure::Limit(ProviderLimit::SpendLimit), Some(Box::new(evidence)));
        record_provider_end(&mut limit, &roles, &root, PROJECT, SPEC, &secrets).await;
        let events = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, limit);
        assert_eq!(events[0].payload["code"], json!(CODE_SPEND_LIMIT));
        assert_eq!(events[0].payload["params"]["role"], "reviewer");
        assert_eq!(events[0].payload["params"]["connection_name"], "Gemini do Ana");
        assert_eq!(super::super::provider_errors::load(&root, Some(SPEC)).len(), 2);
    }

    /// Spec 132 T09: o teto de gasto da Anthropic e a cota diária do Gemini pausam a execução com o
    /// código próprio, como o limite do plano.
    #[test]
    fn provider_limits_pause_with_their_own_code() {
        use crate::agents::llm_client::ProviderLimit;
        let secrets = SecretSet::new(vec![SECRET.to_string()]);
        for (limit, code, text) in [
            (ProviderLimit::SpendLimit, CODE_SPEND_LIMIT, "provider_spend_limit"),
            (ProviderLimit::DailyQuota, CODE_DAILY_QUOTA, "provider_daily_quota"),
            (ProviderLimit::Billing, CODE_BILLING, "provider_billing"),
        ] {
            let events = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Provider(ProviderFailure::Limit(limit), None));
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].payload["code"], json!(code));
            assert_eq!(events[0].payload["params"], json!({}));
            assert_eq!(events[0].payload["text"], json!(text));
            assert_eq!(events[1].payload["status"], "idle");
        }
        let err = RunnerError::Provider { role: "developer", failure: ProviderFailure::Limit(ProviderLimit::SpendLimit), evidence: None };
        assert!(matches!(run_end(Lang::PtBr, Ok(Err(err))), RunEnd::Provider(ProviderFailure::Limit(ProviderLimit::SpendLimit), None)));
    }

    #[test]
    fn terminal_events_are_fixed_and_redacted() {
        let secrets = SecretSet::new(vec![SECRET.to_string()]);
        let failed = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Failed(None));
        assert_eq!(failed.len(), 2);
        assert_eq!(failed[0].payload["text"], RUN_FAILED_TEXT);
        let panicked = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Panicked);
        assert_eq!(panicked[0].payload["text"], RUN_FAILED_TEXT);
        assert_eq!(panicked[1].payload["reason"], "error");
        let provider = terminal_events(
            Lang::PtBr,
            PROJECT,
            &secrets,
            SPEC,
            RunEnd::Provider(ProviderFailure::Http(400, None), None),
        );
        assert_eq!(provider.len(), 2);
        assert_eq!(provider[0].payload["text"], "provider_http_400");
        assert_eq!(provider[1].payload["reason"], "error");
        let reason = crate::agents::llm_client::BadReason::with_code(
            crate::agents::llm_client::BadKind::Incomplete,
            Some("content_filter"),
        );
        let bad = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Provider(ProviderFailure::BadResponse(reason), None));
        assert_eq!(bad[0].payload["text"], "provider_bad_response (incompleta: content_filter)");
        assert_eq!(bad[1].payload["reason"], "error");
        let blocked = terminal_events(
            Lang::PtBr,
            PROJECT,
            &secrets,
            SPEC,
            RunEnd::Outcome(SpecOutcome::Blocked {
                task_id: SECRET.into(),
                reason: String::new(),
                kind: crate::agents::runner::BlockKind::Environmental,
                open_findings: vec![],
            }),
        );
        assert!(!blocked[0].payload.to_string().contains(SECRET));
    }

    #[test]
    fn failed_run_message_carries_redacted_and_capped_cause() {
        // Spec 081 T02: a causa aparece, com scrub, numa linha e limitada sem quebrar caracteres.
        let secrets = SecretSet::new(vec![SECRET.to_string()]);
        let cause = format!("falha do papel developer: limite de 40 passos\ncom {SECRET}");
        let ev = terminal_events(Lang::PtBr,PROJECT, &secrets, SPEC, RunEnd::Failed(Some(cause)));
        let text = ev[0].payload["text"].as_str().expect("texto").to_string();
        assert!(text.starts_with("A execução foi interrompida: falha do papel developer: limite de 40 passos com "), "{text}");
        assert!(text.ends_with("; confira o estado do projeto."), "{text}");
        assert!(!text.contains(SECRET), "segredo vazou: {text}");
        assert_eq!(ev[1].payload["reason"], "error");
        let long = "ç".repeat(400);
        let capped = run_failed_text(Lang::PtBr, &secrets, Some(&long));
        let shown = capped
            .trim_start_matches("A execução foi interrompida: ")
            .trim_end_matches("; confira o estado do projeto.");
        assert!(shown.len() <= RUN_FAILED_CAUSE_MAX_BYTES + '…'.len_utf8(), "{}", shown.len());
        assert!(shown.ends_with('…') && shown.starts_with("çç"), "{shown}");
        assert_eq!(run_failed_text(Lang::PtBr, &secrets, Some("  \n ")), RUN_FAILED_TEXT);
        assert_eq!(run_failed_text(Lang::PtBr, &secrets, None), RUN_FAILED_TEXT);
    }

    #[test]
    fn blocked_event_carries_kind() {
        use crate::agents::runner::BlockKind;
        let secrets = SecretSet::default();
        for (kind, want) in [(BlockKind::Quality, "quality"), (BlockKind::Environmental, "environmental")] {
            let ev = terminal_events(
                Lang::PtBr,
                PROJECT,
                &secrets,
                SPEC,
                RunEnd::Outcome(SpecOutcome::Blocked {
                    task_id: "T01".into(),
                    reason: String::new(),
                    kind,
                    open_findings: vec![],
                }),
            );
            assert_eq!(ev[0].payload["kind"], want);
            assert_eq!(ev[0].payload["reason"], BLOCKED_REASON);
        }
    }

    // ----- Recuperação agêntica de bloqueios (spec 018/T02) -----

    const RETRY_JSON: &str = r#"{"decision":"RETRY","root_cause":"o Developer não valida a entrada","strategy":"escrever primeiro o teste da validação em src/lib.rs"}"#;

    fn recovery_state(
        root: &Path,
        script: Script,
        replies: Vec<crate::agents::architect::testing::Reply>,
    ) -> (AppState, Arc<crate::agents::architect::testing::Scripted>) {
        let mut cfg = config_for(root);
        cfg.agents.architect = AgentConfig {
            connection_id: "literal".into(),
            model: "modelo-arq".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        let backend = crate::agents::architect::testing::Scripted::new(replies);
        let state = state_with(root, cfg, script)
            .with_report_backend(backend.clone())
            .with_prereqs(prereqs(true));
        (state, backend)
    }

    /// Pedidos da recuperação (análise, reforço, opções e diagnóstico) ao `report_backend`.
    ///
    /// Por que filtrar: depois do evento `integrated`, a mesma tarefa do run segue para
    /// `report::after_integration`, que gera o relatório final (`report_system`) e em seguida
    /// atualiza o ARCHITECTURE.md (`architecture_update_system`) pelo MESMO `report_backend` destes
    /// testes. `run_until(.., "integrated")` volta assim que o evento chega, e esses pedidos
    /// posteriores podem ou não já estar gravados quando o teste lê `backend.requests()`: era a
    /// intermitência das contagens. Eles ficam de fora; os da recuperação seguem contados.
    fn recovery_requests(backend: &crate::agents::architect::testing::Scripted) -> Vec<crate::agents::llm_client::ChatRequest> {
        use crate::agents::prompts::{architecture_update_system_for, report_system_for};
        use crate::i18n::Lang;
        let after: Vec<String> = [Lang::PtBr, Lang::En]
            .into_iter()
            .flat_map(|lang| [report_system_for(lang), architecture_update_system_for(lang)])
            .map(|p| p.chars().take(120).collect())
            .collect();
        backend
            .requests()
            .into_iter()
            .filter(|r| !r.messages.first().is_some_and(|m| after.iter().any(|head| m.content.starts_with(head.as_str()))))
            .collect()
    }

    async fn run_until(state: &AppState, status: &str) -> Vec<ApiEvent> {
        let mut rx = state.events().subscribe();
        let (code, body) = call(state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{body}");
        collect_until(&mut rx, status).await
    }

    fn payloads(events: &[ApiEvent], name: &str) -> Vec<Value> {
        events
            .iter()
            .filter(|e| e.event_type() == name)
            .map(|e| e.payload.clone())
            .collect()
    }

    #[tokio::test]
    async fn recovery_retry_resumes_same_run_and_completes() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-retry", true);
        fs::write(root.with_file_name("evidence.txt"), "FAILED: test_validacao\nassertion failed")
            .expect("evidência");
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text(RETRY_JSON)]);
        let events = run_until(&state, "integrated").await;

        assert_eq!(
            payloads(&events, "recovery_started"),
            vec![json!({"spec_id": SPEC, "task_id": "T01", "cycle": 1})]
        );
        assert_eq!(
            payloads(&events, "recovery_decided"),
            vec![json!({"spec_id": SPEC, "task_id": "T01", "cycle": 1, "decision": "RETRY"})]
        );
        let task = read_t01(&root);
        assert_eq!(task.frontmatter.status, TaskStatus::Todo);
        assert_eq!(task.frontmatter.attempts, 0);
        assert!(task.body.contains("## Análise de recuperação #1"), "{}", task.body);
        assert!(task.body.contains("Estratégia: escrever primeiro o teste"), "{}", task.body);
        assert!(events.iter().any(|e| e.event_type() == "agent_message"
            && e.payload["agent"] == "architect"
            && e.payload["task_id"] == "T01"));
        let reqs = recovery_requests(&backend);
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0]
            .tools
            .iter()
            .all(|t| ["read_file", "list_dir", "search_code"].contains(&t.name.as_str())));
        let facts = reqs[0]
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(facts.contains("falta validar a entrada"), "{facts}");
        assert!(facts.contains("assertion failed"), "{facts}");
        assert!(facts.contains("Tarefa T01"), "{facts}");
    }

    /// Caso real (app-test-02): tarefa "escrever testes" com escopo só `tests/**`; o teste exige
    /// alterar `styles.css`. O Arquiteto lê a WORKTREE, decide REVISE_TASK ampliando o escopo e a
    /// tarefa volta a executar com o escopo novo.
    #[tokio::test]
    async fn recovery_revise_task_widens_scope_and_resumes() {
        use crate::agents::architect::testing::{calls, text};
        let root = setup("rec-revise", true);
        fs::write(root.with_file_name("evidence.txt"), "FAILED: tokens\nstyles.css sem --cor-fundo")
            .expect("evidência");
        let mut task = read_t01(&root);
        task.frontmatter.allowed_scope = vec!["tests/**".into()];
        fs::write(t01_path(&root), task.render().expect("render")).expect("escopo");
        let wt = root.join(".sdd/worktrees").join(SPEC).join("T01");
        fs::create_dir_all(&wt).expect("worktree");
        fs::write(wt.join("styles.css"), "SO_NA_WORKTREE").expect("css");
        let revise = r#"{"decision":"REVISE_TASK","root_cause":"o teste exige styles.css, fora do escopo","strategy":"incluir styles.css","objective":"Escrever os testes e criar os tokens em styles.css","acceptance":"1. node --test verde com os tokens em styles.css","allowed_scope":["tests/**","styles.css"]}"#;
        let (state, backend) = recovery_state(
            &root,
            Script::RecoverFixes,
            vec![calls(&[("c1", "read_file", r#"{"path":"styles.css"}"#)]), text(revise)],
        );
        let events = run_until(&state, "integrated").await;
        assert_eq!(
            payloads(&events, "recovery_decided"),
            vec![json!({"spec_id": SPEC, "task_id": "T01", "cycle": 1, "decision": "REVISE_TASK"})]
        );
        let reqs = recovery_requests(&backend);
        assert!(
            reqs[1].messages.iter().any(|m| m.content.contains("SO_NA_WORKTREE")),
            "o modelo deve ler o arquivo da worktree"
        );
        let task = read_t01(&root);
        assert_eq!(task.frontmatter.status, TaskStatus::Todo);
        assert_eq!(task.frontmatter.attempts, 0);
        assert_eq!(task.frontmatter.allowed_scope, vec!["tests/**", "styles.css"]);
        assert!(task.body.contains("criar os tokens em styles.css"), "{}", task.body);
        assert!(task.body.contains("## Análise de recuperação #1"), "{}", task.body);
        assert!(task.body.contains("falta validar a entrada"), "findings preservados: {}", task.body);
    }

    /// Caso real (app-test-03): critério "Teste manual no navegador" que nenhum agente cumpre e
    /// finding do Revisor "verificação manual não executada". O Arquiteto (modelo falso) decide
    /// REVISE_TASK reescrevendo o critério como `(manual)`; a tarefa volta a executar.
    #[tokio::test]
    async fn recovery_rewrites_impossible_manual_criterion() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-manual", true);
        let mut task = read_t01(&root);
        task.body = format!("{}\n\n## Findings\n- F-01: verificação manual não executada\n", task.body);
        fs::write(t01_path(&root), task.render().expect("render")).expect("tarefa");
        let revise = r#"{"decision":"REVISE_TASK","root_cause":"nenhum agente tem navegador","strategy":"critério manual","objective":"Controles da página","acceptance":"1. node --test verde com DOM simulado\n2. (manual) abrir index.html no navegador e conferir os controles"}"#;
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(revise)]);
        let _ = run_until(&state, "integrated").await;
        let task = read_t01(&root);
        assert!(task.body.contains("(manual) abrir index.html"), "{}", task.body);
    }

    #[tokio::test]
    async fn recovery_invalid_revise_task_escalates() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-revise-bad", true);
        let bad = r#"{"decision":"REVISE_TASK","root_cause":"c","strategy":"s","objective":"o","acceptance":"a","allowed_scope":[".git/**"]}"#;
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(bad)]);
        run_until(&state, "blocked").await;
        let task = read_t01(&root);
        assert_eq!(task.frontmatter.status, TaskStatus::Blocked);
        assert!(task.body.contains(recovery::CAUSE_UNAVAILABLE), "{}", task.body);
        assert_eq!(task.frontmatter.allowed_scope, vec!["src/lib.rs"]);
    }

    #[tokio::test]
    async fn recovery_escalate_keeps_task_blocked_with_section() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-escalate", true);
        let json = r#"{"decision":"ESCALATE","root_cause":"requisito ambíguo","proposal":"SPLIT: dividir em duas tarefas"}"#;
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(json)]);
        let events = run_until(&state, "blocked").await;
        let decided = payloads(&events, "recovery_decided");
        assert_eq!(decided.len(), 1);
        assert_eq!(decided[0]["decision"], "ESCALATE");
        let task = read_t01(&root);
        assert_eq!(task.frontmatter.status, TaskStatus::Blocked);
        assert!(task.body.contains("## Análise de recuperação #1"));
        assert!(task.body.contains("Decisão: escalar ao humano"));
        assert!(task.body.contains("Proposta: SPLIT: dividir em duas tarefas"));
    }

    // ----- Recuperação com SPLIT, ADJUST e ASK (spec 124 T03), com o Arquiteto falso -----

    /// Tarefas da spec no Quadro (`GET /state`): `(id, status, depends_on)`.
    async fn board(state: &AppState) -> Vec<(String, String, Vec<String>)> {
        let (code, body) = call(state, request(Method::GET, &format!("/api/projects/{PROJECT}/state"), "", None)).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        let view = json_of(&body);
        let spec = view["specs"].as_array().and_then(|s| s.iter().find(|s| s["id"] == SPEC)).cloned().unwrap_or(Value::Null);
        spec["tasks"]
            .as_array()
            .map(|tasks| {
                tasks
                    .iter()
                    .map(|t| {
                        let deps = t["depends_on"].as_array().map(|d| d.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                        (t["id"].as_str().unwrap_or_default().to_string(), t["status"].as_str().unwrap_or_default().to_string(), deps)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn read_task_file(root: &Path, id: &str) -> Task {
        Task::parse(&fs::read_to_string(root.join(".sdd/specs").join(SPEC).join(format!("tasks/{id}.md"))).expect("tarefa")).expect("parse")
    }

    fn architect_text(events: &[ApiEvent]) -> String {
        payloads(events, "agent_message")
            .iter()
            .filter(|p| p["agent"] == "architect")
            .filter_map(|p| p["text"].as_str().map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// SPLIT: as tarefas novas vão para o `.sdd` e o Quadro, a T02 passa a depender delas, a T01 fica
    /// menor (mesma branch) e a execução segue no mesmo run. O Arquiteto recebeu os sinais.
    #[tokio::test]
    async fn recovery_split_writes_new_tasks_updates_the_board_and_continues() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-split", true);
        fs::write(root.join(".sdd/specs").join(SPEC).join("tasks/T02.md"), task_md("T02", "T01", "TODO")).expect("T02");
        let split = r#"{"decision":"SPLIT","root_cause":"a T01 mistura servidor e provas de ponta a ponta","strategy":"manter na branch a validação; as provas saem para a T03","objective":"Validação no servidor","acceptance":"- validação coberta por testes de unidade","new_tasks":[{"title":"Provas de ponta a ponta","objective":"Dois clientes convergem","acceptance":"- dois clientes convergem","allowed_scope":["tests/**"]}]}"#;
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text(split)]);
        let mut rx = state.events().subscribe();
        let events = run_until(&state, "integrated").await;
        assert_eq!(
            payloads(&events, "recovery_decided"),
            vec![json!({"spec_id": SPEC, "task_id": "T01", "cycle": 1, "decision": "SPLIT"})]
        );
        let t01 = read_t01(&root);
        assert_eq!(t01.frontmatter.status, TaskStatus::Todo);
        assert_eq!(t01.frontmatter.branch, format!("sdd/{SPEC}/T01"), "o trabalho da branch é aproveitado");
        assert!(t01.body.contains("- validação coberta por testes de unidade"), "{}", t01.body);
        assert!(t01.body.contains("- Tarefas novas: T03 (Provas de ponta a ponta)"), "{}", t01.body);
        let t03 = read_task_file(&root, "T03");
        assert_eq!((t03.frontmatter.status, t03.frontmatter.depends_on.clone()), (TaskStatus::Todo, vec!["T01".to_string()]));
        assert_eq!(t03.frontmatter.allowed_scope, vec!["tests/**"]);
        assert_eq!(read_task_file(&root, "T02").frontmatter.depends_on, vec!["T01", "T03"]);
        // O Quadro: a tarefa nova aparece com a dependência, e os eventos chegam ao barramento.
        let tasks = board(&state).await;
        assert!(tasks.contains(&("T03".into(), "TODO".into(), vec!["T01".into()])), "{tasks:?}");
        assert!(tasks.contains(&("T02".into(), "TODO".into(), vec!["T01".into(), "T03".into()])), "{tasks:?}");
        let updated: Vec<Value> = payloads(&events, "task_updated").into_iter().chain(payloads(&drain(&mut rx), "task_updated")).collect();
        assert!(updated.iter().any(|p| p["task_id"] == "T03" && p["status"] == "TODO"), "{updated:?}");
        assert!(architect_text(&events).contains("Análise de recuperação #1 (SPLIT)"), "{}", architect_text(&events));
        assert!(architect_text(&events).contains("Tarefas novas: T03"));
        // Spec 133 T11: a análise chega com `code` e `params` neutros, para a tela traduzir.
        let analysis = payloads(&events, "agent_message")
            .into_iter()
            .find(|p| p["agent"] == "architect" && p["code"] == crate::server::recovery::CODE_ANALYSIS)
            .expect("análise com código");
        assert_eq!(analysis["params"], json!({"cycle": 1, "decision": "SPLIT", "autonomous": 0}), "{analysis}");
        let facts = recovery_requests(&backend)[0].messages.iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n");
        assert!(facts.contains("## Sinais para a decisão") && facts.contains("Critérios de aceitação: 1"), "{facts}");
    }

    /// ADJUST: critérios ao que o ambiente prova, o apontamento vira verificação manual pelo caminho
    /// da arbitragem, a decisão fica no spec.md e a execução segue.
    #[tokio::test]
    async fn recovery_adjust_records_the_decision_on_the_spec_and_continues() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-adjust", true);
        let adjust = r#"{"decision":"ADJUST","root_cause":"o Docker não tem GPU","strategy":"provar a renderização por software","acceptance":"- renderização por software verificada no Docker","findings":[{"id":"F-01","decision":"manual","reason":"sem GPU no Docker"}]}"#;
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(adjust)]);
        let events = run_until(&state, "integrated").await;
        assert_eq!(payloads(&events, "recovery_decided")[0]["decision"], "ADJUST");
        let t01 = read_t01(&root);
        assert_eq!(t01.frontmatter.status, TaskStatus::Todo);
        let acceptance = crate::agents::runner::section_text(&t01.body, crate::agents::runner::ACCEPTANCE).unwrap_or_default();
        assert_eq!(acceptance.trim(), "- renderização por software verificada no Docker\n- (manual) F-01: falta validar a entrada (sem GPU no Docker)");
        assert!(t01.body.contains("- Apontamentos dispensados: F-01 [manual]"), "{}", t01.body);
        let spec = fs::read_to_string(root.join(".sdd/specs").join(SPEC).join("spec.md")).expect("spec");
        assert!(spec.contains("## Decisões de Arbitragem"), "{spec}");
        assert!(spec.contains("Recuperação #1, T01: critérios ajustados ao que o ambiente prova. Motivo: o Docker não tem GPU"), "{spec}");
        assert!(spec.contains("T01 F-01 (verificação manual)"), "{spec}");
    }

    /// ADJUST cujos critérios não cabem: nada é dispensado e a tarefa segue com a estratégia (RETRY).
    #[tokio::test]
    async fn recovery_adjust_that_cannot_be_written_goes_on_as_retry() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-adjust-bad", true);
        let big = "a".repeat(crate::agents::runner::MAX_REVISION_BYTES - 5);
        let adjust = format!(r#"{{"decision":"ADJUST","root_cause":"c","strategy":"seguir","acceptance":"{big}","findings":[{{"id":"F-01","decision":"manual","reason":"m"}}]}}"#);
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(&adjust)]);
        let events = run_until(&state, "integrated").await;
        assert_eq!(payloads(&events, "recovery_decided")[0]["decision"], "RETRY");
        let t01 = read_t01(&root);
        assert_eq!(t01.frontmatter.status, TaskStatus::Todo);
        assert!(t01.body.contains(crate::agents::recovery_plan::ADJUST_REFUSED_LINE), "{}", t01.body);
        assert!(t01.body.contains("falta validar a entrada"), "o apontamento não foi dispensado");
        assert!(!fs::read_to_string(root.join(".sdd/specs").join(SPEC).join("spec.md")).expect("spec").contains("Decisões de Arbitragem"));
    }

    /// ASK: a execução pausa com a pergunta no cartão (seção, mensagem, evento e opções no Quadro).
    #[tokio::test]
    async fn recovery_ask_pauses_with_the_question_card() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-ask", true);
        let ask = r#"{"decision":"ASK","root_cause":"o critério de capturas em 1920x1080 conflita com o tempo da bateria","question":"As capturas em 1920x1080 podem ficar como verificação manual?","options":[{"id":"manual","label":"Sim, manual","kind":"guidance","text":"capturas em 1920x1080 viram verificação manual"}]}"#;
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(ask)]);
        let events = run_until(&state, "blocked").await;
        let decided = payloads(&events, "recovery_decided");
        assert_eq!(decided.len(), 1);
        assert_eq!(decided[0]["decision"], "ASK");
        assert_eq!(decided[0]["options"][0]["id"], "manual");
        let t01 = read_t01(&root);
        assert_eq!(t01.frontmatter.status, TaskStatus::Blocked);
        assert!(t01.body.contains("- Decisão: pergunta ao humano (ASK)"), "{}", t01.body);
        assert!(t01.body.contains("- Pergunta ao humano: As capturas em 1920x1080 podem ficar como verificação manual?"));
        assert!(t01.body.contains("- Contexto:"), "o cartão traz o contexto");
        let message = architect_text(&events);
        assert!(message.contains("Análise de recuperação #1 (ASK)") && message.contains("Pergunta ao humano: As capturas"), "{message}");
        let (_, body) = call(&state, request(Method::GET, &format!("/api/projects/{PROJECT}/state"), "", None)).await;
        assert!(body.contains("\"recovery_options\"") && body.contains("Sim, manual"), "{body}");
    }

    #[tokio::test]
    async fn recovery_third_cycle_only_asks_the_model_for_options() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-limit", true);
        let (state, backend) = recovery_state(
            &root,
            Script::RecoverStuck,
            vec![text(RETRY_JSON), text(RETRY_JSON), text(RETRY_JSON)],
        );
        let events = run_until(&state, "blocked").await;
        assert_eq!(recovery_requests(&backend).len(), 3, "o 3º ciclo só pede opções (spec 074); RETRY não vale");
        let cycles: Vec<u64> = payloads(&events, "recovery_decided")
            .iter()
            .map(|p| p["cycle"].as_u64().unwrap_or(0))
            .collect();
        assert_eq!(cycles, vec![1, 2, 3]);
        let task = read_t01(&root);
        assert_eq!(task.frontmatter.status, TaskStatus::Blocked);
        assert_eq!(crate::agents::runner::recovery_cycles(&task.body), 2, "o limite é determinístico e não conta");
        assert!(task.body.contains(recovery::CAUSE_LIMIT), "{}", task.body);
    }


    const LIMIT_OPTIONS_JSON: &str = r#"{"decision":"ESCALATE","root_cause":"x","options":[{"id":"cfg","label":"Autossuficiente","kind":"config","test_command":"true && node --test --test-force-exit"},{"id":"gui","label":"Orientar","kind":"guidance","text":"mocke o relógio"}]}"#;

    /// Spec 074: o limite de ciclos faz UMA chamada ao modelo só para gerar opções; elas ficam
    /// validadas no cartão, a causa do limite e a linha "não conta" permanecem e nada é aplicado.
    #[tokio::test]
    async fn limit_escalation_asks_the_model_once_for_options() {
        use crate::agents::architect::testing::text;
        let root = setup("lim-opts", true);
        let (state, backend) = recovery_state(
            &root,
            Script::RecoverStuck,
            vec![text(RETRY_JSON), text(RETRY_JSON), text(LIMIT_OPTIONS_JSON)],
        );
        let events = run_until(&state, "blocked").await;
        let reqs = recovery_requests(&backend);
        assert_eq!(reqs.len(), 3);
        let last = reqs[2].messages.iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n");
        assert!(last.contains("Orçamento de recuperação esgotado") && last.contains("NÃO repita"), "{last}");
        assert!(last.contains("Análises de recuperação anteriores"), "{last}");
        let decided = payloads(&events, "recovery_decided");
        assert_eq!(decided[2]["decision"], "ESCALATE");
        let ids: Vec<&str> = decided[2]["options"]
            .as_array()
            .map(|a| a.iter().filter_map(|o| o["id"].as_str()).collect())
            .unwrap_or_default();
        assert_eq!(ids, ["cfg", "gui"], "{decided:?}");
        let task = read_t01(&root);
        assert_eq!(task.frontmatter.status, TaskStatus::Blocked);
        assert!(task.body.contains(recovery::CAUSE_LIMIT), "{}", task.body);
        assert!(task.body.contains(crate::agents::runner::NOT_COUNTED_LINE), "{}", task.body);
        assert_eq!(crate::agents::runner::recovery_cycles(&task.body), 2, "não conta como ciclo");
        assert!(!settings_text(&root).contains("force-exit"), "nada é aplicado sozinho");
    }

    /// Modelo indisponível na chamada de opções: escalada determinística de sempre, sem opções.
    #[tokio::test]
    async fn limit_escalation_without_options_stays_deterministic() {
        use crate::agents::architect::testing::text;
        let root = setup("lim-none", true);
        let (state, _backend) = recovery_state(
            &root,
            Script::RecoverStuck,
            vec![text(RETRY_JSON), text(RETRY_JSON), text("lixo")],
        );
        let events = run_until(&state, "blocked").await;
        let decided = payloads(&events, "recovery_decided");
        assert!(decided[2]["options"].as_array().is_none_or(Vec::is_empty), "{decided:?}");
        let task = read_t01(&root);
        assert!(task.body.contains(recovery::CAUSE_LIMIT), "{}", task.body);
        assert!(task.body.contains(crate::agents::runner::NOT_COUNTED_LINE), "{}", task.body);
        assert!(!task.body.contains("nants:options"), "{}", task.body);
    }

    /// Em YOLO, a chamada de opções do limite nunca aplica a opção `config` sozinha.
    #[tokio::test]
    async fn yolo_limit_escalation_never_applies_an_option() {
        use crate::agents::architect::testing::text;
        let root = setup("lim-yolo", true);
        enable_yolo(&root);
        let (state, backend) = recovery_state(
            &root,
            Script::RecoverStuck,
            vec![text(RETRY_JSON), text(RETRY_JSON), text(RETRY_JSON), text(RETRY_JSON), text(LIMIT_OPTIONS_JSON)],
        );
        let events = run_until(&state, "blocked").await;
        assert_eq!(recovery_requests(&backend).len(), 5);
        assert!(!settings_text(&root).contains("force-exit"), "{}", settings_text(&root));
        let decided = payloads(&events, "recovery_decided");
        assert_eq!(decided[4]["decision"], "ESCALATE");
        assert_eq!(decided[4]["options"].as_array().map(Vec::len), Some(2), "{decided:?}");
        let task = read_t01(&root);
        assert_eq!(task.body.lines().filter(|l| l.starts_with(recovery::AUTONOMOUS_ROUND_PREFIX)).count(), 2);
    }

    /// Spec 073: depois de "Retomar" (marca de retomada humana) a próxima falha volta a chamar o
    /// modelo, mesmo com 3 análises antigas no histórico.
    #[tokio::test]
    async fn recovery_after_human_resume_calls_the_model_again() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-resumed", true);
        let mut md = fs::read_to_string(t01_path(&root)).expect("T01");
        md.push_str("\n## Análise de recuperação #1\n- a\n\n## Análise de recuperação #2\n- b\n\n## Análise de recuperação #3\n- c\n");
        let md = crate::agents::runner::mark_human_resumed(&md);
        fs::write(t01_path(&root), md).expect("T01");
        let (state, backend) = recovery_state(&root, Script::RecoverStuck, vec![text(RETRY_JSON), text(RETRY_JSON), text(RETRY_JSON)]);
        let _ = run_until(&state, "blocked").await;
        assert_eq!(recovery_requests(&backend).len(), 3, "o orçamento zerado chama o modelo de novo (2 ciclos + opções do limite)");
    }

    // ---- Spec 073/T02: opções de recuperação e endpoint recovery-choice ----

    /// ESCALATE com opções de todos os tipos (o `echo ok` rebaixa a verificação e deve cair).
    const OPTIONS_JSON: &str = r#"{"decision":"ESCALATE","root_cause":"faltam dependências","proposal":"decidir o caminho","options":[
        {"id":"cfg","label":"Instalar dependências antes dos testes","kind":"config","test_command":"true && node --test --test-force-exit"},
        {"id":"fraca","label":"Só um eco","kind":"config","test_command":"echo ok"},
        {"id":"gui","label":"Orientar o Desenvolvedor","kind":"guidance","text":"mocke o relógio nos testes"},
        {"id":"env","label":"Preparar o ambiente docker","kind":"environment","text":"instale o node 22 no Dockerfile"}]}"#;
    const REVISE_OPTION_JSON: &str = r#"{"decision":"ESCALATE","root_cause":"escopo curto","proposal":"decidir","options":[
        {"id":"rev","label":"Ampliar o escopo","kind":"revise_task","objective":"novo objetivo","acceptance":"- novo critério","allowed_scope":["src/**","lib/**"]}]}"#;

    fn choice_req(task: &str, body: &str) -> Request<Body> {
        request(
            Method::POST,
            &format!("/api/projects/{PROJECT}/specs/{SPEC}/tasks/{task}/recovery-choice"),
            body,
            Some("application/json"),
        )
    }

    /// Fase 1: o Arquiteto escala com opções e a tarefa fica BLOCKED; o run termina.
    async fn blocked_with_options(tag: &str, reply: &str) -> (PathBuf, Vec<ApiEvent>) {
        use crate::agents::architect::testing::text;
        let root = setup(tag, true);
        git(&root, &["branch", "-M", "main"]);
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(reply)]);
        let events = run_until(&state, "blocked").await;
        assert!(wait_idle(&state).await);
        assert_eq!(read_t01(&root).frontmatter.status, TaskStatus::Blocked);
        (root, events)
    }

    /// Fase 2: novo servidor (o run retomado fica pendurado) para exercitar o endpoint.
    fn resume_state(root: &Path) -> AppState {
        state(root, Script::Hang).with_prereqs(prereqs(true))
    }

    async fn stop(state: &AppState) {
        state.trigger_shutdown();
        assert!(wait_idle(state).await);
    }

    #[tokio::test]
    async fn escalate_persists_validated_options_and_exposes_them() {
        let (root, events) = blocked_with_options("opt-expose", OPTIONS_JSON).await;
        let task = read_t01(&root);
        assert!(task.body.contains(crate::agents::runner::OPTIONS_OPEN), "{}", task.body);
        assert!(task.body.contains("- Opções descartadas: opção 2:") && task.body.contains(recovery::CAUSE_CONFIG_WEAKENS), "{}", task.body);
        let decided = payloads(&events, "recovery_decided");
        let ids: Vec<&str> = decided[0]["options"]
            .as_array()
            .map(|a| a.iter().filter_map(|o| o["id"].as_str()).collect())
            .unwrap_or_default();
        assert_eq!(ids, ["cfg", "gui", "env"], "{decided:?}");
        assert_eq!(decided[0]["options"][0]["kind"], "config");
        assert!(decided[0]["options"][0]["diff"].as_str().is_some_and(|d| d.contains("Antes:")));
        // O texto do diálogo traz o bloco legível pela UI.
        assert!(events.iter().any(|e| e.event_type() == "agent_message"
            && e.payload["text"].as_str().is_some_and(|t| t.contains("<!-- nants:options -->") && t.contains("[guidance] gui"))));
        // API de estado.
        let state = resume_state(&root);
        let (code, body) = call(&state, get_req(&format!("/api/projects/{PROJECT}/state"))).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        let v = json_of(&body);
        let options = &v["specs"][0]["tasks"][0]["recovery_options"];
        assert_eq!(options.as_array().map(Vec::len), Some(3), "{body}");
        assert_eq!(options[1]["label"], "Orientar o Desenvolvedor");
    }

    #[tokio::test]
    async fn recovery_choice_applies_each_kind_marks_human_resume_and_resumes() {
        let cases = [
            ("cfg", OPTIONS_JSON, "config"),
            ("gui", OPTIONS_JSON, "guidance"),
            ("env", OPTIONS_JSON, "environment"),
            ("rev", REVISE_OPTION_JSON, "revise_task"),
        ];
        for (id, reply, kind) in cases {
            let (root, _) = blocked_with_options(&format!("opt-choice-{id}"), reply).await;
            let state = resume_state(&root);
            let (code, body) = call(&state, choice_req("T01", &format!(r#"{{"option_id":"{id}","analysis":1}}"#))).await;
            assert_eq!(code, StatusCode::ACCEPTED, "{id}: {body}");
            let v = json_of(&body);
            assert_eq!((v["applied"].as_str(), v["task_id"].as_str()), (Some(kind), Some("T01")));
            assert!(state.runs().active(PROJECT).is_some(), "{id}: a spec foi retomada");
            let task = read_t01(&root);
            assert_eq!(task.frontmatter.status, TaskStatus::Todo, "{id}");
            assert_eq!(task.frontmatter.attempts, 0);
            assert!(task.body.contains("## Retomada pelo humano"), "{id}: marca de retomada");
            assert_eq!(crate::agents::runner::recovery_cycles(&task.body), 0, "{id}: orçamento zerado");
            assert!(task.body.contains("## Análise de recuperação #1"), "histórico preservado");
            let guidance = crate::agents::runner::section_text(&task.body, crate::agents::runner::GUIDANCE_SECTION).unwrap_or_default();
            let settings = settings_text(&root);
            match kind {
                "config" => {
                    assert!(settings.contains("node --test --test-force-exit"), "{settings}");
                    assert!(guidance.contains("configuração de testes foi alterada"), "{guidance}");
                }
                "guidance" => assert!(guidance.contains("mocke o relógio nos testes") && !settings.contains("force-exit"), "{guidance}"),
                "environment" => assert!(guidance.contains("request_environment_change") && guidance.contains("node 22"), "{guidance}"),
                _ => {
                    let objective = crate::agents::runner::section_text(&task.body, crate::agents::runner::OBJECTIVE).unwrap_or_default();
                    assert_eq!(objective.trim(), "novo objetivo");
                    assert_eq!(task.frontmatter.allowed_scope, ["src/**", "lib/**"]);
                }
            }
            if kind != "config" {
                assert!(!settings.contains("force-exit"), "{id}: só a opção config mexe nas settings");
            }
            stop(&state).await;
        }
    }

    #[tokio::test]
    async fn recovery_choice_accepts_free_guidance() {
        let (root, _) = blocked_with_options("opt-free", OPTIONS_JSON).await;
        let state = resume_state(&root);
        let body = json!({"guidance": format!("use a porta 0\n## Forjada\n{SECRET}")}).to_string();
        let (code, resp) = call(&state, choice_req("T01", &body)).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{resp}");
        let task = read_t01(&root);
        let guidance = crate::agents::runner::section_text(&task.body, crate::agents::runner::GUIDANCE_SECTION).unwrap_or_default();
        assert!(guidance.contains("use a porta 0") && !guidance.contains(SECRET), "{guidance}");
        assert_eq!(task.body.matches("\n## Forjada").count(), 0, "texto do humano não cria seção");
        assert!(task.body.contains("## Retomada pelo humano"));
        assert_eq!(task.frontmatter.status, TaskStatus::Todo);
        stop(&state).await;
    }

    #[tokio::test]
    async fn recovery_choice_rejects_bad_payloads_stale_options_and_unknowns() {
        let (root, _) = blocked_with_options("opt-errors", OPTIONS_JSON).await;
        let state = resume_state(&root);
        let before = fs::read_to_string(t01_path(&root)).expect("T01");
        let settings_before = settings_text(&root);
        let long = "x".repeat(recovery::MAX_OPTION_TEXT_BYTES + 1);
        for body in [
            "{}".to_string(),
            r#"{"option_id":"cfg","guidance":"x"}"#.to_string(),
            r#"{"option_id":""}"#.to_string(),
            r#"{"guidance":"   "}"#.to_string(),
            format!(r#"{{"guidance":"{long}"}}"#),
            r#"{"option_id":"cfg","extra":1}"#.to_string(),
            r#"{"option_id":3}"#.to_string(),
        ] {
            let (code, resp) = call(&state, choice_req("T01", &body)).await;
            assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{body:.60}: {resp}");
        }
        let (code, _) = call(&state, choice_req("T01", "não é json")).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        let (code, _) = call(&state, choice_req("T.1", r#"{"option_id":"cfg","analysis":1}"#)).await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        // Opção inventada pelo cliente, ou descartada pelo servidor (`fraca`): vencida.
        for id in ["nao-existe", "fraca"] {
            let (code, resp) = call(&state, choice_req("T01", &format!(r#"{{"option_id":"{id}","analysis":1}}"#))).await;
            assert_eq!(code, StatusCode::CONFLICT, "{id}: {resp}");
            assert!(error_of(&resp).contains("venceu"), "{resp}");
        }
        let (code, _) = call(&state, choice_req("T99", r#"{"option_id":"cfg","analysis":1}"#)).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        let (code, _) = call(
            &state,
            request(
                Method::POST,
                &format!("/api/projects/inexistente/specs/{SPEC}/tasks/T01/recovery-choice"),
                r#"{"option_id":"cfg","analysis":1}"#,
                Some("application/json"),
            ),
        )
        .await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(fs::read_to_string(t01_path(&root)).expect("T01"), before, "nada foi aplicado");
        assert_eq!(settings_text(&root), settings_before);
        // Retomada em andamento: segunda escolha é recusada (ocupado) e a tarefa já foi reaberta.
        let (code, resp) = call(&state, choice_req("T01", r#"{"option_id":"gui","analysis":1}"#)).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{resp}");
        let (code, _) = call(&state, choice_req("T01", r#"{"option_id":"gui","analysis":1}"#)).await;
        assert_eq!(code, StatusCode::CONFLICT);
        stop(&state).await;
        // Depois do run, a opção venceu pela marca de retomada (a tarefa está TODO, não bloqueada).
        let state = resume_state(&root);
        let (code, resp) = call(&state, choice_req("T01", r#"{"option_id":"gui","analysis":1}"#)).await;
        assert_eq!(code, StatusCode::CONFLICT, "{resp}");
        assert!(error_of(&resp).contains("não está bloqueada"), "{resp}");
    }

    /// Tarefa BLOCKED depois de uma retomada humana: as opções da análise antiga venceram.
    #[tokio::test]
    async fn recovery_choice_options_expire_after_human_resume() {
        let (root, _) = blocked_with_options("opt-expire", OPTIONS_JSON).await;
        let mut task = read_t01(&root);
        task.body = crate::agents::runner::mark_human_resumed(&task.body);
        fs::write(t01_path(&root), task.render().expect("render")).expect("T01");
        let state = resume_state(&root);
        let (code, resp) = call(&state, choice_req("T01", r#"{"option_id":"gui","analysis":1}"#)).await;
        assert_eq!(code, StatusCode::CONFLICT, "{resp}");
        let (_, body) = call(&state, get_req(&format!("/api/projects/{PROJECT}/state"))).await;
        assert!(json_of(&body)["specs"][0]["tasks"][0].get("recovery_options").is_none(), "{body}");
        // Texto livre continua possível.
        let (code, resp) = call(&state, choice_req("T01", r#"{"guidance":"tente de novo"}"#)).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{resp}");
        stop(&state).await;
    }

    /// F-01: retomada recusada (orçamento) desfaz o efeito: tarefa e settings intactas e a opção segue válida.
    #[tokio::test]
    async fn recovery_choice_is_atomic_when_resume_is_refused() {
        use crate::agents::llm_client::Usage;
        use crate::agents::usage::{UsageEvent, UsageSink};
        for id in ["cfg", "gui"] {
            let (root, _) = blocked_with_options(&format!("opt-atomic-{id}"), OPTIONS_JSON).await;
            super::super::usage::FileUsageSink::new(&root, PROJECT).record(UsageEvent {
                agent: "developer".into(),
                model: "m".into(),
                spec: Some(SPEC.into()),
                task: Some("T01".into()),
                usage: Usage::new(600, 500),
                web_search_calls: 0,
                images: 0,
                image_quality: None,
                estimated: false,
                cut: None,
                billing: Default::default(),
                context: None,
                call: Default::default(),
            });
            fs::write(root.join(format!(".sdd/specs/{SPEC}/budget.json")), r#"{"max_tokens":1000}"#).expect("budget");
            let (task_before, settings_before) = (fs::read_to_string(t01_path(&root)).expect("T01"), settings_text(&root));
            let state = resume_state(&root);
            let body = format!(r#"{{"option_id":"{id}","analysis":1}}"#);
            let (code, resp) = call(&state, choice_req("T01", &body)).await;
            assert_eq!(code, StatusCode::CONFLICT, "{id}: {resp}");
            assert_eq!(json_of(&resp)["reason"], REASON_BUDGET);
            assert_eq!(fs::read_to_string(t01_path(&root)).expect("T01"), task_before, "{id}: tarefa intacta");
            assert_eq!(settings_text(&root), settings_before, "{id}: settings intactas");
            // Repetir não acumula duplicatas e a opção continua utilizável depois de liberar o orçamento.
            let (code, _) = call(&state, choice_req("T01", &body)).await;
            assert_eq!(code, StatusCode::CONFLICT);
            assert_eq!(fs::read_to_string(t01_path(&root)).expect("T01"), task_before);
            let put = request(
                Method::PUT,
                &format!("/api/projects/{PROJECT}/specs/{SPEC}/budget"),
                r#"{"max_tokens":5000}"#,
                Some("application/json"),
            );
            assert_eq!(call(&state, put).await.0, StatusCode::OK);
            let (code, resp) = call(&state, choice_req("T01", &body)).await;
            assert_eq!(code, StatusCode::ACCEPTED, "{id}: {resp}");
            stop(&state).await;
        }
    }

    /// F-02: a escolha exige o número da análise; análise diferente da última é 409, sem número é 422.
    #[tokio::test]
    async fn recovery_choice_requires_the_current_analysis_number() {
        let (root, events) = blocked_with_options("opt-analysis", OPTIONS_JSON).await;
        let decided = payloads(&events, "recovery_decided");
        assert_eq!(decided[0]["options_analysis"], 1, "{decided:?}");
        let state = resume_state(&root);
        let (_, body) = call(&state, get_req(&format!("/api/projects/{PROJECT}/state"))).await;
        assert_eq!(json_of(&body)["specs"][0]["tasks"][0]["recovery_options_analysis"], 1, "{body}");
        let before = fs::read_to_string(t01_path(&root)).expect("T01");
        let (code, _) = call(&state, choice_req("T01", r#"{"option_id":"gui"}"#)).await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        let (code, _) = call(&state, choice_req("T01", r#"{"guidance":"x","analysis":1}"#)).await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        // Uma análise mais nova com o mesmo id: o número antigo não vale.
        let mut task = read_t01(&root);
        task.body = task.body.replace("## Análise de recuperação #1", "## Análise de recuperação #2");
        fs::write(t01_path(&root), task.render().expect("render")).expect("T01");
        let (code, resp) = call(&state, choice_req("T01", r#"{"option_id":"gui","analysis":1}"#)).await;
        assert_eq!(code, StatusCode::CONFLICT, "{resp}");
        let (code, _) = call(&state, choice_req("T01", r#"{"option_id":"gui","analysis":2}"#)).await;
        assert_eq!(code, StatusCode::ACCEPTED);
        stop(&state).await;
        let _ = before;
    }

    #[tokio::test]
    async fn recovery_choice_on_task_that_is_not_blocked_conflicts() {
        let root = setup("opt-todo", true);
        let state = state(&root, Script::Hang);
        let (code, resp) = call(&state, choice_req("T01", r#"{"guidance":"x"}"#)).await;
        assert_eq!(code, StatusCode::CONFLICT, "{resp}");
        assert!(error_of(&resp).contains("não está bloqueada"));
    }

    /// YOLO: ESCALATE com opção `config` válida é aplicada sozinha (sem escalar nem pedir aprovação),
    /// conta como rodada autônoma e a tarefa conclui.
    #[tokio::test]
    async fn yolo_applies_a_valid_config_option_instead_of_escalating() {
        use crate::agents::architect::testing::text;
        let root = setup("opt-yolo", true);
        enable_yolo(&root);
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text(OPTIONS_JSON)]);
        let events = run_until(&state, "integrated").await;
        assert_eq!(recovery_requests(&backend).len(), 1, "sem reforço: a opção config resolve");
        assert!(settings_text(&root).contains("node --test --test-force-exit"), "{}", settings_text(&root));
        assert!(payloads(&events, "recovery_decided").iter().all(|p| p["decision"] == "REVISE_CONFIG"));
        assert!(events.iter().all(|e| e.event_type() != "permission_requested"));
        let task = read_t01(&root);
        assert!(task.body.contains(recovery::AUTONOMOUS_ROUND_PREFIX) && !task.body.contains("nants:options"), "{}", task.body);
    }

    /// YOLO não contorna a guarda: opção `config` que rebaixa a verificação é descartada.
    #[tokio::test]
    async fn yolo_never_applies_a_config_option_that_weakens_verification() {
        use crate::agents::architect::testing::text;
        let root = setup("opt-yolo-weak", true);
        enable_yolo(&root);
        let weak = r#"{"decision":"ESCALATE","root_cause":"c","options":[{"id":"fraca","label":"Só eco","kind":"config","test_command":"echo ok"}]}"#;
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(weak), text(weak)]);
        let _ = run_until(&state, "integrated").await;
        assert!(!settings_text(&root).contains("echo ok"), "{}", settings_text(&root));
    }

    /// Sem YOLO, a opção `config` nunca é aplicada pela análise: só pela escolha do humano.
    #[tokio::test]
    async fn config_option_is_not_applied_without_human_choice() {
        let (root, _) = blocked_with_options("opt-no-auto", OPTIONS_JSON).await;
        assert!(!settings_text(&root).contains("force-exit"));
    }

    fn enable_yolo(root: &Path) {
        let path = root.join(".sdd/settings.yaml");
        let mut text = fs::read_to_string(&path).expect("settings");
        text.push_str("yolo: true\n");
        fs::write(&path, text).expect("yolo");
    }

    /// YOLO: depois dos 2 ciclos normais há até 2 rodadas autônomas (contador no corpo da tarefa);
    /// a 5ª análise escala normalmente por limite de ciclos, sem chamar o modelo.
    #[tokio::test]
    async fn yolo_adds_two_autonomous_rounds_then_escalates() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-yolo", true);
        enable_yolo(&root);
        let (state, backend) = recovery_state(
            &root,
            Script::RecoverStuck,
            vec![text(RETRY_JSON), text(RETRY_JSON), text(RETRY_JSON), text(RETRY_JSON), text(RETRY_JSON)],
        );
        let events = run_until(&state, "blocked").await;
        assert_eq!(recovery_requests(&backend).len(), 5, "2 ciclos + 2 rodadas autônomas + 1 chamada só para opções");
        let cycles: Vec<u64> = payloads(&events, "recovery_decided")
            .iter()
            .map(|p| p["cycle"].as_u64().unwrap_or(0))
            .collect();
        assert_eq!(cycles, vec![1, 2, 3, 4, 5]);
        let task = read_t01(&root);
        assert_eq!(task.frontmatter.status, TaskStatus::Blocked);
        let rounds = task.body.lines().filter(|l| l.starts_with(recovery::AUTONOMOUS_ROUND_PREFIX)).count();
        assert_eq!(rounds, 2, "{}", task.body);
        assert!(task.body.contains(recovery::CAUSE_LIMIT), "{}", task.body);
        assert_eq!(recovery_requests(&backend).len(), 5, "a 5ª análise só pede opções ao modelo");
        assert!(events
            .iter()
            .any(|e| e.event_type() == "agent_message" && e.payload["text"].as_str().is_some_and(|t| t.contains("[rodada autônoma]"))));
    }

    /// YOLO: a instrução autônoma vale desde o 1º ciclo; ESCALATE gera uma 2ª pergunta reforçada.
    #[tokio::test]
    async fn recovery_yolo_cycle_one_is_autonomous_and_escalate_is_asked_twice() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-yolo-esc", true);
        enable_yolo(&root);
        let esc = r#"{"decision":"ESCALATE","root_cause":"trocar Plug.Cowboy ou aceitar o risco"}"#;
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text(esc), text(RETRY_JSON)]);
        run_until(&state, "integrated").await;
        // Só os pedidos da recuperação: o relatório final e o ARCHITECTURE.md, pedidos ao mesmo
        // backend depois do `integrated`, chegavam às vezes antes da leitura (ver `recovery_requests`).
        let reqs = recovery_requests(&backend);
        let dump = |r: &crate::agents::llm_client::ChatRequest| {
            r.messages.iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n")
        };
        let dumps: Vec<String> = reqs.iter().map(dump).collect();
        assert_eq!(dumps.len(), 2, "exatamente dois pedidos de recuperação: {dumps:?}");
        let autonomous = dumps.iter().position(|d| d.contains("Aprovação automática: autonomia"));
        let reinforced = dumps.iter().position(|d| d.contains("ESCALATE NÃO É PERMITIDO"));
        assert_eq!((autonomous, reinforced), (Some(0), Some(1)), "{dumps:?}");
    }

    /// YOLO (C-02): ESCALATE persistente vira RETRY com a estratégia sugerida + premissa registrada,
    /// contado como rodada autônoma; o run segue e integra.
    #[tokio::test]
    async fn recovery_yolo_persistent_escalate_becomes_autonomous_retry() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-yolo-esc2", true);
        enable_yolo(&root);
        let esc = r#"{"decision":"ESCALATE","root_cause":"decisão de produto","strategy":"usar a biblioteca atual","proposal":"SPLIT: dividir"}"#;
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text(esc), text(esc)]);
        let events = run_until(&state, "integrated").await;
        assert_eq!(recovery_requests(&backend).len(), 2);
        let task = read_t01(&root);
        assert!(!task.body.contains("Decisão: escalar ao humano"), "{}", task.body);
        assert!(task.body.contains("premissa registrada"), "{}", task.body);
        assert!(task.body.contains("Estratégia: usar a biblioteca atual; SPLIT: dividir"), "{}", task.body);
        assert_eq!(task.body.lines().filter(|l| l.starts_with(recovery::AUTONOMOUS_ROUND_PREFIX)).count(), 1);
        assert_eq!(payloads(&events, "recovery_decided")[0]["decision"], "RETRY");
    }

    /// YOLO: sem rodadas autônomas sobrando o ESCALATE persistente escala normalmente (teto respeitado).
    #[tokio::test]
    async fn recovery_yolo_escalate_respects_autonomous_round_ceiling() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-yolo-esc-ceiling", true);
        enable_yolo(&root);
        let mut task = read_t01(&root);
        task.body = format!(
            "{}\n\n{}1 de 2\n{}2 de 2\n",
            task.body,
            recovery::AUTONOMOUS_ROUND_PREFIX,
            recovery::AUTONOMOUS_ROUND_PREFIX
        );
        fs::write(t01_path(&root), task.render().expect("render")).expect("tarefa");
        let esc = r#"{"decision":"ESCALATE","root_cause":"decisão de produto"}"#;
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(esc), text(esc)]);
        run_until(&state, "blocked").await;
        assert!(read_t01(&root).body.contains("Decisão: escalar ao humano"));
    }

    /// YOLO (C-02): modelo indisponível tem 3 tentativas antes de escalar; sem YOLO, 2.
    #[tokio::test]
    async fn recovery_yolo_unavailable_model_gets_a_third_attempt() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-yolo-unavail", true);
        enable_yolo(&root);
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text("x"), text("y"), text(RETRY_JSON)]);
        run_until(&state, "integrated").await;
        assert_eq!(recovery_requests(&backend).len(), 3);
        let root = setup("rec-yolo-unavail2", true);
        enable_yolo(&root);
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text("x"), text("y"), text("z")]);
        run_until(&state, "blocked").await;
        assert_eq!(recovery_requests(&backend).len(), 3);
        assert!(read_t01(&root).body.contains(recovery::CAUSE_UNAVAILABLE));
    }

    /// YOLO (C-04): falha de build do compose ganha 1 rodada RETRY sem modelo, com nota para
    /// `request_environment_change`; a segunda falha segue para o diagnóstico (escala).
    #[tokio::test]
    async fn yolo_compose_failure_gets_one_environment_fix_round() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-env-fix", true);
        enable_yolo(&root);
        let (state, backend) = recovery_state(&root, Script::EnvComposeFixes, vec![]);
        let events = run_until(&state, "integrated").await;
        assert!(recovery_requests(&backend).is_empty(), "sem modelo");
        let task = read_t01(&root);
        assert!(task.body.contains("request_environment_change"), "{}", task.body);
        assert!(task.body.contains(recovery::ENV_FIX_PREFIX), "{}", task.body);
        assert_eq!(payloads(&events, "recovery_decided")[0]["decision"], "RETRY");

        let root = setup("rec-env-fix2", true);
        enable_yolo(&root);
        let (state, backend) = recovery_state(&root, Script::EnvComposeStuck, vec![text(DIAG_JSON)]);
        run_until(&state, "blocked").await;
        let task = read_t01(&root);
        assert_eq!(task.body.lines().filter(|l| l.starts_with(recovery::ENV_FIX_PREFIX)).count(), 1, "{}", task.body);
        assert_eq!(recovery_requests(&backend).len(), 1, "só o diagnóstico depois da segunda falha");
        assert!(task.body.contains("## Diagnóstico de ambiente #1"), "{}", task.body);
    }

    /// Sem YOLO, a falha de build do compose não é recuperada (comportamento anterior).
    #[tokio::test]
    async fn compose_failure_without_yolo_is_not_retried() {
        let root = setup("rec-env-nofix", true);
        let (state, _backend) = recovery_state(&root, Script::EnvComposeStuck, vec![]);
        let events = run_until(&state, "blocked").await;
        assert!(payloads(&events, "recovery_started").is_empty());
    }

    /// Padrão: ESCALATE do modelo é respeitado com uma só pergunta e sem a instrução YOLO.
    #[tokio::test]
    async fn recovery_standard_escalate_asks_once_without_yolo_note() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-std-esc", true);
        let esc = r#"{"decision":"ESCALATE","root_cause":"decisão de produto"}"#;
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text(esc), text(RETRY_JSON)]);
        run_until(&state, "blocked").await;
        let reqs = recovery_requests(&backend);
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0].messages.iter().all(|m| !m.content.contains("## Aprovação automática")));
    }

    /// Modelo indisponível/JSON inválido: uma nova tentativa antes de escalar (também sem YOLO).
    #[tokio::test]
    async fn recovery_unavailable_model_is_retried_once() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-retry-once", true);
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text("não é JSON"), text(RETRY_JSON)]);
        run_until(&state, "integrated").await;
        assert_eq!(recovery_requests(&backend).len(), 2);
        let root = setup("rec-retry-twice", true);
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text("lixo"), text("lixo")]);
        run_until(&state, "blocked").await;
        assert_eq!(recovery_requests(&backend).len(), 2);
        assert!(read_t01(&root).body.contains(recovery::CAUSE_UNAVAILABLE));
    }

    /// YOLO: falha de ambiente (comando de testes não executou) passa pelo modelo; a config proposta
    /// (que mantém o comando original) é aplicada sozinha e conta como rodada autônoma.
    #[tokio::test]
    async fn yolo_environmental_block_applies_proposed_config() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-yolo-env", true);
        enable_yolo(&root);
        fs::write(root.with_file_name("evidence.txt"), "[falha dos testes: código 127]\nsh: tsc: not found").expect("evidência");
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text(ENV_CONFIG_JSON)]);
        let _ = run_until(&state, "integrated").await;
        let reqs = recovery_requests(&backend);
        assert_eq!(reqs.len(), 1, "o modelo é consultado");
        let facts = reqs[0].messages.iter().map(|m| m.content.clone()).collect::<String>();
        assert!(facts.contains("Falha de ambiente") && facts.contains("tsc: not found") && facts.contains("código: 127"), "{facts}");
        assert!(settings_text(&root).contains("npm ci && true"), "{}", settings_text(&root));
        assert!(read_t01(&root).body.contains(recovery::AUTONOMOUS_ROUND_PREFIX));
    }

    const ENV_CONFIG_JSON: &str = r#"{"decision":"REVISE_CONFIG","root_cause":"faltam dependências na worktree nova","test_command":"npm ci && true"}"#;
    const WEAKEN_JSON: &str = r#"{"decision":"REVISE_CONFIG","root_cause":"tirar o teste","test_command":"echo ok"}"#;

    /// Padrão: a proposta vira decisão do humano (antes/depois); nada é gravado antes da aprovação.
    #[tokio::test]
    async fn environmental_config_without_yolo_goes_to_human() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-env-hitl", true);
        fs::write(root.with_file_name("evidence.txt"), "[falha dos testes: código 127]\nsh: tsc: not found").expect("evidência");
        let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text(ENV_CONFIG_JSON)]);
        let (code, _) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(code, StatusCode::ACCEPTED);
        let pending = wait_pending(&state).await;
        assert_eq!(pending.len(), 1);
        let diff = pending[0].diff.clone().unwrap_or_default();
        assert!(diff.contains("Antes:") && diff.contains("npm ci && true"), "{diff}");
        assert!(!settings_text(&root).contains("npm ci"));
        assert_eq!(recovery_requests(&backend).len(), 1);
    }

    /// Proposta que tira o comando original é recusada pelo servidor e vira escalada explicada.
    #[tokio::test]
    async fn environmental_weakening_config_is_refused_and_escalates() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-env-weaken", true);
        enable_yolo(&root);
        fs::write(root.with_file_name("evidence.txt"), "[falha dos testes: código 127]\nsh: tsc: not found").expect("evidência");
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(WEAKEN_JSON)]);
        let _ = run_until(&state, "blocked").await;
        assert!(!settings_text(&root).contains("echo ok"));
        let task = read_t01(&root);
        assert!(task.body.contains(recovery::CAUSE_CONFIG_WEAKENS), "{}", task.body);
        assert!(task.body.contains("- Contexto:"), "{}", task.body);
    }

    /// Escalada de ambiente sempre traz o contexto estruturado (comando, código, erro, tentativas, opções).
    #[tokio::test]
    async fn environmental_escalate_carries_structured_context() {
        use crate::agents::architect::testing::text;
        for (tag, evidence, code) in [
            ("rec-127", "[falha dos testes: código 127]\nsh: 1: cargo-xyz: command not found", "código: 127"),
            ("rec-timeout", "[falha dos testes: tempo limite]\n[tempo limite de 600s excedido]", "código: tempo limite"),
        ] {
            let root = setup(tag, true);
            fs::write(root.with_file_name("evidence.txt"), evidence).expect("evidência");
            let esc = r#"{"decision":"ESCALATE","root_cause":"o host não tem a ferramenta","proposal":"decidir o ambiente"}"#;
            let (state, backend) = recovery_state(&root, Script::RecoverFixes, vec![text(esc)]);
            let _ = run_until(&state, "blocked").await;
            assert_eq!(recovery_requests(&backend).len(), 1, "{tag}: passa pelo modelo");
            let task = read_t01(&root);
            assert_eq!(task.frontmatter.status, TaskStatus::Blocked, "{tag}");
            for needle in ["- Contexto:", "comando: true", code, "trecho do erro:", "o que foi tentado:", "por que não resolveu: o host não tem a ferramenta", "opções:"] {
                assert!(task.body.contains(needle), "{tag}: falta {needle}: {}", task.body);
            }
        }
    }

    const CONFIG_JSON: &str = r#"{"decision":"REVISE_CONFIG","root_cause":"o test_command invoca docker dentro do workspace","test_command":"true && node --test --test-force-exit"}"#;

    fn settings_text(root: &Path) -> String {
        fs::read_to_string(root.join(".sdd/settings.yaml")).expect("settings")
    }

    async fn wait_pending(state: &AppState) -> Vec<crate::server::approvals::PendingView> {
        let mut pending = Vec::new();
        for _ in 0..1000 {
            pending = state.approvals().list_pending(PROJECT);
            if !pending.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        pending
    }

    // ---- Spec 074/T02: migração host -> docker pela opção `environment` com `files` ----

    const ENV_FILES_JSON: &str = r#"{"decision":"ESCALATE","root_cause":"falta a biblioteca do navegador na imagem","proposal":"ajustar","options":[
        {"id":"mig","label":"Ajustar a imagem do Docker","kind":"environment","text":"navegador na imagem","files":[
            {"path":".nants/compose.yaml","content":"services:\n  workspace:\n    image: node:22\n"}]}]}"#;
    const COMPOSE_V1: &str = "services:\n  workspace:\n    image: node:20\n";
    const COMPOSE_V2: &str = "services:\n  workspace:\n    image: node:22\n";

    fn files_of(content: &str) -> Vec<(String, String)> {
        vec![(".nants/compose.yaml".to_string(), content.to_string())]
    }

    /// Hook pre-commit que sempre falha: o commit da adoção do ambiente é recusado.
    fn break_commits(root: &Path) {
        let hook = root.join(".git/hooks/pre-commit");
        fs::create_dir_all(hook.parent().expect("hooks")).expect("hooks");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").expect("hook");
        let mut perms = fs::metadata(&hook).expect("meta").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&hook, perms).expect("chmod");
    }

    /// Escolhe a opção `mig` e responde o pedido de aprovação do ambiente com `decision`.
    async fn choose_mig(state: &AppState, decision: &str) -> (StatusCode, String, Value) {
        let call_state = state.clone();
        let handle = tokio::spawn(async move {
            call(&call_state, choice_req("T01", r#"{"option_id":"mig","analysis":1}"#)).await
        });
        let pending = wait_pending(state).await;
        assert_eq!(pending.len(), 1, "pedido de aprovação do ambiente esperado");
        assert_eq!(pending[0].kind, Some("environment"));
        let view = json!({"diff": pending[0].diff, "reason": pending[0].reason});
        let uri = format!("/api/projects/{PROJECT}/permissions/{}", pending[0].request_id);
        let (code, body) = call(
            state,
            request(Method::POST, &uri, &json!({"decision": decision}).to_string(), Some("application/json")),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{body}");
        let (code, body) = handle.await.expect("escolha");
        (code, body, view)
    }

    #[test]
    fn adopt_environment_files_commits_marks_docker_and_swaps_the_hash() {
        let root = setup_plain("env-adopt", true);
        let settings = adopt_environment_files(&root, SPEC, &files_of(COMPOSE_V1), Lang::PtBr).expect("adoção");
        assert_eq!(settings.environment, crate::sdd::Environment::Docker);
        let h1 = crate::sdd::environment::env_hash(&root).expect("hash");
        assert!(settings.approved_hashes().contains(&h1));
        assert_eq!(git_out(&root, &["log", "-1", "--format=%s"]).trim(), format!("chore({SPEC}): ambiente aprovado (Nants)"));
        assert_eq!(git_out(&root, &["show", "--name-only", "--format=", "HEAD"]).trim(), ".nants/compose.yaml");
        assert!(settings_text(&root).contains("environment: docker"));
        // Projeto já docker: a opção com `files` troca o hash aprovado.
        let settings = adopt_environment_files(&root, SPEC, &files_of(COMPOSE_V2), Lang::PtBr).expect("troca");
        let h2 = crate::sdd::environment::env_hash(&root).expect("hash 2");
        assert_ne!(h1, h2);
        assert_eq!(settings.environment_hash.as_deref(), Some(h2.as_str()));
        assert!(settings.approved_hashes().contains(&h2));
        assert!(git_out(&root, &["status", "--porcelain", "--", ".nants"]).trim().is_empty());
    }

    #[test]
    fn adopt_environment_files_restores_everything_when_the_commit_fails() {
        let root = setup_plain("env-adopt-fail", true);
        break_commits(&root);
        let (settings_before, head_before) = (settings_text(&root), git_out(&root, &["rev-parse", "HEAD"]));
        // Sem .nants antes: nada pode sobrar.
        assert!(adopt_environment_files(&root, SPEC, &files_of(COMPOSE_V1), Lang::PtBr).is_err());
        assert_eq!(settings_text(&root), settings_before);
        assert!(!root.join(".nants").exists(), ".nants criado pela tentativa foi removido");
        assert_eq!(git_out(&root, &["rev-parse", "HEAD"]), head_before);
        assert!(git_out(&root, &["status", "--porcelain", "--", ".nants"]).trim().is_empty());
        // Com arquivo anterior commitado: volta ao conteúdo antigo.
        fs::remove_file(root.join(".git/hooks/pre-commit")).expect("sem hook");
        adopt_environment_files(&root, SPEC, &files_of(COMPOSE_V1), Lang::PtBr).expect("adoção");
        break_commits(&root);
        let (settings_before, head_before) = (settings_text(&root), git_out(&root, &["rev-parse", "HEAD"]));
        assert!(adopt_environment_files(&root, SPEC, &files_of(COMPOSE_V2), Lang::PtBr).is_err());
        assert_eq!(fs::read_to_string(root.join(".nants/compose.yaml")).expect("compose"), COMPOSE_V1);
        assert_eq!(settings_text(&root), settings_before);
        assert_eq!(git_out(&root, &["rev-parse", "HEAD"]), head_before);
        assert!(git_out(&root, &["status", "--porcelain", "--", ".nants"]).trim().is_empty());
    }

    #[tokio::test]
    async fn recovery_choice_environment_files_asks_approval_then_adopts_docker() {
        let (root, _) = blocked_with_options("env-human", ENV_FILES_JSON).await;
        let state = resume_state(&root);
        let (code, body, view) = choose_mig(&state, "once").await;
        assert_eq!(code, StatusCode::ACCEPTED, "{body}");
        assert_eq!(json_of(&body)["applied"], "environment");
        assert!(view["diff"].as_str().is_some_and(|d| d.contains("+    image: node:22")), "{view}");
        let settings = read_settings_checked(&root).expect("settings");
        assert_eq!(settings.environment, crate::sdd::Environment::Docker);
        let hash = crate::sdd::environment::env_hash(&root).expect("hash");
        assert!(settings.approved_hashes().contains(&hash));
        assert_eq!(git_out(&root, &["log", "-1", "--format=%s"]).trim(), format!("chore({SPEC}): ambiente aprovado (Nants)"));
        let task = read_t01(&root);
        let guidance = crate::agents::runner::section_text(&task.body, crate::agents::runner::GUIDANCE_SECTION).unwrap_or_default();
        assert!(guidance.contains("o servidor aplicou o ajuste aprovado do ambiente docker"), "{guidance}");
        assert!(state.runs().active(PROJECT).is_some(), "a spec foi retomada pelo launch (modo docker)");
        assert!(builds_of(&root).last().copied().unwrap_or(false), "o runner retomado usa o ambiente ajustado");
        stop(&state).await;
    }

    #[tokio::test]
    async fn recovery_choice_environment_files_denied_changes_nothing() {
        let (root, _) = blocked_with_options("env-denied", ENV_FILES_JSON).await;
        let state = resume_state(&root);
        let (settings_before, task_before) = (settings_text(&root), fs::read_to_string(t01_path(&root)).expect("T01"));
        let head = git_out(&root, &["rev-parse", "HEAD"]);
        let (code, body, _) = choose_mig(&state, "deny").await;
        assert_eq!(code, StatusCode::CONFLICT, "{body}");
        assert_eq!(settings_text(&root), settings_before);
        assert_eq!(fs::read_to_string(t01_path(&root)).expect("T01"), task_before);
        assert!(!built_adjusted(&root), "o ambiente não foi alterado");
        assert_eq!(git_out(&root, &["rev-parse", "HEAD"]), head);
        assert!(state.runs().active(PROJECT).is_none());
    }

    #[tokio::test]
    async fn recovery_choice_environment_files_commit_failure_restores() {
        let (root, _) = blocked_with_options("env-commit-fail", ENV_FILES_JSON).await;
        break_commits(&root);
        let state = resume_state(&root);
        let (settings_before, task_before) = (settings_text(&root), fs::read_to_string(t01_path(&root)).expect("T01"));
        let (code, body, _) = choose_mig(&state, "once").await;
        assert_eq!(code, StatusCode::CONFLICT, "{body}");
        assert_eq!(settings_text(&root), settings_before, "nada foi adotado");
        assert_eq!(fs::read_to_string(t01_path(&root)).expect("T01"), task_before);
        assert!(!built_adjusted(&root), "o ambiente não foi alterado");
        assert!(state.runs().active(PROJECT).is_none());
    }

    /// YOLO: a recuperação aprova o ajuste do ambiente docker sozinha, adota, encerra o run e o relança.
    #[tokio::test]
    async fn yolo_environment_files_adjusts_docker_and_relaunches() {
        use crate::agents::architect::testing::text;
        let root = setup("env-yolo", true);
        enable_yolo(&root);
        let (state, backend) = recovery_state(&root, Script::RecoverAdjustsEnv, vec![text(ENV_FILES_JSON)]);
        let events = run_until(&state, "integrated").await;
        assert_eq!(recovery_requests(&backend).len(), 1, "a opção com files não é reforçada");
        let settings = read_settings_checked(&root).expect("settings");
        assert_eq!(settings.environment, crate::sdd::Environment::Docker);
        let hash = crate::sdd::environment::env_hash(&root).expect("hash");
        assert!(settings.approved_hashes().contains(&hash));
        assert_eq!(git_out(&root, &["log", "-1", "--format=%s"]).trim(), format!("chore({SPEC}): ambiente aprovado (Nants)"));
        assert_eq!(builds_of(&root), vec![false, true], "run encerrado e relançado com o ambiente ajustado");
        assert!(events.iter().all(|e| e.event_type() != "permission_requested"));
        assert!(events.iter().any(|e| e.event_type() == "agent_message"
            && e.payload["text"].as_str().is_some_and(|t| t.starts_with(crate::server::approvals::YOLO_AUTO_PREFIX))));
        let task = read_t01(&root);
        assert!(task.body.contains("Ambiente docker ajustado"), "{}", task.body);
        assert!(task.body.contains(&format!("{}1 de", recovery::AUTONOMOUS_ROUND_PREFIX)), "{}", task.body);
        assert_eq!(payloads(&events, "recovery_decided")[0]["decision"], "MIGRATE_ENVIRONMENT");
    }

    /// YOLO com o Docker indisponível depois do launch (ex.: daemon parou): a opção é descartada, o
    /// ambiente não é alterado e o run não é relançado.
    #[tokio::test]
    async fn yolo_environment_files_without_docker_is_discarded() {
        use crate::agents::architect::testing::text;
        use crate::server::prereqs::{CmdOutput, PrereqChecker};
        let root = setup("env-yolo-nodocker", true);
        enable_yolo(&root);
        let (state, _backend) =
            recovery_state(&root, Script::RecoverAdjustsEnv, vec![text(ENV_FILES_JSON), text(ENV_FILES_JSON)]);
        // A prontidão (conferência na hora, spec 093 T02) e o launch veem o Docker (4 comandos do
        // diagnóstico cada); depois deles, o Docker "some".
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let checker = PrereqChecker::new(Arc::new(move |program: &str, _args: &[&str]| {
            let seen = calls.fetch_add(1, Ordering::SeqCst);
            (program == "git" || seen < 8)
                .then(|| CmdOutput { success: true, stdout: "ok".into(), stderr: String::new() })
        }))
        .with_ttl(Duration::ZERO);
        let state = state.with_prereqs(checker);
        let _ = run_until(&state, "blocked").await;
        assert!(!built_adjusted(&root), "o ambiente não foi alterado");
        assert_eq!(
            read_settings_checked(&root).expect("settings").environment,
            crate::sdd::Environment::Docker
        );
        assert_eq!(builds_of(&root).len(), 1);
    }

    /// YOLO: REVISE_CONFIG é validada, aplicada pelo servidor sem pedir ao humano, conta como rodada
    /// autônoma, aparece como "aprovado automaticamente (YOLO)" e a tarefa retoma.
    #[tokio::test]
    async fn revise_config_in_yolo_applies_automatically_and_resumes() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-config-yolo", true);
        enable_yolo(&root);
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(CONFIG_JSON)]);
        let events = run_until(&state, "integrated").await;
        assert!(settings_text(&root).contains("node --test --test-force-exit"), "{}", settings_text(&root));
        assert_eq!(
            payloads(&events, "recovery_decided"),
            vec![json!({"spec_id": SPEC, "task_id": "T01", "cycle": 1, "decision": "REVISE_CONFIG"})]
        );
        assert!(events.iter().any(|e| e.event_type() == "agent_message"
            && e.payload["text"].as_str().is_some_and(|t| t.starts_with(crate::server::approvals::YOLO_AUTO_PREFIX))
            && e.payload["kind"] == "config"));
        assert!(events.iter().all(|e| e.event_type() != "permission_requested"));
        let task = read_t01(&root);
        assert!(task.body.contains("REVISE_CONFIG"), "{}", task.body);
        assert!(task.body.contains(recovery::AUTONOMOUS_ROUND_PREFIX), "{}", task.body);
    }

    /// Sem YOLO: pedido HITL (antes/depois) e só aplica depois da aprovação do humano.
    #[tokio::test]
    async fn revise_config_without_yolo_waits_for_human_approval() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-config-hitl", true);
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(CONFIG_JSON)]);
        let mut rx = state.events().subscribe();
        let (code, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{body}");
        let pending = wait_pending(&state).await;
        assert_eq!(pending.len(), 1, "pedido HITL esperado");
        assert_eq!(pending[0].kind, Some("environment"));
        let diff = pending[0].diff.clone().unwrap_or_default();
        assert!(diff.contains("Antes:") && diff.contains("Depois:") && diff.contains("node --test --test-force-exit"), "{diff}");
        assert!(!settings_text(&root).contains("--test-force-exit"), "não aplica antes da aprovação");
        let uri = format!("/api/projects/{PROJECT}/permissions/{}", pending[0].request_id);
        let (code, body) = call(&state, request(Method::POST, &uri, r#"{"decision":"once"}"#, Some("application/json"))).await;
        assert_eq!(code, StatusCode::OK, "{body}");
        let events = collect_until(&mut rx, "integrated").await;
        assert!(settings_text(&root).contains("node --test --test-force-exit"));
        assert!(payloads(&events, "recovery_decided").iter().any(|p| p["decision"] == "REVISE_CONFIG"));
    }

    /// Humano nega: nada é gravado e a tarefa escala.
    #[tokio::test]
    async fn revise_config_denied_escalates_without_writing() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-config-deny", true);
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(CONFIG_JSON)]);
        let mut rx = state.events().subscribe();
        let (code, _) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(code, StatusCode::ACCEPTED);
        let pending = wait_pending(&state).await;
        let uri = format!("/api/projects/{PROJECT}/permissions/{}", pending[0].request_id);
        let (code, _) = call(&state, request(Method::POST, &uri, r#"{"decision":"deny"}"#, Some("application/json"))).await;
        assert_eq!(code, StatusCode::OK);
        let _ = collect_until(&mut rx, "blocked").await;
        assert!(!settings_text(&root).contains("--test-force-exit"));
        assert!(read_t01(&root).body.contains(recovery::CAUSE_CONFIG_DENIED));
    }

    /// F-01: em YOLO, comando que o YOLO exclui (sudo/su; no container só as exclusões duras) não é
    /// auto-aprovado: segue para o humano e nada é gravado antes.
    #[tokio::test]
    async fn revise_config_in_yolo_with_excluded_command_goes_to_human() {
        use crate::agents::architect::testing::text;
        for (i, cmd) in ["true && sudo npm test", "true && su root", "npm ci && true && sudo make"].iter().enumerate() {
            let root = setup(&format!("rec-config-yolo-excl{i}"), true);
            enable_yolo(&root);
            let json = format!(r#"{{"decision":"REVISE_CONFIG","root_cause":"x","test_command":"{cmd}"}}"#);
            let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(&json)]);
            let before = settings_text(&root);
            let (code, _) = call(&state, approve_req(PROJECT, SPEC, "")).await;
            assert_eq!(code, StatusCode::ACCEPTED);
            let pending = wait_pending(&state).await;
            assert_eq!(pending.len(), 1, "{cmd}: deve pedir ao humano");
            assert_eq!(settings_text(&root), before, "{cmd}");
        }
    }

    #[tokio::test]
    async fn revise_config_in_yolo_with_safe_chain_is_automatic() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-config-yolo-safe", true);
        enable_yolo(&root);
        let json = r#"{"decision":"REVISE_CONFIG","root_cause":"x","test_command":"npm ci && true"}"#;
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(json)]);
        let events = run_until(&state, "integrated").await;
        assert!(settings_text(&root).contains("npm ci && true"));
        assert!(events.iter().all(|e| e.event_type() != "permission_requested"));
    }

    /// Proposta inválida (aqui: check com nome inválido) é rejeitada pelo servidor e escala.
    #[tokio::test]
    async fn revise_config_invalid_proposal_escalates() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-config-bad", true);
        enable_yolo(&root);
        let bad = r#"{"decision":"REVISE_CONFIG","root_cause":"x","checks":[{"name":"Nome Ruim","command":"npm test","blocking":true}]}"#;
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(bad)]);
        let before = settings_text(&root);
        let _ = run_until(&state, "blocked").await;
        assert_eq!(settings_text(&root), before);
        let task = read_t01(&root);
        assert_eq!(task.frontmatter.status, TaskStatus::Blocked);
        assert!(task.body.contains(recovery::CAUSE_CONFIG_INVALID), "{}", task.body);
    }

    /// Migração com o settings real do app-test-04: desembrulha, grava, é idempotente.
    #[test]
    fn docker_settings_migration_unwraps_real_app_test_04_idempotently() {
        let root = temp_dir("migrate-docker").join("repo");
        fs::create_dir_all(root.join(".sdd")).expect("sdd");
        fs::write(root.join(".sdd/settings.yaml"), crate::sdd::docker_command::APP_TEST_04_SETTINGS).expect("settings");
        let (settings, changed) = migrate_docker_settings(&root).expect("migração");
        assert!(changed);
        let settings = settings.expect("docker");
        assert_eq!(settings.test_command, "npm ci && npm test");
        assert_eq!(settings.checks[1].command, "npm ci && npm run build");
        let on_disk = settings_text(&root);
        assert!(!on_disk.contains("docker compose"), "{on_disk}");
        assert!(on_disk.contains("environment: docker"));
        let (again, changed) = migrate_docker_settings(&root).expect("2ª migração");
        assert!(!changed && again.is_some());
        assert_eq!(settings_text(&root), on_disk);
        // Host: nunca migra.
        fs::write(root.join(".sdd/settings.yaml"), SETTINGS_PLAIN).expect("host");
        assert_eq!(migrate_docker_settings(&root).expect("host"), (None, false));
    }

    #[tokio::test]
    async fn recovery_invalid_json_escalates_as_unavailable() {
        use crate::agents::architect::testing::text;
        let cases = [
            ("rec-bad-json", "isto não é JSON"),
            ("rec-bad-enum", r#"{"decision":"SPLIT","root_cause":"x"}"#),
            ("rec-no-strategy", r#"{"decision":"RETRY","root_cause":"x"}"#),
        ];
        for (tag, reply) in cases {
            let root = setup(tag, true);
            let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(reply)]);
            let _ = run_until(&state, "blocked").await;
            let task = read_t01(&root);
            assert_eq!(task.frontmatter.status, TaskStatus::Blocked, "{tag}");
            assert!(task.body.contains(recovery::CAUSE_UNAVAILABLE), "{tag}: {}", task.body);
            assert!(task.body.contains("Decisão: escalar ao humano"), "{tag}");
        }
    }

    #[tokio::test]
    async fn recovery_model_failure_escalates_as_unavailable() {
        let root = setup("rec-fail", true);
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![]);
        let _ = run_until(&state, "blocked").await;
        assert!(read_t01(&root).body.contains(recovery::CAUSE_UNAVAILABLE));
    }

    const DIAG_JSON: &str = r#"{"root_cause":"nada a integrar","what_to_do":"marcar a tarefa como concluída"}"#;

    #[tokio::test]
    async fn environmental_block_gets_a_diagnosis_without_recovery_cycle() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-env", true);
        git(&root, &["branch", "-M", "main"]);
        let (state, backend) = recovery_state(&root, Script::EnvBlock, vec![text(DIAG_JSON), text(DIAG_JSON)]);
        let events = run_until(&state, "blocked").await;
        assert_eq!(recovery_requests(&backend).len(), 1);
        assert!(payloads(&events, "recovery_started").is_empty());
        let task = read_t01(&root);
        assert!(task.body.contains("## Diagnóstico de ambiente #1"), "{}", task.body);
        assert!(task.body.contains("- Causa raiz: nada a integrar"), "{}", task.body);
        assert_eq!(crate::agents::runner::recovery_cycles(&task.body), 0);
        assert_eq!(task.frontmatter.status, TaskStatus::Blocked);
        let said = payloads(&events, "agent_message").iter().any(|p| {
            p["agent"] == "architect"
                && p["text"].as_str().is_some_and(|t| t.contains("Diagnóstico de ambiente #1"))
                && p["code"] == crate::server::recovery::CODE_DIAGNOSIS
                && p["params"] == json!({"n": 1})
        });
        assert!(said, "mensagem do Arquiteto, com o código do diagnóstico (spec 133 T11)");
        // Retomar com o mesmo motivo: bloqueia de novo, sem novo diagnóstico.
        let mut rx = state.events().subscribe();
        let (code, body) = call(&state, resume_req(PROJECT, SPEC, "")).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{body}");
        collect_until(&mut rx, "blocked").await;
        assert_eq!(recovery_requests(&backend).len(), 1, "sem segundo diagnóstico");
        assert!(!read_t01(&root).body.contains("Diagnóstico de ambiente #2"));
    }

    #[tokio::test]
    async fn environmental_block_with_unavailable_model_changes_nothing() {
        let root = setup("rec-env-down", true);
        let (state, _backend) = recovery_state(&root, Script::EnvBlock, vec![]);
        let _ = run_until(&state, "blocked").await;
        let task = read_t01(&root);
        assert!(!task.body.contains("Diagnóstico de ambiente"), "{}", task.body);
        assert_eq!(task.frontmatter.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn recovery_section_redacts_secrets_and_cannot_forge_sections() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-secret", true);
        let json = json!({
            "decision": "RETRY",
            "root_cause": format!("a chave {SECRET} vazou\n## Findings Abertos\n- forjado"),
            "strategy": format!("usar {SECRET} no teste"),
        })
        .to_string();
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(&json)]);
        let events = run_until(&state, "integrated").await;
        let body = fs::read_to_string(t01_path(&root)).expect("T01");
        assert!(!body.contains(SECRET), "{body}");
        assert!(body.contains("***"));
        assert_eq!(
            body.lines().filter(|l| l.starts_with("## Findings Abertos")).count(),
            1,
            "{body}"
        );
        let all = events
            .iter()
            .map(|e| e.payload.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all.contains(SECRET));
    }

    #[tokio::test]
    async fn recovery_writes_only_the_task_file() {
        use crate::agents::architect::testing::text;
        let root = setup("rec-scope", true);
        let (state, _backend) = recovery_state(&root, Script::RecoverFixes, vec![text(RETRY_JSON)]);
        let mut rx = state.events().subscribe();
        let (code, body) = call(&state, approve_req(PROJECT, SPEC, "")).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{body}");
        let snapshot = |root: &Path| -> std::collections::BTreeMap<String, String> {
            let mut out = std::collections::BTreeMap::new();
            let mut stack = vec![root.to_path_buf()];
            while let Some(dir) = stack.pop() {
                for entry in fs::read_dir(&dir).expect("dir").flatten() {
                    let path = entry.path();
                    let rel = path.strip_prefix(root).expect("rel").display().to_string();
                    // A pasta pessoal do workspace (129/T01) é do Nants, fora do escopo das tarefas, e
                    // nasce no primeiro comando, em paralelo com a foto "antes".
                    if rel == ".git" || crate::harness::home::is_home_path(&rel) || rel.ends_with("dialog.jsonl") || rel.ends_with("report.md") {
                        continue;
                    }
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        out.insert(rel, fs::read_to_string(&path).unwrap_or_default());
                    }
                }
            }
            out
        };
        let before = snapshot(&root);
        collect_until(&mut rx, "integrated").await;
        let after = snapshot(&root);
        let spec_md = format!(".sdd/specs/{SPEC}/spec.md");
        let settings = ".sdd/settings.yaml".to_string();
        assert_eq!(before.get(&spec_md), after.get(&spec_md), "spec intocada");
        assert_eq!(before.get(&settings), after.get(&settings), "settings intocado");
        let task_rel = format!(".sdd/specs/{SPEC}/tasks/T01.md");
        for (path, content) in &after {
            if path != &task_rel {
                assert_eq!(before.get(path), Some(content), "escrita fora da tarefa: {path}");
            }
        }
        assert_eq!(before.len(), after.len(), "arquivos criados/removidos");
    }
}
