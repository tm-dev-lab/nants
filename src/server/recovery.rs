//! Recuperação de tarefas bloqueadas por qualidade (spec 018): o Arquiteto analisa a causa raiz e
//! decide RETRY (estratégia nova, o MESMO run continua) ou ESCALATE (o humano decide).
//!
//! Tudo acontece dentro do run (mesma reserva do projeto). O turno do modelo é somente leitura
//! (`ToolHost::report_only`, sem `run_command` nem escrita): o servidor monta os FATOS, valida o
//! JSON da resposta e é quem registra a seção `## Análise de recuperação #n` no corpo da tarefa,
//! pelo mesmo caminho de escrita do runner (`SpecRunner::record_recovery`). O Arquiteto não altera
//! spec, escopo, settings, `test_command` nem permissões, e não aprova nem integra: o Revisor segue
//! independente. Ambiental nunca chama o modelo (o chamador só aciona esta rotina para `Quality`).
//!
//! Limites: no máximo [`MAX_CYCLES`] análises por tarefa (contadas pelas seções no corpo); depois,
//! ESCALATE sem chamar o modelo. Falha, timeout, pânico ou JSON inválido: ESCALATE com causa fixa.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;

use super::approvals::ProjectApprover;
use super::chat::{architect_client, load_policy};
use super::dialog::{deliver, DialogTarget};
use super::events::{ApiEvent, EventBus, EventType, SecretSet};
use super::projects::scrub_text;
use super::runs::SpecRunner;
use super::safe_fs::{self, MAX_READ_BYTES};
use super::state::AppState;
use crate::server::events::BusActivitySink;
use crate::agents::architect::{truncate_bytes, LlmArchitect};
use crate::agents::chat::ChatBackend;
use crate::agents::recovery_plan::{self as plan, CriteriaAdjustment, SplitTask, TaskSplit};
use crate::agents::runner::{
    add_human_guidance, apply_revision, diagnosis_notes, latest_options_block, recovery_cycles, recovery_notes,
    section_text, task_worktree_path, TaskRevision, ACCEPTANCE, FINDINGS, MAX_REVISION_BYTES, OBJECTIVE, OPTIONS_CLOSE,
    OPTIONS_OPEN, SCOPE_ADDED_LINE, TEST_FAILURE_MARK,
};
use super::approvals::{replace_atomic, PERSIST_LOCK};
use crate::harness::allowlist::{self, Policy};
use crate::harness::tools::ApprovalDecision;
use crate::sdd::docker_command;
use crate::sdd::{ArbitrationDecision, Check, Environment, Settings, Task, TaskStatus};

const SETTINGS_REL: &str = ".sdd/settings.yaml";
/// Causa fixa quando a proposta REVISE_CONFIG é inválida.
pub const CAUSE_CONFIG_INVALID: &str = "proposta de configuração de testes inválida";
/// Causa fixa quando o humano nega (ou deixa expirar) a correção da configuração de testes.
pub const CAUSE_CONFIG_DENIED: &str = "correção da configuração de testes não aprovada";
/// Causa fixa quando a correção aprovada não pôde ser gravada.
pub const CAUSE_CONFIG_SAVE: &str = "falha ao gravar a configuração de testes";
/// Linha persistida no corpo da tarefa quando a configuração foi corrigida.
const CONFIG_APPLIED_LINE: &str = "- Configuração de testes corrigida e aplicada pelo servidor";

/// Análises de recuperação por tarefa antes do ESCALATE obrigatório.
pub const MAX_CYCLES: usize = 2;
/// Rodadas autônomas (YOLO) por tarefa depois do limite de ciclos; passou disso, escala normal.
pub const MAX_AUTONOMOUS_ROUNDS: usize = 2;
/// Linha persistida no corpo da tarefa que conta cada rodada autônoma aplicada.
pub const AUTONOMOUS_ROUND_PREFIX: &str = "- Rodada autônoma (aprovação automática) #";
/// Prefixo antigo (antes da 095/T08), ainda contado nas tarefas já gravadas.
const LEGACY_AUTONOMOUS_ROUND_PREFIX: &str = "- Rodada autônoma (YOLO) #";
/// Instrução extra ao modelo na rodada autônoma.
const AUTONOMOUS_NOTE: &str = "\n## Aprovação automática: rodada autônoma\nO limite de ciclos foi atingido, mas o humano pediu autonomia. NÃO escale: responda RETRY com estratégia realmente diferente, SPLIT (divida a tarefa), ADJUST (critérios ao que o ambiente prova) ou REVISE_TASK (reescreva objetivo/critérios; um critério inviável de automatizar pode virar verificação manual, mas NUNCA tire testes do `test_command` nem afrouxe critérios globais).\n";
/// Instrução ao modelo em YOLO desde o 1º ciclo (tarefa não ambiental).
const YOLO_NOTE: &str = "\n## Aprovação automática: autonomia\nO humano ligou a aprovação automática: NÃO escale nem pergunte (ESCALATE ou ASK) por decisão de produto ou técnica (trocar dependência, aceitar risco, ambiguidade). Decida você, registre a premissa adotada na causa raiz e responda RETRY (estratégia nova), SPLIT, ADJUST ou REVISE_TASK.\n";
/// Reforço quando o modelo escalou mesmo em YOLO.
const YOLO_REINFORCED_NOTE: &str = "\n## Aprovação automática: ESCALATE NÃO É PERMITIDO\nSua resposta anterior foi ESCALATE ou ASK, mas o humano pediu autonomia total. Responda OBRIGATORIAMENTE RETRY, SPLIT, ADJUST ou REVISE_TASK: escolha a opção mais razoável, registre a premissa em `root_cause` e siga em frente.\n";
/// Linha persistida no corpo da tarefa que conta a correção de ambiente tentada em YOLO (C-04).
pub const ENV_FIX_PREFIX: &str = "- Correção de ambiente (aprovação automática) #";
/// Prefixo antigo (antes da 095/T08), ainda contado nas tarefas já gravadas.
const LEGACY_ENV_FIX_PREFIX: &str = "- Correção de ambiente (YOLO) #";
/// Linhas do corpo da tarefa com o prefixo atual ou o antigo (antes da 095/T08, com "YOLO").
fn count_marked_lines(body: &str, prefix: &str, legacy: &str) -> usize {
    body.lines().filter(|l| l.starts_with(prefix) || l.starts_with(legacy)).count()
}
/// Cauda da última saída de testes enviada ao modelo.
pub const MAX_TEST_TAIL_BYTES: usize = 6 * 1024;
/// Limite do diff da branch contra a base enviado ao modelo.
pub const MAX_DIFF_BYTES: usize = 12 * 1024;
/// Limite de cada campo da resposta do modelo.
pub const MAX_FIELD_BYTES: usize = 4 * 1024;
const MAX_FILES: usize = 50;
const MAX_SECTION_BYTES: usize = 4 * 1024;
const MAX_FACTS_BYTES: usize = 40 * 1024;
/// Agente do diálogo.
const ARCHITECT_AGENT: &str = "architect";

/// Causas fixas de ESCALATE sem decisão do modelo.
///
/// As `CAUSE_*` são marcadores estruturais (spec 115 T04): o servidor as compara (`starts_with`,
/// contagem de ciclos, escalada determinística) e a web as reconhece pelo texto em pt-BR e as mostra
/// no idioma ativo (`localizeRecoveryCause` em `web/src/recoveryAnalysis.ts`, na análise da Conversa
/// e no cartão de recuperação). Por isso ficam fixas, iguais em todos os idiomas.
pub const CAUSE_UNAVAILABLE: &str = "análise indisponível";
pub const CAUSE_ENVIRONMENT: &str = "ambiente: o comando de testes não executou";
/// Falha de ambiente em que o modelo propôs reescrever a tarefa (rebaixaria a verificação).
pub const CAUSE_ENV_REVISE: &str =
    "ambiente: reescrever a tarefa não resolve falha de ambiente (a verificação não pode ser rebaixada)";
/// Proposta de configuração que removeria etapa de verificação (guarda contra rebaixamento).
pub const CAUSE_CONFIG_WEAKENS: &str = "a proposta removeria verificação";
/// Proposta de `test_command` que repete o comando anterior ou uma etapa (spec 079 T04).
pub const CAUSE_CONFIG_REPEATS: &str = "a proposta repetiria o comando de testes";
/// Falha de ambiente em YOLO sem rodadas autônomas restantes para aplicar nova configuração.
pub const CAUSE_ENV_ROUNDS: &str = "ambiente: limite de rodadas autônomas atingido sem resolver a falha";
/// Cauda da saída de testes mostrada na análise de ambiente e no contexto da escalada.
const ENV_TAIL_BYTES: usize = 2 * 1024;
pub const CAUSE_LIMIT: &str = "limite de ciclos de recuperação atingido";

/// O que o run faz depois da análise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Tarefa reaberta com estratégia nova: o run continua.
    Retry,
    /// Ambiente docker ajustado (spec 074): a tarefa foi reaberta, mas o run atual (com a imagem
    /// antiga) deve terminar e ser relançado pelo launch, que prepara o ambiente novo.
    Relaunch,
    /// Tarefa continua BLOCKED: o run termina como antes.
    Stay,
    /// Shutdown durante a análise.
    Shutdown,
}

/// Decisão validada. REVISE_TASK tem `retry` e `revise` (a tarefa volta a executar já corrigida).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub retry: bool,
    pub revise: Option<TaskRevision>,
    pub root_cause: String,
    pub strategy: Option<String>,
    pub proposal: Option<String>,
    /// REVISE_CONFIG: correção proposta de `test_command`/`checks` (o servidor valida e grava).
    pub config: Option<ConfigProposal>,
    /// ESCALATE: opções propostas pelo modelo (após [`validate_options`], só as válidas).
    pub options: Vec<RecoveryOption>,
    /// Motivos das opções descartadas (formato, validação, guarda); só para o registro.
    pub discarded: Vec<String>,
    /// YOLO: opção `environment` com `files` escolhida para migrar o projeto para docker (spec 074).
    /// Só o servidor a preenche (nunca vem do modelo); [`apply_migration_decision`] a aplica.
    pub migrate: Option<RecoveryOption>,
    /// `scope_add` da estratégia (RETRY, REVISE_TASK, REVISE_CONFIG; spec 079): caminhos a acrescentar
    /// ao `allowed_scope` na mesma decisão. Validados ao registrar ([`section_body`]).
    pub scope_add: Vec<String>,
    /// SPLIT, ADJUST ou ASK (spec 124 T03); `None` nas decisões de sempre.
    pub change: Option<Change>,
}

/// Decisões da spec 124 T03 além de RETRY, REVISE_TASK, REVISE_CONFIG e ESCALATE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A tarefa fica menor e as tarefas novas recebem o restante (`retry`).
    Split(TaskSplit),
    /// Critérios ajustados ao que o ambiente prova; o restante vira verificação manual (`retry`).
    Adjust(CriteriaAdjustment),
    /// Pergunta objetiva ao humano: a execução pausa (como ESCALATE, sem `retry`).
    Ask(String),
}

impl Change {
    /// Nome da decisão, o mesmo do JSON e do cabeçalho da análise.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Split(_) => "SPLIT",
            Self::Adjust(_) => "ADJUST",
            Self::Ask(_) => "ASK",
        }
    }
}

/// Instrução da chamada que só gera opções quando o orçamento de recuperação acabou.
const LIMIT_NOTE: &str = "\n## Orçamento de recuperação esgotado\n\
O limite de tentativas autônomas foi atingido. Responda SOMENTE com a decisão ESCALATE e, em `options`, de 1 a 4 \
caminhos CONCRETOS e EXECUTÁVEIS neste modo de projeto para o humano escolher com um clique. NÃO repita nenhuma \
estratégia das análises anteriores (acima): elas falharam. Respeite as ferramentas reais do modo do projeto.\n";

/// Máximo de opções por análise.
pub const MAX_OPTIONS: usize = 4;
const MAX_OPTION_ID: usize = 32;
const MAX_OPTION_LABEL_CHARS: usize = 80;
/// Limite do texto livre de uma opção ou da orientação do humano.
pub const MAX_OPTION_TEXT_BYTES: usize = 2048;

/// Tipo de uma opção de recuperação (spec 073).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionKind {
    Config,
    Environment,
    Guidance,
    ReviseTask,
}

impl OptionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Environment => "environment",
            Self::Guidance => "guidance",
            Self::ReviseTask => "revise_task",
        }
    }
}

/// Opção de recuperação oferecida ao humano num ESCALATE. É a mesma forma recebida do modelo, gravada
/// no corpo da tarefa e exposta à UI; a escolha referencia só o `id`. `diff` (antes/depois) é
/// preenchido pelo servidor nas opções `config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryOption {
    pub id: String,
    pub label: String,
    pub kind: OptionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<Check>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_scope: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Arquivos `.nants/*` propostos por uma opção `environment` (spec 074). Aqui só são aceitos e
    /// persistidos; a validação do conteúdo e a aplicação ficam para a T02.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<EnvFile>>,
}

/// Arquivo de ambiente proposto numa opção `environment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvFile {
    pub path: String,
    pub content: String,
}

/// Máximo de arquivos de ambiente por opção.
const MAX_OPTION_FILES: usize = 8;
/// Limite de bytes do conteúdo de um arquivo de ambiente proposto (cabe no corpo da tarefa).
const MAX_OPTION_FILE_BYTES: usize = 16 * 1024;
/// Limite somado do conteúdo de todos os arquivos de uma opção.
const MAX_OPTION_FILES_TOTAL_BYTES: usize = 48 * 1024;
/// Limite de bytes do caminho de um arquivo proposto.
const MAX_OPTION_FILE_PATH_BYTES: usize = 200;

/// Correção proposta da configuração de testes. Ao menos um campo presente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProposal {
    pub test_command: Option<String>,
    pub checks: Option<Vec<Check>>,
}

impl Decision {
    fn escalate(cause: &str) -> Self {
        Self {
            retry: false,
            revise: None,
            root_cause: cause.to_string(),
            strategy: None,
            proposal: None,
            config: None,
            options: Vec::new(),
            discarded: Vec::new(),
            migrate: None,
            scope_add: Vec::new(),
            change: None,
        }
    }
}

/// O que o run sabe sobre o projeto.
pub struct Scope<'a> {
    pub state: &'a AppState,
    pub project_id: &'a str,
    pub spec_id: &'a str,
    pub root: &'a Path,
    pub secrets: &'a Arc<SecretSet>,
    pub events: &'a EventBus,
    pub dialog: &'a DialogTarget,
}

/// Tarefa lida do disco, com o que a decisão precisa.
struct Loaded {
    facts: String,
    /// `allowed_scope` atual da tarefa (base da ampliação da decisão, spec 094 T07).
    allowed_scope: Vec<String>,
    cycles: usize,
    environmental: bool,
    /// Projeto docker cujo comando de testes invoca docker: habilita só REVISE_CONFIG.
    config_suspect: bool,
    /// Modo YOLO ligado nas settings do projeto.
    yolo: bool,
    /// Rodadas autônomas já aplicadas a esta tarefa.
    rounds: usize,
    /// Correções de ambiente (YOLO) já tentadas nesta tarefa.
    env_fixes: usize,
    /// Comando de testes das settings (para o contexto da escalada).
    command: String,
    /// Código de saída (ou "tempo limite") lido do cabeçalho da evidência.
    exit_code: String,
    /// Cauda (até [`ENV_TAIL_BYTES`], redigida) da saída de testes.
    error_tail: String,
    /// Bloco extra enviado ao modelo quando a falha é de ambiente (vazio nos demais casos).
    env_note: String,
}

