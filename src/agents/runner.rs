//! Runner determinístico do ciclo de tarefa (BLUEPRINT §5.2), desacoplado do LLM por ports.
//!
//! Fluxo de uma tarefa: `TODO → IN_PROGRESS` (worktree `.sdd/worktrees/<spec>/<task>` na branch
//! `sdd/<spec>/<task>`) → Developer → commit pelo runner se sobrar alteração → `test_command` na
//! worktree (falha volta ao Developer, até 3 vezes por tentativa) → `IN_REVIEW` → Revisor →
//! `APPROVED` (grava `reviewed_sha` e `last_verdict`) ou `CHANGES_REQUESTED` (findings,
//! `attempts + 1`; ao chegar a `max_attempts` → `BLOCKED`) → `INTEGRATING` (squash merge na base,
//! `test_command` na base, commit `feat(<spec>): <task> - <título>` contendo somente o squash) →
//! grava `DONE` no disco e remove worktree e branch.
//!
//! Decisão de arquitetura: a verdade de "integrada" é o Git. Uma tarefa está concluída quando existe
//! na base um commit cujo assunto começa com `feat(<spec>): <task> - `. O status `DONE` em disco é
//! apenas cache: `DONE` sem commit é tratado como `INTEGRATING` (regrava o status e refaz a
//! integração pelos gates); commit sem `DONE` em disco é finalizado. Dependências só são liberadas
//! por commit, nunca por status em disco.
//!
//! Persistência: cada transição é gravada antes da próxima ação. Antes de gravar, o runner relê o
//! arquivo e compara com o último conteúdo que ele próprio gravou (ou leu no início da execução);
//! divergência resulta em `BLOCKED("... modificado externamente")` sem sobrescrever.
//!
//! Estado de trabalho: alterações não staged em `.sdd/specs/**` e em `.sdd/settings.yaml` (configuração
//! local) não sujam a base. Qualquer outra alteração fora de `.sdd` bloqueia a integração, e uma branch
//! que altere `.sdd/settings.yaml` é recusada antes do squash. Qualquer alteração
//! staged também bloqueia: com índice não vazio a integração é recusada com `BLOCKED` e nada é
//! tocado (sem desfazer squash remanescente; o humano resolve e reexecuta).
//!
//! Rollback: `squash_rollback(captured)` existe só para falhas da mesma execução logo após o próprio
//! squash (índice verificado vazio antes). `captured` é o `staged_status` capturado logo após o
//! squash; o rollback recusa atuar se o índice ou a árvore (fora de `.sdd/worktrees` e
//! `.sdd/specs`) divergirem dessa lista. Também recusa se algum path capturado tiver alteração do
//! humano (arquivo não rastreado, ou worktree diferente do índice; para `A`, conteúdo diferente do
//! blob do índice). Essas conferências ocorrem antes de qualquer restore ou rm, porque a janela entre
//! o squash e o rollback inclui o `test_command`. Sem `reset --hard`. Se o rollback não concluir, a
//! tarefa vai a `BLOCKED` com a mensagem indicando para conferir `git status`.
//!
//! Retomada com commit presente: a tarefa nunca vai a `BLOCKED` só por worktree suja. O status em
//! disco permanece `INTEGRATING` e o motivo é devolvido como `Outcome::Blocked`; ao limpar a
//! worktree e reexecutar, a tarefa é finalizada sem novo squash.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::future::Future;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::agents::llm_client::ProviderFailure;
use crate::agents::usage::BudgetGuard;
use crate::harness::compose::ComposeEnv;
use crate::harness::git::{Git, LOCAL_SETTINGS};
use crate::harness::GitError;
use crate::i18n::{self, Lang};
use crate::sdd::formatting;
use crate::sdd::reconciler::is_task_commit;
use crate::sdd::{
    read_settings, read_spec, read_task, sdd_dir, Confidence, Environment, Finding, Formatter, SddError, Settings, SpecStatus, Task,
    TaskStatus, TestRun, Verdict,
};

/// Future boxed usado pelos ports (sem `async-trait`).
pub type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Tempo máximo do `test_command` configurado pelo humano.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(600);
/// Limite de bytes de saída de teste guardados (o excedente é descartado).
pub const MAX_TEST_OUTPUT_BYTES: usize = 64 * 1024;
/// Falhas de `test_command` toleradas dentro de uma mesma tentativa antes de `BLOCKED`.
pub const MAX_TEST_CYCLES: u32 = crate::sdd::DEFAULT_LIMIT;

/// Diretório, dentro de `.sdd`, onde ficam as worktrees das tarefas.
const WORKTREES_DIR: &str = "worktrees";
/// Worktrees das tarefas ficam sob `.sdd`; são excluídas da checagem de limpeza da base.
const WORKTREES_EXCLUDE: &str = ".sdd/worktrees";
/// Título das seções do corpo da tarefa lidas/escritas pelo runner.
/// Início da mensagem `tests` (sem tarefa) com o resultado da verificação final na base (spec 077).
pub const FINAL_BASE_PREFIX: &str = "verificação final na base";
pub const OBJECTIVE: &str = "Objetivo";
pub const ACCEPTANCE: &str = "Critérios de Aceitação";
pub const FINDINGS: &str = "Findings Abertos";
/// Registro compacto dos findings de cada reprovação (spec 063): alimenta as lições do projeto
/// depois que os "Findings Abertos" são limpos na aprovação.
pub const FINDINGS_LOG: &str = "Findings Revisados";
const FINDINGS_LOG_MAX_BYTES: usize = 3 * 1024;
const FINDINGS_LOG_ITEM_BYTES: usize = 240;
/// Prefixo do título das seções de análise de recuperação do Arquiteto (`## ... #n`).
pub const RECOVERY_PREFIX: &str = "Análise de recuperação #";
/// Linha da análise de recuperação com os caminhos acrescentados ao `allowed_scope` (spec 079):
/// gravada pelo servidor e aplicada pelo runner ao registrar a decisão.
pub const SCOPE_ADDED_LINE: &str = "- Escopo ampliado: ";
/// Prefixo do título das seções de diagnóstico de ambiente do Arquiteto (`## ... #n`). Não contam
/// como ciclo de recuperação de qualidade.
pub const DIAGNOSIS_PREFIX: &str = "Diagnóstico de ambiente #";
/// Título da seção-marcador gravada quando a definição da tarefa é revisada e o humano retoma:
/// os ciclos de recuperação anteriores a ela deixam de contar (o histórico permanece no corpo).
pub const REVISED_MARK: &str = "Definição revisada";
/// Título da seção-marcador gravada quando o humano retoma manualmente uma tarefa BLOQUEADA
/// (spec 073): zera o orçamento de ciclos de recuperação como a revisão; o histórico permanece.
pub const HUMAN_RESUME_MARK: &str = "Retomada pelo humano";
/// Seção do corpo da tarefa com as orientações do humano (escolha de uma opção de recuperação ou
/// texto livre, spec 073); entra no briefing do Desenvolvedor e persiste entre execuções.
pub const GUIDANCE_SECTION: &str = "Orientação do humano";
/// Limite do texto acumulado da seção [`GUIDANCE_SECTION`] (as entradas mais antigas caem primeiro).
pub const GUIDANCE_MAX_BYTES: usize = 4 * 1024;
/// Delimitadores do bloco JSON com as opções de recuperação dentro da seção de análise (spec 073).
pub const OPTIONS_OPEN: &str = "<!-- nants:options -->";
pub const OPTIONS_CLOSE: &str = "<!-- /nants:options -->";
/// Linha gravada no fim de uma seção de análise produzida SEM o modelo (escalada determinística,
/// como o limite de ciclos): a seção continua no histórico, mas não conta como ciclo (spec 073).
pub const NOT_COUNTED_LINE: &str = "- Contagem: não conta como ciclo (escalada determinística)";
/// Causa raiz do limite de ciclos; seções antigas (sem [`NOT_COUNTED_LINE`]) com esta causa também
/// não contam, desde que a seção seja uma escalada (`- Decisão: escalar ao humano`).
const LEGACY_LIMIT_CAUSE: &str = "- Causa raiz: limite de ciclos de recuperação atingido";
/// `next_action` (pt-BR) gravado antes da spec 114 numa tarefa BLOQUEADA cuja definição foi
/// editada pelo Arquiteto no chat. Só serve para ler o que já existe em disco: hoje o motivo leva o
/// código [`BLOCK_REVISED`] e o texto no idioma do projeto.
pub const REVISED_NEXT_ACTION: &str = "definição revisada; retomar";
/// Código do motivo de uma tarefa BLOQUEADA com a definição revisada pelo Arquiteto no chat.
pub const BLOCK_REVISED: &str = "revised";

/// Códigos estáveis dos motivos de bloqueio gravados em `next_action_code` (spec 114 T02). Cada um
/// tem o texto `runner.block.<código>` nos dois dicionários, menos o orçamento (`runner.budget.*`,
/// montado pelo [`BudgetGuard`]).
pub const BLOCK_CODES: [&str; 22] = [
    "budget",
    "docker",
    "external_edit",
    "tests_failed",
    "stalled",
    "rejections",
    "not_on_base",
    "staged_index",
    "sdd_dirty",
    "base_dirty",
    "worktree_dirty",
    "local_settings",
    "squash_failed",
    "base_tests_failed",
    "base_tests_not_run",
    "base_check_failed",
    "base_check_not_run",
    "commit_failed",
    "rollback_failed",
    "finalize_dirty",
    BLOCK_REVISED,
    "blocked",
];

/// Motivo de bloqueio (spec 114 T02): o código estável, lido pelas máquinas, e o texto para a
/// pessoa no idioma do projeto. Vai para `next_action_code` e `next_action`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockReason {
    pub code: &'static str,
    pub text: String,
}

impl BlockReason {
    /// Motivo com o texto `runner.block.<code>` no idioma, com os parâmetros preenchidos.
    pub fn new(lang: Lang, code: &'static str, params: &[(&str, &str)]) -> Self {
        Self { code, text: i18n::tr(lang, &format!("runner.block.{code}"), params) }
    }

    /// Motivo com contagem (spec 115 T01): `runner.block.<code>.one`/`.other`, com `{count}`.
    pub fn counted(lang: Lang, code: &'static str, count: u64) -> Self {
        Self { code, text: i18n::tr_plural(lang, &format!("runner.block.{code}"), count, &[]) }
    }
}

/// Tarefa BLOQUEADA com a definição revisada: pelo código ou, nas tarefas gravadas antes da spec
/// 114 (sem código), pelo texto pt-BR fixo.
pub fn is_revised_block(meta: &crate::sdd::TaskMeta) -> bool {
    match meta.next_action_code.as_deref() {
        Some(code) => code == BLOCK_REVISED,
        None => meta.next_action.as_deref() == Some(REVISED_NEXT_ACTION),
    }
}
/// Prefixo da evidência de testes quando o `test_command` falhou (a evidência normal é de sucesso).
pub const TEST_FAILURE_MARK: &str = "[falha dos testes";

/// Cabeçalho estruturado (primeira linha) da evidência de uma falha de testes.
pub fn failure_header(exit_code: Option<i32>, timed_out: bool) -> String {
    if timed_out {
        format!("{TEST_FAILURE_MARK}: tempo limite]\n")
    } else if let Some(code) = exit_code {
        format!("{TEST_FAILURE_MARK}: código {code}]\n")
    } else {
        format!("{TEST_FAILURE_MARK}]\n")
    }
}
/// Marcador, dentro da evidência, que separa a saída dos testes da seção de checagens.
pub const CHECKS_MARK: &str = "\n\n## Checagens\n";
/// Separa, em `test_evidence`, a seção "Critérios" do Desenvolvedor (matriz critério → evidência).
pub const DEV_CRITERIA_MARK: &str = "\n\n## Critérios do Desenvolvedor\n";
/// Limite da seção "Critérios" repassada ao Revisor.
const DEV_CRITERIA_MAX_BYTES: usize = 4096;
/// Cauda máxima (bytes) guardada por checagem.
const CHECK_TAIL_BYTES: usize = 4 * 1024;
/// Cauda da saída de um formatador que falhou, na mensagem da conversa.
const FORMATTER_TAIL_BYTES: usize = 300;

/// Resultado de uma checagem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    pub name: String,
    /// Comando executado, já redigido (spec 082 T01): vai ao Revisor como `$ <comando>`.
    pub command: String,
    pub blocking: bool,
    pub passed: bool,
    pub output: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

const TEST_SUMMARY_MAX_BYTES: usize = 500;

/// Cauda da saída (até `max` bytes, em limite de caractere).
fn tail_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Resumo da saída dos testes: primeira e últimas linhas não vazias, até `TEST_SUMMARY_MAX_BYTES`.
/// Assinatura (FNV-1a 64) do que a evidência cobre: `test_command`, checagens bloqueantes e ambiente.
fn test_signature(settings: &Settings) -> String {
    let mut canon = format!(
        "{}\n{:?}\n{}",
        settings.test_command,
        settings.environment,
        settings.environment_hash.as_deref().unwrap_or("")
    );
    for c in settings.checks.iter().filter(|c| c.blocking) {
        canon.push_str(&format!("\n{}\t{}", c.name, c.command));
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canon.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn test_summary(output: &str) -> String {
    let lines: Vec<&str> = output.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let Some(first) = lines.first() else {
        return String::new();
    };
    let mut tail: Vec<&str> = Vec::new();
    let mut used = first.len();
    for l in lines.iter().skip(1).rev() {
        if tail.len() >= 3 || used + l.len() + 1 > TEST_SUMMARY_MAX_BYTES {
            break;
        }
        used += l.len() + 1;
        tail.push(l);
    }
    tail.reverse();
    let mut out = (*first).to_string();
    for l in tail {
        out.push('\n');
        out.push_str(l);
    }
    truncate_bytes(&out, TEST_SUMMARY_MAX_BYTES).to_string()
}

/// Resumo de uma linha: `checagens: lint ✗ (não bloqueante), build ✓`.
pub fn checks_summary(outcomes: &[CheckOutcome]) -> String {
    checks_summary_marked(outcomes, &[])
}

/// Como [`checks_summary`], marcando as checagens não bloqueantes em `regressions` (passam na base,
/// falham na branch): `lint ✗ (não bloqueante, regressão desta tarefa)` (spec 080 T01).
pub fn checks_summary_marked(outcomes: &[CheckOutcome], regressions: &[String]) -> String {
    checks_summary_in(Lang::PtBr, outcomes, regressions)
}

/// [`checks_summary_marked`] no idioma do projeto (spec 113 T04): é texto da conversa.
pub fn checks_summary_in(lang: Lang, outcomes: &[CheckOutcome], regressions: &[String]) -> String {
    i18n::tr(lang, "runner.checks.list", &[("list", &checks_list_in(lang, outcomes, regressions))])
}

/// Só a lista de [`checks_summary_in`], sem o rótulo `checagens:`.
fn checks_list_in(lang: Lang, outcomes: &[CheckOutcome], regressions: &[String]) -> String {
    let parts: Vec<String> = outcomes
        .iter()
        .map(|o| {
            let mark = if o.passed { "✓" } else { "✗" };
            let args: [(&str, &str); 2] = [("name", &o.name), ("mark", mark)];
            if o.passed || o.blocking {
                format!("{} {mark}", o.name)
            } else if regressions.contains(&o.name) {
                i18n::tr(lang, "runner.checks.non_blocking_regression", &args)
            } else {
                i18n::tr(lang, "runner.checks.non_blocking", &args)
            }
        })
        .collect();
    parts.join(", ")
}

/// Rótulo da checagem não bloqueante que passa na base e falha na branch da tarefa.
pub const REGRESSION_LABEL: &str = "regressão desta tarefa";
/// Separa, dentro da seção de checagens da evidência, a lista de regressões desta tarefa.
pub const REGRESSIONS_MARK: &str = "\n\n## Regressões desta tarefa\n";

/// Resultado de sucesso dos testes (e das checagens, quando houve) numa só linha persistida.
pub fn pass_summary(place: &str, command: &str, checks: Option<&str>) -> String {
    match checks {
        Some(c) => format!("testes ✓{place} · {c}"),
        None => format!("testes ✓{place} (`{command}`)"),
    }
}

/// [`pass_summary`] no idioma do projeto (spec 113 T04); `on_base` acrescenta "na base".
pub fn pass_summary_in(lang: Lang, on_base: bool, command: &str, checks: Option<&str>) -> String {
    let place = if on_base { i18n::text(lang, "runner.tests.on_base") } else { String::new() };
    match checks {
        Some(c) => i18n::tr(lang, "runner.tests.passed_checks", &[("place", &place), ("checks", c)]),
        None => i18n::tr(lang, "runner.tests.passed_command", &[("place", &place), ("command", command)]),
    }
}

/// Início da linha da verificação final na base no idioma (ver [`FINAL_BASE_PREFIX`]).
pub fn final_base_prefix(lang: Lang) -> String {
    i18n::text(lang, "runner.final_base.prefix")
}

/// Alerta anexado à evidência quando um comando termina com 0 sem saída própria.
pub const SILENT_PASS_WARNING: &str = "ALERTA: o comando terminou sem saída própria; verifique se ele rodou de fato.";

/// `true` quando a saída só tem linhas do próprio Docker Compose (Network/Container/Volume
/// Creating/Created/Started/Removing…) ou nada.
pub fn is_silent_output(output: &str) -> bool {
    const ACTIONS: [&str; 14] = [
        "Creating", "Created", "Starting", "Started", "Running", "Waiting", "Healthy", "Stopping", "Stopped",
        "Removing", "Removed", "Recreate", "Building", "Built",
    ];
    output.lines().all(|l| {
        let t = l.trim_start_matches(|c: char| !c.is_alphanumeric()).trim_end();
        t.is_empty()
            || (["Network ", "Container ", "Volume "].iter().any(|p| t.starts_with(p))
                && ACTIONS.iter().any(|a| t.ends_with(a)))
    })
}

/// Seção "Checagens" (cauda limitada por checagem) para o prompt do Revisor; cada uma traz
/// `$ <comando redigido>` antes da saída (spec 082 T01).
pub fn checks_section(outcomes: &[CheckOutcome]) -> String {
    outcomes
        .iter()
        .map(|o| {
            let status = if o.passed {
                "passou"
            } else if o.blocking {
                "FALHOU (bloqueante)"
            } else {
                "FALHOU (não bloqueante)"
            };
            let warn = if o.passed && o.exit_code.unwrap_or(0) == 0 && is_silent_output(&o.output) {
                format!("\n{SILENT_PASS_WARNING}")
            } else {
                String::new()
            };
            format!(
                "### {} - {status}\n$ {}\n{}{warn}",
                o.name,
                o.command,
                tail_bytes(o.output.trim_end(), CHECK_TAIL_BYTES)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Intervalo de consulta do processo de teste.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Tempo extra para drenar a saída depois que o processo termina.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

/// Erros do runner.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("identificador inválido para {name}: {value:?}")]
    InvalidId { name: &'static str, value: String },
    #[error("configuração inválida: {0}")]
    Config(String),
    #[error("estado inválido: {0}")]
    Invalid(String),
    #[error("tarefa {task} com dependências não DONE: {}", pending.join(", "))]
    DependenciesPending { task: String, pending: Vec<String> },
    #[error(transparent)]
    Sdd(#[from] SddError),
    #[error(transparent)]
    Git(#[from] GitError),
    #[error("erro de I/O em {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("falha ao executar teste: {0}")]
    Tests(String),
    /// Falha do ambiente Docker (daemon fora, build falhou, ambiente divergente): Environmental.
    #[error("ambiente docker: {0}")]
    Docker(String),
    #[error("falha do papel {role}: {message}")]
    Port { role: &'static str, message: String },
    /// Falha do provedor LLM de um papel. Carrega só a causa fixa (sem corpo, URL ou chave).
    #[error("falha do papel {role}: {failure}")]
    Provider {
        role: &'static str,
        failure: ProviderFailure,
    },
    /// Arquivo de estado alterado por outro processo desde a última leitura/gravação do runner.
    #[error("{path} modificado externamente")]
    ExternalEdit { path: String },
    /// Cancelamento cooperativo solicitado; o estado persistido não foi alterado por esta etapa.
    #[error("execução cancelada")]
    Cancelled,
}

/// Classe do bloqueio: define se a recuperação agêntica se aplica (só `Quality`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// O código/entrega é a causa: reprovações esgotadas, testes falhando, testes da base após squash.
    Quality,
    /// O ambiente é a causa: base suja, edição externa, falha de Git/I/O, config ausente.
    Environmental,
    /// Orçamento da spec atingido: não é qualidade nem ambiente; sem recuperação nem diagnóstico.
    Budget,
}

impl BlockKind {
    /// Valor estável usado em eventos da API.
    pub fn as_str(self) -> &'static str {
        match self {
            BlockKind::Quality => "quality",
            BlockKind::Environmental => "environmental",
            BlockKind::Budget => "budget",
        }
    }

    /// Tarefa já `BLOCKED` em disco (causa desconhecida): findings abertos indicam qualidade;
    /// sem findings, ambiental.
    fn infer(open_findings: &[Finding]) -> Self {
        if open_findings.is_empty() {
            BlockKind::Environmental
        } else {
            BlockKind::Quality
        }
    }
}

/// Resultado de `run_task`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Blocked {
        reason: String,
        kind: BlockKind,
        open_findings: Vec<Finding>,
    },
}

/// Resultado de `run_spec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecOutcome {
    /// Todas as tarefas DONE; a spec foi marcada `COMPLETED`.
    Completed,
    /// Parou na primeira tarefa BLOCKED.
    Blocked {
        task_id: String,
        reason: String,
        kind: BlockKind,
        open_findings: Vec<Finding>,
    },
    /// Nenhuma tarefa elegível: dependências não atendidas (ex.: ausente ou BLOCKED).
    Stalled { pending: Vec<String> },
}

/// Pedido de implementação enviado ao Desenvolvedor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevBrief {
    pub spec_id: String,
    pub task_id: String,
    pub title: String,
    pub objective: String,
    pub acceptance: String,
    pub allowed_scope: Vec<String>,
    pub worktree: PathBuf,
    pub branch: String,
    pub findings: Vec<Finding>,
    /// Saída do `test_command` que falhou na tentativa atual, quando houver.
    pub test_output: Option<String>,
    /// Seções "Análise de recuperação #n" do corpo da tarefa (estratégia nova do Arquiteto); vazio sem elas.
    pub recovery: String,
    /// Seção "Tentativas anteriores" do corpo da tarefa (resumo do Dev + diff --stat por tentativa); vazio sem ela.
    pub history: String,
    /// Seção "Orientação do humano" do corpo da tarefa (spec 073); vazia sem ela.
    pub human_guidance: String,
    /// Seções "Premissas" e "Abordagem Técnica" do spec.md (≤ 4 KiB, já redigido); vazio sem elas.
    pub spec_context: String,
    /// Subseção `### Desenvolvedor` de "Orientações por papel" do spec.md (≤ 1,5 KiB, redigida); vazia sem ela.
    pub role_guidance: String,
    /// Bloco do perfil de especialista do Desenvolvedor (spec 063; raiz do projeto, redigido e
    /// limitado); vazio sem perfil ou com `skills: false`.
    pub skill_profile: String,
    /// Lista de arquivos rastreados da worktree (≤ 200 linhas / 4 KiB), sem conteúdo; vazio sem arquivos.
    pub project_files: String,
    /// Época da tentativa (ver [`ask_epoch`]): chave do cache de perguntas repetidas ao Arquiteto.
    pub ask_epoch: u64,
    /// Progresso da sessão anterior da mesma tentativa, quando ela esgotou o limite de passos
    /// (spec 081 T01): resumo redigido + `diff --stat`, ≤ ~3 KiB; vazio nos demais casos.
    pub previous_progress: String,
    /// Formatadores que o Nants roda depois do Developer (spec 081 T02; o mesmo
    /// `formatting::briefing_list` do Revisor, redigido); vazio sem formatadores.
    pub formatters: String,
    /// Globs de `screenshots` do projeto (spec 082 T03): com eles, o Desenvolvedor vê com
    /// `view_screenshot` as capturas gravadas desde o início da própria sessão; vazio = sem a ferramenta.
    pub screenshot_globs: Vec<String>,
    /// Imagens de referência do usuário (spec 084 T02), lidas de `.sdd/specs/<spec>/references/` no
    /// repositório principal; vazio = sem a seção nem as referências em `view_screenshot`.
    pub references: crate::sdd::references::References,
    /// Branch base do projeto (`settings.base_branch`): a conferência antes do fim do Desenvolvedor
    /// (spec 087 T04) compara a worktree com ela; vazia = não confere o diff.
    pub base_branch: String,
    /// Pasta da spec no repositório principal (`.sdd/specs/<spec>`): cache e manifesto das imagens
    /// geradas (spec 090 T02).
    pub spec_dir: PathBuf,
    /// Seção "## Guia de arte" do spec.md (redigida, ≤ 1500 caracteres), anteposta ao prompt de
    /// `generate_image`; vazia sem ela.
    pub art_guide: String,
}

/// Relato do Desenvolvedor após uma rodada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevReport {
    pub summary: String,
    /// A sessão esgotou o limite de passos (ou parou sem progresso) antes da resposta final.
    pub exhausted: bool,
}

/// Entrada enviada ao Revisor (somente leitura).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInput {
    pub spec_id: String,
    pub task_id: String,
    pub acceptance: String,
    /// Diff do briefing: `--stat` completo + diffs de texto até o orçamento + lista de omitidos
    /// (ver [`review_diff`]).
    pub diff: String,
    /// Intervalo da tarefa (`base...branch`), para a ferramenta `diff_file`.
    pub base: String,
    pub branch: String,
    pub test_output: String,
    /// Saídas das checagens (lint/typecheck/build) rodadas após os testes; vazio sem checks.
    pub checks: String,
    /// Seção "Critérios" da resposta final do Desenvolvedor (critério → evidência); vazio sem ela.
    pub dev_criteria: String,
    /// Outras tarefas da spec ("Txx: título", uma por linha); fora do escopo desta revisão.
    pub other_tasks: String,
    /// Findings da revisão anterior (texto), quando houver; vazio na primeira revisão.
    pub prior_findings: String,
    /// Diff desde a última reprovação (`rejected_sha..branch`, mesmo orçamento do diff); vazio sem reprovação.
    pub incremental_diff: String,
    /// Arquivos alterados desde o commit revisado na rodada anterior (`rejected_sha...branch`, spec
    /// 092 T04); `None` na 1ª revisão ou sem base do diff. Os FATOS mostram no máximo
    /// [`CHANGED_FILES_MAX`] caminhos.
    pub changed_files: Option<Vec<String>>,
    /// Seções "Premissas" e "Abordagem Técnica" do spec.md (≤ 4 KiB, já redigido); vazio sem elas.
    pub spec_context: String,
    /// Subseção `### Revisor` de "Orientações por papel" do spec.md (≤ 1,5 KiB, redigida); vazia sem ela.
    pub role_guidance: String,
    /// Bloco do perfil de especialista do Revisor (spec 063); vazio sem perfil ou com `skills: false`.
    pub skill_profile: String,
    /// Formatadores automáticos do projeto (spec 078), um por linha, já redigidos; vazio sem formatadores.
    pub formatters: String,
    pub worktree: PathBuf,
    /// Época da tentativa (ver [`ask_epoch`]): chave do cache de perguntas repetidas ao Arquiteto.
    pub ask_epoch: u64,
    /// Checagens não bloqueantes que passam na base e falham na branch (spec 080 T01), separadas por
    /// vírgula; vazio sem regressão ou sem resultado da base.
    pub regressions: String,
    /// Capturas de tela da execução de testes que gerou a evidência (spec 082 T03), das mais
    /// recentes às mais antigas; sem nenhuma utilizável, `view_screenshot` não é oferecida.
    pub screenshots: Vec<crate::sdd::screenshots::Capture>,
    /// Linha das capturas de tarefas anteriores que esta execução não gera mais (spec 086 T01, ver
    /// [`crate::sdd::screenshots::render_vanished`]); vazia sem faltas e na verificação final.
    pub vanished_captures: String,
    /// Imagens de referência do usuário (spec 084 T02); vazio = sem a seção nem as referências.
    pub references: crate::sdd::references::References,
    /// Imagens geradas da spec (spec 090 T02): os FATOS listam as da tarefa (ou todas, na
    /// verificação final) e `view_screenshot` as abre pelo caminho; vazio = nada muda.
    pub images: crate::sdd::images::SpecImages,
    /// Imagens planejadas da tarefa (spec 092 T03, do objetivo e dos critérios) e a disponibilidade
    /// da geração nesta execução; sem caminhos planejados, nada muda nos FATOS.
    pub planned_images: crate::sdd::images::PlannedImages,
}

/// Época da tentativa corrente da tarefa: muda a cada tentativa nova e a cada análise de recuperação
/// (que zera `attempts`). Perguntas repetidas ao Arquiteto só reaproveitam respostas da mesma época.
pub fn ask_epoch(task: &Task) -> u64 {
    ((recovery_total(&task.body) as u64) << 32) | u64::from(task.frontmatter.attempts + 1)
}

/// Teto conjunto das seções "Premissas" e "Abordagem Técnica" nos briefings.
pub const SPEC_CONTEXT_MAX_BYTES: usize = 4 * 1024;
/// Tetos da lista de arquivos do projeto no briefing do Desenvolvedor.
pub const PROJECT_FILES_MAX_LINES: usize = 200;
pub const PROJECT_FILES_MAX_BYTES: usize = 4 * 1024;

/// Corta `text` em `max` bytes numa fronteira de caractere.
fn truncate_bytes(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Extrai "Premissas" e "Abordagem Técnica" do spec.md, juntas em até `max` bytes.
pub fn spec_context_of(spec_md: &str, max: usize) -> String {
    let mut out = String::new();
    for title in ["Premissas", "Abordagem Técnica"] {
        if let Some(text) = section_text(spec_md, title) {
            let text = text.trim();
            if !text.is_empty() {
                out.push_str(&format!("### {title}\n{text}\n\n"));
            }
        }
    }
    let out = out.trim_end();
    if out.len() > max {
        format!("{}\n[...truncado]", truncate_bytes(out, max).trim_end())
    } else {
        out.to_string()
    }
}

/// Prefixo literal de diretório de um glob de escopo (`src/agents/**` -> `src/agents/`).
fn scope_dir_prefix(glob: &str) -> String {
    let g = glob.trim().trim_start_matches("./");
    let literal = match g.find(['*', '?', '[', '{']) {
        Some(i) => &g[..i],
        None => g,
    };
    match literal.rfind('/') {
        Some(i) => literal[..=i].to_string(),
        None => String::new(),
    }
}

/// Lista de arquivos para o briefing: primeiro os que casam com o escopo (ou estão em seus
/// diretórios), depois os demais; respeita `max_lines`/`max_bytes` e informa quantos foram omitidos.
pub fn project_files_listing(files: &[String], scope: &[String], max_lines: usize, max_bytes: usize) -> String {
    let prefixes: Vec<String> = scope.iter().map(|g| scope_dir_prefix(g)).collect();
    let files: Vec<&String> = files.iter().filter(|f| !f.starts_with(".sdd/")).collect();
    if files.is_empty() {
        return String::new();
    }
    let in_scope = |f: &str| prefixes.iter().any(|p| !p.is_empty() && f.starts_with(p.as_str()));
    let mut ordered: Vec<&String> = files.iter().copied().filter(|f| in_scope(f)).collect();
    ordered.extend(files.iter().copied().filter(|f| !in_scope(f)));
    let mut out = String::new();
    let mut shown = 0usize;
    for f in &ordered {
        // Reserva espaço para a linha de omitidos.
        if shown >= max_lines || out.len() + f.len() + 1 > max_bytes.saturating_sub(40) {
            break;
        }
        out.push_str(f);
        out.push('\n');
        shown += 1;
    }
    let omitted = ordered.len() - shown;
    if omitted > 0 {
        out.push_str(&format!("... e mais {omitted} arquivos omitidos\n"));
    }
    out.trim_end().to_string()
}

/// Máximo de entradas em `diff_lines`; acima disso sai a 2ª mais antiga (a 1ª tentativa fica).
const MAX_DIFF_LINES_ENTRIES: usize = 20;

/// Spec 087 T05: caminho de teste ou documentação, fora da contagem do diff de código. Regra simples
/// (sem diferenciar maiúsculas): `*.md`; algum diretório `test`, `tests`, `spec`, `specs` ou `__tests__`;
/// nome com `.test.` ou `.spec.`; nome cujo radical começa com `test_` ou termina em `_test`/`_tests`.
pub fn is_test_or_doc(path: &str) -> bool {
    let lower = path.trim().replace('\\', "/").to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('/').filter(|p| !p.is_empty()).collect();
    let Some(name) = parts.pop() else {
        return false;
    };
    if name.ends_with(".md") || name.contains(".test.") || name.contains(".spec.") {
        return true;
    }
    if parts.iter().any(|d| matches!(*d, "test" | "tests" | "spec" | "specs" | "__tests__")) {
        return true;
    }
    let stem = name.split('.').next().unwrap_or_default();
    stem.starts_with("test_") || stem.ends_with("_test") || stem.ends_with("_tests")
}

/// Linhas alteradas (+ e -) de código em `base...branch`, sem testes, docs nem binários.
pub fn code_diff_lines(repo: &Git, base: &str, branch: &str) -> Result<u64, GitError> {
    Ok(repo
        .numstat(base, branch)?
        .iter()
        .filter(|n| !is_test_or_doc(&n.path))
        .map(|n| n.added.unwrap_or(0) + n.deleted.unwrap_or(0))
        .sum())
}

/// Acrescenta a contagem da tentativa a `diff_lines`, mantendo a 1ª e as mais recentes no teto.
fn push_diff_lines(list: &mut Vec<u64>, lines: u64) {
    list.push(lines);
    while list.len() > MAX_DIFF_LINES_ENTRIES {
        list.remove(1);
    }
}

/// Monta o diff do briefing do Revisor: `--stat` completo; diffs de texto por arquivo, dos
/// menores para os maiores, enquanto couberem em `budget` bytes; binários (detecção do git) e
/// arquivos que não cabem viram "Arquivos omitidos" (pedidos com `diff_file`).
pub fn review_diff(repo: &Git, base: &str, branch: &str, budget: usize) -> Result<String, GitError> {
    let stat = repo.diff_stat(base, branch)?;
    let mut text_files: Vec<(usize, String, String, String)> = Vec::new();
    let mut omitted: Vec<String> = Vec::new();
    for n in repo.numstat(base, branch)? {
        match (n.added, n.deleted) {
            (Some(a), Some(d)) => {
                let d_text = repo.diff_file(base, branch, &n.path)?;
                text_files.push((d_text.len(), n.path, format!("+{a}/-{d}"), d_text));
            }
            _ => omitted.push(format!("- {} (binário)", n.path)),
        }
    }
    text_files.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut used = 0usize;
    let mut fits = true;
    let mut included = String::new();
    for (size, path, lines, body) in text_files {
        if fits && used + size <= budget {
            used += size;
            included.push_str(&body);
            if !body.ends_with('\n') {
                included.push('\n');
            }
        } else {
            fits = false;
            omitted.push(format!("- {path} ({lines} linhas, {size} bytes de diff)"));
        }
    }
    let mut out = format!("### git diff --stat\n{stat}\n### Diffs incluídos\n{included}");
    if !omitted.is_empty() {
        out.push_str(&format!(
            "\n### Arquivos omitidos (peça com diff_file)\n{}\n",
            omitted.join("\n")
        ));
    }
    Ok(out)
}

/// Uma linha da matriz critério → evidência do Revisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionCheck {
    pub criterion: String,
    pub met: bool,
    pub evidence: String,
}

/// Extrai (limitada) a seção "Critérios" da resposta final do Desenvolvedor: do título até o
/// próximo título markdown. Sem a seção, devolve vazio.
pub fn extract_dev_criteria(summary: &str) -> String {
    let is_title = |l: &str| {
        let t = l.trim().trim_start_matches('#').trim().trim_matches('*').trim().trim_end_matches(':').trim();
        t.eq_ignore_ascii_case("critérios") || t.eq_ignore_ascii_case("criterios")
    };
    let mut lines = summary.lines();
    if !lines.by_ref().any(is_title) {
        return String::new();
    }
    let mut out = String::new();
    for line in lines {
        if line.trim_start().starts_with('#') {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    let out = out.trim();
    tail_bytes_head(out, DEV_CRITERIA_MAX_BYTES)
}

fn tail_bytes_head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncado]", &text[..end])
}

/// Texto da matriz para o diálogo: uma linha `✓|✗ critério: evidência` por critério (sem travessão, 095/T08).
pub fn render_criteria(criteria: &[CriterionCheck]) -> String {
    criteria
        .iter()
        .map(|c| {
            let mark = if c.met { "✓" } else { "✗" };
            if c.evidence.trim().is_empty() {
                format!("{mark} {}", c.criterion)
            } else {
                format!("{mark} {}: {}", c.criterion, c.evidence)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Marca que separa o resumo da matriz na mensagem do Revisor (a UI renderiza o checklist).
pub const CRITERIA_HEADER: &str = "Critérios:";

/// Marca que o runner acrescenta aos critérios `(manual)` enviados ao Revisor.
pub const HUMAN_CHECK_MARK: &str = "verificação humana, não bloqueia";
/// Marca antiga (antes da 095/T08), ainda reconhecida nos critérios já gravados.
const LEGACY_HUMAN_CHECK_MARK: &str = "verificação humana — não bloqueia";

/// `true` se o critério (linha de checklist) começa com `(manual)`, sem diferenciar maiúsculas,
/// depois de marcadores como `-`, `*`, `1.` e `[ ]`/`[x]`; ou se já carrega a marca de verificação humana.
pub fn is_manual_criterion(line: &str) -> bool {
    let mut t = line.trim();
    loop {
        let before = t;
        t = t.trim_start_matches(['-', '*', '+']).trim_start();
        if let Some(rest) = t.strip_prefix('[') {
            if let Some((mark, after)) = rest.split_once(']') {
                if mark.len() <= 1 {
                    t = after.trim_start();
                }
            }
        }
        let digits = t.trim_start_matches(|c: char| c.is_ascii_digit());
        if digits.len() < t.len() {
            if let Some(after) = digits.strip_prefix('.').or_else(|| digits.strip_prefix(')')) {
                t = after.trim_start();
            }
        }
        if t == before {
            break;
        }
    }
    let end = line.trim_end();
    t.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("(manual)"))
        || end.ends_with(&format!("[{HUMAN_CHECK_MARK}]"))
        || end.ends_with(&format!("[{LEGACY_HUMAN_CHECK_MARK}]"))
}

/// Itens `(manual)` dos critérios de aceitação (texto sem marcador de lista).
pub fn manual_criteria(acceptance: &str) -> Vec<String> {
    acceptance
        .lines()
        .filter(|l| is_manual_criterion(l))
        .map(|l| l.trim().trim_start_matches(['-', '*', '+']).trim().to_string())
        .collect()
}

/// Critérios de aceitação para o Revisor: itens `(manual)` ganham a marca de verificação humana.
pub fn mark_manual_criteria(acceptance: &str) -> String {
    acceptance
        .lines()
        .map(|l| {
            if is_manual_criterion(l) && !l.contains(HUMAN_CHECK_MARK) && !l.contains(LEGACY_HUMAN_CHECK_MARK) {
                format!("{l} [{HUMAN_CHECK_MARK}]")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// APPROVED com algum critério `not_met` (exceto `(manual)`, que não bloqueia) vira CHANGES_REQUESTED, com um finding gerado por critério.
/// Os findings gerados saem no idioma do projeto (`lang`): o Desenvolvedor os lê no corpo da tarefa.
pub fn enforce_criteria(lang: Lang, mut result: ReviewResult) -> ReviewResult {
    if result.verdict != Verdict::Approved {
        return result;
    }
    let unmet: Vec<&CriterionCheck> = result.criteria.iter().filter(|c| !c.met && !is_manual_criterion(&c.criterion)).collect();
    if unmet.is_empty() {
        return result;
    }
    let mut n = result.findings.len();
    let mut generated = Vec::new();
    for c in unmet {
        n += 1;
        let evidence = if c.evidence.trim().is_empty() { i18n::text(lang, "runner.review.no_evidence") } else { c.evidence.trim().to_string() };
        generated.push(Finding {
            id: format!("F-{n:02}"),
            severity: "HIGH".into(),
            location: i18n::text(lang, "runner.review.unmet_location"),
            description: i18n::tr(lang, "runner.review.unmet_description", &[("criterion", &c.criterion), ("evidence", &evidence)]),
            recommendation: i18n::text(lang, "runner.review.unmet_recommendation"),
            ..Finding::default()
        });
    }
    result.findings.extend(generated);
    result.verdict = Verdict::ChangesRequested;
    result
}

/// Rótulo do veredito na primeira linha da mensagem do Revisor (spec 078).
pub const VERDICT_APPROVED_LABEL: &str = "Aprovado";
pub const VERDICT_CHANGES_LABEL: &str = "Pediu ajustes";
/// Seção dos findings bloqueantes na mensagem do Revisor; vem antes de [`CRITERIA_HEADER`].
pub const FINDINGS_HEADER: &str = "Apontamentos:";
/// Linha das capturas de tela vistas pelo Revisor (spec 082 T03); vem antes de [`CRITERIA_HEADER`].
pub const CAPTURES_HEADER: &str = "Capturas avaliadas:";
/// Seção dos findings que não bloqueiam (MEDIUM sobre código inalterado, ou findings de uma aprovação).
pub const OBSERVATIONS_HEADER: &str = "Observações (não bloqueiam):";
/// Seção do corpo da tarefa com as observações rebaixadas (não contam como reprovação).
pub const OBSERVATIONS: &str = "Observações da revisão";
const OBSERVATIONS_MAX_BYTES: usize = 3 * 1024;
/// Prefixo da linha do cenário de falha (spec 087 T05), recuada logo abaixo do finding.
pub const SCENARIO_PREFIX: &str = "Cenário de falha:";
/// Marca, junto da severidade, do finding de baixa confiança (spec 087 T05).
pub const LOW_CONFIDENCE_LABEL: &str = "baixa confiança";
/// Limite de bytes de cada linha de finding na mensagem do Revisor.
const FINDING_LINE_BYTES: usize = 400;

/// Resultado da convergência: veredito ajustado e findings que não bloqueiam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converged {
    /// `findings` contém só os bloqueantes (vazio quando o veredito é APPROVED).
    pub result: ReviewResult,
    /// Findings que aparecem na conversa e no relatório mas não bloqueiam.
    pub observations: Vec<Finding>,
}

/// Trecho de um arquivo alterado desde a última reprovação (linhas do arquivo novo, inclusivas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedHunk {
    pub path: String,
    pub start: u32,
    pub end: u32,
}

fn is_medium(f: &Finding) -> bool {
    f.severity.trim().eq_ignore_ascii_case("MEDIUM")
}

fn norm_path(p: &str) -> &str {
    p.trim().trim_start_matches("./").trim_start_matches('/')
}

fn same_file_path(a: &str, b: &str) -> bool {
    let (a, b) = (norm_path(a), norm_path(b));
    !a.is_empty() && !b.is_empty() && (a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}")))
}

/// Caminhos (e faixas de linhas) citados em `location`: `caminho:linha`, `caminho:a-b` ou só `caminho`.
/// Vazio quando nada parece um caminho (ex.: `"critérios de aceitação"`).
fn parse_locations(location: &str) -> Vec<(String, Option<(u32, u32)>)> {
    let mut out = Vec::new();
    for raw in location.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let token = raw
            .trim_start_matches(['`', '"', '\'', '('])
            .trim_end_matches(['`', '"', '\'', ')', '.', ':']);
        let colon = token
            .char_indices()
            .find(|&(i, c)| c == ':' && token[i + 1..].starts_with(|d: char| d.is_ascii_digit()));
        let (path, lines) = match colon {
            Some((i, _)) => {
                let range = token[i + 1..].split(':').next().unwrap_or_default();
                let (a, b) = range.split_once('-').unwrap_or((range, ""));
                let a = a.parse::<u32>().ok();
                let b = b.parse::<u32>().ok();
                (&token[..i], a.map(|a| (a, b.unwrap_or(a).max(a))))
            }
            None => (token, None),
        };
        // Sem '/', só conta como caminho com sufixo de linha (`:N`, `:a-b`): "v1.2" ou "Foo.bar" não são caminhos.
        if path.is_empty() || !(path.contains('/') || (path.contains('.') && lines.is_some())) {
            continue;
        }
        out.push((path.to_string(), lines));
    }
    out
}

/// Hunks (`@@ -a,b +c,d @@`) de um diff de um arquivo, em linhas do arquivo novo.
pub fn parse_hunks(path: &str, diff: &str) -> Vec<ChangedHunk> {
    diff.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("@@ ")?;
            let new = rest.split_whitespace().find(|t| t.starts_with('+'))?.trim_start_matches('+');
            let (start, len) = match new.split_once(',') {
                Some((s, n)) => (s.parse::<u32>().ok()?, n.parse::<u32>().ok()?),
                None => (new.parse::<u32>().ok()?, 1),
            };
            Some(ChangedHunk { path: path.to_string(), start, end: start + len.saturating_sub(1) })
        })
        .collect()
}

/// Hunks alterados entre `since` (sha da última reprovação) e `branch`.
fn changed_hunks(repo: &Git, since: &str, branch: &str) -> Result<Vec<ChangedHunk>, GitError> {
    let mut out = Vec::new();
    for path in repo.changed_paths(since, branch)? {
        out.extend(parse_hunks(&path, &repo.diff_file(since, branch, &path)?));
    }
    Ok(out)
}

/// Convergência da re-revisão (spec 078). Com hunks alterados desde a última
/// reprovação, só bloqueiam: HIGH, MEDIUM dentro de um hunk alterado, MEDIUM no mesmo arquivo de um
/// finding anterior e MEDIUM sem caminho analisável; os demais MEDIUM viram observação. O rebaixamento
/// só vale quando leva à aprovação (spec 082): se, depois de particionar, ainda sobra finding bloqueante
/// ou critério não manual `not_met`, nada é rebaixado e todos os findings (na ordem original) bloqueiam.
/// Se nada bloqueante sobra, o veredito vira APPROVED. Em APPROVED, todos os findings devolvidos são
/// observações. Na 1ª revisão nada é rebaixado.
pub fn converge(lang: Lang, result: ReviewResult, previous: &[Finding], hunks: &[ChangedHunk]) -> Converged {
    let (result, low) = demote_low_confidence(lang, result);
    let Converged { result, mut observations } = converge_findings(lang, result, previous, hunks);
    observations.extend(low);
    Converged { result, observations }
}

/// Spec 087 T05: finding com `confidence: low` nunca reprova e vira observação (inclusive na 1ª
/// revisão). Em CHANGES_REQUESTED, sem outro finding: critério não manual `not_met` vira finding
/// bloqueante (como em [`enforce_criteria`]); sem nada mais que bloqueie, o veredito vira APPROVED.
fn demote_low_confidence(lang: Lang, mut result: ReviewResult) -> (ReviewResult, Vec<Finding>) {
    if !result.findings.iter().any(|f| f.confidence == Confidence::Low) {
        return (result, Vec::new());
    }
    let (low, kept): (Vec<Finding>, Vec<Finding>) =
        std::mem::take(&mut result.findings).into_iter().partition(|f| f.confidence == Confidence::Low);
    result.findings = kept;
    if result.verdict == Verdict::ChangesRequested && result.findings.is_empty() {
        result.verdict = Verdict::Approved;
        result.summary = i18n::tr(lang, "runner.review.low_confidence_demoted", &[("summary", result.summary.trim_end())]);
        // Critério não atendido continua bloqueando: volta a CHANGES_REQUESTED com findings gerados.
        result = enforce_criteria(lang, result);
    }
    (result, low)
}

/// Spec 092 T04: rótulo, junto da severidade, da observação rebaixada porque o arquivo não mudou
/// desde a revisão anterior (a UI o mostra como selo).
pub const UNCHANGED_FILE_LABEL: &str = "observação (arquivo não mudou nesta rodada)";
/// Teto da lista de arquivos alterados desde a revisão anterior nos FATOS da re-revisão.
pub const CHANGED_FILES_MAX: usize = 50;

/// Convergência com a lista de arquivos alterados desde a revisão anterior (spec 092 T04).
/// `changed_files` é `Some` só na re-revisão (há `rejected_sha` e o git respondeu): então um finding
/// NOVO (não reaberto, ver [`is_reopened`]), MEDIUM ou LOW, cuja `location` aponta só para arquivos
/// fora dessa lista vira observação com [`UNCHANGED_FILE_LABEL`] na severidade, mesmo que outro
/// finding continue bloqueando. Nunca são rebaixados: HIGH; location sem caminho ou de
/// captura/imagem; finding cujo texto cita um arquivo alterado ([`cites_changed_file`], regressão
/// induzida); location cujo arquivo não existe na branch (`exists` devolve `false`, inclusive quando
/// a checagem falha: ex. teste que deveria ter sido criado). Sem bloqueante restante, o veredito vira
/// APPROVED (critério não manual `not_met` continua bloqueando), como em [`demote_low_confidence`].
/// O que sobra segue em [`converge`].
pub fn converge_with_files(
    lang: Lang,
    result: ReviewResult,
    previous: &[Finding],
    hunks: &[ChangedHunk],
    changed_files: Option<&[String]>,
    exists: &dyn Fn(&str) -> bool,
) -> Converged {
    let (result, unchanged) = match changed_files {
        Some(changed) => demote_unchanged_files(lang, result, previous, changed, exists),
        None => (result, Vec::new()),
    };
    let Converged { result, mut observations } = converge(lang, result, previous, hunks);
    observations.extend(unchanged);
    Converged { result, observations }
}

fn demote_unchanged_files(
    lang: Lang,
    mut result: ReviewResult,
    previous: &[Finding],
    changed: &[String],
    exists: &dyn Fn(&str) -> bool,
) -> (ReviewResult, Vec<Finding>) {
    if result.verdict != Verdict::ChangesRequested {
        return (result, Vec::new());
    }
    let demotable = |f: &Finding| -> bool {
        let severity = f.severity.trim();
        if !(severity.eq_ignore_ascii_case("MEDIUM") || severity.eq_ignore_ascii_case("LOW")) || cites_image(&f.location) {
            return false;
        }
        let locs = parse_locations(&f.location);
        !locs.is_empty()
            && locs.iter().all(|(path, _)| !changed.iter().any(|c| same_file_path(c, path)) && exists(norm_path(path)))
            && !previous.iter().any(|p| is_reopened(f, p))
            && !cites_changed_file(f, changed)
    };
    if !result.findings.iter().any(|f| demotable(f)) {
        return (result, Vec::new());
    }
    let (demoted, kept): (Vec<Finding>, Vec<Finding>) =
        std::mem::take(&mut result.findings).into_iter().partition(|f| demotable(f));
    result.findings = kept;
    if result.findings.is_empty() {
        result.verdict = Verdict::Approved;
        result.summary = i18n::tr(lang, "runner.review.unchanged_files_demoted", &[("summary", result.summary.trim_end())]);
        // Critério não atendido continua bloqueando: volta a CHANGES_REQUESTED com findings gerados.
        result = enforce_criteria(lang, result);
    }
    let labeled = demoted
        .into_iter()
        .map(|f| Finding { severity: format!("{}, {UNCHANGED_FILE_LABEL}", f.severity.trim()), ..f })
        .collect();
    (result, labeled)
}

/// O arquivo `path` (relativo, sem `..`) existe na worktree da tarefa. Qualquer falha na checagem
/// (worktree ausente, caminho absoluto ou com `..`, erro de E/S) responde `false`: não rebaixa.
fn file_in_worktree(worktree: &Path, path: &str) -> bool {
    let rel = Path::new(path);
    if path.is_empty() || rel.is_absolute() || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return false;
    }
    worktree.is_dir() && fs::metadata(worktree.join(rel)).is_ok_and(|m| m.is_file())
}

/// `location` que cita captura de tela ou imagem (a palavra ou uma extensão de imagem).
fn cites_image(location: &str) -> bool {
    let lower = location.to_lowercase();
    ["captura", "screenshot", "imagem", "referência"].iter().any(|w| lower.contains(w))
        || lower
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '`' | '"' | '\'' | '(' | ')'))
            .any(|t| [".png", ".jpg", ".jpeg", ".webp", ".gif", ".svg", ".avif"].iter().any(|e| t.ends_with(e)))
}

/// Finding reaberto de um anterior: mesmo id, ou algum caminho da location igual ao de um finding
/// anterior. Só é chamado para findings cujos arquivos não mudaram: aí o apontamento anterior quase
/// certamente continua aberto, mesmo com outra redação (a similaridade de texto não importa).
fn is_reopened(f: &Finding, previous: &Finding) -> bool {
    let id = f.id.trim();
    if !id.is_empty() && id == previous.id.trim() {
        return true;
    }
    let prev_paths = parse_locations(&previous.location);
    parse_locations(&f.location).iter().any(|(a, _)| prev_paths.iter().any(|(b, _)| same_file_path(a, b)))
}

/// O texto do finding (descrição, recomendação ou cenário) cita um arquivo alterado nesta rodada:
/// o caminho completo, ou o nome do arquivo com extensão quando só um alterado tem esse nome.
/// Indica regressão induzida pela mudança (spec 092 T04, F-02).
fn cites_changed_file(f: &Finding, changed: &[String]) -> bool {
    let text = format!("{}\n{}\n{}", f.description, f.recommendation, f.failure_scenario);
    let tokens: Vec<&str> = text
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '/')))
        .map(|t| t.trim_end_matches('.'))
        .filter(|t| !t.is_empty())
        .collect();
    let file_name = |p: &str| norm_path(p).rsplit('/').next().unwrap_or_default().to_string();
    changed.iter().any(|c| {
        let path = norm_path(c);
        if path.is_empty() {
            return false;
        }
        if text.contains(path) {
            return true;
        }
        let name = file_name(c);
        let unique = changed.iter().filter(|o| file_name(o) == name).count() == 1;
        name.contains('.') && unique && tokens.iter().any(|t| *t == name || t.ends_with(&format!("/{name}")))
    })
}

/// FATOS da re-revisão (spec 092 T04): arquivos alterados desde a revisão anterior, só caminhos, no
/// máximo [`CHANGED_FILES_MAX`]; vazio = "(nenhum)".
pub fn changed_files_facts(changed: &[String]) -> String {
    let mut out = String::from("## Arquivos alterados desde a revisão anterior (DADO)\n");
    if changed.is_empty() {
        out.push_str("(nenhum)\n");
    }
    for path in changed.iter().take(CHANGED_FILES_MAX) {
        out.push_str(&format!("- {path}\n"));
    }
    if changed.len() > CHANGED_FILES_MAX {
        out.push_str(&format!("… e mais {}\n", changed.len() - CHANGED_FILES_MAX));
    }
    out
}

fn converge_findings(lang: Lang, mut result: ReviewResult, previous: &[Finding], hunks: &[ChangedHunk]) -> Converged {
    if result.verdict == Verdict::Approved {
        let observations = std::mem::take(&mut result.findings);
        return Converged { result, observations };
    }
    // Sem hunks (sem base do diff) não há como saber o que mudou: nada é rebaixado.
    if result.verdict != Verdict::ChangesRequested || hunks.is_empty() {
        return Converged { result, observations: Vec::new() };
    }
    let prev_paths: Vec<String> = previous.iter().flat_map(|f| parse_locations(&f.location)).map(|(p, _)| p).collect();
    let blocks = |f: &Finding| -> bool {
        if !is_medium(f) {
            return true;
        }
        let locs = parse_locations(&f.location);
        locs.is_empty()
            || locs.iter().any(|(path, lines)| {
                prev_paths.iter().any(|p| same_file_path(p, path))
                    || hunks.iter().any(|h| same_file_path(&h.path, path) && lines.is_none_or(|(a, b)| a <= h.end && b >= h.start))
            })
    };
    let unmet = result.criteria.iter().any(|c| !c.met && !is_manual_criterion(&c.criterion));
    let has_blocking = result.findings.iter().any(|f| blocks(f));
    let demotable = result.findings.iter().any(|f| !blocks(f));
    // Rebaixar sem aprovar só esconderia apontamentos do Desenvolvedor (spec 082): tudo bloqueia.
    if unmet || has_blocking || !demotable {
        return Converged { result, observations: Vec::new() };
    }
    let observations = std::mem::take(&mut result.findings);
    result.verdict = Verdict::Approved;
    result.summary = i18n::tr(lang, "runner.review.untouched_code_demoted", &[("summary", result.summary.trim_end())]);
    Converged { result, observations }
}

fn finding_line(f: &Finding, with_recommendation: bool, redact: &dyn Fn(&str) -> String) -> String {
    let flat = |t: &str| redact(t).split_whitespace().collect::<Vec<_>>().join(" ");
    let severity = match f.confidence {
        Confidence::High => flat(&f.severity),
        Confidence::Low => format!("{}, {LOW_CONFIDENCE_LABEL}", flat(&f.severity)),
    };
    let mut line = format!("{} [{severity}] {}: {}", flat(&f.id), flat(&f.location), flat(&f.description));
    let rec = flat(&f.recommendation);
    if with_recommendation && !rec.is_empty() {
        line.push_str(" → ");
        line.push_str(&rec);
    }
    let mut line = clip_line(line);
    // Spec 087 T05: o cenário de falha vai numa linha própria, recuada, logo abaixo do finding.
    let scenario = flat(&f.failure_scenario);
    if !scenario.is_empty() {
        line.push_str(&format!("\n  {}", clip_line(format!("{SCENARIO_PREFIX} {scenario}"))));
    }
    line
}

/// Corta a linha em [`FINDING_LINE_BYTES`] (limite de caractere UTF-8), com reticências.
fn clip_line(mut line: String) -> String {
    if line.len() > FINDING_LINE_BYTES {
        let mut end = FINDING_LINE_BYTES;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push('…');
    }
    line
}

/// Mensagem do Revisor na conversa (spec 078): veredito e resumo; `Apontamentos:`; `Observações
/// (não bloqueiam):`; `Capturas avaliadas:` (spec 082 T03: uma linha com os nomes das cópias em
/// `.sdd/specs/<spec>/captures/`, separados por vírgula); por último `Critérios:` (a UI só lê o
/// checklist depois dessa marca).
pub fn render_review_message(
    verdict: Verdict,
    summary: &str,
    findings: &[Finding],
    observations: &[Finding],
    captures: &[String],
    criteria: &[CriterionCheck],
    redact: &dyn Fn(&str) -> String,
) -> String {
    let label = if verdict == Verdict::Approved { VERDICT_APPROVED_LABEL } else { VERDICT_CHANGES_LABEL };
    let mut out = format!("{label}: {}", summary.trim());
    if !findings.is_empty() {
        out.push_str(&format!("\n\n{FINDINGS_HEADER}"));
        for f in findings {
            out.push_str(&format!("\n- {}", finding_line(f, true, redact)));
        }
    }
    if !observations.is_empty() {
        out.push_str(&format!("\n\n{OBSERVATIONS_HEADER}"));
        for f in observations {
            out.push_str(&format!("\n- {}", finding_line(f, false, redact)));
        }
    }
    if !captures.is_empty() {
        out.push_str(&format!("\n\n{CAPTURES_HEADER} {}", captures.join(", ")));
    }
    if !criteria.is_empty() {
        out.push_str(&format!("\n\n{CRITERIA_HEADER}\n{}", render_criteria(criteria)));
    }
    out
}

/// Acrescenta as observações à seção "Observações da revisão" (uma linha por finding, sem repetição,
/// mantendo as mais recentes dentro do teto). `None` quando não há nada a registrar nem seção existente.
fn append_observations(existing: &str, observations: &[Finding], redact: &dyn Fn(&str) -> String) -> Option<String> {
    let mut lines: Vec<String> = existing.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect();
    for f in observations {
        // Uma linha por finding: o cenário de falha (linha recuada) fica só na conversa.
        let full = finding_line(f, false, redact);
        let line = format!("- {}", full.lines().next().unwrap_or_default());
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    while lines.len() > 1 && lines.iter().map(|l| l.len() + 1).sum::<usize>() > OBSERVATIONS_MAX_BYTES {
        lines.remove(0);
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Veredito do Revisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewResult {
    pub verdict: Verdict,
    pub summary: String,
    pub findings: Vec<Finding>,
    /// Matriz critério → evidência; vazia quando o Revisor não a devolveu.
    pub criteria: Vec<CriterionCheck>,
    /// Capturas de tela vistas pelo Revisor com `view_screenshot` (spec 082 T03), na ordem; vazio
    /// sem capturas ou depois da recusa de imagens pelo provedor.
    pub screenshots_viewed: Vec<String>,
}

/// Resultado de `test_command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    pub passed: bool,
    pub output: String,
    /// Código de saída do processo, quando houve saída normal.
    pub exit_code: Option<i32>,
    /// `true` quando o tempo limite foi excedido.
    pub timed_out: bool,
}

/// Papel Desenvolvedor: implementa o pedido dentro da worktree.
pub trait DeveloperPort: Send + Sync {
    fn implement<'a>(&'a self, brief: &'a DevBrief) -> Fut<'a, Result<DevReport, RunnerError>>;

    /// Geração de imagens na execução da tarefa `task_id` (spec 092 T03): a última falha final
    /// registrada nesta execução ou a disponibilidade atual. Sem gerador, indisponível.
    fn image_generation(&self, _task_id: &str) -> crate::sdd::images::GenerationStatus {
        crate::sdd::images::GenerationStatus::Unavailable
    }

    /// Início de uma tentativa da tarefa (spec 092 T03): esquece o que a tentativa anterior
    /// registrou (falha de geração de imagem). As sessões dentro da tentativa não chamam isto.
    fn begin_attempt(&self, _task_id: &str) {}
}

/// Papel Revisor: avalia o diff e devolve veredito estruturado.
pub trait ReviewerPort: Send + Sync {
    fn review<'a>(&'a self, input: &'a ReviewInput)
        -> Fut<'a, Result<ReviewResult, RunnerError>>;
}

/// Executor do `test_command` em um diretório.
pub trait TestExecutor: Send + Sync {
    fn run<'a>(&'a self, dir: &'a Path, command: &'a str)
        -> Fut<'a, Result<TestResult, RunnerError>>;
}

/// Tipo de comando de verificação em execução.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Tests,
    Check,
}

impl CommandKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tests => "tests",
            Self::Check => "check",
        }
    }
}

static COMMAND_RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Parâmetros de um aviso com `code` (spec 102): só textos neutros de idioma (ids, caminhos,
/// erros técnicos). O saneamento final é do servidor (`message_params`).
fn notice_params(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), serde_json::Value::from(*v))).collect()
}

/// Problema não fatal ao concluir uma tarefa: vira um `SystemNotice` com `code` (spec 102).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskWarning {
    /// Não deu para devolver a posse dos arquivos de `path` ao usuário do host.
    Ownership { path: String, error: String },
    /// Não deu para apagar a worktree `path` (a tarefa já está integrada).
    Cleanup { path: String, error: String },
}

impl TaskWarning {
    /// Texto de reserva no idioma do projeto (`system.<code>`), `code` e `params`.
    fn notice(&self, lang: Lang) -> (String, &'static str, serde_json::Map<String, serde_json::Value>) {
        let (code, path, error) = match self {
            Self::Ownership { path, error } => ("task.ownership_failed", path, error),
            Self::Cleanup { path, error } => ("task.cleanup_failed", path, error),
        };
        let pairs = [("path", path.as_str()), ("error", error.as_str())];
        (system_text(lang, code, &pairs), code, notice_params(&pairs))
    }
}

/// Texto de reserva de uma mensagem do sistema com `code` (spec 115 T03): `system.<code>` do
/// dicionário no idioma do projeto, o mesmo texto que a web monta pelo código.
fn system_text(lang: Lang, code: &str, params: &[(&str, &str)]) -> String {
    i18n::tr(lang, &format!("system.{code}"), params)
}

/// Evento emitido nas transições e mensagens dos agentes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerEvent {
    AgentMessage {
        agent: String,
        text: String,
        /// Tarefa em curso; `None` em mensagens sem tarefa (ex.: sistema).
        task_id: Option<String>,
        /// Tentativa corrente da tarefa (1-based: `attempts + 1`); `None` sem tarefa.
        attempt: Option<u32>,
    },
    /// Início/fim de um `test_command` ou checagem (atividade ao vivo "Testes", spec 071). Não é
    /// persistido: o resultado final segue como `AgentMessage` do agente `tests`.
    CommandRun {
        /// Id único da execução; o fim referencia o mesmo id do início.
        run_id: String,
        /// `true` no início, `false` no fim.
        started: bool,
        kind: CommandKind,
        /// Rótulo de reserva em pt-BR (ex.: "Testes · tentativa 2", "Checagem build · tentativa 2").
        label: String,
        /// `code` estável da etapa (spec 102): `tests`, `tests_base`, `check` ou `check_base`.
        stage_code: &'static str,
        /// `params` da etapa: `attempt` e, na checagem, `check` (nome).
        stage_params: serde_json::Map<String, serde_json::Value>,
        /// Comando já com scrub.
        command: String,
        task_id: String,
        attempt: u32,
    },
    TaskUpdated {
        spec_id: String,
        task_id: String,
        status: TaskStatus,
    },
    Integrated {
        spec_id: String,
        task_id: String,
    },
    /// Mensagem do sistema (agente `system`) com `code` estável e `params` neutros de idioma (spec
    /// 102): a interface a mostra pelo `code` no idioma atual; `text` é a reserva em pt-BR (e o
    /// que vai para os registros).
    SystemNotice {
        text: String,
        code: String,
        params: serde_json::Map<String, serde_json::Value>,
        /// Tarefa a que o aviso se refere (vai para o diálogo da spec com ela); `None` sem tarefa.
        task_id: Option<String>,
    },
}

/// Confirmação humana (Padrão) da tarefa de correção criada pela verificação final; em YOLO a
/// implementação aprova sozinha. Sem porta configurada, o runner cria a tarefa direto.
pub trait FinalGate: Send + Sync {
    fn approve_fix<'a>(&'a self, spec_id: &'a str, reason: &'a str) -> Fut<'a, bool>;
}

/// Chave do dicionário com o título da tarefa de correção criada pela verificação final (spec 107
/// T01): o título sai no idioma ativo do projeto e fica gravado no `Txx.md` como conteúdo.
pub const FINAL_FIX_TITLE_KEY: &str = "runner.final_fix_title";

/// Título da tarefa de correção no idioma.
pub fn final_fix_title(lang: crate::i18n::Lang) -> String {
    crate::i18n::text(lang, FINAL_FIX_TITLE_KEY)
}

/// A tarefa é a de correção da verificação final (marca da 2ª rodada)? Pela marca estável
/// `kind: "final_fix"` (spec 107 T01) ou, nas tarefas gravadas antes dela, pelo título em qualquer
/// idioma suportado.
pub fn is_final_fix(task: &Task) -> bool {
    let meta = &task.frontmatter;
    meta.kind.as_deref() == Some(crate::sdd::TASK_KIND_FINAL_FIX)
        || [crate::i18n::Lang::PtBr, crate::i18n::Lang::En]
            .into_iter()
            .any(|lang| meta.title == final_fix_title(lang))
}
/// Seção de critérios globais do spec.md.
pub const GLOBAL_CRITERIA: &str = "Critérios de Aceitação Globais";

/// Resultado da verificação final.
enum FinalStep {
    Complete,
    Continue,
}

/// Destino dos eventos do runner.
pub trait EventSink: Send + Sync {
    fn emit(&self, ev: RunnerEvent);
}

/// Descarta todos os eventos.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _ev: RunnerEvent) {}
}

/// Executa `test_command` real com `sh -c`, cwd = diretório pedido, timeout e saída limitada.
///
/// O comando vem do `settings.yaml` configurado pelo humano, nunca de um agente.
#[derive(Debug, Clone)]
pub struct ShellTestExecutor {
    timeout: Duration,
    /// Idioma das marcas que o executor acrescenta à saída (corte e tempo limite), lido a cada
    /// execução: é o idioma do projeto (spec 115 T04).
    lang: i18n::SharedLang,
}

impl ShellTestExecutor {
    /// Executor com o timeout padrão de [`TEST_TIMEOUT`].
    pub fn new() -> Self {
        Self::with_timeout(TEST_TIMEOUT)
    }

    /// Executor com timeout customizado (usado em testes).
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout, lang: i18n::SharedLang::default() }
    }

    /// Marcas da saída no idioma do projeto.
    pub fn with_lang(mut self, lang: i18n::SharedLang) -> Self {
        self.lang = lang;
        self
    }
}

impl Default for ShellTestExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl TestExecutor for ShellTestExecutor {
    fn run<'a>(
        &'a self,
        dir: &'a Path,
        command: &'a str,
    ) -> Fut<'a, Result<TestResult, RunnerError>> {
        let dir = dir.to_path_buf();
        let command = command.to_string();
        let timeout = self.timeout;
        let lang = self.lang.get();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || run_shell(&dir, &command, timeout, lang))
                .await
                .map_err(|e| RunnerError::Tests(format!("execução interrompida: {e}")))?
        })
    }
}

/// Acumula saída até `MAX_TEST_OUTPUT_BYTES` e marca quando houve descarte.
#[derive(Default)]
struct Capture {
    buf: Vec<u8>,
    truncated: bool,
}

impl Capture {
    fn push(&mut self, chunk: &[u8]) {
        let room = MAX_TEST_OUTPUT_BYTES - self.buf.len();
        if chunk.len() <= room {
            self.buf.extend_from_slice(chunk);
        } else {
            self.buf.extend_from_slice(&chunk[..room]);
            self.truncated = true;
        }
    }
}

/// Copia `reader` para o canal até EOF ou erro.
fn pump(mut reader: impl Read, tx: Sender<Vec<u8>>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

/// Marca de tempo limite acrescentada à saída dos testes (no host e no Docker), no idioma do
/// projeto (spec 115 T04): a pessoa a vê na saída dos testes.
fn timed_out_mark(lang: Lang, timeout: Duration) -> String {
    let secs = timeout.as_secs().to_string();
    format!("\n[{}]\n", i18n::tr(lang, "runner.output.timed_out", &[("secs", &secs)]))
}

/// Executa `command` com `sh -c` em `dir`. Após o timeout, mata o processo e não espera os pipes.
fn run_shell(dir: &Path, command: &str, timeout: Duration, lang: Lang) -> Result<TestResult, RunnerError> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RunnerError::Tests(format!("não foi possível iniciar `{command}`: {e}")))?;

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        thread::spawn(move || pump(out, tx));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        thread::spawn(move || pump(err, tx));
    }
    drop(tx);

    let deadline = Instant::now() + timeout;
    let mut capture = Capture::default();
    let mut timed_out = false;
    let mut exit_ok = false;
    let mut exit_code: Option<i32> = None;
    loop {
        while let Ok(chunk) = rx.try_recv() {
            capture.push(&chunk);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_ok = status.success();
                exit_code = status.code();
                break;
            }
            Ok(None) => {}
            Err(e) => return Err(RunnerError::Tests(format!("falha ao aguardar teste: {e}"))),
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }

    let grace_end = Instant::now() + DRAIN_GRACE;
    loop {
        let left = grace_end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(chunk) => capture.push(&chunk),
            Err(_) => break,
        }
    }

    let mut output = String::from_utf8_lossy(&capture.buf).into_owned();
    if capture.truncated {
        output.push_str(&format!("\n[{}]\n", i18n::text(lang, "runner.output.truncated")));
    }
    if timed_out {
        output.push_str(&timed_out_mark(lang, timeout));
    }
    Ok(TestResult {
        passed: !timed_out && exit_ok,
        exit_code,
        timed_out,
        output,
    })
}

/// Orquestrador do ciclo de tarefa e da sequência de uma spec.
pub struct TaskRunner {
    repo_root: PathBuf,
    developer: Box<dyn DeveloperPort>,
    reviewer: Box<dyn ReviewerPort>,
    tests: Box<dyn TestExecutor>,
    events: Box<dyn EventSink>,
    /// Último conteúdo conhecido (lido ou gravado pelo runner) de cada arquivo de estado.
    baselines: SharedBaselines,
    /// Pedido de cancelamento cooperativo (padrão: nunca cancelado).
    cancel: Arc<AtomicBool>,
    /// Compose de testes (`environment: docker`); sem ele, o modo docker bloqueia por ambiente.
    compose: Option<Arc<ComposeEnv>>,
    /// Orçamento da spec, consultado antes de cada chamada de agente.
    budget: Option<Arc<dyn BudgetGuard>>,
    /// Segredos conhecidos, redigidos do histórico de tentativas gravado na tarefa.
    secrets: Option<Arc<crate::server::events::SecretSet>>,
    /// Confirmação da tarefa de correção da verificação final (HITL no modo Padrão).
    final_gate: Option<Arc<dyn FinalGate>>,
    /// Atividade ao vivo da verificação final (agente reviewer, sem tarefa); o sink traz o `stage`.
    final_activity: Option<crate::agents::activity::ActivityScope>,
    /// Resultado de checagens não bloqueantes na base, por (sha da base, nome, comando): `Some(passou)`
    /// ou `None` se não executou. Como a base só muda na integração, cada checagem roda na base no
    /// máximo uma vez por tarefa (spec 080 T01).
    base_checks: Mutex<HashMap<(String, String, String), Option<bool>>>,
    /// Avisos de formatador que falhou já mostrados nesta execução, por (formatador, texto): a
    /// mesma falha não se repete na conversa (spec 092 T05).
    formatter_failures: Mutex<HashSet<(String, String)>>,
    /// Idioma ativo do projeto (spec 107 T01): título da tarefa de correção da verificação final.
    lang: crate::i18n::SharedLang,
}

/// Rótulo (`stage`) da atividade ao vivo da verificação final.
pub const FINAL_VERIFICATION_STAGE: &str = "final_verification";

/// Próximo passo após uma etapa: continuar o laço ou encerrar com resultado.
enum Step {
    Next,
    Finish(Outcome),
}

impl TaskRunner {
    /// Cria o runner para o repositório em `repo_root`.
    pub fn new(
        repo_root: impl Into<PathBuf>,
        developer: Box<dyn DeveloperPort>,
        reviewer: Box<dyn ReviewerPort>,
        tests: Box<dyn TestExecutor>,
        events: Box<dyn EventSink>,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            developer,
            reviewer,
            tests,
            events,
            baselines: Arc::new(Mutex::new(HashMap::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            compose: None,
            budget: None,
            secrets: None,
            final_gate: None,
            final_activity: None,
            base_checks: Mutex::new(HashMap::new()),
            formatter_failures: Mutex::new(HashSet::new()),
            lang: crate::i18n::SharedLang::default(),
        }
    }

    /// Idioma ativo do projeto, lido na hora de criar a tarefa de correção da verificação final.
    pub fn with_lang(mut self, lang: crate::i18n::SharedLang) -> Self {
        self.lang = lang;
        self
    }

    /// Sink da atividade ao vivo da verificação final (o sink deve marcar `stage: "final_verification"`).
    pub fn with_final_activity(mut self, sink: Arc<dyn crate::agents::activity::ActivitySink>, spec_id: &str) -> Self {
        self.final_activity = Some(crate::agents::activity::ActivityScope::new(
            sink,
            "reviewer",
            Some(spec_id.to_string()),
            None,
        ));
        self
    }

    /// Porta de confirmação da tarefa de correção da verificação final.
    pub fn with_final_gate(mut self, gate: Arc<dyn FinalGate>) -> Self {
        self.final_gate = Some(gate);
        self
    }

    /// Segredos a redigir do histórico de tentativas (resumo do Dev e `--stat`).
    pub fn with_secrets(mut self, secrets: Arc<crate::server::events::SecretSet>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    fn scrub(&self, text: &str) -> String {
        match &self.secrets {
            Some(s) => crate::server::projects::scrub_text(s, text),
            None => text.to_string(),
        }
    }

    /// Progresso de uma sessão do Desenvolvedor que esgotou o limite de passos (spec 081 T01):
    /// resumo redigido (≤ 2 KiB) + `diff --stat` da branch (≤ 1 KiB).
    fn progress_note(&self, settings: &Settings, task: &Task, summary: &str) -> String {
        let stat = self
            .git()
            .diff_stat(&settings.base_branch, &task.frontmatter.branch)
            .unwrap_or_default();
        format!(
            "Resumo da sessão:\n{}\n\ndiff --stat da branch:\n{}",
            indent_headings(&tail_bytes_head(self.scrub(summary).trim(), HISTORY_SUMMARY_BYTES)),
            indent_headings(&tail_bytes_head(self.scrub(&stat).trim(), HISTORY_STAT_BYTES)),
        )
    }

    /// Acrescenta a tentativa do Dev à seção "Tentativas anteriores" (teto total, mantendo as mais recentes).
    fn record_attempt(&self, settings: &Settings, task: &mut Task, summary: &str) -> Result<(), RunnerError> {
        let stat = self
            .git()
            .diff_stat(&settings.base_branch, &task.frontmatter.branch)
            .unwrap_or_default();
        let existing = section_text(&task.body, HISTORY).unwrap_or_default();
        let n = next_attempt_number(&existing);
        let entry = format!(
            "### Tentativa {n}\nResumo do Desenvolvedor:\n{}\n\ndiff --stat:\n{}\n",
            indent_headings(&tail_bytes_head(self.scrub(summary).trim(), HISTORY_SUMMARY_BYTES)),
            indent_headings(&tail_bytes_head(self.scrub(&stat).trim(), HISTORY_STAT_BYTES)),
        );
        let text = append_history(&existing, &entry, HISTORY_MAX_BYTES);
        task.body = replace_section(&task.body, HISTORY, Some(text.trim_end().to_string()));
        // Sem `TaskUpdated`: o status não mudou; só o corpo (mesmo caminho de escrita guardada).
        let path = self.task_path(&task.frontmatter.spec_id, &task.frontmatter.id);
        self.write_guarded(&path, &task.render()?)
    }

    /// Compartilha as linhas de base de escrita com um [`TaskEditor`] (spec 055): edições do
    /// Arquiteto feitas durante a execução de um agente não contam como edição externa.
    pub fn with_baselines(mut self, baselines: SharedBaselines) -> Self {
        self.baselines = baselines;
        self
    }

    /// Relê do disco o que `ask_architect` pode ter mudado durante a execução de um agente
    /// (`allowed_scope`, critérios e orientações), para que a próxima gravação não o desfaça.
    fn sync_advice(&self, task: &mut Task) {
        let path = self.task_path(&task.frontmatter.spec_id, &task.frontmatter.id);
        let Ok(disk) = crate::sdd::read_task(&path) else { return };
        let Some(advice) = section_text(&disk.body, ADVICE_SECTION) else { return };
        task.frontmatter.allowed_scope = disk.frontmatter.allowed_scope.clone();
        if let Some(acceptance) = section_text(&disk.body, ACCEPTANCE) {
            task.body = replace_section(&task.body, ACCEPTANCE, Some(acceptance.trim_end().to_string()));
        }
        task.body = replace_section(&task.body, ADVICE_SECTION, Some(advice.trim_end().to_string()));
    }

    /// Liga o Compose de testes usado quando `settings.environment` é `docker`.
    pub fn with_compose(mut self, compose: Arc<ComposeEnv>) -> Self {
        self.compose = Some(compose);
        self
    }

    /// Executa `command` em `dir`: no host (`TestExecutor`) ou, em modo docker, no serviço
    /// `workspace` do Compose. `fresh` = é o `test_command` (recria o estado antes).
    async fn exec_cmd(
        &self,
        settings: &Settings,
        dir: &Path,
        command: &str,
        fresh: bool,
    ) -> Result<TestResult, RunnerError> {
        if settings.environment != Environment::Docker {
            return self.tests.run(dir, command).await;
        }
        let compose = self
            .compose
            .clone()
            .ok_or_else(|| RunnerError::Docker("projeto em modo docker sem executor de compose".into()))?;
        let (dir, command) = (dir.to_path_buf(), command.to_string());
        let joined = tokio::task::spawn_blocking(move || {
            if fresh {
                compose.prepare(&dir)?;
            }
            let out = compose.exec(&dir, &command, TEST_TIMEOUT);
            // Devolve ao usuário o que um container root (imagens antigas) tenha criado.
            let _ = compose.repair_ownership(&dir);
            out
        })
        .await
        .map_err(|e| RunnerError::Tests(format!("execução interrompida: {e}")))?;
        let mut out = joined.map_err(RunnerError::Docker)?;
        if out.timed_out {
            out.output.push_str(&timed_out_mark(self.lang.get(), TEST_TIMEOUT));
        }
        Ok(TestResult { passed: out.success(), output: out.output, exit_code: out.code, timed_out: out.timed_out })
    }

    /// `exec_cmd` com os eventos de início/fim ao vivo (`RunnerEvent::CommandRun`). `check` =
    /// nome da checagem (`None` = `test_command`); `place` = " na base" ou vazio.
    #[allow(clippy::too_many_arguments)]
    async fn run_tracked(
        &self,
        settings: &Settings,
        dir: &Path,
        command: &str,
        fresh: bool,
        check: Option<&str>,
        place: &str,
        task_id: &str,
        attempt: u32,
    ) -> Result<TestResult, RunnerError> {
        let (kind, what) = match check {
            Some(name) => (CommandKind::Check, format!("Checagem {name}")),
            None => (CommandKind::Tests, "Testes".to_string()),
        };
        let stage_code = match (check.is_some(), place.is_empty()) {
            (false, true) => "tests",
            (false, false) => "tests_base",
            (true, true) => "check",
            (true, false) => "check_base",
        };
        let mut stage_params = serde_json::Map::new();
        stage_params.insert("attempt".into(), serde_json::Value::from(attempt));
        if let Some(name) = check {
            stage_params.insert("check".into(), serde_json::Value::from(name));
        }
        let run_id = format!(
            "{}-{}-{}",
            task_id,
            attempt,
            COMMAND_RUN_SEQ.fetch_add(1, Ordering::SeqCst)
        );
        let event = |started: bool| RunnerEvent::CommandRun {
            run_id: run_id.clone(),
            started,
            kind,
            label: format!("{what}{place} · tentativa {attempt}"),
            stage_code,
            stage_params: stage_params.clone(),
            command: self.scrub(command),
            task_id: task_id.to_string(),
            attempt,
        };
        self.events.emit(event(true));
        let out = self.exec_cmd(settings, dir, command, fresh).await;
        self.events.emit(event(false));
        out
    }

    /// Mensagem do agente `tests` na conversa da tarefa.
    fn tests_message(&self, task: &Task, attempt: u32, text: String) {
        self.events.emit(RunnerEvent::AgentMessage {
            agent: "tests".into(),
            text,
            task_id: Some(task.frontmatter.id.clone()),
            attempt: Some(attempt),
        });
    }

    /// Formatação automática por módulo (spec 078), depois do commit do Desenvolvedor e antes dos
    /// testes. Roda, em ordem, os formatadores cujos `paths` casam com os arquivos alterados pela
    /// tarefa. Formatador que falha tem as mudanças descartadas e não bloqueia a tarefa; o que ficou
    /// fora de `allowed_scope` é restaurado e o resto vira um único commit. Só o cancelamento é erro.
    async fn format_delivery(
        &self,
        settings: &Settings,
        task: &Task,
        wt: &Path,
        attempt: u32,
    ) -> Result<(), RunnerError> {
        if settings.formatters.is_empty() {
            return Ok(());
        }
        let changed: Vec<String> = Git::new(wt.to_path_buf())
            .changed_paths(&settings.base_branch, &task.frontmatter.branch)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| wt.join(p).exists())
            .collect();
        let selected = formatting::select(&settings.formatters, &changed);
        if selected.is_empty() {
            return Ok(());
        }
        let outcome = self.run_formatters(settings, task, wt, attempt, &selected).await;
        if outcome.is_err() {
            // Cancelamento no meio: a worktree não fica com formatação pela metade.
            let _ = formatting::reset_to_head(wt);
        }
        outcome
    }

    /// Uma execução do formatador: no serviço `workspace` (docker) ou no host, em `workdir`.
    async fn exec_formatter(&self, settings: &Settings, wt: &Path, f: &Formatter) -> Result<TestResult, RunnerError> {
        if settings.environment == Environment::Docker {
            self.exec_cmd(settings, wt, &formatting::workspace_command(f), false).await
        } else {
            match formatting::host_dir(wt, f) {
                Ok(dir) => self.exec_cmd(settings, &dir, &f.command, false).await,
                Err(why) => Err(RunnerError::Tests(why)),
            }
        }
    }

    async fn run_formatters(
        &self,
        settings: &Settings,
        task: &Task,
        wt: &Path,
        attempt: u32,
        selected: &[&Formatter],
    ) -> Result<(), RunnerError> {
        let lang = self.lang.get();
        let failed = |why: String| i18n::tr(lang, "runner.format.failed", &[("why", &why)]);
        if let Err(why) = formatting::snapshot(wt) {
            self.tests_message(task, attempt, self.scrub(&failed(why)));
            return Ok(());
        }
        let mut applied: Vec<&str> = Vec::new();
        for f in selected {
            self.check_cancel()?;
            let mut result = self.exec_formatter(settings, wt, f).await;
            // Spec 092 T05: formatador que sai ≠ 0 sem estourar o tempo (comum quando ele altera
            // arquivos) roda de novo; se a 2ª sair 0, as mudanças da 1ª ficam.
            if matches!(&result, Ok(r) if !r.passed && !r.timed_out) {
                self.check_cancel()?;
                result = self.exec_formatter(settings, wt, f).await;
            }
            let problem = match result {
                Ok(r) if r.passed => None,
                Ok(r) if r.timed_out => Some(i18n::text(lang, "runner.format.timed_out")),
                Ok(r) => {
                    let code = r.exit_code.map_or_else(|| "?".to_string(), |c| c.to_string());
                    let output = tail_bytes(r.output.trim_end(), FORMATTER_TAIL_BYTES);
                    Some(i18n::tr(lang, "runner.format.exit", &[("code", &code), ("output", output)]))
                }
                Err(RunnerError::Cancelled) => return Err(RunnerError::Cancelled),
                Err(e) => Some(e.to_string()),
            };
            match problem {
                None => {
                    if formatting::touches_scope(wt, &task.frontmatter.allowed_scope).unwrap_or(false) {
                        applied.push(&f.name);
                    }
                    if let Err(why) = formatting::snapshot(wt) {
                        self.tests_message(task, attempt, self.scrub(&failed(why)));
                        let _ = formatting::reset_to_head(wt);
                        return Ok(());
                    }
                }
                Some(why) => {
                    if let Err(e) = formatting::discard(wt) {
                        let _ = formatting::reset_to_head(wt);
                        self.tests_message(task, attempt, self.scrub(&failed(e)));
                        return Ok(());
                    }
                    let text = self.scrub(&i18n::tr(lang, "runner.format.tool_failed", &[("name", &f.name), ("why", &why)]));
                    let first = self
                        .formatter_failures
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert((f.name.clone(), text.clone()));
                    if first {
                        self.tests_message(task, attempt, text);
                    }
                }
            }
        }
        let prefix = format!("style({}): {} - ", task.frontmatter.spec_id, task.frontmatter.id);
        let message = i18n::commit_subject(lang, &prefix, "commit.formatting");
        match formatting::finalize(wt, &task.frontmatter.allowed_scope, &message) {
            Ok(files) if files.is_empty() => {}
            Ok(files) => {
                let count = u64::try_from(files.len()).unwrap_or(u64::MAX);
                let text = i18n::tr_plural(lang, "runner.format.applied", count, &[("names", &applied.join(", "))]);
                self.tests_message(task, attempt, text);
            }
            Err(why) => {
                let _ = formatting::reset_to_head(wt);
                self.tests_message(task, attempt, self.scrub(&failed(why)));
            }
        }
        Ok(())
    }

    /// Orçamento da spec: atingido, a tarefa corrente vai a `BLOCKED` ([`BlockKind::Budget`]).
    pub fn with_budget(mut self, budget: Arc<dyn BudgetGuard>) -> Self {
        self.budget = Some(budget);
        self
    }

    /// `Some(Step)` quando o orçamento da spec já foi atingido (antes de chamar um agente).
    fn budget_stop(&self, task: &mut Task) -> Result<Option<Step>, RunnerError> {
        let Some(guard) = &self.budget else {
            return Ok(None);
        };
        match guard.exceeded(&task.frontmatter.spec_id, self.lang.get()) {
            Some(text) => Ok(Some(self.block(task, BlockReason { code: "budget", text }, BlockKind::Budget)?)),
            None => Ok(None),
        }
    }

    /// Define o sinal de cancelamento cooperativo. É checado entre etapas (antes de cada tarefa,
    /// tentativa do Desenvolvedor, revisão e integração), nunca no meio do squash e dos testes da base.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    /// `Err(Cancelled)` quando o cancelamento foi pedido. Não toca o estado.
    fn check_cancel(&self) -> Result<(), RunnerError> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(RunnerError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Executa (ou retoma) a tarefa `task_id` da spec `spec_id` até `DONE` ou `BLOCKED`.
    pub async fn run_task(&self, spec_id: &str, task_id: &str) -> Result<Outcome, RunnerError> {
        validate_id("spec_id", spec_id)?;
        validate_id("task_id", task_id)?;
        let settings = read_settings(&sdd_dir(&self.repo_root).join("settings.yaml"))?;
        if settings.base_branch.trim().is_empty() {
            return Err(RunnerError::Config("base_branch vazio".into()));
        }
        if settings.test_command.trim().is_empty() {
            return Err(RunnerError::Config("test_command vazio".into()));
        }
        let path = self.task_path(spec_id, task_id);
        self.remember(&path)?;
        let mut task = read_task(&path)?;
        if task.frontmatter.spec_id != spec_id || task.frontmatter.id != task_id {
            return Err(RunnerError::Invalid(format!(
                "arquivo de {spec_id}/{task_id} declara {}/{}",
                task.frontmatter.spec_id, task.frontmatter.id
            )));
        }
        match self.drive(&settings, &mut task).await {
            Err(RunnerError::Docker(reason)) => Ok(Outcome::Blocked {
                reason: self.block_reason("docker", &[("reason", &reason)]).text,
                kind: BlockKind::Environmental,
                open_findings: vec![],
            }),
            Err(RunnerError::ExternalEdit { path }) => Ok(Outcome::Blocked {
                reason: self.block_reason("external_edit", &[("path", &path.to_string())]).text,
                kind: BlockKind::Environmental,
                open_findings: vec![],
            }),
            other => other,
        }
    }

    /// Laço da máquina de estados. `DONE` e `INTEGRATING` são conferidos contra o Git primeiro.
    async fn drive(&self, settings: &Settings, task: &mut Task) -> Result<Outcome, RunnerError> {
        loop {
            self.check_cancel()?;
            let spec_id = task.frontmatter.spec_id.clone();
            let task_id = task.frontmatter.id.clone();
            if matches!(
                task.frontmatter.status,
                TaskStatus::Done | TaskStatus::Integrating | TaskStatus::Blocked
            ) {
                let committed = self.is_committed(&settings.base_branch, &spec_id, &task_id)?;
                match (task.frontmatter.status, committed) {
                    (TaskStatus::Done, true) => return Ok(Outcome::Done),
                    (TaskStatus::Done, false) => {
                        self.transition(
                            task,
                            TaskStatus::Integrating,
                            "DONE sem commit na base: refazer integração",
                        )?;
                        continue;
                    }
                    (_, true) => {
                        return match self.finalize_resumed(&self.git(), task)? {
                            Step::Finish(outcome) => Ok(outcome),
                            Step::Next => Ok(Outcome::Done),
                        }
                    }
                    _ => {}
                }
            }
            let step = match task.frontmatter.status {
                TaskStatus::Done => Step::Finish(Outcome::Done),
                TaskStatus::Blocked => {
                    let open_findings = read_findings(&task.body)?;
                    return Ok(Outcome::Blocked {
                        reason: reason_of(task, self.lang.get()),
                        kind: BlockKind::infer(&open_findings),
                        open_findings,
                    });
                }
                TaskStatus::Todo => {
                    self.check_deps(&settings.base_branch, task)?;
                    self.ensure_worktree(settings, task)?;
                    self.transition(task, TaskStatus::InProgress, "aguardar developer")?;
                    Step::Next
                }
                TaskStatus::InProgress | TaskStatus::ChangesRequested => {
                    self.develop(settings, task).await?
                }
                TaskStatus::InReview => self.review(settings, task).await?,
                TaskStatus::Approved | TaskStatus::Integrating => {
                    self.integrate(settings, task).await?
                }
            };
            if let Step::Finish(outcome) = step {
                return Ok(outcome);
            }
        }
    }

    /// Executa as tarefas elegíveis da spec em ordem, respeitando `depends_on`.
    ///
    /// Para na primeira tarefa BLOCKED. Quando todas estão DONE, marca a spec `COMPLETED`.
    pub async fn run_spec(&self, spec_id: &str) -> Result<SpecOutcome, RunnerError> {
        validate_id("spec_id", spec_id)?;
        let settings = read_settings(&sdd_dir(&self.repo_root).join("settings.yaml"))?;
        self.remember(&self.spec_dir(spec_id).join("spec.md"))?;
        loop {
            self.check_cancel()?;
            let tasks = self.load_tasks(spec_id)?;
            if tasks.is_empty() {
                return Err(RunnerError::Invalid(format!("spec {spec_id} sem tarefas")));
            }
            let committed = self.committed_ids(&settings.base_branch, spec_id, &tasks)?;
            // Concluída = DONE em disco e commit de integração na base.
            let finished = |t: &Task| {
                t.frontmatter.status == TaskStatus::Done && committed.contains(&t.frontmatter.id)
            };
            if tasks.iter().all(finished) {
                if let FinalStep::Continue = self.final_verification(&settings, spec_id, &tasks).await? {
                    continue;
                }
                self.complete_spec(spec_id)?;
                return Ok(SpecOutcome::Completed);
            }
            let next = tasks.iter().find(|t| {
                !finished(t)
                    && (t.frontmatter.status == TaskStatus::Blocked
                        || is_eligible(t, &committed))
            });
            let Some(next) = next else {
                let pending = tasks
                    .iter()
                    .filter(|t| !finished(t))
                    .map(|t| t.frontmatter.id.clone())
                    .collect();
                return Ok(SpecOutcome::Stalled { pending });
            };
            let task_id = next.frontmatter.id.clone();
            if next.frontmatter.status == TaskStatus::Blocked
                && !committed.contains(&task_id)
            {
                let open_findings = read_findings(&next.body)?;
                return Ok(SpecOutcome::Blocked {
                    task_id,
                    reason: reason_of(next, self.lang.get()),
                    kind: BlockKind::infer(&open_findings),
                    open_findings,
                });
            }
            match self.run_task(spec_id, &task_id).await? {
                Outcome::Done => {}
                Outcome::Blocked {
                    reason,
                    kind,
                    open_findings,
                } => {
                    return Ok(SpecOutcome::Blocked {
                        task_id,
                        reason,
                        kind,
                        open_findings,
                    })
                }
            }
        }
    }

    /// Desenvolvimento e testes até `IN_REVIEW`, ou `BLOCKED` após `max_test_cycles` falhas de teste.
    async fn develop(&self, settings: &Settings, task: &mut Task) -> Result<Step, RunnerError> {
        if task.frontmatter.status != TaskStatus::InProgress {
            self.transition(task, TaskStatus::InProgress, "aguardar developer")?;
        }
        task.frontmatter.tested_sha = None;
        task.frontmatter.tested_signature = None;
        task.frontmatter.tested_at = None;
        let wt = self.ensure_worktree(settings, task)?;
        let attempt = task.frontmatter.attempts + 1;
        let mut test_output: Option<String> = None;
        // Spec 081 T01: progresso da sessão anterior que esgotou o limite de passos.
        let mut previous_progress = String::new();
        let mut failed_runs: u32 = 0;
        let max_cycles = settings.max_test_cycles;
        // Spec 092 T03: a falha de geração vale para a tentativa inteira (todas as sessões).
        self.developer.begin_attempt(&task.frontmatter.id);
        loop {
            self.check_cancel()?;
            if let Some(step) = self.budget_stop(task)? {
                return Ok(step);
            }
            let mut brief = self.brief(task, &wt, test_output.clone(), settings)?;
            brief.previous_progress = std::mem::take(&mut previous_progress);
            let report = self.developer.implement(&brief).await?;
            let exhausted = report.exhausted;
            self.sync_advice(task);
            let dev_criteria = extract_dev_criteria(&report.summary);
            let history_summary = report.summary.clone();
            self.events.emit(RunnerEvent::AgentMessage {
                agent: "developer".into(),
                text: report.summary,
                task_id: Some(task.frontmatter.id.clone()),
                attempt: Some(attempt),
            });
            commit_pending(
                &wt,
                &self.repo_root,
                &task.frontmatter.spec_id,
                &task.frontmatter.id,
                &task.frontmatter.branch,
                self.lang.get(),
            )?;
            // Formatação automática (spec 078): vira commit próprio, antes do HEAD testado e revisado.
            self.format_delivery(settings, task, &wt, attempt).await?;
            self.record_attempt(settings, task, &history_summary)?;
            if exhausted {
                previous_progress = self.progress_note(settings, task, &history_summary);
            }

            // Início da execução: artefatos de falha (spec 079) e capturas de tela (spec 082 T03) mais
            // antigos que isto são ignorados.
            let run_started = std::time::SystemTime::now();
            let mut test = match self.run_tracked(settings, &wt, &settings.test_command, true, None, "", &task.frontmatter.id, attempt).await {
                Ok(t) => t,
                Err(RunnerError::Docker(reason)) => {
                    return self.block(task, self.block_reason("docker", &[("reason", &reason)]), BlockKind::Environmental);
                }
                Err(e) => return Err(e),
            };
            let mut checks_text = String::new();
            if test.passed && !settings.checks.is_empty() {
                let mut outcomes = Vec::with_capacity(settings.checks.len());
                for check in &settings.checks {
                    self.check_cancel()?;
                    let r = match self.run_tracked(settings, &wt, &check.command, false, Some(&check.name), "", &task.frontmatter.id, attempt).await {
                        Ok(r) => r,
                        Err(RunnerError::Docker(reason)) => {
                            return self.block(task, self.block_reason("docker", &[("reason", &reason)]), BlockKind::Environmental);
                        }
                        Err(e) => return Err(e),
                    };
                    outcomes.push(CheckOutcome {
                        name: check.name.clone(),
                        command: self.scrub(&check.command),
                        blocking: check.blocking,
                        passed: r.passed,
                        output: r.output,
                        exit_code: r.exit_code,
                        timed_out: r.timed_out,
                    });
                }
                let blocking_failed: Vec<&CheckOutcome> =
                    outcomes.iter().filter(|o| o.blocking && !o.passed).collect();
                let regressions = if blocking_failed.is_empty() {
                    self.check_regressions(settings, &outcomes, &task.frontmatter.id, attempt).await?
                } else {
                    Vec::new()
                };
                let summary = checks_summary_in(self.lang.get(), &outcomes, &regressions);
                // Só afirma "testes ✓" se nenhuma checagem bloqueante falhou (senão contradiz a falha).
                let text = if blocking_failed.is_empty() {
                    pass_summary_in(self.lang.get(), false, &self.scrub(&settings.test_command), Some(&summary))
                } else {
                    summary
                };
                self.events.emit(RunnerEvent::AgentMessage {
                    agent: "tests".into(),
                    text,
                    task_id: Some(task.frontmatter.id.clone()),
                    attempt: Some(attempt),
                });
                if !blocking_failed.is_empty() {
                    // Mesma rota de teste falhando: a saída volta ao Desenvolvedor e conta no limite.
                    let output = blocking_failed
                        .iter()
                        .map(|o| format!("checagem {} falhou:\n{}", o.name, tail_bytes(o.output.trim_end(), CHECK_TAIL_BYTES)))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    // exit_code/timed_out do primeiro bloqueante que falhou: 126/127 e timeout seguem
                    // reconhecidos como ambiente (spec 018) em vez de qualidade.
                    let (exit_code, timed_out) = (blocking_failed[0].exit_code, blocking_failed[0].timed_out);
                    test = TestResult { passed: false, output, exit_code, timed_out };
                } else {
                    checks_text = format!("{CHECKS_MARK}{}", checks_section(&outcomes));
                    if !regressions.is_empty() {
                        checks_text.push_str(REGRESSIONS_MARK);
                        checks_text.push_str(&regressions.join(", "));
                    }
                }
            } else if test.passed {
                self.events.emit(RunnerEvent::AgentMessage {
                    agent: "tests".into(),
                    text: pass_summary_in(self.lang.get(), false, &self.scrub(&settings.test_command), None),
                    task_id: Some(task.frontmatter.id.clone()),
                    attempt: Some(attempt),
                });
            }
            if test.passed {
                let crit = if dev_criteria.is_empty() { String::new() } else { format!("{DEV_CRITERIA_MARK}{dev_criteria}") };
                let silent = if test.exit_code.unwrap_or(0) == 0 && is_silent_output(&test.output) {
                    format!("\n{SILENT_PASS_WARNING}\n")
                } else {
                    String::new()
                };
                task.frontmatter.test_evidence = Some(format!("{}{silent}{checks_text}{crit}", test.output));
                // Vincula a evidência ao sha e ao que foi rodado: só então a base pode reaproveitá-la.
                task.frontmatter.tested_sha = Git::new(wt.clone()).rev_parse("HEAD").ok();
                task.frontmatter.tested_signature = Some(test_signature(settings));
                // Spec 082 T03: o início desta execução delimita as capturas de tela da revisão.
                task.frontmatter.tested_at = Some(rfc3339_of(run_started));
                self.transition(task, TaskStatus::InReview, "aguardar revisor")?;
                return Ok(Step::Next);
            }
            // Artefatos de falha (spec 079): seguem com a saída para o Desenvolvedor, a recuperação
            // e `test_evidence`.
            test.output.push_str(&crate::sdd::failure_artifacts::collect(
                &wt,
                &settings.failure_artifacts,
                run_started,
                &|t| self.scrub(t),
            ));
            failed_runs += 1;
            task.frontmatter.tested_sha = None;
            task.frontmatter.tested_signature = None;
            task.frontmatter.tested_at = None;
            self.events.emit(RunnerEvent::AgentMessage {
                agent: "tests".into(),
                text: i18n::tr(
                    self.lang.get(),
                    "runner.tests.failed_cycle",
                    &[("count", &failed_runs.to_string()), ("max", &max_cycles.to_string())],
                ),
                task_id: Some(task.frontmatter.id.clone()),
                attempt: Some(attempt),
            });
            if failed_runs >= max_cycles {
                // A última falha fica registrada: é o fato que o Arquiteto analisa na recuperação.
                task.frontmatter.test_evidence = Some(format!("{}{}", failure_header(test.exit_code, test.timed_out), test.output));
                return self.block(
                    task,
                    self.block_reason_counted("tests_failed", u64::from(failed_runs)),
                    BlockKind::Quality,
                );
            }
            test_output = Some(test.output);
        }
    }

    /// Revisão: `APPROVED` grava `reviewed_sha`; `CHANGES_REQUESTED` conta a tentativa.
    async fn review(&self, settings: &Settings, task: &mut Task) -> Result<Step, RunnerError> {
        let root = self.git();
        let branch = task.frontmatter.branch.clone();
        if branch.is_empty() {
            return Err(RunnerError::Invalid(format!(
                "tarefa {} sem branch registrada",
                task.frontmatter.id
            )));
        }
        let budget = settings
            .review_diff_budget
            .unwrap_or(crate::sdd::DEFAULT_REVIEW_DIFF_BUDGET);
        let diff = review_diff(&root, &settings.base_branch, &branch, budget)?;
        let evidence = task.frontmatter.test_evidence.clone().unwrap_or_default();
        let (evidence, dev_criteria) = match evidence.split_once(DEV_CRITERIA_MARK) {
            Some((e, c)) => (e.to_string(), c.to_string()),
            None => (evidence, String::new()),
        };
        let (test_output, checks) = match evidence.split_once(CHECKS_MARK) {
            Some((t, c)) => (t.to_string(), c.to_string()),
            None => (evidence, String::new()),
        };
        // Spec 082 T01: o Revisor vê qual comando (redigido) gerou a saída.
        let test_output = format!("$ {}\n{test_output}", self.scrub(&settings.test_command));
        let (checks, regressions) = match checks.rsplit_once(REGRESSIONS_MARK) {
            Some((c, r)) => (c.to_string(), r.trim().to_string()),
            None => (checks, String::new()),
        };
        let other_tasks = self
            .load_tasks(&task.frontmatter.spec_id)
            .unwrap_or_default()
            .iter()
            .filter(|t| t.frontmatter.id != task.frontmatter.id)
            .map(|t| format!("{}: {}", t.frontmatter.id, t.frontmatter.title))
            .collect::<Vec<_>>()
            .join("\n");
        let previous = read_findings(&task.body)?;
        let prior_findings = previous
            .iter()
            .map(|f| {
                format!(
                    "- {} [{}] {}: {}. Recomendação: {}",
                    f.id, f.severity, f.location, f.description, f.recommendation
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let incremental_diff = match task.frontmatter.rejected_sha.as_deref() {
            Some(sha) => review_diff(&root, sha, &branch, budget).unwrap_or_default(),
            None => String::new(),
        };
        // Spec 092 T04: arquivos alterados desde o commit revisado na rodada anterior (só na re-revisão;
        // falha do git = sem lista, e nada é rebaixado por arquivo inalterado).
        let changed_files = task
            .frontmatter
            .rejected_sha
            .as_deref()
            .and_then(|sha| root.changed_paths(sha, &branch).ok());
        let worktree = self.worktree_path(&task.frontmatter.spec_id, &task.frontmatter.id);
        // Spec 082 T03: capturas gravadas a partir do início da execução de testes da evidência
        // (`tested_at`); sem esse instante, nenhuma.
        let screenshots = match task.frontmatter.tested_at.as_deref().and_then(crate::sdd::screenshots::parse_rfc3339) {
            Some(since) => crate::sdd::screenshots::list(&worktree, &settings.screenshots, since),
            None => Vec::new(),
        };
        // Spec 086 T01: cópias de outras tarefas da spec que as capturas atuais não geram mais.
        let vanished_captures = if settings.screenshots.is_empty() {
            String::new()
        } else {
            let current: Vec<String> = screenshots.iter().map(|c| c.name.clone()).collect();
            let spec_dir = self.spec_dir(&task.frontmatter.spec_id);
            let list = crate::sdd::screenshots::vanished(&spec_dir, &task.frontmatter.id, &current);
            crate::sdd::screenshots::render_vanished(&list)
        };
        let input = ReviewInput {
            prior_findings: self.scrub(&prior_findings),
            spec_context: self.spec_context(&task.frontmatter.spec_id),
            role_guidance: self.role_guidance(&task.frontmatter.spec_id, crate::sdd::role_guidance::ROLE_REVIEWER),
            skill_profile: self.skill_profile("reviewer"),
            formatters: self.scrub(&formatting::briefing_list(&settings.formatters)),
            incremental_diff: self.scrub(&incremental_diff),
            changed_files: changed_files.as_ref().map(|list| list.iter().map(|p| self.scrub(p)).collect()),
            spec_id: task.frontmatter.spec_id.clone(),
            task_id: task.frontmatter.id.clone(),
            acceptance: mark_manual_criteria(&acceptance_of(&task.body)),
            diff,
            base: settings.base_branch.clone(),
            branch: branch.clone(),
            test_output,
            checks,
            dev_criteria,
            other_tasks,
            worktree,
            ask_epoch: ask_epoch(task),
            regressions,
            screenshots,
            vanished_captures,
            references: self.references(&task.frontmatter.spec_id),
            images: crate::sdd::images::SpecImages::load(&self.spec_dir(&task.frontmatter.spec_id)),
            planned_images: self.planned_images(task),
        };
        if let Some(step) = self.budget_stop(task)? {
            return Ok(step);
        }
        let reviewed = self.reviewer.review(&input).await?;
        self.sync_advice(task);
        // Spec 087 T05: tamanho do diff de código desta tentativa (informativo; falha do git não registra).
        if let Ok(lines) = code_diff_lines(&root, &settings.base_branch, &branch) {
            push_diff_lines(&mut task.frontmatter.diff_lines, lines);
        }
        let attempt = task.frontmatter.attempts + 1;
        let captures = self.copy_captures(&input, &task.frontmatter.id, attempt, &reviewed.screenshots_viewed);
        // Convergência (spec 078): só na re-revisão há findings anteriores ou diff desde a reprovação.
        // Sem base do diff (`rejected_sha` ausente, p.ex. após recuperação, revisão ou retomada humana, que
        // limpam o sha sem apagar os findings) ou com falha ao ler os hunks, nada é rebaixado, como na 1ª revisão.
        let hunks = task.frontmatter.rejected_sha.as_deref().and_then(|sha| changed_hunks(&root, sha, &branch).ok());
        let enforced = enforce_criteria(self.lang.get(), reviewed);
        // Spec 092 T04: na re-revisão, finding novo MEDIUM/LOW em arquivo que não mudou (e que existe na
        // worktree da branch revisada) vira observação.
        let exists = |path: &str| file_in_worktree(&input.worktree, path);
        let Converged { result, observations } = match &hunks {
            Some(h) => converge_with_files(self.lang.get(), enforced, &previous, h, changed_files.as_deref(), &exists),
            None => converge_with_files(self.lang.get(), enforced, &previous, &[], changed_files.as_deref(), &exists),
        };
        let text = render_review_message(
            result.verdict,
            &result.summary,
            &result.findings,
            &observations,
            &captures,
            &result.criteria,
            &|t| self.scrub(t),
        );
        self.events.emit(RunnerEvent::AgentMessage {
            agent: "reviewer".into(),
            text,
            task_id: Some(task.frontmatter.id.clone()),
            attempt: Some(attempt),
        });
        if !observations.is_empty() {
            let existing = section_text(&task.body, OBSERVATIONS).unwrap_or_default();
            task.body = replace_section(&task.body, OBSERVATIONS, append_observations(&existing, &observations, &|t| self.scrub(t)));
        }
        match result.verdict {
            Verdict::Approved => {
                task.frontmatter.last_verdict = Verdict::Approved;
                task.frontmatter.reviewed_sha = Some(root.rev_parse(&branch)?);
                task.frontmatter.rejected_sha = None;
                task.body = replace_section(&task.body, FINDINGS, None);
                self.transition(task, TaskStatus::Approved, "integrar")?;
                Ok(Step::Next)
            }
            Verdict::ChangesRequested => {
                task.frontmatter.last_verdict = Verdict::ChangesRequested;
                task.frontmatter.attempts += 1;
                task.body = replace_section(&task.body, FINDINGS, render_findings(&result.findings)?);
                let log = append_findings_log(
                    &section_text(&task.body, FINDINGS_LOG).unwrap_or_default(),
                    task.frontmatter.attempts,
                    &findings_summary(&result.findings, &|t| self.scrub(t)),
                );
                task.body = replace_section(&task.body, FINDINGS_LOG, log);
                let head = root.rev_parse(&branch)?;
                let stalled = task.frontmatter.rejected_sha.as_deref() == Some(head.as_str());
                task.frontmatter.rejected_sha = Some(head);
                if stalled {
                    return self.block(
                        task,
                        self.block_reason_counted("stalled", u64::from(task.frontmatter.attempts)),
                        BlockKind::Quality,
                    );
                }
                if task.frontmatter.attempts >= task.frontmatter.max_attempts {
                    return self.block(
                        task,
                        self.block_reason_counted("rejections", u64::from(task.frontmatter.attempts)),
                        BlockKind::Quality,
                    );
                }
                self.transition(task, TaskStatus::ChangesRequested, "corrigir findings")?;
                Ok(Step::Next)
            }
            Verdict::None => Err(RunnerError::Invalid(
                "revisor devolveu veredito NONE".into(),
            )),
        }
    }

    /// Integração: squash merge na base, teste na base, commit e finalização.
    async fn integrate(&self, settings: &Settings, task: &mut Task) -> Result<Step, RunnerError> {
        let spec = task.frontmatter.spec_id.clone();
        let id = task.frontmatter.id.clone();
        let title = task.frontmatter.title.clone();
        let base = settings.base_branch.as_str();
        let root = self.git();

        if root.current_branch()? != base {
            return self.block(
                task,
                self.block_reason("not_on_base", &[("base", base)]),
                BlockKind::Environmental,
            );
        }
        let branch = task.frontmatter.branch.clone();
        if branch.is_empty() {
            return Err(RunnerError::Invalid(format!(
                "tarefa {id} sem branch registrada"
            )));
        }
        // Índice não vazio (ex.: squash interrompido ou alteração do humano): não toca em nada.
        if root.has_staged_changes()? {
            return self.block(
                task,
                self.block_reason("staged_index", &[]),
                BlockKind::Environmental,
            );
        }
        if let Some(reason) = self.dirty_reason()? {
            return self.block(task, reason, BlockKind::Environmental);
        }
        let wt = self.worktree_path(&spec, &id);
        if wt.is_dir() && !Git::new(wt.clone()).is_clean_ignoring_sdd()? {
            return self.block(
                task,
                self.block_reason("worktree_dirty", &[]),
                BlockKind::Environmental,
            );
        }
        let head = root.rev_parse(&branch)?;
        if task.frontmatter.last_verdict != Verdict::Approved
            || task.frontmatter.reviewed_sha.as_deref() != Some(head.as_str())
        {
            self.transition(
                task,
                TaskStatus::InReview,
                "revisão obrigatória: veredito ou reviewed_sha diverge do HEAD da branch",
            )?;
            return Ok(Step::Next);
        }
        // A configuração local não entra na base por squash: branch que a altera é recusada antes de
        // qualquer efeito (sem squash, sem INTEGRATING). O humano decide e reexecuta.
        if root.changed_paths(base, &branch)?.iter().any(|p| p == LOCAL_SETTINGS) {
            return self.block(
                task,
                self.block_reason("local_settings", &[("path", LOCAL_SETTINGS)]),
                BlockKind::Environmental,
            );
        }

        if task.frontmatter.status != TaskStatus::Integrating {
            self.transition(task, TaskStatus::Integrating, "squash merge em andamento")?;
        }
        // Estado do índice logo após o squash: a única lista que o rollback desta execução usa.
        let squashed = root.squash_merge(&branch).map_err(|e| e.to_string());
        let captured = root.staged_status().map_err(|e| e.to_string());
        if let Err(e) = squashed {
            return self.rollback_then_block(
                &root,
                task,
                self.block_reason("squash_failed", &[("error", &e)]),
                BlockKind::Environmental,
                &captured,
            );
        }
        // Árvore staged (fora de .sdd/) idêntica à revisada, com evidência de aprovação da branch
        // (test_command e checagens bloqueantes passaram antes da revisão): não repete na base.
        // Erro do git na comparação => falha segura: roda normalmente.
        let reused_sha = self.reusable_sha(settings, task, &root);
        let test_run = if let Some(sha) = &reused_sha {
            let short = sha.get(..7).unwrap_or(sha);
            self.events.emit(RunnerEvent::AgentMessage {
                agent: "tests".into(),
                text: i18n::tr(self.lang.get(), "runner.tests.reused", &[("sha", short)]),
                task_id: Some(id.clone()),
                attempt: Some(task.frontmatter.attempts + 1),
            });
            TestRun {
                command: self.scrub(&settings.test_command),
                exit_code: Some(0),
                at: crate::server::projects::now_rfc3339(),
                summary: i18n::tr(self.lang.get(), "runner.tests.reused_summary", &[("sha", short)]),
            }
        } else {
            match self.run_tracked(settings, root.root(), &settings.test_command, true, None, " na base", &task.frontmatter.id, task.frontmatter.attempts + 1).await {
                Ok(t) if t.passed => TestRun {
                    command: self.scrub(&settings.test_command),
                    exit_code: t.exit_code,
                    at: crate::server::projects::now_rfc3339(),
                    summary: test_summary(&self.scrub(&t.output)),
                },
                Ok(t) => {
                    self.events.emit(RunnerEvent::AgentMessage {
                        agent: "tests".into(),
                        text: t.output,
                        task_id: Some(id.clone()),
                        attempt: Some(task.frontmatter.attempts + 1),
                    });
                    return self.rollback_then_block(
                        &root,
                        task,
                        self.block_reason("base_tests_failed", &[]),
                        BlockKind::Quality,
                        &captured,
                    );
                }
                Err(e) => {
                    return self.rollback_then_block(
                        &root,
                        task,
                        self.block_reason("base_tests_not_run", &[("error", &e.to_string())]),
                        BlockKind::Environmental,
                        &captured,
                    );
                }
            }
        };

        // Na base rodam só as checagens bloqueantes (as não bloqueantes já foram insumo do Revisor).
        let base_checks = if reused_sha.is_some() { &[][..] } else { &settings.checks[..] };
        for check in base_checks.iter().filter(|c| c.blocking) {
            match self.run_tracked(settings, root.root(), &check.command, false, Some(&check.name), " na base", &task.frontmatter.id, task.frontmatter.attempts + 1).await {
                Ok(t) if t.passed => {}
                Ok(t) => {
                    self.events.emit(RunnerEvent::AgentMessage {
                        agent: "tests".into(),
                        text: format!(
                            "{}\n{}",
                            i18n::tr(self.lang.get(), "runner.checks.failed_on_base", &[("name", &check.name)]),
                            tail_bytes(t.output.trim_end(), CHECK_TAIL_BYTES)
                        ),
                        task_id: Some(id.clone()),
                        attempt: Some(task.frontmatter.attempts + 1),
                    });
                    return self.rollback_then_block(
                        &root,
                        task,
                        self.block_reason("base_check_failed", &[("name", &check.name)]),
                        BlockKind::Quality,
                        &captured,
                    );
                }
                Err(e) => {
                    return self.rollback_then_block(
                        &root,
                        task,
                        self.block_reason("base_check_not_run", &[("name", &check.name), ("error", &e.to_string())]),
                        BlockKind::Environmental,
                        &captured,
                    );
                }
            }
        }

        if reused_sha.is_none() {
            let blocking: Vec<String> =
                settings.checks.iter().filter(|c| c.blocking).map(|c| format!("{} ✓", c.name)).collect();
            let lang = self.lang.get();
            let checks = (!blocking.is_empty()).then(|| i18n::tr(lang, "runner.checks.list", &[("list", &blocking.join(", "))]));
            self.events.emit(RunnerEvent::AgentMessage {
                agent: "tests".into(),
                text: pass_summary_in(lang, true, &self.scrub(&settings.test_command), checks.as_deref()),
                task_id: Some(id.clone()),
                attempt: Some(task.frontmatter.attempts + 1),
            });
        }

        // Só o squash entra no commit. O DONE é gravado depois, como cache do estado do Git.
        // Squash vazio (tarefa já satisfeita pela base): o commit de integração sai `--allow-empty`.
        let msg = format!("feat({spec}): {id} - {title}");
        let empty = match root.has_staged_changes() {
            Ok(staged) => !staged,
            Err(e) => {
                return self.rollback_then_block(
                    &root,
                    task,
                    self.block_reason("commit_failed", &[("error", &e.to_string())]),
                    BlockKind::Environmental,
                    &captured,
                );
            }
        };
        let committed = if empty {
            root.commit_allow_empty(&msg)
        } else {
            root.commit(&msg)
        };
        if let Err(e) = committed {
            return self.rollback_then_block(
                &root,
                task,
                self.block_reason("commit_failed", &[("error", &e.to_string())]),
                BlockKind::Environmental,
                &captured,
            );
        }
        task.frontmatter.test_run = Some(test_run);
        if empty {
            return self.finalize_with(&root, task, "sem alterações: já presente na base");
        }
        self.finalize_resumed(&root, task)
    }

    /// Sha revisado cuja evidência pode ser reaproveitada na base: a árvore staged (sem `.sdd/`) é
    /// idêntica à de `reviewed_sha` e a branch registrou aprovação do `test_command` e das
    /// checagens bloqueantes. `None` (roda normalmente) em qualquer dúvida, inclusive erro do git.
    fn reusable_sha(&self, settings: &Settings, task: &Task, root: &Git) -> Option<String> {
        let sha = task.frontmatter.reviewed_sha.as_deref()?;
        if task.frontmatter.tested_sha.as_deref() != Some(sha)
            || task.frontmatter.tested_signature.as_deref() != Some(test_signature(settings).as_str())
        {
            return None;
        }
        let evidence = task.frontmatter.test_evidence.as_deref()?;
        if evidence.trim().is_empty() || evidence.starts_with(TEST_FAILURE_MARK) {
            return None;
        }
        if settings.checks.iter().any(|c| c.blocking) && !evidence.contains(CHECKS_MARK) {
            return None;
        }
        match root.staged_tree_equals(sha, ".sdd") {
            Ok(true) => Some(sha.to_string()),
            _ => None,
        }
    }

    /// Desfaz o squash desta execução (usando `captured`) e marca `BLOCKED`. Falha do rollback não
    /// escapa: vira motivo explícito sobre o estado do índice.
    fn rollback_then_block(
        &self,
        root: &Git,
        task: &mut Task,
        cause: BlockReason,
        kind: BlockKind,
        captured: &Result<Vec<(String, String)>, String>,
    ) -> Result<Step, RunnerError> {
        let undone = match captured {
            Ok(list) => root.squash_rollback(list).map_err(|e| e.to_string()),
            Err(e) => Err(format!("índice do squash não lido: {e}")),
        };
        match undone {
            Ok(()) => self.block(task, cause, kind),
            Err(e) => {
                let reason = self.block_reason("rollback_failed", &[("cause", &cause.text), ("error", &e)]);
                self.block(task, reason, BlockKind::Environmental)
            }
        }
    }

    /// Retomada com a integração já na base: remove worktree/branch e marca `DONE`.
    /// Worktree com alterações não commitadas bloqueia sem remover nada.
    fn finalize_resumed(&self, root: &Git, task: &mut Task) -> Result<Step, RunnerError> {
        self.finalize_with(root, task, "concluída")
    }

    /// Como [`Self::finalize_resumed`], com a nota gravada em `next_action` ao concluir.
    fn finalize_with(&self, root: &Git, task: &mut Task, note: &str) -> Result<Step, RunnerError> {
        let wt = self.worktree_path(&task.frontmatter.spec_id, &task.frontmatter.id);
        // Posse devolvida antes de qualquer leitura/remoção (arquivos criados como root no Docker).
        let mut warnings: Vec<TaskWarning> = Vec::new();
        for dir in [wt.as_path(), root.root()] {
            if dir.is_dir() {
                if let Err(e) = self.repair_dir(dir) {
                    warnings.push(TaskWarning::Ownership { path: dir.display().to_string(), error: e.to_string() });
                }
            }
        }
        // Só worktree registrada conta como suja (pasta órfã sobe para o repositório pai e
        // enxergaria `?? .sdd/`); o estado do SDD nunca é sujeira.
        if wt.is_dir()
            && root.is_registered_worktree(&wt)?
            && !Git::new(wt.clone()).is_clean_ignoring_sdd()?
        {
            // Commit presente: não grava BLOCKED; o status em disco fica como está e a retomada
            // finaliza quando a worktree for limpa.
            let id = task.frontmatter.id.clone();
            let path = wt.display().to_string();
            self.events.emit(RunnerEvent::SystemNotice {
                text: system_text(self.lang.get(), "task.worktree_dirty", &[("task", &id), ("path", &path)]),
                code: "task.worktree_dirty".into(),
                params: notice_params(&[("task", &id), ("path", &path)]),
                task_id: Some(id),
            });
            return Ok(Step::Finish(Outcome::Blocked {
                reason: self.block_reason("finalize_dirty", &[]).text,
                kind: BlockKind::Environmental,
                open_findings: read_findings(&task.body)?,
            }));
        }
        // O commit de integração já está na base: falha de limpeza não desfaz a conclusão.
        if let Err(first) = self.cleanup(root, task) {
            if wt.is_dir() {
                let _ = self.repair_dir(&wt);
            }
            if let Err(second) = self.cleanup(root, task) {
                warnings.push(TaskWarning::Cleanup { path: wt.display().to_string(), error: format!("{first}; {second}") });
            }
        }
        // Um aviso por problema, cada um com o seu `code` (spec 102).
        for warning in warnings {
            let (text, code, params) = warning.notice(self.lang.get());
            self.events.emit(RunnerEvent::SystemNotice {
                text,
                code: code.into(),
                params,
                task_id: Some(task.frontmatter.id.clone()),
            });
        }
        self.transition(task, TaskStatus::Done, note)?;
        self.events.emit(RunnerEvent::Integrated {
            spec_id: task.frontmatter.spec_id.clone(),
            task_id: task.frontmatter.id.clone(),
        });
        Ok(Step::Finish(Outcome::Done))
    }

    /// Devolve ao uid do host arquivos de outro dono em `dir` (só no modo docker, sem sudo).
    fn repair_dir(&self, dir: &Path) -> Result<usize, String> {
        match &self.compose {
            Some(c) => c.repair_ownership(dir),
            None => Ok(0),
        }
    }

    /// Apaga pasta órfã de worktree. Só dentro de `<root>/.sdd/worktrees/` (canonicalizado) e
    /// nunca segue symlink.
    fn remove_orphan_dir(&self, dir: &Path) -> Result<(), RunnerError> {
        let invalid = RunnerError::Invalid;
        let meta = fs::symlink_metadata(dir).map_err(|e| invalid(format!("{}: {e}", dir.display())))?;
        if meta.file_type().is_symlink() {
            return Err(invalid(format!("{} é um symlink; não removido", dir.display())));
        }
        let allowed = fs::canonicalize(sdd_dir(&self.repo_root).join(WORKTREES_DIR))
            .map_err(|e| invalid(format!("pasta de worktrees: {e}")))?;
        let real = fs::canonicalize(dir).map_err(|e| invalid(format!("{}: {e}", dir.display())))?;
        if real == allowed || !real.starts_with(&allowed) {
            return Err(invalid(format!("{} fora de .sdd/worktrees; não removido", dir.display())));
        }
        fs::remove_dir_all(&real).map_err(|e| invalid(format!("{}: {e}", real.display())))
    }

    /// Remove a worktree (se existir) e a branch da tarefa. Só chamada após o commit na base.
    fn cleanup(&self, root: &Git, task: &Task) -> Result<(), RunnerError> {
        let wt = self.worktree_path(&task.frontmatter.spec_id, &task.frontmatter.id);
        if wt.is_dir() {
            if root.is_registered_worktree(&wt)? {
                root.worktree_remove(&wt)?;
            } else {
                self.remove_orphan_dir(&wt)?;
            }
        }
        let branch = task.frontmatter.branch.clone();
        if !branch.is_empty() && root.branches()?.iter().any(|b| *b == branch) {
            root.branch_delete(&branch)?;
        }
        // Pasta da spec vazia: remove (remove_dir só remove vazia e não segue symlink).
        let spec_dir = sdd_dir(&self.repo_root).join(WORKTREES_DIR).join(&task.frontmatter.spec_id);
        if fs::symlink_metadata(&spec_dir).is_ok_and(|m| m.is_dir()) {
            let _ = fs::remove_dir(&spec_dir);
        }
        Ok(())
    }

    /// Motivo de bloqueio por alterações não commitadas na base, ou `None` se a base está limpa.
    ///
    /// Alterações não staged em `.sdd/specs/**` (estado de trabalho: `spec.md`, `tasks/*.md`) e em
    /// `.sdd/settings.yaml` (configuração local, versionada ou não) não bloqueiam. Qualquer outro
    /// caminho não commitado (arquivos de `.sdd` fora dessas duas entradas, arquivos fora de `.sdd`)
    /// bloqueia. Alterações staged são tratadas antes, em `integrate`.
    fn dirty_reason(&self) -> Result<Option<BlockReason>, RunnerError> {
        for path in self.git().porcelain_paths(&[WORKTREES_EXCLUDE])? {
            // `.sdd/skills/` (perfis de especialista, spec 063) é do humano/Arquiteto, como `.sdd/specs/`.
            if path.starts_with(".sdd/specs/") || path.starts_with(".sdd/skills/") || path == LOCAL_SETTINGS {
                continue;
            }
            if path.starts_with(".sdd/") {
                return Ok(Some(self.block_reason("sdd_dirty", &[("path", &path)])));
            }
            return Ok(Some(self.block_reason("base_dirty", &[("path", &path)])));
        }
        Ok(None)
    }

    /// Motivo de bloqueio `code` no idioma do projeto (spec 114 T02).
    fn block_reason(&self, code: &'static str, params: &[(&str, &str)]) -> BlockReason {
        BlockReason::new(self.lang.get(), code, params)
    }

    /// Motivo de bloqueio `code` com contagem, no plural do idioma (spec 115 T01).
    fn block_reason_counted(&self, code: &'static str, count: u64) -> BlockReason {
        BlockReason::counted(self.lang.get(), code, count)
    }

    /// Marca `BLOCKED` com o motivo (texto em `next_action`, código em `next_action_code`) e
    /// devolve o encerramento do laço.
    fn block(
        &self,
        task: &mut Task,
        reason: BlockReason,
        kind: BlockKind,
    ) -> Result<Step, RunnerError> {
        task.frontmatter.status = TaskStatus::Blocked;
        task.frontmatter.next_action = Some(reason.text.clone());
        task.frontmatter.next_action_code = Some(reason.code.to_string());
        self.persist(task)?;
        Ok(Step::Finish(Outcome::Blocked {
            reason: reason.text,
            kind,
            open_findings: read_findings(&task.body)?,
        }))
    }

    /// Garante a worktree da tarefa: reaproveita a existente ou cria a partir da base/branch.
    /// Só reutiliza um diretório que o Git registra como worktree da branch da tarefa; diretório
    /// comum no mesmo caminho é erro (nunca recebe commit).
    fn ensure_worktree(&self, settings: &Settings, task: &mut Task) -> Result<PathBuf, RunnerError> {
        let spec = task.frontmatter.spec_id.clone();
        let id = task.frontmatter.id.clone();
        let wt = self.worktree_path(&spec, &id);
        let branch = branch_name(&spec, &id);
        let root = self.git();
        if wt.exists() {
            let registered = root.worktree_list()?.iter().any(|w| {
                w.branch.as_deref() == Some(branch.as_str()) && same_path(&w.path, &wt)
            });
            if !registered {
                return Err(RunnerError::Invalid(format!(
                    "{} existe e não é a worktree da branch {branch}",
                    wt.display()
                )));
            }
        } else if root.branches()?.iter().any(|b| *b == branch) {
            root.worktree_attach(&branch, &wt)?;
        } else {
            root.worktree_add(&branch, &wt, &settings.base_branch)?;
        }
        task.frontmatter.branch = branch;
        Ok(wt)
    }

    /// Verifica que todas as dependências estão integradas na base (commit presente).
    fn check_deps(&self, base: &str, task: &Task) -> Result<(), RunnerError> {
        let spec = &task.frontmatter.spec_id;
        let mut pending = Vec::new();
        for dep in &task.frontmatter.depends_on {
            let ok = validate_id("depends_on", dep).is_ok() && self.is_committed(base, spec, dep)?;
            if !ok {
                pending.push(dep.clone());
            }
        }
        if pending.is_empty() {
            Ok(())
        } else {
            Err(RunnerError::DependenciesPending {
                task: task.frontmatter.id.clone(),
                pending,
            })
        }
    }

    /// Premissas e Abordagem Técnica do spec.md (redigidas); vazio se ausentes ou ilegíveis.
    fn spec_context(&self, spec_id: &str) -> String {
        let text = fs::read_to_string(self.spec_dir(spec_id).join("spec.md")).unwrap_or_default();
        self.scrub(&spec_context_of(&text, SPEC_CONTEXT_MAX_BYTES))
    }

    /// Orientação do Arquiteto para o papel (`Desenvolvedor`/`Revisor`), redigida; vazia se ausente.
    fn role_guidance(&self, spec_id: &str, role: &str) -> String {
        let text = fs::read_to_string(self.spec_dir(spec_id).join("spec.md")).unwrap_or_default();
        // Redige o spec inteiro e só depois trunca: segredo no corte não vaza prefixo.
        crate::sdd::role_guidance::role_guidance_of(&self.scrub(&text), role)
    }

    /// Guia de arte da spec (spec 090 T02), redigido antes do corte; vazio sem a seção.
    fn art_guide(&self, spec_id: &str) -> String {
        let text = fs::read_to_string(self.spec_dir(spec_id).join("spec.md")).unwrap_or_default();
        crate::sdd::images::art_guide(&self.scrub(&text))
    }

    /// Perfil de especialista do papel, lido da RAIZ do projeto (nunca da worktree), redigido e
    /// limitado; vazio sem perfil ou com `skills: false` em settings.
    fn skill_profile(&self, role: &str) -> String {
        // Settings ilegível = ligado (o padrão), igual ao relatório final.
        let on = read_settings(&sdd_dir(&self.repo_root).join("settings.yaml")).map(|s| s.skills).unwrap_or(true);
        if !on {
            return String::new();
        }
        crate::sdd::skills::block(&self.repo_root, role, |t| self.scrub(t)).unwrap_or_default()
    }

    /// Monta o pedido do Desenvolvedor a partir do estado atual da tarefa.
    fn brief(
        &self,
        task: &Task,
        wt: &Path,
        test_output: Option<String>,
        settings: &Settings,
    ) -> Result<DevBrief, RunnerError> {
        let yolo = settings.yolo;
        Ok(DevBrief {
            spec_id: task.frontmatter.spec_id.clone(),
            task_id: task.frontmatter.id.clone(),
            title: task.frontmatter.title.clone(),
            objective: section_text(&task.body, OBJECTIVE)
                .unwrap_or_default()
                .trim()
                .to_string(),
            acceptance: acceptance_of(&task.body),
            allowed_scope: task.frontmatter.allowed_scope.clone(),
            worktree: wt.to_path_buf(),
            branch: task.frontmatter.branch.clone(),
            findings: read_findings(&task.body)?,
            test_output,
            recovery: recovery_notes_for_dev(&task.body, yolo),
            history: section_text(&task.body, HISTORY).unwrap_or_default().trim().to_string(),
            human_guidance: section_text(&task.body, GUIDANCE_SECTION).unwrap_or_default().trim().to_string(),
            spec_context: self.spec_context(&task.frontmatter.spec_id),
            role_guidance: self.role_guidance(&task.frontmatter.spec_id, crate::sdd::role_guidance::ROLE_DEVELOPER),
            skill_profile: self.skill_profile("developer"),
            project_files: self.scrub(&project_files_listing(
                &Git::new(wt.to_path_buf()).ls_files().unwrap_or_default(),
                &task.frontmatter.allowed_scope,
                PROJECT_FILES_MAX_LINES,
                PROJECT_FILES_MAX_BYTES,
            )),
            ask_epoch: ask_epoch(task),
            previous_progress: String::new(),
            formatters: self.scrub(&formatting::briefing_list(&settings.formatters)),
            screenshot_globs: settings.screenshots.clone(),
            references: self.references(&task.frontmatter.spec_id),
            base_branch: settings.base_branch.clone(),
            spec_dir: self.spec_dir(&task.frontmatter.spec_id),
            art_guide: self.art_guide(&task.frontmatter.spec_id),
        })
    }

    /// Copia para `.sdd/specs/<spec>/captures/` as capturas que o Revisor viu (spec 082 T03), só as da
    /// lista do briefing, lidas de `input.worktree`. Devolve os nomes das cópias (melhor esforço).
    fn copy_captures(&self, input: &ReviewInput, task_id: &str, attempt: u32, viewed: &[String]) -> Vec<String> {
        let listed: Vec<String> = viewed
            .iter()
            .filter(|v| input.screenshots.iter().any(|c| c.usable() && &c.name == *v))
            .cloned()
            .collect();
        crate::sdd::screenshots::copy_viewed(&self.spec_dir(&input.spec_id), task_id, attempt, &input.worktree, &listed)
    }

    /// Carrega as tarefas da spec, ordenadas pelo nome do arquivo.
    fn load_tasks(&self, spec_id: &str) -> Result<Vec<Task>, RunnerError> {
        let dir = self.spec_dir(spec_id).join("tasks");
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(RunnerError::Io { path: dir, source }),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|source| RunnerError::Io {
                    path: dir.clone(),
                    source,
                })?
                .path();
            if path.is_file() && path.extension() == Some(OsStr::new("md")) {
                paths.push(path);
            }
        }
        paths.sort();
        paths.iter().map(|p| Ok(read_task(p)?)).collect()
    }

    /// Aviso fixo da verificação final (spec 102): mensagem do sistema com `code` e `params`
    /// (já sem segredos); o texto de reserva é `system.<code>` no idioma do projeto (spec 115 T03).
    /// Listas e erros longos (`tail`) não vão em `params` (cortados em 512 bytes): ficam no `text`,
    /// depois da primeira quebra de linha, e a interface os mostra como vieram, abaixo da frase
    /// fixa traduzida.
    fn final_notice(&self, code: &str, params: &[(&str, &str)], tail: Option<&str>) {
        let scrubbed: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, self.scrub(v))).collect();
        let pairs: Vec<(&str, &str)> = scrubbed.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let fixed = system_text(self.lang.get(), code, &pairs);
        let text = match tail {
            Some(tail) => format!("{fixed}\n{tail}"),
            None => fixed,
        };
        self.events.emit(RunnerEvent::SystemNotice {
            text: self.scrub(&text),
            code: code.into(),
            params: notice_params(&pairs),
            task_id: None,
        });
    }

    fn say(&self, text: String) {
        self.events.emit(RunnerEvent::AgentMessage {
            agent: "reviewer".into(),
            text: self.scrub(&text),
            task_id: None,
            attempt: None,
        });
    }

    /// Verificação final (spec 059/T02): antes de concluir, o Revisor confere os "Critérios de
    /// Aceitação Globais" do spec.md contra a base integrada e os `test_run` das tarefas. Sem
    /// critérios globais (ou spec já concluída) pula. Reprovação cria UMA tarefa de correção
    /// (confirmada pelo humano no modo Padrão); na rodada seguinte, falha vira aviso e conclui.
    async fn final_verification(
        &self,
        settings: &Settings,
        spec_id: &str,
        tasks: &[Task],
    ) -> Result<FinalStep, RunnerError> {
        let spec_path = self.spec_dir(spec_id).join("spec.md");
        let spec_text = fs::read_to_string(&spec_path).unwrap_or_default();
        if read_spec(&spec_path).is_ok_and(|s| s.frontmatter.status == SpecStatus::Completed) {
            return Ok(FinalStep::Complete);
        }
        let criteria = section_text(&spec_text, GLOBAL_CRITERIA)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if criteria.is_empty() {
            return Ok(FinalStep::Complete);
        }
        let total_listed = criteria.lines().filter(|l| !l.trim().is_empty()).count();
        if let Some(reason) = self.budget.as_ref().and_then(|g| g.exceeded(spec_id, self.lang.get())) {
            self.final_notice("final.skipped", &[("reason", &reason.to_string())], None);
            return Ok(FinalStep::Complete);
        }
        // O indicador cobre os testes da base e a conferência do Revisor; o `drop` limpa em qualquer saída.
        let live = self.final_activity.as_ref().map(|s| s.begin());
        // Spec 082 T03: capturas da verificação final são as gravadas a partir do teste na base.
        let base_started = std::time::SystemTime::now();
        let (base_text, checks_text, fail_tail) = self.final_base_evidence(settings).await?;
        let screenshots = crate::sdd::screenshots::list(&self.repo_root, &settings.screenshots, base_started);
        let mut test_output = base_text;
        let lang = self.lang.get();
        for t in tasks {
            let f = &t.frontmatter;
            let line = match &f.test_run {
                Some(r) => i18n::tr(
                    lang,
                    "runner.final_base.task_test_run",
                    &[
                        ("task", &f.id),
                        ("command", &r.command),
                        ("code", &r.exit_code.map_or("?".to_string(), |c| c.to_string())),
                        ("at", &r.at),
                        ("summary", &r.summary),
                    ],
                ),
                None => i18n::tr(lang, "runner.final_base.task_no_test_run", &[("task", &f.id)]),
            };
            test_output.push_str(&line);
            test_output.push('\n');
        }
        let input = ReviewInput {
            spec_id: spec_id.to_string(),
            task_id: "FINAL".into(),
            acceptance: mark_manual_criteria(&criteria),
            diff: "(verificação final dos critérios globais: sem diff; inspecione a árvore integrada da base com as ferramentas de leitura)".into(),
            base: settings.base_branch.clone(),
            branch: settings.base_branch.clone(),
            test_output: self.scrub(truncate_bytes(&test_output, 8 * 1024)),
            checks: self.scrub(&checks_text),
            dev_criteria: String::new(),
            other_tasks: tasks
                .iter()
                .map(|t| format!("{}: {}", t.frontmatter.id, t.frontmatter.title))
                .collect::<Vec<_>>()
                .join("\n"),
            prior_findings: String::new(),
            incremental_diff: String::new(),
            changed_files: None,
            spec_context: self.spec_context(spec_id),
            role_guidance: self.role_guidance(spec_id, crate::sdd::role_guidance::ROLE_REVIEWER),
            skill_profile: self.skill_profile("reviewer"),
            formatters: String::new(),
            worktree: self.repo_root.clone(),
            ask_epoch: 0,
            regressions: String::new(),
            screenshots,
            vanished_captures: String::new(),
            references: self.references(spec_id),
            images: crate::sdd::images::SpecImages::load(&self.spec_dir(spec_id)),
            planned_images: crate::sdd::images::PlannedImages::default(),
        };
        let reviewed = self.reviewer.review(&input).await;
        drop(live);
        let (Converged { result, observations }, captures) = match reviewed {
            Ok(r) => {
                // Spec 084 T01: a 2ª rodada (depois da tarefa de correção) grava `final-2-…`, sem
                // sobrescrever as cópias da 1ª.
                let round = if tasks.iter().any(is_final_fix) { 2 } else { 1 };
                let captures = self.copy_captures(&input, "FINAL", round, &r.screenshots_viewed);
                (converge(self.lang.get(), enforce_criteria(self.lang.get(), r), &[], &[]), captures)
            }
            Err(RunnerError::Cancelled) => return Err(RunnerError::Cancelled),
            Err(e) => {
                // O erro pode passar do teto de um param: vai inteiro no texto, depois da frase fixa.
                self.final_notice("final.unavailable", &[], Some(&e.to_string()));
                return Ok(FinalStep::Complete);
            }
        };
        let passed = result.verdict == Verdict::Approved;
        let total = if result.criteria.is_empty() { total_listed } else { result.criteria.len() };
        let met = if result.criteria.is_empty() {
            if passed { total } else { 0 }
        } else {
            result.criteria.iter().filter(|c| c.met || is_manual_criterion(&c.criterion)).count()
        };
        let matrix = render_review_message(
            result.verdict,
            &result.summary,
            &result.findings,
            &observations,
            &captures,
            &result.criteria,
            &|t| self.scrub(t),
        );
        self.say(format!("Verificação final: {met} de {total} critérios atendidos\n\n{matrix}"));
        if passed {
            return Ok(FinalStep::Complete);
        }
        let unmet: Vec<String> = result
            .criteria
            .iter()
            .filter(|c| !c.met && !is_manual_criterion(&c.criterion))
            .map(|c| format!("- {}: {}", c.criterion, c.evidence.trim()))
            .chain(result.findings.iter().map(|f| format!("- {}: {}", f.location, f.description)))
            .collect();
        let detail = if unmet.is_empty() { result.summary.clone() } else { unmet.join("\n") };
        if tasks.iter().any(is_final_fix) {
            self.final_notice("final.still_unmet", &[], Some(&detail));
            return Ok(FinalStep::Complete);
        }
        if let Some(gate) = &self.final_gate {
            let (met, total) = (met.to_string(), total.to_string());
            let question = i18n::tr(self.lang.get(), "runner.final_fix.reason", &[("met", &met), ("total", &total)]);
            let reason = format!("{question}\n{detail}");
            if !gate.approve_fix(spec_id, &reason).await {
                self.final_notice("final.fix_refused", &[], Some(&detail));
                return Ok(FinalStep::Complete);
            }
        }
        let number = tasks
            .iter()
            .filter_map(|t| t.frontmatter.id.strip_prefix('T')?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let id = format!("T{number:02}");
        let mut scope: Vec<String> = Vec::new();
        for g in tasks.iter().flat_map(|t| t.frontmatter.allowed_scope.iter()) {
            if !scope.contains(g) {
                scope.push(g.clone());
            }
        }
        let lang = self.lang.get();
        let title = final_fix_title(lang);
        // Os cabeçalhos `## Objetivo` e `## Critérios de Aceitação` são lidos pelo parser da tarefa
        // (`section_text`) e ficam fixos; o texto livre sai no idioma do projeto (spec 115 T04).
        let goal = i18n::text(lang, "runner.final_fix.objective");
        let reference = i18n::text(lang, "runner.final_fix.reference_heading");
        let fix = Task {
            frontmatter: crate::sdd::TaskMeta {
                id: id.clone(),
                spec_id: spec_id.to_string(),
                title: title.clone(),
                status: TaskStatus::Todo,
                depends_on: Vec::new(),
                branch: String::new(),
                attempts: 0,
                max_attempts: 3,
                last_verdict: Verdict::None,
                allowed_scope: scope,
                covers: Vec::new(),
                reviewed_sha: None,
                rejected_sha: None,
                tested_sha: None,
                tested_signature: None,
                tested_at: None,
                test_evidence: None,
                test_run: None,
                next_action: Some("aguardando developer".into()),
                next_action_code: None,
                diff_lines: Vec::new(),
                kind: Some(crate::sdd::TASK_KIND_FINAL_FIX.into()),
            },
            body: format!(
                "## {OBJECTIVE}\n{goal}\n\n## {ACCEPTANCE}\n{detail}\n\n## {reference}\n{criteria}\n{fail_tail}"
            ),
        };
        self.write_guarded(&self.task_path(spec_id, &id), &fix.render()?)?;
        self.final_notice("final.fix_created", &[("task", &id), ("title", &title)], None);
        self.events.emit(RunnerEvent::TaskUpdated {
            spec_id: spec_id.to_string(),
            task_id: id,
            status: TaskStatus::Todo,
        });
        Ok(FinalStep::Continue)
    }

    /// Regressões desta tarefa (spec 080 T01): checagens não bloqueantes que falharam na branch e
    /// passam na base. Não há resultado prévio da base para elas (a integração só roda as bloqueantes
    /// e a verificação final só no fim da spec), então a checagem roda na base sob demanda, só quando
    /// falha na branch, e o resultado fica em cache por sha da base: no máximo uma vez por tarefa.
    /// Se não houver resultado da base (sha ilegível ou checagem que não executou), não marca.
    async fn check_regressions(
        &self,
        settings: &Settings,
        outcomes: &[CheckOutcome],
        task_id: &str,
        attempt: u32,
    ) -> Result<Vec<String>, RunnerError> {
        let failing: Vec<&CheckOutcome> = outcomes.iter().filter(|o| !o.blocking && !o.passed).collect();
        if failing.is_empty() {
            return Ok(Vec::new());
        }
        let Ok(base_sha) = self.git().rev_parse(&settings.base_branch) else {
            return Ok(Vec::new());
        };
        let mut regressions = Vec::new();
        for outcome in failing {
            let Some(check) = settings.checks.iter().find(|c| c.name == outcome.name) else {
                continue;
            };
            let key = (base_sha.clone(), check.name.clone(), check.command.clone());
            let cached = self
                .base_checks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&key)
                .copied();
            let on_base = match cached {
                Some(result) => result,
                None => {
                    self.check_cancel()?;
                    let result = match self
                        .run_tracked(settings, &self.repo_root, &check.command, false, Some(&check.name), " na base", task_id, attempt)
                        .await
                    {
                        Ok(r) => Some(r.passed),
                        Err(RunnerError::Cancelled) => return Err(RunnerError::Cancelled),
                        Err(_) => None,
                    };
                    self.base_checks
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(key, result);
                    result
                }
            };
            if on_base == Some(true) {
                regressions.push(check.name.clone());
            }
        }
        Ok(regressions)
    }

    /// Evidência real da verificação final: roda na base o `test_command` e todas as checagens
    /// (bloqueantes ou não). Devolve `(resumo do teste, seção de checagens, cauda do que falhou)`.
    /// Falha de execução vira "não executado: <motivo>"; só `Cancelled` propaga.
    async fn final_base_evidence(&self, settings: &Settings) -> Result<(String, String, String), RunnerError> {
        const WHERE: &str = " na base";
        let root = self.repo_root.as_path();
        let mut fail_tail = String::new();
        let command = self.scrub(&settings.test_command);
        let lang = self.lang.get();
        let mut test_mark = i18n::text(lang, "runner.final_base.not_run");
        let test_text = match self.run_tracked(settings, root, &settings.test_command, true, None, WHERE, "FINAL", 1).await {
            Ok(t) => {
                test_mark = if t.passed { "✓" } else { "✗" }.to_string();
                let key = if t.passed { "runner.final_base.tests_passed" } else { "runner.final_base.tests_failed" };
                let code = t.exit_code.map_or("?".to_string(), |c| c.to_string());
                let tail = self.scrub(tail_bytes(t.output.trim_end(), CHECK_TAIL_BYTES));
                if !t.passed {
                    let heading = i18n::text(lang, "runner.final_base.tests_output_heading");
                    fail_tail.push_str(&format!("\n## {heading}\n```\n{tail}\n```\n"));
                }
                format!("$ {command}\n{}:\n{tail}\n", i18n::tr(lang, key, &[("code", &code)]))
            }
            Err(RunnerError::Cancelled) => return Err(RunnerError::Cancelled),
            Err(e) => format!("$ {command}\n{}\n", i18n::tr(lang, "runner.final_base.tests_not_run", &[("error", &e.to_string())])),
        };
        let mut outcomes = Vec::with_capacity(settings.checks.len());
        let mut not_run = Vec::new();
        let mut not_run_names = Vec::new();
        for check in &settings.checks {
            self.check_cancel()?;
            match self.run_tracked(settings, root, &check.command, false, Some(&check.name), WHERE, "FINAL", 1).await {
                Ok(r) => outcomes.push(CheckOutcome {
                    name: check.name.clone(),
                    command: self.scrub(&check.command),
                    blocking: check.blocking,
                    passed: r.passed,
                    output: r.output,
                    exit_code: r.exit_code,
                    timed_out: r.timed_out,
                }),
                Err(RunnerError::Cancelled) => return Err(RunnerError::Cancelled),
                Err(e) => {
                    not_run_names.push(i18n::tr(lang, "runner.checks.not_run", &[("name", &check.name)]));
                    let line = i18n::tr(lang, "runner.final_base.check_not_run", &[("name", &check.name), ("error", &e.to_string())]);
                    not_run.push(format!("### {line}\n$ {}", self.scrub(&check.command)));
                }
            }
        }
        // Spec 077 T02: o resultado na base vira mensagem `tests` (sem tarefa), no formato das por tarefa;
        // o relatório lê esta linha como fato da verificação final.
        let mut parts: Vec<String> = Vec::new();
        if !outcomes.is_empty() {
            parts.push(checks_list_in(lang, &outcomes, &[]));
        }
        parts.extend(not_run_names);
        let checks_part = if parts.is_empty() {
            String::new()
        } else {
            format!(" · {}", i18n::tr(lang, "runner.checks.list", &[("list", &parts.join(", "))]))
        };
        self.events.emit(RunnerEvent::AgentMessage {
            agent: "tests".into(),
            text: i18n::tr(
                lang,
                "runner.final_base.line",
                &[("prefix", &final_base_prefix(lang)), ("mark", &test_mark), ("checks", &checks_part)],
            ),
            task_id: None,
            attempt: None,
        });
        for o in outcomes.iter().filter(|o| !o.passed) {
            let tail = self.scrub(tail_bytes(o.output.trim_end(), CHECK_TAIL_BYTES));
            let heading = i18n::tr(lang, "runner.final_base.check_output_heading", &[("name", &o.name)]);
            fail_tail.push_str(&format!("\n## {heading}\n```\n{tail}\n```\n"));
        }
        let mut sections = Vec::new();
        if !outcomes.is_empty() {
            sections.push(checks_section(&outcomes));
        }
        sections.extend(not_run);
        Ok((test_text, sections.join("\n\n"), fail_tail))
    }

    /// Marca `spec.md` como `COMPLETED` (idempotente).
    fn complete_spec(&self, spec_id: &str) -> Result<(), RunnerError> {
        let path = self.spec_dir(spec_id).join("spec.md");
        let mut spec = read_spec(&path)?;
        if spec.frontmatter.status != SpecStatus::Completed {
            spec.frontmatter.status = SpecStatus::Completed;
            self.write_guarded(&path, &spec.render()?)?;
        }
        Ok(())
    }

    /// Grava a tarefa e emite `TaskUpdated`.
    fn persist(&self, task: &Task) -> Result<(), RunnerError> {
        let spec_id = task.frontmatter.spec_id.clone();
        let task_id = task.frontmatter.id.clone();
        self.write_guarded(&self.task_path(&spec_id, &task_id), &task.render()?)?;
        self.events.emit(RunnerEvent::TaskUpdated {
            spec_id,
            task_id,
            status: task.frontmatter.status,
        });
        Ok(())
    }

    /// Registra no corpo da tarefa a análise de recuperação do Arquiteto (seção `## Análise de
    /// recuperação #n`, `n` = seções existentes + 1) pelo mesmo caminho de escrita do runner
    /// (guardado, com `TaskUpdated`). Com `retry`, zera `attempts` e volta a tarefa a `TODO`; sem
    /// ele, a tarefa continua como está (`BLOCKED`). Devolve `n`.
    pub fn record_recovery(
        &self,
        spec_id: &str,
        task_id: &str,
        retry: bool,
        text: &str,
    ) -> Result<usize, RunnerError> {
        validate_id("spec_id", spec_id)?;
        validate_id("task_id", task_id)?;
        let mut task = read_task(&self.task_path(spec_id, task_id))?;
        if task.frontmatter.status != TaskStatus::Blocked {
            return Err(RunnerError::Invalid(format!("tarefa {task_id} não está bloqueada")));
        }
        let n = recovery_total(&task.body) + 1;
        let mut recorded = text.to_string();
        if retry {
            // `scope_add` da estratégia (spec 079): vale na mesma decisão que reabre a tarefa, e o
            // registro mostra só o que entrou (spec 094 T07).
            let ext = apply_scope_added(&mut task.frontmatter.allowed_scope, text);
            recorded = record_scope_extension(text, &ext);
        }
        task.body = replace_section(&task.body, &format!("{RECOVERY_PREFIX}{n}"), Some(recorded));
        if retry {
            task.frontmatter.status = TaskStatus::Todo;
            task.frontmatter.attempts = 0;
            task.frontmatter.rejected_sha = None;
            task.frontmatter.next_action = None;
            task.frontmatter.next_action_code = None;
        }
        self.persist(&task)?;
        Ok(n)
    }

    /// Registra a seção `## Diagnóstico de ambiente #n` (n = diagnósticos existentes + 1) na tarefa
    /// BLOQUEADA, sem alterar status nem `attempts` (não é ciclo de recuperação). Devolve `n`.
    pub fn record_diagnosis(&self, spec_id: &str, task_id: &str, text: &str) -> Result<usize, RunnerError> {
        validate_id("spec_id", spec_id)?;
        validate_id("task_id", task_id)?;
        let mut task = read_task(&self.task_path(spec_id, task_id))?;
        if task.frontmatter.status != TaskStatus::Blocked {
            return Err(RunnerError::Invalid(format!("tarefa {task_id} não está bloqueada")));
        }
        let n = diagnosis_total(&task.body) + 1;
        task.body = replace_section(&task.body, &format!("{DIAGNOSIS_PREFIX}{n}"), Some(text.to_string()));
        self.persist(&task)?;
        Ok(n)
    }

    /// REVISE_TASK: reescreve Objetivo e Critérios (e `allowed_scope`, se enviado) da tarefa
    /// BLOQUEADA, registra a análise como seção de recuperação e a reabre (`TODO`, `attempts`
    /// zerados). Findings, título, branch e demais campos ficam intactos. Devolve o ciclo.
    pub fn record_revision(
        &self,
        spec_id: &str,
        task_id: &str,
        revision: &TaskRevision,
        text: &str,
    ) -> Result<usize, RunnerError> {
        validate_id("spec_id", spec_id)?;
        validate_id("task_id", task_id)?;
        revision.validate().map_err(RunnerError::Invalid)?;
        let mut task = read_task(&self.task_path(spec_id, task_id))?;
        if task.frontmatter.status != TaskStatus::Blocked {
            return Err(RunnerError::Invalid(format!("tarefa {task_id} não está bloqueada")));
        }
        let n = recovery_total(&task.body) + 1;
        task.body = replace_section(&task.body, OBJECTIVE, Some(revision.objective.trim().to_string()));
        task.body = replace_section(&task.body, ACCEPTANCE, Some(revision.acceptance.trim().to_string()));
        if let Some(scope) = &revision.allowed_scope {
            task.frontmatter.allowed_scope = scope.iter().map(|s| s.trim().to_string()).collect();
        }
        let ext = apply_scope_added(&mut task.frontmatter.allowed_scope, text);
        let recorded = record_scope_extension(text, &ext);
        task.body = replace_section(&task.body, &format!("{RECOVERY_PREFIX}{n}"), Some(recorded));
        task.frontmatter.status = TaskStatus::Todo;
        task.frontmatter.attempts = 0;
        task.frontmatter.rejected_sha = None;
        task.frontmatter.next_action = None;
        task.frontmatter.next_action_code = None;
        self.persist(&task)?;
        Ok(n)
    }

    /// Troca o status, registra `next_action` e persiste.
    fn transition(
        &self,
        task: &mut Task,
        status: TaskStatus,
        next_action: &str,
    ) -> Result<(), RunnerError> {
        task.frontmatter.status = status;
        task.frontmatter.next_action = Some(next_action.to_string());
        task.frontmatter.next_action_code = None;
        self.persist(task)
    }

    fn git(&self) -> Git {
        Git::new(self.repo_root.clone())
    }

    /// Registra o conteúdo atual de um arquivo de estado como linha de base para `write_guarded`.
    fn remember(&self, path: &Path) -> Result<(), RunnerError> {
        let text = raw_read(path)?;
        self.baselines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_path_buf(), text);
        Ok(())
    }

    /// Grava `text` somente se o arquivo ainda tem o conteúdo que o runner conhece.
    /// Divergência devolve `ExternalEdit` sem tocar no arquivo.
    fn write_guarded(&self, path: &Path, text: &str) -> Result<(), RunnerError> {
        write_guarded_in(&self.baselines, &self.repo_root, path, text)
    }

    /// `true` quando a base contém o commit de integração da tarefa (fonte da verdade de `DONE`).
    fn is_committed(&self, base: &str, spec_id: &str, task_id: &str) -> Result<bool, RunnerError> {
        is_integrated(&self.git(), base, spec_id, task_id)
    }

    /// Ids das tarefas da spec cuja integração consta na base.
    fn committed_ids(
        &self,
        base: &str,
        spec_id: &str,
        tasks: &[Task],
    ) -> Result<HashSet<String>, RunnerError> {
        let mut ids = HashSet::new();
        for t in tasks {
            if self.is_committed(base, spec_id, &t.frontmatter.id)? {
                ids.insert(t.frontmatter.id.clone());
            }
        }
        Ok(ids)
    }

    fn spec_dir(&self, spec_id: &str) -> PathBuf {
        sdd_dir(&self.repo_root).join("specs").join(spec_id)
    }

    /// Imagens de referência do usuário copiadas na aprovação (spec 084 T02), do diretório da spec
    /// no repositório principal (as worktrees não têm `.sdd/specs`).
    fn references(&self, spec_id: &str) -> crate::sdd::references::References {
        crate::sdd::references::References::load(&self.spec_dir(spec_id))
    }

    /// Imagens planejadas da tarefa (spec 092 T03): caminhos citados no objetivo e nos critérios,
    /// com a disponibilidade da geração informada pelo Desenvolvedor. Sem caminhos, vazio.
    fn planned_images(&self, task: &Task) -> crate::sdd::images::PlannedImages {
        let objective = section_text(&task.body, OBJECTIVE).unwrap_or_default();
        let text = format!("{objective}\n{}", acceptance_of(&task.body));
        // Só o que o Developer pode gerar nesta tarefa: dentro do `allowed_scope` e sem as
        // referências do usuário (pelo nome do arquivo).
        let spec_dir = self.spec_dir(&task.frontmatter.spec_id);
        let refs = crate::sdd::references::References::load(&spec_dir);
        let paths: Vec<String> = crate::sdd::images::planned_paths(&text)
            .into_iter()
            .filter(|p| crate::harness::tools::write_scope_allows(&task.frontmatter.allowed_scope, p))
            .filter(|p| !refs.files.iter().any(|f| p.rsplit('/').next() == Some(f.as_str())))
            .collect();
        if paths.is_empty() {
            return crate::sdd::images::PlannedImages::default();
        }
        use crate::sdd::images::GenerationStatus;
        let status = match self.developer.image_generation(&task.frontmatter.id) {
            GenerationStatus::Failed(reason) => GenerationStatus::Failed(self.scrub(&reason)),
            // Limite ou registro cheio, lidos do disco agora: nunca "disponível".
            GenerationStatus::Available => match crate::sdd::images::spec_refusal(&spec_dir) {
                Some(reason) => GenerationStatus::Failed(reason.to_string()),
                None => GenerationStatus::Available,
            },
            other => other,
        };
        crate::sdd::images::PlannedImages { paths, status }
    }

    fn task_path(&self, spec_id: &str, task_id: &str) -> PathBuf {
        self.spec_dir(spec_id)
            .join("tasks")
            .join(format!("{task_id}.md"))
    }

    /// Caminho injetivo: `.sdd/worktrees/<spec>/<task>` (ids não contêm separadores).
    fn worktree_path(&self, spec_id: &str, task_id: &str) -> PathBuf {
        sdd_dir(&self.repo_root)
            .join(WORKTREES_DIR)
            .join(spec_id)
            .join(task_id)
    }
}

/// `true` quando a base contém o commit de integração da tarefa (fonte da verdade de `DONE`).
pub fn is_integrated(
    git: &Git,
    base: &str,
    spec_id: &str,
    task_id: &str,
) -> Result<bool, RunnerError> {
    let prefix = format!("feat({spec_id}): {task_id} - ");
    Ok(git
        .subjects_matching(base, &prefix)?
        .iter()
        .any(|s| is_task_commit(s, spec_id, task_id)))
}

/// `true` quando a tarefa pode ser executada agora: não BLOCKED e todas as dependências
/// integradas na base (`committed`, ids com commit de integração).
fn is_eligible(task: &Task, committed: &HashSet<String>) -> bool {
    task.frontmatter.status != TaskStatus::Blocked
        && task
            .frontmatter
            .depends_on
            .iter()
            .all(|dep| committed.contains(dep))
}

/// Lê um arquivo como texto, preservando o caminho no erro.
fn raw_read(path: &Path) -> Result<String, RunnerError> {
    fs::read_to_string(path).map_err(|source| RunnerError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Motivo registrado para uma tarefa BLOCKED: o texto gravado, no idioma em que foi gravado
/// (inclusive o pt-BR das tarefas antigas), ou, sem ele, o motivo genérico no idioma do projeto.
fn reason_of(task: &Task, lang: Lang) -> String {
    task.frontmatter
        .next_action
        .clone()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| BlockReason::new(lang, "blocked", &[]).text)
}

/// Instante em RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`, segundos truncados).
fn rfc3339_of(at: std::time::SystemTime) -> String {
    let secs = at.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    crate::server::projects::format_utc(secs)
}

/// Nome da branch de uma tarefa: `sdd/<spec>/<task>`.
fn branch_name(spec_id: &str, task_id: &str) -> String {
    format!("sdd/{spec_id}/{task_id}")
}

/// Aceita apenas identificadores seguros para caminhos e branches.
fn validate_id(name: &'static str, value: &str) -> Result<(), RunnerError> {
    let ok = !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('.')
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(RunnerError::InvalidId {
            name,
            value: value.to_string(),
        })
    }
}

/// `true` quando `a` e `b` existem e apontam para o mesmo caminho real.
fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Se a worktree tiver alterações, indexa e commita (mensagem de branch, não de squash).
/// Antes, confirma que `wt` é a worktree da `branch` da tarefa e não a raiz do repositório.
fn commit_pending(
    wt: &Path,
    root: &Path,
    spec_id: &str,
    task_id: &str,
    branch: &str,
    lang: Lang,
) -> Result<(), RunnerError> {
    if same_path(wt, root) || !wt.join(".git").exists() {
        return Err(RunnerError::Invalid(format!(
            "{} não é uma worktree do repositório",
            wt.display()
        )));
    }
    let wt_git = Git::new(wt.to_path_buf());
    if wt_git.current_branch()? != branch {
        return Err(RunnerError::Invalid(format!(
            "worktree {} não está na branch {branch}",
            wt.display()
        )));
    }
    if !wt_git.is_clean()? {
        wt_git.add_all()?;
        wt_git.commit(&i18n::commit_subject(lang, &format!("chore({spec_id}): {task_id} - "), "commit.developer_changes"))?;
    }
    Ok(())
}

/// Linhas de base de escrita compartilháveis entre o runner e o [`TaskEditor`].
pub type SharedBaselines = Arc<Mutex<HashMap<PathBuf, String>>>;

/// Seção do corpo da tarefa onde `ask_architect` registra pergunta, resposta e mudanças.
pub const ADVICE_SECTION: &str = "Orientações do Arquiteto";
/// Máximo de globs de `allowed_scope` da tarefa após ampliações.
const MAX_TASK_SCOPE: usize = 32;
/// Limite do histórico da seção de orientações (bytes).
const MAX_ADVICE_SECTION: usize = 16 * 1024;

/// Gravação guardada: só escreve se o arquivo ainda tem o conteúdo que o runner conhece.
fn write_guarded_in(
    baselines: &SharedBaselines,
    repo_root: &Path,
    path: &Path,
    text: &str,
) -> Result<(), RunnerError> {
    let mut baselines = baselines.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(known) = baselines.get(path) {
        if raw_read(path)? != *known {
            let shown = path.strip_prefix(repo_root).unwrap_or(path);
            return Err(RunnerError::ExternalEdit {
                path: shown.display().to_string(),
            });
        }
    }
    fs::write(path, text).map_err(|source| RunnerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    baselines.insert(path.to_path_buf(), text.to_string());
    Ok(())
}

/// Edita a tarefa em disco durante a execução de um agente (`ask_architect`), pelo mesmo
/// caminho guardado do runner (as linhas de base são compartilhadas).
#[derive(Clone)]
pub struct TaskEditor {
    repo_root: PathBuf,
    baselines: SharedBaselines,
}

impl TaskEditor {
    pub fn new(repo_root: impl Into<PathBuf>, baselines: SharedBaselines) -> Self {
        Self { repo_root: repo_root.into(), baselines }
    }

    /// Aplica a orientação: acrescenta `scope_add` ao `allowed_scope` (globs já filtrados),
    /// substitui os critérios por `acceptance` (se válidos) e anexa `entry` à seção
    /// [`ADVICE_SECTION`]. Devolve os globs efetivamente acrescentados e se os critérios mudaram.
    pub fn apply_advice(
        &self,
        spec_id: &str,
        task_id: &str,
        entry: &str,
        scope_add: &[String],
        acceptance: Option<&str>,
    ) -> Result<(Vec<String>, bool), RunnerError> {
        self.apply_advice_report(spec_id, task_id, entry, scope_add, acceptance).map(|(ext, rewritten)| (ext.added, rewritten))
    }

    /// Como [`Self::apply_advice`], devolvendo também o que NÃO entrou no `allowed_scope` e o motivo
    /// (spec 094 T07). A ampliação segue [`extend_scope`] e o registro anexado a `entry` lista só o
    /// que entrou ("allowed_scope ampliado") e, por caminho, o motivo do que não entrou.
    pub fn apply_advice_report(
        &self,
        spec_id: &str,
        task_id: &str,
        entry: &str,
        scope_add: &[String],
        acceptance: Option<&str>,
    ) -> Result<(ScopeExtension, bool), RunnerError> {
        validate_id("spec_id", spec_id)?;
        validate_id("task_id", task_id)?;
        let path = sdd_dir(&self.repo_root).join("specs").join(spec_id).join("tasks").join(format!("{task_id}.md"));
        let mut task = read_task(&path)?;
        let (scope_add, _) = crate::harness::tools::filter_scope_add(scope_add);
        let ext = extend_scope(&mut task.frontmatter.allowed_scope, &scope_add);
        let mut entry = entry.trim().to_string();
        if !ext.added.is_empty() {
            entry.push_str(&format!("\n- allowed_scope ampliado: {}", ext.added.join(", ")));
        }
        for refusal in &ext.refused {
            entry.push_str(&format!("\n- {}", refusal.describe()));
        }
        let mut rewritten = false;
        if let Some(text) = acceptance {
            if validate_text_block("critérios", text).is_ok() {
                task.body = replace_section(&task.body, ACCEPTANCE, Some(text.trim().to_string()));
                rewritten = true;
            }
        }
        let mut history = section_text(&task.body, ADVICE_SECTION).unwrap_or_default().trim_end().to_string();
        if !history.is_empty() {
            history.push_str("\n\n");
        }
        history.push_str(&entry);
        if history.len() > MAX_ADVICE_SECTION {
            let mut cut = history.len() - MAX_ADVICE_SECTION;
            while !history.is_char_boundary(cut) {
                cut += 1;
            }
            history = history[cut..].to_string();
        }
        task.body = replace_section(&task.body, ADVICE_SECTION, Some(history));
        write_guarded_in(&self.baselines, &self.repo_root, &path, &task.render()?)?;
        Ok((ext, rewritten))
    }
}

const HISTORY: &str = "Tentativas anteriores";
const HISTORY_SUMMARY_BYTES: usize = 2 * 1024;
const HISTORY_STAT_BYTES: usize = 1024;
const HISTORY_MAX_BYTES: usize = 8 * 1024;

/// Próximo número de tentativa: maior `### Tentativa <k>` existente + 1 (1 se nenhum).
fn next_attempt_number(existing: &str) -> usize {
    existing
        .lines()
        .filter_map(|l| l.strip_prefix("### Tentativa "))
        .filter_map(|r| r.trim().parse::<usize>().ok())
        .max()
        .map_or(1, |m| m + 1)
}

/// Recua linhas que começam com `#` para que não quebrem a seção do corpo da tarefa.
fn indent_headings(text: &str) -> String {
    text.lines()
        .map(|l| if l.starts_with('#') { format!(" {l}") } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Junta `entry` ao histórico e descarta as tentativas mais antigas até caber em `max` bytes
/// (a mais recente é sempre mantida, truncada se preciso).
fn append_history(existing: &str, entry: &str, max: usize) -> String {
    let mut entries: Vec<String> = Vec::new();
    for part in existing.split("### Tentativa ") {
        if !part.trim().is_empty() {
            entries.push(format!("### Tentativa {part}"));
        }
    }
    entries.push(entry.to_string());
    let mut kept: Vec<String> = Vec::new();
    let mut total = 0usize;
    for e in entries.into_iter().rev() {
        if kept.is_empty() {
            let e = tail_bytes_head(&e, max);
            total += e.len();
            kept.push(e);
        } else if total + e.len() <= max {
            total += e.len();
            kept.push(e);
        } else {
            break;
        }
    }
    kept.reverse();
    kept.concat()
}

/// Critérios de aceitação da tarefa; sem a seção, usa o corpo inteiro.
fn acceptance_of(body: &str) -> String {
    section_text(body, ACCEPTANCE)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| body.trim().to_string())
}

/// Conteúdo da seção `## <title>` (sem o cabeçalho), até o próximo `## ` ou fim.
pub fn section_text(body: &str, title: &str) -> Option<String> {
    let header = format!("## {title}");
    let mut found = false;
    let mut out = String::new();
    for line in body.lines() {
        if !found {
            if line.trim_end() == header {
                found = true;
            }
            continue;
        }
        if line.starts_with("## ") {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    found.then_some(out)
}

/// Limite de cada texto de uma revisão de tarefa.
pub const MAX_REVISION_BYTES: usize = 8 * 1024;
/// Máximo de globs de `allowed_scope` numa revisão.
pub const MAX_REVISION_SCOPE: usize = 32;

/// Nova definição de uma tarefa bloqueada (decisão REVISE_TASK do Arquiteto).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRevision {
    pub objective: String,
    pub acceptance: String,
    pub allowed_scope: Option<Vec<String>>,
}

impl TaskRevision {
    /// Textos não vazios, ≤ 8 KiB, sem frontmatter/`---` nem cabeçalhos de seção; escopo com globs
    /// relativos, sem `..`, `.sdd/**` nem `.git/**`, no máximo 32 itens.
    pub fn validate(&self) -> Result<(), String> {
        for (name, text) in [("objetivo", &self.objective), ("critérios", &self.acceptance)] {
            validate_text_block(name, text)?;
        }
        if let Some(scope) = &self.allowed_scope {
            validate_scope(scope)?;
        }
        Ok(())
    }
}

/// Texto de seção da tarefa: não vazio, ≤ 8 KiB, sem frontmatter/`---` nem cabeçalhos de seção.
pub fn validate_text_block(name: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{name} vazio"));
    }
    if text.len() > MAX_REVISION_BYTES {
        return Err(format!("{name} grande demais"));
    }
    if text.lines().any(|l| {
        let l = l.trim();
        l.starts_with("---") || l.starts_with("# ") || l.starts_with("## ")
    }) {
        return Err(format!("{name} contém frontmatter ou cabeçalho de seção"));
    }
    Ok(())
}

/// Valida um `allowed_scope`: globs relativos, sem `..`, `.sdd/**` nem `.git/**`, 1 a 32 itens.
pub fn validate_scope(scope: &[String]) -> Result<(), String> {
    if scope.is_empty() || scope.len() > MAX_REVISION_SCOPE {
        return Err("allowed_scope com quantidade inválida de itens".into());
    }
    for glob in scope {
        let g = glob.trim().trim_start_matches("./");
        let first = g.split('/').next().unwrap_or("");
        if g.is_empty()
            || g.len() > 200
            || g.starts_with('/')
            || g.contains('\\')
            || g.contains('\n')
            || g.split('/').any(|s| s == "..")
            || first == "**"
            || first.eq_ignore_ascii_case(".sdd")
            || first.eq_ignore_ascii_case(".git")
        {
            return Err(format!("glob de escopo inválido: {glob}"));
        }
    }
    Ok(())
}

/// Caminho da worktree de uma tarefa: `.sdd/worktrees/<spec>/<task>` sob a raiz do projeto.
pub fn task_worktree_path(repo_root: &Path, spec_id: &str, task_id: &str) -> PathBuf {
    sdd_dir(repo_root).join(WORKTREES_DIR).join(spec_id).join(task_id)
}

/// Quantidade de seções de análise do modelo posteriores ao último marcador de definição revisada
/// ou de retomada pelo humano (as anteriores seguem no corpo como histórico, mas não contam).
/// Seções marcadas com [`NOT_COUNTED_LINE`] (escaladas determinísticas) ou com a causa do limite
/// de ciclos (formato anterior à marca) também não contam.
pub fn recovery_cycles(body: &str) -> usize {
    let marks = [format!("## {REVISED_MARK}"), format!("## {HUMAN_RESUME_MARK}")];
    let analysis = format!("## {RECOVERY_PREFIX}");
    let mut count = 0;
    let mut pending = false; // seção de análise aberta, ainda não contada
    let (mut legacy_cause, mut escalated) = (false, false);
    for line in body.lines().chain(std::iter::once("## ")) {
        if line.starts_with("## ") {
            if pending {
                count += 1;
            }
            pending = false;
            (legacy_cause, escalated) = (false, false);
            if marks.iter().any(|m| line.trim_end() == m) {
                count = 0;
            } else if line.starts_with(&analysis) {
                pending = true;
            }
        } else if pending {
            let l = line.trim_end();
            legacy_cause |= l == LEGACY_LIMIT_CAUSE;
            escalated |= l == "- Decisão: escalar ao humano";
            // Formato antigo: a causa do limite só isenta se a mesma seção escalou (um RETRY com
            // essa causa, vinda do modelo, continua contando).
            if l == NOT_COUNTED_LINE || (legacy_cause && escalated) {
                pending = false;
            }
        }
    }
    count
}

/// Separa os caminhos de `scope_add` (análise de recuperação) em aceitos e recusados. Aceitos:
/// válidos como `allowed_scope` (relativos, sem `..`, fora de `.sdd/`, `.git/` e `.nants/`), sem
/// duplicatas, no máximo [`crate::harness::tools::MAX_SCOPE_ADD`].
pub fn validate_scope_add(raw: &[String]) -> (Vec<String>, Vec<String>) {
    let mut ok: Vec<String> = Vec::new();
    let mut bad = Vec::new();
    for path in raw {
        let p = path.trim().trim_start_matches("./").to_string();
        let first = p.split('/').next().unwrap_or("");
        let valid = validate_scope(std::slice::from_ref(&p)).is_ok()
            && !first.eq_ignore_ascii_case(".nants")
            && !p.contains(", ");
        if !valid || (ok.len() >= crate::harness::tools::MAX_SCOPE_ADD && !ok.contains(&p)) {
            bad.push(path.clone());
        } else if !ok.contains(&p) {
            ok.push(p);
        }
    }
    (ok, bad)
}

/// Resultado de uma ampliação do `allowed_scope` (spec 094 T07): o que entrou e o que não entrou,
/// com o motivo fechado.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeExtension {
    pub added: Vec<String>,
    pub refused: Vec<crate::harness::tools::ScopeRefusal>,
}

/// Glob da lista (com `*`) que cobre o caminho, pelo mesmo matcher do bloqueio de escrita.
fn covering_glob<'a>(scope: &'a [String], path: &str) -> Option<&'a String> {
    scope
        .iter()
        .find(|g| g.contains('*') && g.as_str() != path && crate::harness::tools::glob_match_path(g, path))
}

/// Compacta o `allowed_scope`: remove SÓ entradas literais (sem `*`) cobertas por um glob da lista,
/// preservando a ordem das demais. Glob nunca sai e nunca é comparado com glob, então o conjunto de
/// caminhos permitidos não encolhe. Devolve as entradas removidas.
pub fn compact_scope(scope: &mut Vec<String>) -> Vec<String> {
    let globs: Vec<String> = scope.iter().filter(|g| g.contains('*')).cloned().collect();
    let mut removed = Vec::new();
    scope.retain(|entry| {
        let redundant = !entry.contains('*') && covering_glob(&globs, entry).is_some();
        if redundant {
            removed.push(entry.clone());
        }
        !redundant
    });
    removed
}

/// Amplia o `allowed_scope` com `add` (já validados): compacta antes do limite; o caminho já
/// presente, ou literal coberto por um glob da lista, não entra nem ocupa vaga; sem vaga (mesmo
/// compactado), é recusado pelo limite. Mesmas regras para `ask_architect` e para a recuperação.
pub fn extend_scope(scope: &mut Vec<String>, add: &[String]) -> ScopeExtension {
    use crate::harness::tools::ScopeRefusal;
    let mut out = ScopeExtension::default();
    if add.is_empty() {
        return out;
    }
    compact_scope(scope);
    for path in add {
        if scope.contains(path) {
            out.refused.push(ScopeRefusal::Covered { path: path.clone(), by: path.clone() });
        } else if let Some(glob) = covering_glob(scope, path).filter(|_| !path.contains('*')) {
            out.refused.push(ScopeRefusal::Covered { path: path.clone(), by: glob.clone() });
        } else if scope.len() < MAX_TASK_SCOPE {
            scope.push(path.clone());
            out.added.push(path.clone());
        } else {
            out.refused.push(ScopeRefusal::Limit { path: path.clone(), limit: MAX_TASK_SCOPE });
        }
    }
    out
}

/// Prefixo da linha da análise com um caminho de `scope_add` que não entrou e o motivo fechado.
pub const SCOPE_REFUSED_LINE: &str = "- Escopo não ampliado: ";

/// Acrescenta ao `allowed_scope` os caminhos da linha [`SCOPE_ADDED_LINE`] da análise (revalidados,
/// pelas regras de [`extend_scope`]). Só a linha no início (sem recuo) conta: texto do modelo
/// nas demais linhas da seção vem recuado e não forja a ampliação.
fn apply_scope_added(scope: &mut Vec<String>, text: &str) -> ScopeExtension {
    let raw: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix(SCOPE_ADDED_LINE))
        .flat_map(|l| l.split(", ").map(|p| p.trim().to_string()))
        .filter(|p| !p.is_empty())
        .collect();
    let (paths, _) = validate_scope_add(&raw);
    extend_scope(scope, &paths)
}

/// Registro honesto da ampliação na análise: a linha [`SCOPE_ADDED_LINE`] passa a listar só o que
/// entrou (some se nada entrou) e cada caminho que não entrou ganha uma linha
/// [`SCOPE_REFUSED_LINE`] com o motivo. Sem a linha, o texto fica como está. Idempotente.
fn record_scope_extension(text: &str, ext: &ScopeExtension) -> String {
    if !text.lines().any(|l| l.starts_with(SCOPE_ADDED_LINE)) {
        return text.to_string();
    }
    let mut lines = Vec::new();
    let mut placed = false;
    for line in text.lines() {
        if !line.starts_with(SCOPE_ADDED_LINE) {
            lines.push(line.to_string());
            continue;
        }
        if std::mem::replace(&mut placed, true) {
            continue;
        }
        if !ext.added.is_empty() {
            lines.push(format!("{SCOPE_ADDED_LINE}{}", ext.added.join(", ")));
        }
        lines.extend(ext.refused.iter().map(|r| format!("{SCOPE_REFUSED_LINE}{}", r.describe())));
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// O texto da análise como será registrado ao aplicá-lo sobre `scope` (sem alterar `scope`): a
/// conversa mostra o mesmo que o corpo da tarefa.
pub fn reconcile_scope_added(scope: &[String], text: &str) -> String {
    let mut probe = scope.to_vec();
    let ext = apply_scope_added(&mut probe, text);
    record_scope_extension(text, &ext)
}

/// Última análise de recuperação do corpo (cabeçalho incluído, sem o bloco JSON das opções): a
/// estratégia vigente da tarefa. `None` sem análise.
pub fn latest_recovery(body: &str) -> Option<String> {
    let notes = recovery_notes(body);
    let header = format!("## {RECOVERY_PREFIX}");
    // Só cabeçalho no início de linha (texto do modelo nas seções vem recuado).
    let start = match notes.rfind(&format!("\n{header}")) {
        Some(i) => i + 1,
        None if notes.starts_with(&header) => 0,
        None => return None,
    };
    let latest = notes[start..].trim();
    (!latest.is_empty()).then(|| latest.to_string())
}

/// Total de seções de análise de recuperação no corpo (inclui as anteriores a uma revisão);
/// numera a próxima seção sem colidir com o histórico.
fn recovery_total(body: &str) -> usize {
    let analysis = format!("## {RECOVERY_PREFIX}");
    body.lines().filter(|l| l.starts_with(&analysis)).count()
}

/// Total de seções de diagnóstico de ambiente no corpo.
pub fn diagnosis_total(body: &str) -> usize {
    let header = format!("## {DIAGNOSIS_PREFIX}");
    body.lines().filter(|l| l.starts_with(&header)).count()
}

/// Texto (cabeçalhos incluídos) das seções de diagnóstico de ambiente, em ordem.
pub fn diagnosis_notes(body: &str) -> String {
    let header = format!("## {DIAGNOSIS_PREFIX}");
    let mut out = String::new();
    let mut inside = false;
    for line in body.lines() {
        if line.starts_with("## ") {
            inside = line.starts_with(&header);
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

/// Acrescenta ao fim do corpo o marcador de definição revisada (sempre no fim: ele separa o
/// histórico dos ciclos que contam). Idempotente se o corpo já termina no marcador.
pub fn mark_revised(body: &str) -> String {
    let mark = format!("## {REVISED_MARK}");
    if body.lines().rev().find(|l| l.starts_with("## ")).is_some_and(|l| l.trim_end() == mark) {
        return body.to_string();
    }
    let mut out = body.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&mark);
    out.push_str("\n- Definição revisada; ciclos de recuperação anteriores não contam mais.\n");
    out
}

/// Acrescenta ao fim do corpo o marcador de retomada pelo humano (idempotente se o corpo já termina
/// nele): zera o orçamento de ciclos sem apagar o histórico.
pub fn mark_human_resumed(body: &str) -> String {
    let mark = format!("## {HUMAN_RESUME_MARK}");
    if body.lines().rev().find(|l| l.starts_with("## ")).is_some_and(|l| l.trim_end() == mark) {
        return body.to_string();
    }
    let mut out = body.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&mark);
    out.push_str("\n- Retomada manualmente pelo humano; ciclos de recuperação anteriores não contam mais.\n");
    out
}

/// Texto (cabeçalhos incluídos) das seções de análise de recuperação, em ordem.
pub fn recovery_notes(body: &str) -> String {
    let header = format!("## {RECOVERY_PREFIX}");
    let mut out = String::new();
    let mut inside = false;
    let mut in_options = false;
    for line in body.lines() {
        if line.starts_with("## ") {
            inside = line.starts_with(&header);
            in_options = false;
        }
        // O bloco JSON das opções é para a UI/servidor, não para os agentes.
        if line.trim_end() == OPTIONS_OPEN {
            in_options = true;
            continue;
        }
        if in_options {
            in_options = line.trim_end() != OPTIONS_CLOSE;
            continue;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

/// JSON (uma linha) do bloco de opções da ÚLTIMA análise de recuperação, com o número da análise.
/// `None` se a última análise não tem opções ou se, depois dela, o humano retomou ou a definição foi
/// revisada (opções vencidas).
pub fn latest_options_block(body: &str) -> Option<(usize, String)> {
    let header = format!("## {RECOVERY_PREFIX}");
    let stale = [format!("## {REVISED_MARK}"), format!("## {HUMAN_RESUME_MARK}")];
    let mut number: Option<usize> = None;
    let mut json: Option<String> = None;
    let mut state = 0u8; // 0 fora, 1 abriu, 2 leu o JSON
    let mut inside = false; // dentro da seção da própria análise
    for line in body.lines() {
        if line.starts_with("## ") {
            inside = false;
            if let Some(n) = line.strip_prefix(&header).and_then(|n| n.split_whitespace().next()?.parse::<usize>().ok()) {
                (number, json, state, inside) = (Some(n), None, 0, true);
            } else if stale.iter().any(|m| line.trim_end() == m) {
                (number, json, state) = (None, None, 0);
            }
            continue;
        }
        if number.is_none() || !inside {
            continue;
        }
        let l = line.trim_end();
        match state {
            0 if l == OPTIONS_OPEN => state = 1,
            1 if l == OPTIONS_CLOSE => state = 0,
            1 => {
                json.get_or_insert_with(|| l.to_string());
                state = 2;
            }
            2 if l == OPTIONS_CLOSE => state = 0,
            _ => {}
        }
    }
    Some((number?, json?))
}

/// Acrescenta uma entrada à seção "Orientação do humano" (texto do humano, já redigido). Linhas que
/// começariam seção são recuadas; acima de [`GUIDANCE_MAX_BYTES`] as entradas mais antigas caem.
pub fn add_human_guidance(body: &str, entry: &str) -> String {
    let existing = section_text(body, GUIDANCE_SECTION).unwrap_or_default();
    let mut entries: Vec<String> = Vec::new();
    for line in existing.lines() {
        match (line.starts_with("- "), entries.last_mut()) {
            (false, Some(last)) => {
                last.push('\n');
                last.push_str(line);
            }
            _ if line.trim().is_empty() => {}
            _ => entries.push(line.to_string()),
        }
    }
    let mut item = String::from("- ");
    item.push_str(&indent_headings(entry.trim()).replace('\n', "\n  "));
    entries.push(tail_bytes_head(&item, GUIDANCE_MAX_BYTES));
    while entries.len() > 1 && entries.iter().map(|e| e.len() + 1).sum::<usize>() > GUIDANCE_MAX_BYTES {
        entries.remove(0);
    }
    replace_section(body, GUIDANCE_SECTION, Some(entries.join("\n")))
}

/// Aplica uma revisão (já validada) ao corpo e ao escopo da tarefa, sem mudar o status (a retomada
/// reabre a tarefa). Findings, título e demais campos ficam intactos.
pub fn apply_revision(task: &mut Task, revision: &TaskRevision) {
    task.body = replace_section(&task.body, OBJECTIVE, Some(revision.objective.trim().to_string()));
    task.body = replace_section(&task.body, ACCEPTANCE, Some(revision.acceptance.trim().to_string()));
    if let Some(scope) = &revision.allowed_scope {
        task.frontmatter.allowed_scope = scope.iter().map(|s| s.trim().to_string()).collect();
    }
}

/// Notas de recuperação para o briefing do Desenvolvedor (spec 062, C-03): análises superadas por
/// uma mais recente ganham "(superada pela análise #n)" no cabeçalho; em YOLO, uma decisão antiga de
/// "escalar ao humano" é apresentada sem sugerir espera e o texto final diz que não há decisão
/// pendente. Só a apresentação muda: o corpo da tarefa em disco fica intacto.
pub fn recovery_notes_for_dev(body: &str, yolo: bool) -> String {
    let notes = recovery_notes(body);
    if notes.is_empty() {
        return notes;
    }
    let header = format!("## {RECOVERY_PREFIX}");
    let number = |line: &str| line.strip_prefix(&header).and_then(|n| n.split_whitespace().next()?.parse::<usize>().ok());
    let latest = notes.lines().filter_map(number).max();
    let mut out = String::new();
    for line in notes.lines() {
        match (number(line), latest) {
            (Some(n), Some(last)) if n != last => {
                out.push_str(&format!("{} (superada pela análise #{last})\n", line.trim_end()));
            }
            _ if yolo && line.trim_end() == "- Decisão: escalar ao humano" => {
                out.push_str("- Decisão: escalada registrada (não há decisão pendente do humano)\n");
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    if yolo {
        out.push_str("\nNão há decisão pendente do humano; siga a estratégia da análise mais recente.\n");
    }
    out.trim().to_string()
}

/// Substitui (ou acrescenta, ou remove quando `content` é `None`) a seção `## <title>`.
fn replace_section(body: &str, title: &str, content: Option<String>) -> String {
    let header = format!("## {title}");
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    let start = lines.iter().position(|l| l.trim_end() == header);
    match (start, content) {
        (Some(s), content) => {
            let end = lines[s + 1..]
                .iter()
                .position(|l| l.starts_with("## "))
                .map_or(lines.len(), |p| s + 1 + p);
            let mut out = String::new();
            for l in &lines[..s] {
                out.push_str(l);
            }
            if let Some(c) = content {
                out.push_str(&header);
                out.push('\n');
                out.push_str(&c);
                out.push('\n');
            }
            for l in &lines[end..] {
                out.push_str(l);
            }
            out
        }
        (None, Some(c)) => {
            let mut out = body.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&header);
            out.push('\n');
            out.push_str(&c);
            out.push('\n');
            out
        }
        (None, None) => body.to_string(),
    }
}

/// Uma linha por finding (`id [severidade] local: descrição`), em uma única linha e truncada.
/// `redact` roda em cada campo ANTES de normalizar espaços e de cortar (segredo no limite do corte).
fn findings_summary(findings: &[Finding], redact: &dyn Fn(&str) -> String) -> String {
    let mut out = String::new();
    for f in findings {
        let flat = |t: &str| redact(t).split_whitespace().collect::<Vec<_>>().join(" ");
        let line = format!("{} [{}] {}: {}", flat(&f.id), flat(&f.severity), flat(&f.location), flat(&f.description));
        let mut end = line.len().min(FINDINGS_LOG_ITEM_BYTES);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str("- ");
        out.push_str(&line[..end]);
        out.push('\n');
    }
    out
}

/// Acrescenta a reprovação `n` ao registro de findings, descartando as mais antigas acima do teto.
/// `None` (nada a registrar) mantém a seção como está.
fn append_findings_log(existing: &str, n: u32, summary: &str) -> Option<String> {
    if summary.trim().is_empty() {
        return (!existing.trim().is_empty()).then(|| existing.trim_end().to_string());
    }
    let mut entries: Vec<String> = existing
        .split("### Reprovação ")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("### Reprovação {}", p.trim_end()))
        .collect();
    entries.push(format!("### Reprovação {n}\n{}", summary.trim_end()));
    while entries.len() > 1 && entries.iter().map(|e| e.len() + 1).sum::<usize>() > FINDINGS_LOG_MAX_BYTES {
        entries.remove(0);
    }
    Some(entries.join("\n"))
}

/// Serializa findings em YAML para a seção `## Findings Abertos`; `None` quando vazio.
fn render_findings(findings: &[Finding]) -> Result<Option<String>, RunnerError> {
    if findings.is_empty() {
        return Ok(None);
    }
    serde_yaml::to_string(findings)
        .map(Some)
        .map_err(|e| RunnerError::Invalid(format!("findings não serializáveis: {e}")))
}

/// Lê os findings abertos do corpo da tarefa. Seção ausente ou vazia significa nenhum.
fn read_findings(body: &str) -> Result<Vec<Finding>, RunnerError> {
    match section_text(body, FINDINGS) {
        None => Ok(Vec::new()),
        Some(text) if text.trim().is_empty() => Ok(Vec::new()),
        Some(text) => serde_yaml::from_str::<Vec<Finding>>(&text)
            .map_err(|e| RunnerError::Invalid(format!("'{FINDINGS}' inválido: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_kind_wire_values_and_inference_for_blocked_on_disk() {
        assert_eq!(BlockKind::Quality.as_str(), "quality");
        assert_eq!(BlockKind::Environmental.as_str(), "environmental");
        assert_eq!(BlockKind::infer(&[]), BlockKind::Environmental);
        assert_eq!(BlockKind::infer(&[finding("F-01")]), BlockKind::Quality);
    }
    use crate::sdd::{Settings, Spec, SpecMeta, SpecStatus, Task, TaskMeta, Verdict};
    use std::collections::VecDeque;
    use std::env;
    use std::process;
    use std::sync::{Arc, Mutex};

    const SPEC: &str = "s1";

    /// Executa `git` em `dir` com o ambiente de redirecionamento limpo; falha o teste se errar.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} falhou: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn task(id: &str, status: TaskStatus, deps: &[&str]) -> Task {
        Task {
            frontmatter: TaskMeta {
                id: id.into(),
                spec_id: SPEC.into(),
                title: format!("Título {id}"),
                status,
                depends_on: deps.iter().map(|d| d.to_string()).collect(),
                branch: String::new(),
                attempts: 0,
                max_attempts: 3,
                last_verdict: Verdict::None,
                allowed_scope: vec!["src/**".into()],
                covers: Vec::new(),
                reviewed_sha: None,
                rejected_sha: None,
                tested_sha: None,
                tested_signature: None,
                tested_at: None,
                test_evidence: None,
                test_run: None,
                next_action: None,
                next_action_code: None,
                diff_lines: Vec::new(),
                kind: None,
            },
            body: format!("## Objetivo\nFazer {id}.\n\n## Critérios de Aceitação\n- arquivo criado\n"),
        }
    }

    fn finding(id: &str) -> Finding {
        Finding {
            id: id.into(),
            severity: "HIGH".into(),
            location: "src/lib.rs:1".into(),
            description: "descrição".into(),
            recommendation: "recomendação".into(),
            ..Finding::default()
        }
    }

    fn approved(findings: Vec<Finding>) -> ReviewResult {
        ReviewResult {
            verdict: Verdict::Approved,
            summary: "aprovado".into(),
            findings,
            criteria: Vec::new(),
            screenshots_viewed: Vec::new(),
        }
    }

    fn rejected(findings: Vec<Finding>) -> ReviewResult {
        ReviewResult {
            verdict: Verdict::ChangesRequested,
            summary: "reprovado".into(),
            findings,
            criteria: Vec::new(),
            screenshots_viewed: Vec::new(),
        }
    }

    /// Desenvolvedor falso: grava `out-<task>-<n>.txt` na worktree (sem commitar).
    #[derive(Clone, Default)]
    struct FakeDev {
        briefs: Arc<Mutex<Vec<DevBrief>>>,
        /// Não escreve nada a partir da 2ª chamada (simula Desenvolvedor sem alterações).
        idle_after_first: bool,
        /// A 1ª sessão termina esgotada (limite de passos), com o segredo no resumo.
        exhaust_first: bool,
        /// Geração de imagens informada à revisão (spec 092 T03); `None` = padrão do trait.
        image_status: Option<crate::sdd::images::GenerationStatus>,
        /// Tarefas de cada `begin_attempt` recebido, em ordem.
        begun: Arc<Mutex<Vec<String>>>,
    }

    impl FakeDev {
        fn briefs(&self) -> Vec<DevBrief> {
            self.briefs.lock().unwrap().clone()
        }
    }

    impl DeveloperPort for FakeDev {
        fn implement<'a>(&'a self, brief: &'a DevBrief) -> Fut<'a, Result<DevReport, RunnerError>> {
            Box::pin(async move {
                let n = {
                    let mut all = self.briefs.lock().unwrap();
                    all.push(brief.clone());
                    all.len()
                };
                if !(self.idle_after_first && n > 1) {
                    fs::write(
                        brief.worktree.join(format!("out-{}-{n}.txt", brief.task_id)),
                        "conteúdo\n",
                    )
                    .unwrap();
                }
                if self.exhaust_first && n == 1 {
                    return Ok(DevReport {
                        summary: "Limite de passos atingido (150 passos).\n\nfiz metade; usei sk-SEGREDO-123456".into(),
                        exhausted: true,
                    });
                }
                Ok(DevReport {
                    summary: format!("dev {n}\n\n## Critérios\n1. ok — src/a.rs:{n}\n\n## Outro\nx"),
                    exhausted: false,
                })
            })
        }

        fn image_generation(&self, _task_id: &str) -> crate::sdd::images::GenerationStatus {
            self.image_status.clone().unwrap_or_default()
        }

        fn begin_attempt(&self, task_id: &str) {
            self.begun.lock().unwrap().push(task_id.to_string());
        }
    }

    type Decide = Arc<dyn Fn(usize, &ReviewInput) -> ReviewResult + Send + Sync>;

    /// Revisor roteirizado: `decide(n, input)` com `n` = chamadas anteriores para a mesma tarefa.
    #[derive(Clone)]
    struct FakeReviewer {
        decide: Decide,
        inputs: Arc<Mutex<Vec<ReviewInput>>>,
    }

    impl FakeReviewer {
        fn new(f: impl Fn(usize, &ReviewInput) -> ReviewResult + Send + Sync + 'static) -> Self {
            Self {
                decide: Arc::new(f),
                inputs: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn inputs(&self) -> Vec<ReviewInput> {
            self.inputs.lock().unwrap().clone()
        }
    }

    impl ReviewerPort for FakeReviewer {
        fn review<'a>(
            &'a self,
            input: &'a ReviewInput,
        ) -> Fut<'a, Result<ReviewResult, RunnerError>> {
            Box::pin(async move {
                let prior = {
                    let mut all = self.inputs.lock().unwrap();
                    let prior = all.iter().filter(|i| i.task_id == input.task_id).count();
                    all.push(input.clone());
                    prior
                };
                Ok((self.decide)(prior, input))
            })
        }
    }

    /// Revisor que aprova e, na primeira revisão, faz a base andar com um arquivo fora de `.sdd/`:
    /// a árvore pós-squash deixa de ser idêntica à revisada, então os testes rodam na base.
    fn base_moving_reviewer(repo: PathBuf) -> FakeReviewer {
        FakeReviewer::new(move |prior, _| {
            if prior == 0 {
                fs::write(repo.join("base-andou.txt"), "x\n").unwrap();
                git_in(&repo, &["add", "-A"]);
                git_in(&repo, &["commit", "-m", "chore: base andou"]);
            }
            approved(vec![])
        })
    }

    /// Testes roteirizados: cada chamada consome um resultado (`true` = passa); vazio = passa.
    #[derive(Clone, Default)]
    struct ScriptedTests {
        script: Arc<Mutex<VecDeque<bool>>>,
        calls: Arc<Mutex<usize>>,
        /// `(índice da chamada, arquivo, conteúdo)`: simula edição do humano durante o teste.
        effect: Option<(usize, PathBuf, String)>,
    }

    impl ScriptedTests {
        fn new(script: Vec<bool>) -> Self {
            Self {
                script: Arc::new(Mutex::new(script.into())),
                calls: Arc::new(Mutex::new(0)),
                effect: None,
            }
        }

        fn with_effect(mut self, call: usize, path: PathBuf, content: &str) -> Self {
            self.effect = Some((call, path, content.to_string()));
            self
        }
    }

    impl TestExecutor for ScriptedTests {
        fn run<'a>(
            &'a self,
            _dir: &'a Path,
            _command: &'a str,
        ) -> Fut<'a, Result<TestResult, RunnerError>> {
            Box::pin(async move {
                let n = {
                    let mut calls = self.calls.lock().unwrap();
                    *calls += 1;
                    *calls
                };
                if let Some((call, path, content)) = &self.effect {
                    if *call + 1 == n {
                        fs::write(path, content).unwrap();
                    }
                }
                let passed = self.script.lock().unwrap().pop_front().unwrap_or(true);
                let output = if passed {
                    format!("ok {n}")
                } else {
                    format!("falha {n}")
                };
                Ok(TestResult { passed, output, exit_code: None, timed_out: false })
            })
        }
    }

    /// Coleta eventos para inspeção pelos testes.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<RunnerEvent>>>);

    impl Recorder {
        fn events(&self) -> Vec<RunnerEvent> {
            self.0.lock().unwrap().clone()
        }

        fn statuses(&self, task_id: &str) -> Vec<TaskStatus> {
            self.events()
                .into_iter()
                .filter_map(|e| match e {
                    RunnerEvent::TaskUpdated {
                        task_id: t, status, ..
                    } if t == task_id => Some(status),
                    _ => None,
                })
                .collect()
        }
    }

    impl EventSink for Recorder {
        fn emit(&self, ev: RunnerEvent) {
            self.0.lock().unwrap().push(ev);
        }
    }

    /// Repositório Git temporário com `.sdd` (settings e spec) e commit inicial.
    struct Fixture {
        repo: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str, test_command: &str) -> Self {
            let base = env::temp_dir().join(format!("nants-runner-{}-{}", process::id(), tag));
            let _ = fs::remove_dir_all(&base);
            let repo = base.join("repo");
            fs::create_dir_all(&repo).unwrap();
            git_in(&repo, &["init", "-b", "main"]);
            git_in(&repo, &["config", "user.name", "Nants Test"]);
            git_in(&repo, &["config", "user.email", "test@example.com"]);
            git_in(&repo, &["config", "commit.gpgsign", "false"]);
            fs::write(repo.join("README.md"), "inicial\n").unwrap();
            let sdd = repo.join(".sdd");
            crate::sdd::write_settings(
                &sdd.join("settings.yaml"),
                &Settings {
                    base_branch: "main".into(),
                    merge_strategy: "squash".into(),
                    test_command: test_command.into(),
                    allowed_commands: vec![],
                    denied_commands: vec![],
                    checks: Vec::new(),
                    formatters: Vec::new(),
                    failure_artifacts: Vec::new(),
                    screenshots: Vec::new(),
                    environment: Default::default(),
                    environment_hash: None,
                    approved_environment_hashes: Vec::new(),
                    max_attempts: crate::sdd::DEFAULT_LIMIT,
                    max_test_cycles: crate::sdd::DEFAULT_LIMIT,
                    yolo: false,
                    review_diff_budget: None,
                    skills: true,
                    ask_architect_limit: None,
                    architect_internet: false,
                },
            )
            .unwrap();
            crate::sdd::write_spec(
                &sdd.join("specs").join(SPEC).join("spec.md"),
                &Spec {
                    frontmatter: SpecMeta {
                        id: SPEC.into(),
                        title: "Spec de teste".into(),
                        status: SpecStatus::InProgress,
                        base_branch: "main".into(),
                        created_at: "2026-10-13".into(),
                        test_command: None,
                        run_command: None,
                        run_url: None,
                        checks: None,
                        formatters: None,
                        failure_artifacts: None,
                        screenshots: None,
                        references: None,
                        environment: None,
                    },
                    body: "## Contexto\nteste\n".into(),
                },
            )
            .unwrap();
            let fx = Self { repo };
            fx.commit_all("chore: commit inicial");
            fx
        }

        fn commit_all(&self, msg: &str) {
            git_in(&self.repo, &["add", "-A"]);
            git_in(&self.repo, &["commit", "-m", msg]);
        }

        fn add_task(&self, task: &Task) {
            self.put_task(task);
            self.commit_all("chore: tarefa");
        }

        fn put_task(&self, task: &Task) {
            crate::sdd::write_task(&self.task_path(&task.frontmatter.id), task).unwrap();
        }

        fn task_path(&self, id: &str) -> PathBuf {
            self.repo.join(".sdd/specs/s1/tasks").join(format!("{id}.md"))
        }

        fn read_task(&self, id: &str) -> Task {
            read_task(&self.task_path(id)).unwrap()
        }

        fn worktree(&self, id: &str) -> PathBuf {
            self.repo.join(".sdd/worktrees").join(SPEC).join(id)
        }

        fn settings_path(&self) -> PathBuf {
            self.repo.join(".sdd/settings.yaml")
        }

        fn head(&self) -> String {
            git_in(&self.repo, &["rev-parse", "main"])
        }

        fn main_subjects(&self) -> Vec<String> {
            git_in(&self.repo, &["log", "--format=%s", "main"])
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn feat_commits(&self) -> usize {
            self.main_subjects()
                .iter()
                .filter(|s| s.starts_with("feat(s1): "))
                .count()
        }

        fn branch_exists(&self, branch: &str) -> bool {
            git_in(&self.repo, &["branch", "--list", branch]).contains(branch)
        }

        /// Cria a worktree e a branch da tarefa com um commit, como se o Desenvolvedor tivesse rodado.
        /// Retorna o HEAD da branch.
        fn leftover_work(&self, id: &str) -> String {
            let branch = branch_name(SPEC, id);
            let wt = self.worktree(id);
            git_in(
                &self.repo,
                &["worktree", "add", "-B", &branch, wt.to_str().unwrap(), "main"],
            );
            fs::write(wt.join(format!("out-{id}-1.txt")), "conteúdo\n").unwrap();
            git_in(&wt, &["add", "-A"]);
            git_in(&wt, &["commit", "-m", "chore: trabalho"]);
            git_in(&self.repo, &["rev-parse", &branch])
        }

        fn noop_runner(&self, rev: &FakeReviewer) -> TaskRunner {
            TaskRunner::new(
                self.repo.clone(),
                Box::new(NoopDev),
                Box::new(rev.clone()),
                Box::new(ScriptedTests::default()),
                Box::new(Recorder::default()),
            )
        }

        fn runner(
            &self,
            dev: &FakeDev,
            rev: &FakeReviewer,
            tests: impl TestExecutor + 'static,
            rec: &Recorder,
        ) -> TaskRunner {
            TaskRunner::new(
                self.repo.clone(),
                Box::new(dev.clone()),
                Box::new(rev.clone()),
                Box::new(tests),
                Box::new(rec.clone()),
            )
        }
    }

    #[test]
    fn role_guidance_scrubs_before_truncating() {
        let fx = Fixture::new("rg-scrub", "true");
        let secret = "sk-SEGREDO-123456";
        let dir = fx.repo.join(".sdd/specs/s1");
        fs::create_dir_all(&dir).unwrap();
        for pad in [1520usize, 1530, 1536 - 8] {
            let body = format!("## Orientações por papel\n### Revisor\n{}{secret}\n", "a".repeat(pad));
            fs::write(dir.join("spec.md"), body).unwrap();
            let runner = fx
                .noop_runner(&FakeReviewer::new(|_, _| approved(vec![])))
                .with_secrets(Arc::new(crate::server::events::SecretSet::new(vec![secret.to_string()])));
            let g = runner.role_guidance("s1", "Revisor");
            assert!(!g.contains("sk-SEG") && !g.contains(secret), "{g}");
        }
    }

    fn write_profile(root: &Path, role: &str, extra: &str) {
        let dir = root.join(".sdd/skills");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{role}.md")),
            format!("---\nrole: {role}\nstack: [clojure]\nupdated_at: 2026-10-16\nsource_spec: s1\n---\n## Papel\n- {extra}\n"),
        )
        .unwrap();
    }

    #[test]
    fn skill_profile_reads_root_per_role_scrubs_before_truncating_and_honors_off() {
        let fx = Fixture::new("skill-prof", "true");
        let secret = "sk-SEGREDO-123456";
        let runner = |fx: &Fixture| {
            fx.noop_runner(&FakeReviewer::new(|_, _| approved(vec![])))
                .with_secrets(Arc::new(crate::server::events::SecretSet::new(vec![secret.to_string()])))
        };
        assert_eq!(runner(&fx).skill_profile("developer"), "", "sem perfil: nada");
        write_profile(&fx.repo, "developer", "usa deps.edn");
        let dev = runner(&fx).skill_profile("developer");
        assert!(dev.starts_with(crate::sdd::skills::BLOCK_LABEL) && dev.contains("usa deps.edn"), "{dev}");
        assert_eq!(runner(&fx).skill_profile("reviewer"), "", "cada papel lê o seu");
        // Segredo no limite do corte: redigido antes de truncar.
        for pad in [1700usize, 1990, 2030] {
            write_profile(&fx.repo, "reviewer", &format!("{}{secret}", "a".repeat(pad)));
            let r = runner(&fx).skill_profile("reviewer");
            assert!(!r.contains("sk-SEG") && !r.contains(secret), "{r}");
        }
        // skills: false desliga sem apagar o arquivo.
        let sp = fx.repo.join(".sdd/settings.yaml");
        let mut s = crate::sdd::read_settings(&sp).unwrap();
        s.skills = false;
        crate::sdd::write_settings(&sp, &s).unwrap();
        assert_eq!(runner(&fx).skill_profile("developer"), "");
        assert!(fx.repo.join(".sdd/skills/developer.md").exists());
    }

    /// Spec 092 T03: a revisão recebe as imagens planejadas do objetivo e dos critérios, com a
    /// disponibilidade da geração informada pelo Desenvolvedor (o falso não gera: indisponível).
    #[tokio::test]
    async fn review_input_carries_planned_images_of_the_task() {
        let fx = Fixture::new("planejadas", "true");
        let mut t = task("T01", TaskStatus::Todo, &[]);
        t.frontmatter.allowed_scope = vec!["src/**".into(), "web/img/**".into()];
        // F-02: referência do usuário (pelo prefixo ou pelo nome) e caminho fora do escopo não contam.
        t.body = "## Objetivo\nHero em `web/img/hero.webp`, como referencia/mockup.png.\n\n## Critérios de Aceitação\n\
            - web/img/terra.webp existe\n- captura test-results/home.png\n- docs/arte/fora.webp\n- web/img/0123456789abcdef0123456789abcdef.png\n".into();
        fx.add_task(&t);
        fx.add_task(&task("T02", TaskStatus::Todo, &["T01"]));
        let refs = fx.repo.join(".sdd/specs").join(SPEC).join("references");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("0123456789abcdef0123456789abcdef.png"), crate::sdd::images::testing::png(2, 2)).unwrap();
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        assert_eq!(runner.run_task(SPEC, "T01").await.unwrap(), Outcome::Done);
        assert_eq!(runner.run_task(SPEC, "T02").await.unwrap(), Outcome::Done);
        let inputs = rev.inputs();
        assert_eq!(inputs[0].planned_images.paths, vec!["web/img/hero.webp", "web/img/terra.webp"]);
        assert_eq!(inputs[0].planned_images.status, crate::sdd::images::GenerationStatus::Unavailable);
        assert_eq!(inputs[1].planned_images, crate::sdd::images::PlannedImages::default(), "sem imagens, nada muda");
    }

    /// Spec 092 T03 (F-03): `begin_attempt` uma vez por tentativa, não a cada sessão (teste vermelho
    /// gera uma 2ª sessão na mesma tentativa).
    #[tokio::test]
    async fn begin_attempt_runs_once_per_attempt_not_per_session() {
        let fx = Fixture::new("begin-attempt", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let outcome = fx.runner(&dev, &rev, ScriptedTests::new(vec![false, true]), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(dev.briefs().len(), 2, "duas sessões na mesma tentativa");
        assert_eq!(dev.begun.lock().unwrap().as_slice(), ["T01"]);
    }

    /// Spec 092 T03 (F-01): com o limite de imagens da spec atingido (lido do disco na revisão), o
    /// status nunca é "disponível", mesmo que o Developer informe disponível.
    #[tokio::test]
    async fn planned_images_status_is_not_available_at_the_spec_limit() {
        use crate::sdd::images::{self, GenerationStatus, ManifestEntry};
        let fx = Fixture::new("planejadas-limite", "true");
        let mut t = task("T01", TaskStatus::Todo, &[]);
        t.frontmatter.allowed_scope = vec!["web/**".into()];
        t.body = "## Objetivo\nGere web/img/hero.webp.\n\n## Critérios de Aceitação\n- web/img/hero.webp existe\n".into();
        fx.add_task(&t);
        let spec_dir = fx.repo.join(".sdd/specs").join(SPEC);
        for i in 0..images::MAX_NEW_PER_SPEC {
            let entry = ManifestEntry {
                path: format!("web/img/{i}.webp"),
                hash: "a".repeat(64),
                prompt: "p".into(),
                width: None,
                height: None,
                bytes: 1,
                task: "T00".into(),
                at: "2026-10-19T00:00:00Z".into(),
                reused: false,
                stored: true,
            };
            images::append_manifest(&spec_dir, &entry).unwrap();
        }
        let dev = FakeDev { image_status: Some(GenerationStatus::Available), ..FakeDev::default() };
        let (rev, rec) = (FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        assert_eq!(fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap(), Outcome::Done);
        let planned = &rev.inputs()[0].planned_images;
        assert_eq!(planned.paths, vec!["web/img/hero.webp"]);
        assert_eq!(planned.status, GenerationStatus::Failed(images::REASON_LIMIT.into()));
    }

    #[tokio::test]
    async fn approves_and_integrates_directly() {
        let fx = Fixture::new("direto", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (
            FakeDev::default(),
            FakeReviewer::new(|_, _| approved(vec![])),
            Recorder::default(),
        );
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done);
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::Done);
        assert!(t.frontmatter.reviewed_sha.is_some());
        let run = t.frontmatter.test_run.expect("test_run gravado na integração");
        assert_eq!(run.command, "true");
        assert!(run.at.ends_with('Z') && run.at.len() == 20, "{}", run.at);
        assert!(run.summary.len() <= 500);
        assert_eq!(fx.feat_commits(), 1);
        assert!(fx.main_subjects().contains(&"feat(s1): T01 - Título T01".to_string()));
        assert!(fx.repo.join("out-T01-1.txt").exists(), "alteração do dev integrada");
        assert!(!fx.worktree("T01").exists());
        assert!(!fx.branch_exists("sdd/s1/T01"));
        let last_commit = git_in(&fx.repo, &["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(last_commit, "out-T01-1.txt", "commit contém somente o squash");
        assert_eq!(rev.inputs().len(), 1);
        assert!(rev.inputs()[0].diff.contains("out-T01-1.txt"));
        assert_eq!(
            rec.statuses("T01"),
            vec![
                TaskStatus::InProgress,
                TaskStatus::InReview,
                TaskStatus::Approved,
                TaskStatus::Integrating,
                TaskStatus::Done
            ]
        );
        assert!(matches!(
            rec.events().last(),
            Some(RunnerEvent::Integrated { task_id, .. }) if task_id == "T01"
        ));
    }

    #[tokio::test]
    async fn changes_requested_then_fix_is_integrated() {
        let fx = Fixture::new("correcao", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| {
            if n == 0 {
                rejected(vec![finding("F-01")])
            } else {
                approved(vec![])
            }
        });
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done);
        let briefs = dev.briefs();
        assert_eq!(briefs.len(), 2);
        assert!(briefs[0].findings.is_empty());
        assert_eq!(briefs[1].findings, vec![finding("F-01")]);
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.attempts, 1);
        assert_eq!(t.frontmatter.last_verdict, Verdict::Approved);
        assert!(!t.body.contains("## Findings Abertos"), "findings limpos na aprovação");
        let log = section_text(&t.body, FINDINGS_LOG).expect("registro dos findings revisados");
        assert!(log.contains("### Reprovação 1") && log.contains("F-01"), "{log}");
        assert_eq!(fx.feat_commits(), 1);
        assert_eq!(
            rec.statuses("T01"),
            vec![
                TaskStatus::InProgress,
                TaskStatus::InReview,
                TaskStatus::ChangesRequested,
                TaskStatus::InProgress,
                TaskStatus::InReview,
                TaskStatus::Approved,
                TaskStatus::Integrating,
                TaskStatus::Done
            ]
        );
    }

    #[test]
    fn extract_dev_criteria_takes_section_until_next_heading() {
        let s = "feito\n\n**Critérios:**\n1. a — x.rs:1\n2. b — teste t\n\n## Notas\nzz";
        assert_eq!(extract_dev_criteria(s), "1. a — x.rs:1\n2. b — teste t");
        assert_eq!(extract_dev_criteria("sem seção"), "");
        let big = format!("## Critérios\n{}", "é".repeat(5000));
        let out = extract_dev_criteria(&big);
        assert!(out.len() < 4200 && out.ends_with("[truncado]"));
    }

    #[test]
    fn approved_with_not_met_becomes_changes_requested_with_generated_finding() {
        let mut r = approved(vec![]);
        r.criteria = vec![
            CriterionCheck { criterion: "a".into(), met: true, evidence: "t1".into() },
            CriterionCheck { criterion: "b".into(), met: false, evidence: String::new() },
        ];
        let out = enforce_criteria(Lang::PtBr, r.clone());
        assert_eq!(out.verdict, Verdict::ChangesRequested);
        assert_eq!(out.findings.len(), 1);
        assert!(out.findings[0].description.contains("b"));
        r.criteria.pop();
        assert_eq!(enforce_criteria(Lang::PtBr, r).verdict, Verdict::Approved);
        assert_eq!(render_criteria(&out.criteria), "✓ a: t1\n✗ b");
    }

    #[test]
    fn manual_criteria_do_not_block_and_are_marked() {
        assert!(is_manual_criterion("- [ ] (Manual) abrir no navegador"));
        assert!(is_manual_criterion("2. (manual) testar no celular"));
        assert!(!is_manual_criterion("1. node --test verde (manual não)"));
        let mut r = approved(vec![]);
        r.criteria = vec![CriterionCheck { criterion: "(manual) olhar no navegador".into(), met: false, evidence: String::new() }];
        assert_eq!(enforce_criteria(Lang::PtBr, r).verdict, Verdict::Approved);
        let marked = mark_manual_criteria("1. teste verde\n- [ ] (manual) ver no navegador");
        assert!(marked.contains(&format!("(manual) ver no navegador [{HUMAN_CHECK_MARK}]")), "{marked}");
        assert!(!marked.lines().next().unwrap_or("").contains(HUMAN_CHECK_MARK));
        assert_eq!(manual_criteria("1. a\n- [ ] (manual) b"), vec!["[ ] (manual) b".to_string()]);
        // Spec 095 T08: a marca nova não tem travessão; a antiga, já gravada, segue reconhecida e não duplica.
        assert!(!HUMAN_CHECK_MARK.contains('\u{2014}'));
        let legacy = format!("- tudo funciona [{LEGACY_HUMAN_CHECK_MARK}]");
        assert!(is_manual_criterion(&legacy));
        assert_eq!(mark_manual_criteria(&legacy), legacy);
    }

    #[tokio::test]
    async fn reviewer_gets_dev_criteria_and_matrix_is_published_and_enforced() {
        let fx = Fixture::new("matriz-criterios", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| {
            let mut r = if n == 0 { approved(vec![]) } else { approved(vec![]) };
            r.criteria = vec![CriterionCheck { criterion: "c1".into(), met: n != 0, evidence: "e".into() }];
            r
        });
        let rec = Recorder::default();
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        let inputs = rev.inputs();
        assert_eq!(inputs[0].dev_criteria, "1. ok — src/a.rs:1");
        let texts: Vec<String> = rec
            .events()
            .into_iter()
            .filter_map(|ev| match ev {
                RunnerEvent::AgentMessage { agent, text, .. } if agent == "reviewer" => Some(text),
                _ => None,
            })
            .collect();
        assert!(texts[0].contains("Critérios:\n✗ c1: e"));
        assert!(texts[1].contains("Critérios:\n✓ c1: e"));
        assert_eq!(inputs.len(), 2, "not_met reprovou a primeira rodada");
    }

    fn medium(id: &str, location: &str) -> Finding {
        Finding {
            id: id.into(),
            severity: "MEDIUM".into(),
            location: location.into(),
            description: "descrição".into(),
            recommendation: "recomendação".into(),
            ..Finding::default()
        }
    }

    fn hunk(path: &str, start: u32, end: u32) -> ChangedHunk {
        ChangedHunk { path: path.into(), start, end }
    }

    fn no_redact(t: &str) -> String {
        t.to_string()
    }

    fn met(criterion: &str, met: bool) -> CriterionCheck {
        CriterionCheck { criterion: criterion.into(), met, evidence: "e".into() }
    }

    #[test]
    fn review_message_approved_without_findings_has_label_and_criteria_only() {
        let text = render_review_message(Verdict::Approved, "tudo certo", &[], &[], &[], &[met("c1", true)], &no_redact);
        assert_eq!(text, "Aprovado: tudo certo\n\nCritérios:\n✓ c1: e");
        let bare = render_review_message(Verdict::Approved, "ok", &[], &[], &[], &[], &no_redact);
        assert_eq!(bare, "Aprovado: ok");
    }

    #[test]
    fn review_message_changes_lists_findings_before_criteria() {
        let text = render_review_message(
            Verdict::ChangesRequested,
            "faltam coisas",
            &[medium("F-01", "src/a.rs:10")],
            &[],
            &[],
            &[met("c1", false)],
            &no_redact,
        );
        assert_eq!(
            text,
            "Pediu ajustes: faltam coisas\n\nApontamentos:\n- F-01 [MEDIUM] src/a.rs:10: descrição → recomendação\n\nCritérios:\n✗ c1: e"
        );
        assert!(text.find(FINDINGS_HEADER) < text.find(CRITERIA_HEADER));
    }

    #[test]
    fn review_message_with_observations_omits_empty_sections_and_scrubs() {
        let text = render_review_message(
            Verdict::Approved,
            "ok",
            &[],
            &[medium("F-02", "src/b.rs:3")],
            &[],
            &[met("c1", true)],
            &|t| t.replace("descrição", "[REDACTED]"),
        );
        assert_eq!(
            text,
            "Aprovado: ok\n\nObservações (não bloqueiam):\n- F-02 [MEDIUM] src/b.rs:3: [REDACTED]\n\nCritérios:\n✓ c1: e"
        );
        assert!(!text.contains(FINDINGS_HEADER));
    }

    #[test]
    fn review_message_lists_evaluated_captures_before_criteria() {
        // Spec 082 T03: "Capturas avaliadas:" depois das observações e antes de "Critérios:" (sempre por último).
        let captures = vec!["t01-1-shots-home.png".to_string(), "t01-1-shots-menu.png".to_string()];
        let text = render_review_message(
            Verdict::Approved,
            "ok",
            &[],
            &[medium("F-02", "src/b.rs:3")],
            &captures,
            &[met("c1", true)],
            &no_redact,
        );
        assert_eq!(
            text,
            "Aprovado: ok\n\nObservações (não bloqueiam):\n- F-02 [MEDIUM] src/b.rs:3: descrição\n\nCapturas avaliadas: t01-1-shots-home.png, t01-1-shots-menu.png\n\nCritérios:\n✓ c1: e"
        );
        let bare = render_review_message(Verdict::Approved, "ok", &[], &[], &captures, &[], &no_redact);
        assert!(bare.ends_with("\n\nCapturas avaliadas: t01-1-shots-home.png, t01-1-shots-menu.png"), "{bare}");
    }

    #[test]
    fn review_message_both_sections_and_long_lines_are_truncated() {
        let mut long = medium("F-03", "src/c.rs:1");
        long.description = "palavra ".repeat(200);
        let text = render_review_message(Verdict::ChangesRequested, "s", &[long.clone()], &[long], &[], &[], &no_redact);
        assert!(text.contains(FINDINGS_HEADER) && text.contains(OBSERVATIONS_HEADER));
        assert!(text.lines().all(|l| l.len() <= FINDING_LINE_BYTES + 6), "{text}");
        assert!(text.contains('…'));
    }

    #[test]
    fn converge_medium_inside_changed_hunk_blocks() {
        let r = rejected(vec![medium("F-02", "src/a.rs:12")]);
        let out = converge(Lang::PtBr, r, &[finding("F-01")], &[hunk("src/a.rs", 10, 15)]);
        assert_eq!(out.result.verdict, Verdict::ChangesRequested);
        assert_eq!(out.result.findings.len(), 1);
        assert!(out.observations.is_empty());
        // faixa a-b que cruza o hunk
        let out = converge(Lang::PtBr, rejected(vec![medium("F-02", "src/a.rs:1-10")]), &[finding("F-01")], &[hunk("src/a.rs", 10, 15)]);
        assert_eq!(out.result.findings.len(), 1);
    }

    #[test]
    fn converge_medium_in_same_file_as_previous_finding_blocks() {
        let mut prev = finding("F-01");
        prev.location = "src/a.rs:99".into();
        let out = converge(Lang::PtBr, rejected(vec![medium("F-02", "src/a.rs:3")]), &[prev], &[hunk("src/z.rs", 1, 2)]);
        assert_eq!(out.result.verdict, Verdict::ChangesRequested);
        assert_eq!(out.result.findings.len(), 1);
    }

    #[test]
    fn converge_medium_on_untouched_file_becomes_observation_and_promotes() {
        let out = converge(Lang::PtBr, rejected(vec![medium("F-02", "src/b.rs:3")]), &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]);
        assert_eq!(out.result.verdict, Verdict::Approved);
        assert!(out.result.findings.is_empty());
        assert_eq!(out.observations, vec![medium("F-02", "src/b.rs:3")]);
        assert!(out.result.summary.contains("viraram observações"), "{}", out.result.summary);
        // mesmo arquivo, mas fora das linhas alteradas e sem finding anterior nele
        let out = converge(Lang::PtBr, rejected(vec![medium("F-02", "src/z.rs:500")]), &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]);
        assert_eq!(out.result.verdict, Verdict::Approved);
    }

    #[test]
    fn converge_high_and_unlocated_findings_always_block() {
        let mut high = finding("F-02");
        high.location = "src/untouched.rs:1".into();
        let out = converge(Lang::PtBr, rejected(vec![high, medium("F-03", "src/b.rs:3")]), &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]);
        assert_eq!(out.result.verdict, Verdict::ChangesRequested);
        // Spec 082: com um bloqueante sobrando, nada é rebaixado (o caso da T04 do Test 04).
        let ids: Vec<&str> = out.result.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["F-02", "F-03"], "os dois bloqueiam, na ordem original");
        assert!(out.observations.is_empty());
        let out = converge(Lang::PtBr, rejected(vec![medium("F-04", "estrutura geral do módulo")]), &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]);
        assert_eq!(out.result.verdict, Verdict::ChangesRequested);
        assert_eq!(out.result.findings.len(), 1);
    }

    #[test]
    fn converge_unmet_criterion_prevents_promotion() {
        let mut r = rejected(vec![medium("F-02", "src/b.rs:3")]);
        r.criteria = vec![met("c1", false)];
        let out = converge(Lang::PtBr, r, &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]);
        assert_eq!(out.result.verdict, Verdict::ChangesRequested);
        // Spec 082: critério `not_met` mantém o MEDIUM bloqueante (o caso da T02 do Test 04).
        assert_eq!(out.result.findings, vec![medium("F-02", "src/b.rs:3")]);
        assert!(out.observations.is_empty());
        let mut manual = rejected(vec![medium("F-02", "src/b.rs:3")]);
        manual.criteria = vec![met("(manual) olhar no navegador", false)];
        assert_eq!(converge(Lang::PtBr, manual, &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]).result.verdict, Verdict::Approved);
    }

    #[test]
    fn converge_medium_outside_hunks_with_unmet_criterion_keeps_blocking() {
        let mut r = rejected(vec![medium("F-02", "src/b.rs:3"), medium("F-03", "src/c.rs:9")]);
        r.criteria = vec![met("c1", true), met("c2", false)];
        let out = converge(Lang::PtBr, r.clone(), &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]);
        assert_eq!(out.result, r, "nada é rebaixado quando não leva à aprovação");
        assert!(out.observations.is_empty());
    }

    #[test]
    fn converge_high_plus_medium_outside_hunks_both_block() {
        let high = Finding { location: "src/z.rs:1".into(), ..finding("F-05") };
        let r = rejected(vec![medium("F-06", "src/b.rs:3"), high]);
        let out = converge(Lang::PtBr, r.clone(), &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]);
        assert_eq!(out.result, r);
        assert!(out.observations.is_empty());
    }

    #[test]
    fn converge_only_medium_outside_hunks_without_unmet_approves_with_observations() {
        let mut r = rejected(vec![medium("F-02", "src/b.rs:3"), medium("F-03", "src/c.rs:9")]);
        r.criteria = vec![met("c1", true)];
        let out = converge(Lang::PtBr, r, &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]);
        assert_eq!(out.result.verdict, Verdict::Approved);
        assert!(out.result.findings.is_empty());
        assert_eq!(out.observations, vec![medium("F-02", "src/b.rs:3"), medium("F-03", "src/c.rs:9")]);
        assert!(out.result.summary.contains("viraram observações"), "{}", out.result.summary);
    }

    #[test]
    fn converge_without_hunks_never_demotes_even_with_previous_findings() {
        let r = rejected(vec![medium("F-02", "src/b.rs:3")]);
        let out = converge(Lang::PtBr, r.clone(), &[finding("F-01")], &[]);
        assert_eq!(out.result, r, "veredito e findings inalterados");
        assert!(out.observations.is_empty());
    }

    #[test]
    fn locations_without_slash_need_a_line_suffix_to_count_as_path() {
        for loc in ["critério 2.1", "v1.2", "obj.metodo", "Foo.bar", "Foo.bar e v1.2"] {
            assert!(parse_locations(loc).is_empty(), "{loc}");
            let out = converge(Lang::PtBr, rejected(vec![medium("F-02", loc)]), &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]);
            assert_eq!(out.result.verdict, Verdict::ChangesRequested, "{loc}");
            assert_eq!(out.result.findings.len(), 1, "{loc}");
            assert!(out.observations.is_empty(), "{loc}");
        }
        assert_eq!(parse_locations("main.rs:4"), vec![("main.rs".to_string(), Some((4, 4)))]);
        assert_eq!(parse_locations("src/lib.rs"), vec![("src/lib.rs".to_string(), None)]);
    }

    #[tokio::test]
    async fn review_with_previous_findings_but_no_rejected_sha_does_not_demote() {
        let fx = Fixture::new("sem-rejected-sha", "true");
        let mut t = task("T01", TaskStatus::Todo, &[]);
        t.body.push_str(&format!("\n## {FINDINGS}\n{}", render_findings(&[finding("F-01")]).unwrap().unwrap()));
        assert!(t.frontmatter.rejected_sha.is_none());
        fx.add_task(&t);
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| {
            if n == 0 { rejected(vec![medium("F-02", "docs/intocado.md:7")]) } else { approved(vec![]) }
        });
        let rec = Recorder::default();
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(rev.inputs().len(), 2, "o MEDIUM bloqueou: sem base do diff nada é rebaixado");
        assert!(reviewer_texts(&rec)[0].starts_with("Pediu ajustes: "));
        assert!(section_text(&fx.read_task("T01").body, OBSERVATIONS).is_none());
    }

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    fn yes(_: &str) -> bool {
        true
    }

    fn labeled(f: Finding) -> Finding {
        Finding { severity: format!("{}, {UNCHANGED_FILE_LABEL}", f.severity), ..f }
    }

    /// Spec 092 T04: finding novo MEDIUM (ou LOW) em arquivo inalterado vira observação e aprova.
    #[test]
    fn rereview_new_medium_on_unchanged_file_is_observation_and_approves() {
        let changed = files(&["src/z.rs"]);
        let out = converge_with_files(Lang::PtBr, rejected(vec![medium("F-02", "src/b.rs:3")]), &[finding("F-01")], &[], Some(&changed), &yes);
        assert_eq!(out.result.verdict, Verdict::Approved);
        assert!(out.result.findings.is_empty());
        assert_eq!(out.observations, vec![labeled(medium("F-02", "src/b.rs:3"))]);
        assert!(out.result.summary.contains("não mudaram desde a revisão anterior"), "{}", out.result.summary);
        let low = Finding { severity: "LOW".into(), ..medium("F-03", "src/c.rs") };
        let out = converge_with_files(Lang::PtBr, rejected(vec![low.clone()]), &[finding("F-01")], &[], Some(&changed), &yes);
        assert_eq!(out.result.verdict, Verdict::Approved);
        assert_eq!(out.observations, vec![labeled(low)]);
        // Com um HIGH que continua bloqueando, o MEDIUM novo vira observação mesmo assim.
        let high = Finding { location: "src/z.rs:1".into(), ..finding("F-04") };
        let out = converge_with_files(Lang::PtBr, 
            rejected(vec![high.clone(), medium("F-05", "src/b.rs:3")]),
            &[finding("F-01")],
            &[hunk("src/z.rs", 1, 2)],
            Some(&changed),
            &yes,
        );
        assert_eq!(out.result.verdict, Verdict::ChangesRequested);
        assert_eq!(out.result.findings, vec![high]);
        assert_eq!(out.observations, vec![labeled(medium("F-05", "src/b.rs:3"))]);
        // Critério não manual `not_met` continua bloqueando (como na 087/T05).
        let mut unmet = rejected(vec![medium("F-02", "src/b.rs:3")]);
        unmet.criteria = vec![met("c1", false)];
        let out = converge_with_files(Lang::PtBr, unmet, &[finding("F-01")], &[], Some(&changed), &yes);
        assert_eq!(out.result.verdict, Verdict::ChangesRequested);
        assert_eq!(out.result.findings[0].location, "critérios de aceitação");
        assert_eq!(out.observations.len(), 1);
    }

    #[test]
    fn rereview_high_on_unchanged_file_blocks() {
        let r = rejected(vec![Finding { location: "src/b.rs:3".into(), ..finding("F-02") }]);
        let out = converge_with_files(Lang::PtBr, r.clone(), &[finding("F-01")], &[], Some(&files(&["src/z.rs"])), &yes);
        assert_eq!(out.result, r);
        assert!(out.observations.is_empty());
    }

    #[test]
    fn rereview_new_medium_on_changed_file_blocks() {
        let r = rejected(vec![medium("F-02", "./src/b.rs:3")]);
        let out = converge_with_files(Lang::PtBr, r.clone(), &[finding("F-01")], &[hunk("src/b.rs", 3, 3)], Some(&files(&["src/b.rs"])), &yes);
        assert_eq!(out.result, r);
        assert!(out.observations.is_empty());
        // Vários caminhos: basta um alterado para bloquear.
        let r = rejected(vec![medium("F-02", "src/a.rs:1, src/b.rs:3")]);
        let out = converge_with_files(Lang::PtBr, r.clone(), &[finding("F-01")], &[hunk("src/b.rs", 3, 3)], Some(&files(&["src/b.rs"])), &yes);
        assert_eq!(out.result, r);
    }

    #[test]
    fn rereview_reopened_finding_blocks() {
        let changed = files(&["src/z.rs"]);
        // Mesmo id.
        let r = rejected(vec![medium("F-01", "src/b.rs:3")]);
        let out = converge_with_files(Lang::PtBr, r.clone(), &[finding("F-01")], &[], Some(&changed), &yes);
        assert_eq!(out.result, r);
        // Mesmo arquivo com descrição parecida (id renumerado).
        let prev = Finding { description: "Falta validar o campo email vazio".into(), ..medium("F-01", "src/b.rs:9") };
        let again = Finding { description: "Ainda falta validar o campo email quando vazio".into(), ..medium("F-03", "src/b.rs:12") };
        let r = rejected(vec![again]);
        let out = converge_with_files(Lang::PtBr, r.clone(), &[prev.clone()], &[], Some(&changed), &yes);
        assert_eq!(out.result, r);
        assert!(out.observations.is_empty());
        // Arquivo do finding anterior que não mudou, com outra redação: o apontamento continua aberto
        // (F-01 da revisão): bloqueia, sem depender da similaridade do texto.
        let other = Finding { description: "Nome de variável enganoso no laço".into(), ..medium("F-03", "./src/b.rs:12") };
        let r = rejected(vec![other]);
        let out = converge_with_files(Lang::PtBr, r.clone(), &[prev], &[], Some(&changed), &yes);
        assert_eq!(out.result, r);
        assert!(out.observations.is_empty());
    }

    /// F-02 da revisão: regressão induzida (o texto cita um arquivo alterado) bloqueia.
    #[test]
    fn rereview_finding_citing_changed_file_blocks() {
        let changed = files(&["src/a.rs", "web/src/App.tsx"]);
        for (desc, rec, scenario) in [
            ("A mudança em src/a.rs quebrou a chamada daqui", "", ""),
            ("Chamada desatualizada", "Ajustar à nova assinatura de App.tsx.", ""),
            ("Chamada desatualizada", "", "nova assinatura em (a.rs:10) → erro de compilação"),
        ] {
            let f = Finding {
                description: desc.into(),
                recommendation: rec.into(),
                failure_scenario: scenario.into(),
                ..medium("F-02", "src/b.rs:3")
            };
            let r = rejected(vec![f]);
            let out = converge_with_files(Lang::PtBr, r.clone(), &[finding("F-01")], &[], Some(&changed), &yes);
            assert_eq!(out.result, r, "{desc} {rec} {scenario}");
            assert!(out.observations.is_empty());
        }
        // Nome ambíguo (dois alterados com o mesmo nome) ou só parecido não conta.
        let ambiguous = files(&["src/x/mod.rs", "src/y/mod.rs"]);
        let f = Finding { description: "Veja mod.rs e data.rs".into(), ..medium("F-02", "src/b.rs:3") };
        let out = converge_with_files(Lang::PtBr, rejected(vec![f]), &[finding("F-01")], &[], Some(&ambiguous), &yes);
        assert_eq!(out.result.verdict, Verdict::Approved);
        let f = Finding { description: "Veja data.rs".into(), ..medium("F-02", "src/b.rs:3") };
        let out = converge_with_files(Lang::PtBr, rejected(vec![f]), &[finding("F-01")], &[], Some(&changed), &yes);
        assert_eq!(out.result.verdict, Verdict::Approved);
    }

    /// F-02 da revisão: location num arquivo que não existe na branch (ex.: teste faltando) bloqueia.
    #[test]
    fn rereview_finding_on_missing_file_blocks() {
        let changed = files(&["src/a.rs"]);
        let r = rejected(vec![medium("F-02", "tests/faltando_test.rs:1")]);
        let exists = |p: &str| p != "tests/faltando_test.rs";
        let out = converge_with_files(Lang::PtBr, r.clone(), &[finding("F-01")], &[], Some(&changed), &exists);
        assert_eq!(out.result, r);
        assert!(out.observations.is_empty());
        // Checagem real na worktree: existente rebaixa; ausente, `..` ou worktree inexistente não.
        let dir = env::temp_dir().join(format!("nants-092t04-{}", process::id()));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/b.rs"), "x").unwrap();
        assert!(file_in_worktree(&dir, "src/b.rs"));
        assert!(!file_in_worktree(&dir, "src/nao.rs"));
        assert!(!file_in_worktree(&dir, "../src/b.rs") && !file_in_worktree(&dir, "/etc/passwd"));
        assert!(!file_in_worktree(&dir.join("sumiu"), "src/b.rs"));
        let real = |p: &str| file_in_worktree(&dir, p);
        let out = converge_with_files(Lang::PtBr, rejected(vec![medium("F-02", "src/b.rs:3")]), &[finding("F-01")], &[], Some(&changed), &real);
        assert_eq!(out.result.verdict, Verdict::Approved);
        let r = rejected(vec![medium("F-02", "src/nao.rs:3")]);
        assert_eq!(converge_with_files(Lang::PtBr, r.clone(), &[finding("F-01")], &[], Some(&changed), &real).result, r);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rereview_never_demotes_unlocated_or_image_locations() {
        let changed = files(&["src/z.rs"]);
        for loc in ["estrutura geral do módulo", "captura t01-1-home.png", "public/img/hero.webp", "referência a.png × captura b.png"] {
            let r = rejected(vec![medium("F-02", loc)]);
            let out = converge_with_files(Lang::PtBr, r.clone(), &[finding("F-01")], &[], Some(&changed), &yes);
            assert_eq!(out.result, r, "{loc}");
            assert!(out.observations.is_empty(), "{loc}");
        }
    }

    #[test]
    fn first_review_never_demotes_by_unchanged_file() {
        // 1ª revisão: sem `rejected_sha` não há lista de arquivos alterados.
        let r = rejected(vec![medium("F-01", "src/b.rs:3")]);
        let out = converge_with_files(Lang::PtBr, r.clone(), &[], &[], None, &yes);
        assert_eq!(out.result, r);
        assert!(out.observations.is_empty());
    }

    #[test]
    fn changed_files_facts_lists_paths_up_to_fifty() {
        let list: Vec<String> = (0..52).map(|i| format!("src/{i}.rs")).collect();
        let text = changed_files_facts(&list);
        assert_eq!(text.lines().filter(|l| l.starts_with("- ")).count(), CHANGED_FILES_MAX);
        assert!(text.ends_with("… e mais 2\n"), "{text}");
        assert!(changed_files_facts(&[]).contains("(nenhum)"));
    }

    #[test]
    fn converge_first_review_is_unchanged_and_approved_findings_become_observations() {
        let r = rejected(vec![medium("F-01", "src/b.rs:3"), finding("F-02")]);
        let out = converge(Lang::PtBr, r.clone(), &[], &[]);
        assert_eq!(out.result, r);
        assert!(out.observations.is_empty());
        let out = converge(Lang::PtBr, approved(vec![medium("F-01", "src/b.rs:3")]), &[], &[]);
        assert_eq!(out.result.verdict, Verdict::Approved);
        assert!(out.result.findings.is_empty());
        assert_eq!(out.observations, vec![medium("F-01", "src/b.rs:3")]);
    }

    #[test]
    fn parse_hunks_and_locations_read_ranges_and_paths() {
        let diff = "diff --git a/x b/x\n@@ -1,2 +3,4 @@ fn a\n+l\n@@ -10 +20 @@\n@@ -5,3 +7,0 @@\n";
        assert_eq!(parse_hunks("x", diff), vec![hunk("x", 3, 6), hunk("x", 20, 20), hunk("x", 7, 7)]);
        assert_eq!(parse_locations("src/a.rs:10"), vec![("src/a.rs".to_string(), Some((10, 10)))]);
        assert_eq!(parse_locations("`src/a.rs:10-20`."), vec![("src/a.rs".to_string(), Some((10, 20)))]);
        assert_eq!(parse_locations("src/a.rs:5:3, web/b.tsx"), vec![("src/a.rs".to_string(), Some((5, 5))), ("web/b.tsx".to_string(), None)]);
        assert!(parse_locations("critérios de aceitação").is_empty());
        assert!(same_file_path("./src/a.rs", "src/a.rs") && same_file_path("x/src/a.rs", "src/a.rs"));
        assert!(!same_file_path("src/a.rs", "src/ab.rs") && !same_file_path("", ""));
    }

    #[test]
    fn observations_are_appended_deduplicated_and_bounded() {
        let first = append_observations("", &[medium("F-02", "src/b.rs:3")], &no_redact).expect("seção");
        assert_eq!(first, "- F-02 [MEDIUM] src/b.rs:3: descrição");
        let again = append_observations(&first, &[medium("F-02", "src/b.rs:3"), medium("F-03", "src/c.rs:1")], &no_redact).expect("seção");
        assert_eq!(again.lines().count(), 2, "{again}");
        assert_eq!(append_observations("", &[], &no_redact), None);
        let many: Vec<Finding> = (0..100).map(|n| medium(&format!("F-{n:02}"), "src/d.rs:1")).collect();
        let bounded = append_observations("", &many, &no_redact).expect("seção");
        assert!(bounded.len() <= OBSERVATIONS_MAX_BYTES + 120 && bounded.contains("F-99"), "{}", bounded.len());
    }

    fn reviewer_texts(rec: &Recorder) -> Vec<String> {
        rec.events()
            .into_iter()
            .filter_map(|ev| match ev {
                RunnerEvent::AgentMessage { agent, text, .. } if agent == "reviewer" => Some(text),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn rereview_medium_on_untouched_code_becomes_observation_and_approves() {
        let fx = Fixture::new("convergencia", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| {
            if n == 0 {
                rejected(vec![finding("F-01")])
            } else {
                rejected(vec![medium("F-02", "docs/intocado.md:7")])
            }
        });
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Done);
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.attempts, 1, "a observação não conta como reprovação");
        assert_eq!(rev.inputs().len(), 2);
        let obs = section_text(&t.body, OBSERVATIONS).expect("observações registradas");
        assert!(obs.contains("F-02") && obs.contains("docs/intocado.md:7"), "{obs}");
        let texts = reviewer_texts(&rec);
        assert!(texts[0].starts_with("Pediu ajustes: ") && texts[0].contains("Apontamentos:\n- F-01 [HIGH] src/lib.rs:1"), "{}", texts[0]);
        assert!(texts[1].starts_with("Aprovado: ") && texts[1].contains("Observações (não bloqueiam):\n- F-02"), "{}", texts[1]);
    }

    #[tokio::test]
    async fn rereview_medium_in_changed_file_still_requests_changes() {
        let fx = Fixture::new("convergencia-bloqueia", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        // O Desenvolvedor falso cria out-T01-<n>.txt; o finding anterior aponta o mesmo arquivo.
        let rev = FakeReviewer::new(|n, _| match n {
            0 => rejected(vec![Finding { location: "out-T01-1.txt:1".into(), ..finding("F-01") }]),
            1 => rejected(vec![medium("F-02", "out-T01-1.txt:9")]),
            _ => approved(vec![]),
        });
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(rev.inputs().len(), 3, "o MEDIUM no mesmo arquivo do finding anterior bloqueou");
        assert_eq!(fx.read_task("T01").frontmatter.attempts, 2);
    }

    #[tokio::test]
    async fn first_review_approved_with_medium_records_observation() {
        let fx = Fixture::new("aprovado-com-observacao", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![medium("F-01", "src/x.rs:2")]));
        let rec = Recorder::default();
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        let t = fx.read_task("T01");
        assert!(section_text(&t.body, OBSERVATIONS).expect("seção").contains("F-01"));
        assert!(reviewer_texts(&rec)[0].contains("Observações (não bloqueiam):\n- F-01 [MEDIUM] src/x.rs:2"));
    }

    /// Desenvolvedor que "pergunta ao Arquiteto" na 1ª chamada (amplia o escopo em disco).
    #[derive(Clone)]
    struct AskingDev {
        inner: FakeDev,
        editor: TaskEditor,
    }

    impl DeveloperPort for AskingDev {
        fn implement<'a>(&'a self, brief: &'a DevBrief) -> Fut<'a, Result<DevReport, RunnerError>> {
            Box::pin(async move {
                if self.inner.briefs().is_empty() {
                    self.editor
                        .apply_advice(&brief.spec_id, &brief.task_id, "### Pergunta (developer)\nq\n\nResposta: a", &["deps.edn".into()], None)
                        .unwrap();
                }
                self.inner.implement(brief).await
            })
        }
    }

    #[tokio::test]
    async fn advice_edits_survive_runner_persist_and_reach_next_review() {
        let fx = Fixture::new("conselho", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let baselines: SharedBaselines = Default::default();
        let editor = TaskEditor::new(fx.repo.clone(), Arc::clone(&baselines));
        let dev = AskingDev { inner: FakeDev::default(), editor: editor.clone() };
        let ed = editor.clone();
        let rev = FakeReviewer::new(move |n, _| {
            if n == 0 {
                ed.apply_advice(SPEC, "T01", "### Pergunta (reviewer)\nq\n\nResposta: a", &["../x".into()], Some("- critério novo"))
                    .unwrap();
                rejected(vec![finding("F-01")])
            } else {
                approved(vec![])
            }
        });
        let rec = Recorder::default();
        let runner = TaskRunner::new(
            fx.repo.clone(),
            Box::new(dev.clone()),
            Box::new(rev.clone()),
            Box::new(ScriptedTests::default()),
            Box::new(rec.clone()),
        )
        .with_baselines(baselines);
        let outcome = runner.run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
        let inputs = rev.inputs();
        assert!(inputs[0].acceptance.contains("arquivo criado"));
        assert!(inputs[1].acceptance.contains("critério novo") && !inputs[1].acceptance.contains("arquivo criado"));
        assert!(!fx.read_task("T01").frontmatter.allowed_scope.iter().any(|g| g.contains("..")));
        let briefs = dev.inner.briefs();
        assert!(briefs[0].allowed_scope == vec!["src/**"], "1º brief anterior à pergunta");
        assert!(briefs[1].allowed_scope.contains(&"deps.edn".to_string()), "escopo ampliado vale depois");
        assert!(briefs[1].acceptance.contains("critério novo"));
    }

    #[test]
    fn task_editor_records_section_and_rejects_bad_acceptance() {
        let fx = Fixture::new("editor", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let editor = TaskEditor::new(fx.repo.clone(), Default::default());
        let (added, rewritten) = editor
            .apply_advice(SPEC, "T01", "### Pergunta (developer)\nq\n\nResposta: a", &["a.txt".into(), "a.txt".into()], Some("## cabeçalho"))
            .unwrap();
        assert_eq!(added, vec!["a.txt"]);
        assert!(!rewritten, "critério com cabeçalho é recusado");
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.allowed_scope, vec!["src/**", "a.txt"]);
        assert!(acceptance_of(&t.body).contains("arquivo criado"));
        assert!(section_text(&t.body, ADVICE_SECTION).unwrap().contains("Resposta: a"));
    }

    #[tokio::test]
    async fn agent_messages_carry_task_and_current_attempt() {
        let fx = Fixture::new("mensagens-tarefa", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| {
            if n == 0 {
                rejected(vec![finding("F-01")])
            } else {
                approved(vec![])
            }
        });
        let rec = Recorder::default();
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        let seen: Vec<(String, Option<String>, Option<u32>)> = rec
            .events()
            .into_iter()
            .filter_map(|ev| match ev {
                RunnerEvent::AgentMessage {
                    agent,
                    task_id,
                    attempt,
                    ..
                } => Some((agent, task_id, attempt)),
                _ => None,
            })
            .collect();
        let dev_attempts: Vec<_> = seen
            .iter()
            .filter(|(a, _, _)| a == "developer")
            .map(|(_, t, n)| (t.clone(), *n))
            .collect();
        assert_eq!(
            dev_attempts,
            vec![(Some("T01".into()), Some(1)), (Some("T01".into()), Some(2))]
        );
        let rev_attempts: Vec<_> = seen
            .iter()
            .filter(|(a, _, _)| a == "reviewer")
            .map(|(_, t, n)| (t.clone(), *n))
            .collect();
        assert_eq!(
            rev_attempts,
            vec![(Some("T01".into()), Some(1)), (Some("T01".into()), Some(2))]
        );
    }

    #[tokio::test]
    async fn third_rejection_blocks_without_merge() {
        let fx = Fixture::new("terceira", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let head_before = fx.head();
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| rejected(vec![finding("F-01")]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Blocked {
                reason: "3 reprovações do revisor".into(),
                kind: BlockKind::Quality,
                open_findings: vec![finding("F-01")],
            }
        );
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::Blocked);
        assert_eq!(t.frontmatter.attempts, 3);
        assert_eq!(dev.briefs().len(), 3);
        assert_eq!(rev.inputs().len(), 3);
        assert_eq!(fx.head(), head_before, "base intocada");
        assert_eq!(fx.feat_commits(), 0);
        assert!(fx.worktree("T01").is_dir(), "worktree preservada para análise");
    }

    /// Spec 087 T05: cada revisão registra as linhas de código do diff da tentativa.
    #[tokio::test]
    async fn each_reviewed_attempt_records_code_diff_lines() {
        let fx = Fixture::new("diff-linhas", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| rejected(vec![finding("F-01")]));
        let rec = Recorder::default();
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::Blocked);
        // O Desenvolvedor falso cria um arquivo de uma linha por tentativa.
        assert_eq!(t.frontmatter.diff_lines, vec![1, 2, 3]);
        assert!(t.render().unwrap().contains("diff_lines:"));
    }

    #[test]
    fn test_and_doc_paths_are_excluded_from_code_lines() {
        for path in [
            "README.md",
            "docs/Guia.MD",
            "tests/e2e.rs",
            "web/src/__tests__/a.tsx",
            "src/a.test.ts",
            "src/b.spec.tsx",
            "spec/models/user_spec.rb",
            "src/server/recovery_tests.rs",
            "pkg/handler_test.go",
            "test_utils.py",
        ] {
            assert!(is_test_or_doc(path), "{path}");
        }
        for path in ["src/main.rs", "src/latest.rs", "web/src/App.tsx", "Cargo.toml", "src/contest/mod.rs", ""] {
            assert!(!is_test_or_doc(path), "{path}");
        }
    }

    #[test]
    fn diff_lines_keep_first_attempt_within_cap() {
        let mut list = Vec::new();
        for n in 0..(MAX_DIFF_LINES_ENTRIES as u64 + 5) {
            push_diff_lines(&mut list, n);
        }
        assert_eq!(list.len(), MAX_DIFF_LINES_ENTRIES);
        assert_eq!(list.first(), Some(&0), "1ª tentativa preservada");
        assert_eq!(list.last(), Some(&(MAX_DIFF_LINES_ENTRIES as u64 + 4)));
    }

    /// Spec 115 T03: avisos da tarefa com o texto de reserva `system.<code>` do idioma do projeto.
    #[test]
    fn task_notices_use_the_system_text_of_each_language() {
        let cleanup = TaskWarning::Cleanup { path: "/w".into(), error: "e".into() };
        let ownership = TaskWarning::Ownership { path: "/w".into(), error: "e".into() };
        let (pt, code, params) = cleanup.notice(Lang::PtBr);
        assert_eq!(code, "task.cleanup_failed");
        assert_eq!(params, notice_params(&[("path", "/w"), ("error", "e")]));
        assert!(pt.starts_with("Aviso: não consegui apagar a worktree /w: e. A tarefa está integrada"), "{pt}");
        let (en, _, _) = cleanup.notice(Lang::En);
        assert!(en.starts_with("Warning: could not delete the worktree /w: e. The task is integrated"), "{en}");
        let (pt, code, _) = ownership.notice(Lang::PtBr);
        assert_eq!((pt.as_str(), code), ("Aviso: não consegui devolver a posse dos arquivos de /w: e", "task.ownership_failed"));
        let (en, _, _) = ownership.notice(Lang::En);
        assert_eq!(en, "Warning: could not give back ownership of the files in /w: e");
        let dirty = |lang| system_text(lang, "task.worktree_dirty", &[("task", "T01"), ("path", "/w")]);
        assert!(dirty(Lang::PtBr).starts_with("Tarefa T01 bloqueada: a worktree /w tem alterações"));
        assert!(dirty(Lang::En).starts_with("Task T01 blocked: the worktree /w has uncommitted changes"));
        assert_no_pt(&[dirty(Lang::En), en]);
    }

    /// Spec 115 T03: findings gerados e notas da convergência no idioma do projeto; o local gerado
    /// nunca parece um caminho, nos dois idiomas.
    #[test]
    fn generated_findings_and_convergence_notes_follow_the_project_language() {
        let unmet = |evidence: &str| ReviewResult {
            criteria: vec![CriterionCheck { criterion: "c1".into(), met: false, evidence: evidence.into() }],
            ..approved(vec![])
        };
        for (lang, evidence, location, description, recommendation) in [
            (Lang::PtBr, "", "critérios de aceitação", "Critério não atendido: c1. Evidência: (nenhuma)", "Atender o critério com código e teste que o exercitem"),
            (Lang::En, "seen", "acceptance criteria", "Criterion not met: c1. Evidence: seen", "Meet the criterion with code and a test that exercises it"),
        ] {
            let out = enforce_criteria(lang, unmet(evidence));
            assert_eq!(out.verdict, Verdict::ChangesRequested);
            let f = &out.findings[0];
            assert_eq!((f.location.as_str(), f.description.as_str(), f.recommendation.as_str()), (location, description, recommendation));
            assert!(parse_locations(&f.location).is_empty(), "{lang:?}");
        }
        let low = || Finding { confidence: Confidence::Low, ..finding("F-01") };
        let untouched = || rejected(vec![medium("F-02", "src/b.rs:3")]);
        let changed = vec!["src/z.rs".to_string()];
        let yes = |_: &str| true;
        for (lang, low_note, untouched_note, unchanged_note) in [
            (
                Lang::PtBr,
                "(findings de baixa confiança viraram observações.)",
                "(findings MEDIUM sobre código não alterado desde a última reprovação viraram observações.)",
                "(findings novos em arquivos que não mudaram desde a revisão anterior viraram observações.)",
            ),
            (
                Lang::En,
                "(low confidence findings became observations.)",
                "(MEDIUM findings on code unchanged since the last rejection became observations.)",
                "(new findings in files that did not change since the previous review became observations.)",
            ),
        ] {
            let summary = converge(lang, rejected(vec![low()]), &[], &[]).result.summary;
            assert!(summary.ends_with(low_note), "{summary}");
            let summary = converge(lang, untouched(), &[finding("F-01")], &[hunk("src/z.rs", 1, 2)]).result.summary;
            assert!(summary.ends_with(untouched_note), "{summary}");
            let summary = converge_with_files(lang, untouched(), &[finding("F-01")], &[], Some(&changed), &yes).result.summary;
            assert!(summary.ends_with(unchanged_note), "{summary}");
            if lang == Lang::En {
                assert_no_pt(&[summary]);
            }
        }
    }

    #[test]
    fn low_confidence_findings_do_not_block_even_in_first_review() {
        let low = Finding { confidence: Confidence::Low, ..finding("F-01") };
        let Converged { result, observations } = converge(Lang::PtBr, rejected(vec![low.clone()]), &[], &[]);
        assert_eq!(result.verdict, Verdict::Approved);
        assert!(result.findings.is_empty());
        assert!(result.summary.contains("baixa confiança"), "{}", result.summary);
        assert_eq!(observations, vec![low.clone()]);

        // Com outro finding bloqueante, a reprovação segue só com ele; o low vira observação.
        let Converged { result, observations } = converge(Lang::PtBr, rejected(vec![low.clone(), finding("F-02")]), &[], &[]);
        assert_eq!(result.verdict, Verdict::ChangesRequested);
        assert_eq!(result.findings.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), vec!["F-02"]);
        assert_eq!(observations, vec![low.clone()]);

        // Critério not_met continua bloqueando, com finding gerado.
        let mut with_unmet = rejected(vec![low.clone()]);
        with_unmet.criteria = vec![met("c1", false)];
        let Converged { result, observations } = converge(Lang::PtBr, with_unmet, &[], &[]);
        assert_eq!(result.verdict, Verdict::ChangesRequested);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].location, "critérios de aceitação");
        assert_eq!(observations, vec![low]);
    }

    #[test]
    fn review_message_shows_failure_scenario_and_low_confidence() {
        let high = Finding { failure_scenario: "lista vazia → pânico\nno índice 0".into(), ..finding("F-01") };
        let low = Finding { confidence: Confidence::Low, ..finding("F-02") };
        let text = render_review_message(Verdict::ChangesRequested, "r", &[high.clone()], &[low.clone()], &[], &[], &no_redact);
        assert!(
            text.contains("\n- F-01 [HIGH] src/lib.rs:1: descrição → recomendação\n  Cenário de falha: lista vazia → pânico no índice 0"),
            "{text}"
        );
        assert!(text.contains("- F-02 [HIGH, baixa confiança] src/lib.rs:1: descrição"), "{text}");
        // Sem cenário, nenhuma linha extra.
        let plain = render_review_message(Verdict::ChangesRequested, "r", &[finding("F-03")], &[], &[], &[], &no_redact);
        assert!(!plain.contains(SCENARIO_PREFIX), "{plain}");
        // A seção de observações da tarefa guarda uma linha por finding, sem o cenário.
        let section = append_observations("", &[high], &no_redact).unwrap();
        assert_eq!(section.lines().count(), 1, "{section}");
    }

    #[test]
    fn findings_with_scenario_round_trip_and_old_yaml_still_parses() {
        let f = Finding { failure_scenario: "x → y".into(), confidence: Confidence::Low, ..finding("F-01") };
        let yaml = render_findings(&[f.clone()]).unwrap().unwrap();
        assert!(yaml.contains("failure_scenario") && yaml.contains("confidence: low"), "{yaml}");
        assert_eq!(serde_yaml::from_str::<Vec<Finding>>(&yaml).unwrap(), vec![f]);
        let old = render_findings(&[finding("F-02")]).unwrap().unwrap();
        assert!(!old.contains("failure_scenario") && !old.contains("confidence"), "{old}");
        let parsed = serde_yaml::from_str::<Vec<Finding>>(&old).unwrap();
        assert_eq!(parsed[0].confidence, Confidence::High);
        assert!(parsed[0].failure_scenario.is_empty());
    }

    #[tokio::test]
    async fn unchanged_branch_between_rejections_blocks_as_impasse() {
        let fx = Fixture::new("impasse", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev { idle_after_first: true, ..FakeDev::default() };
        let rev = FakeReviewer::new(|_, _| rejected(vec![finding("F-01")]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        match outcome {
            Outcome::Blocked { reason, kind, .. } => {
                assert_eq!(kind, BlockKind::Quality);
                assert!(reason.starts_with("impasse: o Desenvolvedor não alterou a branch"), "{reason}");
            }
            other => panic!("esperava Blocked: {other:?}"),
        }
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::Blocked);
        assert_eq!(t.frontmatter.attempts, 2);
        assert!(t.frontmatter.rejected_sha.is_some());
    }

    #[tokio::test]
    async fn second_attempt_gets_history_and_second_review_gets_prior_findings_and_incremental_diff() {
        let fx = Fixture::new("memoria", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| if n == 0 { rejected(vec![finding("F-01")]) } else { approved(vec![]) });
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Done);
        let briefs = dev.briefs();
        assert_eq!(briefs.len(), 2);
        assert!(briefs[0].history.is_empty());
        assert!(briefs[1].history.contains("### Tentativa 1"), "{}", briefs[1].history);
        assert!(briefs[1].history.contains("dev 1"));
        assert!(briefs[1].history.contains("out-T01-1.txt"), "diff --stat: {}", briefs[1].history);
        let inputs = rev.inputs();
        assert!(inputs[0].prior_findings.is_empty() && inputs[0].incremental_diff.is_empty());
        assert!(inputs[1].prior_findings.contains("F-01"));
        assert!(inputs[1].incremental_diff.contains("out-T01-2.txt"), "{}", inputs[1].incremental_diff);
        assert!(!inputs[1].incremental_diff.contains("out-T01-1.txt\n+"), "só o que mudou desde a reprovação");
        // Spec 092 T04: os FATOS da re-revisão trazem os arquivos alterados desde a revisão anterior.
        assert_eq!(inputs[0].changed_files, None);
        assert_eq!(inputs[1].changed_files, Some(vec!["out-T01-2.txt".to_string()]));
    }

    /// Spec 092 T04: na re-revisão, o finding novo MEDIUM num arquivo que não mudou desde a reprovação
    /// vira observação rotulada e a tarefa é aprovada.
    #[tokio::test]
    async fn rereview_new_medium_on_unchanged_file_becomes_observation_and_approves() {
        let fx = Fixture::new("arquivo-inalterado", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| {
            if n == 0 { rejected(vec![finding("F-01")]) } else { rejected(vec![medium("F-07", "out-T01-1.txt:1")]) }
        });
        let rec = Recorder::default();
        let outcome = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(rev.inputs().len(), 2);
        let texts = reviewer_texts(&rec);
        assert!(texts[1].starts_with("Aprovado: "), "{}", texts[1]);
        assert!(texts[1].contains(&format!("- F-07 [MEDIUM, {UNCHANGED_FILE_LABEL}] out-T01-1.txt:1: descrição")), "{}", texts[1]);
        let obs = section_text(&fx.read_task("T01").body, OBSERVATIONS).unwrap_or_default();
        assert!(obs.contains(UNCHANGED_FILE_LABEL), "{obs}");
    }

    #[test]
    fn append_history_keeps_recent_within_limit() {
        let big = |n: usize| format!("### Tentativa {n}\n{}\n", "x".repeat(300));
        let mut text = String::new();
        for n in 1..=10 {
            text = append_history(&text, &big(n), 1000);
        }
        assert!(text.len() <= 1000, "{}", text.len());
        assert!(text.contains("### Tentativa 10"));
        assert!(!text.contains("### Tentativa 1\n"));
        let one = append_history("", &"y".repeat(5000), 1000);
        assert!(one.len() <= 1000 + "\n[truncado]".len());
    }

    struct LeakyDev;

    impl DeveloperPort for LeakyDev {
        fn implement<'a>(&'a self, brief: &'a DevBrief) -> Fut<'a, Result<DevReport, RunnerError>> {
            Box::pin(async move {
                fs::write(brief.worktree.join("sk-SEGREDO-123456.txt"), "x\n").unwrap();
                Ok(DevReport { summary: "usei sk-SEGREDO-123456 aqui".into(), exhausted: false })
            })
        }
    }

    #[test]
    fn attempt_numbers_stay_unique_and_increasing_after_truncation() {
        let mut text = String::new();
        let mut last = 0;
        for _ in 0..12 {
            let n = next_attempt_number(&text);
            assert!(n > last, "{n} <= {last}");
            last = n;
            text = append_history(&text, &format!("### Tentativa {n}\n{}\n", "x".repeat(300)), 1000);
        }
        assert_eq!(last, 12);
    }

    #[tokio::test]
    async fn exhausted_session_continues_flow_and_next_session_gets_progress() {
        let fx = Fixture::new("progresso", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let secrets = Arc::new(crate::server::events::SecretSet::new(vec!["sk-SEGREDO-123456".to_string()]));
        let dev = FakeDev { exhaust_first: true, ..FakeDev::default() };
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let runner = TaskRunner::new(
            fx.repo.clone(),
            Box::new(dev.clone()),
            Box::new(rev.clone()),
            Box::new(ScriptedTests::new(vec![false, true])),
            Box::new(rec.clone()),
        )
        .with_secrets(secrets);
        // Estouro → commit → testes (falham) → próxima sessão com o progresso → testes → revisão.
        let outcome = runner.run_task(SPEC, "T01").await.expect("sem Err no estouro");
        assert_eq!(outcome, Outcome::Done);
        let briefs = dev.briefs();
        assert_eq!(briefs.len(), 2);
        assert!(briefs[0].previous_progress.is_empty());
        let progress = &briefs[1].previous_progress;
        assert!(progress.contains("fiz metade"), "{progress}");
        assert!(progress.contains("out-T01-1.txt"), "diff --stat: {progress}");
        assert!(!progress.contains("sk-SEGREDO-123456"), "redigido: {progress}");
        assert!(progress.len() <= 3 * 1024 + 128, "{} bytes", progress.len());
        assert!(briefs[1].test_output.is_some(), "a saída do teste também segue");
        assert_eq!(rev.inputs().len(), 1);
    }

    #[tokio::test]
    async fn non_exhausted_session_does_not_pass_progress() {
        let fx = Fixture::new("sem-progresso", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::new(vec![false, true]), &rec)
            .run_task(SPEC, "T01")
            .await
            .expect("ok");
        assert_eq!(outcome, Outcome::Done);
        assert!(dev.briefs().iter().all(|b| b.previous_progress.is_empty()));
    }

    #[tokio::test]
    async fn history_is_scrubbed_and_preserved_across_runs() {
        let fx = Fixture::new("memoria-segredo", "true");
        let mut t = task("T01", TaskStatus::Todo, &[]);
        t.body = replace_section(&t.body, HISTORY, Some("### Tentativa 1\nantigo".into()));
        fx.add_task(&t);
        let secrets = Arc::new(crate::server::events::SecretSet::new(vec!["sk-SEGREDO-123456".to_string()]));
        let rev = FakeReviewer::new(|_, _| rejected(vec![finding("F-01")]));
        let rec = Recorder::default();
        let runner = TaskRunner::new(
            fx.repo.clone(),
            Box::new(LeakyDev),
            Box::new(rev.clone()),
            Box::new(ScriptedTests::default()),
            Box::new(rec.clone()),
        )
        .with_secrets(secrets);
        let _ = runner.run_task(SPEC, "T01").await;
        let body = fx.read_task("T01").body;
        let h = section_text(&body, HISTORY).unwrap();
        assert!(h.contains("antigo") && h.contains("### Tentativa 2"), "{h}");
        assert!(!h.contains("sk-SEGREDO-123456"), "{h}");
    }

    #[test]
    fn test_summary_keeps_first_and_last_lines_within_limit() {
        assert_eq!(test_summary(""), "");
        assert_eq!(test_summary("\n  \n"), "");
        let out = "running 3 tests\n\nt1 ok\nt2 ok\n\nt3 ok\ntest result: ok. 3 passed\n\n";
        let s = test_summary(out);
        assert!(s.starts_with("running 3 tests\n"), "{s}");
        assert!(s.ends_with("test result: ok. 3 passed"), "{s}");
        assert!(!s.contains("\n\n"), "{s}");
        let big = format!("inicio\n{}\nfim", "x".repeat(2000));
        let s = test_summary(&big);
        assert!(s.len() <= TEST_SUMMARY_MAX_BYTES, "{}", s.len());
        assert!(s.starts_with("inicio"), "{s}");
    }

    #[test]
    fn test_summary_never_persists_secret_prefix_crossing_the_cut() {
        let fx = Fixture::new("resumo-segredo", "true");
        let secret = "sk-SEGREDO-123456";
        let runner = TaskRunner::new(
            fx.repo.clone(),
            Box::new(FakeDev::default()),
            Box::new(FakeReviewer::new(|_, _| approved(vec![]))),
            Box::new(ScriptedTests::default()),
            Box::new(Recorder::default()),
        )
        .with_secrets(Arc::new(crate::server::events::SecretSet::new(vec![secret.to_string()])));
        // Segredo na última linha, atravessando o byte 500 do texto bruto.
        let out = format!("inicio\n{}{secret}\n", "y".repeat(490));
        let s = test_summary(&runner.scrub(&out));
        assert!(s.len() <= TEST_SUMMARY_MAX_BYTES, "{}", s.len());
        assert!(!s.contains("sk-"), "{s}");
        assert!(!s.contains("SEGRE"), "{s}");
    }

    #[test]
    fn frontmatter_without_test_run_parses_and_roundtrips() {
        let t = task("T01", TaskStatus::Todo, &[]);
        assert!(t.frontmatter.test_run.is_none());
        assert!(!t.render().unwrap().contains("test_run"));
        let mut t2 = t.clone();
        t2.frontmatter.test_run = Some(TestRun {
            command: "cargo test".into(),
            exit_code: Some(0),
            at: "2026-10-16T00:00:00Z".into(),
            summary: "ok".into(),
        });
        let raw = t2.render().unwrap();
        assert!(raw.contains("test_run"), "{raw}");
        assert_eq!(Task::parse(&raw).unwrap(), t2);
    }

    #[test]
    fn frontmatter_without_rejected_sha_parses() {
        let t = task("T01", TaskStatus::Todo, &[]);
        assert!(t.frontmatter.rejected_sha.is_none());
        let raw = t.render().unwrap();
        assert!(!raw.contains("rejected_sha"));
    }

    #[tokio::test]
    async fn failing_tests_never_reach_review() {
        let fx = Fixture::new("teste-falha", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let tests = ScriptedTests::new(vec![false, true]);
        let outcome = fx
            .runner(&dev, &rev, tests, &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done);
        let briefs = dev.briefs();
        assert_eq!(briefs.len(), 2, "dev reenviado após falha de teste");
        assert_eq!(briefs[1].test_output.as_deref(), Some("falha 1"));
        let inputs = rev.inputs();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].test_output, "$ true\nok 2", "revisor recebe só a saída verde, com o comando");
        assert_eq!(
            rec.statuses("T01"),
            vec![
                TaskStatus::InProgress,
                TaskStatus::InReview,
                TaskStatus::Approved,
                TaskStatus::Integrating,
                TaskStatus::Done
            ]
        );
    }

    #[tokio::test]
    async fn real_failing_test_command_blocks_after_three_cycles() {
        let fx = Fixture::new("teste-real", "false");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ShellTestExecutor::new(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Blocked { .. }));
        assert_eq!(dev.briefs().len(), MAX_TEST_CYCLES as usize);
        assert!(rev.inputs().is_empty(), "revisor nunca chamado");
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
        assert_eq!(fx.feat_commits(), 0);
        let evidence = fx.read_task("T01").frontmatter.test_evidence.unwrap_or_default();
        assert!(evidence.starts_with(TEST_FAILURE_MARK), "falha registrada: {evidence:?}");
    }

    fn enable_docker(fx: &Fixture) {
        fs::create_dir_all(fx.repo.join(".nants")).unwrap();
        fs::write(
            fx.repo.join(".nants/compose.yaml"),
            "services:\n  workspace:\n    image: alpine:3\n    command: sleep infinity\n  db:\n    image: alpine:3\n",
        )
        .unwrap();
        let mut settings = read_settings(&fx.settings_path()).unwrap();
        settings.environment = Environment::Docker;
        settings.checks = vec![check("build", "make build", true)];
        crate::sdd::write_settings(&fx.settings_path(), &settings).unwrap();
        fx.commit_all("chore: ambiente docker");
    }

    #[tokio::test]
    async fn docker_mode_runs_tests_and_checks_in_compose_with_clean_state() {
        use crate::harness::compose::fake::FakeRunner;
        let fx = Fixture::new("docker-ok", "make test");
        enable_docker(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let fake = Arc::new(FakeRunner::default());
        let (dev, rev, rec) = (FakeDev::default(), base_moving_reviewer(fx.repo.clone()), Recorder::default());
        // O executor do host falharia qualquer comando: prova que nada roda no host.
        let runner = fx
            .runner(&dev, &rev, ShellTestExecutor::with_timeout(Duration::from_secs(5)), &rec)
            .with_compose(Arc::new(crate::harness::compose::fake::test_env("rok", fake.clone())));
        assert_eq!(runner.run_task(SPEC, "T01").await.unwrap(), Outcome::Done);
        let subs = fake.subcommands();
        // worktree: down, build, up, run(test), run(check); base: down, up, run(test), run(check)
        assert_eq!(subs.iter().filter(|s| *s == "down").count(), 2, "{subs:?}");
        assert_eq!(subs.iter().filter(|s| *s == "run").count(), 4, "{subs:?}");
        assert_eq!(subs[0], "down");
        assert_eq!(subs[1], "up", "sem build: workspace usa image");
        assert_eq!(subs[2], "run");
        let calls = fake.calls.lock().unwrap();
        assert!(calls[2].args.ends_with(&["--entrypoint".into(), "sh".into(), "workspace".into(), "-lc".into(), "make test".into()]));
        assert!(calls[3].args.ends_with(&["--entrypoint".into(), "sh".into(), "workspace".into(), "-lc".into(), "make build".into()]));
        assert_eq!(calls[4].args[7], "down", "estado limpo antes do test_command na base");
    }

    #[tokio::test]
    async fn docker_failure_blocks_as_environmental() {
        use crate::harness::compose::fake::FakeRunner;
        let fx = Fixture::new("docker-down", "make test");
        enable_docker(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let fake = Arc::new(FakeRunner::default());
        fake.reply("down", 1, "Cannot connect to the Docker daemon at unix:///var/run/docker.sock");
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let runner = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_compose(Arc::new(crate::harness::compose::fake::test_env("rdown", fake)));
        match runner.run_task(SPEC, "T01").await.unwrap() {
            Outcome::Blocked { reason, kind, .. } => {
                assert_eq!(kind, BlockKind::Environmental);
                assert!(reason.contains("daemon do Docker"), "{reason}");
            }
            other => panic!("esperava Blocked: {other:?}"),
        }
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
        assert!(rev.inputs().is_empty());
    }

    /// Spec 115 T04: no modo Docker, a marca de tempo limite da saída dos testes sai no idioma do
    /// projeto (o executor do Compose devolve a saída crua e o `timed_out`).
    #[tokio::test]
    async fn docker_timeout_mark_follows_the_project_language() {
        use crate::harness::compose::fake::FakeRunner;
        let secs = TEST_TIMEOUT.as_secs();
        for (lang, name, mark) in [
            (Lang::En, "docker-timeout-en", format!("[time limit of {secs}s exceeded]")),
            (Lang::PtBr, "docker-timeout-pt", format!("[tempo limite de {secs}s excedido]")),
        ] {
            let fx = Fixture::new(name, "make test");
            enable_docker(&fx);
            let fake = Arc::new(FakeRunner::default());
            fake.reply_timeout("run", "compilando\n");
            let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
            let runner = fx
                .runner(&dev, &rev, ScriptedTests::default(), &rec)
                .with_compose(Arc::new(crate::harness::compose::fake::test_env(name, fake)))
                .with_lang(i18n::SharedLang::new(lang));
            let settings = read_settings(&fx.settings_path()).unwrap();
            let out = runner.exec_cmd(&settings, &fx.repo, "make test", false).await.unwrap();
            assert!(out.timed_out && !out.passed);
            assert_eq!(out.output, format!("compilando\n\n{mark}\n"));
        }
    }

    struct Over;

    impl BudgetGuard for Over {
        fn exceeded(&self, _spec: &str, _lang: crate::i18n::Lang) -> Option<String> {
            Some("orçamento da spec atingido: 10 de 5 tokens".into())
        }
    }

    #[tokio::test]
    async fn final_verification_over_budget_skips_reviewer_and_completes_with_warning() {
        let fx = Fixture::new("final-budget", "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        let out = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_budget(Arc::new(Over))
            .run_spec(SPEC)
            .await
            .unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        assert!(rev.inputs().iter().all(|i| i.task_id != "FINAL"));
        assert!(rec.events().into_iter().any(|e| matches!(e, RunnerEvent::SystemNotice { text, code, params, .. } if code == "final.skipped" && text.starts_with("AVISO: verificação final não executada porque o orçamento") && params.contains_key("reason"))));
    }

    #[tokio::test]
    async fn budget_exceeded_blocks_before_any_agent_call_with_budget_kind() {
        let fx = Fixture::new("budget", "make test");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).with_budget(Arc::new(Over));
        match runner.run_task(SPEC, "T01").await.unwrap() {
            Outcome::Blocked { reason, kind, .. } => {
                assert_eq!(kind, BlockKind::Budget);
                assert_eq!(kind.as_str(), "budget");
                assert!(reason.starts_with("orçamento da spec atingido"), "{reason}");
            }
            other => panic!("esperava Blocked: {other:?}"),
        }
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::Blocked);
        assert!(t.frontmatter.next_action.unwrap().contains("orçamento"));
        assert!(rev.inputs().is_empty());
    }

    #[tokio::test]
    async fn docker_mode_without_compose_executor_blocks_environmental() {
        let fx = Fixture::new("docker-sem", "make test");
        enable_docker(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let out = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert!(matches!(out, Outcome::Blocked { kind: BlockKind::Environmental, .. }), "{out:?}");
    }

    fn set_checks(fx: &Fixture, checks: Vec<crate::sdd::Check>) {
        let mut settings = read_settings(&fx.settings_path()).unwrap();
        settings.checks = checks;
        crate::sdd::write_settings(&fx.settings_path(), &settings).unwrap();
        fx.commit_all("chore: checks");
    }

    fn check(name: &str, command: &str, blocking: bool) -> crate::sdd::Check {
        crate::sdd::Check { name: name.into(), command: command.into(), blocking }
    }

    #[tokio::test]
    async fn identical_tree_reuses_branch_tests_and_checks_on_base() {
        let fx = Fixture::new("reuso", "true");
        set_checks(&fx, vec![check("build", "true", true), check("lint", "true", false)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        // Se a base repetisse, a 4ª chamada (teste na base) falharia e bloquearia.
        let tests = ScriptedTests::new(vec![true, true, true, false, false]);
        let outcome = fx.runner(&dev, &rev, tests.clone(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
        // worktree: teste, build, lint; base: nada.
        assert_eq!(*tests.calls.lock().unwrap(), 3, "nada reexecutado na base");
        let t = fx.read_task("T01");
        let run = t.frontmatter.test_run.expect("test_run gravado");
        let short = t.frontmatter.reviewed_sha.as_deref().unwrap()[..7].to_string();
        assert_eq!(run.command, "true");
        assert_eq!(run.exit_code, Some(0));
        assert_eq!(run.summary, format!("reaproveitado: árvore idêntica à testada em {short}"));
        assert!(
            rec.events().iter().any(|e| matches!(e, RunnerEvent::AgentMessage { agent, text, .. }
                if agent == "tests" && text.contains("não repetidos na base") && text.contains(&short))),
            "{:?}",
            rec.events()
        );
    }

    #[tokio::test]
    async fn different_tree_runs_tests_and_checks_on_base() {
        let fx = Fixture::new("reuso-difere", "true");
        set_checks(&fx, vec![check("build", "true", true)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = base_moving_reviewer(fx.repo.clone());
        let tests = ScriptedTests::default();
        let outcome = fx.runner(&dev, &rev, tests.clone(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
        // worktree: teste, build; base: teste, build.
        assert_eq!(*tests.calls.lock().unwrap(), 4);
        let run = fx.read_task("T01").frontmatter.test_run.expect("test_run gravado");
        assert!(!run.summary.starts_with("reaproveitado"), "{}", run.summary);
    }

    fn tests_texts(rec: &Recorder) -> Vec<String> {
        rec.events()
            .into_iter()
            .filter_map(|e| match e {
                RunnerEvent::AgentMessage { agent, text, .. } if agent == "tests" => Some(text),
                _ => None,
            })
            .collect()
    }

    /// Desenvolvedor que grava arquivos fixos (caminho relativo, conteúdo) a cada chamada.
    struct FilesDev(Vec<(&'static str, &'static str)>);

    impl DeveloperPort for FilesDev {
        fn implement<'a>(&'a self, brief: &'a DevBrief) -> Fut<'a, Result<DevReport, RunnerError>> {
            Box::pin(async move {
                for (rel, content) in &self.0 {
                    let path = brief.worktree.join(rel);
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(path, content).unwrap();
                }
                Ok(DevReport { summary: "feito".into(), exhausted: false })
            })
        }
    }

    fn formatter(name: &str, command: &str, workdir: &str, paths: &[&str]) -> crate::sdd::Formatter {
        crate::sdd::Formatter {
            name: name.into(),
            command: command.into(),
            workdir: workdir.into(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn set_formatters(fx: &Fixture, formatters: Vec<crate::sdd::Formatter>) {
        let mut settings = read_settings(&fx.settings_path()).unwrap();
        settings.formatters = formatters;
        crate::sdd::write_settings(&fx.settings_path(), &settings).unwrap();
        fx.commit_all("chore: formatters");
    }

    fn module_task() -> Task {
        let mut t = task("T01", TaskStatus::Todo, &[]);
        t.frontmatter.allowed_scope = vec!["web/**".into(), "api/**".into()];
        t
    }

    /// Roda a T01 com `FilesDev` e devolve (mensagens `tests`, assuntos da branch vistos na revisão).
    async fn run_with_formatters(
        fx: &Fixture,
        files: Vec<(&'static str, &'static str)>,
    ) -> (Vec<String>, Vec<String>) {
        fx.add_task(&module_task());
        let rec = Recorder::default();
        let subjects = Arc::new(Mutex::new(Vec::new()));
        let (wt, seen) = (fx.worktree("T01"), Arc::clone(&subjects));
        let rev = FakeReviewer::new(move |_, _| {
            *seen.lock().unwrap() = git_in(&wt, &["log", "--format=%s", "main..HEAD"])
                .lines()
                .map(str::to_string)
                .collect();
            approved(vec![])
        });
        let runner = TaskRunner::new(
            fx.repo.clone(),
            Box::new(FilesDev(files)),
            Box::new(rev),
            Box::new(ShellTestExecutor::new()),
            Box::new(rec.clone()),
        );
        assert_eq!(runner.run_task(SPEC, "T01").await.unwrap(), Outcome::Done);
        let seen = subjects.lock().unwrap().clone();
        (tests_texts(&rec), seen)
    }

    /// Executor do shell que simula tempo esgotado nos comandos com `SLOW` (spec 113 T04).
    struct SlowShell(ShellTestExecutor);

    impl TestExecutor for SlowShell {
        fn run<'a>(&'a self, dir: &'a Path, command: &'a str) -> Fut<'a, Result<TestResult, RunnerError>> {
            if command.contains("SLOW") {
                return Box::pin(async { Ok(TestResult { passed: false, output: String::new(), exit_code: None, timed_out: true }) });
            }
            self.0.run(dir, command)
        }
    }

    /// Spec 115 T04 (C-09): o commit do Nants na branch da tarefa mantém o prefixo convencional e
    /// leva o texto no idioma do projeto; só o `feat(<spec>): Txx - ` marca o DONE.
    #[test]
    fn developer_commit_keeps_the_prefix_and_follows_the_project_language() {
        for (lang, name, text) in [
            (crate::i18n::Lang::En, "dev-commit-en", "developer changes"),
            (crate::i18n::Lang::PtBr, "dev-commit-pt", "alterações do desenvolvedor"),
        ] {
            let fx = Fixture::new(name, "true");
            let wt = fx.repo.with_file_name("wt");
            git_in(&fx.repo, &["worktree", "add", "-q", "-b", "sdd/s1/T01", wt.to_str().unwrap()]);
            fs::write(wt.join("novo.txt"), "x\n").unwrap();
            commit_pending(&wt, &fx.repo, SPEC, "T01", "sdd/s1/T01", lang).unwrap();
            let subject = git_in(&wt, &["log", "-1", "--format=%s"]);
            assert_eq!(subject, format!("chore(s1): T01 - {text}"));
            assert!(!is_task_commit(&subject, SPEC, "T01"), "commit da branch não marca DONE");
        }
    }

    /// Spec 115 T04 (C-09): em projeto en, a tarefa termina DONE pelo commit `feat(<spec>): Txx - `.
    #[tokio::test]
    async fn done_is_detected_by_the_feat_prefix_in_an_english_project() {
        let en = formatter_texts_in(crate::i18n::Lang::En, "done-prefix-en").await;
        assert!(!en.is_empty());
        let root = env::temp_dir().join(format!("nants-runner-{}-done-prefix-en", process::id())).join("repo");
        let subject = git_in(&root, &["log", "-1", "--format=%s", "main"]);
        assert!(subject.starts_with("feat(s1): T01 - "), "{subject}");
        assert!(is_integrated(&Git::new(root.clone()), "main", SPEC, "T01").unwrap());
        // A branch da tarefa (já apagada) levou os commits com o prefixo e o texto em inglês.
        let lost = git_in(&root, &["fsck", "--unreachable", "--no-reflogs"]);
        let subjects: Vec<String> = lost
            .lines()
            .filter_map(|l| l.strip_prefix("unreachable commit "))
            .map(|sha| git_in(&root, &["log", "-1", "--format=%s", sha.trim()]))
            .collect();
        assert!(subjects.iter().any(|l| l == "style(s1): T01 - automatic formatting"), "{subjects:?}");
        assert!(subjects.iter().any(|l| l == "chore(s1): T01 - developer changes"), "{subjects:?}");
    }

    /// Formatador que aplica, um que sai 3 e um que estoura o tempo, no idioma pedido; devolve as
    /// mensagens `tests`.
    async fn formatter_texts_in(lang: crate::i18n::Lang, name: &str) -> Vec<String> {
        let fx = Fixture::new(name, "true");
        set_formatters(
            &fx,
            vec![
                formatter("web", "for f in *.txt; do tr a-z A-Z < \"$f\" > \"$f.tmp\" && mv \"$f.tmp\" \"$f\"; done", "web", &["web/**"]),
                formatter("boom", "echo oops-text; exit 3", ".", &["**"]),
                formatter("slow", "echo SLOW", ".", &["**"]),
            ],
        );
        fx.add_task(&module_task());
        let rec = Recorder::default();
        let runner = TaskRunner::new(
            fx.repo.clone(),
            Box::new(FilesDev(vec![("web/a.txt", "abc\n")])),
            Box::new(FakeReviewer::new(|_, _| approved(vec![]))),
            Box::new(SlowShell(ShellTestExecutor::new())),
            Box::new(rec.clone()),
        )
        .with_lang(crate::i18n::SharedLang::new(lang));
        assert_eq!(runner.run_task(SPEC, "T01").await.unwrap(), Outcome::Done);
        tests_texts(&rec)
    }

    #[tokio::test]
    async fn formatter_messages_follow_project_language() {
        let pt = formatter_texts_in(crate::i18n::Lang::PtBr, "fmt-idioma-pt").await;
        assert!(pt.iter().any(|t| t.starts_with("formatação automática (boom) falhou (não bloqueia): saída 3: ") && t.contains("oops-text")), "{pt:?}");
        assert!(pt.contains(&"formatação automática (slow) falhou (não bloqueia): tempo esgotado".to_string()), "{pt:?}");
        assert!(pt.contains(&"formatação automática: 1 arquivo (web)".to_string()), "{pt:?}");

        let en = formatter_texts_in(crate::i18n::Lang::En, "fmt-idioma-en").await;
        assert!(en.iter().any(|t| t.starts_with("automatic formatting (boom) failed (does not block): exit 3: ") && t.contains("oops-text")), "{en:?}");
        assert!(en.contains(&"automatic formatting (slow) failed (does not block): timed out".to_string()), "{en:?}");
        assert!(en.contains(&"automatic formatting: 1 file (web)".to_string()), "{en:?}");
        assert_no_pt(&en);
        assert_eq!(i18n::tr(crate::i18n::Lang::En, "runner.format.failed", &[("why", "x")]), "automatic formatting failed (does not block): x");
        assert_eq!(i18n::tr(crate::i18n::Lang::PtBr, "runner.format.failed", &[("why", "x")]), "formatação automática falhou (não bloqueia): x");
    }

    #[tokio::test]
    async fn formatters_run_by_module_commit_only_scope_and_failures_do_not_block() {
        let fx = Fixture::new("fmt-modulos", "true");
        set_formatters(
            &fx,
            vec![
                formatter("web", "for f in *.txt; do tr a-z A-Z < \"$f\" > \"$f.tmp\" && mv \"$f.tmp\" \"$f\"; done", "web", &["web/**"]),
                formatter("api", "echo done > formatted.txt", "api", &["api/**"]),
                formatter("docs", "touch ran-docs.txt", ".", &["docs/**"]),
                formatter("wide", "echo x > outside.txt", ".", &["**"]),
                formatter("boom", "echo partial > partial.txt; echo oops-text; exit 3", ".", &["**"]),
                formatter("after", "echo late > api/late.txt", ".", &["**"]),
            ],
        );
        let (texts, subjects) = run_with_formatters(&fx, vec![("web/a.txt", "abc\n"), ("api/b.txt", "x\n")]).await;

        assert_eq!(
            subjects,
            vec!["style(s1): T01 - formatação automática", "chore(s1): T01 - alterações do desenvolvedor"],
            "commit de formatação fica depois do commit do Desenvolvedor"
        );
        assert_eq!(fs::read_to_string(fx.repo.join("web/a.txt")).unwrap(), "ABC\n", "workdir web respeitado");
        assert_eq!(fs::read_to_string(fx.repo.join("api/formatted.txt")).unwrap(), "done\n", "workdir api respeitado");
        assert_eq!(fs::read_to_string(fx.repo.join("api/late.txt")).unwrap(), "late\n", "o seguinte roda após a falha");
        assert!(!fx.repo.join("web/formatted.txt").exists());
        assert!(!fx.repo.join("ran-docs.txt").exists() && !fx.repo.join("docs/ran-docs.txt").exists(), "módulo não tocado não roda");
        assert!(!fx.repo.join("outside.txt").exists(), "fora de allowed_scope é revertido");
        assert!(!fx.repo.join("partial.txt").exists(), "mudanças do formatador que falhou são descartadas");
        let boom: Vec<&String> = texts.iter().filter(|t| t.starts_with("formatação automática (boom) falhou (não bloqueia): ")).collect();
        assert_eq!(boom.len(), 1, "{texts:?}");
        assert!(boom[0].contains("saída 3") && boom[0].contains("oops-text"), "{}", boom[0]);
        assert!(texts.contains(&"formatação automática: 3 arquivos (web, api, after)".to_string()), "{texts:?}");
        assert!(texts.iter().any(|t| t.starts_with("testes ✓")), "o fluxo segue para os testes: {texts:?}");
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn formatters_without_changes_or_without_match_leave_no_commit_nor_message() {
        let fx = Fixture::new("fmt-nada", "true");
        set_formatters(
            &fx,
            vec![
                formatter("noop", "true", ".", &["**"]),
                formatter("docs", "touch ran-docs.txt", ".", &["docs/**"]),
            ],
        );
        let (texts, subjects) = run_with_formatters(&fx, vec![("web/a.txt", "abc\n")]).await;
        assert_eq!(subjects, vec!["chore(s1): T01 - alterações do desenvolvedor"]);
        assert!(texts.iter().all(|t| !t.contains("formatação")), "{texts:?}");
        assert!(!fx.repo.join("ran-docs.txt").exists());
    }

    #[tokio::test]
    async fn no_formatters_configured_runs_nothing() {
        let fx = Fixture::new("fmt-sem", "true");
        let (texts, subjects) = run_with_formatters(&fx, vec![("web/a.txt", "abc\n")]).await;
        assert_eq!(subjects, vec!["chore(s1): T01 - alterações do desenvolvedor"]);
        assert!(texts.iter().all(|t| !t.contains("formatação")), "{texts:?}");
        assert_eq!(fs::read_to_string(fx.repo.join("web/a.txt")).unwrap(), "abc\n");
    }

    /// Spec 092 T05: formatador que altera e sai 1, e na 2ª execução sai 0, conta como aplicado
    /// (as mudanças da 1ª ficam), sem aviso de falha.
    #[tokio::test]
    async fn formatter_exit_1_then_0_is_applied() {
        let fx = Fixture::new("fmt-retry-ok", "true");
        let marker = std::env::temp_dir().join(format!("nants-fmt-retry-{}", std::process::id()));
        let _ = fs::remove_file(&marker);
        // O caminho entra uma vez só: com o TMPDIR do scripts/test.sh, repeti-lo passava do limite de
        // 200 caracteres do comando e o formatador era descartado.
        let cmd = format!(
            "m='{m}'; [ -f \"$m\" ] && exit 0; touch \"$m\"; tr a-z A-Z <a.txt >a.tmp && mv a.tmp a.txt; exit 1",
            m = marker.display()
        );
        assert!(cmd.chars().count() <= crate::sdd::TEST_COMMAND_MAX_CHARS, "{cmd}");
        set_formatters(&fx, vec![formatter("flaky", &cmd, "web", &["web/**"])]);
        let (texts, subjects) = run_with_formatters(&fx, vec![("web/a.txt", "abc\n")]).await;
        let _ = fs::remove_file(&marker);
        assert_eq!(fs::read_to_string(fx.repo.join("web/a.txt")).unwrap(), "ABC\n", "mudança da 1ª execução fica");
        assert!(texts.iter().all(|t| !t.contains("falhou")), "{texts:?}");
        assert!(texts.contains(&"formatação automática: 1 arquivo (flaky)".to_string()), "{texts:?}");
        assert_eq!(subjects[0], "style(s1): T01 - formatação automática");
    }

    /// Spec 092 T05: falha nas duas execuções descarta e avisa; a mesma falha numa 2ª tentativa
    /// da tarefa (mesma execução) não repete o aviso.
    #[tokio::test]
    async fn formatter_failing_twice_is_discarded_and_repeated_message_suppressed() {
        let fx = Fixture::new("fmt-retry-falha", "true");
        let count = std::env::temp_dir().join(format!("nants-fmt-count-{}", std::process::id()));
        let _ = fs::remove_file(&count);
        let cmd = format!("echo x >> '{}'; echo partial > partial.txt; echo oops-text; exit 3", count.display());
        set_formatters(&fx, vec![formatter("boom", &cmd, ".", &["**"])]);
        fx.add_task(&module_task());
        let rec = Recorder::default();
        let rev = FakeReviewer::new(|n, _| if n == 0 { rejected(vec![finding("F-01")]) } else { approved(vec![]) });
        let runner = TaskRunner::new(
            fx.repo.clone(),
            Box::new(FilesDev(vec![("web/a.txt", "abc\n")])),
            Box::new(rev),
            Box::new(ShellTestExecutor::new()),
            Box::new(rec.clone()),
        );
        assert_eq!(runner.run_task(SPEC, "T01").await.unwrap(), Outcome::Done);
        let runs = fs::read_to_string(&count).unwrap_or_default().lines().count();
        let _ = fs::remove_file(&count);
        assert_eq!(runs, 4, "2 execuções por tentativa, 2 tentativas");
        assert!(!fx.repo.join("partial.txt").exists(), "mudanças descartadas");
        let texts = tests_texts(&rec);
        let boom: Vec<&String> = texts.iter().filter(|t| t.starts_with("formatação automática (boom) falhou (não bloqueia): ")).collect();
        assert_eq!(boom.len(), 1, "{texts:?}");
        assert!(boom[0].contains("saída 3") && boom[0].contains("oops-text"), "{}", boom[0]);
    }

    #[tokio::test]
    async fn formatter_with_missing_workdir_fails_without_blocking() {
        let fx = Fixture::new("fmt-workdir", "true");
        set_formatters(&fx, vec![formatter("ghost", "echo x > y.txt", "nao-existe", &["web/**"])]);
        let (texts, subjects) = run_with_formatters(&fx, vec![("web/a.txt", "abc\n")]).await;
        assert_eq!(subjects, vec!["chore(s1): T01 - alterações do desenvolvedor"]);
        assert!(
            texts.iter().any(|t| t.starts_with("formatação automática (ghost) falhou (não bloqueia): ") && t.contains("nao-existe")),
            "{texts:?}"
        );
        assert!(!fx.repo.join("y.txt").exists());
    }

    #[tokio::test]
    async fn passing_tests_emit_one_persisted_result_on_branch_and_base() {
        let fx = Fixture::new("resultado-ok", "true");
        set_checks(&fx, vec![check("build", "true", true)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = base_moving_reviewer(fx.repo.clone());
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(
            tests_texts(&rec),
            vec!["testes ✓ · checagens: build ✓", "testes ✓ na base · checagens: build ✓"]
        );

        let fx = Fixture::new("resultado-ok-sem-checks", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = base_moving_reviewer(fx.repo.clone());
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(tests_texts(&rec), vec!["testes ✓ (`true`)", "testes ✓ na base (`true`)"]);
    }

    #[tokio::test]
    async fn emits_live_start_and_end_for_tests_and_checks_on_branch_and_base() {
        let fx = Fixture::new("ao-vivo", "true");
        set_checks(&fx, vec![check("build", "true", true)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = base_moving_reviewer(fx.repo.clone());
        let outcome = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
        let runs: Vec<(String, bool, CommandKind, String, String)> = rec
            .events()
            .into_iter()
            .filter_map(|e| match e {
                RunnerEvent::CommandRun { run_id, started, kind, label, task_id, stage_code, stage_params, .. } => {
                    assert_eq!(task_id, "T01");
                    // Spec 102: o `code` da etapa e os `params` acompanham o rótulo de reserva.
                    let expected = match (kind, label.contains(" na base")) {
                        (CommandKind::Tests, false) => "tests",
                        (CommandKind::Tests, true) => "tests_base",
                        (CommandKind::Check, false) => "check",
                        (CommandKind::Check, true) => "check_base",
                    };
                    assert_eq!(stage_code, expected, "{label}");
                    assert_eq!(stage_params.get("attempt"), Some(&serde_json::Value::from(1)));
                    if kind == CommandKind::Check {
                        assert_eq!(stage_params.get("check"), Some(&serde_json::Value::from("build")));
                    }
                    Some((run_id, started, kind, label, task_id))
                }
                _ => None,
            })
            .collect();
        // worktree: testes, build; base: testes, build; cada um com início e fim do mesmo id.
        assert_eq!(runs.len(), 8, "{runs:?}");
        for pair in runs.chunks(2) {
            assert!(pair[0].1 && !pair[1].1, "{pair:?}");
            assert_eq!(pair[0].0, pair[1].0);
        }
        assert_eq!(runs[0].2, CommandKind::Tests);
        assert_eq!(runs[0].3, "Testes · tentativa 1");
        assert_eq!(runs[2].2, CommandKind::Check);
        assert_eq!(runs[2].3, "Checagem build · tentativa 1");
        assert_eq!(runs[4].3, "Testes na base · tentativa 1");
        assert_eq!(runs[6].3, "Checagem build na base · tentativa 1");
        let ids: std::collections::HashSet<_> = runs.iter().map(|r| &r.0).collect();
        assert_eq!(ids.len(), 4);
    }

    #[tokio::test]
    async fn git_error_or_missing_evidence_disables_reuse() {
        let fx = Fixture::new("reuso-falha-segura", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        let settings = read_settings(&fx.settings_path()).unwrap();
        let root = Git::new(fx.repo.clone());
        let head = fx.head();
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        t.frontmatter.reviewed_sha = Some(head.clone());
        t.frontmatter.test_evidence = Some("ok\n".into());
        // Sem vínculo da evidência ao sha testado/assinatura: roda na base.
        assert_eq!(runner.reusable_sha(&settings, &t, &root), None);
        t.frontmatter.tested_sha = Some(head.clone());
        t.frontmatter.tested_signature = Some(test_signature(&settings));
        assert_eq!(runner.reusable_sha(&settings, &t, &root), Some(head.clone()), "caminho feliz");
        // F-01: evidência de A, mas o revisado é B (mesma árvore): B nunca foi testado => roda.
        let mut a = t.clone();
        a.frontmatter.tested_sha = Some("a".repeat(40));
        assert_eq!(runner.reusable_sha(&settings, &a, &root), None);
        // Assinatura diferente (test_command mudou) => roda.
        let mut changed = settings.clone();
        changed.test_command = "outro comando".into();
        assert_eq!(runner.reusable_sha(&changed, &t, &root), None);
        // sha inexistente: o git falha na comparação => roda os testes.
        t.frontmatter.reviewed_sha = Some("0".repeat(40));
        assert_eq!(runner.reusable_sha(&settings, &t, &root), None);
        // sem evidência de aprovação da branch (ou com marca de falha) => roda os testes.
        t.frontmatter.reviewed_sha = Some(head.clone());
        t.frontmatter.test_evidence = None;
        assert_eq!(runner.reusable_sha(&settings, &t, &root), None);
        t.frontmatter.test_evidence = Some(failure_header(Some(1), false));
        assert_eq!(runner.reusable_sha(&settings, &t, &root), None);
    }

    #[tokio::test]
    async fn blocking_check_failure_returns_to_developer_and_counts() {
        let fx = Fixture::new("check-bloqueante", "true");
        set_checks(&fx, vec![check("build", "echo build-quebrado; false", true)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ShellTestExecutor::new(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Blocked { .. }));
        let briefs = dev.briefs();
        assert_eq!(briefs.len(), MAX_TEST_CYCLES as usize);
        assert!(briefs[1].test_output.as_deref().unwrap_or("").contains("build-quebrado"));
        assert!(rev.inputs().is_empty());
        let texts = tests_texts(&rec);
        assert!(!texts.is_empty());
        assert!(texts.iter().all(|t| !t.starts_with("testes ✓")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("build ✗")), "{texts:?}");
    }

    fn set_failure_artifacts(fx: &Fixture, globs: &[&str]) {
        let mut settings = read_settings(&fx.settings_path()).unwrap();
        settings.failure_artifacts = globs.iter().map(|g| g.to_string()).collect();
        crate::sdd::write_settings(&fx.settings_path(), &settings).unwrap();
        fx.commit_all("chore: failure_artifacts");
    }

    const WRITES_ARTIFACT: &str =
        "mkdir -p test-results/login && echo pagina-em-branco-TypeError > test-results/login/error-context.md && false";

    #[tokio::test]
    async fn failure_artifacts_reach_developer_and_evidence() {
        let fx = Fixture::new("fa-anexo", WRITES_ARTIFACT);
        set_failure_artifacts(&fx, &["test-results/**/error-context.md"]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let outcome = fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert!(matches!(outcome, Outcome::Blocked { .. }), "{outcome:?}");
        let brief = dev.briefs()[1].test_output.clone().unwrap_or_default();
        assert!(brief.contains("## Artefatos da falha"), "{brief}");
        assert!(brief.contains("### test-results/login/error-context.md") && brief.contains("pagina-em-branco-TypeError"), "{brief}");
        let evidence = fx.read_task("T01").frontmatter.test_evidence.unwrap_or_default();
        assert!(evidence.starts_with(&failure_header(Some(1), false)), "{evidence}");
        assert!(evidence.contains("## Artefatos da falha") && evidence.contains("pagina-em-branco-TypeError"), "{evidence}");
    }

    /// Configura `screenshots` e ignora `shots/` (como o exclude de `test-results/` num projeto real),
    /// para as capturas gravadas pelos testes não sujarem a worktree.
    fn set_screenshots(fx: &Fixture, globs: &[&str]) {
        let mut settings = read_settings(&fx.settings_path()).unwrap();
        settings.screenshots = globs.iter().map(|g| g.to_string()).collect();
        crate::sdd::write_settings(&fx.settings_path(), &settings).unwrap();
        fs::write(fx.repo.join(".gitignore"), "shots/\n").unwrap();
        fx.commit_all("chore: screenshots");
    }

    /// Testes que gravam uma captura PNG (assinatura real) e uma falsa com a extensão certa.
    const WRITES_SCREENSHOT: &str =
        "mkdir -p shots && printf '\\211PNG\\r\\n\\032\\n' > shots/home.png && echo texto > shots/fake.png";

    #[tokio::test]
    async fn reviewer_sees_screenshots_and_viewed_ones_are_copied_and_listed() {
        // Spec 082 T03: o Revisor recebe as capturas da execução, pede uma e ela é copiada para
        // `.sdd/specs/<spec>/captures/`, com a linha "Capturas avaliadas:" antes de "Critérios:".
        let fx = Fixture::new("shots-review", WRITES_SCREENSHOT);
        set_screenshots(&fx, &["shots/*.png"]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, input| ReviewResult {
            criteria: vec![CriterionCheck { criterion: "c1".into(), met: true, evidence: "e".into() }],
            screenshots_viewed: input.screenshots.iter().filter(|c| c.usable()).map(|c| c.name.clone()).collect(),
            ..approved(vec![])
        });
        let rec = Recorder::default();
        let outcome = fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
        assert_eq!(dev.briefs()[0].screenshot_globs, vec!["shots/*.png".to_string()]);
        let input = &rev.inputs()[0];
        let names: Vec<&str> = input.screenshots.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["shots/home.png"], "a falsa fica de fora");
        let copy = fx.repo.join(".sdd/specs").join(SPEC).join("captures/t01-1-home.png");
        assert_eq!(fs::read(&copy).expect("cópia"), b"\x89PNG\r\n\x1a\n");
        let text = reviewer_texts(&rec).pop().expect("mensagem do revisor");
        assert!(text.contains("\n\nCapturas avaliadas: t01-1-home.png\n\nCritérios:\n✓ c1: e"), "{text}");
    }

    #[tokio::test]
    async fn reviewer_is_told_which_previous_task_captures_vanished() {
        // Spec 086 T01: cópias de outra tarefa que a execução atual não gera mais vão para o briefing.
        let fx = Fixture::new("shots-vanished", WRITES_SCREENSHOT);
        set_screenshots(&fx, &["shots/*.png"]);
        let captures = fx.repo.join(".sdd/specs").join(SPEC).join("captures");
        fs::create_dir_all(&captures).unwrap();
        for f in ["t01-1-home.png", "t01-2-inicio.png", "t02-1-propria.png", "final-1-geral.png"] {
            fs::write(captures.join(f), b"\x89PNG\r\n\x1a\n").unwrap();
        }
        fx.add_task(&task("T02", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let outcome = fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T02").await.unwrap();
        assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
        let input = &rev.inputs()[0];
        assert_eq!(
            input.vanished_captures,
            format!("{}: inicio.png (T01)", crate::sdd::screenshots::VANISHED_LABEL)
        );
    }

    #[tokio::test]
    async fn without_screenshots_configured_reviewer_gets_none_and_nothing_is_copied() {
        let fx = Fixture::new("shots-none", WRITES_SCREENSHOT);
        set_screenshots(&fx, &[]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, _| ReviewResult { screenshots_viewed: vec!["shots/home.png".into()], ..approved(vec![]) });
        let outcome = fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert!(matches!(outcome, Outcome::Done), "{outcome:?}");
        assert!(rev.inputs()[0].screenshots.is_empty() && dev.briefs()[0].screenshot_globs.is_empty());
        assert!(!fx.repo.join(".sdd/specs").join(SPEC).join("captures").exists());
        assert!(!reviewer_texts(&rec).concat().contains(CAPTURES_HEADER));
    }

    #[tokio::test]
    async fn failure_artifacts_absent_leaves_failure_output_unchanged() {
        let fx = Fixture::new("fa-sem", WRITES_ARTIFACT);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T01").await.unwrap();
        let evidence = fx.read_task("T01").frontmatter.test_evidence.unwrap_or_default();
        assert!(!evidence.contains("Artefatos da falha"), "{evidence}");
        assert!(!dev.briefs()[1].test_output.clone().unwrap_or_default().contains("Artefatos da falha"));
    }

    #[tokio::test]
    async fn failure_artifacts_attach_on_blocking_check_failure() {
        let fx = Fixture::new("fa-check", "true");
        set_checks(&fx, vec![check("e2e", "mkdir -p out && echo detalhe-da-checagem > out/ctx.md && false", true)]);
        set_failure_artifacts(&fx, &["out/*.md"]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T01").await.unwrap();
        let brief = dev.briefs()[1].test_output.clone().unwrap_or_default();
        assert!(brief.contains("checagem e2e falhou") && brief.contains("### out/ctx.md") && brief.contains("detalhe-da-checagem"), "{brief}");
    }

    #[tokio::test]
    async fn blocking_check_not_found_keeps_environment_header_in_evidence() {
        let fx = Fixture::new("check-127", "true");
        set_checks(&fx, vec![check("build", "comando-que-nao-existe-xyz", true)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ShellTestExecutor::new(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Blocked { .. }));
        let evidence = fx.read_task("T01").frontmatter.test_evidence.unwrap_or_default();
        assert!(evidence.starts_with(&failure_header(Some(127), false)), "{evidence:?}");
        assert!(crate::server::recovery::command_did_not_run(&evidence));
    }

    #[tokio::test]
    async fn non_blocking_check_failure_reaches_reviewer_and_summary_is_emitted() {
        let fx = Fixture::new("check-advisory", "true");
        set_checks(
            &fx,
            vec![check("lint", "echo aviso-de-lint; false", false), check("build", "true", true)],
        );
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        assert_eq!(dev.briefs().len(), 1);
        let inputs = rev.inputs();
        assert_eq!(inputs.len(), 1);
        assert!(inputs[0].checks.contains("aviso-de-lint"), "{:?}", inputs[0].checks);
        assert!(inputs[0].checks.contains("lint - FALHOU (não bloqueante)"));
        assert!(!inputs[0].test_output.contains("Checagens"));
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
    }

    /// Execuções (início) de uma checagem na base.
    fn base_check_runs(rec: &Recorder, name: &str) -> usize {
        let label = format!("Checagem {name} na base");
        rec.events()
            .iter()
            .filter(|e| matches!(e, RunnerEvent::CommandRun { started: true, label: l, .. } if l.starts_with(&label)))
            .count()
    }

    #[tokio::test]
    async fn non_blocking_check_passing_on_base_and_failing_on_branch_is_a_regression() {
        // `.git` é diretório na base e arquivo na worktree: a checagem passa na base e falha na branch.
        let fx = Fixture::new("check-regressao", "true");
        set_checks(&fx, vec![check("lint", "test -d .git", false)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| if n == 0 { rejected(vec![finding("F-01")]) } else { approved(vec![]) });
        let rec = Recorder::default();
        let outcome = fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
        let texts = tests_texts(&rec);
        assert!(
            texts.iter().any(|t| t.contains("lint ✗ (não bloqueante, regressão desta tarefa)")),
            "{texts:?}"
        );
        let inputs = rev.inputs();
        assert_eq!(inputs.len(), 2);
        for input in &inputs {
            assert_eq!(input.regressions, "lint");
            assert!(!input.checks.contains("Regressões desta tarefa"), "{}", input.checks);
        }
        // Duas tentativas, mesma base: a checagem rodou na base uma única vez (cache).
        assert_eq!(base_check_runs(&rec, "lint"), 1);
    }

    #[tokio::test]
    async fn non_blocking_check_failing_on_base_too_is_not_a_regression() {
        let fx = Fixture::new("check-sem-regressao", "true");
        set_checks(&fx, vec![check("lint", "false", false)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T01").await.unwrap();
        let texts = tests_texts(&rec);
        assert!(texts.iter().any(|t| t.contains("lint ✗ (não bloqueante)")), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("regressão")), "{texts:?}");
        assert!(rev.inputs()[0].regressions.is_empty());
        assert_eq!(base_check_runs(&rec, "lint"), 1);
    }

    #[tokio::test]
    async fn passing_non_blocking_check_does_not_run_on_base() {
        let fx = Fixture::new("check-passa", "true");
        set_checks(&fx, vec![check("lint", "true", false)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(base_check_runs(&rec, "lint"), 0);
        assert!(rev.inputs()[0].regressions.is_empty());
    }

    #[test]
    fn checks_summary_marks_regressions() {
        let o = |n: &str, b: bool, p: bool| CheckOutcome { name: n.into(), command: String::new(), blocking: b, passed: p, output: String::new(), exit_code: None, timed_out: false };
        assert_eq!(
            checks_summary_marked(&[o("lint", false, false), o("fmt", false, false), o("build", true, true)], &["lint".into()]),
            "checagens: lint ✗ (não bloqueante, regressão desta tarefa), fmt ✗ (não bloqueante), build ✓"
        );
    }

    #[test]
    fn silent_output_ignores_compose_lines_and_warns_in_checks() {
        let compose = " Network app_default  Creating\n Container app-db-1  Started\n ✔ Volume app_data  Created\n";
        assert!(is_silent_output(""));
        assert!(is_silent_output(compose));
        assert!(!is_silent_output(&format!("{compose}BUILD SUCCESSFUL\n")));
        let mk = |output: &str| CheckOutcome {
            name: "build".into(),
            command: "make build".into(),
            blocking: true,
            passed: true,
            output: output.into(),
            exit_code: Some(0),
            timed_out: false,
        };
        assert!(checks_section(&[mk(compose)]).contains(SILENT_PASS_WARNING));
        assert!(!checks_section(&[mk("BUILD SUCCESSFUL")]).contains(SILENT_PASS_WARNING));
    }

    #[test]
    fn checks_summary_marks_non_blocking_failures() {
        let o = |n: &str, b: bool, p: bool| CheckOutcome { name: n.into(), command: String::new(), blocking: b, passed: p, output: String::new(), exit_code: None, timed_out: false };
        assert_eq!(
            checks_summary(&[o("lint", false, false), o("build", true, true)]),
            "checagens: lint ✗ (não bloqueante), build ✓"
        );
    }

    #[tokio::test]
    async fn without_checks_reviewer_gets_empty_checks() {
        let fx = Fixture::new("sem-checks", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        fx.runner(&dev, &rev, ShellTestExecutor::new(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert!(rev.inputs()[0].checks.is_empty());
    }

    #[tokio::test]
    async fn record_recovery_retry_reopens_and_developer_brief_carries_the_section() {
        let fx = Fixture::new("recuperacao", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| if n < 3 { rejected(vec![finding("F-01")]) } else { approved(vec![]) });
        let rec = Recorder::default();
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        let outcome = runner.run_task(SPEC, "T01").await.unwrap();
        assert!(matches!(outcome, Outcome::Blocked { .. }));

        let n = runner
            .record_recovery(SPEC, "T01", true, "- Causa raiz: x\n- Estratégia: testes primeiro")
            .unwrap();
        assert_eq!(n, 1);
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::Todo);
        assert_eq!(t.frontmatter.attempts, 0);
        assert_eq!(recovery_cycles(&t.body), 1);
        assert!(t.body.contains("## Análise de recuperação #1\n- Causa raiz: x"));

        let outcome = runner.run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
        let last = dev.briefs().pop().unwrap();
        assert!(last.recovery.contains("Estratégia: testes primeiro"), "{:?}", last.recovery);
        assert!(last.recovery.starts_with("## Análise de recuperação #1"));
    }

    /// Spec 073: a orientação do humano gravada no corpo chega ao briefing do Desenvolvedor, e o
    /// bloco de opções (JSON) fica fora do que os agentes recebem.
    #[tokio::test]
    async fn human_guidance_reaches_the_developer_brief_and_options_block_does_not() {
        let fx = Fixture::new("orientacao-humano", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|n, _| if n < 3 { rejected(vec![finding("F-01")]) } else { approved(vec![]) });
        let rec = Recorder::default();
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        assert!(matches!(runner.run_task(SPEC, "T01").await.unwrap(), Outcome::Blocked { .. }));
        let text = format!("- Causa raiz: x\n- Decisão: escalar ao humano\n{OPTIONS_OPEN}\n[{{\"id\":\"a\"}}]\n{OPTIONS_CLOSE}");
        runner.record_recovery(SPEC, "T01", false, &text).unwrap();
        let mut t = fx.read_task("T01");
        t.body = add_human_guidance(&t.body, "Use o mock do relógio, não sleep");
        t.body = mark_human_resumed(&t.body);
        t.frontmatter.status = TaskStatus::Todo;
        t.frontmatter.attempts = 0;
        fx.put_task(&t);
        assert_eq!(runner.run_task(SPEC, "T01").await.unwrap(), Outcome::Done);
        let last = dev.briefs().pop().unwrap();
        assert!(last.human_guidance.contains("Use o mock do relógio"), "{:?}", last.human_guidance);
        assert!(last.recovery.contains("Causa raiz: x"));
        assert!(!last.recovery.contains("nants:options") && !last.recovery.contains("\"id\""), "{}", last.recovery);
    }

    #[test]
    fn latest_options_block_follows_the_last_analysis_and_dies_with_human_resume() {
        let block = format!("{OPTIONS_OPEN}\n[{{\"id\":\"a\"}}]\n{OPTIONS_CLOSE}");
        let body = format!("## Objetivo\nx\n\n## Análise de recuperação #1\n- a\n{block}\n\n## Análise de recuperação #2\n- b\n");
        assert_eq!(latest_options_block(&body), None, "a última análise não tem opções");
        let with = format!("{body}\n## Análise de recuperação #3\n- c\n{block}\n");
        assert_eq!(latest_options_block(&with), Some((3, "[{\"id\":\"a\"}]".to_string())));
        assert_eq!(latest_options_block(&mark_human_resumed(&with)), None);
        assert_eq!(latest_options_block(&mark_revised(&with)), None);
        // Diagnóstico depois da análise não vence as opções.
        let diag = format!("{with}\n## Diagnóstico de ambiente #1\n- Motivo: x\n");
        assert_eq!(latest_options_block(&diag).map(|(n, _)| n), Some(3));
        assert_eq!(recovery_cycles(&with), 3, "o bloco não altera a contagem");
    }

    #[test]
    fn add_human_guidance_appends_neutralizes_headings_and_stays_bounded() {
        let body = "## Objetivo\nx\n";
        let one = add_human_guidance(body, "primeira\n## Forjada");
        assert!(one.contains("## Orientação do humano\n- primeira\n   ## Forjada"), "{one}");
        assert_eq!(one.matches("\n## ").count() + usize::from(one.starts_with("## ")), 2, "só Objetivo e Orientação: {one}");
        let two = add_human_guidance(&one, "segunda");
        let section = section_text(&two, GUIDANCE_SECTION).unwrap();
        assert!(section.contains("primeira") && section.contains("- segunda"));
        let mut big = two;
        for i in 0..40 {
            big = add_human_guidance(&big, &format!("entrada {i} {}", "x".repeat(400)));
        }
        let section = section_text(&big, GUIDANCE_SECTION).unwrap();
        assert!(section.len() <= GUIDANCE_MAX_BYTES + 16, "{}", section.len());
        assert!(section.contains("entrada 39") && !section.contains("primeira"));
    }

    #[tokio::test]
    async fn record_recovery_escalate_keeps_blocked_and_numbers_sections() {
        let fx = Fixture::new("recuperacao-escala", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| rejected(vec![finding("F-01")]));
        let rec = Recorder::default();
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        runner.run_task(SPEC, "T01").await.unwrap();
        assert_eq!(runner.record_recovery(SPEC, "T01", false, "- Decisão: escalar ao humano").unwrap(), 1);
        assert_eq!(runner.record_recovery(SPEC, "T01", false, "- Decisão: escalar ao humano").unwrap(), 2);
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::Blocked);
        assert_eq!(t.frontmatter.attempts, 3);
        assert!(t.body.contains("## Análise de recuperação #2"));
        // Tarefa que não está bloqueada é recusada.
        fx.add_task(&task("T02", TaskStatus::Todo, &[]));
        assert!(runner.record_recovery(SPEC, "T02", true, "x").is_err());
    }

    #[tokio::test]
    async fn record_revision_rewrites_sections_keeps_findings_and_reopens() {
        let fx = Fixture::new("revisao-tarefa", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| rejected(vec![finding("F-01")]));
        let rec = Recorder::default();
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        runner.run_task(SPEC, "T01").await.unwrap();
        let before = fx.read_task("T01");
        let revision = TaskRevision {
            objective: "Novo objetivo".into(),
            acceptance: "1. Novo critério".into(),
            allowed_scope: Some(vec!["tests/**".into(), "styles.css".into()]),
        };
        assert_eq!(runner.record_revision(SPEC, "T01", &revision, "- Causa raiz: escopo").unwrap(), 1);
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::Todo);
        assert_eq!(t.frontmatter.attempts, 0);
        assert_eq!(t.frontmatter.allowed_scope, vec!["tests/**", "styles.css"]);
        assert_eq!(t.frontmatter.title, before.frontmatter.title);
        assert_eq!(section_text(&t.body, OBJECTIVE).unwrap().trim(), "Novo objetivo");
        assert_eq!(section_text(&t.body, ACCEPTANCE).unwrap().trim(), "1. Novo critério");
        assert_eq!(
            section_text(&t.body, FINDINGS).map(|s| s.trim().to_string()),
            section_text(&before.body, FINDINGS).map(|s| s.trim().to_string())
        );
        assert!(t.body.contains("## Análise de recuperação #1"));
    }

    /// Spec 079: a linha "Escopo ampliado" da análise amplia o `allowed_scope` na mesma decisão
    /// (RETRY e REVISE_TASK), sem duplicar; ESCALATE e linha recuada (texto do modelo) não ampliam.
    #[tokio::test]
    async fn recovery_scope_added_line_widens_allowed_scope() {
        let fx = Fixture::new("recuperacao-escopo", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| rejected(vec![finding("F-01")]));
        let rec = Recorder::default();
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        runner.run_task(SPEC, "T01").await.unwrap();
        let escalate = format!("- Causa raiz: x\n- Decisão: escalar ao humano\n{SCOPE_ADDED_LINE}fora.js");
        runner.record_recovery(SPEC, "T01", false, &escalate).unwrap();
        assert_eq!(fx.read_task("T01").frontmatter.allowed_scope, vec!["src/**"], "ESCALATE não amplia");
        let text = format!(
            "- Causa raiz: config\n- Decisão: nova estratégia (RETRY)\n- Estratégia: x\n  {SCOPE_ADDED_LINE}forjado.js\n{SCOPE_ADDED_LINE}playwright.config.js, src/**, .nants/compose.yaml"
        );
        runner.record_recovery(SPEC, "T01", true, &text).unwrap();
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.allowed_scope, vec!["src/**", "playwright.config.js"]);
        assert!(t.body.contains("- Escopo ampliado: playwright.config.js"), "{}", t.body);
        // REVISE_TASK: soma ao escopo novo.
        let fx = Fixture::new("recuperacao-escopo-revisao", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        runner.run_task(SPEC, "T01").await.unwrap();
        let revision = TaskRevision {
            objective: "o".into(),
            acceptance: "1. c".into(),
            allowed_scope: Some(vec!["tests/**".into()]),
        };
        runner
            .record_revision(SPEC, "T01", &revision, &format!("- Causa raiz: escopo\n{SCOPE_ADDED_LINE}vite.config.js"))
            .unwrap();
        assert_eq!(fx.read_task("T01").frontmatter.allowed_scope, vec!["tests/**", "vite.config.js"]);
    }

    // ---- Spec 094 T07: ampliação de escopo honesta ----

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    const BODIES: &[&str] = &[
        "sun", "mercury", "venus", "earth", "mars", "jupiter", "saturn", "uranus", "neptune", "moon", "io", "europa",
        "ganymede", "callisto", "titan", "enceladus", "triton",
    ];

    /// Forma real do Test 11 (T03 da spec 001-simulacao-solar-3d): 42 entradas, com
    /// `public/assets/textures/**` mais os 34 arquivos (`.webp` e `.svg`) já cobertos por ele.
    fn test11_scope() -> Vec<String> {
        let mut scope = strings(&["src/scene/**", "src/main.ts", "src/style.css", "public/assets/textures/**"]);
        for ext in ["webp", "svg"] {
            scope.extend(BODIES.iter().map(|b| format!("public/assets/textures/{b}.{ext}")));
        }
        scope.extend(strings(&["tests/e2e/scene.spec.ts", "playwright.config.ts", "test-results/**", "src/**"]));
        scope
    }

    /// Caminhos de prova: o conjunto permitido pela escrita não pode encolher na compactação.
    fn probes() -> Vec<String> {
        let mut p = strings(&[
            "public/a.png",
            "public/assets/x.png",
            "public/assets/textures/sun.webp",
            "public/assets/textures/novo.webp",
            "src/main.ts",
            "src/style.css",
            "src/scene/a.ts",
            "tests/e2e/scene.spec.ts",
            "tests/e2e/base-page.spec.ts",
            "playwright.config.ts",
            "test-results/r.json",
            "README.md",
        ]);
        p.extend(BODIES.iter().map(|b| format!("public/assets/textures/{b}.svg")));
        p
    }

    fn allowed_set(scope: &[String]) -> Vec<bool> {
        probes().iter().map(|p| crate::harness::tools::write_scope_allows(scope, p)).collect()
    }

    #[test]
    fn compaction_drops_only_literals_covered_by_a_glob() {
        let mut scope = strings(&["public/**", "public/assets/**", "public/a.png"]);
        let before = allowed_set(&scope);
        assert_eq!(compact_scope(&mut scope), vec!["public/a.png"]);
        assert_eq!(scope, vec!["public/**", "public/assets/**"], "nenhum glob sai, mesmo coberto por outro glob");
        assert_eq!(allowed_set(&scope), before);
        // Literal não coberto fica; a ordem das demais é preservada.
        let mut scope = strings(&["b.txt", "src/**", "a.txt", "src/x.rs"]);
        compact_scope(&mut scope);
        assert_eq!(scope, vec!["b.txt", "src/**", "a.txt"]);
    }

    #[test]
    fn test11_shape_compacts_and_admits_the_new_path() {
        let mut scope = test11_scope();
        assert_eq!(scope.len(), 42);
        assert!(scope.len() > MAX_TASK_SCOPE);
        let before = allowed_set(&scope);
        assert!(!crate::harness::tools::write_scope_allows(&scope, "tests/e2e/base-page.spec.ts"));
        let ext = extend_scope(&mut scope, &strings(&["tests/e2e/base-page.spec.ts"]));
        assert_eq!(ext.added, vec!["tests/e2e/base-page.spec.ts"]);
        assert!(ext.refused.is_empty(), "{ext:?}");
        assert_eq!(
            scope,
            strings(&[
                "src/scene/**",
                "public/assets/textures/**",
                "tests/e2e/scene.spec.ts",
                "playwright.config.ts",
                "test-results/**",
                "src/**",
                "tests/e2e/base-page.spec.ts",
            ])
        );
        // O conjunto permitido não encolheu e agora inclui o caminho novo.
        let after = allowed_set(&scope);
        for (i, (b, a)) in before.iter().zip(&after).enumerate() {
            assert!(!b || *a, "encolheu em {}", probes()[i]);
        }
        assert!(crate::harness::tools::write_scope_allows(&scope, "tests/e2e/base-page.spec.ts"));
    }

    #[test]
    fn full_scope_without_redundancy_refuses_with_the_limit() {
        let mut scope: Vec<String> = (0..MAX_TASK_SCOPE).map(|i| format!("f{i}.txt")).collect();
        let ext = extend_scope(&mut scope, &strings(&["tests/e2e/base-page.spec.ts"]));
        assert!(ext.added.is_empty());
        assert_eq!(scope.len(), MAX_TASK_SCOPE);
        assert_eq!(
            ext.refused,
            vec![crate::harness::tools::ScopeRefusal::Limit { path: "tests/e2e/base-page.spec.ts".into(), limit: MAX_TASK_SCOPE }]
        );
        assert_eq!(
            ext.refused[0].describe(),
            "`tests/e2e/base-page.spec.ts` não ampliado: limite de 32 entradas no allowed_scope"
        );
    }

    #[test]
    fn covered_path_takes_no_slot_and_says_so() {
        let mut scope = strings(&["public/assets/textures/**", "src/**"]);
        let ext = extend_scope(&mut scope, &strings(&["public/assets/textures/sun.webp", "src/**"]));
        assert!(ext.added.is_empty());
        assert_eq!(scope, vec!["public/assets/textures/**", "src/**"]);
        let said: Vec<String> = ext.refused.iter().map(|r| r.describe()).collect();
        assert_eq!(
            said,
            vec![
                "`public/assets/textures/sun.webp` já coberto por `public/assets/textures/**`".to_string(),
                "`src/**` já coberto por `src/**`".to_string(),
            ]
        );
        // Glob pedido não é comparado com glob: entra (se houver vaga).
        let ext = extend_scope(&mut scope, &strings(&["public/**"]));
        assert_eq!(ext.added, vec!["public/**"]);
    }

    fn task_with_scope(id: &str, scope: Vec<String>) -> Task {
        let mut t = task(id, TaskStatus::Todo, &[]);
        t.frontmatter.allowed_scope = scope;
        t
    }

    #[test]
    fn advice_records_only_what_entered() {
        let fx = Fixture::new("conselho-honesto", "true");
        fx.add_task(&task_with_scope("T03", test11_scope()));
        let mut full: Vec<String> = (0..MAX_TASK_SCOPE).map(|i| format!("f{i}.txt")).collect();
        full[0] = "public/assets/**".into();
        fx.add_task(&task_with_scope("T04", full.clone()));
        let editor = TaskEditor::new(fx.repo.clone(), Default::default());
        let entry = "### Pergunta (developer)\nposso editar?\n\nResposta: sim";
        let add = strings(&["tests/e2e/base-page.spec.ts"]);
        // Forma real: compacta e o acréscimo entra.
        let (ext, _) = editor.apply_advice_report(SPEC, "T03", entry, &add, None).unwrap();
        assert_eq!(ext.added, add);
        let t = fx.read_task("T03");
        assert_eq!(t.frontmatter.allowed_scope.len(), 7);
        assert!(crate::harness::tools::write_scope_allows(&t.frontmatter.allowed_scope, "tests/e2e/base-page.spec.ts"));
        let history = section_text(&t.body, ADVICE_SECTION).unwrap();
        assert!(history.contains("- allowed_scope ampliado: tests/e2e/base-page.spec.ts"), "{history}");
        // Lista cheia sem redundância: recusa com o motivo, e o registro não diz "ampliado".
        let (ext, _) = editor.apply_advice_report(SPEC, "T04", entry, &add, None).unwrap();
        assert!(ext.added.is_empty());
        let t = fx.read_task("T04");
        assert_eq!(t.frontmatter.allowed_scope, full);
        let history = section_text(&t.body, ADVICE_SECTION).unwrap();
        assert!(!history.contains("allowed_scope ampliado"), "{history}");
        assert!(history.contains("- `tests/e2e/base-page.spec.ts` não ampliado: limite de 32 entradas no allowed_scope"), "{history}");
        // Caminho já coberto: nenhuma entrada nova, e o registro diz que já estava coberto.
        let (ext, _) = editor.apply_advice_report(SPEC, "T04", entry, &strings(&["public/assets/a.png"]), None).unwrap();
        assert!(ext.added.is_empty());
        assert_eq!(fx.read_task("T04").frontmatter.allowed_scope, full);
        let history = section_text(&fx.read_task("T04").body, ADVICE_SECTION).unwrap();
        assert!(history.contains("- `public/assets/a.png` já coberto por `public/assets/**`"), "{history}");
    }

    #[tokio::test]
    async fn recovery_follows_the_same_rules_and_record() {
        let fx = Fixture::new("recuperacao-honesta", "true");
        let mut full: Vec<String> = (0..MAX_TASK_SCOPE).map(|i| format!("f{i}.txt")).collect();
        full[0] = "src/**".into();
        fx.add_task(&task_with_scope("T01", full.clone()));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| rejected(vec![finding("F-01")]));
        let rec = Recorder::default();
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        runner.run_task(SPEC, "T01").await.unwrap();
        let text = format!("- Causa raiz: x\n- Decisão: nova estratégia (RETRY)\n{SCOPE_ADDED_LINE}tests/e2e/base-page.spec.ts, src/a.ts");
        let shown = reconcile_scope_added(&fx.read_task("T01").frontmatter.allowed_scope, &text);
        runner.record_recovery(SPEC, "T01", true, &text).unwrap();
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.allowed_scope, full, "nada entrou");
        let section = section_text(&t.body, &format!("{RECOVERY_PREFIX}1")).unwrap();
        assert!(!section.contains(SCOPE_ADDED_LINE), "{section}");
        assert!(section.contains("- Escopo não ampliado: `tests/e2e/base-page.spec.ts` não ampliado: limite de 32 entradas no allowed_scope"), "{section}");
        assert!(section.contains("- Escopo não ampliado: `src/a.ts` já coberto por `src/**`"), "{section}");
        assert_eq!(section.trim(), shown.trim(), "a conversa vê o mesmo registro do corpo");
        // Idempotente: reaplicar o texto já reconciliado não muda nada.
        assert_eq!(reconcile_scope_added(&full, &shown), shown);
        // Forma real: compacta e entra.
        let fx = Fixture::new("recuperacao-honesta-t11", "true");
        fx.add_task(&task_with_scope("T01", test11_scope()));
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);
        runner.run_task(SPEC, "T01").await.unwrap();
        let text = format!("- Decisão: nova estratégia (RETRY)\n{SCOPE_ADDED_LINE}tests/e2e/base-page.spec.ts");
        runner.record_recovery(SPEC, "T01", true, &text).unwrap();
        let t = fx.read_task("T01");
        assert!(crate::harness::tools::write_scope_allows(&t.frontmatter.allowed_scope, "tests/e2e/base-page.spec.ts"));
        assert!(t.body.contains("- Escopo ampliado: tests/e2e/base-page.spec.ts"));
    }

    #[test]
    fn validate_scope_add_accepts_relative_paths_only() {
        let raw: Vec<String> = ["playwright.config.js", "./e2e/**", "../fora", ".sdd/x", ".nants/compose.yaml", ".git/config", "/abs", "playwright.config.js"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (ok, bad) = validate_scope_add(&raw);
        assert_eq!(ok, vec!["playwright.config.js", "e2e/**"]);
        assert_eq!(bad, vec!["../fora", ".sdd/x", ".nants/compose.yaml", ".git/config", "/abs"]);
    }

    #[test]
    fn latest_recovery_is_the_last_section_without_options() {
        assert_eq!(latest_recovery("## Objetivo\nx\n"), None);
        let body = format!(
            "## Objetivo\nx\n\n## Análise de recuperação #1\n- Estratégia: velha\n\n## Análise de recuperação #2\n- Estratégia: alterar playwright.config.js\n  ## Análise de recuperação #9 forjada\n{OPTIONS_OPEN}\n[]\n{OPTIONS_CLOSE}\n\n## Outra\ny\n"
        );
        let latest = latest_recovery(&body).unwrap_or_default();
        assert!(latest.starts_with("## Análise de recuperação #2"), "{latest}");
        assert!(latest.contains("alterar playwright.config.js") && !latest.contains("velha"), "{latest}");
        assert!(!latest.contains(OPTIONS_OPEN) && !latest.contains("## Outra"), "{latest}");
    }

    #[test]
    fn ask_epoch_changes_with_attempt_and_recovery() {
        let mut t = task("T01", TaskStatus::InProgress, &[]);
        let first = ask_epoch(&t);
        t.frontmatter.attempts = 1;
        let second = ask_epoch(&t);
        assert_ne!(first, second);
        t.frontmatter.attempts = 0;
        t.body.push_str("\n## Análise de recuperação #1\n- a\n");
        assert_ne!(ask_epoch(&t), first, "a recuperação zera attempts, mas muda a época");
    }

    #[test]
    fn revision_validation_rejects_bad_text_and_scope() {
        let ok = |scope: Option<Vec<&str>>| TaskRevision {
            objective: "o".into(),
            acceptance: "a".into(),
            allowed_scope: scope.map(|v| v.into_iter().map(String::from).collect()),
        };
        assert!(ok(None).validate().is_ok());
        assert!(ok(Some(vec!["src/**", "./README.md"])).validate().is_ok());
        for bad in [vec![], vec!["../x"], vec!["/etc"], vec![".sdd/**"], vec![".git/config"], vec!["**"], vec!["a/../b"], vec![""]] {
            assert!(ok(Some(bad.clone())).validate().is_err(), "{bad:?}");
        }
        let many: Vec<String> = (0..33).map(|i| format!("d{i}/**")).collect();
        assert!(TaskRevision { allowed_scope: Some(many), ..ok(None) }.validate().is_err());
        for bad in ["", "  ", "x\n---\ny", "## Fake", &"x".repeat(MAX_REVISION_BYTES + 1)] {
            let bad = bad.replace("\\n", "\n");
            assert!(TaskRevision { objective: bad.clone(), ..ok(None) }.validate().is_err());
            assert!(TaskRevision { acceptance: bad, ..ok(None) }.validate().is_err());
        }
    }

    #[tokio::test]
    async fn diverged_reviewed_sha_returns_to_review_before_integrating() {
        let fx = Fixture::new("divergencia", "true");
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        fx.leftover_work("T01");
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some("0".repeat(40));
        fx.add_task(&t);
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done);
        assert_eq!(rev.inputs().len(), 1, "voltou a IN_REVIEW e foi revisada");
        assert!(dev.briefs().is_empty(), "developer não foi acionado");
        assert_eq!(
            rec.statuses("T01"),
            vec![
                TaskStatus::InReview,
                TaskStatus::Approved,
                TaskStatus::Integrating,
                TaskStatus::Done
            ]
        );
        assert_eq!(fx.feat_commits(), 1);
    }

    #[tokio::test]
    async fn resume_integrating_with_commit_on_base_finishes_without_new_merge() {
        let fx = Fixture::new("retomada", "true");
        let head = fx.leftover_work("T01");
        git_in(&fx.repo, &["merge", "--squash", &branch_name(SPEC, "T01")]);
        git_in(&fx.repo, &["commit", "-m", "feat(s1): T01 - Título T01"]);
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        fx.put_task(&t);
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done);
        assert_eq!(fx.feat_commits(), 1, "nenhum novo squash");
        assert!(rev.inputs().is_empty());
        assert!(dev.briefs().is_empty());
        assert!(!fx.worktree("T01").exists());
        assert!(!fx.branch_exists("sdd/s1/T01"));
        assert_eq!(rec.statuses("T01"), vec![TaskStatus::Done]);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn run_spec_resumes_integrating_with_commit_and_continues_to_next_task() {
        let fx = Fixture::new("retomada-spec", "true");
        let head = fx.leftover_work("T01");
        git_in(&fx.repo, &["merge", "--squash", &branch_name(SPEC, "T01")]);
        git_in(&fx.repo, &["commit", "-m", "feat(s1): T01 - Título T01"]);
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        fx.put_task(&t);
        fx.add_task(&task("T02", TaskStatus::Todo, &["T01"]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_spec(SPEC)
            .await
            .unwrap();
        assert_eq!(outcome, SpecOutcome::Completed);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
        assert_eq!(fx.read_task("T02").frontmatter.status, TaskStatus::Done);
        assert_eq!(dev.briefs().iter().map(|b| b.task_id.as_str()).collect::<Vec<_>>(), ["T02"]);
        assert!(!fx.worktree("T01").exists());
    }

    #[tokio::test]
    async fn resume_integrating_with_cleanup_failure_is_done_with_warning() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new("limpeza-falha", "true");
        let head = fx.leftover_work("T01");
        git_in(&fx.repo, &["merge", "--squash", &branch_name(SPEC, "T01")]);
        git_in(&fx.repo, &["commit", "-m", "feat(s1): T01 - Título T01"]);
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        fx.put_task(&t);
        // Pasta ignorada e sem permissão: a remoção da worktree falha (como arquivos de root).
        fs::write(fx.repo.join(".git/info/exclude"), "locked/\n").unwrap();
        let locked = fx.worktree("T01").join("locked");
        fs::create_dir_all(locked.join("sub")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let outcome = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(outcome.unwrap(), Outcome::Done);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
        let warned = rec.events().into_iter().any(|e| matches!(e,
            RunnerEvent::SystemNotice { text, code, params, task_id }
                if text.contains("não consegui apagar a worktree")
                    && code == "task.cleanup_failed"
                    && params.get("path").and_then(|p| p.as_str()).is_some_and(|p| p.ends_with("T01"))
                    && params.contains_key("error")
                    && task_id.as_deref() == Some("T01")));
        assert!(warned, "aviso claro no diálogo");
    }

    #[tokio::test]
    async fn dirty_base_blocks_without_touching_repository() {
        let fx = Fixture::new("base-suja", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        fs::write(fx.repo.join("novo.txt"), "sujo\n").unwrap();
        let head_before = fx.head();
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Blocked { .. }));
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
        assert_eq!(fx.head(), head_before, "base intocada");
        assert_eq!(fs::read_to_string(fx.repo.join("novo.txt")).unwrap(), "sujo\n");
        assert_eq!(fx.feat_commits(), 0);
    }

    #[tokio::test]
    async fn run_spec_respects_depends_on_and_stops_at_blocked() {
        let fx = Fixture::new("spec-bloqueio", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &["T03"]));
        fx.add_task(&task("T02", TaskStatus::Todo, &["T01"]));
        fx.add_task(&task("T03", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, input| {
            if input.task_id == "T03" {
                rejected(vec![finding("F-09")])
            } else {
                approved(vec![])
            }
        });
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_spec(SPEC)
            .await
            .unwrap();

        assert_eq!(
            outcome,
            SpecOutcome::Blocked {
                task_id: "T03".into(),
                reason: "3 reprovações do revisor".into(),
                kind: BlockKind::Quality,
                open_findings: vec![finding("F-09")],
            }
        );
        let briefs = dev.briefs();
        assert_eq!(briefs.len(), 3);
        assert!(briefs.iter().all(|b| b.task_id == "T03"), "T03 roda primeiro");
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Todo);
        assert_eq!(fx.read_task("T02").frontmatter.status, TaskStatus::Todo);
        assert_eq!(fx.read_task("T03").frontmatter.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn run_spec_completes_and_marks_spec_completed() {
        let fx = Fixture::new("spec-completa", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &["T02"]));
        fx.add_task(&task("T02", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_spec(SPEC)
            .await
            .unwrap();

        assert_eq!(outcome, SpecOutcome::Completed);
        let briefs = dev.briefs();
        assert_eq!(briefs.iter().map(|b| b.task_id.as_str()).collect::<Vec<_>>(), ["T02", "T01"]);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
        assert_eq!(fx.read_task("T02").frontmatter.status, TaskStatus::Done);
        assert_eq!(fx.feat_commits(), 2);
        let spec = read_spec_file(&fx);
        assert_eq!(spec.frontmatter.status, SpecStatus::Completed);
        assert_eq!(spec.body, "## Contexto\nteste\n");
    }

    #[tokio::test]
    async fn resumed_in_progress_spec_reuses_worktree_of_reset_task() {
        // Retomada: a tarefa BLOCKED voltou a TODO com attempts 0; a worktree/branch da tentativa
        // anterior continua registrada e é reaproveitada, sem erro de "worktree já existe".
        let fx = Fixture::new("retomada-worktree", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        fx.leftover_work("T01");
        let rec = Recorder::default();
        let runner = fx.runner(
            &FakeDev::default(),
            &FakeReviewer::new(|_, _| approved(vec![])),
            ScriptedTests::default(),
            &rec,
        );
        let outcome = runner.run_spec(SPEC).await.unwrap();

        assert!(matches!(outcome, SpecOutcome::Completed), "{outcome:?}");
        assert_eq!(fx.feat_commits(), 1);
        assert!(!fx.worktree("T01").exists());
        assert!(!fx.branch_exists("sdd/s1/T01"));
    }

    #[tokio::test]
    async fn cooperative_cancel_before_run_stops_without_touching_state() {
        let fx = Fixture::new("cancel-antes", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let runner = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_cancel(cancel);
        let err = runner.run_spec(SPEC).await.unwrap_err();
        assert!(matches!(err, RunnerError::Cancelled), "{err}");
        assert!(dev.briefs().is_empty(), "nenhuma tarefa deve começar");
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Todo);
    }

    #[tokio::test]
    async fn cancel_during_review_stops_before_squash() {
        let fx = Fixture::new("cancel-revisao", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let rev = FakeReviewer::new(move |_, _| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            approved(vec![])
        });
        let rec = Recorder::default();
        let runner = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_cancel(cancel);
        let err = runner.run_spec(SPEC).await.unwrap_err();
        assert!(matches!(err, RunnerError::Cancelled), "{err}");
        assert_eq!(fx.feat_commits(), 0, "squash não pode ocorrer após cancelamento");
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Approved);
    }

    /// Sessão do Desenvolvedor interrompida pela parada (spec 086 T04): escreve na worktree, a flag é
    /// acionada no meio da sessão e ela termina com `Cancelled`, como o laço do `LlmDeveloper`.
    struct StoppedDev(Arc<std::sync::atomic::AtomicBool>);

    impl DeveloperPort for StoppedDev {
        fn implement<'a>(&'a self, brief: &'a DevBrief) -> Fut<'a, Result<DevReport, RunnerError>> {
            Box::pin(async move {
                fs::write(brief.worktree.join("parcial.txt"), "meio caminho\n").unwrap();
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(RunnerError::Cancelled)
            })
        }
    }

    #[tokio::test]
    async fn stop_during_developer_session_keeps_task_retomable_without_attempt_or_block() {
        let fx = Fixture::new("parar-dev", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runner = TaskRunner::new(
            fx.repo.clone(),
            Box::new(StoppedDev(Arc::clone(&cancel))),
            Box::new(rev.clone()),
            Box::new(ScriptedTests::default()),
            Box::new(rec.clone()),
        )
        .with_cancel(cancel);
        let err = runner.run_spec(SPEC).await.unwrap_err();
        assert!(matches!(err, RunnerError::Cancelled), "{err}");
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::InProgress, "sem BLOCKED nem reprovação");
        assert_eq!(t.frontmatter.attempts, 0, "a parada não conta tentativa");
        assert!(rev.inputs().is_empty(), "sem revisão depois da parada");
        assert!(fx.worktree("T01").join("parcial.txt").exists(), "worktree preservada");
        assert_eq!(fx.feat_commits(), 0);
        // "Continuar": novo run (flag nova) retoma a mesma tarefa e integra com o trabalho anterior.
        let dev = FakeDev::default();
        let out = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_spec(SPEC).await.unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        assert_eq!(dev.briefs().len(), 1);
        assert_eq!(fx.feat_commits(), 1);
        assert!(git_in(&fx.repo, &["log", "--stat", "-1"]).contains("parcial.txt"));
    }

    fn read_spec_file(fx: &Fixture) -> Spec {
        crate::sdd::read_spec(&fx.repo.join(".sdd/specs/s1/spec.md")).unwrap()
    }

    struct Gate(bool, std::sync::atomic::AtomicUsize);
    impl FinalGate for Gate {
        fn approve_fix<'a>(&'a self, _spec_id: &'a str, _reason: &'a str) -> Fut<'a, bool> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let answer = self.0;
            Box::pin(async move { answer })
        }
    }

    fn with_globals(fx: &Fixture) {
        let mut spec = read_spec_file(fx);
        spec.body = "## Contexto\nteste\n\n## Critérios de Aceitação Globais\n- tudo funciona\n- (manual) olhar a tela\n".into();
        crate::sdd::write_spec(&fx.repo.join(".sdd/specs/s1/spec.md"), &spec).unwrap();
        fx.commit_all("chore: globais");
    }

    fn matrix(met: bool) -> ReviewResult {
        let c = vec![CriterionCheck { criterion: "tudo funciona".into(), met, evidence: "visto".into() }];
        if met {
            ReviewResult { criteria: c, ..approved(vec![]) }
        } else {
            enforce_criteria(Lang::PtBr, ReviewResult { criteria: c, ..approved(vec![]) })
        }
    }

    #[tokio::test]
    async fn final_verification_all_met_completes_and_reports_count() {
        let fx = Fixture::new("final-ok", "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        let out = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_spec(SPEC).await.unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        let last = rev.inputs().pop().unwrap();
        assert_eq!(last.task_id, "FINAL");
        assert!(last.acceptance.contains("tudo funciona") && last.acceptance.contains(HUMAN_CHECK_MARK));
        assert!(last.test_output.contains("T01"), "{}", last.test_output);
        let said = rec.events().into_iter().any(|e| matches!(e, RunnerEvent::AgentMessage { text, .. } if text.starts_with("Verificação final: 2 de 2 critérios atendidos") || text.starts_with("Verificação final: 1 de 1 critérios atendidos")));
        assert!(said, "{:?}", rec.events());
        assert_eq!(read_spec_file(&fx).frontmatter.status, SpecStatus::Completed);
    }

    /// Executor que só interfere na base (`dir == root`): falha a checagem `fail_cmd` ou não executa
    /// o teste (`err_tests`). Fora da base tudo passa. Saída = `SAIDA <comando>`.
    #[derive(Clone)]
    struct BaseProbe {
        root: PathBuf,
        fail_cmd: Option<String>,
        err_tests: bool,
        base_calls: Arc<Mutex<Vec<String>>>,
    }

    impl BaseProbe {
        fn new(root: &Path) -> Self {
            Self { root: root.to_path_buf(), fail_cmd: None, err_tests: false, base_calls: Arc::new(Mutex::new(Vec::new())) }
        }
    }

    impl TestExecutor for BaseProbe {
        fn run<'a>(&'a self, dir: &'a Path, command: &'a str) -> Fut<'a, Result<TestResult, RunnerError>> {
            Box::pin(async move {
                let on_base = dir == self.root;
                if on_base {
                    self.base_calls.lock().unwrap().push(command.to_string());
                    if self.err_tests && command == "true" {
                        return Err(RunnerError::Tests("ambiente indisponível".into()));
                    }
                }
                let failed = on_base && self.fail_cmd.as_deref() == Some(command);
                Ok(TestResult {
                    passed: !failed,
                    output: format!("SAIDA {command}"),
                    exit_code: Some(if failed { 1 } else { 0 }),
                    timed_out: false,
                })
            })
        }
    }

    #[tokio::test]
    async fn final_verification_sends_real_base_tests_and_checks_to_reviewer() {
        let fx = Fixture::new("final-real", "true");
        set_checks(&fx, vec![check("build", "echo build-ok", true), check("lint", "echo lint-ok", false)]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        let probe = BaseProbe::new(&fx.repo);
        let out = fx.runner(&dev, &rev, probe.clone(), &rec).run_spec(SPEC).await.unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        let last = rev.inputs().pop().unwrap();
        assert_eq!(last.task_id, "FINAL");
        assert!(last.test_output.contains("SAIDA true") && last.test_output.contains("exit=0 passou"), "{}", last.test_output);
        assert!(last.test_output.contains("reaproveitado"), "resumo por tarefa mantido: {}", last.test_output);
        assert!(last.checks.contains("### build - passou") && last.checks.contains("SAIDA echo build-ok"), "{}", last.checks);
        assert!(last.checks.contains("### lint - passou") && last.checks.contains("SAIDA echo lint-ok"), "{}", last.checks);
        assert_eq!(*probe.base_calls.lock().unwrap(), vec!["true", "echo build-ok", "echo lint-ok"]);
    }

    const CMD_SECRET: &str = "sk-SEGREDO-123456";

    fn secrets() -> Arc<crate::server::events::SecretSet> {
        Arc::new(crate::server::events::SecretSet::new(vec![CMD_SECRET.to_string()]))
    }

    #[tokio::test]
    async fn task_evidence_starts_with_redacted_command_and_checks_carry_theirs() {
        let fx = Fixture::new("cmd-evidencia", &format!("TOKEN={CMD_SECRET} npm ci && npm test"));
        set_checks(&fx, vec![check("build", &format!("KEY={CMD_SECRET} npm run build"), true)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let out = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).with_secrets(secrets()).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(out, Outcome::Done);
        let input = rev.inputs().remove(0);
        let first = input.test_output.lines().next().unwrap_or_default().to_string();
        assert!(first.starts_with("$ TOKEN=") && first.ends_with(" npm ci && npm test"), "{first}");
        assert!(!input.test_output.contains(CMD_SECRET) && !input.test_output.contains("sk-SEG"), "{}", input.test_output);
        assert!(input.checks.contains("### build - passou\n$ KEY="), "{}", input.checks);
        assert!(input.checks.contains(" npm run build\n"), "{}", input.checks);
        assert!(!input.checks.contains(CMD_SECRET), "{}", input.checks);
    }

    #[tokio::test]
    async fn final_evidence_starts_with_redacted_command_and_checks_carry_theirs() {
        let fx = Fixture::new("cmd-final", &format!("TOKEN={CMD_SECRET} true"));
        set_checks(&fx, vec![check("build", "echo build-ok", true)]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        let probe = BaseProbe::new(&fx.repo);
        let out = fx.runner(&dev, &rev, probe, &rec).with_secrets(secrets()).run_spec(SPEC).await.unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        let last = rev.inputs().pop().unwrap();
        assert_eq!(last.task_id, "FINAL");
        let first = last.test_output.lines().next().unwrap_or_default().to_string();
        assert!(first.starts_with("$ TOKEN=") && first.ends_with(" true"), "{first}");
        assert!(!last.test_output.contains(CMD_SECRET), "{}", last.test_output);
        assert!(last.checks.contains("### build - passou\n$ echo build-ok\n"), "{}", last.checks);
    }

    #[tokio::test]
    async fn final_verification_failing_base_check_reaches_reviewer_and_fix_task() {
        let fx = Fixture::new("final-check-falha", "true");
        set_checks(&fx, vec![check("build", "echo build-ok", true), check("lint", "echo lint-ruim", false)]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(false) } else { approved(vec![]) });
        let mut probe = BaseProbe::new(&fx.repo);
        probe.fail_cmd = Some("echo lint-ruim".into());
        fx.runner(&dev, &rev, probe, &rec).run_spec(SPEC).await.unwrap();
        let last = rev.inputs().into_iter().find(|i| i.task_id == "FINAL").unwrap();
        assert!(last.checks.contains("### lint - FALHOU (não bloqueante)") && last.checks.contains("SAIDA echo lint-ruim"), "{}", last.checks);
        assert!(last.checks.contains("### build - passou"), "{}", last.checks);
        let fix = fx.read_task("T02");
        assert_eq!(fix.frontmatter.title, "Correção da verificação final");
        // Spec 102: o aviso da tarefa criada sai do sistema, com `code` e o id da tarefa.
        assert!(rec.events().into_iter().any(|e| matches!(e, RunnerEvent::SystemNotice { code, params, task_id: None, .. } if code == "final.fix_created" && params.get("task") == Some(&serde_json::Value::from("T02")))));
        assert!(fix.body.contains("Saída da checagem lint na base") && fix.body.contains("SAIDA echo lint-ruim"), "{}", fix.body);
    }

    #[tokio::test]
    async fn final_verification_execution_error_is_noted_and_continues() {
        let fx = Fixture::new("final-nao-exec", "true");
        set_checks(&fx, vec![check("build", "echo build-ok", true)]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        let mut probe = BaseProbe::new(&fx.repo);
        probe.err_tests = true;
        let out = fx.runner(&dev, &rev, probe, &rec).run_spec(SPEC).await.unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        let last = rev.inputs().pop().unwrap();
        assert_eq!(last.task_id, "FINAL");
        assert!(last.test_output.contains("não executado") && last.test_output.contains("ambiente indisponível"), "{}", last.test_output);
        assert!(last.checks.contains("### build - passou"), "{}", last.checks);
    }

    /// Mensagens `tests` sem tarefa (verificação final na base, spec 077 T02).
    fn final_base_messages(rec: &Recorder) -> Vec<String> {
        rec.events()
            .into_iter()
            .filter_map(|e| match e {
                RunnerEvent::AgentMessage { agent, text, task_id: None, .. } if agent == "tests" => Some(text),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn final_verification_records_base_result_message_on_success() {
        let fx = Fixture::new("final-msg-ok", "true");
        set_checks(&fx, vec![check("build", "echo build-ok", true)]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        fx.runner(&dev, &rev, BaseProbe::new(&fx.repo), &rec).run_spec(SPEC).await.unwrap();
        assert_eq!(final_base_messages(&rec), vec!["verificação final na base: testes ✓ · checagens: build ✓"]);
    }

    #[tokio::test]
    async fn final_verification_records_base_result_message_on_failure_and_not_run() {
        let fx = Fixture::new("final-msg-falha", "true");
        set_checks(&fx, vec![check("build", "echo build-ok", true), check("lint", "echo lint-ruim", false)]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(false) } else { approved(vec![]) });
        let mut probe = BaseProbe::new(&fx.repo);
        probe.fail_cmd = Some("echo lint-ruim".into());
        fx.runner(&dev, &rev, probe, &rec).run_spec(SPEC).await.unwrap();
        let msgs = final_base_messages(&rec);
        assert_eq!(msgs[0], "verificação final na base: testes ✓ · checagens: build ✓, lint ✗ (não bloqueante)");

        let fx = Fixture::new("final-msg-nao-exec", "true");
        set_checks(&fx, vec![check("build", "echo build-ok", true)]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        let mut probe = BaseProbe::new(&fx.repo);
        probe.err_tests = true;
        fx.runner(&dev, &rev, probe, &rec).run_spec(SPEC).await.unwrap();
        assert_eq!(final_base_messages(&rec), vec!["verificação final na base: testes não executado · checagens: build ✓"]);
    }

    #[tokio::test]
    async fn final_verification_without_globals_records_no_base_message() {
        let fx = Fixture::new("final-msg-sem", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        fx.runner(&dev, &rev, BaseProbe::new(&fx.repo), &rec).run_spec(SPEC).await.unwrap();
        assert!(final_base_messages(&rec).is_empty());
    }

    #[tokio::test]
    async fn final_verification_activity_covers_base_tests() {
        use crate::agents::activity::ActivityKind;
        let fx = Fixture::new("final-live-tests", "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        let act = Arc::new(crate::agents::activity::testing::Recorder::default());
        fx.runner(&dev, &rev, BaseProbe::new(&fx.repo), &rec).with_final_activity(act.clone(), SPEC).run_spec(SPEC).await.unwrap();
        let events = rec.events();
        let first_final_run = events.iter().position(|e| matches!(e, RunnerEvent::CommandRun { task_id, started: true, .. } if task_id == "FINAL"));
        assert!(first_final_run.is_some(), "{events:?}");
        let kinds: Vec<_> = final_activity_kinds(&act).into_iter().map(|k| k.2).collect();
        assert_eq!(kinds, vec![ActivityKind::Start, ActivityKind::End]);
    }

    struct FailingFinalReviewer;

    impl ReviewerPort for FailingFinalReviewer {
        fn review<'a>(&'a self, input: &'a ReviewInput) -> Fut<'a, Result<ReviewResult, RunnerError>> {
            Box::pin(async move {
                if input.task_id == "FINAL" {
                    Err(RunnerError::Invalid("falha simulada".into()))
                } else {
                    Ok(approved(vec![]))
                }
            })
        }
    }

    fn final_activity_kinds(act: &crate::agents::activity::testing::Recorder) -> Vec<(String, Option<String>, crate::agents::activity::ActivityKind)> {
        act.events().into_iter().map(|e| (e.agent, e.task_id, e.kind)).collect()
    }

    #[tokio::test]
    async fn final_verification_emits_and_clears_live_activity() {
        use crate::agents::activity::ActivityKind;
        let fx = Fixture::new("final-live", "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        let act = Arc::new(crate::agents::activity::testing::Recorder::default());
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_final_activity(act.clone(), SPEC)
            .run_spec(SPEC)
            .await
            .unwrap();
        let got = final_activity_kinds(&act);
        assert_eq!(
            got,
            vec![
                ("reviewer".to_string(), None, ActivityKind::Start),
                ("reviewer".to_string(), None, ActivityKind::End),
            ]
        );
        assert_eq!(act.events()[0].spec_id.as_deref(), Some(SPEC));
    }

    #[tokio::test]
    async fn final_verification_clears_live_activity_on_reviewer_error() {
        use crate::agents::activity::ActivityKind;
        let fx = Fixture::new("final-live-err", "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let act = Arc::new(crate::agents::activity::testing::Recorder::default());
        let out = TaskRunner::new(
            fx.repo.clone(),
            Box::new(dev.clone()),
            Box::new(FailingFinalReviewer),
            Box::new(ScriptedTests::default()),
            Box::new(rec.clone()),
        )
            .with_final_activity(act.clone(), SPEC)
            .run_spec(SPEC)
            .await
            .unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        let kinds: Vec<_> = final_activity_kinds(&act).into_iter().map(|k| k.2).collect();
        assert_eq!(kinds, vec![ActivityKind::Start, ActivityKind::End]);
    }

    #[tokio::test]
    async fn final_verification_without_globals_emits_no_live_activity() {
        let fx = Fixture::new("final-live-skip", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let act = Arc::new(crate::agents::activity::testing::Recorder::default());
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_final_activity(act.clone(), SPEC)
            .run_spec(SPEC)
            .await
            .unwrap();
        assert!(act.events().is_empty());
    }

    #[tokio::test]
    async fn final_verification_without_globals_is_skipped() {
        let fx = Fixture::new("final-skip", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_spec(SPEC).await.unwrap();
        assert!(rev.inputs().iter().all(|i| i.task_id != "FINAL"));
    }

    #[tokio::test]
    async fn final_verification_denied_gate_completes_with_warning_without_task() {
        let fx = Fixture::new("final-deny", "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        // Spec 102 T06: evidência longa (a lista passa dos 512 bytes de um param).
        let long = "evidência ".repeat(80);
        let evidence = long.clone();
        let rev = FakeReviewer::new(move |_, i| {
            if i.task_id != "FINAL" {
                return approved(vec![]);
            }
            let c = vec![CriterionCheck { criterion: "tudo funciona".into(), met: false, evidence: evidence.clone() }];
            enforce_criteria(Lang::PtBr, ReviewResult { criteria: c, ..approved(vec![]) })
        });
        let gate = Arc::new(Gate(false, Default::default()));
        let out = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_final_gate(gate.clone())
            .run_spec(SPEC)
            .await
            .unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        assert_eq!(gate.1.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!fx.task_path("T02").exists());
        // A lista vai inteira no texto, depois da frase fixa; nenhum param a carrega (seria cortada).
        assert!(long.len() > 512);
        assert!(rec.events().into_iter().any(|e| matches!(e, RunnerEvent::SystemNotice { text, code, params, .. }
            if code == "final.fix_refused" && text.starts_with("AVISO") && text.contains(long.trim()) && params.is_empty())));
    }

    #[tokio::test]
    async fn final_verification_approved_gate_creates_one_task_and_reverifies_once() {
        let fx = Fixture::new("final-fix", "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(false) } else { approved(vec![]) });
        let gate = Arc::new(Gate(true, Default::default()));
        let out = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_final_gate(gate.clone())
            .run_spec(SPEC)
            .await
            .unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        assert_eq!(gate.1.load(std::sync::atomic::Ordering::SeqCst), 1, "pede confirmação uma vez");
        assert_eq!(fx.read_task("T02").frontmatter.title, "Correção da verificação final");
        assert_eq!(fx.read_task("T02").frontmatter.status, TaskStatus::Done);
        assert!(!fx.task_path("T03").exists(), "no máximo uma rodada de correção");
        assert_eq!(rev.inputs().iter().filter(|i| i.task_id == "FINAL").count(), 2);
    }

    /// Spec 113 T04: palavras do pt-BR que não podem aparecer nas mensagens `tests` com o projeto
    /// em en (sentinela: falha se um texto fixo em pt voltar ao caminho en).
    const PT_SENTINEL: [&str; 16] = [
        "testes", "checagem", "checagens", "na base", "não", "falhou", "reaproveitado", "árvore",
        "verificação", "bloqueante", "executado", "regressão", "formatação", "esgotado", "saída", "arquivos",
    ];

    fn assert_no_pt(texts: &[String]) {
        for t in texts {
            let lower = t.to_lowercase();
            for word in PT_SENTINEL {
                assert!(!lower.contains(word), "texto em pt no caminho en ({word}): {t}");
            }
        }
    }

    fn en() -> crate::i18n::SharedLang {
        crate::i18n::SharedLang::new(crate::i18n::Lang::En)
    }

    #[tokio::test]
    async fn tests_messages_follow_project_language_en() {
        let fx = Fixture::new("resultado-en", "true");
        set_checks(&fx, vec![check("build", "true", true), check("lint", "true", false)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = base_moving_reviewer(fx.repo.clone());
        // worktree: teste falha (1/N), depois teste, build ✓ e lint ✗ (regressão: passa na base); base: teste, build.
        let tests = ScriptedTests::new(vec![false, true, true, false, true, true]);
        fx.runner(&dev, &rev, tests, &rec).with_lang(en()).run_task(SPEC, "T01").await.unwrap();
        let texts = tests_texts(&rec);
        assert_eq!(texts[0], "test_command failed (1/5)", "{texts:?}");
        assert_eq!(
            texts[1..],
            ["tests ✓ · checks: build ✓, lint ✗ (non-blocking, regression from this task)", "tests ✓ on the base · checks: build ✓"],
            "{texts:?}"
        );
        assert_no_pt(&texts);

        let fx = Fixture::new("resultado-en-sem-checks", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = base_moving_reviewer(fx.repo.clone());
        fx.runner(&dev, &rev, ScriptedTests::default(), &rec).with_lang(en()).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(tests_texts(&rec), vec!["tests ✓ (`true`)", "tests ✓ on the base (`true`)"]);
    }

    #[tokio::test]
    async fn failed_cycle_message_in_pt_br() {
        let fx = Fixture::new("ciclo-pt", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = base_moving_reviewer(fx.repo.clone());
        fx.runner(&dev, &rev, ScriptedTests::new(vec![false]), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(tests_texts(&rec)[0], "test_command falhou (1/5)");
    }

    #[tokio::test]
    async fn reused_base_message_follows_project_language_en() {
        let fx = Fixture::new("reuso-en", "true");
        set_checks(&fx, vec![check("build", "true", true)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let outcome = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).with_lang(en()).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
        let t = fx.read_task("T01");
        let short = t.frontmatter.reviewed_sha.as_deref().unwrap()[..7].to_string();
        assert_eq!(t.frontmatter.test_run.expect("test_run").summary, format!("reused: tree identical to the one tested at {short}"));
        let texts = tests_texts(&rec);
        assert!(
            texts.contains(&format!("tests and checks not repeated on the base: tree identical to the one tested at {short} (outside .sdd/)")),
            "{texts:?}"
        );
        assert_no_pt(&texts);
    }

    #[tokio::test]
    async fn failed_base_check_message_in_both_languages() {
        for (lang, name, expected) in [
            (crate::i18n::Lang::PtBr, "check-base-pt", "checagem build falhou na base:\n"),
            (crate::i18n::Lang::En, "check-base-en", "check build failed on the base:\n"),
        ] {
            let fx = Fixture::new(name, "true");
            set_checks(&fx, vec![check("build", "true", true)]);
            fx.add_task(&task("T01", TaskStatus::Todo, &[]));
            let (dev, rec) = (FakeDev::default(), Recorder::default());
            let rev = base_moving_reviewer(fx.repo.clone());
            // worktree: teste, build; base: teste ✓, build ✗.
            let tests = ScriptedTests::new(vec![true, true, true, false]);
            let runner = fx.runner(&dev, &rev, tests, &rec).with_lang(crate::i18n::SharedLang::new(lang));
            let out = runner.run_task(SPEC, "T01").await.unwrap();
            assert!(matches!(out, Outcome::Blocked { .. }), "{out:?}");
            let texts = tests_texts(&rec);
            assert!(texts.iter().any(|t| t.starts_with(expected)), "{texts:?}");
            if lang == crate::i18n::Lang::En {
                assert_no_pt(&texts);
            }
        }
    }

    #[tokio::test]
    async fn final_base_message_follows_project_language_en() {
        let fx = Fixture::new("final-msg-en", "true");
        set_checks(&fx, vec![check("build", "echo build-ok", true), check("lint", "echo lint-ruim", false)]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        let mut probe = BaseProbe::new(&fx.repo);
        probe.fail_cmd = Some("echo lint-ruim".into());
        fx.runner(&dev, &rev, probe, &rec).with_lang(en()).run_spec(SPEC).await.unwrap();
        let msgs = final_base_messages(&rec);
        assert_eq!(msgs, vec!["final verification on the base: tests ✓ · checks: build ✓, lint ✗ (non-blocking)"]);
        assert!(msgs[0].starts_with(&final_base_prefix(crate::i18n::Lang::En)));
        assert_no_pt(&msgs);

        let fx = Fixture::new("final-msg-en-nao-exec", "true");
        set_checks(&fx, vec![check("build", "echo build-ok", true)]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(true) } else { approved(vec![]) });
        let mut probe = BaseProbe::new(&fx.repo);
        probe.err_tests = true;
        fx.runner(&dev, &rev, probe, &rec).with_lang(en()).run_spec(SPEC).await.unwrap();
        assert_eq!(final_base_messages(&rec), vec!["final verification on the base: tests not run · checks: build ✓"]);
    }

    #[test]
    fn summaries_in_both_languages_and_pt_prefix_constant() {
        use crate::i18n::Lang;
        let o = |n: &str, b: bool, p: bool| CheckOutcome { name: n.into(), command: String::new(), blocking: b, passed: p, output: String::new(), exit_code: None, timed_out: false };
        let outcomes = [o("lint", false, false), o("fmt", false, false), o("build", true, true)];
        assert_eq!(
            checks_summary_in(Lang::PtBr, &outcomes, &["lint".into()]),
            "checagens: lint ✗ (não bloqueante, regressão desta tarefa), fmt ✗ (não bloqueante), build ✓"
        );
        let en = checks_summary_in(Lang::En, &outcomes, &["lint".into()]);
        assert_eq!(en, "checks: lint ✗ (non-blocking, regression from this task), fmt ✗ (non-blocking), build ✓");
        assert_no_pt(&[en]);
        assert_eq!(pass_summary_in(Lang::PtBr, true, "make", None), pass_summary(" na base", "make", None));
        assert_eq!(pass_summary_in(Lang::PtBr, false, "make", Some("checagens: a ✓")), pass_summary("", "make", Some("checagens: a ✓")));
        assert_eq!(pass_summary_in(Lang::En, true, "make", None), "tests ✓ on the base (`make`)");
        assert_eq!(final_base_prefix(Lang::PtBr), FINAL_BASE_PREFIX);
        assert!(REGRESSIONS_MARK.contains("Regressões") && i18n::text(Lang::PtBr, "runner.checks.non_blocking_regression").contains(REGRESSION_LABEL));
    }

    /// Spec 107 T01: a tarefa de correção sai no idioma ativo do projeto, com a marca estável
    /// `kind: final_fix`, e o aviso `final.fix_created` leva o título gravado.
    async fn final_fix_created_in(lang: crate::i18n::Lang, name: &str, expected: &str) {
        let fx = Fixture::new(name, "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(false) } else { approved(vec![]) });
        let out = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_lang(crate::i18n::SharedLang::new(lang))
            .run_spec(SPEC)
            .await
            .unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        let fix = fx.read_task("T02");
        assert_eq!(fix.frontmatter.title, expected);
        assert_eq!(fix.frontmatter.kind.as_deref(), Some(crate::sdd::TASK_KIND_FINAL_FIX));
        let raw = std::fs::read_to_string(fx.task_path("T02")).unwrap();
        assert!(raw.contains("kind:") && raw.contains("final_fix"), "{raw}");
        // Spec 115 T04 (C-07): os cabeçalhos lidos pelo parser ficam fixos; o texto livre segue o
        // idioma do projeto.
        let goal = i18n::text(lang, "runner.final_fix.objective");
        let reference = i18n::text(lang, "runner.final_fix.reference_heading");
        assert!(section_text(&fix.body, OBJECTIVE).is_some_and(|s| s.trim() == goal), "{}", fix.body);
        assert!(section_text(&fix.body, ACCEPTANCE).is_some(), "{}", fix.body);
        assert!(fix.body.contains(&format!("\n## {reference}\n")), "{}", fix.body);
        // O que vai ao Revisor na verificação final: a linha de cada tarefa no idioma do projeto.
        let final_input = rev.inputs().into_iter().find(|i| i.task_id == "FINAL").expect("FINAL");
        let at = if lang == crate::i18n::Lang::En { "` exit=0 at " } else { "` exit=0 em " };
        assert!(final_input.test_output.contains(at), "{}", final_input.test_output);
        if lang == crate::i18n::Lang::En {
            assert_no_pt(&[goal, reference]);
        }
        // Spec 115 T03: o texto de reserva é o `system.<code>` do idioma do projeto.
        let created = i18n::tr(lang, "system.final.fix_created", &[("task", "T02"), ("title", expected)]);
        assert!(rec.events().into_iter().any(|e| matches!(e, RunnerEvent::SystemNotice { text, code, params, .. }
            if code == "final.fix_created"
                && params.get("title") == Some(&serde_json::Value::from(expected))
                && text == created)));
        // 2ª rodada reconhecida pela marca: nenhuma nova tarefa, e a falha vira aviso.
        assert!(!fx.task_path("T03").exists(), "no máximo uma rodada de correção");
        assert_eq!(rev.inputs().iter().filter(|i| i.task_id == "FINAL").count(), 2);
        let still = format!("{}\n", i18n::text(lang, "system.final.still_unmet"));
        let unmet: Vec<String> = rec
            .events()
            .into_iter()
            .filter_map(|e| match e {
                RunnerEvent::SystemNotice { text, code, .. } if code == "final.still_unmet" => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(unmet.len(), 1);
        assert!(unmet[0].starts_with(&still) && unmet[0].contains("tudo funciona"), "{}", unmet[0]);
        if lang == crate::i18n::Lang::En {
            assert_no_pt(&[created, still]);
        }
    }

    #[tokio::test]
    async fn final_fix_task_title_follows_project_language_en() {
        final_fix_created_in(crate::i18n::Lang::En, "final-fix-en", "Final verification fix").await;
    }

    #[tokio::test]
    async fn final_fix_task_title_follows_project_language_pt_br() {
        final_fix_created_in(crate::i18n::Lang::PtBr, "final-fix-pt", "Correção da verificação final").await;
    }

    /// Portão que registra a pergunta recebida (spec 114 T02).
    #[derive(Default)]
    struct AskedGate(Mutex<Vec<String>>);
    impl FinalGate for AskedGate {
        fn approve_fix<'a>(&'a self, _spec_id: &'a str, reason: &'a str) -> Fut<'a, bool> {
            self.0.lock().unwrap().push(reason.to_string());
            Box::pin(async { false })
        }
    }

    async fn final_fix_question_in(lang: crate::i18n::Lang, name: &str) -> String {
        let fx = Fixture::new(name, "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(false) } else { approved(vec![]) });
        let gate = Arc::new(AskedGate::default());
        let out = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .with_lang(crate::i18n::SharedLang::new(lang))
            .with_final_gate(gate.clone())
            .run_spec(SPEC)
            .await
            .unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        let asked = gate.0.lock().unwrap().clone();
        assert_eq!(asked.len(), 1, "{asked:?}");
        asked[0].clone()
    }

    /// Spec 114 T02 (achado 11): a pergunta da verificação final sai no idioma do projeto; o
    /// detalhe (critério e evidência do Revisor) é conteúdo e vem como está.
    #[tokio::test]
    async fn final_fix_question_follows_project_language() {
        let en = final_fix_question_in(crate::i18n::Lang::En, "final-ask-en").await;
        let first = en.lines().next().unwrap_or_default().to_string();
        assert_eq!(first, "Final verification: 0 of 1 criteria met. Create a fix task?", "{en}");
        assert_no_pt(&[first]);
        assert!(en.contains("- tudo funciona"), "{en}");
        let pt = final_fix_question_in(crate::i18n::Lang::PtBr, "final-ask-pt").await;
        assert!(pt.starts_with("Verificação final: 0 de 1 critérios atendidos. Criar tarefa de correção?\n"), "{pt}");
    }

    /// Spec 114 T02 (achado 8): o motivo do bloqueio grava o código estável e o texto no idioma
    /// do projeto, nos dois idiomas.
    #[tokio::test]
    async fn block_reason_has_stable_code_and_project_language_text() {
        for (lang, name, expected) in [
            (crate::i18n::Lang::En, "bloqueio-en", "3 reviewer rejections"),
            (crate::i18n::Lang::PtBr, "bloqueio-pt", "3 reprovações do revisor"),
        ] {
            let fx = Fixture::new(name, "true");
            fx.add_task(&task("T01", TaskStatus::Todo, &[]));
            let (dev, rec) = (FakeDev::default(), Recorder::default());
            let rev = FakeReviewer::new(|_, _| rejected(vec![finding("F-01")]));
            let out = fx
                .runner(&dev, &rev, ScriptedTests::default(), &rec)
                .with_lang(crate::i18n::SharedLang::new(lang))
                .run_task(SPEC, "T01")
                .await
                .unwrap();
            assert!(matches!(&out, Outcome::Blocked { reason, .. } if reason == expected), "{out:?}");
            let t = fx.read_task("T01");
            assert_eq!(t.frontmatter.next_action.as_deref(), Some(expected));
            assert_eq!(t.frontmatter.next_action_code.as_deref(), Some("rejections"));
            let raw = std::fs::read_to_string(fx.task_path("T01")).unwrap();
            assert!(raw.contains("next_action_code:") && raw.contains("rejections"), "{raw}");
            if lang == crate::i18n::Lang::En {
                assert_no_pt(&[expected.to_string()]);
            }
        }
    }

    /// Spec 114 T02: tarefa BLOCKED antiga (motivo em pt-BR, sem código) continua lida e exibida
    /// como está, mesmo com o projeto em en; sem motivo, o genérico sai no idioma do projeto.
    #[tokio::test]
    async fn legacy_pt_block_reason_is_still_read() {
        let fx = Fixture::new("bloqueio-legado", "true");
        let mut old = task("T01", TaskStatus::Blocked, &[]);
        old.frontmatter.next_action =
            Some("impasse: o Desenvolvedor não alterou a branch desde a reprovação anterior (2 reprovações)".into());
        fx.add_task(&old);
        let mut bare = task("T02", TaskStatus::Blocked, &[]);
        bare.frontmatter.next_action = None;
        fx.add_task(&bare);
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).with_lang(en());
        let out = runner.run_task(SPEC, "T01").await.unwrap();
        assert!(matches!(&out, Outcome::Blocked { reason, .. } if reason.starts_with("impasse: o Desenvolvedor")), "{out:?}");
        assert_eq!(fx.read_task("T01").frontmatter.next_action_code, None, "o legado não ganha código inventado");
        let out = runner.run_task(SPEC, "T02").await.unwrap();
        assert!(matches!(&out, Outcome::Blocked { reason, .. } if reason == "task BLOCKED"), "{out:?}");
    }

    /// Spec 114 T02 (sentinela): todo código de bloqueio tem texto nos dois dicionários, o en sem
    /// português e o pt-BR diferente do en; o revisado é reconhecido pelo código e pelo legado.
    #[test]
    fn every_block_code_has_both_languages_without_portuguese_in_english() {
        use crate::i18n::lookup;
        for code in BLOCK_CODES.iter().filter(|c| **c != "budget") {
            let base = format!("runner.block.{code}");
            // Com contagem, o texto vem no plural (spec 115 T01): as duas formas.
            let keys: Vec<String> = if lookup(Lang::En, &format!("{base}.one")).is_some() {
                vec![format!("{base}.one"), format!("{base}.other")]
            } else {
                vec![base]
            };
            for key in keys {
                let en = lookup(Lang::En, &key).unwrap_or_else(|| panic!("sem en: {key}"));
                let pt = lookup(Lang::PtBr, &key).unwrap_or_else(|| panic!("sem pt-BR: {key}"));
                assert_ne!(en, pt, "{key}");
                assert_no_pt(&[en.to_string()]);
                assert!(!en.chars().any(|c| "ãõçáéíóúâêôà".contains(c)), "{key}: {en}");
            }
        }
        assert_eq!(BlockReason::counted(Lang::En, "rejections", 1).text, "1 reviewer rejection");
        assert_eq!(BlockReason::counted(Lang::PtBr, "tests_failed", 3).text, "test_command falhou 3 vezes na mesma tentativa");
        for key in ["runner.budget.tokens", "runner.budget.usd", "runner.final_fix.reason"] {
            assert_no_pt(&[lookup(Lang::En, key).unwrap_or_default().to_string()]);
        }
        let mut meta = task("T01", TaskStatus::Blocked, &[]).frontmatter;
        meta.next_action = Some(REVISED_NEXT_ACTION.into());
        assert!(is_revised_block(&meta), "legado pt-BR sem código");
        meta.next_action = Some(BlockReason::new(Lang::En, BLOCK_REVISED, &[]).text);
        meta.next_action_code = Some(BLOCK_REVISED.into());
        assert!(is_revised_block(&meta), "código atual, texto em en");
        meta.next_action = Some(REVISED_NEXT_ACTION.into());
        meta.next_action_code = Some("rejections".into());
        assert!(!is_revised_block(&meta), "o código vence o texto");
    }

    /// Spec 107 T01: tarefa gravada antes da marca (só o título, em pt-BR ou en) ainda marca a 2ª rodada.
    async fn legacy_fix_marks_second_round(title: &str, name: &str) {
        let fx = Fixture::new(name, "true");
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let mut old = task("T02", TaskStatus::Todo, &["T01"]);
        old.frontmatter.title = title.into();
        assert_eq!(old.frontmatter.kind, None);
        fx.add_task(&old);
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| if i.task_id == "FINAL" { matrix(false) } else { approved(vec![]) });
        let out = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_spec(SPEC).await.unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        assert!(!fx.task_path("T03").exists(), "tarefa antiga reconhecida: sem nova correção");
        assert_eq!(rev.inputs().iter().filter(|i| i.task_id == "FINAL").count(), 1);
        assert!(rec.events().into_iter().any(|e| matches!(e, RunnerEvent::SystemNotice { code, .. } if code == "final.still_unmet")));
        assert!(!rec.events().into_iter().any(|e| matches!(e, RunnerEvent::SystemNotice { code, .. } if code == "final.fix_created")));
    }

    #[tokio::test]
    async fn legacy_fix_task_with_pt_br_title_marks_second_round() {
        legacy_fix_marks_second_round("Correção da verificação final", "final-legado-pt").await;
    }

    #[tokio::test]
    async fn legacy_fix_task_with_en_title_marks_second_round() {
        legacy_fix_marks_second_round("Final verification fix", "final-legado-en").await;
    }

    #[test]
    fn is_final_fix_reads_mark_or_title_in_both_languages() {
        let mut t = task("T05", TaskStatus::Todo, &[]);
        assert!(!is_final_fix(&t));
        t.frontmatter.kind = Some(crate::sdd::TASK_KIND_FINAL_FIX.into());
        assert!(is_final_fix(&t), "marca estável, qualquer título");
        t.frontmatter.kind = None;
        for title in ["Correção da verificação final", "Final verification fix"] {
            t.frontmatter.title = title.into();
            assert!(is_final_fix(&t), "{title}");
        }
        assert_eq!(final_fix_title(crate::i18n::Lang::PtBr), "Correção da verificação final");
        assert_eq!(final_fix_title(crate::i18n::Lang::En), "Final verification fix");
    }

    #[tokio::test]
    async fn final_verification_second_round_copies_final_2_and_keeps_final_1() {
        // Spec 084 T01: depois da tarefa de correção, a 2ª rodada grava `final-2-…` sem sobrescrever a 1ª.
        let fx = Fixture::new("final-shots", WRITES_SCREENSHOT);
        set_screenshots(&fx, &["shots/*.png"]);
        with_globals(&fx);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let (dev, rec) = (FakeDev::default(), Recorder::default());
        let rev = FakeReviewer::new(|_, i| {
            let base = if i.task_id == "FINAL" { matrix(false) } else { approved(vec![]) };
            ReviewResult { screenshots_viewed: i.screenshots.iter().filter(|c| c.usable()).map(|c| c.name.clone()).collect(), ..base }
        });
        let gate = Arc::new(Gate(true, Default::default()));
        let out = fx
            .runner(&dev, &rev, ShellTestExecutor::new(), &rec)
            .with_final_gate(gate)
            .run_spec(SPEC)
            .await
            .unwrap();
        assert_eq!(out, SpecOutcome::Completed);
        assert_eq!(fx.read_task("T02").frontmatter.title, "Correção da verificação final");
        let finals: Vec<_> = rev.inputs().into_iter().filter(|i| i.task_id == "FINAL").collect();
        assert_eq!(finals.len(), 2);
        assert!(finals.iter().all(|i| i.screenshots.iter().any(|c| c.name == "shots/home.png")));
        let captures = fx.repo.join(".sdd/specs").join(SPEC).join("captures");
        assert!(captures.join("final-1-home.png").is_file() && captures.join("final-2-home.png").is_file());
        let texts = reviewer_texts(&rec).concat();
        assert!(texts.contains("Capturas avaliadas: final-1-home.png"), "{texts}");
        assert!(texts.contains("Capturas avaliadas: final-2-home.png"), "{texts}");
    }

    #[tokio::test]
    async fn run_spec_stalls_on_missing_dependency_and_run_task_rejects_it() {
        let fx = Fixture::new("spec-parada", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &["T09"]));
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let runner = fx.runner(&dev, &rev, ScriptedTests::default(), &rec);

        let outcome = runner.run_spec(SPEC).await.unwrap();
        assert_eq!(
            outcome,
            SpecOutcome::Stalled {
                pending: vec!["T01".into()]
            }
        );
        assert!(matches!(
            runner.run_task(SPEC, "T01").await,
            Err(RunnerError::DependenciesPending { .. })
        ));
        assert!(dev.briefs().is_empty());
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Todo);
        assert!(!fx.worktree("T01").exists());
    }

    #[tokio::test]
    async fn invalid_ids_are_rejected_before_touching_disk() {
        let fx = Fixture::new("ids", "true");
        let runner = fx.runner(
            &FakeDev::default(),
            &FakeReviewer::new(|_, _| approved(vec![])),
            ScriptedTests::default(),
            &Recorder::default(),
        );
        assert!(matches!(
            runner.run_task("../x", "T01").await,
            Err(RunnerError::InvalidId { name: "spec_id", .. })
        ));
        assert!(matches!(
            runner.run_task(SPEC, "--all").await,
            Err(RunnerError::InvalidId { name: "task_id", .. })
        ));
    }

    #[tokio::test]
    async fn run_task_rejects_empty_test_command() {
        let fx = Fixture::new("sem-teste", "  ");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let runner = fx.runner(
            &FakeDev::default(),
            &FakeReviewer::new(|_, _| approved(vec![])),
            ScriptedTests::default(),
            &Recorder::default(),
        );
        assert!(matches!(
            runner.run_task(SPEC, "T01").await,
            Err(RunnerError::Config(_))
        ));
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Todo);
    }

    #[tokio::test]
    async fn shell_executor_reports_status_and_output() {
        let dir = env::temp_dir().join(format!("nants-runner-{}-shell", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ex = ShellTestExecutor::new();
        let ok = ex.run(&dir, "echo oi").await.unwrap();
        assert!(ok.passed);
        assert!(ok.output.contains("oi"));
        let bad = ex.run(&dir, "echo quebrou; exit 3").await.unwrap();
        assert!(!bad.passed);
        assert!(bad.output.contains("quebrou"));
        let cwd = ex.run(&dir, "pwd").await.unwrap();
        assert!(cwd.output.contains(dir.file_name().unwrap().to_str().unwrap()));
    }

    #[test]
    fn shell_missing_command_exits_127_and_header_is_structured() {
        let r = run_shell(&env::temp_dir(), "comando_que_nao_existe_xyz", Duration::from_secs(10), Lang::PtBr).unwrap();
        assert!(!r.passed);
        assert_eq!(r.exit_code, Some(127));
        assert_eq!(failure_header(r.exit_code, r.timed_out), "[falha dos testes: código 127]\n");
        assert_eq!(failure_header(None, true), "[falha dos testes: tempo limite]\n");
        assert_eq!(failure_header(None, false), "[falha dos testes]\n");
    }

    #[tokio::test]
    async fn shell_executor_times_out() {
        let dir = env::temp_dir();
        let ex = ShellTestExecutor::with_timeout(Duration::from_millis(300));
        let started = Instant::now();
        let r = ex.run(&dir, "sleep 5").await.unwrap();
        assert!(!r.passed);
        assert!(r.output.contains("tempo limite"));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    /// Spec 115 T04: as marcas do executor (corte e tempo limite) saem no idioma do projeto.
    #[tokio::test]
    async fn shell_executor_marks_follow_the_project_language() {
        let dir = env::temp_dir();
        let lang = i18n::SharedLang::new(Lang::En);
        let ex = ShellTestExecutor::with_timeout(Duration::from_millis(300)).with_lang(lang.clone());
        let r = ex.run(&dir, "sleep 5").await.unwrap();
        assert!(r.output.contains("[time limit of 0s exceeded]") && !r.output.contains("tempo limite"), "{}", r.output);
        let big = format!("head -c {} /dev/zero | tr '\\0' a", MAX_TEST_OUTPUT_BYTES + 10);
        let ex = ShellTestExecutor::new().with_lang(lang.clone());
        let r = ex.run(&dir, &big).await.unwrap();
        assert!(r.output.ends_with("[output truncated at 64 KiB]\n"), "{}", &r.output[r.output.len() - 60..]);
        lang.set(Lang::PtBr);
        let r = ex.run(&dir, &big).await.unwrap();
        assert!(r.output.ends_with("[saída truncada em 64 KiB]\n"));
    }

    #[tokio::test]
    async fn shell_executor_truncates_output() {
        let dir = env::temp_dir();
        let ex = ShellTestExecutor::new();
        let r = ex.run(&dir, "yes | head -c 200000").await.unwrap();
        assert!(r.passed);
        assert!(r.output.contains("truncada"));
        assert!(r.output.len() <= MAX_TEST_OUTPUT_BYTES + 100);
    }

    #[test]
    fn dev_notes_mark_superseded_analyses_and_yolo_has_no_pending_human() {
        let body = "## Objetivo\nx\n\n## Análise de recuperação #1\n- Causa raiz: a\n- Decisão: escalar ao humano\n\n## Análise de recuperação #2\n- Causa raiz: b\n- Decisão: nova estratégia (RETRY)\n";
        let plain = recovery_notes_for_dev(body, false);
        assert!(plain.contains("## Análise de recuperação #1 (superada pela análise #2)"), "{plain}");
        assert!(plain.contains("## Análise de recuperação #2\n"), "a mais recente não é marcada: {plain}");
        assert!(plain.contains("- Decisão: escalar ao humano"));
        assert!(!plain.contains("Não há decisão pendente"));
        let yolo = recovery_notes_for_dev(body, true);
        assert!(yolo.contains("Não há decisão pendente do humano; siga a estratégia da análise mais recente"), "{yolo}");
        assert!(!yolo.contains("escalar ao humano"), "{yolo}");
        assert!(body.contains("escalar ao humano"), "corpo em disco intacto");
        assert_eq!(recovery_notes_for_dev("## Objetivo\nx\n", true), "");
    }

    #[test]
    fn recovery_cycles_count_only_after_last_revision_mark() {
        let body = "## Objetivo\nx\n\n## Análise de recuperação #1\n- a\n\n## Análise de recuperação #2\n- b\n";
        assert_eq!(recovery_cycles(body), 2);
        let marked = mark_revised(body);
        assert_eq!(recovery_cycles(&marked), 0);
        assert_eq!(mark_revised(&marked), marked, "idempotente");
        assert!(marked.contains("## Análise de recuperação #2"), "histórico preservado");
        let more = format!("{marked}\n## Análise de recuperação #3\n- c\n");
        assert_eq!(recovery_cycles(&more), 1);
        assert_eq!(recovery_total(&more), 3);
    }

    /// Trecho real da T02 do App Test 17: #1 e #2 são escaladas antigas (formato sem marca, causa de
    /// ambiente) e #3 e #4 são o limite de ciclos ("3 de 2").
    const APP_TEST_17_T02: &str = "## Objetivo\nx\n\n## Análise de recuperação #1\n- Causa raiz: ambiente: o comando de testes não executou\n- Decisão: escalar ao humano\n\n## Análise de recuperação #2\n- Causa raiz: ambiente: o comando de testes não executou\n- Decisão: escalar ao humano\n\n## Análise de recuperação #3\n- Causa raiz: limite de ciclos de recuperação atingido\n- Decisão: escalar ao humano\n- Contexto:\n  - comando: npm test\n  - código: 127\n  - o que foi tentado: 2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas\n  - por que não resolveu: limite de ciclos de recuperação atingido\n\n## Análise de recuperação #4\n- Causa raiz: limite de ciclos de recuperação atingido\n- Decisão: escalar ao humano\n- Contexto:\n  - comando: npm test\n  - código: 127\n  - o que foi tentado: 3 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas\n  - por que não resolveu: limite de ciclos de recuperação atingido\n";

    #[test]
    fn recovery_cycles_ignore_deterministic_escalations() {
        // Formato real do App Test 17: só #1 e #2 (indistinguíveis de análises do modelo) contam.
        assert_eq!(recovery_cycles(APP_TEST_17_T02), 2);
        assert_eq!(recovery_total(APP_TEST_17_T02), 4);
        // Seção nova marcada como determinística não conta, em qualquer posição.
        let body = format!(
            "## Análise de recuperação #1\n- a\n\n## Análise de recuperação #2\n- Causa raiz: x\n{NOT_COUNTED_LINE}\n\n## Análise de recuperação #3\n- c\n"
        );
        assert_eq!(recovery_cycles(&body), 2);
    }

    #[test]
    fn forged_limit_cause_in_a_retry_section_still_counts() {
        let body = "## Análise de recuperação #1\n- Causa raiz: limite de ciclos de recuperação atingido\n- Decisão: nova estratégia (RETRY)\n";
        assert_eq!(recovery_cycles(body), 1);
    }

    #[test]
    fn human_resume_mark_zeroes_cycles_and_keeps_history() {
        let resumed = mark_human_resumed(APP_TEST_17_T02);
        assert_eq!(recovery_cycles(&resumed), 0);
        assert_eq!(mark_human_resumed(&resumed), resumed, "idempotente");
        assert!(resumed.contains("## Análise de recuperação #4"), "histórico preservado");
        // A próxima falha vira o ciclo #5 (numeração sem colisão) e é a 1ª contada.
        let next = format!("{resumed}\n## Análise de recuperação #5\n- nova\n");
        assert_eq!(recovery_cycles(&next), 1);
        assert_eq!(recovery_total(&next), 5);
    }

    #[test]
    fn findings_summary_redacts_secret_crossing_the_cut() {
        let secret = "sk-segredo-abcdef123456";
        let mut f = finding("F-01");
        f.description = format!("{}{secret} fim", "a".repeat(200));
        let redact = |t: &str| t.replace(secret, "[REDACTED]");
        let summary = findings_summary(&[f], &redact);
        assert!(!summary.contains("sk-segredo") && !summary.contains("abcdef"), "{summary}");
        let mut g = finding("F-02");
        g.description = "x  sk-segredo-\n abcdef123456".to_string();
        let sp = |t: &str| t.replace("sk-segredo-\n abcdef123456", "[REDACTED]");
        assert!(findings_summary(&[g], &sp).contains("[REDACTED]"));
    }

    #[test]
    fn findings_log_is_flat_bounded_and_keeps_recent() {
        let mut f = finding("F-01");
        f.description = "linha1\n## injetado\nlinha3".repeat(40);
        let summary = findings_summary(&[f], &|t| t.to_string());
        assert_eq!(summary.lines().count(), 1, "uma linha por finding");
        assert!(summary.len() <= FINDINGS_LOG_ITEM_BYTES + 3);
        assert_eq!(append_findings_log("", 1, "  "), None, "nada a registrar");
        let mut acc = String::new();
        for n in 1..=30 {
            acc = append_findings_log(&acc, n, &summary).expect("acc");
        }
        assert!(acc.len() <= FINDINGS_LOG_MAX_BYTES + 16, "{}", acc.len());
        assert!(acc.contains("### Reprovação 30") && !acc.contains("### Reprovação 1\n"));
    }

    #[test]
    fn replace_section_adds_replaces_and_removes() {
        let body = "## Objetivo\nx\n\n## Critérios de Aceitação\n- a\n";
        let f1 = render_findings(&[finding("F-01")]).unwrap();
        let with = replace_section(body, FINDINGS, f1.clone());
        assert_eq!(read_findings(&with).unwrap(), vec![finding("F-01")]);
        assert_eq!(replace_section(&with, FINDINGS, f1.clone()), with, "idempotente");
        let f2 = render_findings(&[finding("F-02")]).unwrap();
        let swapped = replace_section(&with, FINDINGS, f2);
        assert_eq!(read_findings(&swapped).unwrap(), vec![finding("F-02")]);
        assert_eq!(swapped.matches("## Findings Abertos").count(), 1);
        let removed = replace_section(&swapped, FINDINGS, None);
        assert!(read_findings(&removed).unwrap().is_empty());
        assert_eq!(section_text(&removed, OBJECTIVE).unwrap().trim(), "x");
        assert_eq!(acceptance_of(&removed), "- a");
    }

    #[test]
    fn read_findings_rejects_malformed_section() {
        let body = "## Findings Abertos\nisto não é lista\n";
        assert!(matches!(read_findings(body), Err(RunnerError::Invalid(_))));
    }

    #[tokio::test]
    async fn done_on_disk_without_commit_is_not_released_to_dependents() {
        let fx = Fixture::new("done-sem-commit", "true");
        fx.add_task(&task("T02", TaskStatus::Todo, &["T01"]));
        let mut t1 = task("T01", TaskStatus::Done, &[]);
        t1.frontmatter.branch = branch_name(SPEC, "T01");
        fx.put_task(&t1);
        // Squash remanescente de outra coisa no índice: não é exatamente o squash da branch.
        fs::write(fx.repo.join("extra.txt"), "x\n").unwrap();
        git_in(&fx.repo, &["add", "extra.txt"]);
        let head_before = fx.head();
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &Recorder::default())
            .run_spec(SPEC)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            SpecOutcome::Blocked { ref task_id, .. } if task_id == "T01"
        ));
        assert!(dev.briefs().is_empty(), "T02 não foi liberada nem executada");
        assert_eq!(fx.read_task("T02").frontmatter.status, TaskStatus::Todo);
        assert_eq!(fx.head(), head_before, "base intocada");
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
        assert_eq!(
            git_in(&fx.repo, &["diff", "--cached", "--name-only"]),
            "extra.txt",
            "índice alheio preservado"
        );
    }

    #[tokio::test]
    async fn empty_squash_integrates_with_empty_commit() {
        let fx = Fixture::new("squash-vazio", "true");
        let wt = fx.worktree("T01");
        git_in(
            &fx.repo,
            &["worktree", "add", "-B", &branch_name(SPEC, "T01"), wt.to_str().unwrap(), "main"],
        );
        let head = git_in(&fx.repo, &["rev-parse", &branch_name(SPEC, "T01")]);
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        t.frontmatter.last_verdict = Verdict::Approved;
        fx.put_task(&t);
        let outcome = fx
            .runner(
                &FakeDev::default(),
                &FakeReviewer::new(|_, _| approved(vec![])),
                ScriptedTests::default(),
                &Recorder::default(),
            )
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done);
        assert_eq!(fx.feat_commits(), 1);
        assert_eq!(git_in(&fx.repo, &["show", "--name-only", "--format=", "HEAD"]), "");
        let t = fx.read_task("T01");
        assert_eq!(t.frontmatter.status, TaskStatus::Done);
        assert_eq!(
            t.frontmatter.next_action.as_deref(),
            Some("sem alterações: já presente na base")
        );
    }

    /// Desenvolvedor que não altera nada (tarefa já satisfeita pela base).
    struct NoopDev;

    impl DeveloperPort for NoopDev {
        fn implement<'a>(&'a self, _brief: &'a DevBrief) -> Fut<'a, Result<DevReport, RunnerError>> {
            Box::pin(async move {
                Ok(DevReport {
                    summary: "nada a fazer".into(),
                    exhausted: false,
                })
            })
        }
    }

    #[tokio::test]
    async fn resumed_task_blocked_by_failed_integration_commit_with_identical_branch_finishes() {
        let fx = Fixture::new("retomar-vazio", "true");
        let wt = fx.worktree("T01");
        git_in(
            &fx.repo,
            &["worktree", "add", "-B", &branch_name(SPEC, "T01"), wt.to_str().unwrap(), "main"],
        );
        let head = git_in(&fx.repo, &["rev-parse", &branch_name(SPEC, "T01")]);
        // Estado que o Retomar grava (reset_plan): BLOCKED -> TODO, attempts 0, next_action limpo;
        // branch, reviewed_sha e veredito anteriores permanecem.
        let mut t = task("T01", TaskStatus::Todo, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        t.frontmatter.last_verdict = Verdict::Approved;
        t.frontmatter.attempts = 0;
        t.frontmatter.next_action = None;
        fx.put_task(&t);
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let outcome = fx
            .noop_runner(&rev)
            .run_spec(SPEC)
            .await
            .unwrap();

        assert_eq!(outcome, SpecOutcome::Completed);
        assert_eq!(fx.feat_commits(), 1);
        assert_eq!(git_in(&fx.repo, &["show", "--name-only", "--format=", "HEAD"]), "");
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn done_on_disk_without_commit_reintegrates_with_empty_index() {
        let fx = Fixture::new("done-reintegra", "true");
        let head = fx.leftover_work("T01");
        fx.put_task(&task("T02", TaskStatus::Todo, &["T01"]));
        let mut t1 = task("T01", TaskStatus::Done, &[]);
        t1.frontmatter.branch = branch_name(SPEC, "T01");
        t1.frontmatter.reviewed_sha = Some(head);
        t1.frontmatter.last_verdict = Verdict::Approved;
        fx.put_task(&t1);
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &Recorder::default())
            .run_spec(SPEC)
            .await
            .unwrap();

        assert_eq!(outcome, SpecOutcome::Completed);
        assert_eq!(fx.feat_commits(), 2, "T01 integrada uma vez, T02 depois");
        assert_eq!(
            dev.briefs().iter().map(|b| b.task_id.as_str()).collect::<Vec<_>>(),
            ["T02"],
            "só T02 passou pelo developer"
        );
        assert!(rev.inputs().iter().all(|i| i.task_id == "T02"));
    }

    #[tokio::test]
    async fn done_on_disk_without_commit_with_same_name_staged_by_human_blocks() {
        let fx = Fixture::new("done-staged-humano", "true");
        let head = fx.leftover_work("T01");
        let mut t1 = task("T01", TaskStatus::Done, &[]);
        t1.frontmatter.branch = branch_name(SPEC, "T01");
        t1.frontmatter.reviewed_sha = Some(head);
        t1.frontmatter.last_verdict = Verdict::Approved;
        fx.put_task(&t1);
        // O humano stage um arquivo com o mesmo nome que a branch cria.
        fs::write(fx.repo.join("out-T01-1.txt"), "humano\n").unwrap();
        git_in(&fx.repo, &["add", "out-T01-1.txt"]);
        let head_before = fx.head();
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Blocked { .. }));
        assert_eq!(fs::read_to_string(fx.repo.join("out-T01-1.txt")).unwrap(), "humano\n");
        assert_eq!(
            git_in(&fx.repo, &["diff", "--cached", "--name-only"]),
            "out-T01-1.txt",
            "índice intacto"
        );
        assert_eq!(fx.head(), head_before, "base intocada");
        assert!(dev.briefs().is_empty());
        assert!(rev.inputs().is_empty());
    }

    #[tokio::test]
    async fn squash_conflict_rolls_back_without_escaping() {
        let fx = Fixture::new("conflito", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let wt = fx.worktree("T01");
        git_in(
            &fx.repo,
            &["worktree", "add", "-B", &branch_name(SPEC, "T01"), wt.to_str().unwrap(), "main"],
        );
        fs::write(wt.join("README.md"), "branch\n").unwrap();
        fs::write(wt.join("novo.txt"), "branch\n").unwrap();
        git_in(&wt, &["add", "-A"]);
        git_in(&wt, &["commit", "-m", "chore: branch"]);
        // A base avança alterando os mesmos arquivos: squash conflitante.
        fs::write(fx.repo.join("README.md"), "main\n").unwrap();
        fs::write(fx.repo.join("novo.txt"), "main\n").unwrap();
        git_in(&fx.repo, &["add", "README.md", "novo.txt"]);
        git_in(&fx.repo, &["commit", "-m", "chore: main avança"]);
        let head = git_in(&fx.repo, &["rev-parse", &branch_name(SPEC, "T01")]);
        let main_head = fx.head();
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        t.frontmatter.last_verdict = Verdict::Approved;
        fx.put_task(&t);
        let outcome = fx
            .runner(
                &FakeDev::default(),
                &FakeReviewer::new(|_, _| approved(vec![])),
                ScriptedTests::default(),
                &Recorder::default(),
            )
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            Outcome::Blocked { ref reason, .. } if reason.starts_with("squash merge falhou")
        ));
        assert_eq!(git_in(&fx.repo, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(fs::read_to_string(fx.repo.join("README.md")).unwrap(), "main\n");
        assert!(!fx.repo.join("novo.txt").exists() || fs::read_to_string(fx.repo.join("novo.txt")).unwrap() == "main\n");
        assert_eq!(fx.head(), main_head, "base intocada");
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn unstaged_spec_edits_do_not_block_next_integration() {
        let fx = Fixture::new("estado-trabalho", "true");
        let head = fx.leftover_work("T01");
        git_in(&fx.repo, &["merge", "--squash", &branch_name(SPEC, "T01")]);
        git_in(&fx.repo, &["commit", "-m", "feat(s1): T01 - Título T01"]);
        let mut t1 = task("T01", TaskStatus::Integrating, &[]);
        t1.frontmatter.branch = branch_name(SPEC, "T01");
        t1.frontmatter.reviewed_sha = Some(head);
        fx.put_task(&t1);
        let rec = Recorder::default();
        let runner = fx.runner(
            &FakeDev::default(),
            &FakeReviewer::new(|_, _| approved(vec![])),
            ScriptedTests::default(),
            &rec,
        );
        assert_eq!(runner.run_task(SPEC, "T01").await.unwrap(), Outcome::Done);
        // DONE gravado sem commit (estado de trabalho) e spec COMPLETED não commitado.
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
        let spec_path = fx.repo.join(".sdd/specs/s1/spec.md");
        let spec_text = fs::read_to_string(&spec_path).unwrap();
        fs::write(&spec_path, format!("{spec_text}\nnota de estado\n")).unwrap();
        fx.put_task(&task("T02", TaskStatus::Todo, &[]));

        let outcome = runner.run_task(SPEC, "T02").await.unwrap();
        assert_eq!(outcome, Outcome::Done, "estado de trabalho não bloqueia");
        assert_eq!(fx.feat_commits(), 2);
    }

    #[tokio::test]
    async fn dirty_skill_profile_does_not_block_integration() {
        let fx = Fixture::new("perfil-sujo", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        fs::create_dir_all(fx.repo.join(".sdd/skills")).unwrap();
        fs::write(fx.repo.join(".sdd/skills/developer.md"), "editado à mão\n").unwrap();
        let outcome = fx
            .runner(&FakeDev::default(), &FakeReviewer::new(|_, _| approved(vec![])), ScriptedTests::default(), &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Done, "perfil sujo não bloqueia");
    }

    #[tokio::test]
    async fn external_edit_during_review_blocks_without_overwrite() {
        let fx = Fixture::new("edicao-externa", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let path = fx.task_path("T01");
        let rev = FakeReviewer::new(move |_, _| {
            let mut text = fs::read_to_string(&path).unwrap();
            text.push_str("# editado pelo humano\n");
            fs::write(&path, text).unwrap();
            approved(vec![])
        });
        let outcome = fx
            .runner(&FakeDev::default(), &rev, ScriptedTests::default(), &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Blocked {
                reason: ".sdd/specs/s1/tasks/T01.md modificado externamente".into(),
                kind: BlockKind::Environmental,
                open_findings: vec![],
            }
        );
        assert!(fs::read_to_string(fx.task_path("T01"))
            .unwrap()
            .ends_with("# editado pelo humano\n"));
        assert_eq!(fx.feat_commits(), 0);
    }

    #[tokio::test]
    async fn plain_directory_at_worktree_path_is_never_reused_or_committed() {
        let fx = Fixture::new("dir-comum", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        fs::create_dir_all(fx.worktree("T01")).unwrap();
        fs::write(fx.worktree("T01").join("sujo.txt"), "x\n").unwrap();
        let head_before = fx.head();
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let result = fx
            .runner(&dev, &rev, ScriptedTests::default(), &Recorder::default())
            .run_task(SPEC, "T01")
            .await;

        assert!(matches!(result, Err(RunnerError::Invalid(_))));
        assert!(dev.briefs().is_empty(), "developer não acionado");
        assert_eq!(fx.head(), head_before, "nenhum commit na base");
        assert!(fx.worktree("T01").join("sujo.txt").exists());
    }

    #[test]
    fn worktree_paths_do_not_collide_across_spec_and_task_boundaries() {
        let fx = Fixture::new("colisao", "true");
        let runner = fx.runner(
            &FakeDev::default(),
            &FakeReviewer::new(|_, _| approved(vec![])),
            ScriptedTests::default(),
            &Recorder::default(),
        );
        assert_ne!(
            runner.worktree_path("a-b", "c"),
            runner.worktree_path("a", "b-c")
        );
        assert_eq!(runner.worktree_path(SPEC, "T01"), fx.worktree("T01"));
    }

    #[tokio::test]
    async fn unstaged_human_edit_in_other_sdd_file_blocks_integration() {
        let fx = Fixture::new("sdd-humano", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let notas = fx.repo.join(".sdd/notas.md");
        fs::write(&notas, "nota do humano\n").unwrap();
        let head_before = fx.head();
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Blocked {
                reason: "alterações não commitadas em .sdd: .sdd/notas.md".into(),
                kind: BlockKind::Environmental,
                open_findings: vec![],
            }
        );
        assert_eq!(fs::read_to_string(&notas).unwrap(), "nota do humano\n");
        assert_eq!(fx.head(), head_before);
        assert_eq!(fx.feat_commits(), 0);
    }

    #[tokio::test]
    async fn unstaged_settings_edit_does_not_block_nor_enter_the_squash_commit() {
        // Configuração local: `.sdd/settings.yaml` versionado e modificado, não staged.
        let fx = Fixture::new("settings-nao-staged", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let human = format!(
            "{}# nota do humano\n",
            fs::read_to_string(fx.settings_path()).unwrap()
        );
        fs::write(fx.settings_path(), &human).unwrap();
        let outcome = fx
            .runner(
                &FakeDev::default(),
                &FakeReviewer::new(|_, _| approved(vec![])),
                ScriptedTests::default(),
                &Recorder::default(),
            )
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done, "settings modificado não bloqueia");
        let last_commit = git_in(&fx.repo, &["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(last_commit, "out-T01-1.txt", "commit contém somente o squash");
        assert_eq!(fs::read_to_string(fx.settings_path()).unwrap(), human, "edição preservada");
        assert!(git_in(&fx.repo, &["diff", "--cached", "--name-only"]).is_empty());
        assert_eq!(fx.feat_commits(), 1);
    }

    #[tokio::test]
    async fn branch_that_changes_settings_is_refused_before_squash() {
        let fx = Fixture::new("branch-settings", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let branch = branch_name(SPEC, "T01");
        let wt = fx.worktree("T01");
        git_in(
            &fx.repo,
            &["worktree", "add", "-B", &branch, wt.to_str().unwrap(), "main"],
        );
        let original = fs::read_to_string(fx.settings_path()).unwrap();
        fs::write(wt.join(".sdd/settings.yaml"), format!("{original}# alterado pelo dev\n")).unwrap();
        git_in(&wt, &["add", "-A"]);
        git_in(&wt, &["commit", "-m", "chore: dev altera settings"]);
        let head = git_in(&fx.repo, &["rev-parse", &branch]);
        let mut t = task("T01", TaskStatus::Approved, &[]);
        t.frontmatter.branch = branch.clone();
        t.frontmatter.reviewed_sha = Some(head);
        t.frontmatter.last_verdict = Verdict::Approved;
        fx.put_task(&t);
        let head_before = fx.head();

        let outcome = fx
            .runner(
                &FakeDev::default(),
                &FakeReviewer::new(|_, _| approved(vec![])),
                ScriptedTests::default(),
                &Recorder::default(),
            )
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Blocked {
                reason: "a branch altera .sdd/settings.yaml; integração recusada".into(),
                kind: BlockKind::Environmental,
                open_findings: vec![],
            }
        );
        assert_eq!(fx.head(), head_before, "base intocada");
        assert!(git_in(&fx.repo, &["diff", "--cached", "--name-only"]).is_empty(), "nada staged");
        assert_eq!(fs::read_to_string(fx.settings_path()).unwrap(), original, "settings da base intacto");
        assert_eq!(fx.feat_commits(), 0);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn staged_human_change_blocks_before_squash() {
        let fx = Fixture::new("staged-humano", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let human = format!(
            "# edição do humano\n{}",
            fs::read_to_string(fx.settings_path()).unwrap()
        );
        fs::write(fx.settings_path(), &human).unwrap();
        git_in(&fx.repo, &["add", ".sdd/settings.yaml"]);
        let head_before = fx.head();
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Blocked {
                reason: "índice da base com alterações staged; resolva manualmente: git restore --staged --worktree <paths> ou git commit; depois reexecute".into(),
                kind: BlockKind::Environmental,
                open_findings: vec![],
            }
        );
        assert_eq!(fx.head(), head_before, "base intocada");
        assert_eq!(
            git_in(&fx.repo, &["diff", "--cached", "--name-only"]),
            ".sdd/settings.yaml",
            "edição staged do humano preservada"
        );
        assert_eq!(fx.feat_commits(), 0);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn failed_base_tests_roll_back_squash_and_keep_human_edits() {
        let fx = Fixture::new("rollback", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let human = format!(
            "# edição do humano\n{}",
            fs::read_to_string(fx.settings_path()).unwrap()
        );
        let dev = FakeDev::default();
        let rev = base_moving_reviewer(fx.repo.clone());
        // Chamada 0: teste do dev (passa). Chamada 1: teste na base (falha) após o squash.
        let tests = ScriptedTests::new(vec![true, false]).with_effect(
            1,
            fx.settings_path(),
            &human,
        );
        let outcome = fx
            .runner(&dev, &rev, tests, &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        // Edição do humano em `.sdd/settings.yaml` (configuração local) não impede o rollback, que
        // só desfaz os caminhos do squash; a edição é preservada.
        assert_eq!(
            outcome,
            Outcome::Blocked {
                reason: "testes na base falharam após o squash".into(),
                kind: BlockKind::Quality,
                open_findings: vec![],
            }
        );
        assert_eq!(fs::read_to_string(fx.settings_path()).unwrap(), human, "edição preservada");
        assert!(!fx.repo.join("out-T01-1.txt").exists(), "squash desfeito");
        assert!(git_in(&fx.repo, &["diff", "--cached", "--name-only"]).is_empty());
        assert_eq!(fx.main_subjects()[0], "chore: base andou", "base intocada pela integração");
        assert_eq!(fx.feat_commits(), 0);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn human_edit_inside_squash_path_during_base_tests_blocks_and_keeps_file() {
        let fx = Fixture::new("edita-squash", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let squashed = fx.repo.join("out-T01-1.txt");
        // Chamada 0: teste do dev (passa). Chamada 1: teste na base; o humano edita o arquivo
        // do squash durante essa execução e depois o teste falha.
        let tests = ScriptedTests::new(vec![true, false]).with_effect(1, squashed.clone(), "humano\n");
        let outcome = fx
            .runner(&FakeDev::default(), &base_moving_reviewer(fx.repo.clone()), tests, &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            Outcome::Blocked { ref reason, .. }
                if reason.starts_with("testes na base falharam após o squash; rollback do squash não concluído")
        ));
        assert_eq!(fs::read_to_string(&squashed).unwrap(), "humano\n", "edição preservada");
        assert_eq!(
            git_in(&fx.repo, &["diff", "--cached", "--name-only"]),
            "out-T01-1.txt",
            "squash mantido no índice para conferência"
        );
        assert_eq!(fx.feat_commits(), 0);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn failed_base_tests_roll_back_squash_without_human_edits() {
        let fx = Fixture::new("rollback-limpo", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = base_moving_reviewer(fx.repo.clone());
        let tests = ScriptedTests::new(vec![true, false]);
        let outcome = fx
            .runner(&dev, &rev, tests, &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Blocked {
                reason: "testes na base falharam após o squash".into(),
                kind: BlockKind::Quality,
                open_findings: vec![],
            }
        );
        assert!(!fx.repo.join("out-T01-1.txt").exists(), "arquivo do squash removido");
        assert_eq!(git_in(&fx.repo, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(fx.main_subjects()[0], "chore: base andou", "base intocada pela integração");
        assert_eq!(fx.feat_commits(), 0);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn blocking_check_failing_on_base_rolls_back_and_blocks() {
        let fx = Fixture::new("check-base", "true");
        set_checks(&fx, vec![check("build", "true", true), check("lint", "true", false)]);
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let dev = FakeDev::default();
        let rev = base_moving_reviewer(fx.repo.clone());
        // worktree: teste, build, lint; base: teste, build (falha). Lint não roda na base.
        let tests = ScriptedTests::new(vec![true, true, true, true, false]);
        let outcome = fx
            .runner(&dev, &rev, tests, &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        assert_eq!(
            outcome,
            Outcome::Blocked {
                reason: "checagem build na base falhou após o squash".into(),
                kind: BlockKind::Quality,
                open_findings: vec![],
            }
        );
        assert_eq!(git_in(&fx.repo, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(fx.main_subjects()[0], "chore: base andou");
        assert_eq!(fx.feat_commits(), 0);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Blocked);
    }

    fn integrate_leftover(fx: &Fixture) {
        let head = fx.leftover_work("T01");
        git_in(&fx.repo, &["merge", "--squash", &branch_name(SPEC, "T01")]);
        git_in(&fx.repo, &["commit", "-m", "feat(s1): T01 - Título T01"]);
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        fx.put_task(&t);
    }

    #[tokio::test]
    async fn resume_with_orphan_worktree_dir_finishes_and_removes_it() {
        let fx = Fixture::new("orfa", "true");
        integrate_leftover(&fx);
        fx.add_task(&task("T02", TaskStatus::Todo, &["T01"]));
        // Desregistra a worktree mantendo a pasta e arquivos (inclusive ignorados).
        let wt = fx.worktree("T01");
        fs::remove_file(wt.join(".git")).unwrap();
        fs::remove_dir_all(fx.repo.join(".git/worktrees")).unwrap();
        fs::create_dir_all(wt.join("node_modules/x")).unwrap();
        fs::write(wt.join("node_modules/x/a.js"), "x").unwrap();
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let outcome = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_spec(SPEC).await.unwrap();
        assert_eq!(outcome, SpecOutcome::Completed);
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
        assert!(!wt.exists());
        assert!(!wt.parent().unwrap().exists(), "pasta vazia da spec removida");
        assert_eq!(dev.briefs().iter().map(|b| b.task_id.as_str()).collect::<Vec<_>>(), ["T02"]);
    }

    #[tokio::test]
    async fn resume_with_only_sdd_dirty_registered_worktree_finishes() {
        let fx = Fixture::new("sdd-sujo", "true");
        integrate_leftover(&fx);
        let wt = fx.worktree("T01");
        fs::create_dir_all(wt.join(".sdd")).unwrap();
        fs::write(wt.join(".sdd/estado.md"), "x").unwrap();
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let outcome = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert_eq!(outcome, Outcome::Done);
    }

    #[tokio::test]
    async fn untracked_sdd_in_worktree_does_not_block_pre_merge_integration() {
        let fx = Fixture::new("sdd-nao-rastreado", "true");
        fx.add_task(&task("T01", TaskStatus::Todo, &[]));
        let wt = fx.worktree("T01");
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(move |_, _| {
            fs::create_dir_all(wt.join(".sdd")).unwrap();
            fs::write(wt.join(".sdd/estado.md"), "x").unwrap();
            approved(vec![])
        });
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(fx.feat_commits(), 1);
    }

    #[tokio::test]
    async fn dirty_registered_worktree_block_publishes_system_message() {
        let fx = Fixture::new("msg-bloqueio", "true");
        integrate_leftover(&fx);
        fs::write(fx.worktree("T01").join("sujo.txt"), "x").unwrap();
        let (dev, rev, rec) = (FakeDev::default(), FakeReviewer::new(|_, _| approved(vec![])), Recorder::default());
        let outcome = fx.runner(&dev, &rev, ScriptedTests::default(), &rec).run_task(SPEC, "T01").await.unwrap();
        assert!(matches!(outcome, Outcome::Blocked { .. }));
        assert!(rec.events().into_iter().any(|e| matches!(e,
            RunnerEvent::SystemNotice { text, code, .. } if code == "task.worktree_dirty" && text.contains("Continuar"))));
    }

    #[tokio::test]
    async fn resumed_integration_with_dirty_worktree_blocks_without_removing() {
        let fx = Fixture::new("resume-sujo", "true");
        let head = fx.leftover_work("T01");
        git_in(&fx.repo, &["merge", "--squash", &branch_name(SPEC, "T01")]);
        git_in(&fx.repo, &["commit", "-m", "feat(s1): T01 - Título T01"]);
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        fx.put_task(&t);
        fs::write(fx.worktree("T01").join("sujo.txt"), "não commitado\n").unwrap();
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();
        // Spec 102: aviso com `code` e `params` neutros, preso à tarefa; texto pt-BR de reserva.
        let notice = rec.events().into_iter().any(|e| matches!(e,
            RunnerEvent::SystemNotice { text, code, params, task_id }
                if code == "task.worktree_dirty"
                    && text.starts_with("Tarefa T01 bloqueada")
                    && params.get("task").and_then(|v| v.as_str()) == Some("T01")
                    && params.get("path").and_then(|v| v.as_str()).is_some_and(|p| p.ends_with("T01"))
                    && task_id.as_deref() == Some("T01")));
        assert!(notice, "aviso da worktree suja com code");

        assert_eq!(
            outcome,
            Outcome::Blocked {
                reason: "worktree com alterações não commitadas; limpe e reexecute".into(),
                kind: BlockKind::Environmental,
                open_findings: vec![],
            }
        );
        assert!(fx.worktree("T01").join("sujo.txt").exists(), "nada removido");
        assert!(fx.branch_exists("sdd/s1/T01"));
        assert_eq!(fx.feat_commits(), 1, "sem duplo commit");
        assert_eq!(
            fx.read_task("T01").frontmatter.status,
            TaskStatus::Integrating,
            "BLOCKED não gravado com commit presente"
        );

        fs::remove_file(fx.worktree("T01").join("sujo.txt")).unwrap();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done);
        assert_eq!(fx.feat_commits(), 1, "sem novo squash");
        assert_eq!(fx.read_task("T01").frontmatter.status, TaskStatus::Done);
        assert!(!fx.worktree("T01").exists());
        assert!(!fx.branch_exists("sdd/s1/T01"));
        assert!(dev.briefs().is_empty());
    }

    #[tokio::test]
    async fn integration_requires_last_verdict_approved() {
        let fx = Fixture::new("sem-veredito", "true");
        let head = fx.leftover_work("T01");
        let mut t = task("T01", TaskStatus::Approved, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        assert_eq!(t.frontmatter.last_verdict, Verdict::None);
        fx.put_task(&t);
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let rec = Recorder::default();
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &rec)
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done);
        assert_eq!(rev.inputs().len(), 1, "voltou a IN_REVIEW e foi revisada");
        assert_eq!(
            rec.statuses("T01"),
            vec![
                TaskStatus::InReview,
                TaskStatus::Approved,
                TaskStatus::Integrating,
                TaskStatus::Done
            ]
        );
        assert_eq!(fx.feat_commits(), 1);
    }

    #[tokio::test]
    async fn resume_finds_integration_commit_beyond_500_later_commits() {
        let fx = Fixture::new("janela", "true");
        let head = fx.leftover_work("T01");
        git_in(&fx.repo, &["merge", "--squash", &branch_name(SPEC, "T01")]);
        git_in(&fx.repo, &["commit", "-m", "feat(s1): T01 - Título T01"]);
        for i in 0..510 {
            git_in(
                &fx.repo,
                &["commit", "--allow-empty", "-q", "-m", &format!("chore: ruído {i}")],
            );
        }
        let mut t = task("T01", TaskStatus::Integrating, &[]);
        t.frontmatter.branch = branch_name(SPEC, "T01");
        t.frontmatter.reviewed_sha = Some(head);
        fx.put_task(&t);
        let dev = FakeDev::default();
        let rev = FakeReviewer::new(|_, _| approved(vec![]));
        let outcome = fx
            .runner(&dev, &rev, ScriptedTests::default(), &Recorder::default())
            .run_task(SPEC, "T01")
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Done);
        assert_eq!(fx.feat_commits(), 1, "nenhum novo squash");
        assert!(dev.briefs().is_empty());
        assert!(rev.inputs().is_empty());
    }
}

#[cfg(test)]
mod spec_context_tests {
    use super::*;

    #[test]
    fn spec_context_extracts_sections_within_limit() {
        let spec = "---\nid: x\n---\n## Contexto\nc\n## Premissas\n- p1\n## Abordagem Técnica\n- a1\n## Outra\nz\n";
        let t = spec_context_of(spec, 4096);
        assert!(t.contains("p1") && t.contains("a1") && !t.contains('z'), "{t}");
        assert_eq!(spec_context_of("## Contexto\nc\n", 4096), "");
        let big = format!("## Premissas\n{}\n", "ç".repeat(5000));
        let t = spec_context_of(&big, 4096);
        assert!(t.len() <= 4096 + 20 && t.contains("truncado"));
    }

    #[test]
    fn role_guidance_is_per_role() {
        use crate::sdd::role_guidance::{role_guidance_of, ROLE_DEVELOPER, ROLE_REVIEWER};
        let spec = "## Orientações por papel\n### Desenvolvedor\nusa lein\n### Revisor\nolhe deps\n";
        assert_eq!(role_guidance_of(spec, ROLE_DEVELOPER), "usa lein");
        assert_eq!(role_guidance_of(spec, ROLE_REVIEWER), "olhe deps");
        assert_eq!(role_guidance_of("## Premissas\n- x\n", ROLE_REVIEWER), "");
    }

    #[test]
    fn project_files_prioritizes_scope_and_reports_omitted() {
        let files: Vec<String> = ["Cargo.toml", "docs/a.md", "src/agents/x.rs", ".sdd/s.md", "src/main.rs"]
            .iter().map(|s| s.to_string()).collect();
        let t = project_files_listing(&files, &["src/agents/**".into()], 200, 4096);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "src/agents/x.rs");
        assert!(t.contains("Cargo.toml") && !t.contains(".sdd"));
        let t = project_files_listing(&files, &["src/agents/**".into()], 2, 4096);
        assert!(t.contains("src/agents/x.rs") && t.contains("e mais 2 arquivos omitidos"), "{t}");
        let many: Vec<String> = (0..500).map(|i| format!("dir/file{i}.rs")).collect();
        let t = project_files_listing(&many, &[], 1000, 4096);
        assert!(t.len() <= 4096 && t.contains("omitidos"), "{}", t.len());
    }

    #[test]
    fn scope_prefix_extraction() {
        assert_eq!(scope_dir_prefix("src/agents/**"), "src/agents/");
        assert_eq!(scope_dir_prefix("src/a.rs"), "src/");
        assert_eq!(scope_dir_prefix("**/*.rs"), "");
    }
}