enum Stop {
    Shutdown,
    Unavailable,
}

/// Analisa uma tarefa bloqueada por qualidade e registra o resultado. Nunca falha para o chamador:
/// qualquer problema resulta em [`Next::Stay`] (BLOCKED, como antes).
///
/// `env_reason` é o motivo do bloqueio quando ele é ambiental (`None` para qualidade): em YOLO, falha
/// de build/up do compose (ou motivo citando `.nants/*`) ganha uma rodada RETRY para o Desenvolvedor
/// corrigir via `request_environment_change`; a segunda falha segue para o diagnóstico.
pub async fn recover(
    scope: &Scope<'_>,
    runner: &Arc<dyn SpecRunner>,
    task_id: &str,
    env_reason: Option<&str>,
    shutdown: &mut watch::Receiver<bool>,
) -> Next {
    if *shutdown.borrow() {
        return Next::Shutdown;
    }
    let (root, spec_id, tid) = (scope.root.to_path_buf(), scope.spec_id.to_string(), task_id.to_string());
    let secrets = Arc::clone(scope.secrets);
    let loaded = tokio::task::spawn_blocking(move || load(&root, &spec_id, &tid, &secrets))
        .await
        .ok()
        .flatten();
    // Sem tarefa legível não há onde registrar: BLOCKED como antes, sem evento.
    let Some(loaded) = loaded else {
        return Next::Stay;
    };
    // Ambiental sem config suspeita só entra aqui pela correção de ambiente em YOLO (C-04).
    let env_fix = env_reason.filter(|r| {
        loaded.yolo
            && !loaded.config_suspect
            && is_env_fixable(r)
            && loaded.env_fixes == 0
            && loaded.rounds < MAX_AUTONOMOUS_ROUNDS
    });
    if env_reason.is_some() && env_fix.is_none() && !loaded.config_suspect {
        return Next::Stay;
    }
    let cycle = loaded.cycles + 1;
    scope
        .events
        .publish(cycle_event(scope, EventType::RecoveryStarted, task_id, cycle, None, &[], None));

    let autonomous = loaded.yolo
        && loaded.cycles >= MAX_CYCLES
        && !loaded.environmental
        && loaded.rounds < MAX_AUTONOMOUS_ROUNDS;
    let mut forced = false;
    let decision = if let Some(reason) = env_fix {
        forced = true;
        env_fix_decision(&scrub_text(scope.secrets, reason))
    } else if loaded.cycles >= MAX_CYCLES && !autonomous {
        match limit_escalation(scope, &loaded, task_id, shutdown, CAUSE_LIMIT).await {
            Ok(decision) => decision,
            Err(Stop::Shutdown) => return Next::Shutdown,
            Err(Stop::Unavailable) => Decision::escalate(CAUSE_LIMIT),
        }
    } else {
        // Falha de ambiente também passa pela análise do Arquiteto (nunca escalada cega); o YOLO
        // "premissa" não se aplica: a saída é RETRY com estratégia, REVISE_CONFIG ou ESCALATE com contexto.
        let yolo_mode = loaded.yolo && !loaded.environmental;
        let facts = format!("{}{}", loaded.facts, loaded.env_note);
        match decide(scope, &facts, task_id, shutdown, yolo_mode, autonomous).await {
            Ok(decision) => {
                // Opções do ESCALATE: validadas pelo servidor; em YOLO uma opção `config` válida é
                // aplicada sozinha (spec 073), dentro do limite de rodadas.
                let decision = prepare_options(scope, &loaded, decision).await;
                // ESCALATE final em YOLO (modelo): segue a estratégia sugerida como rodada autônoma,
                // enquanto houver rodadas (C-02).
                if yolo_mode && loaded.rounds < MAX_AUTONOMOUS_ROUNDS && is_escalate(&decision) {
                    forced = true;
                    premise_retry(scope.state.project_lang(scope.project_id).get(), decision)
                } else {
                    decision
                }
            }
            Err(Stop::Shutdown) => return Next::Shutdown,
            Err(Stop::Unavailable) => Decision::escalate(CAUSE_UNAVAILABLE),
        }
    };

    // Ambiental: REVISE_TASK escala (não se resolve limitação de ambiente mexendo na tarefa).
    let decision = restrict_environmental(loaded.environmental, decision);
    // YOLO aplica a nova configuração sozinho, mas só dentro do limite de rodadas autônomas.
    let decision = if loaded.environmental
        && loaded.yolo
        && decision.config.is_some()
        && loaded.rounds >= MAX_AUTONOMOUS_ROUNDS
    {
        match limit_escalation(scope, &loaded, task_id, shutdown, CAUSE_ENV_ROUNDS).await {
            Ok(decision) => decision,
            Err(Stop::Shutdown) => return Next::Shutdown,
            Err(Stop::Unavailable) => Decision::escalate(CAUSE_ENV_ROUNDS),
        }
    } else {
        decision
    };
    let decision = if decision.config.is_some() {
        match apply_config_decision(scope, task_id, decision, shutdown).await {
            Ok(decision) => decision,
            Err(Stop::Shutdown) => return Next::Shutdown,
            Err(Stop::Unavailable) => Decision::escalate(CAUSE_UNAVAILABLE),
        }
    } else {
        decision
    };
    // YOLO: a opção `environment` com `files` ajusta o ambiente docker (aprovação automática).
    let migrated = decision.migrate.clone();
    let decision = if migrated.is_some() {
        match apply_migration_decision(scope, task_id, decision).await {
            Ok(decision) => decision,
            Err(Stop::Shutdown) => return Next::Shutdown,
            Err(Stop::Unavailable) => Decision::escalate(CAUSE_UNAVAILABLE),
        }
    } else {
        decision
    };
    let migrated = migrated.filter(|_| decision.retry && decision.migrate.is_some());
    let mut text = section_body(scope.secrets, &decision);
    // Spec 129 T05: REVISE_CONFIG aplicada que repete a instalação: o aviso da spec 126, com a regra,
    // entra na própria análise (o Arquiteto o vê junto da decisão) e o comando fica como aprovado.
    if let (true, Some(config)) = (decision.retry, &decision.config) {
        let lang = scope.state.project_lang(scope.project_id).get();
        for line in repeated_install_warnings(scope.root, config, lang) {
            text.push_str(&format!("\n- {}", clean(scope.secrets, &line)));
        }
    }
    if decision.retry {
        // Spec 094 T07: a linha "Escopo ampliado" mostra só o que vai entrar (mesmas regras do
        // runner ao registrar), e a conversa vê o mesmo registro do corpo da tarefa.
        let revised = match &decision.change {
            Some(Change::Split(split)) => Some(&split.current),
            _ => decision.revise.as_ref(),
        };
        let base = match revised.and_then(|r| r.allowed_scope.as_ref()) {
            Some(scope) => scope.iter().map(|g| g.trim().to_string()).collect(),
            None => loaded.allowed_scope.clone(),
        };
        text = crate::agents::runner::reconcile_scope_added(&base, &text);
    }
    if migrated.is_some() {
        text.push_str(&format!("\n{MIGRATED_LINE}"));
    }
    if !decision.retry {
        text.push_str(&escalation_context(scope.secrets, &loaded, &decision));
    }
    // Rodada autônoma só conta quando a decisão reabre a tarefa; a linha fica no corpo (persistida).
    // REVISE_CONFIG aplicada em YOLO também conta (a correção foi autônoma).
    let round = decision.retry
        && (autonomous || forced || (loaded.yolo && (decision.config.is_some() || migrated.is_some())));
    if round {
        if env_fix.is_some() {
            text.push_str(&format!("\n{ENV_FIX_PREFIX}{}", loaded.env_fixes + 1));
        }
        text.push_str(&format!("\n{AUTONOMOUS_ROUND_PREFIX}{} de {MAX_AUTONOMOUS_ROUNDS}", loaded.rounds + 1));
    }
    // Escalada sem o modelo (limite, rodadas, indisponível) e correção determinística de ambiente
    // não contam como ciclo de recuperação (spec 073).
    let deterministic = env_fix.is_some()
        || (!decision.retry
            && [CAUSE_LIMIT, CAUSE_ENV_ROUNDS, CAUSE_UNAVAILABLE].contains(&decision.root_cause.as_str()));
    if deterministic {
        text.push('\n');
        text.push_str(crate::agents::runner::NOT_COUNTED_LINE);
    }
    let (spec_id, tid, body) = (scope.spec_id.to_string(), task_id.to_string(), text.clone());
    let runner_job = Arc::clone(runner);
    let job_decision = decision.clone();
    let job_secrets = Arc::clone(scope.secrets);
    let written = tokio::task::spawn_blocking(move || record(&*runner_job, &spec_id, &tid, &job_decision, &body, &job_secrets))
        .await
        .ok()
        .flatten();
    // Sem registro não há retomada: a tarefa fica como está (BLOCKED).
    let (recorded, kind) = match written {
        Some(w) => {
            text = w.text;
            (Some(w.cycle), w.kind)
        }
        None => (None, Recorded::Escalate),
    };
    let applied_retry = decision.retry && recorded.is_some() && kind != Recorded::Escalate;
    let announced = match kind {
        _ if !applied_retry && matches!(decision.change, Some(Change::Ask(_))) && recorded.is_some() => "ASK",
        _ if !applied_retry => "ESCALATE",
        _ if decision.config.is_some() => "REVISE_CONFIG",
        _ if migrated.is_some() => "MIGRATE_ENVIRONMENT",
        Recorded::Split => "SPLIT",
        Recorded::Adjust => "ADJUST",
        Recorded::Revise => "REVISE_TASK",
        Recorded::Retry | Recorded::Escalate => "RETRY",
    };
    let published: &[RecoveryOption] = if applied_retry { &[] } else { &decision.options };
    let analysis = if published.is_empty() { None } else { recorded };
    scope.events.publish(cycle_event(
        scope,
        EventType::RecoveryDecided,
        task_id,
        cycle,
        Some(announced),
        published,
        analysis,
    ));
    let yolo_tag = if round && applied_retry { " [rodada autônoma]" } else { "" };
    let message = format!("Análise de recuperação #{cycle} ({announced}){yolo_tag}\n{text}");
    deliver(
        scope.events,
        Some(scope.dialog),
        ApiEvent::new(
            EventType::AgentMessage,
            Some(scope.project_id.to_string()),
            json!({
                "agent": ARCHITECT_AGENT,
                "text": scrub_text(scope.secrets, &message),
                "task_id": task_id,
                "attempt": Value::Null,
                "seq": Value::Null,
            }),
        ),
    );
    if applied_retry && migrated.is_some() {
        Next::Relaunch
    } else if applied_retry {
        Next::Retry
    } else {
        Next::Stay
    }
}

/// Como a decisão ficou registrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorded {
    Retry,
    Revise,
    Split,
    Adjust,
    Escalate,
}

/// Registro feito: ciclo, texto da seção (o que a conversa também mostra) e como ficou.
struct Written {
    cycle: usize,
    text: String,
    kind: Recorded,
}

/// Grava a decisão pelo runner: SPLIT e ADJUST pelos caminhos próprios (spec 124 T03), REVISE_TASK
/// pela revisão e o resto pela seção de sempre. ADJUST cujos critérios não podem ser gravados segue
/// como RETRY com a estratégia (como o `keep` da arbitragem), com a linha que diz isso.
fn record(runner: &dyn SpecRunner, spec_id: &str, task_id: &str, decision: &Decision, body: &str, secrets: &SecretSet) -> Option<Written> {
    let scrub = |t: &str| scrub_text(secrets, t);
    match &decision.change {
        Some(Change::Split(split)) if decision.retry => {
            let current = TaskRevision {
                objective: scrub(&split.current.objective),
                acceptance: scrub(&split.current.acceptance),
                allowed_scope: split.current.allowed_scope.clone(),
            };
            let split = TaskSplit { current, new_tasks: split.new_tasks.clone() };
            let (cycle, text) = runner.record_split(spec_id, task_id, &split, body).ok()?;
            Some(Written { cycle, text, kind: Recorded::Split })
        }
        Some(Change::Adjust(adjustment)) if decision.retry => match runner.record_adjust(spec_id, task_id, adjustment, body) {
            Ok((cycle, text)) => Some(Written { cycle, text, kind: Recorded::Adjust }),
            Err(crate::agents::runner::RunnerError::Invalid(_)) => {
                let text = format!("{}\n{}", body.trim_end(), plan::ADJUST_REFUSED_LINE);
                let cycle = runner.record_recovery(spec_id, task_id, true, &text).ok()?;
                Some(Written { cycle, text, kind: Recorded::Retry })
            }
            Err(_) => None,
        },
        _ => {
            if let (true, Some(r)) = (decision.retry, decision.revise.as_ref()) {
                let revision = TaskRevision {
                    objective: scrub(&r.objective),
                    acceptance: scrub(&r.acceptance),
                    allowed_scope: r.allowed_scope.clone(),
                };
                let cycle = runner.record_revision(spec_id, task_id, &revision, body).ok()?;
                return Some(Written { cycle, text: body.to_string(), kind: Recorded::Revise });
            }
            let cycle = runner.record_recovery(spec_id, task_id, decision.retry, body).ok()?;
            let kind = if decision.retry { Recorded::Retry } else { Recorded::Escalate };
            Some(Written { cycle, text: body.to_string(), kind })
        }
    }
}

/// Linha do relato quando a recuperação ajustou o ambiente docker (spec 074).
pub const MIGRATED_LINE: &str = "Ambiente docker ajustado: o run será relançado com o ambiente novo.";

/// Causa de escalada quando o ajuste do ambiente docker proposto não pôde ser aplicado.
const CAUSE_MIGRATION_FAILED: &str = "ajuste do ambiente docker não aplicado";
/// Causa quando o humano nega a aprovação do ambiente.
pub const CAUSE_ENV_DENIED: &str = "aprovação do ambiente negada pelo humano";

/// Falha ao ajustar o ambiente docker por uma opção `environment` com `files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateError {
    /// O humano negou a aprovação (ou ela expirou): nada foi alterado.
    Denied,
    /// Opção inválida agora (arquivos, docker indisponível) ou falha ao adotar; nada ficou alterado.
    Failed(String),
    /// Shutdown durante a aprovação.
    Shutdown,
}

/// Aplica uma opção `environment` com `files` (spec 074): revalida (inclui Docker disponível), pede a
/// aprovação do ambiente (`decide_environment`: automática em YOLO, HITL nos demais casos) e, só
/// depois, adota na base (grava `.nants/`, commita, marca docker + hash, com restauração em falha).
/// Nunca grava `.nants` sem a aprovação.
pub async fn adjust_environment(
    state: &AppState,
    project_id: &str,
    dialog: &DialogTarget,
    root: &Path,
    secrets: &Arc<SecretSet>,
    spec_id: &str,
    task_id: &str,
    option: &RecoveryOption,
    reason: &str,
) -> Result<(), MigrateError> {
    use crate::harness::tools::{Approver, EnvChangeRequest, EnvFileChange, Role};
    let (job_root, job_secrets, job_option) = (root.to_path_buf(), Arc::clone(secrets), option.clone());
    let prereqs = state.prereqs().clone();
    let checked = tokio::task::spawn_blocking(move || {
        validate_option(&job_root, &job_secrets, job_option, false, &[], &|| prereqs.docker_available())
    })
    .await
    .map_err(|_| MigrateError::Failed("falha interna na validação".to_string()))?
    .map_err(MigrateError::Failed)?;
    let files = checked.files.clone().ok_or_else(|| MigrateError::Failed("opção sem files".to_string()))?;
    let diff = checked.diff.clone().unwrap_or_default();
    let changes: Vec<EnvFileChange> = files
        .iter()
        .map(|f| EnvFileChange {
            path: f.path.clone(),
            diff: if files.len() == 1 || diff.is_empty() {
                diff.clone()
            } else {
                // Diff por arquivo: a parte do diff unificado que começa neste caminho.
                diff_section(&diff, &f.path)
            },
        })
        .collect();
    let approver = ProjectApprover::new(
        project_id,
        root.to_path_buf(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::clone(secrets),
    )
    .with_dialog(dialog.clone())
    .with_lang(state.project_lang(project_id));
    let req = EnvChangeRequest {
        role: Role::Architect,
        task_id: Some(task_id.to_string()),
        reason: scrub_text(secrets, reason),
        files: changes,
    };
    // Enquanto a aprovação está pendente, o Arquiteto não grava arquivos de ambiente.
    let _guard = crate::sdd::environment::begin_approval(root);
    let mut shutdown = state.shutdown_signal();
    let approval = tokio::select! {
        biased;
        _ = wait_shutdown(&mut shutdown) => return Err(MigrateError::Shutdown),
        approval = approver.approve_environment(&req) => approval,
    };
    if approval == ApprovalDecision::Deny {
        return Err(MigrateError::Denied);
    }
    let (job_root, spec) = (root.to_path_buf(), spec_id.to_string());
    let pairs: Vec<(String, String)> = files.into_iter().map(|f| (f.path, f.content)).collect();
    let lang = state.project_lang(project_id).get();
    tokio::task::spawn_blocking(move || super::runs::adopt_environment_files(&job_root, &spec, &pairs, lang))
        .await
        .map_err(|_| MigrateError::Failed("falha interna na adoção".to_string()))?
        .map(|_| ())
        .map_err(MigrateError::Failed)
}

/// Trecho do diff unificado concatenado que pertence a `path` (cabeçalho `--- ` até o próximo).
fn diff_section(diff: &str, path: &str) -> String {
    let marker = format!("+++ {path}\n");
    let Some(at) = diff.find(&marker) else { return String::new() };
    let start = diff[..at].rfind("--- ").unwrap_or(0);
    let rest = &diff[at + marker.len()..];
    let end = rest.find("\n--- ").map_or(diff.len(), |i| at + marker.len() + i);
    diff[start..end].to_string()
}

/// YOLO: aplica a migração da decisão. Sucesso: a decisão segue (RETRY). Negada, inválida ou com
/// falha: ESCALATE com a causa e a opção preservada para o humano (nada ficou alterado).
async fn apply_migration_decision(
    scope: &Scope<'_>,
    task_id: &str,
    decision: Decision,
) -> Result<Decision, Stop> {
    let Some(option) = decision.migrate.clone() else {
        return Ok(decision);
    };
    let reason = crate::i18n::tr(
        scope.state.project_lang(scope.project_id).get(),
        "approvals.recovery.env_reason",
        &[("task", task_id), ("cause", &decision.root_cause)],
    );
    let migrated = adjust_environment(
        scope.state,
        scope.project_id,
        scope.dialog,
        scope.root,
        scope.secrets,
        scope.spec_id,
        task_id,
        &option,
        &reason,
    )
    .await;
    match migrated {
        Ok(()) => Ok(decision),
        Err(MigrateError::Shutdown) => Err(Stop::Shutdown),
        Err(err) => {
            let cause = match err {
                MigrateError::Denied => CAUSE_ENV_DENIED.to_string(),
                MigrateError::Failed(m) => format!("{CAUSE_MIGRATION_FAILED}: {m}"),
                MigrateError::Shutdown => CAUSE_UNAVAILABLE.to_string(),
            };
            let mut escalated = Decision::escalate(&cause);
            escalated.options = vec![option];
            Ok(escalated)
        }
    }
}

/// Fatos enviados ao modelo: em YOLO a instrução autônoma vale desde o 1º ciclo; `reinforced` troca
/// pela versão que proíbe ESCALATE.
fn model_facts(base: &str, yolo: bool, over_limit: bool, reinforced: bool) -> String {
    let note = if !yolo {
        ""
    } else if reinforced {
        YOLO_REINFORCED_NOTE
    } else if over_limit {
        AUTONOMOUS_NOTE
    } else {
        YOLO_NOTE
    };
    format!("{base}{note}")
}

/// Motivo ambiental cuja correção é um arquivo `.nants/*`: build/up do compose ou citação de `.nants/`.
pub fn is_env_fixable(reason: &str) -> bool {
    if crate::harness::compose::is_non_file_cause(reason) {
        return false;
    }
    let r = reason.to_lowercase();
    r.contains(".nants/") || r.contains("compose build") || r.contains("compose up") || r.contains("build/up")
}

/// Modo YOLO ligado nas settings do projeto (ilegível: desligado).
pub fn yolo_enabled(root: &Path) -> bool {
    safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES)
        .ok()
        .and_then(|t| Settings::parse(&t).ok())
        .is_some_and(|s| s.yolo)
}

/// RETRY sem modelo: o Desenvolvedor corrige o ambiente pelo fluxo de permissão.
fn env_fix_decision(reason: &str) -> Decision {
    Decision {
        retry: true,
        revise: None,
        root_cause: format!(
            "falha do ambiente docker causada por arquivos do ambiente (.nants/*): {}",
            truncate_bytes(reason.trim(), 600)
        ),
        strategy: Some(
            "Corrija o ambiente com `request_environment_change` (.nants/compose.yaml / .nants/Dockerfile*); \
             não edite .nants/ com write_file (fora do escopo). Leia a mensagem de erro do build/up, ajuste o arquivo \
             e valide de novo. Não há decisão pendente do humano."
                .to_string(),
        ),
        proposal: None,
        config: None,
        options: Vec::new(),
        discarded: Vec::new(),
        migrate: None,
        scope_add: Vec::new(),
        change: None,
    }
}

/// Causa raiz com a opção que a aprovação automática aplicou, no idioma do projeto (spec 115 T03).
fn auto_applied(lang: crate::i18n::Lang, cause: &str, label: &str) -> String {
    crate::i18n::tr(lang, "recovery.auto_applied", &[("cause", cause), ("label", label)])
}

/// Converte um ESCALATE em RETRY com a estratégia/proposta sugerida e a premissa registrada (C-02).
/// A premissa (`recovery.premise_note`) e a estratégia padrão quando o modelo escalou sem sugerir
/// nada (`recovery.default_strategy`) vão para a causa raiz e a estratégia da análise, que a pessoa
/// lê no Quadro e no cartão de recuperação: saem no idioma do projeto (spec 115 T04).
fn premise_retry(lang: crate::i18n::Lang, escalate: Decision) -> Decision {
    let mut strategy = escalate.strategy.clone().unwrap_or_default();
    // A pergunta de um ASK vira parte da estratégia: o Desenvolvedor adota a premissa mais razoável.
    let question = match &escalate.change {
        Some(Change::Ask(q)) => Some(q),
        _ => None,
    };
    for extra in escalate.proposal.iter().chain(question) {
        if !strategy.is_empty() {
            strategy.push_str("; ");
        }
        strategy.push_str(extra);
    }
    if strategy.trim().is_empty() {
        strategy = crate::i18n::text(lang, "recovery.default_strategy");
    }
    Decision {
        retry: true,
        revise: None,
        root_cause: format!("{} ({})", escalate.root_cause, crate::i18n::text(lang, "recovery.premise_note")),
        strategy: Some(strategy),
        proposal: None,
        config: None,
        options: Vec::new(),
        discarded: Vec::new(),
        migrate: None,
        scope_add: Vec::new(),
        change: None,
    }
}

/// Valida as opções de um ESCALATE do modelo. Em YOLO, com rodadas sobrando, a primeira opção
/// `config` válida vira uma decisão REVISE_CONFIG (validada de novo e aplicada por
/// [`apply_config_decision`]); sem ela, o ESCALATE segue com as opções válidas.
async fn prepare_options(scope: &Scope<'_>, loaded: &Loaded, decision: Decision) -> Decision {
    if !is_escalate(&decision) || (decision.options.is_empty() && decision.discarded.is_empty()) {
        return decision;
    }
    let decision = validate_decision_options(scope, loaded, decision).await;
    let lang = scope.state.project_lang(scope.project_id).get();
    if loaded.yolo && loaded.rounds < MAX_AUTONOMOUS_ROUNDS {
        let config = decision.options.iter().find(|o| o.kind == OptionKind::Config).map(|o| {
            (o.label.clone(), ConfigProposal { test_command: o.test_command.clone(), checks: o.checks.clone() })
        });
        if let Some((label, proposal)) = config {
            return Decision {
                retry: true,
                revise: None,
                root_cause: auto_applied(lang, &decision.root_cause, &label),
                strategy: decision.strategy.or(Some(label)),
                proposal: None,
                config: Some(proposal),
                options: Vec::new(),
                discarded: Vec::new(),
                migrate: None,
                scope_add: Vec::new(),
                change: None,
            };
        }
        // Ajuste do ambiente docker (spec 074): a primeira opção `environment` com `files` válida.
        let migrate = decision.options.iter().find(|o| o.kind == OptionKind::Environment && o.files.is_some());
        if let Some(option) = migrate {
            return Decision {
                retry: true,
                revise: None,
                root_cause: auto_applied(lang, &decision.root_cause, &option.label),
                strategy: decision.strategy.clone().or_else(|| Some(option.label.clone())),
                proposal: None,
                config: None,
                options: Vec::new(),
                discarded: Vec::new(),
                migrate: Some(option.clone()),
                scope_add: Vec::new(),
                change: None,
            };
        }
    }
    decision
}

/// Valida as opções da decisão (sem aplicar nenhuma): as inválidas viram `discarded`.
async fn validate_decision_options(scope: &Scope<'_>, loaded: &Loaded, mut decision: Decision) -> Decision {
    let (root, secrets) = (scope.root.to_path_buf(), Arc::clone(scope.secrets));
    let (options, environmental) = (std::mem::take(&mut decision.options), loaded.environmental);
    let prereqs = scope.state.prereqs().clone();
    let validated = tokio::task::spawn_blocking(move || {
        validate_options(&root, &secrets, options, environmental, &|| prereqs.docker_available())
    })
    .await
    .unwrap_or_default();
    decision.options = validated.0;
    decision.discarded.extend(validated.1);
    decision
}

/// Escalada por limite (CAUSE_LIMIT / CAUSE_ENV_ROUNDS): UMA chamada ao modelo só para gerar opções
/// (ESCALATE forçado, sem repetir estratégias anteriores). As opções passam pela validação, mas
/// nunca são aplicadas aqui, nem em YOLO. Modelo indisponível ou zero opções válidas: escalada
/// determinística de sempre (só a causa). Shutdown propaga.
async fn limit_escalation(
    scope: &Scope<'_>,
    loaded: &Loaded,
    task_id: &str,
    shutdown: &mut watch::Receiver<bool>,
    cause: &str,
) -> Result<Decision, Stop> {
    let facts = format!("{}{}{LIMIT_NOTE}", loaded.facts, loaded.env_note);
    let answer = match ask_model(scope, &facts, task_id, shutdown).await {
        Ok(answer) => answer,
        Err(Stop::Shutdown) => return Err(Stop::Shutdown),
        Err(Stop::Unavailable) => return Ok(Decision::escalate(cause)),
    };
    if !is_escalate(&answer) || answer.options.is_empty() {
        return Ok(Decision::escalate(cause));
    }
    let mut proposed = Decision::escalate(cause);
    proposed.options = answer.options;
    proposed.discarded = answer.discarded;
    let validated = validate_decision_options(scope, loaded, proposed).await;
    if validated.options.is_empty() {
        return Ok(Decision::escalate(cause));
    }
    Ok(validated)
}

fn is_escalate(decision: &Decision) -> bool {
    !decision.retry && decision.config.is_none()
}

/// Pergunta ao modelo com uma nova tentativa se indisponível/inválido (YOLO ou não) e, em YOLO,
/// uma 2ª pergunta reforçada se ele escalar. Persistindo, devolve o ESCALATE.
async fn decide(
    scope: &Scope<'_>,
    base: &str,
    task_id: &str,
    shutdown: &mut watch::Receiver<bool>,
    yolo: bool,
    over_limit: bool,
) -> Result<Decision, Stop> {
    let facts = model_facts(base, yolo, over_limit, false);
    let mut first = ask_model(scope, &facts, task_id, shutdown).await;
    // Indisponível: nova tentativa (e, em YOLO, uma 3ª antes de escalar).
    for _ in 0..(if yolo { 2 } else { 1 }) {
        if matches!(first, Err(Stop::Unavailable)) {
            first = ask_model(scope, &facts, task_id, shutdown).await;
        }
    }
    let first = first?;
    // ESCALATE que já traz uma opção `config` (ou `environment` com `files`) não é reforçado: ela é
    // validada e aplicada sozinha.
    let has_config_option = first
        .options
        .iter()
        .any(|o| o.kind == OptionKind::Config || (o.kind == OptionKind::Environment && o.files.is_some()));
    if !(yolo && is_escalate(&first)) || has_config_option {
        return Ok(first);
    }
    match ask_model(scope, &model_facts(base, yolo, over_limit, true), task_id, shutdown).await {
        Ok(second) => Ok(second),
        Err(Stop::Shutdown) => Err(Stop::Shutdown),
        Err(Stop::Unavailable) => Ok(first),
    }
}

/// Valida a proposta REVISE_CONFIG, pede aprovação (HITL; YOLO aprova sozinho) e grava a correção
/// em `settings.yaml` (só o servidor grava). Qualquer problema vira ESCALATE com causa fixa.
async fn apply_config_decision(
    scope: &Scope<'_>,
    task_id: &str,
    decision: Decision,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Decision, Stop> {
    let Some(proposal) = decision.config.clone() else {
        return Ok(decision);
    };
    let (root, secrets) = (scope.root.to_path_buf(), Arc::clone(scope.secrets));
    let checked = tokio::task::spawn_blocking(move || validate_config(&root, &proposal, &secrets))
        .await
        .map_err(|_| Stop::Unavailable)?;
    let change = match checked {
        Ok(change) => change,
        Err(reason) if reason.starts_with(CAUSE_CONFIG_WEAKENS) || reason.starts_with(CAUSE_CONFIG_REPEATS) => {
            return Ok(Decision::escalate(&reason))
        }
        Err(reason) => return Ok(Decision::escalate(&format!("{CAUSE_CONFIG_INVALID}: {reason}"))),
    };
    let approver = ProjectApprover::new(
        scope.project_id,
        scope.root.to_path_buf(),
        scope.state.approvals().clone(),
        scope.state.events().clone(),
        Arc::clone(scope.secrets),
    )
    .with_dialog(scope.dialog.clone())
    .with_lang(scope.state.project_lang(scope.project_id));
    let reason = crate::i18n::tr(
        scope.state.project_lang(scope.project_id).get(),
        "approvals.recovery.config_reason",
        &[("task", task_id), ("cause", &decision.root_cause)],
    );
    let approval = tokio::select! {
        biased;
        _ = wait_shutdown(shutdown) => return Err(Stop::Shutdown),
        approval = approver.approve_config_change(Some(task_id), &reason, &change.diff, &change.commands) => approval,
    };
    if approval == ApprovalDecision::Deny {
        return Ok(Decision::escalate(CAUSE_CONFIG_DENIED));
    }
    let (root, secrets) = (scope.root.to_path_buf(), Arc::clone(scope.secrets));
    let applied = tokio::task::spawn_blocking(move || write_config(&root, &change, &secrets))
        .await
        .map_err(|_| Stop::Unavailable)?;
    if applied.is_err() {
        return Ok(Decision::escalate(CAUSE_CONFIG_SAVE));
    }
    Ok(decision)
}

/// Falha de ambiente admite RETRY (estratégia nova), REVISE_CONFIG ou ESCALATE; REVISE_TASK escala,
/// porque reescrever objetivo/critérios para contornar o ambiente rebaixaria a verificação.
fn restrict_environmental(environmental: bool, decision: Decision) -> Decision {
    // SPLIT e ADJUST também reescrevem a tarefa (spec 124 T03): mesma regra do REVISE_TASK.
    let rewrites = decision.revise.is_some() || matches!(decision.change, Some(Change::Split(_) | Change::Adjust(_)));
    if environmental && rewrites {
        Decision::escalate(CAUSE_ENV_REVISE)
    } else {
        decision
    }
}

/// Primeiras palavras que encerram ou desviam o fluxo: uma etapa assim anula as seguintes.
const CONTROL_WORDS: [&str; 6] = ["exit", "return", "false", ":", "break", "continue"];

/// Divide um comando em etapas separadas por `&&` FORA de aspas, cada uma como lista de palavras sem
/// aspas e com espaços normalizados. `None` se houver qualquer outra construção de shell fora de
/// aspas (`;`, `||`, `|`, `&`, quebra de linha, subshell, agrupamento, redirecionamento, crase,
/// barra invertida), aspas abertas ou etapa vazia.
fn parse_steps(command: &str) -> Option<Vec<Vec<String>>> {
    let mut steps: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                in_word = true;
            }
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
                if words.is_empty() {
                    return None;
                }
                steps.push(std::mem::take(&mut words));
            }
            '&' | ';' | '|' | '\n' | '\r' | '(' | ')' | '{' | '}' | '`' | '<' | '>' | '\\' => return None,
            _ => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        words.push(word);
    }
    if words.is_empty() {
        return None;
    }
    steps.push(words);
    Some(steps)
}

/// `true` se `new` executa, como etapa inteira de um encadeamento só de `&&`, cada etapa de
/// `original` (comparação por palavras: espaços e aspas normalizados). `new` não pode ter etapa de
/// saída/controle (`exit`, `false`...). Se o original não é analisável, exige comando idêntico.
/// Conservador de propósito: `npm test || true`, `exit 0 && npm test` e `echo "x && npm test"` falham.
fn contains_command(new: &str, original: &str) -> bool {
    let Some(have) = parse_steps(new) else {
        return false;
    };
    if have.iter().any(|step| step.first().is_some_and(|w| CONTROL_WORDS.contains(&w.as_str()))) {
        return false;
    }
    match parse_steps(original) {
        Some(wanted) => wanted.iter().all(|step| have.contains(step)),
        None => new.trim() == original.trim(),
    }
}

/// Guarda contra rebaixamento da verificação: o novo `test_command` precisa conter o original e cada
/// check bloqueante original precisa continuar sendo executado (no `test_command` ou em check
/// bloqueante). Num projeto docker, o comando original que invoca docker/podman está sendo
/// reescrito: o novo precisa conter o comando INTERNO desembrulhado (só esse item é flexibilizado; se
/// o formato não é desembrulhável, só ele fica isento). Os demais itens continuam protegidos.
///
/// LIMITAÇÃO: o servidor só confere objetivamente o `test_command`/checks das settings. Mudanças
/// internas a scripts do projeto (ex.: reescrever o script `test` do package.json para tirar o
/// Playwright) não são detectáveis aqui; ficam cobertas pelo prompt da recuperação e pela regra do
/// REVISE_TASK, que proíbem explicitamente rebaixar a verificação.
fn check_not_weakened(current: &Settings, new_test: &str, new_checks: &[Check]) -> Result<(), String> {
    let docker = current.environment == Environment::Docker;
    // `None`: nada objetivo a preservar (comando vazio ou invólucro docker não desembrulhável).
    let orig = |c: &str| -> Option<String> {
        let c = c.trim();
        if docker && docker_command::invokes_container_tool(c) {
            return docker_command::unwrap_workspace_command(c);
        }
        (!c.is_empty()).then(|| c.to_string())
    };
    if let Some(original_test) = orig(&current.test_command) {
        if !contains_command(new_test, &original_test) {
            return Err(format!("{CAUSE_CONFIG_WEAKENS}: o novo test_command não executa mais `{original_test}`"));
        }
    }
    for check in current.checks.iter().filter(|c| c.blocking) {
        let Some(original) = orig(&check.command) else {
            continue;
        };
        let kept = contains_command(new_test, &original)
            || new_checks.iter().any(|n| n.blocking && contains_command(&n.command, &original));
        if !kept {
            return Err(format!("{CAUSE_CONFIG_WEAKENS}: o check bloqueante `{}` deixaria de ser executado", check.name));
        }
    }
    Ok(())
}

/// Texto "Contexto" anexado a toda escalada: o que a UI e o humano precisam para decidir.
fn escalation_context(secrets: &SecretSet, loaded: &Loaded, decision: &Decision) -> String {
    let mut lines = vec![
        "- Contexto:".to_string(),
        format!("  - comando: {}", clean(secrets, &loaded.command)),
        format!("  - código: {}", clean(secrets, &loaded.exit_code)),
        "  - trecho do erro:".to_string(),
    ];
    let tail = loaded.error_tail.trim();
    if tail.is_empty() {
        lines.push("      (sem saída registrada)".to_string());
    }
    for line in tail.lines().filter(|l| !l.trim().is_empty()) {
        lines.push(format!("      {}", scrub_text(secrets, line.trim_end())));
    }
    lines.push(format!(
        "  - o que foi tentado: {} de {MAX_CYCLES} ciclos de recuperação e {} de {MAX_AUTONOMOUS_ROUNDS} rodadas autônomas",
        loaded.cycles.min(MAX_CYCLES),
        loaded.rounds.min(MAX_AUTONOMOUS_ROUNDS)
    ));
    lines.push(format!("  - por que não resolveu: {}", clean(secrets, &decision.root_cause)));
    let options = if !decision.options.is_empty() {
        "  - opções: escolha uma das opções propostas acima, escreva uma orientação própria ou use Retomar"
    } else if matches!(decision.change, Some(Change::Ask(_))) {
        plan::ASK_OPTIONS_LINE
    } else if loaded.environmental {
        "  - opções: (1) ajustar `test_command`/checks nas configurações para preparar o que falta (ex.: instalar dependências antes dos testes); \
         (2) ajustar o ambiente docker (.nants/) quando a imagem não tem o componente; \
         (3) corrigir o ambiente manualmente e usar Retomar; \
         (4) NÃO remover os testes do comando para contornar o problema"
    } else {
        "  - opções: (1) orientar o Desenvolvedor com uma estratégia concreta e usar Retomar; \
         (2) revisar o objetivo/critérios da tarefa; (3) decidir o ponto de produto em aberto"
    };
    lines.push(options.to_string());
    format!("\n{}", lines.join("\n"))
}

/// Spec 129 T05: linha dos FATOS da recuperação com o `install_command` e a regra de que o Nants já
/// o roda antes (o REVISE_CONFIG não o repete). Vazio sem instalação. Texto para o modelo.
fn install_rule(install: Option<&str>) -> String {
    match install.map(str::trim).filter(|c| !c.is_empty()) {
        Some(install) => format!(
            "\ninstall_command: {install} (o Nants já a roda antes do test_command e dos checks, em toda worktree nova; \
             o test_command e os checks de um REVISE_CONFIG NÃO repetem a instalação: nada de `{install} && <testes>`)"
        ),
        None => String::new(),
    }
}

/// Fatos extras da falha de ambiente enviados ao Arquiteto (comando, código, cauda do erro e regras).
/// Spec 129 T05: com `install_command`, o conselho é a instalação já declarada (que o Nants roda
/// antes), nunca embuti-la no `test_command`.
fn env_note(command: &str, exit_code: &str, error_tail: &str, install: Option<&str>) -> String {
    let worktree = match install.map(str::trim).filter(|c| !c.is_empty()) {
        Some(install) => format!(
            "O `test_command` e os checks rodam numa worktree NOVA, e o Nants roda antes deles o install_command \
             (`{install}`): NÃO repita a instalação no `test_command`. Se falta dependência, ela precisa estar \
             declarada no projeto para essa instalação trazê-la, ou a imagem precisa fornecê-la (ambiente docker). \
             Decida: RETRY com estratégia, REVISE_CONFIG (sem a instalação no comando) ou ESCALATE com a causa."
        ),
        None => "O `test_command` e os checks rodam numa worktree NOVA, sem dependências instaladas. Decida: \
                 RETRY com estratégia, REVISE_CONFIG (torne o comando autossuficiente, mantendo o comando original \
                 como etapa: ex. `npm ci && npm test`) ou ESCALATE com a causa."
            .to_string(),
    };
    format!(
        "\n## Falha de ambiente: o comando de testes NÃO executou\n\
         comando: {command}\ncódigo: {exit_code}\ncauda do erro:\n{error_tail}\n\
         {worktree} Limitação de ambiente se resolve \
         mudando o ambiente ou a configuração, NUNCA a verificação: não tire testes do `test_command`, não \
         troque por verificação manual, não afrouxe os critérios globais. REVISE_TASK não é aceito aqui.\n"
    )
}

/// Seção "Critérios globais" do `spec.md` (vazia se não houver).
fn global_criteria(spec_text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in spec_text.lines() {
        if line.starts_with("## ") {
            inside = line.to_lowercase().contains("critérios globais");
            continue;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Guarda contra repetição (spec 079 T04): o `test_command` é global e roda uma vez por execução.
/// Recusa o novo comando em que o comando anterior aparece duas ou mais vezes, ou em que a mesma
/// etapa entre `&&` se repete no mesmo diretório (ex.: `a && b && a && b`; `cd` zera as etapas vistas). Repetir a suíte para satisfazer o critério
/// de uma tarefa dobraria o custo de toda execução futura; critério assim se corrige com REVISE_TASK.
fn check_not_repeated(current: &Settings, new_test: &str) -> Result<(), String> {
    let refused = |what: String| {
        Err(format!(
            "{CAUSE_CONFIG_REPEATS}: {what}. O test_command roda uma vez por execução e vale para todas as tarefas; \
             critério que exige execuções repetidas se corrige com REVISE_TASK (idempotência vira teste dentro da suíte). \
             O test_command anterior foi mantido."
        ))
    };
    let steps = parse_steps(new_test);
    if let Some(steps) = &steps {
        // A repetição só conta dentro do mesmo diretório: uma etapa `cd …` (monorepo) zera as vistas.
        let mut seen: Vec<&Vec<String>> = Vec::new();
        for step in steps {
            if step.first().is_some_and(|w| w == "cd") {
                seen.clear();
                continue;
            }
            if seen.contains(&step) {
                return refused(format!("a etapa `{}` aparece mais de uma vez no novo test_command", step.join(" ")));
            }
            seen.push(step);
        }
    }
    let docker = current.environment == Environment::Docker;
    let previous = current.test_command.trim();
    let previous = if docker && docker_command::invokes_container_tool(previous) {
        docker_command::unwrap_workspace_command(previous).unwrap_or_else(|| previous.to_string())
    } else {
        previous.to_string()
    };
    if previous.is_empty() {
        return Ok(());
    }
    let occurrences = match (&steps, parse_steps(&previous)) {
        (Some(have), Some(wanted)) if !wanted.is_empty() && wanted.len() <= have.len() => {
            have.windows(wanted.len()).filter(|w| *w == wanted.as_slice()).count()
        }
        (Some(_), Some(_)) => 0,
        _ => new_test.matches(previous.as_str()).count(),
    };
    if occurrences >= 2 {
        return refused(format!("o comando anterior `{previous}` aparece {occurrences} vezes no novo test_command"));
    }
    Ok(())
}

/// Mudança já validada: configuração resultante e diff antes/depois (redigido).
struct ConfigChange {
    test_command: Option<String>,
    checks: Option<Vec<Check>>,
    diff: String,
    /// Todos os comandos resultantes (para a regra de auto-aprovação YOLO).
    commands: Vec<String>,
}

fn render_checks(checks: &[Check]) -> String {
    if checks.is_empty() {
        return "checks: (nenhum)".to_string();
    }
    checks
        .iter()
        .map(|c| format!("check {} ({}): {}", c.name, if c.blocking { "bloqueante" } else { "informativo" }, c.command))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Mesmas regras do plano (forma, nomes, sem docker em modo docker, sem segredo) + política de
/// comandos (denylist recusa). Proposta sem efeito (igual ao atual) também é recusada.
fn validate_config(root: &Path, proposal: &ConfigProposal, secrets: &SecretSet) -> Result<ConfigChange, String> {
    let text = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES).map_err(|_| "settings ilegível".to_string())?;
    let current = Settings::parse(&text).map_err(|_| "settings inválido".to_string())?;
    let docker = current.environment == Environment::Docker;
    let norm = |c: &str| -> Result<String, String> {
        let shaped = crate::sdd::check_test_command_shape(c).map_err(|_| "comando inválido (uma linha, até 200 caracteres)".to_string())?;
        if scrub_text(secrets, &shaped) != shaped {
            return Err("comando contém segredo".to_string());
        }
        if docker {
            docker_command::normalize(&shaped).map_err(|m| m.to_string())
        } else {
            Ok(shaped)
        }
    };
    let test_command = proposal.test_command.as_deref().map(norm).transpose()?;
    let checks = match &proposal.checks {
        Some(list) => {
            let mut normalized = Vec::with_capacity(list.len());
            for c in list {
                normalized.push(Check { command: norm(&c.command)?, ..c.clone() });
            }
            Some(crate::sdd::validate_checks(&normalized).ok_or_else(|| "checks inválidos".to_string())?)
        }
        None => None,
    };
    let policy = Policy::from_settings(&current);
    let all = test_command.iter().cloned().chain(checks.iter().flatten().map(|c| c.command.clone()));
    for command in all {
        if let allowlist::Decision::Deny(why) = allowlist::evaluate(&command, &policy) {
            return Err(format!("comando recusado pela política: {why}"));
        }
    }
    let new_test = test_command.clone().unwrap_or_else(|| current.test_command.clone());
    let new_checks = checks.clone().unwrap_or_else(|| current.checks.clone());
    check_not_weakened(&current, &new_test, &new_checks)?;
    if let Some(proposed) = &test_command {
        check_not_repeated(&current, proposed)?;
    }
    if new_test == current.test_command && new_checks == current.checks {
        return Err("a proposta não muda a configuração atual".to_string());
    }
    let diff = scrub_text(
        secrets,
        &format!(
            "Antes:\ntest_command: {}\n{}\n\nDepois:\ntest_command: {}\n{}",
            current.test_command,
            render_checks(&current.checks),
            new_test,
            render_checks(&new_checks)
        ),
    );
    let commands = std::iter::once(new_test).chain(new_checks.iter().map(|c| c.command.clone())).collect();
    Ok(ConfigChange { test_command, checks, diff, commands })
}

/// Spec 129 T05: avisos (pelo dicionário, no idioma do projeto) dos campos que a proposta
/// REVISE_CONFIG mudou e que, nas settings gravadas, começam com o `install_command`. As mesmas
/// chaves do aviso da spec 126 (`runner.install.repeated*`), que dizem que o Nants já roda a
/// instalação antes. Vazio sem `install_command`, sem repetição ou com settings ilegível.
fn repeated_install_warnings(root: &Path, config: &ConfigProposal, lang: crate::i18n::Lang) -> Vec<String> {
    let Some(settings) = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES)
        .ok()
        .and_then(|text| Settings::parse(&text).ok())
    else {
        return Vec::new();
    };
    let Some(install) = settings.install_command.as_deref().map(str::trim).filter(|c| !c.is_empty()) else {
        return Vec::new();
    };
    crate::sdd::test_levels::repeated_install(&settings)
        .into_iter()
        .filter(|(field, _)| match field.as_str() {
            "test_command" => config.test_command.is_some(),
            f => f.starts_with("checks[") && config.checks.is_some(),
        })
        .map(|(field, command)| {
            let key = if crate::sdd::test_levels::is_test_field(&field) { "runner.install.repeated" } else { "runner.install.repeated_step" };
            crate::i18n::tr(lang, key, &[("field", &field), ("command", &command), ("install", install)])
        })
        .collect()
}

/// Grava a correção (releitura sob o `PERSIST_LOCK`, escrita atômica); preserva os demais campos.
fn write_config(root: &Path, change: &ConfigChange, _secrets: &SecretSet) -> Result<(), ()> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let text = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES).map_err(|_| ())?;
    let mut settings = Settings::parse(&text).map_err(|_| ())?;
    if let Some(t) = &change.test_command {
        settings.test_command = t.clone();
    }
    if let Some(c) = &change.checks {
        settings.checks = c.clone();
    }
    let rendered = settings.render().map_err(|_| ())?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| ())
}

/// Projeto docker cujo `test_command`/`checks` ainda invocam docker/podman: erro de CONFIGURAÇÃO
/// (o comando falha dentro do workspace), não de ambiente. Habilita a recuperação (REVISE_CONFIG).
pub fn config_suspect(root: &Path) -> bool {
    safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES)
        .ok()
        .and_then(|t| Settings::parse(&t).ok())
        .is_some_and(|s| {
            s.environment == Environment::Docker
                && (docker_command::invokes_container_tool(&s.test_command)
                    || s.checks.iter().any(|c| docker_command::invokes_container_tool(&c.command)))
        })
}

/// Evento do ciclo; `options` (só no ESCALATE com opções válidas) vai no campo `options` do payload.
fn cycle_event(
    scope: &Scope<'_>,
    kind: EventType,
    task_id: &str,
    cycle: usize,
    decision: Option<&str>,
    options: &[RecoveryOption],
    analysis: Option<usize>,
) -> ApiEvent {
    let mut payload = json!({"spec_id": scope.spec_id, "task_id": task_id, "cycle": cycle});
    if let (Some(decision), Some(map)) = (decision, payload.as_object_mut()) {
        map.insert("decision".into(), Value::from(decision));
    }
    if let (false, Some(map)) = (options.is_empty(), payload.as_object_mut()) {
        map.insert("options".into(), serde_json::to_value(options).unwrap_or(Value::Null));
        // Número da análise que ofereceu as opções: a UI o devolve em `recovery-choice`.
        map.insert("options_analysis".into(), analysis.map_or(Value::Null, Value::from));
    }
    ApiEvent::new(kind, Some(scope.project_id.to_string()), payload)
}

/// Turno do modelo (task própria: pânico vira indisponível), com timeout e shutdown.
async fn ask_model(
    scope: &Scope<'_>,
    facts: &str,
    task_id: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Decision, Stop> {
    let answer = ask_raw(scope, facts, task_id, shutdown, false).await?;
    parse_decision(&answer).ok_or(Stop::Unavailable)
}

/// Turno somente leitura do Arquiteto (recuperação ou diagnóstico); devolve o texto bruto.
async fn ask_raw(
    scope: &Scope<'_>,
    facts: &str,
    task_id: &str,
    shutdown: &mut watch::Receiver<bool>,
    diagnosis: bool,
) -> Result<String, Stop> {
    let state = scope.state;
    let (model, effort, client) = architect_client(state).await.map_err(|_| Stop::Unavailable)?;
    let backend: Arc<dyn ChatBackend> = match state.report_backend() {
        Some(backend) => backend,
        None => Arc::new(client),
    };
    let root: PathBuf = scope.root.to_path_buf();
    let policy = load_policy(root.clone()).await;
    let approver = Arc::new(ProjectApprover::new(
        scope.project_id,
        root.clone(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::clone(scope.secrets),
    )
    .with_dialog(scope.dialog.clone())
    .with_lang(state.project_lang(scope.project_id)));
    let activity = Arc::new(
        BusActivitySink::new(state.events().clone(), scope.project_id, Arc::clone(scope.secrets))
            .with_trail(root.clone(), scope.spec_id),
    );
    // Internet só na recuperação (não no diagnóstico de ambiente) e só com o interruptor ligado.
    let internet = if diagnosis {
        None
    } else {
        super::chat::internet_setup(state, root.clone(), scope.secrets).await
    };
    let mut architect = LlmArchitect::new(backend, model, Some(effort), policy, approver)
        .with_lang(state.project_lang(scope.project_id))
        .with_activity(
        activity,
        Some(scope.spec_id.to_string()),
        Some(task_id.to_string()),
    )
    .with_usage(super::usage::FileUsageSink::redacting(root.clone(), scope.project_id, Arc::clone(scope.secrets)))
    .with_memory(Arc::clone(scope.secrets));
    if let Some(setup) = internet {
        architect = architect.with_internet(setup);
    }
    let facts = facts.to_string();
    let read_root = read_root(&root, scope.spec_id, task_id);
    let mut job = tokio::spawn(async move {
        if diagnosis {
            architect.diagnose(&read_root, &facts).await
        } else {
            architect.recover(&read_root, &facts).await
        }
    });
    tokio::select! {
        biased;
        _ = wait_shutdown(shutdown) => {
            job.abort();
            return Err(Stop::Shutdown);
        }
        joined = tokio::time::timeout(state.report_timeout(), &mut job) => match joined {
            Err(_) => {
                job.abort();
                return Err(Stop::Unavailable);
            }
            Ok(Ok(Ok(text))) => Ok(text),
            Ok(_) => Err(Stop::Unavailable),
        },
    }
}

/// Diagnóstico validado do bloqueio de ambiente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub root_cause: String,
    pub what_to_do: String,
}

/// JSON estrito `{"root_cause","what_to_do"}`: só esses campos, strings não vazias até [`MAX_FIELD_BYTES`].
pub fn parse_diagnosis(raw: &str) -> Option<Diagnosis> {
    let value: Value = serde_json::from_str(raw.trim()).ok()?;
    let map = value.as_object()?;
    if map.len() != 2 {
        return None;
    }
    let field = |name: &str| -> Option<String> {
        let s = map.get(name)?.as_str()?.trim();
        (!s.is_empty() && s.len() <= MAX_FIELD_BYTES).then(|| s.to_string())
    };
    Some(Diagnosis {
        root_cause: field("root_cause")?,
        what_to_do: field("what_to_do")?,
    })
}

/// Motivo normalizado em uma linha (espaços colapsados, redigido, limitado): chave de deduplicação.
fn reason_key(secrets: &SecretSet, reason: &str) -> String {
    let one_line = scrub_text(secrets, reason).split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_bytes(&one_line, 400)
}

/// Linha de motivo gravada na seção (e comparada para não repetir o diagnóstico).
fn reason_line(key: &str) -> String {
    format!("- Motivo: {key}")
}

/// Estado git resumido (branch atual, status curto, diff --stat base...branch), limitado.
fn git_summary(root: &Path, base: Option<&str>, branch: &str) -> String {
    let run = |args: &[&str]| -> String {
        match Command::new("git").args(args).current_dir(root).output() {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if text.is_empty() {
                    "(vazio)".into()
                } else {
                    truncate_bytes(&text, 3 * 1024)
                }
            }
            _ => "(indisponível)".into(),
        }
    };
    let range = match base.filter(|b| !b.is_empty() && !b.starts_with('-')) {
        Some(b) if !branch.is_empty() && !branch.starts_with('-') => Some(format!("{b}...{branch}")),
        _ => None,
    };
    let stat = match &range {
        Some(r) => run(&["diff", "--stat", r, "--"]),
        None => "(indisponível)".into(),
    };
    format!(
        "Branch atual: {}

## git status --short
{}

## diff --stat base...branch
{}
",
        run(&["rev-parse", "--abbrev-ref", "HEAD"]),
        run(&["status", "--short"]),
        stat
    )
}

struct DiagFacts {
    facts: String,
    already: bool,
}

/// Lê a tarefa bloqueada e monta os fatos do diagnóstico; `already` quando o mesmo motivo já tem seção.
fn load_diagnosis(root: &Path, spec_id: &str, task_id: &str, reason: &str, secrets: &SecretSet) -> Option<DiagFacts> {
    let rel = format!(".sdd/specs/{spec_id}/tasks/{task_id}.md");
    let raw = safe_fs::read_regular(root, &rel, MAX_READ_BYTES).ok()?;
    let task = Task::parse(&raw).ok()?;
    if task.frontmatter.status != TaskStatus::Blocked {
        return None;
    }
    let key = reason_key(secrets, reason);
    let already = diagnosis_notes(&task.body).lines().any(|l| l.trim_end() == reason_line(&key));
    let meta = &task.frontmatter;
    let base = safe_fs::read_regular(root, ".sdd/settings.yaml", MAX_READ_BYTES)
        .ok()
        .and_then(|r| Settings::parse(&r).ok())
        .map(|s| s.base_branch);
    let evidence = meta.test_evidence.clone().unwrap_or_default();
    let mut out = format!("Tarefa {} (spec {spec_id}): {}
", meta.id, truncate_bytes(&meta.title, 200));
    out.push_str(&format!(
        "
## Motivo completo do bloqueio
{}
",
        scrub_text(secrets, &truncate_bytes(reason.trim(), MAX_SECTION_BYTES))
    ));
    out.push_str(&format!("
## Estado git
{}", git_summary(root, base.as_deref(), &meta.branch)));
    out.push_str(&format!(
        "
## Cauda da última saída de testes
{}
",
        scrub_text(secrets, &evidence_tail(&evidence, MAX_TEST_TAIL_BYTES))
    ));
    Some(DiagFacts {
        facts: scrub_text(secrets, &truncate_bytes(&out, MAX_FACTS_BYTES)),
        already,
    })
}

/// Diagnóstico do Arquiteto para tarefa BLOCKED por ambiente: no máximo um por (tarefa, motivo).
/// Somente leitura; a decisão é sempre escalar (nunca reabre a tarefa nem conta como ciclo de
/// recuperação). Falha, timeout ou JSON inválido: sem diagnóstico, comportamento anterior.
pub async fn diagnose(
    scope: &Scope<'_>,
    runner: &Arc<dyn SpecRunner>,
    task_id: &str,
    reason: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> Next {
    if *shutdown.borrow() {
        return Next::Shutdown;
    }
    let (root, spec_id, tid) = (scope.root.to_path_buf(), scope.spec_id.to_string(), task_id.to_string());
    let (secrets, reason_owned) = (Arc::clone(scope.secrets), reason.to_string());
    let loaded = tokio::task::spawn_blocking(move || load_diagnosis(&root, &spec_id, &tid, &reason_owned, &secrets))
        .await
        .ok()
        .flatten();
    let Some(loaded) = loaded else {
        return Next::Stay;
    };
    if loaded.already {
        return Next::Stay;
    }
    let answer = match ask_raw(scope, &loaded.facts, task_id, shutdown, true).await {
        Ok(answer) => answer,
        Err(Stop::Shutdown) => return Next::Shutdown,
        Err(Stop::Unavailable) => return Next::Stay,
    };
    let Some(diagnosis) = parse_diagnosis(&answer) else {
        return Next::Stay;
    };
    let secrets = scope.secrets;
    let text = format!(
        "{}
- Causa raiz: {}
- O que fazer: {}",
        reason_line(&reason_key(secrets, reason)),
        clean(secrets, &diagnosis.root_cause),
        clean(secrets, &diagnosis.what_to_do)
    );
    let (spec_id, tid, body) = (scope.spec_id.to_string(), task_id.to_string(), text.clone());
    let runner_job = Arc::clone(runner);
    let recorded = tokio::task::spawn_blocking(move || runner_job.record_diagnosis(&spec_id, &tid, &body))
        .await
        .ok()
        .and_then(Result::ok);
    let Some(n) = recorded else {
        return Next::Stay;
    };
    let message = format!("Diagnóstico de ambiente #{n}
{text}");
    deliver(
        scope.events,
        Some(scope.dialog),
        ApiEvent::new(
            EventType::AgentMessage,
            Some(scope.project_id.to_string()),
            json!({
                "agent": ARCHITECT_AGENT,
                "text": scrub_text(scope.secrets, &message),
                "task_id": task_id,
                "attempt": Value::Null,
                "seq": Value::Null,
            }),
        ),
    );
    Next::Stay
}

/// Escolha do humano num cartão de escalada (spec 073).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// `id` de uma opção e número da análise que a ofereceu; precisa ser a ÚLTIMA (o conteúdo vem
    /// do servidor, nunca do cliente).
    Option(String, usize),
    /// Texto livre (já validado: não vazio, até [`MAX_OPTION_TEXT_BYTES`]).
    Guidance(String),
}

/// Falha ao aplicar a escolha; o chamador traduz em status HTTP com mensagens estáticas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceError {
    /// Tarefa inexistente.
    NotFound,
    /// A tarefa não está BLOCKED.
    NotBlocked,
    /// A opção não existe na última análise, venceu ou deixou de ser válida.
    Stale,
    /// Falha de leitura ou escrita.
    Io,
}

/// Opções vigentes da tarefa: as da última análise de recuperação, se nada (retomada, revisão) as
/// venceu depois. JSON ilegível ou campos inconsistentes: nenhuma.
pub fn current_options(body: &str) -> Vec<RecoveryOption> {
    current_options_numbered(body).map(|(_, options)| options).unwrap_or_default()
}

/// Como [`current_options`], com o número da análise que as ofereceu (a UI o devolve na escolha).
pub fn current_options_numbered(body: &str) -> Option<(usize, Vec<RecoveryOption>)> {
    let (n, json) = latest_options_block(body)?;
    let options = serde_json::from_str::<Vec<RecoveryOption>>(&json).ok()?;
    (!options.is_empty()).then_some((n, options))
}

/// Texto da tarefa (e das settings) antes de aplicar uma escolha, para compensar se a retomada falhar.
pub struct Snapshot {
    task_rel: String,
    task: String,
    settings: Option<String>,
}

/// Lê o estado a restaurar; `None` se a tarefa não é legível.
pub fn snapshot(root: &Path, spec_id: &str, task_id: &str) -> Option<Snapshot> {
    let task_rel = format!(".sdd/specs/{spec_id}/tasks/{task_id}.md");
    let task = safe_fs::read_regular(root, &task_rel, MAX_READ_BYTES).ok()?;
    let settings = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES).ok();
    Some(Snapshot { task_rel, task, settings })
}

/// Restaura a tarefa e, se `with_settings`, as settings ao texto do [`Snapshot`].
pub fn restore(root: &Path, snap: &Snapshot, with_settings: bool) {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = replace_atomic(root, &snap.task_rel, snap.task.as_bytes());
    if let (true, Some(settings)) = (with_settings, &snap.settings) {
        let _ = replace_atomic(root, SETTINGS_REL, settings.as_bytes());
    }
}

/// Lê a tarefa BLOCKED de uma escolha.
fn load_blocked(root: &Path, spec_id: &str, task_id: &str) -> Result<(String, Task), ChoiceError> {
    let rel = format!(".sdd/specs/{spec_id}/tasks/{task_id}.md");
    let raw = match safe_fs::read_regular(root, &rel, MAX_READ_BYTES) {
        Ok(raw) => raw,
        Err(safe_fs::SafeFsError::Missing) => return Err(ChoiceError::NotFound),
        Err(_) => return Err(ChoiceError::Io),
    };
    let task = Task::parse(&raw).map_err(|_| ChoiceError::Io)?;
    if task.frontmatter.status != TaskStatus::Blocked {
        return Err(ChoiceError::NotBlocked);
    }
    Ok((rel, task))
}

/// Opção escolhida, da ÚLTIMA análise, revalidada pelo servidor (o conteúdo nunca vem do cliente).
fn resolve_option(
    root: &Path,
    task: &Task,
    id: &str,
    analysis: usize,
    secrets: &SecretSet,
    docker: DockerProbe<'_>,
) -> Result<RecoveryOption, ChoiceError> {
    let (n, stored) = current_options_numbered(&task.body).ok_or(ChoiceError::Stale)?;
    if n != analysis {
        return Err(ChoiceError::Stale);
    }
    let stored = stored.into_iter().find(|o| o.id == id).ok_or(ChoiceError::Stale)?;
    // Mesma validação da geração (tamanho, privilégio, scrub, guarda, regra ambiental, docker).
    let environmental = command_did_not_run(task.frontmatter.test_evidence.as_deref().unwrap_or_default());
    validate_option(root, secrets, stored, environmental, &[], docker).map_err(|_| ChoiceError::Stale)
}

/// Se a escolha é uma opção `environment` com `files`, devolve a opção revalidada: ela exige
/// aprovação do ambiente e adoção pelo servidor ([`adjust_environment`]) ANTES de [`apply_choice`].
pub fn env_files_option(
    root: &Path,
    spec_id: &str,
    task_id: &str,
    choice: &Choice,
    secrets: &SecretSet,
    docker: DockerProbe<'_>,
) -> Result<Option<RecoveryOption>, ChoiceError> {
    let Choice::Option(id, analysis) = choice else {
        return Ok(None);
    };
    let (_, task) = load_blocked(root, spec_id, task_id)?;
    let option = resolve_option(root, &task, id, *analysis, secrets, docker)?;
    Ok((option.kind == OptionKind::Environment && option.files.is_some()).then_some(option))
}

/// Aplica a escolha à tarefa BLOCKED e devolve o tipo aplicado (`config`, `environment`, `guidance`,
/// `revise_task` ou `guidance` para texto livre). Efeitos:
/// - `config`: valida de novo (guarda + política) e grava `test_command`/`checks` em settings;
/// - `guidance`/`environment`/texto livre: registra a entrada em "## Orientação do humano";
/// - `environment` com `files`: o servidor JÁ adotou o ambiente (`adopted`, ver [`adjust_environment`]);
///   sem isso a escolha é recusada (Io). O registro diz que o ambiente foi migrado;
/// - `revise_task`: reescreve objetivo/critérios/escopo.
///
/// Não muda o status nem grava a marca de retomada: quem retoma (`reset_plan`) faz as duas coisas.
///
/// O cabeçalho da seção (`## Orientação do humano`) é lido pelo runner e fica fixo; as entradas
/// são texto livre e saem no idioma do projeto, `lang` (spec 115 T04).
#[allow(clippy::too_many_arguments)]
pub fn apply_choice(
    root: &Path,
    spec_id: &str,
    task_id: &str,
    choice: &Choice,
    secrets: &SecretSet,
    docker: DockerProbe<'_>,
    adopted: bool,
    lang: crate::i18n::Lang,
) -> Result<&'static str, ChoiceError> {
    let tr = |key: &str, params: &[(&str, &str)]| crate::i18n::tr(lang, key, params);
    let (rel, mut task) = load_blocked(root, spec_id, task_id)?;
    let applied = match choice {
        Choice::Guidance(text) => {
            let entry = tr("recovery.choice.guidance", &[("text", &scrub_text(secrets, text.trim()))]);
            task.body = add_human_guidance(&task.body, &entry);
            OptionKind::Guidance.as_str()
        }
        Choice::Option(id, analysis) => {
            let option = resolve_option(root, &task, id, *analysis, secrets, docker)?;
            match option.kind {
                OptionKind::Config => {
                    let proposal =
                        ConfigProposal { test_command: option.test_command.clone(), checks: option.checks.clone() };
                    let change = validate_config(root, &proposal, secrets).map_err(|_| ChoiceError::Stale)?;
                    write_config(root, &change, secrets).map_err(|_| ChoiceError::Io)?;
                    let entry = tr(
                        "recovery.choice.config",
                        &[("label", &option.label), ("commands", &change.commands.join(" | "))],
                    );
                    task.body = add_human_guidance(&task.body, &scrub_text(secrets, &entry));
                }
                OptionKind::Guidance => {
                    let entry =
                        tr("recovery.choice.option", &[("label", &option.label), ("text", option.text.as_deref().unwrap_or_default())]);
                    task.body = add_human_guidance(&task.body, &entry);
                }
                OptionKind::Environment => {
                    let text = option.text.as_deref().unwrap_or_default();
                    let entry = match &option.files {
                        Some(files) if adopted => {
                            let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
                            tr(
                                "recovery.choice.environment_adopted",
                                &[("label", &option.label), ("files", &names.join(", ")), ("text", text)],
                            )
                        }
                        Some(_) => return Err(ChoiceError::Io),
                        None => tr("recovery.choice.environment_note", &[("label", &option.label), ("text", text)]),
                    };
                    task.body = add_human_guidance(&task.body, &scrub_text(secrets, &entry));
                }
                OptionKind::ReviseTask => {
                    let revision = TaskRevision {
                        objective: option.objective.clone().unwrap_or_default(),
                        acceptance: option.acceptance.clone().unwrap_or_default(),
                        allowed_scope: option.allowed_scope.clone(),
                    };
                    apply_revision(&mut task, &revision);
                    let entry = tr("recovery.choice.revised", &[("label", &option.label)]);
                    task.body = add_human_guidance(&task.body, &entry);
                }
            }
            option.kind.as_str()
        }
    };
    let rendered = task.render().map_err(|_| ChoiceError::Io)?;
    replace_atomic(root, &rel, rendered.as_bytes()).map_err(|_| ChoiceError::Io)?;
    Ok(applied)
}

async fn wait_shutdown(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

/// Valida o JSON estrito da resposta: objeto só com os campos conhecidos, `decision` no enum,
/// `root_cause` sempre, `strategy` em RETRY/REVISE_TASK, cada campo não vazio e com até
/// [`MAX_FIELD_BYTES`]. REVISE_TASK exige `objective` e `acceptance` (até [`MAX_REVISION_BYTES`])
/// e aceita `allowed_scope`; a revisão inteira passa por [`TaskRevision::validate`]. RETRY,
/// REVISE_TASK e REVISE_CONFIG aceitam `scope_add` (lista de caminhos; ESCALATE o ignora). Inválido: `None`.
pub fn parse_decision(raw: &str) -> Option<Decision> {
    let value: Value = serde_json::from_str(raw.trim()).ok()?;
    let map = value.as_object()?;
    const KEYS: [&str; 14] = [
        "new_tasks",
        "findings",
        "question",
        "scope_add",
        "options",
        "test_command",
        "checks",
        "decision",
        "root_cause",
        "strategy",
        "proposal",
        "objective",
        "acceptance",
        "allowed_scope",
    ];
    if map.keys().any(|k| !KEYS.contains(&k.as_str())) {
        return None;
    }
    let field = |name: &str, max: usize| -> Result<Option<String>, ()> {
        match map.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if !s.trim().is_empty() && s.len() <= max => Ok(Some(s.trim().to_string())),
            Some(_) => Err(()),
        }
    };
    let root_cause = field("root_cause", MAX_FIELD_BYTES).ok()??;
    let strategy = field("strategy", MAX_FIELD_BYTES).ok()?;
    let proposal = field("proposal", MAX_FIELD_BYTES).ok()?;
    let objective = field("objective", MAX_REVISION_BYTES).ok()?;
    let acceptance = field("acceptance", MAX_REVISION_BYTES).ok()?;
    let scope_value = map.get("allowed_scope").filter(|v| !v.is_null());
    let decision = map.get("decision")?.as_str()?;
    // `scope_add`: lista de strings (o conteúdo é validado ao registrar); ignorado em ESCALATE.
    let scope_add: Vec<String> = match map.get("scope_add") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(|v| v.as_str().map(str::to_string)).collect::<Option<Vec<_>>>()?,
        Some(_) => return None,
    };
    let has_revision_fields = objective.is_some() || acceptance.is_some() || scope_value.is_some();
    let config_value = map.get("test_command").filter(|v| !v.is_null());
    let checks_value = map.get("checks").filter(|v| !v.is_null());
    if decision != "REVISE_CONFIG" && (config_value.is_some() || checks_value.is_some()) {
        return None;
    }
    // Campos da spec 124 T03: cada um só na sua decisão.
    let present = |name: &str| map.get(name).is_some_and(|v| !v.is_null());
    if (decision != "SPLIT" && present("new_tasks"))
        || (decision != "ADJUST" && present("findings"))
        || (decision != "ASK" && present("question"))
    {
        return None;
    }
    let scope_list = |value: Option<&Value>| -> Option<Option<Vec<String>>> {
        match value.filter(|v| !v.is_null()) {
            None => Some(None),
            Some(Value::Array(items)) => Some(Some(items.iter().map(|v| v.as_str().map(str::to_string)).collect::<Option<Vec<_>>>()?)),
            Some(_) => None,
        }
    };
    let retry_with = |change: Change, revise: Option<TaskRevision>, strategy: String| Decision {
        retry: true,
        revise,
        root_cause: root_cause.clone(),
        strategy: Some(strategy),
        proposal: proposal.clone(),
        config: None,
        options: Vec::new(),
        discarded: Vec::new(),
        migrate: None,
        scope_add: scope_add.clone(),
        change: Some(change),
    };
    match decision {
        "SPLIT" => {
            let current = TaskRevision { objective: objective?, acceptance: acceptance?, allowed_scope: scope_list(scope_value)? };
            let mut new_tasks = Vec::new();
            for item in map.get("new_tasks")?.as_array()? {
                let o = item.as_object()?;
                if o.keys().any(|k| !["title", "objective", "acceptance", "allowed_scope"].contains(&k.as_str())) {
                    return None;
                }
                let text = |name: &str| o.get(name)?.as_str().map(|s| s.trim().to_string());
                new_tasks.push(SplitTask {
                    title: text("title")?,
                    objective: text("objective")?,
                    acceptance: text("acceptance")?,
                    allowed_scope: scope_list(o.get("allowed_scope"))??,
                });
            }
            let split = TaskSplit { current, new_tasks };
            split.validate().ok()?;
            Some(retry_with(Change::Split(split), None, strategy?))
        }
        "ADJUST" if objective.is_none() && scope_value.is_none() => {
            let mut findings = Vec::new();
            match map.get("findings").filter(|v| !v.is_null()) {
                None => {}
                Some(Value::Array(items)) => {
                    for item in items {
                        let o = item.as_object()?;
                        if o.keys().any(|k| !["id", "decision", "reason"].contains(&k.as_str())) {
                            return None;
                        }
                        let id = o.get("id")?.as_str()?.trim().to_string();
                        let decision = ArbitrationDecision::from_code(o.get("decision")?.as_str()?)?;
                        let reason = o.get("reason").map_or(Some(""), Value::as_str)?.split_whitespace().collect::<Vec<_>>().join(" ");
                        findings.push((id, decision, reason));
                    }
                }
                Some(_) => return None,
            }
            let adjustment = CriteriaAdjustment { acceptance: acceptance?, findings, reason: root_cause.clone() };
            adjustment.validate().ok()?;
            Some(retry_with(Change::Adjust(adjustment), None, strategy?))
        }
        "ASK" if !has_revision_fields => {
            let question = field("question", plan::MAX_QUESTION_BYTES).ok()??;
            let (options, discarded) = parse_options(map.get("options"));
            Some(Decision {
                retry: false,
                revise: None,
                root_cause,
                strategy,
                proposal,
                config: None,
                options,
                discarded,
                migrate: None,
                scope_add: Vec::new(),
                change: Some(Change::Ask(question)),
            })
        }
        "REVISE_CONFIG" if !has_revision_fields => {
            let test_command = match config_value {
                None => None,
                Some(Value::String(c)) if !c.trim().is_empty() => Some(c.trim().to_string()),
                Some(_) => return None,
            };
            let checks = match checks_value {
                None => None,
                Some(Value::Array(items)) => {
                    let mut out = Vec::with_capacity(items.len());
                    for item in items {
                        let o = item.as_object()?;
                        if o.keys().any(|k| !["name", "command", "blocking"].contains(&k.as_str())) {
                            return None;
                        }
                        out.push(Check {
                            name: o.get("name")?.as_str()?.to_string(),
                            command: o.get("command")?.as_str()?.to_string(),
                            blocking: o.get("blocking").map_or(Some(false), Value::as_bool)?,
                        });
                    }
                    Some(out)
                }
                Some(_) => return None,
            };
            if test_command.is_none() && checks.is_none() {
                return None;
            }
            Some(Decision {
                retry: true,
                revise: None,
                root_cause,
                strategy,
                proposal,
                config: Some(ConfigProposal { test_command, checks }),
                options: Vec::new(),
                discarded: Vec::new(),
                migrate: None,
                scope_add,
                change: None,
            })
        }
        "RETRY" if !has_revision_fields => Some(Decision {
            retry: true,
            revise: None,
            root_cause,
            strategy: Some(strategy?),
            proposal,
            config: None,
            options: Vec::new(),
            discarded: Vec::new(),
            migrate: None,
            scope_add,
            change: None,
        }),
        "ESCALATE" if !has_revision_fields => {
            let (options, discarded) = parse_options(map.get("options"));
            Some(Decision {
                retry: false,
                revise: None,
                root_cause,
                strategy,
                proposal,
                config: None,
                options,
                discarded,
                migrate: None,
                scope_add: Vec::new(),
                change: None,
            })
        }
        "REVISE_TASK" => {
            let allowed_scope = match scope_value {
                None => None,
                Some(Value::Array(items)) => Some(
                    items
                        .iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()?,
                ),
                Some(_) => return None,
            };
            let revision = TaskRevision {
                objective: objective?,
                acceptance: acceptance?,
                allowed_scope,
            };
            revision.validate().ok()?;
            Some(Decision {
                retry: true,
                revise: Some(revision),
                root_cause,
                strategy: Some(strategy?),
                proposal,
                config: None,
                options: Vec::new(),
                discarded: Vec::new(),
                migrate: None,
                scope_add,
                change: None,
            })
        }
        _ => None,
    }
}

/// Opções do JSON do modelo: ao menos o formato (campos conhecidos, tipos) e o máximo de
/// [`MAX_OPTIONS`]. O conteúdo é validado depois por [`validate_options`]. Devolve também os motivos
/// dos itens descartados já aqui.
fn parse_options(value: Option<&Value>) -> (Vec<RecoveryOption>, Vec<String>) {
    let mut options = Vec::new();
    let mut discarded = Vec::new();
    match value {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                if i >= MAX_OPTIONS {
                    discarded.push(format!("opção {}: acima do máximo de {MAX_OPTIONS}", i + 1));
                    continue;
                }
                match serde_json::from_value::<RecoveryOption>(item.clone()) {
                    // O antes/depois é sempre calculado pelo servidor.
                    Ok(option) => options.push(RecoveryOption { diff: None, ..option }),
                    Err(_) => discarded.push(format!("opção {}: formato inválido", i + 1)),
                }
            }
        }
        Some(_) => discarded.push("opções: formato inválido (esperava uma lista)".to_string()),
    }
    (options, discarded)
}

/// Tokens de elevação de privilégio, proibidos em qualquer texto de opção.
fn mentions_privilege(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|w| ["sudo", "su", "doas", "pkexec"].contains(&w.to_lowercase().as_str()))
}

/// Texto livre de opção: não vazio, até `max` bytes, redigido, sem privilégio.
fn option_text(secrets: &SecretSet, value: Option<&String>, what: &str, max: usize) -> Result<String, String> {
    let text = value.map(|t| t.trim()).filter(|t| !t.is_empty()).ok_or_else(|| format!("{what} ausente"))?;
    if text.len() > max {
        return Err(format!("{what} grande demais"));
    }
    if mentions_privilege(text) {
        return Err(format!("{what} pede elevação de privilégio"));
    }
    Ok(scrub_text(secrets, text))
}

/// Sonda lazy da disponibilidade do Docker (prereqs); só é chamada para opções com `files`.
pub type DockerProbe<'a> = &'a dyn Fn() -> bool;

/// Valida os arquivos de ambiente de uma opção `environment` (spec 074): só `.nants/compose.yaml` e
/// `.nants/Dockerfile*`, sem repetição, cada um por `validate_env_file`, o conjunto por
/// `check_build_paths`, com `compose.yaml` presente (proposto ou já existente) e Docker disponível.
/// Devolve o diff (proposta contra o estado atual) para a UI.
fn validate_env_files(root: &Path, files: &[EnvFile], docker: DockerProbe<'_>) -> Result<String, String> {
    use crate::sdd::environment as env;
    let mut seen: Vec<&str> = Vec::new();
    for f in files {
        if !env::is_env_file(&f.path) {
            return Err("files: só .nants/compose.yaml e .nants/Dockerfile* são aceitos".to_string());
        }
        if seen.contains(&f.path.as_str()) {
            return Err(format!("files: {} repetido", f.path));
        }
        seen.push(&f.path);
        env::validate_env_file(&f.path, &f.content).map_err(|m| format!("files: {}: {m}", f.path))?;
    }
    let current = env::read_env_files(root);
    if !seen.contains(&env::COMPOSE_REL) && !current.iter().any(|(rel, _)| rel == env::COMPOSE_REL) {
        return Err("files: falta .nants/compose.yaml".to_string());
    }
    let proposed: Vec<(String, String)> = files.iter().map(|f| (f.path.clone(), f.content.clone())).collect();
    env::check_build_paths(root, &proposed).map_err(|m| format!("files: {m}"))?;
    if !docker() {
        return Err(CAUSE_DOCKER_UNAVAILABLE.to_string());
    }
    let diff: Vec<String> = files
        .iter()
        .map(|f| {
            let old = current.iter().find(|(rel, _)| *rel == f.path).map(|(_, c)| c.as_str()).unwrap_or_default();
            env::unified_diff(&f.path, old, &f.content)
        })
        .filter(|d| !d.is_empty())
        .collect();
    Ok(diff.join("\n"))
}

/// Motivo do descarte de opção com `files` quando o Docker não está disponível (spec 074).
pub const CAUSE_DOCKER_UNAVAILABLE: &str = "docker indisponível";

/// Valida e normaliza uma opção; o `diff` das opções `config` vem de [`validate_config`] e o das
/// opções `environment` com `files`, de [`validate_env_files`].
fn validate_option(
    root: &Path,
    secrets: &SecretSet,
    option: RecoveryOption,
    environmental: bool,
    taken: &[RecoveryOption],
    docker: DockerProbe<'_>,
) -> Result<RecoveryOption, String> {
    let id = option.id.trim().to_string();
    if id.is_empty() || id.len() > MAX_OPTION_ID || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("id inválido".to_string());
    }
    if taken.iter().any(|t| t.id == id) {
        return Err("id repetido".to_string());
    }
    let label = scrub_text(secrets, &option.label.split_whitespace().collect::<Vec<_>>().join(" "));
    if label.is_empty() || label.chars().count() > MAX_OPTION_LABEL_CHARS || mentions_privilege(&label) {
        return Err("rótulo inválido".to_string());
    }
    let mut out = RecoveryOption {
        id,
        label,
        kind: option.kind,
        test_command: None,
        checks: None,
        text: None,
        objective: None,
        acceptance: None,
        allowed_scope: None,
        diff: None,
        files: None,
    };
    let has_text = option.text.is_some();
    let has_revision = option.objective.is_some() || option.acceptance.is_some() || option.allowed_scope.is_some();
    let has_config = option.test_command.is_some() || option.checks.is_some();
    if option.files.is_some() && option.kind != OptionKind::Environment {
        return Err("campos de outro tipo de opção".to_string());
    }
    match option.kind {
        OptionKind::Config => {
            if has_text || has_revision {
                return Err("campos de outro tipo de opção".to_string());
            }
            if !has_config {
                return Err("config sem test_command nem checks".to_string());
            }
            let proposal = ConfigProposal { test_command: option.test_command, checks: option.checks };
            let change = validate_config(root, &proposal, secrets)?;
            out.test_command = change.test_command;
            out.checks = change.checks;
            out.diff = Some(change.diff);
        }
        OptionKind::Environment | OptionKind::Guidance => {
            if has_config || has_revision {
                return Err("campos de outro tipo de opção".to_string());
            }
            out.text = Some(option_text(secrets, option.text.as_ref(), "texto", MAX_OPTION_TEXT_BYTES)?);
            if let Some(files) = option.files {
                // Spec 129 T05: o compose proposto ganha `${NANTS_WORKTREE:-..}` (roda sem variável fora do Nants).
                let files: Vec<EnvFile> = files
                    .into_iter()
                    .map(|mut f| {
                        if f.path == crate::sdd::environment::COMPOSE_REL {
                            f.content = crate::sdd::environment::with_worktree_default(&f.content);
                        }
                        f
                    })
                    .collect();
                if files.is_empty() || files.len() > MAX_OPTION_FILES {
                    return Err(format!("files deve ter de 1 a {MAX_OPTION_FILES} itens"));
                }
                if files.iter().any(|f| f.path.contains("<!--") || f.path.contains("-->") || f.content.contains("<!--") || f.content.contains("-->")) {
                    return Err("files com delimitadores reservados".to_string());
                }
                if files.iter().any(|f| f.path.len() > MAX_OPTION_FILE_PATH_BYTES) {
                    return Err(format!("files: caminho acima de {MAX_OPTION_FILE_PATH_BYTES} bytes"));
                }
                if files.iter().any(|f| f.content.len() > MAX_OPTION_FILE_BYTES) {
                    return Err(format!("files: arquivo acima de {MAX_OPTION_FILE_BYTES} bytes"));
                }
                if files.iter().map(|f| f.content.len()).sum::<usize>() > MAX_OPTION_FILES_TOTAL_BYTES {
                    return Err(format!("files: total acima de {MAX_OPTION_FILES_TOTAL_BYTES} bytes"));
                }
                // Segredo no arquivo: rejeita (um arquivo redigido não pode ser aplicado depois).
                if files.iter().any(|f| scrub_text(secrets, &f.path) != f.path || scrub_text(secrets, &f.content) != f.content) {
                    return Err("files contém segredo".to_string());
                }
                out.diff = Some(validate_env_files(root, &files, docker)?);
                out.files = Some(files);
            }
        }
        OptionKind::ReviseTask => {
            if has_config || has_text {
                return Err("campos de outro tipo de opção".to_string());
            }
            if environmental {
                return Err(CAUSE_ENV_REVISE.to_string());
            }
            let revision = TaskRevision {
                objective: option_text(secrets, option.objective.as_ref(), "objetivo", MAX_REVISION_BYTES)?,
                acceptance: option_text(secrets, option.acceptance.as_ref(), "critérios", MAX_REVISION_BYTES)?,
                allowed_scope: option.allowed_scope,
            };
            revision.validate()?;
            out.objective = Some(revision.objective);
            out.acceptance = Some(revision.acceptance);
            out.allowed_scope = revision.allowed_scope.map(|s| s.iter().map(|g| g.trim().to_string()).collect());
        }
    }
    Ok(out)
}

/// Valida as opções do modelo, uma a uma: as inválidas são descartadas com o motivo. Devolve as
/// válidas (ordem original) e os motivos.
fn validate_options(
    root: &Path,
    secrets: &SecretSet,
    options: Vec<RecoveryOption>,
    environmental: bool,
    docker: DockerProbe<'_>,
) -> (Vec<RecoveryOption>, Vec<String>) {
    let mut valid: Vec<RecoveryOption> = Vec::new();
    let mut discarded = Vec::new();
    for (i, option) in options.into_iter().enumerate() {
        match validate_option(root, secrets, option, environmental, &valid, docker) {
            Ok(option) => valid.push(option),
            Err(reason) => discarded.push(format!("opção {}: {}", i + 1, reason)),
        }
    }
    (valid, discarded)
}

/// Texto da seção (sem o cabeçalho): redigido, e sem linha iniciada por `#` (não cria seções).
fn section_body(secrets: &SecretSet, decision: &Decision) -> String {
    // Causa fixa de escalada determinística em decisão RETRY (vinda do modelo) é neutralizada: não
    // pode se passar por seção que não conta como ciclo.
    let fixed = [CAUSE_LIMIT, CAUSE_ENV_ROUNDS, CAUSE_UNAVAILABLE].contains(&decision.root_cause.as_str());
    let cause = if decision.retry && fixed {
        format!("(citada pelo modelo) {}", decision.root_cause)
    } else {
        decision.root_cause.clone()
    };
    let mut lines = vec![format!("- Causa raiz: {}", clean(secrets, &cause))];
    if let Some(change) = &decision.change {
        match change {
            Change::Split(split) => {
                lines.push(plan::SPLIT_DECISION_LINE.to_string());
                if let Some(scope) = &split.current.allowed_scope {
                    lines.push(format!("- Novo allowed_scope: {}", clean(secrets, &scope.join(", "))));
                }
            }
            Change::Adjust(_) => lines.push(plan::ADJUST_DECISION_LINE.to_string()),
            Change::Ask(question) => {
                lines.push(plan::ASK_DECISION_LINE.to_string());
                lines.push(format!("{}{}", plan::QUESTION_LABEL, clean(secrets, question)));
            }
        }
        if let (true, Some(strategy)) = (decision.retry, &decision.strategy) {
            lines.push(format!("- Estratégia: {}", clean(secrets, strategy)));
        }
    } else if let Some(config) = &decision.config {
        lines.push("- Decisão: configuração de testes revisada (REVISE_CONFIG)".to_string());
        if let Some(strategy) = &decision.strategy {
            lines.push(format!("- Estratégia: {}", clean(secrets, strategy)));
        }
        if let Some(t) = &config.test_command {
            lines.push(format!("- Novo test_command: {}", clean(secrets, t)));
        }
        if let Some(checks) = &config.checks {
            lines.push(format!("- Novos checks: {}", clean(secrets, &render_checks(checks).replace('\n', "; "))));
        }
        lines.push(CONFIG_APPLIED_LINE.to_string());
    } else if let Some(revision) = &decision.revise {
        lines.push("- Decisão: tarefa revisada (REVISE_TASK)".to_string());
        if let Some(strategy) = &decision.strategy {
            lines.push(format!("- Estratégia: {}", clean(secrets, strategy)));
        }
        if let Some(scope) = &revision.allowed_scope {
            lines.push(format!("- Novo allowed_scope: {}", clean(secrets, &scope.join(", "))));
        }
    } else if decision.retry {
        lines.push("- Decisão: nova estratégia (RETRY)".to_string());
        if let Some(strategy) = &decision.strategy {
            lines.push(format!("- Estratégia: {}", clean(secrets, strategy)));
        }
    } else {
        lines.push("- Decisão: escalar ao humano".to_string());
    }
    if decision.retry && !decision.scope_add.is_empty() {
        // A linha "Escopo ampliado" é aplicada ao `allowed_scope` pelo runner ao registrar a decisão.
        let (ok, bad) = crate::agents::runner::validate_scope_add(&decision.scope_add);
        if !ok.is_empty() {
            lines.push(format!("{SCOPE_ADDED_LINE}{}", clean(secrets, &ok.join(", "))));
        }
        if !bad.is_empty() {
            lines.push(format!("- Escopo recusado: {}", clean(secrets, &bad.join(", "))));
        }
    }
    if !decision.retry {
        if let Some(strategy) = &decision.strategy {
            lines.push(format!("- Estratégia sugerida: {}", clean(secrets, strategy)));
        }
        if let Some(proposal) = &decision.proposal {
            lines.push(format!("- Proposta: {}", clean(secrets, proposal)));
        }
        if !decision.options.is_empty() {
            lines.push("- Opções para o humano:".to_string());
            for option in &decision.options {
                lines.push(format!("  - [{}] {}: {}", option.kind.as_str(), option.id, clean(secrets, &option.label)));
            }
            lines.push(OPTIONS_OPEN.to_string());
            lines.push(serde_json::to_string(&decision.options).unwrap_or_else(|_| "[]".to_string()));
            lines.push(OPTIONS_CLOSE.to_string());
        }
        if !decision.discarded.is_empty() {
            lines.push(format!("- Opções descartadas: {}", clean(secrets, &decision.discarded.join("; "))));
        }
    }
    lines.join("\n")
}

/// Redige segredos e recua as linhas seguintes à primeira (nunca começam com `#`).
fn clean(secrets: &SecretSet, text: &str) -> String {
    // Defesa em profundidade: texto do modelo não forja os delimitadores do bloco de opções.
    let scrubbed = scrub_text(secrets, text.trim()).replace("<!--", "< !--").replace("-->", "-- >");
    let mut out = String::new();
    for (i, line) in scrubbed.lines().enumerate() {
        if i > 0 {
            out.push_str("\n  ");
        }
        out.push_str(line.trim_end());
    }
    out
}

/// `true` quando a evidência indica que o comando de testes não executou de fato. Decide SÓ pelo
/// cabeçalho estruturado da primeira linha (`[falha dos testes: código N]` ou `: tempo limite]`):
/// código 126/127 ou tempo limite. Sem cabeçalho ou outro código: qualidade.
pub fn command_did_not_run(evidence: &str) -> bool {
    let first = evidence.lines().next().unwrap_or("").trim();
    if !first.starts_with(TEST_FAILURE_MARK) {
        return false;
    }
    first == format!("{TEST_FAILURE_MARK}: tempo limite]")
        || first == format!("{TEST_FAILURE_MARK}: código 126]")
        || first == format!("{TEST_FAILURE_MARK}: código 127]")
}

/// Cauda de `text` com até `max` bytes (em fronteira de caractere).
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Cauda da evidência de testes para a análise: a saída do runner (antes da seção de artefatos da
/// falha, spec 079) tem a própria cauda de `max` bytes, e a seção `## Artefatos da falha` vem à
/// parte, do início, com até `max` mais 1/4 (cabeçalhos dos arquivos). Sem a seção, é a cauda simples.
fn evidence_tail(text: &str, max: usize) -> String {
    let marker = format!("\n{}\n", crate::sdd::failure_artifacts::SECTION);
    let Some(at) = text.rfind(&marker) else {
        return tail(text, max).to_string();
    };
    let (output, artifacts) = (text[..at].trim_end(), text[at..].trim());
    let budget = max + max / 4;
    let shown = truncate_bytes(artifacts, budget);
    let mut out = format!("{}\n\n{shown}", tail(output, max));
    if shown.len() < artifacts.len() {
        out.push_str("\n[… artefatos truncados]");
    }
    out
}

/// Lê a tarefa e monta os fatos (redigidos, limitados). `None` se a tarefa não é legível ou não
/// está bloqueada.
fn load(root: &Path, spec_id: &str, task_id: &str, secrets: &SecretSet) -> Option<Loaded> {
    let rel = format!(".sdd/specs/{spec_id}/tasks/{task_id}.md");
    let raw = safe_fs::read_regular(root, &rel, MAX_READ_BYTES).ok()?;
    let task = Task::parse(&raw).ok()?;
    if task.frontmatter.status != TaskStatus::Blocked {
        return None;
    }
    let meta = &task.frontmatter;
    let evidence = meta.test_evidence.clone().unwrap_or_default();
    let cap = |text: String, max: usize| truncate_bytes(&scrub_text(secrets, text.trim()), max);
    let section = |title: &str| section_text(&task.body, title).unwrap_or_default();
    let parsed = safe_fs::read_regular(root, ".sdd/settings.yaml", MAX_READ_BYTES)
        .ok()
        .and_then(|r| Settings::parse(&r).ok());
    let yolo = parsed.as_ref().is_some_and(|s| s.yolo);
    let config_facts = parsed.as_ref().map(|s| {
        format!(
            "ambiente: {}\ntest_command: {}\n{}{}",
            "docker (test_command e checks rodam DENTRO do serviço workspace)",
            s.test_command,
            render_checks(&s.checks),
            install_rule(s.install_command.as_deref())
        )
    });
    let command = parsed.as_ref().map(|s| s.test_command.clone()).unwrap_or_default();
    let install = parsed.as_ref().and_then(|s| s.install_command.clone());
    let base = parsed.map(|s| s.base_branch);
    let environmental = command_did_not_run(&evidence);
    let first_line = evidence.lines().next().unwrap_or("").trim();
    let exit_code = first_line
        .strip_prefix(TEST_FAILURE_MARK)
        .map(|rest| rest.trim_start_matches(':').trim().trim_end_matches(']').trim().trim_start_matches("código").trim().to_string())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| "não informado".to_string());
    let error_tail = scrub_text(secrets, &evidence_tail(evidence.as_str(), ENV_TAIL_BYTES)).trim().to_string();

    let mut out = String::new();
    out.push_str(&format!(
        "Tarefa {} (spec {spec_id}): {}\n",
        meta.id,
        cap(meta.title.clone(), 200)
    ));
    // O texto está no idioma do projeto (ou em pt-BR, nas tarefas antigas); o código estável
    // (spec 114 T02), quando houver, identifica o motivo sem depender do idioma.
    let block_code = meta.next_action_code.as_deref().map(|c| format!(" (código: {})", cap(c.to_string(), 40))).unwrap_or_default();
    out.push_str(&format!(
        "Motivo do bloqueio: {}{block_code}\n",
        cap(meta.next_action.clone().unwrap_or_default(), 500)
    ));
    out.push_str(&format!("allowed_scope: {}\n", cap(meta.allowed_scope.join(", "), 500)));
    out.push_str(&diff_growth_line(&meta.diff_lines));
    out.push_str(&format!("\n## Objetivo\n{}\n", cap(section(OBJECTIVE), MAX_SECTION_BYTES)));
    out.push_str(&format!(
        "\n## Critérios de aceitação\n{}\n",
        cap(section(ACCEPTANCE), MAX_SECTION_BYTES)
    ));
    out.push_str(&format!("\n## Findings abertos\n{}\n", cap(section(FINDINGS), MAX_SECTION_BYTES)));
    let previous = recovery_notes(&task.body);
    if !previous.is_empty() {
        out.push_str(&format!(
            "\n## Análises de recuperação anteriores (a nova estratégia deve ser DIFERENTE)\n{}\n",
            cap(previous, MAX_SECTION_BYTES * 2)
        ));
    }
    // Spec 124 T03: recorrência, contestações, tamanho, histórico e custo, para escolher entre RETRY,
    // SPLIT, ADJUST e ASK.
    out.push('\n');
    out.push_str(&cap(plan::signals(&task, &task_cost(root, spec_id, &meta.id)), MAX_SECTION_BYTES));
    out.push('\n');
    // Spec 124 T06: o aviso de custo (sinal, não limite) quando a tarefa passou do patamar.
    if let Some(alert) = super::cost_alert::task_alert(root, spec_id, &meta.id) {
        out.push_str(&super::cost_alert::architect_line(&alert));
        out.push('\n');
    }
    // Spec 124 T04: capacidades do ambiente, aviso de orçamento de tempo e rodadas instáveis.
    let caps = crate::sdd::capabilities::prompt_section(&crate::sdd::capabilities::read(root), crate::sdd::capabilities::Audience::Architect);
    if !caps.is_empty() {
        out.push_str(&format!("\n{}\n", cap(caps, MAX_SECTION_BYTES)));
    }
    let flaky = section(crate::agents::runner::FLAKY_SECTION);
    if !flaky.trim().is_empty() {
        out.push_str(&format!("\n## Rodadas instáveis desta tarefa (falharam e passaram ao repetir sem mudança)\n{}\n", cap(flaky, MAX_SECTION_BYTES)));
    }
    out.push_str(&mode_facts());
    if let Some(config) = config_facts {
        out.push_str(&format!("\n## Configuração de testes atual (REVISE_CONFIG pode corrigi-la)\n{}\n", cap(config, MAX_SECTION_BYTES)));
    }
    let spec_md = safe_fs::read_regular(root, &format!(".sdd/specs/{spec_id}/spec.md"), MAX_READ_BYTES).ok();
    // Spec 129 T02: o diagnóstico da persistência da instalação, com a regra de que o gravado fora
    // de /work se perde entre comandos.
    let footprint = spec_md.as_deref().map(crate::agents::runner::install_footprint_prompt).unwrap_or_default();
    if !footprint.is_empty() {
        out.push_str(&format!("\n{}\n", cap(footprint, MAX_SECTION_BYTES)));
    }
    let criteria = spec_md.as_deref().map(global_criteria).unwrap_or_default();
    if !criteria.trim().is_empty() {
        out.push_str(&format!(
            "\n## Critérios globais da spec (a verificação nunca pode ser rebaixada)\n{}\n",
            cap(criteria, MAX_SECTION_BYTES)
        ));
    }
    let shown = evidence.as_str();
    out.push_str(&format!(
        "\n## Cauda da última saída de testes\n{}\n",
        scrub_text(secrets, &evidence_tail(shown, MAX_TEST_TAIL_BYTES))
    ));
    out.push_str(&format!(
        "\n## Diff da branch contra a base (resumido)\n{}\n",
        scrub_text(secrets, &diff_patch(root, base.as_deref(), &meta.branch))
    ));
    out.push_str(&format!(
        "\n## Arquivos alterados na branch\n{}\n",
        diff_names(root, base.as_deref(), &meta.branch)
    ));
    Some(Loaded {
        cycles: recovery_cycles(&task.body),
        // Config docker que invoca docker/podman é falha de CONFIGURAÇÃO (corrigível), não de ambiente.
        environmental,
        env_note: if environmental { env_note(&command, &exit_code, &error_tail, install.as_deref()) } else { String::new() },
        command,
        exit_code,
        error_tail,
        config_suspect: config_suspect(root),
        yolo,
        rounds: count_marked_lines(&task.body, AUTONOMOUS_ROUND_PREFIX, LEGACY_AUTONOMOUS_ROUND_PREFIX),
        env_fixes: count_marked_lines(&task.body, ENV_FIX_PREFIX, LEGACY_ENV_FIX_PREFIX),
        allowed_scope: task.frontmatter.allowed_scope.clone(),
        facts: scrub_text(secrets, &truncate_bytes(&out, MAX_FACTS_BYTES)),
    })
}

/// Uso da tarefa até aqui, por papel, na ordem em que aparecem (`usage.jsonl` da spec).
pub(crate) fn task_cost(root: &Path, spec_id: &str, task_id: &str) -> Vec<plan::RoleCost> {
    task_cost_of(&super::usage::load_records(root, Some(spec_id)), task_id)
}

/// [`task_cost`] sobre registros já lidos (spec 124 T06: o aviso de custo lê o arquivo uma vez só
/// para todas as tarefas da spec).
pub(crate) fn task_cost_of(records: &[super::usage::UsageRecord], task_id: &str) -> Vec<plan::RoleCost> {
    let mut out: Vec<plan::RoleCost> = Vec::new();
    for r in records.iter().filter(|r| r.task.as_deref() == Some(task_id)) {
        let at = match out.iter().position(|c| c.agent == r.agent) {
            Some(at) => at,
            None => {
                out.push(plan::RoleCost { agent: r.agent.clone(), ..plan::RoleCost::default() });
                out.len() - 1
            }
        };
        if let Some(c) = out.get_mut(at) {
            c.calls += 1;
            c.input += r.input;
            c.output += r.output;
        }
    }
    out
}

/// Spec 087 T05: linha dos FATOS com o diff de código (sem testes nem docs) da 1ª e da última
/// tentativa revisada; vazia sem registro.
fn diff_growth_line(diff_lines: &[u64]) -> String {
    match diff_lines {
        [] => String::new(),
        [only] => format!("diff de código: tentativa 1 = {only} linhas\n"),
        [first, .., last] => format!("diff de código: tentativa 1 = {first} linhas, última = {last} linhas\n"),
    }
}

/// Bloco de fatos com o modo real do projeto e as ferramentas do Desenvolvedor nele (spec 074).
fn mode_facts() -> String {
    "\n## Modo do projeto: DOCKER\n\
     Ferramentas do Desenvolvedor: read_file, list_dir, search_code, write_file, apply_patch, run_command, \
     run_browser_tests, ask_architect e request_environment_change (altera .nants/compose.yaml e \
     .nants/Dockerfile* mediante aprovação do humano). test_command e checks rodam DENTRO do serviço workspace. \
     Se a imagem não tem um componente comprovado (navegador, biblioteca nativa, SDK), a saída é um AJUSTE do \
     ambiente: uma opção `environment` com `files` ([{\"path\",\"content\"}] para .nants/compose.yaml e \
     .nants/Dockerfile*), que o servidor valida e aplica após aprovação. Nunca sudo/su/doas/pkexec.\n"
        .to_string()
}

/// Raiz de leitura do turno: a worktree da tarefa (`.sdd/worktrees/<spec>/<task>`) quando é um
/// diretório real (nenhum componente é symlink) dentro do projeto; senão a raiz do projeto.
pub fn read_root(root: &Path, spec_id: &str, task_id: &str) -> PathBuf {
    let plain = |id: &str| !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\']);
    if !plain(spec_id) || !plain(task_id) {
        return root.to_path_buf();
    }
    let worktree = task_worktree_path(root, spec_id, task_id);
    let mut current = root.to_path_buf();
    for part in [".sdd", "worktrees", spec_id, task_id] {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            _ => return root.to_path_buf(),
        }
    }
    worktree
}

/// Diff (patch) da branch contra a base, limitado a [`MAX_DIFF_BYTES`]; falha vira "indisponível".
fn diff_patch(root: &Path, base: Option<&str>, branch: &str) -> String {
    const UNAVAILABLE: &str = "(indisponível)";
    let Some(base) = base.filter(|b| !b.is_empty() && !b.starts_with('-')) else {
        return UNAVAILABLE.into();
    };
    if branch.is_empty() || branch.starts_with('-') {
        return UNAVAILABLE.into();
    }
    let out = Command::new("git")
        .args(["diff", "--no-color", "--no-ext-diff", &format!("{base}...{branch}"), "--"])
        .current_dir(root)
        .output();
    match out {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            if text.trim().is_empty() {
                "(nenhuma alteração)".into()
            } else {
                truncate_bytes(&text, MAX_DIFF_BYTES)
            }
        }
        _ => UNAVAILABLE.into(),
    }
}

/// Nomes dos arquivos alterados pela branch em relação à base (limitado); falha vira "indisponível".
fn diff_names(root: &Path, base: Option<&str>, branch: &str) -> String {
    const UNAVAILABLE: &str = "(indisponível)";
    let Some(base) = base.filter(|b| !b.is_empty() && !b.starts_with('-')) else {
        return UNAVAILABLE.into();
    };
    if branch.is_empty() || branch.starts_with('-') {
        return UNAVAILABLE.into();
    }
    let out = Command::new("git")
        .args(["diff", "--name-status", &format!("{base}...{branch}"), "--"])
        .current_dir(root)
        .output();
    let Ok(out) = out else {
        return UNAVAILABLE.into();
    };
    if !out.status.success() {
        return UNAVAILABLE.into();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return "(nenhuma alteração)".into();
    }
    let mut shown = lines.iter().take(MAX_FILES).copied().collect::<Vec<_>>().join("\n");
    if lines.len() > MAX_FILES {
        shown.push_str(&format!("\n(+{} arquivos omitidos)", lines.len() - MAX_FILES));
    }
    shown
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
