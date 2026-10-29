//! Endpoints de projetos: `POST /api/projects` e `GET /api/projects/:id/state`.
//!
//! Projetos são diretórios locais canonicalizados. Todo I/O sobre o projeto passa por
//! `safe_fs` (sem seguir symlinks, sem FIFOs/dispositivos, com limite de tamanho).
//! Nenhum I/O de filesystem ou git ocorre sob o lock da configuração.
//! O estado não contém caminhos absolutos nem branches temporárias; todo texto e id
//! livre é redigido por `SecretSet` antes de sair.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map};

use super::approvals::{replace_atomic, PendingView, PERSIST_LOCK};
use super::config_api::{redact_urls, sanitize_for_view, save_atomic, validate_text_field, INVALID_VIEW};
use super::error::ApiError;
use super::events::{validate_project_id, ApiEvent, EventType, SecretSet};
use super::runs::RunView;
use super::safe_fs::{self, SafeFsError, MAX_READ_BYTES};
use super::state::{AppState, ReserveError, REMOVE_RESERVATION, REPORT_BUSY_CODE, SETTINGS_RESERVATION};
use super::dialog::{self, DIALOG_EXCLUDE_LINE, DIALOG_TMP_EXCLUDE_LINE};
use super::report::{REPORT_EXCLUDE_LINE, REPORT_TMP_EXCLUDE_LINE};
use crate::config::{Config, Project};
use crate::harness::git::Git;
use crate::sdd::{self, Settings, Spec, SpecStatus, Task, TaskStatus, TestCommandError, Verdict};

/// Tamanho máximo do `path` (bytes), como em Linux.
pub const MAX_PATH_BYTES: usize = 4096;
/// Diretórios de sistema que nunca viram projeto nem são navegados.
const SYSTEM_DIRS: [&str; 6] = ["/proc", "/sys", "/dev", "/etc", "/boot", "/root"];
/// Linha adicionada a `.git/info/exclude` para o diretório de worktrees.
const EXCLUDE_LINE: &str = ".sdd/worktrees/";
/// Linha do exclude para a configuração local do projeto (`allowed_commands`, `test_command`).
/// Linhas do exclude para o registro de uso de tokens (`usage.jsonl`, por spec e do chat).
const USAGE_EXCLUDE_LINE: &str = ".sdd/specs/**/usage.jsonl";
/// Cópias das capturas de tela vistas pelo Revisor (spec 082 T03): ficam fora do Git.
const CAPTURES_EXCLUDE_LINE: &str = ".sdd/specs/*/captures/";
/// Íntegras das saídas grandes de ferramenta (spec 122 T02): ficam fora do controle de versão. O
/// `info/exclude` do repositório vale também para as worktrees.
const OUTPUTS_EXCLUDE_LINE: &str = ".nants-outputs/";
/// Artefatos do Playwright (spec 036): ficam fora do Git.
const PLAYWRIGHT_EXCLUDE_LINES: [&str; 2] = ["test-results/", "playwright-report/"];
const BUDGET_EXCLUDE_LINE: &str = ".sdd/specs/**/budget.json";
const USAGE_CHAT_EXCLUDE_LINE: &str = ".sdd/specs/.usage.jsonl";
const EXCLUDE_SETTINGS_LINE: &str = ".sdd/settings.yaml";
/// Caminhos relativos ao projeto usados pela inicialização e pelo estado.
const SETTINGS_REL: &str = ".sdd/settings.yaml";
const SPECS_REL: &str = ".sdd/specs";
const EXCLUDE_REL: &str = ".git/info/exclude";
/// Branch base quando não é possível detectá-la.
const DEFAULT_BASE_BRANCH: &str = "main";
/// Tamanho máximo do trecho com slug do id do projeto.
const SLUG_MAX_CHARS: usize = 40;
/// Limite de leitura de `.git/HEAD`.
const HEAD_MAX_BYTES: u64 = 4096;
/// Prefixo de `HEAD` simbólico apontando para uma branch local.
const HEAD_REF_PREFIX: &str = "ref: refs/heads/";
/// Tamanho máximo do nome de branch lido de `HEAD`.
const BRANCH_MAX_CHARS: usize = 100;
/// Tempo total máximo da inicialização em disco.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Substituto de ids que contêm segredos conhecidos.
const REDACTED_ID: &str = "[redigido]";
/// Mensagem exibida quando um arquivo do `.sdd` não pode ser lido ou interpretado.
pub const INVALID_FILE: &str = "arquivo inválido";

/// Serializa a inicialização em disco (sem segurar o lock da configuração).
static INIT_GUARD: Mutex<()> = Mutex::new(());

/// Corpo de `POST /api/projects`: `{name, path}` (pasta existente, contrato original) ou só
/// `{name}` (pasta nova `<raiz>/<slug>` na pasta de projetos gerenciada, spec 093 T02).
#[derive(Deserialize)]
struct CreateProjectInput {
    name: String,
    #[serde(default)]
    path: Option<String>,
}

/// Projeto como exibido à API. `path` é o caminho canônico validado.
#[derive(Serialize)]
struct ProjectResponse {
    id: String,
    name: String,
    path: String,
    created_at: String,
    is_git_repo: bool,
    initialized: bool,
    git_initialized: bool,
}

/// Estado de um projeto: sem caminhos absolutos, branches ou payloads brutos.
#[derive(Serialize)]
pub struct ProjectStateView {
    id: String,
    name: String,
    is_git_repo: bool,
    initialized: bool,
    base_branch: String,
    /// Comando de testes de `settings.yaml`; vazio quando ausente ou sem configuração.
    test_command: String,
    /// `true` quando `test_command` exibido é redigido ou inválido (não pode ser editado de volta).
    test_command_redacted: bool,
    /// Tentativas do Desenvolvedor por tarefa nova (`settings.yaml`; padrão 5).
    max_attempts: u32,
    /// Ciclos de teste por tentativa antes de bloquear (`settings.yaml`; padrão 5).
    max_test_cycles: u32,
    /// Modo YOLO ligado (`settings.yaml`; padrão false).
    yolo: bool,
    /// Internet do Arquiteto ligada (`settings.yaml`; padrão false; spec 076).
    architect_internet: bool,
    /// A conexão do Arquiteto oferece busca nativa (`wire_api = responses` e sem rejeição do
    /// provedor nesta execução). Falso: o Arquiteto ainda lê páginas e consulta registros.
    web_search_available: bool,
    /// O par (conexão, modelo) do Desenvolvedor gera imagens (`wire_api = responses` e sem recusa
    /// do provedor nas últimas 6 h; spec 090). Falso: ilustrações em SVG/CSS.
    image_generation_available: bool,
    /// `true` enquanto o runner do projeto executa uma spec.
    running: bool,
    /// Spec em execução e status do runner (`null` quando ocioso).
    run: Option<RunView>,
    specs: Vec<SpecView>,
    /// Pedidos de permissão (HITL) pendentes do projeto, em ordem de criação.
    pending_permissions: Vec<PendingView>,
    /// `true` enquanto o relatório final de uma spec concluída é gerado (o projeto não está
    /// executando; as ações de execução respondem 409 com motivo próprio).
    report_pending: bool,
    /// Turno do Arquiteto em andamento (`null` sem turno).
    architect_turn: Option<super::state::ArchitectTurn>,
}

#[derive(Serialize)]
pub struct SpecView {
    id: String,
    title: String,
    status: Option<SpecStatus>,
    tasks: Vec<TaskView>,
    /// Ordem de execução das tarefas (spec 128 T04): topológica pelo `depends_on` e, nos empates,
    /// pelo id. A lista do plano e o quadro mostram as tarefas nessa ordem, com os ids como estão.
    /// Omitida com ciclo (não há ordem; a revisão do plano aponta o ciclo) ou sem tarefas.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    execution_order: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
    /// Ambiente docker proposto pela spec (modo, serviços, imagem base e arquivos).
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<EnvView>,
    /// Critérios globais do spec.md (spec 136 T04), redigidos e com teto, com as citações `kb:`
    /// como estão no arquivo (omitido quando vazio).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    criteria: Vec<String>,
    /// Itens da seção `## Premissas` do spec.md (omitido quando vazio).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    premises: Vec<String>,
    /// Orientações por papel do spec.md (`## Orientações por papel`; omitido quando vazio).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    role_guidance: Vec<RoleGuidanceView>,
    /// Perfis de especialista do projeto (`.sdd/skills`; omitido sem perfis), com a situação em
    /// relação a esta spec.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    skills: Vec<SkillView>,
    /// Avisos de critério que cita caminho fora do `allowed_scope` (plano; nunca bloqueiam).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    scope_warnings: Vec<String>,
    /// Avisos de cobertura dos critérios globais pelo `covers` das tarefas (plano; nunca bloqueiam;
    /// spec 086 T07). Omitido quando vazio ou sem `covers` no plano.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    coverage_warnings: Vec<String>,
    /// Revisão do plano pelo Revisor (spec 086 T08), só na spec em AWAITING_APPROVAL e só da versão
    /// atual do plano (ou `running`). Omitida sem resultado.
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_review: Option<super::plan_review::PlanReviewView>,
    /// O que mudou no plano desde a versão anterior (spec 086 T09), só na spec em AWAITING_APPROVAL
    /// e só quando o resumo é da versão atual. Omitido sem mudanças. Cada linha vem com `code` e
    /// `params` (spec 131 T06), ou em texto quando o arquivo é de antes.
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_changes: Option<Vec<super::plan_changes::PlanChangeLine>>,
    /// Ações gravadas em `tools.jsonl` por `task_id` (inclui `FINAL`, a verificação final); link
    /// "Ações (N)" da UI. Contagem em cache por tamanho/mtime. Omitido quando vazio.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    tool_counts: BTreeMap<String, usize>,
    /// Aviso de custo por tarefa (spec 124 T06): só as que passaram do patamar. Sinal, nunca
    /// limite. Omitido quando vazio.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    cost_alerts: BTreeMap<String, super::cost_alert::CostAlert>,
}

#[derive(Serialize)]
pub struct EnvView {
    mode: &'static str,
    #[serde(flatten)]
    summary: crate::sdd::environment::EnvSummary,
}

#[derive(Serialize)]
pub struct TaskView {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) status: Option<TaskStatus>,
    attempts: u32,
    max_attempts: u32,
    depends_on: Vec<String>,
    last_review_verdict: Option<Verdict>,
    /// Motivo do bloqueio (redigido e limitado); ausente quando vazio.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_action: Option<String>,
    /// Opções de recuperação vigentes (spec 073): só em tarefa BLOCKED cuja última análise as
    /// ofereceu e que nada venceu depois. Omitido quando vazio.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    recovery_options: Vec<super::recovery::RecoveryOption>,
    /// Número da análise que ofereceu `recovery_options`; a UI o envia de volta na escolha.
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_options_analysis: Option<usize>,
    /// Critérios de aceitação da tarefa (spec 136 T04), redigidos e com teto (omitido quando vazio).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    criteria: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<&'static str>,
}

/// Limite de caracteres (Unicode) do motivo do bloqueio exibido.
const NEXT_ACTION_MAX_CHARS: usize = 200;

/// Motivo do bloqueio: controles viram espaço, a redação de segredos/URLs roda ANTES do corte
/// (um segredo nunca vaza cortado ao meio), depois trunca por caracteres com "…". Vazio ou só
/// espaços vira `None`. Nunca emite o marcador de inválido.
pub(super) fn next_action_view(secrets: &SecretSet, raw: Option<&str>) -> Option<String> {
    let flat: String = raw?.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let scrubbed = scrub_text(secrets, flat.trim());
    let text = scrubbed.trim();
    if text.is_empty() {
        return None;
    }
    let mut out: String = text.chars().take(NEXT_ACTION_MAX_CHARS).collect();
    if text.chars().count() > NEXT_ACTION_MAX_CHARS {
        out.push('…');
    }
    Some(out)
}

/// Motivo pelo qual um caminho não pode ser usado como diretório.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirProblem {
    Missing,
    Denied,
    NotDir,
    Invalid,
}

/// `true` quando `path` está em um diretório de sistema da lista fixa.
pub fn is_system_dir(path: &Path) -> bool {
    SYSTEM_DIRS.iter().any(|dir| path.starts_with(dir))
}

/// Valida a forma do caminho informado: absoluto, até 4096 bytes, sem controle e sem `..`.
/// Mensagens são estáticas e não ecoam a entrada.
pub fn validate_path_shape(raw: &str) -> Result<(), ApiError> {
    if raw.is_empty() {
        return Err(ApiError::coded(StatusCode::BAD_REQUEST, "path_required", Map::new()));
    }
    if raw.len() > MAX_PATH_BYTES || raw.chars().any(char::is_control) {
        return Err(ApiError::coded(StatusCode::BAD_REQUEST, "path_too_long", Map::new()));
    }
    if !raw.starts_with('/') {
        return Err(ApiError::coded(StatusCode::BAD_REQUEST, "path_not_absolute", Map::new()));
    }
    if raw.split('/').any(|c| c == "..") {
        return Err(ApiError::coded(StatusCode::BAD_REQUEST, "path_dot_dot", Map::new()));
    }
    Ok(())
}

/// Canonicaliza `raw` e exige que seja um diretório existente.
pub fn resolve_dir(raw: &Path) -> Result<PathBuf, DirProblem> {
    let canonical = fs::canonicalize(raw).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => DirProblem::Missing,
        io::ErrorKind::PermissionDenied => DirProblem::Denied,
        _ => DirProblem::Invalid,
    })?;
    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err(DirProblem::NotDir)
    }
}

/// Redação de segredos conhecidos (substituição exata) e de URLs com userinfo.
pub fn scrub_text(secrets: &SecretSet, text: &str) -> String {
    redact_urls(&secrets.scrub_exact(text))
}

/// Texto livre seguro: redige segredos primeiro, depois valida tamanho e controle.
pub fn text_field(secrets: &SecretSet, field: &str, raw: &str) -> String {
    let scrubbed = scrub_text(secrets, raw);
    sanitize_for_view(&scrubbed, |v| validate_text_field(field, v))
}

/// Id livre seguro: se a redação de segredos o altera, vira `[redigido]`.
pub fn id_view(secrets: &SecretSet, raw: &str) -> String {
    if scrub_text(secrets, raw) != raw {
        return REDACTED_ID.to_string();
    }
    sanitize_for_view(raw, validate_project_id)
}

/// `true` quando `root/.git` existe (diretório, arquivo ou symlink), sem seguir links.
pub fn is_git_repo(root: &Path) -> bool {
    fs::symlink_metadata(root.join(".git")).is_ok()
}

/// Id estável: slug do nome (`[a-z0-9-]`, até 40) + `-` + 8 hex do FNV-1a do caminho canônico.
pub fn project_id(name: &str, canonical: &str) -> String {
    format!("{}-{}", slugify(name), &fnv1a_hex(canonical)[..8])
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(SLUG_MAX_CHARS);
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        "projeto".to_string()
    } else {
        trimmed.to_string()
    }
}

/// FNV-1a 64 bits em hexadecimal (estável entre versões do Rust, diferente de `DefaultHasher`).
fn fnv1a_hex(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Data UTC atual em RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`).
pub(crate) fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(secs)
}

/// Formata segundos desde a época Unix como RFC 3339 UTC (algoritmo de dias civis).
pub(crate) fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Traduz falhas de I/O seguro: symlink/especial/grande → 409 estático; resto → 500 estático.
pub(super) fn safe_error(e: SafeFsError) -> ApiError {
    match e {
        SafeFsError::Unsafe | SafeFsError::NotRegular | SafeFsError::TooLarge | SafeFsError::Invalid => {
            ApiError::coded(StatusCode::CONFLICT, e.code(), Map::new())
        }
        SafeFsError::Missing | SafeFsError::Io => ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "sdd_init_failed", Map::new()),
    }
}

/// Branch base lida de `.git/HEAD`, sem subprocess git.
/// Exige `.git` diretório real (gitfile, worktree, symlink ou ausente → `main`).
/// `HEAD` só é aceito como `ref: refs/heads/NAME` em uma linha; qualquer outra forma
/// (destacado, ilegível, grande demais, nome inválido) vira `main`.
pub fn read_head_branch(root: &Path) -> String {
    let git_is_dir = matches!(
        fs::symlink_metadata(root.join(".git")),
        Ok(m) if m.file_type().is_dir()
    );
    if !git_is_dir {
        return DEFAULT_BASE_BRANCH.to_string();
    }
    safe_fs::read_regular(root, ".git/HEAD", HEAD_MAX_BYTES)
        .ok()
        .and_then(|text| parse_head_branch(&text))
        .unwrap_or_else(|| DEFAULT_BASE_BRANCH.to_string())
}

/// Interpreta o conteúdo de `HEAD`: exatamente `ref: refs/heads/NAME` (uma linha, `\n` final opcional).
fn parse_head_branch(text: &str) -> Option<String> {
    let line = text.strip_suffix('\n').unwrap_or(text);
    let name = line.strip_prefix(HEAD_REF_PREFIX)?;
    valid_branch_name(name).then(|| name.to_string())
}

/// Nome de branch seguro: `[A-Za-z0-9._/-]`, 1 a 100 bytes. Regras de `git check-ref-format`
/// relevantes: sem `..`, `//`, `@{`; não começa com `-`, `.` ou `/`; não termina com `.` ou `/`;
/// cada segmento (separado por `/`) não é vazio, não começa com `.` e não termina com `.lock`.
fn valid_branch_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= BRANCH_MAX_CHARS
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && !name.starts_with(|c| matches!(c, '-' | '.' | '/'))
        && !name.ends_with(|c| matches!(c, '.' | '/'))
        && name
            .split('/')
            .all(|seg| !seg.is_empty() && !seg.starts_with('.') && !seg.ends_with(".lock"))
}

/// Resultado do estado em disco de um projeto (lido sem lock de configuração).
#[derive(Clone, Copy)]
struct DiskState {
    is_git: bool,
    initialized: bool,
    /// `true` quando o Nants rodou `git init` ou criou o commit inicial neste cadastro.
    git_initialized: bool,
}

/// Metadados sem seguir links; caminho ausente vira `None`.
fn optional_meta(path: &Path) -> Result<Option<fs::Metadata>, ApiError> {
    match fs::symlink_metadata(path) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "sdd_init_failed", Map::new())),
    }
}

/// Pré-validação total dos destinos da inicialização. Não escreve nada.
/// Symlink em qualquer componente → 409 (via safe_fs); tipo inadequado → 409 estático.
/// `.git/info/exclude` deve ser arquivo regular de até 1 MiB quando o repo tem `.git` diretório.
fn precheck_init_targets(root: &Path, git_dir: bool) -> Result<(), ApiError> {
    if git_dir {
        precheck_exclude(root)?;
    }
    check_dir_slot(&root.join(".sdd"))?;
    check_dir_slot(&root.join(SPECS_REL))?;
    safe_fs::check_chain(root, SETTINGS_REL).map_err(safe_error)?;
    if optional_meta(&root.join(SETTINGS_REL))?.is_some_and(|m| !m.file_type().is_file()) {
        return Err(ApiError::coded(StatusCode::CONFLICT, "project_config_invalid", Map::new()));
    }
    Ok(())
}

/// `.git/info` e `.git/info/exclude` seguros para escrita: sem symlink e exclude regular de até 1 MiB.
fn precheck_exclude(root: &Path) -> Result<(), ApiError> {
    check_dir_slot(&root.join(".git/info"))?;
    safe_fs::check_chain(root, EXCLUDE_REL).map_err(safe_error)?;
    if let Some(meta) = optional_meta(&root.join(EXCLUDE_REL))? {
        if !meta.file_type().is_file() || meta.len() > MAX_READ_BYTES {
            return Err(ApiError::coded(StatusCode::CONFLICT, "project_structure_invalid", Map::new()));
        }
    }
    Ok(())
}

/// `true` quando `.git` é diretório real. Symlink em `.git` → erro seguro.
fn has_git_dir(root: &Path) -> Result<bool, ApiError> {
    let git_meta = optional_meta(&root.join(".git"))?;
    if git_meta.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
        return Err(safe_error(SafeFsError::Unsafe));
    }
    Ok(git_meta.is_some_and(|m| m.file_type().is_dir()))
}

/// Validação pura de `ensure_local_excludes`: não escreve nada. Usada na fase de validação.
pub(super) fn check_local_excludes(root: &Path) -> Result<(), ApiError> {
    if has_git_dir(root)? {
        precheck_exclude(root)?;
    }
    Ok(())
}

/// Registra no `.git/info/exclude` as entradas locais do Nants, sem duplicar: `.sdd/worktrees/`
/// e `.sdd/settings.yaml`. Idempotente; também migra projetos criados antes dessa linha.
/// Sem `.git` diretório (não há repositório ou é gitfile) não faz nada. Symlink ou tipo inseguro → erro.
/// Não tem efeito sobre arquivos já rastreados: quem os versionou continua vendo alterações neles.
pub(super) fn ensure_local_excludes(root: &Path) -> Result<(), ApiError> {
    if !has_git_dir(root)? {
        return Ok(());
    }
    precheck_exclude(root)?;
    for line in [
        EXCLUDE_LINE,
        EXCLUDE_SETTINGS_LINE,
        DIALOG_EXCLUDE_LINE,
        DIALOG_TMP_EXCLUDE_LINE,
        REPORT_EXCLUDE_LINE,
        REPORT_TMP_EXCLUDE_LINE,
        super::plan_review::PLAN_REVIEW_EXCLUDE_LINE,
        super::plan_review::PLAN_REVIEW_TMP_EXCLUDE_LINE,
        super::plan_changes::PLAN_CHANGES_EXCLUDE_LINE,
        super::plan_changes::PLAN_CHANGES_TMP_EXCLUDE_LINE,
        super::runs::RENUMBER_JOURNAL_EXCLUDE_LINE,
        super::runs::RENUMBER_JOURNAL_TMP_EXCLUDE_LINE,
        USAGE_EXCLUDE_LINE,
        USAGE_CHAT_EXCLUDE_LINE,
        super::usage::CUTS_EXCLUDE_LINE,
        super::usage::CUTS_CHAT_EXCLUDE_LINE,
        super::usage::CONTEXT_EXCLUDE_LINE,
        super::usage::CONTEXT_CHAT_EXCLUDE_LINE,
        super::provider_errors::PROVIDER_ERRORS_EXCLUDE_LINE,
        super::provider_errors::PROVIDER_ERRORS_CHAT_EXCLUDE_LINE,
        CAPTURES_EXCLUDE_LINE,
        OUTPUTS_EXCLUDE_LINE,
        crate::harness::home::EXCLUDE_LINE,
        BUDGET_EXCLUDE_LINE,
        super::chat_attachments::ATTACHMENTS_EXCLUDE_LINE,
        crate::kb::KB_EXCLUDE_LINE,
        crate::sdd::capabilities::CAPABILITIES_EXCLUDE_LINE,
        crate::sdd::capabilities::CAPABILITIES_TMP_EXCLUDE_LINE,
    ] {
        safe_fs::append_line_once(root, EXCLUDE_REL, line).map_err(safe_error)?;
    }
    for line in PLAYWRIGHT_EXCLUDE_LINES {
        safe_fs::append_line_once(root, EXCLUDE_REL, line).map_err(safe_error)?;
    }
    Ok(())
}

/// Entrada que deve ser diretório real (`.sdd`, `.sdd/specs`, `.git/info`), checada antes
/// de qualquer `check_chain` nos filhos. Ausente → ok; symlink → 409 seguro;
/// qualquer outro tipo (arquivo, FIFO, dispositivo) → 409 estático.
fn check_dir_slot(path: &Path) -> Result<(), ApiError> {
    match optional_meta(path)? {
        None => Ok(()),
        Some(m) if m.file_type().is_symlink() => Err(safe_error(SafeFsError::Unsafe)),
        Some(m) if m.file_type().is_dir() => Ok(()),
        Some(_) => Err(ApiError::coded(StatusCode::CONFLICT, "project_structure_invalid", Map::new())),
    }
}

/// Garante `.sdd/` sem sobrescrever nada e registra `.sdd/worktrees/` no exclude do repo.
/// Pré-valida todos os destinos antes de qualquer escrita: violação → 409 e nada é criado.
/// Só a escrita fica sob `INIT_GUARD`; a branch base é lida antes, fora do guard.
/// Nunca executa `git init`.
fn init_sdd(root: &Path, yolo: bool, lang: crate::i18n::Lang) -> Result<DiskState, ApiError> {
    let _guard = INIT_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let git_meta = optional_meta(&root.join(".git"))?;
    if git_meta.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
        return Err(safe_error(SafeFsError::Unsafe));
    }
    let git_dir = git_meta.as_ref().is_some_and(|m| m.file_type().is_dir());

    precheck_init_targets(root, git_dir)?;
    let git_initialized = ensure_git_repo(root, git_meta.is_some(), lang)?;
    let is_git = true;
    let base = read_head_branch(root);
    safe_fs::ensure_dirs(root, SPECS_REL).map_err(safe_error)?;

    let settings = Settings {
        base_branch: base,
        merge_strategy: "squash".into(),
        test_command: String::new(),
        full_test_command: None,
        install_command: None,
        lockfiles: Vec::new(),
        allowed_commands: Vec::new(),
        denied_commands: Vec::new(),
        checks: Vec::new(),
        formatters: Vec::new(),
        failure_artifacts: Vec::new(),
        screenshots: Vec::new(),
        environment: Default::default(),
        environment_hash: None,
        approved_environment_hashes: Vec::new(),
        max_attempts: crate::sdd::DEFAULT_LIMIT,
        max_test_cycles: crate::sdd::DEFAULT_LIMIT,
        yolo,
        review_diff_budget: None,
        skills: true,
        ask_architect_limit: None,
        architect_internet: false,
    };
    let yaml = settings
        .render()
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "sdd_init_failed", Map::new()))?;
    safe_fs::write_new(root, SETTINGS_REL, &yaml).map_err(safe_error)?;
    ensure_local_excludes(root)?;
    Ok(DiskState {
        is_git,
        initialized: sdd_initialized(root),
        git_initialized,
    })
}

/// Prefixo do commit inicial do repositório criado pelo Nants; o texto depois dele sai no idioma
/// do projeto (`commit.init_repo`, spec 115 T04).
const INITIAL_COMMIT_PREFIX: &str = "chore: ";

/// Garante repositório com ao menos um commit. Devolve `true` se o Nants fez init e/ou commit.
/// Pasta sem `.git` dentro de outro repositório → 409 sem criar nada.
fn ensure_git_repo(root: &Path, has_git: bool, lang: crate::i18n::Lang) -> Result<bool, ApiError> {
    let git = Git::new(root).with_env("GIT_TERMINAL_PROMPT", "0");
    if !has_git {
        let ancestor = root
            .ancestors()
            .skip(1)
            .find(|a| a.join(".git").exists())
            .map(|a| a.display().to_string());
        if let Some(top) = git.toplevel().or(ancestor) {
            return Err(ApiError::coded(StatusCode::CONFLICT, "project_nested_repo", super::error::params([("path", json!(top))])));
        }
        git.init_main().map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "git_init_failed", Map::new()))?;
    } else if !git.is_unborn() {
        return Ok(false);
    }
    git.commit_empty(&crate::i18n::commit_subject(lang, INITIAL_COMMIT_PREFIX, "commit.init_repo"))
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "git_init_failed", Map::new()))?;
    Ok(true)
}

/// Executa `init_sdd` fora do runtime assíncrono, com tempo total limitado.
async fn init_with_timeout(root: PathBuf, yolo: bool, lang: crate::i18n::Lang) -> Result<DiskState, ApiError> {
    let job = tokio::task::spawn_blocking(move || init_sdd(&root, yolo, lang));
    match tokio::time::timeout(INIT_TIMEOUT, job).await {
        Err(_) => Err(ApiError::coded(StatusCode::GATEWAY_TIMEOUT, "project_init_timeout", Map::new())),
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "sdd_init_failed", Map::new())),
    }
}

/// Converte o corpo JSON. Malformado → 400; campos inválidos → 422 sem eco de valores.
fn parse_input(body: &[u8]) -> Result<CreateProjectInput, ApiError> {
    serde_json::from_slice(body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields", Map::new())
        } else {
            ApiError::coded(StatusCode::BAD_REQUEST, "json_malformed", Map::new())
        }
    })
}

fn map_create_problem(problem: DirProblem) -> ApiError {
    match problem {
        DirProblem::Missing => ApiError::coded(StatusCode::BAD_REQUEST, "path_missing", Map::new()),
        DirProblem::Denied => ApiError::coded(StatusCode::FORBIDDEN, "path_no_permission", Map::new()),
        DirProblem::NotDir => ApiError::coded(StatusCode::BAD_REQUEST, "path_not_dir", Map::new()),
        DirProblem::Invalid => ApiError::coded(StatusCode::BAD_REQUEST, "field_path_invalid", Map::new()),
    }
}

async fn blocking<T, F>(job: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))?
}

fn project_response(secrets: &SecretSet, project: &Project, disk: DiskState) -> ProjectResponse {
    ProjectResponse {
        id: id_view(secrets, &project.id),
        name: text_field(secrets, "name", &project.name),
        path: project.path.clone(),
        created_at: text_field(secrets, "created_at", &project.created_at),
        is_git_repo: disk.is_git,
        initialized: disk.initialized,
        git_initialized: disk.git_initialized,
    }
}

/// Resultado da busca de um projeto pelo caminho canônico ou pelo id.
enum Lookup {
    Same(Project),
    Conflict,
    Free,
}

fn lookup(cfg: &Config, path: &str, id: &str) -> Lookup {
    if let Some(project) = cfg.projects.iter().find(|p| p.path == path) {
        Lookup::Same(project.clone())
    } else if cfg.projects.iter().any(|p| p.id == id) {
        Lookup::Conflict
    } else {
        Lookup::Free
    }
}

/// `POST /api/projects`: cadastra (201) ou devolve o existente para o mesmo caminho (200).
///
/// Fases: (1) validar e canonicalizar; (2) calcular id; (3) checar a config sob leitura;
/// (4) inicializar disco fora de qualquer lock; (5) sob escrita, rechecar e persistir.
///
/// Sem `path` (spec 093 T02), cria `<raiz>/<slug>` na pasta de projetos e segue o mesmo fluxo; a
/// pasta já existente é 409 `{error: "project_dir_exists", path}`. Sem a prontidão
/// `create_project`, 412 `prereq_missing` (antes de qualquer efeito).
pub async fn create_project(State(state): State<AppState>, body: Bytes) -> Result<Response, ApiError> {
    let input = parse_input(&body)?;
    if let Err(resp) = super::readiness::require(&state, super::readiness::Gate::CreateProject).await {
        return Ok(resp);
    }
    let name = input.name.trim().to_string();
    let Some(raw_path) = input.path else {
        return create_by_name(&state, &name).await;
    };
    if name.is_empty() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "name_required", Map::new()));
    }
    validate_text_field("name", &name)?;
    validate_path_shape(&raw_path)?;

    let raw = PathBuf::from(&raw_path);
    let canonical = blocking(move || resolve_dir(&raw).map_err(map_create_problem)).await?;
    register_project(&state, &name, canonical).await
}

/// `{name}` sem `path`: valida o nome, gera o slug, cria a pasta na raiz gerenciada e registra.
async fn create_by_name(state: &AppState, name: &str) -> Result<Response, ApiError> {
    use super::projects_root::{create_project_dir, folder_slug, name_invalid, validate_root};
    if name.is_empty() {
        return Err(name_invalid());
    }
    validate_text_field("name", name)?;
    let slug = folder_slug(name).ok_or_else(name_invalid)?;
    let cfg = state.config().read().await.clone();
    let root = validate_root(cfg.projects_root_raw(), &cfg, std::env::var_os("HOME"))?;
    let created = tokio::task::spawn_blocking(move || create_project_dir(&root, &slug, &cfg))
        .await
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))?;
    let canonical = match created {
        Ok(dir) => dir,
        Err(e) => return Ok(e.into_response()),
    };
    register_project(state, name, canonical).await
}

/// Fases comuns depois de a pasta existir (caminho canônico): checagens de raiz e sistema, id,
/// config sob leitura, inicialização em disco (Git e `.sdd`) e registro sob escrita.
async fn register_project(state: &AppState, name: &str, canonical: PathBuf) -> Result<Response, ApiError> {
    let state = state.clone();
    let name = name.to_string();
    if canonical.parent().is_none() {
        return Err(ApiError::coded(StatusCode::FORBIDDEN, "root_dir_forbidden", Map::new()));
    }
    if is_system_dir(&canonical) {
        return Err(ApiError::coded(StatusCode::FORBIDDEN, "system_dir_forbidden", Map::new()));
    }
    let canonical_str = canonical
        .to_str()
        .ok_or_else(|| ApiError::coded(StatusCode::BAD_REQUEST, "field_path_invalid", Map::new()))?
        .to_string();

    let secrets = state.secret_set().await;
    let display_name = scrub_text(&secrets, &name);
    let id = project_id(&display_name, &canonical_str);

    let found = {
        let cfg = state.config().read().await;
        lookup(&cfg, &canonical_str, &id)
    };
    match found {
        Lookup::Same(existing) => {
            let root = canonical.clone();
            let disk = blocking(move || {
                Ok(DiskState {
                    is_git: is_git_repo(&root),
                    initialized: sdd_initialized(&root),
                    git_initialized: false,
                })
            })
            .await?;
            let view = project_response(&secrets, &existing, disk);
            return Ok((StatusCode::OK, Json(view)).into_response());
        }
        Lookup::Conflict => return Err(id_conflict()),
        Lookup::Free => {}
    }

    let inherited_yolo = state.config().read().await.last_yolo;
    let disk = init_with_timeout(canonical.clone(), inherited_yolo, state.project_lang(&id).get()).await?;
    let project = Project {
        id,
        name: display_name,
        path: canonical_str,
        created_at: now_rfc3339(),
    };

    let mut guard = state.config().write().await;
    match lookup(&guard, &project.path, &project.id) {
        Lookup::Same(existing) => {
            let view = project_response(&secrets, &existing, disk);
            Ok((StatusCode::OK, Json(view)).into_response())
        }
        Lookup::Conflict => Err(id_conflict()),
        Lookup::Free => {
            let view = project_response(&secrets, &project, disk);
            let mut next = guard.clone();
            let readmit = PathBuf::from(&project.path);
            next.projects.push(project);
            persist(&state, &next).await?;
            let created = next.projects.last().cloned();
            *guard = next;
            drop(guard);
            // Projeto (re)admitido: o diálogo volta a gravar neste caminho.
            dialog::revive_project(&readmit);
            // Projeto existente sem ARCHITECTURE.md: levantamento inicial em segundo plano.
            if let Some(project) = created {
                super::survey::spawn_initial_survey(&state, project);
            }
            Ok((StatusCode::CREATED, Json(view)).into_response())
        }
    }
}

fn id_conflict() -> ApiError {
    ApiError::coded(StatusCode::CONFLICT, "project_id_taken", Map::new())
}

async fn persist(state: &AppState, cfg: &Config) -> Result<(), ApiError> {
    let path = state.config_path().to_path_buf();
    let snapshot = cfg.clone();
    tokio::task::spawn_blocking(move || save_atomic(&path, &snapshot))
        .await
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "config_save_failed", Map::new()))?
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "config_save_failed", Map::new()))
}

/// `GET /api/projects/:id/state`
pub async fn get_state(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let project = find_project(&state, &id).await?;
    let view = project_view(&state, project).await?;
    Ok(Json(view).into_response())
}

/// Projeto cadastrado pelo id, ou 404.
pub(super) async fn find_project(state: &AppState, id: &str) -> Result<Project, ApiError> {
    let cfg = state.config().read().await;
    let found = cfg.projects.iter().find(|p| p.id == id).cloned();
    found.ok_or_else(|| ApiError::coded(StatusCode::NOT_FOUND, "project_not_found", Map::new()))
}

/// Estado do projeto como exibido à API (disco + execução + pedidos pendentes).
async fn project_view(state: &AppState, project: Project) -> Result<ProjectStateView, ApiError> {
    let id = project.id.clone();
    let secrets: Arc<SecretSet> = state.secret_set().await;
    let root = PathBuf::from(&project.path);
    let review_now = super::plan_review::reviewer_configured(state).await.then(SystemTime::now);
    let mut view = blocking(move || Ok(build_state_at(&root, &project, &secrets, review_now))).await?;
    view.web_search_available = state.web_search_available().await;
    view.image_generation_available = state.developer_image_generation_available().await;
    view.run = state.runs().active(&id);
    view.running = view.run.is_some();
    view.pending_permissions = state.approvals().list_pending(&id);
    view.report_pending = state.runs().is_report(&id);
    view.architect_turn = state.architect_turn(&id);
    Ok(view)
}

#[cfg(test)]
#[path = "remove_project_tests.rs"]
mod remove_project_tests;

#[cfg(test)]
#[path = "reserve_race_tests.rs"]
mod reserve_race_tests;


/// `DELETE /api/projects/:id`: remove o projeto **só do Nants**. O corpo é ignorado.
///
/// O que sai: a entrada em `config.projects` (gravação atômica; em falha nada muda na memória) e
/// o estado de runtime do projeto (pedidos pendentes negados, escopos `run`). O que fica: a pasta
/// do projeto inteira (código, `.git`, `.sdd`, worktrees, branches `sdd/*`, histórico do chat).
/// Nenhum arquivo do projeto é lido, alterado ou apagado por esta rota.
///
/// Respostas: 200 `{"ok":true,"removed":id}`; 422 id inválido; 404 projeto inexistente (também em
/// repetição); 409 com execução ou turno de chat em andamento, sem efeito algum; 500 se a
/// configuração não puder ser gravada, com o projeto mantido. Atomicidade: as reservas de chat e
/// de execução são tomadas sob o lock de escrita da configuração e seguradas até o fim.
pub async fn remove_project(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    // `{"remove_volumes": true}` (opcional): apaga também os volumes do `-run`. Qualquer outro corpo = não.
    let remove_volumes = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("remove_volumes").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    let mut guard = state.config().write().await;
    let index = guard
        .projects
        .iter()
        .position(|p| p.id == id)
        .ok_or_else(|| ApiError::coded(StatusCode::NOT_FOUND, "project_not_found", Map::new()))?;
    let _run = state
        .runs()
        .try_start(&id, REMOVE_RESERVATION)
        .map_err(|_| {
            let code = if state.runs().is_report(&id) { REPORT_BUSY_CODE } else { "run_active" };
            ApiError::coded(StatusCode::CONFLICT, code, Map::new())
        })?;
    let _chat = state
        .try_begin_chat(&id)
        .ok_or_else(|| ApiError::coded(StatusCode::CONFLICT, "chat_active", Map::new()))?;

    let project_root = PathBuf::from(&guard.projects[index].path);
    let mut next = guard.clone();
    next.projects.remove(index);
    persist(&state, &next).await?;
    *guard = next;
    // Estado da interface do projeto (spec 093): apagado ainda sob o write lock, o mesmo que o PUT
    // de `ui-state` usa (como leitor) para conferir o projeto; um PUT tardio vê 404 e não recria.
    super::ui_state::remove_project_doc(state.data_dir(), &id);
    drop(guard);

    // O app de teste não sobrevive ao projeto (a reserva `__remove__` segue valendo).
    state.apps().stop(&id, super::app::REASON_PROJECT_REMOVED).await;
    // Console (spec 130 T03): o último resultado e o histórico do projeto removido somem.
    state.apps().console().forget(state.data_dir().map(Path::to_path_buf), &id).await;
    // Base de conhecimento (spec 134/T03): o índice em memória do projeto removido sai do cache.
    crate::kb::index::forget(&project_root);
    // Ambiente Docker: derruba -test e -run (volumes do -run só com a confirmação do usuário).
    {
        let (provider, root, pid) = (state.compose().clone(), project_root.clone(), id.clone());
        let torn = tokio::task::spawn_blocking(move || super::envs::teardown(&provider, &root, &pid, true, remove_volumes));
        match tokio::time::timeout(super::envs::CLEANUP_TIMEOUT, torn).await {
            Ok(Ok(errors)) => errors.iter().for_each(|e| eprintln!("aviso: cleanup Docker de {id}: {e}")),
            _ => eprintln!("aviso: cleanup Docker de {id} não concluiu"),
        }
    }
    // Libera fila de gravação e seqs do diálogo (a thread encerra ao drenar); a pasta fica no disco.
    drop(dialog::forget_project(&project_root));
    state.approvals().clear_project(&id, state.events());
    state.events().live_activity().clear_project(&id);
    state.events().publish(ApiEvent::new(
        EventType::ProjectRemoved,
        Some(id.clone()),
        json!({ "project_id": id }),
    ));
    Ok(Json(json!({ "ok": true, "removed": id })).into_response())
}

/// Corpo de `PUT /api/projects/:id/settings`.
#[derive(Deserialize)]
struct SettingsInput {
    test_command: Option<String>,
    max_attempts: Option<u32>,
    max_test_cycles: Option<u32>,
}

/// `PUT /api/projects/:id/settings` com `{"test_command": "..."}`.
///
/// Grava só o comando de testes (sob o mesmo lock de `always`, sem perder as demais chaves).
/// Recusa com 409 se o projeto tem execução ativa. Responde 200 com o estado do projeto.
/// Erros são estáticos: nunca ecoam o comando informado.
pub async fn put_settings(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    // Reserva interna sob o read lock da configuração: exclui execução ativa e a remoção durante a
    // escrita (a remoção pede o write lock e recusa com 409 enquanto a reserva existir).
    let (project, _settings) = state
        .reserve_run(&id, SETTINGS_RESERVATION)
        .await
        .map_err(|e| match e {
            ReserveError::NotFound => ApiError::coded(StatusCode::NOT_FOUND, "project_not_found", Map::new()),
            ReserveError::Busy => ApiError::coded(StatusCode::CONFLICT, "run_active", Map::new()),
            ReserveError::ReportInProgress => ApiError::coded(StatusCode::CONFLICT, REPORT_BUSY_CODE, Map::new()),
        })?;
    let input = parse_settings_input(&body)?;
    if input.test_command.is_none() && input.max_attempts.is_none() && input.max_test_cycles.is_none() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "test_command_required", Map::new()));
    }
    for limit in [input.max_attempts, input.max_test_cycles].into_iter().flatten() {
        if !sdd::LIMIT_RANGE.contains(&limit) {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "limits_range", Map::new()));
        }
    }
    let command = match &input.test_command {
        Some(raw) => {
            let command = check_test_command_shape(raw)?;
            if command == INVALID_VIEW {
                return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "test_command_redacted", Map::new()));
            }
            Some(command)
        }
        None => None,
    };
    let secrets = state.secret_set().await;
    let root = PathBuf::from(&project.path);
    let (attempts, cycles) = (input.max_attempts, input.max_test_cycles);
    blocking(move || {
        write_settings_inner(root.as_path(), command.as_deref(), &secrets, false, (attempts, cycles)).map(|_| ())
    })
    .await?;
    let view = project_view(&state, project).await?;
    Ok(Json(view).into_response())
}

/// Corpo de `PUT /api/projects/:id/yolo`.
#[derive(Deserialize)]
struct YoloInput {
    enabled: bool,
}

/// `PUT /api/projects/:id/yolo` com `{"enabled": true|false}`: liga/desliga o modo YOLO.
/// Só a API altera (agentes não escrevem `settings.yaml`). Vale também durante um run (não exige
/// reserva): a próxima decisão do servidor já lê o novo valor. Responde 200 com o estado.
pub async fn put_yolo(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let project = find_project(&state, &id).await?;
    let input: YoloInput = serde_json::from_slice(&body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields", Map::new())
        } else {
            ApiError::coded(StatusCode::BAD_REQUEST, "json_malformed", Map::new())
        }
    })?;
    let root = PathBuf::from(&project.path);
    let enabled = input.enabled;
    blocking(move || write_yolo(root.as_path(), enabled)).await?;
    // Última escolha vira a preferência de projetos novos (spec 064); falha aqui não desfaz o modo.
    let mut guard = state.config().write().await;
    if guard.last_yolo != enabled {
        let mut next = guard.clone();
        next.last_yolo = enabled;
        if persist(&state, &next).await.is_ok() {
            *guard = next;
        }
    }
    drop(guard);
    let view = project_view(&state, project).await?;
    Ok(Json(view).into_response())
}

/// Corpo de `PUT /api/projects/:id/internet`.
#[derive(Deserialize)]
struct InternetInput {
    enabled: bool,
}

/// `PUT /api/projects/:id/internet` com `{"enabled": true|false}` (spec 076): liga/desliga a
/// internet do Arquiteto (plano e recuperação). Mesmo molde do YOLO: só a API altera, vale
/// durante um run (o próximo turno do Arquiteto lê o valor) e responde 200 com o estado.
pub async fn put_internet(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let project = find_project(&state, &id).await?;
    let input: InternetInput = serde_json::from_slice(&body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields", Map::new())
        } else {
            ApiError::coded(StatusCode::BAD_REQUEST, "json_malformed", Map::new())
        }
    })?;
    let root = PathBuf::from(&project.path);
    let enabled = input.enabled;
    blocking(move || write_internet(root.as_path(), enabled)).await?;
    let view = project_view(&state, project).await?;
    Ok(Json(view).into_response())
}

/// Grava `architect_internet` em `settings.yaml` (leitura-modificação-escrita atômica).
pub(super) fn write_internet(root: &Path, enabled: bool) -> Result<(), ApiError> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let text = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES).map_err(settings_error)?;
    let mut settings = Settings::parse(&text).map_err(|_| ApiError::coded(StatusCode::CONFLICT, "project_config_invalid", Map::new()))?;
    settings.architect_internet = enabled;
    let rendered = settings.render().map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "test_command_save_failed", Map::new()))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "test_command_save_failed", Map::new()))
}

/// Grava `yolo` em `settings.yaml` (leitura-modificação-escrita atômica sob o `PERSIST_LOCK`).
pub(super) fn write_yolo(root: &Path, enabled: bool) -> Result<(), ApiError> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let text = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES).map_err(settings_error)?;
    let mut settings = Settings::parse(&text).map_err(|_| ApiError::coded(StatusCode::CONFLICT, "project_config_invalid", Map::new()))?;
    settings.yolo = enabled;
    let rendered = settings.render().map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "test_command_save_failed", Map::new()))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes()).map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "test_command_save_failed", Map::new()))
}

/// Decodifica o corpo: malformado → 400; campo ausente ou tipo errado → 422 sem eco.
fn parse_settings_input(body: &[u8]) -> Result<SettingsInput, ApiError> {
    serde_json::from_slice(body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields", Map::new())
        } else {
            ApiError::coded(StatusCode::BAD_REQUEST, "json_malformed", Map::new())
        }
    })
}

/// Forma do `test_command` do PUT (regra compartilhada em `sdd::check_test_command_shape`).
/// É o mesmo limite usado na leitura, então todo valor exibido é aceito de volta na gravação.
fn check_test_command_shape(raw: &str) -> Result<String, ApiError> {
    sdd::check_test_command_shape(raw).map_err(|e| match e {
        TestCommandError::Empty => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "test_command_required", Map::new()),
        TestCommandError::Invalid => ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "test_command_invalid", Map::new()),
    })
}


/// `test_command` na leitura: redigido por `SecretSet`; valor que a gravação não aceitaria
/// (forma inválida) vira `[inválido]`. Vazio continua vazio (comando ainda não configurado).
fn test_command_view(secrets: &SecretSet, raw: &str) -> String {
    if raw.trim().is_empty() {
        return String::new();
    }
    let scrubbed = scrub_text(secrets, raw);
    sanitize_for_view(&scrubbed, |v| check_test_command_shape(v).map(|_| ()))
}

/// `true` quando a forma exibida difere do valor armazenado (redigido ou inválido).
fn test_command_redacted(secrets: &SecretSet, raw: &str) -> bool {
    !raw.trim().is_empty() && test_command_view(secrets, raw) != raw
}

/// Grava `test_command` em `.sdd/settings.yaml` por leitura-modificação-escrita atômica sob o
/// `PERSIST_LOCK` compartilhado com `always`. Ausente, inválido ou não regular → 409 estático.
/// Recusa só o eco da própria leitura: se o valor armazenado foi redigido, o texto igual à forma
/// exibida é rejeitado (gravá-lo trocaria o comando real). Qualquer outro texto é aceito.
#[cfg(test)]
pub(super) fn write_test_command(root: &Path, command: &str, secrets: &SecretSet) -> Result<(), ApiError> {
    write_settings_inner(root, Some(command), secrets, false, (None, None)).map(|_| ())
}

/// Como [`write_test_command`], mas só grava se o `test_command` ainda estiver vazio, sob o mesmo
/// lock (não sobrescreve um valor definido por outro caminho). Devolve `true` se gravou.
pub(super) fn write_test_command_if_empty(
    root: &Path,
    command: &str,
    secrets: &SecretSet,
) -> Result<bool, ApiError> {
    write_settings_inner(root, Some(command), secrets, true, (None, None))
}

/// Única leitura-mutação-escrita de `settings.yaml`: `test_command` e limites juntos, atômico.
fn write_settings_inner(
    root: &Path,
    command: Option<&str>,
    secrets: &SecretSet,
    only_if_empty: bool,
    limits: (Option<u32>, Option<u32>),
) -> Result<bool, ApiError> {
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let text = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES).map_err(settings_error)?;
    let mut settings = Settings::parse(&text)
        .map_err(|_| ApiError::coded(StatusCode::CONFLICT, "project_config_invalid", Map::new()))?;
    if let Some(command) = command {
        if only_if_empty && !settings.test_command.trim().is_empty() {
            return Ok(false);
        }
        let shown = test_command_view(secrets, &settings.test_command);
        if shown != settings.test_command && command == shown {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "test_command_redacted", Map::new()));
        }
        settings.test_command = if settings.environment == sdd::Environment::Docker {
            sdd::docker_command::normalize(command)
                .map_err(|_| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "test_command_docker", Map::new()))?
        } else {
            command.to_string()
        };
    }
    if let Some(v) = limits.0 {
        settings.max_attempts = v;
    }
    if let Some(v) = limits.1 {
        settings.max_test_cycles = v;
    }
    let rendered = settings
        .render()
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "test_command_save_failed", Map::new()))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes())
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "test_command_save_failed", Map::new()))?;
    Ok(true)
}

/// Grava as checagens propostas pelo Arquiteto no `settings.yaml`, só se ainda não houver nenhuma
/// (mesmo lock do `test_command`). Comandos são redigidos; lista inválida não grava. `true` se gravou.
pub(super) fn write_checks_if_empty(
    root: &Path,
    checks: &[sdd::Check],
    secrets: &SecretSet,
) -> Result<bool, ApiError> {
    let scrubbed: Vec<sdd::Check> = checks
        .iter()
        .map(|c| sdd::Check { command: scrub_text(secrets, &c.command), ..c.clone() })
        .collect();
    let Some(valid) = sdd::validate_checks(&scrubbed) else {
        return Ok(false);
    };
    if valid.is_empty() {
        return Ok(false);
    }
    let _persist = PERSIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let text = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES).map_err(settings_error)?;
    let mut settings = Settings::parse(&text)
        .map_err(|_| ApiError::coded(StatusCode::CONFLICT, "project_config_invalid", Map::new()))?;
    if !settings.checks.is_empty() {
        return Ok(false);
    }
    settings.checks = if settings.environment == sdd::Environment::Docker {
        sdd::docker_command::normalize_all("", &valid)
            .map_err(|_| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "test_command_docker", Map::new()))?
            .1
    } else {
        valid
    };
    let rendered = settings
        .render()
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "test_command_save_failed", Map::new()))?;
    replace_atomic(root, SETTINGS_REL, rendered.as_bytes())
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "test_command_save_failed", Map::new()))?;
    Ok(true)
}

/// Falhas de leitura de `settings.yaml` na escrita: symlink → 409 seguro; I/O → 500; resto
/// (ausente, diretório, FIFO, grande) → 409 com a mesma mensagem da inicialização.
fn settings_error(e: SafeFsError) -> ApiError {
    match e {
        SafeFsError::Unsafe => safe_error(e),
        SafeFsError::Io => ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "test_command_save_failed", Map::new()),
        _ => ApiError::coded(StatusCode::CONFLICT, "project_config_invalid", Map::new()),
    }
}

/// `.sdd` real (não symlink), `settings.yaml` regular e sem symlink no caminho.
fn sdd_initialized(root: &Path) -> bool {
    matches!(fs::symlink_metadata(root.join(".sdd")), Ok(m) if m.file_type().is_dir())
        && matches!(fs::symlink_metadata(root.join(SETTINGS_REL)), Ok(m) if m.file_type().is_file())
        && safe_fs::check_chain(root, SETTINGS_REL).is_ok()
}

/// Monta o estado a partir do `.sdd` do projeto. Erros de um arquivo viram `error` no item.
/// `settings.yaml` só é lido quando o `.sdd` é um diretório real inicializado.
/// Nunca executa git: a branch base sem `settings.yaml` utilizável vem de `read_head_branch`.
pub fn build_state(root: &Path, project: &Project, secrets: &SecretSet) -> ProjectStateView {
    build_state_at(root, project, secrets, None)
}

/// [`build_state`] com a espera da revisão do plano (spec 092 T02): `review_now` é o instante
/// atual quando o papel `reviewer` está configurado (`None` sem Revisor: nada de `pending`).
pub fn build_state_at(root: &Path, project: &Project, secrets: &SecretSet, review_now: Option<SystemTime>) -> ProjectStateView {
    let is_git = is_git_repo(root);
    let initialized = sdd_initialized(root);
    let mut yolo = false;
    let mut architect_internet = false;
    let (base_branch, test_command, test_command_redacted, specs, limits) = if initialized {
        let settings = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES)
            .ok()
            .and_then(|text| Settings::parse(&text).ok());
        yolo = settings.as_ref().is_some_and(|s| s.yolo);
        architect_internet = settings.as_ref().is_some_and(|s| s.architect_internet);
        match settings {
            Some(settings) => (
                text_field(secrets, "base_branch", &settings.base_branch),
                test_command_view(secrets, &settings.test_command),
                test_command_redacted(secrets, &settings.test_command),
                load_specs(root, secrets, review_now),
                (settings.max_attempts, settings.max_test_cycles),
            ),
            None => (
                text_field(secrets, "base_branch", &read_head_branch(root)),
                String::new(),
                false,
                load_specs(root, secrets, review_now),
                (sdd::DEFAULT_LIMIT, sdd::DEFAULT_LIMIT),
            ),
        }
    } else {
        (
            text_field(secrets, "base_branch", &read_head_branch(root)),
            String::new(),
            false,
            Vec::new(),
            (sdd::DEFAULT_LIMIT, sdd::DEFAULT_LIMIT),
        )
    };
    ProjectStateView {
        id: id_view(secrets, &project.id),
        name: text_field(secrets, "name", &project.name),
        is_git_repo: is_git,
        initialized,
        base_branch,
        test_command,
        test_command_redacted,
        max_attempts: limits.0,
        max_test_cycles: limits.1,
        yolo,
        architect_internet,
        web_search_available: false,
        image_generation_available: false,
        running: false,
        run: None,
        specs,
        pending_permissions: Vec::new(),
        report_pending: false,
        architect_turn: None,
    }
}

/// Nomes de entradas reais de `dir`: diretórios (`dirs=true`) ou arquivos regulares,
/// ordenados. Symlinks e arquivos especiais são ignorados; `DirEntry::file_type` não segue links.
pub(super) fn real_children(dir: &Path, dirs: bool) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| {
            e.file_type()
                .is_ok_and(|t| if dirs { t.is_dir() } else { t.is_file() })
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Pendências do disco para `GET /api/attention` (spec 093 T01): plano aguardando aprovação e
/// execução pausada (tarefa BLOCKED) ou parada da spec ativa, com a mesma escolha de spec ativa e as
/// mesmas regras de `pickActiveSpec`/`isPausedRun`/`isStalledRun` do cliente. `running` = execução
/// ativa do projeto; `review_now` é o mesmo de [`build_state_at`] (revisão do plano `pending`).
/// O plano só conta quando o usuário pode aprová-lo: sem revisão `pending`/`running` (o cliente
/// desabilita o Aprovar). Só ids e rótulos fixos: nada de texto do modelo.
pub(super) fn disk_attention(
    root: &Path,
    secrets: &SecretSet,
    running: bool,
    review_now: Option<SystemTime>,
) -> super::attention::DiskAttention {
    use super::attention::{DiskAttention, EscalationOption, PausedOrStalled};
    // Mesma visão do `build_state_at`: basta o SDD inicializado; `settings.yaml` ilegível não
    // esconde as specs (o `/state` também as lista).
    if !sdd_initialized(root) {
        return DiskAttention::default();
    }
    let specs = load_specs(root, secrets, review_now);
    let last = |status: SpecStatus| specs.iter().rev().find(|s| s.status == Some(status));
    let Some(active) = last(SpecStatus::InProgress)
        .or_else(|| last(SpecStatus::AwaitingApproval))
        .or_else(|| last(SpecStatus::Draft))
        .or_else(|| specs.last())
    else {
        return DiskAttention::default();
    };
    let mut out = DiskAttention {
        plan_awaiting_approval: !running
            && active.status == Some(SpecStatus::AwaitingApproval)
            && !active.plan_review.as_ref().is_some_and(|r| {
                r.status == super::plan_review::STATUS_PENDING || r.status == super::plan_review::STATUS_RUNNING
            }),
        paused_or_stalled: None,
    };
    if running || active.status != Some(SpecStatus::InProgress) {
        return out;
    }
    let blocked: Vec<&TaskView> = active.tasks.iter().filter(|t| t.status == Some(TaskStatus::Blocked)).collect();
    if let Some(first) = blocked.first() {
        // A tarefa com opções de escalada vigentes tem a ação; sem nenhuma, a primeira bloqueada.
        let task = blocked.iter().find(|t| !t.recovery_options.is_empty()).unwrap_or(first);
        let options: Vec<EscalationOption> = task
            .recovery_options
            .iter()
            .map(|o| EscalationOption { id: o.id.clone(), kind: o.kind.as_str() })
            .collect();
        out.paused_or_stalled = Some(PausedOrStalled {
            kind: "paused",
            spec_id: active.id.clone(),
            task_id: Some(task.id.clone()),
            analysis: task.recovery_options_analysis.filter(|_| !options.is_empty()),
            escalation_options: (!options.is_empty()).then_some(options),
            stop_code: None,
            role: None,
        });
    } else if active.tasks.iter().any(|t| t.status != Some(TaskStatus::Done)) {
        // Spec 133 T02: a parada pelo provedor diz o motivo e o papel no sino.
        let (stop_code, role) = super::attention::provider_stop(root, &active.id);
        out.paused_or_stalled = Some(PausedOrStalled {
            kind: "stalled",
            spec_id: active.id.clone(),
            task_id: None,
            analysis: None,
            escalation_options: None,
            stop_code,
            role,
        });
    }
    out
}

/// Ordem de execução das tarefas exibidas (spec 128 T04, [`crate::sdd::execution_order`]); vazia
/// com ciclo.
fn execution_order_view(tasks: &[TaskView]) -> Vec<String> {
    let plan: Vec<(String, Vec<String>)> = tasks.iter().map(|t| (t.id.clone(), t.depends_on.clone())).collect();
    crate::sdd::execution_order::execution_order(&plan).unwrap_or_default()
}

/// Lista specs de `.sdd/specs` (caminho inteiro sem symlink). Ausente → vazio.
fn load_specs(root: &Path, secrets: &SecretSet, review_now: Option<SystemTime>) -> Vec<SpecView> {
    if safe_fs::check_chain(root, SPECS_REL).is_err() {
        return Vec::new();
    }
    let profiles = load_profiles(root);
    real_children(&root.join(SPECS_REL), true)
        .iter()
        // Pastas ocultas (ex.: `.chat-attachments`, spec 082 T05) não são specs.
        .filter(|name| !name.starts_with('.'))
        .map(|name| {
            let spec_rel = format!("{SPECS_REL}/{name}");
            let tasks = load_tasks(root, &spec_rel, secrets);
            let tool_counts = super::tools_trail::counts(root, name, secrets);
            let states: Vec<(String, bool)> = tasks.iter().map(|t| (t.id.clone(), t.status == Some(TaskStatus::Done))).collect();
            let cost_alerts = super::cost_alert::alerts(&super::cost_alert::spend_by_task(root, name), &states);
            let execution_order = execution_order_view(&tasks);
            match read_spec(root, &format!("{spec_rel}/spec.md")) {
                Ok(spec) => SpecView {
                    id: id_view(secrets, &spec.frontmatter.id),
                    title: text_field(secrets, "title", &spec.frontmatter.title),
                    status: Some(spec.frontmatter.status),
                    tasks,
                    execution_order,
                    error: None,
                    environment: env_view(root, &spec, secrets),
                    criteria: criteria_view(secrets, &spec.body, crate::agents::runner::GLOBAL_CRITERIA),
                    premises: premises_view(secrets, &spec.body),
                    role_guidance: role_guidance_view(secrets, &spec.body),
                    skills: skills_view(&profiles, secrets, &spec.frontmatter.id),
                    scope_warnings: if in_plan(&spec) {
                        plan_scope_warnings(root, &spec_rel)
                            .iter()
                            .map(|w| scrub_text(secrets, w))
                            .collect()
                    } else {
                        Vec::new()
                    },
                    coverage_warnings: if in_plan(&spec) {
                        plan_coverage_warnings(root, &spec_rel, &spec, secrets)
                    } else {
                        Vec::new()
                    },
                    plan_review: if spec.frontmatter.status == SpecStatus::AwaitingApproval {
                        match review_now {
                            Some(now) => super::plan_review::view_at(root, name, secrets, true, now),
                            None => super::plan_review::view(root, name, secrets),
                        }
                    } else {
                        None
                    },
                    plan_changes: if spec.frontmatter.status == SpecStatus::AwaitingApproval {
                        super::plan_changes::view(root, name, secrets)
                    } else {
                        None
                    },
                    tool_counts,
                    cost_alerts,
                },
                Err(_) => SpecView {
                    id: id_view(secrets, name),
                    title: String::new(),
                    status: None,
                    tasks,
                    execution_order,
                    error: Some(INVALID_FILE),
                    environment: None,
                    criteria: Vec::new(),
                    premises: Vec::new(),
                    role_guidance: Vec::new(),
                    skills: Vec::new(),
                    scope_warnings: Vec::new(),
                    coverage_warnings: Vec::new(),
                    plan_review: None,
                    plan_changes: None,
                    tool_counts,
                    cost_alerts,
                },
            }
        })
        .collect()
}

/// Avisos de escopo das tarefas legíveis de uma spec (`spec_rel` = `.sdd/specs/<id>`), já filtrados
/// e agrupados por tarefa (spec 094 T02): a UI e a autocorreção do Arquiteto usam esta forma.
pub(super) fn plan_scope_warnings(root: &Path, spec_rel: &str) -> Vec<String> {
    crate::sdd::scope_warnings::grouped_scope_warnings(&plan_tasks(root, spec_rel))
}

/// Avisos de cobertura de uma spec em plano, redigidos.
fn plan_coverage_warnings(root: &Path, spec_rel: &str, spec: &Spec, secrets: &SecretSet) -> Vec<String> {
    // Redige antes do corte do critério (60 caracteres): segredo cortado não vaza prefixo.
    crate::sdd::coverage::coverage_warnings(&scrub_text(secrets, &spec.body), &plan_tasks(root, spec_rel))
        .iter()
        .map(|w| scrub_text(secrets, w))
        .collect()
}

/// Avisos determinísticos (escopo e cobertura, redigidos) das specs em plano que o turno do chat
/// alterou (spec 092 T01): assinatura diferente da de `before` ou spec nova. Com mais de uma spec,
/// cada aviso leva o id da spec.
pub(super) fn changed_plan_warnings(root: &Path, before: &BTreeMap<String, String>, secrets: &SecretSet) -> Vec<String> {
    let changed = super::plan_changes::changed_specs(root, before);
    let tagged = changed.len() > 1;
    let mut out = Vec::new();
    for (id, spec) in changed {
        let spec_rel = format!("{SPECS_REL}/{id}");
        let scope = plan_scope_warnings(root, &spec_rel).into_iter().map(|w| scrub_text(secrets, &w));
        let run = plan_run_warnings(root, &spec_rel, &spec, secrets);
        for w in scope.chain(plan_coverage_warnings(root, &spec_rel, &spec, secrets)).chain(run) {
            out.push(if tagged { format!("{id}: {w}") } else { w });
        }
    }
    out
}

/// Aviso do plano sem `run_command` nem a decisão `nothing_to_run` (spec 131 T04). Texto de prompt
/// (vai ao Arquiteto pela conferência do plano), não de tela.
pub(super) const RUN_COMMAND_MISSING_WARNING: &str = "a spec não declara `run_command`: como o usuário vê o produto \
funcionando? Declare no frontmatter o comando que o mostra (num CLI, um exemplo real sobre os dados de amostra; um fluxo \
longo vai para um script do projeto, que o comando chama). Se não houver o que executar (ex.: uma biblioteca usada só \
pelos testes), registre a decisão nas decisões do spec e, numa frase, no frontmatter `nothing_to_run`. Sem saber, \
pergunte ao usuário na resposta";

/// Avisos do plano sobre a execução (spec 131 T04): sem `run_command` nem decisão registrada, e
/// critérios (globais e das tarefas) que citam um campo da spec inexistente ou vazio (detecção em
/// [`crate::sdd::field_citations::missing_fields_cited`], a mesma da execução).
fn plan_run_warnings(root: &Path, spec_rel: &str, spec: &Spec, secrets: &SecretSet) -> Vec<String> {
    use crate::sdd::coverage::{criterion_items, global_criteria, snippet};
    use crate::sdd::field_citations::missing_fields_cited;
    let meta = &spec.frontmatter;
    let mut out = Vec::new();
    if meta.run_command.is_none() && meta.nothing_to_run.is_none() {
        out.push(RUN_COMMAND_MISSING_WARNING.to_string());
    }
    let settings = safe_fs::read_regular(root, SETTINGS_REL, MAX_READ_BYTES)
        .ok()
        .and_then(|t| Settings::parse(&t).ok());
    let mut check = |label: &str, criterion: &str| {
        let fields = missing_fields_cited(criterion, meta, settings.as_ref());
        if fields.is_empty() {
            return;
        }
        let names: Vec<String> = fields.iter().map(|f| format!("`{f}`")).collect();
        out.push(format!(
            "{label}: o critério \"{}\" cita {}, que a spec não declara: declare o campo ou reescreva o critério para \
             citar só o que existe",
            // Redige antes do corte do trecho: segredo cortado não vaza prefixo (como na cobertura).
            scrub_text(secrets, &snippet(&scrub_text(secrets, criterion))),
            names.join(", ")
        ));
    };
    for (i, criterion) in global_criteria(&spec.body).iter().enumerate() {
        check(&format!("G{}", i + 1), criterion);
    }
    for task in plan_tasks(root, spec_rel) {
        let acceptance = crate::agents::runner::section_text(&task.body, crate::agents::runner::ACCEPTANCE).unwrap_or_default();
        for criterion in criterion_items(&acceptance) {
            check(&task.frontmatter.id, &criterion);
        }
    }
    out
}

/// Spec ainda em plano (rascunho ou aguardando aprovação): só então os avisos do plano aparecem.
fn in_plan(spec: &Spec) -> bool {
    matches!(spec.frontmatter.status, SpecStatus::Draft | SpecStatus::AwaitingApproval)
}

/// Tarefas legíveis de `<spec_rel>/tasks`, em ordem de nome. Ausente ou symlink → vazio.
fn plan_tasks(root: &Path, spec_rel: &str) -> Vec<Task> {
    let tasks_rel = format!("{spec_rel}/tasks");
    if safe_fs::check_chain(root, &tasks_rel).is_err() {
        return Vec::new();
    }
    real_children(&root.join(&tasks_rel), false)
        .iter()
        .filter(|n| n.ends_with(".md"))
        .filter_map(|n| read_task(root, &format!("{tasks_rel}/{n}")).ok())
        .collect()
}

/// Orientação de um papel exposta ao cartão do plano.
#[derive(Serialize)]
struct RoleGuidanceView {
    role: &'static str,
    text: String,
}

/// Subseções de `## Orientações por papel` (≤ 1,5 KiB cada, redigidas de segredos).
fn role_guidance_view(secrets: &SecretSet, body: &str) -> Vec<RoleGuidanceView> {
    use crate::sdd::role_guidance::{role_guidance_of, ROLE_DEVELOPER, ROLE_REVIEWER};
    [ROLE_DEVELOPER, ROLE_REVIEWER]
        .into_iter()
        .filter_map(|role| {
            // Redige o texto inteiro antes de truncar: segredo no limite não vaza prefixo.
            let text = role_guidance_of(&scrub_text(secrets, body), role);
            (!text.is_empty()).then_some(RoleGuidanceView { role, text })
        })
        .collect()
}

/// Perfil de especialista exposto ao cartão do plano.
#[derive(Serialize)]
struct SkillView {
    role: &'static str,
    stack: Vec<String>,
    /// Motivo da última reescrita, quando houver.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// `criado`, `atualizado` ou `reaproveitado` em relação a esta spec.
    status: &'static str,
    updated_at: String,
    text: String,
}

/// Perfil lido da raiz, com o conteúdo atual e o do HEAD (para a situação).
struct LoadedProfile {
    role: &'static str,
    profile: crate::sdd::skills::Profile,
    current: String,
    head: Option<String>,
}

/// Lê os perfis da raiz (tolerante) e o conteúdo de cada um no HEAD; uma vez por listagem.
fn load_profiles(root: &Path) -> Vec<LoadedProfile> {
    use crate::sdd::skills::{parse, rel_path, MAX_LESSONS_SECTION_BYTES, MAX_PROFILE_BYTES, ROLES};
    let git = super::spec_result::has_real_git_dir(root).then(|| Git::new(root));
    ROLES
        .into_iter()
        .filter_map(|role| {
            let rel = rel_path(role);
            let max = ((MAX_PROFILE_BYTES + MAX_LESSONS_SECTION_BYTES) * 2) as u64;
            let current = safe_fs::read_regular(root, &rel, max).ok()?;
            let profile = parse(&current)?;
            let head = git.as_ref().and_then(|g| g.show_head(&rel));
            Some(LoadedProfile { role, profile, current, head })
        })
        .collect()
}

/// Perfis para uma spec: texto e motivo redigidos de segredos, situação segundo HEAD e `source_spec`.
fn skills_view(profiles: &[LoadedProfile], secrets: &SecretSet, spec_id: &str) -> Vec<SkillView> {
    profiles
        .iter()
        .map(|p| SkillView {
            role: p.role,
            stack: p.profile.meta.stack.iter().map(|s| text_field(secrets, "stack", s)).collect(),
            reason: p.profile.meta.reason.as_deref().map(|r| scrub_text(secrets, r.trim())).filter(|r| !r.is_empty()),
            status: crate::sdd::skills::status_for(&p.profile, &p.current, p.head.as_deref(), spec_id),
            updated_at: scrub_text(secrets, p.profile.meta.updated_at.trim()),
            text: scrub_text(secrets, p.profile.body.trim()),
        })
        .collect()
}

const MAX_PREMISES: usize = 12;
const MAX_PREMISE_CHARS: usize = 300;

/// Itens de lista da seção `## Premissas` (máx. 12, 300 caracteres cada, redigidos de segredos).
fn premises_view(secrets: &SecretSet, body: &str) -> Vec<String> {
    let mut in_section = false;
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            if in_section {
                break;
            }
            in_section = t.starts_with("##") && t.trim_start_matches('#').trim().eq_ignore_ascii_case("premissas");
            continue;
        }
        if !in_section || out.len() >= MAX_PREMISES {
            continue;
        }
        let item = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "))
            .or_else(|| t.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).map(|(_, r)| r));
        if let Some(item) = item.map(str::trim).filter(|i| !i.is_empty()) {
            // Redige o item inteiro e só depois trunca: segredo no limite não vaza prefixo.
            let clean = scrub_text(secrets, item);
            out.push(clean.chars().take(MAX_PREMISE_CHARS).collect());
        }
    }
    out
}

const MAX_CRITERIA: usize = 24;
const MAX_CRITERION_CHARS: usize = 600;

/// Critérios da seção `## <title>` (spec 136 T04): cada item de primeiro nível, com as linhas da
/// sublista e as de continuação juntadas a ele, redigido de segredos ANTES do corte (máx. 24 itens
/// de 600 caracteres, com "…" quando corta). As citações `kb:` ficam como estão no arquivo.
fn criteria_view(secrets: &SecretSet, body: &str, title: &str) -> Vec<String> {
    let Some(section) = crate::agents::runner::section_text(body, title) else {
        return Vec::new();
    };
    let mut items: Vec<String> = Vec::new();
    let mut open = false;
    for line in section.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(item) = crate::sdd::coverage::criterion_items(line).into_iter().next() {
            if items.len() >= MAX_CRITERIA {
                break;
            }
            items.push(item);
            open = true;
            continue;
        }
        // Sublista ou continuação: junta ao item aberto, sem o marcador.
        if let (true, Some(last)) = (open, items.last_mut()) {
            let t = line.trim();
            let t = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).unwrap_or(t).trim();
            last.push(' ');
            last.push_str(t);
        }
    }
    items
        .iter()
        .map(|item| {
            let clean = scrub_text(secrets, item);
            if clean.chars().count() <= MAX_CRITERION_CHARS {
                return clean;
            }
            let mut cut: String = clean.chars().take(MAX_CRITERION_CHARS).collect();
            cut.push('…');
            cut
        })
        .collect()
}

/// Ambiente docker da spec em rascunho (os arquivos são redigidos de segredos).
fn env_view(root: &Path, spec: &Spec, secrets: &SecretSet) -> Option<EnvView> {
    if spec.frontmatter.environment.as_deref() != Some("docker")
        || !matches!(spec.frontmatter.status, SpecStatus::Draft | SpecStatus::AwaitingApproval)
    {
        return None;
    }
    let mut summary = crate::sdd::environment::summarize(root)?;
    for f in &mut summary.files {
        f.content = scrub_text(secrets, &f.content);
    }
    Some(EnvView { mode: "docker", summary })
}

pub(super) fn read_spec(root: &Path, rel: &str) -> Result<Spec, ()> {
    let text = safe_fs::read_regular(root, rel, MAX_READ_BYTES).map_err(|_| ())?;
    Spec::parse(&text).map_err(|_| ())
}

pub(super) fn read_task(root: &Path, rel: &str) -> Result<Task, ()> {
    let text = safe_fs::read_regular(root, rel, MAX_READ_BYTES).map_err(|_| ())?;
    Task::parse(&text).map_err(|_| ())
}

/// Tarefas `*.md` de `<spec_rel>/tasks`, ordenadas. Ausente ou symlink → vazio.
pub(super) fn load_tasks(root: &Path, spec_rel: &str, secrets: &SecretSet) -> Vec<TaskView> {
    let tasks_rel = format!("{spec_rel}/tasks");
    if safe_fs::check_chain(root, &tasks_rel).is_err() {
        return Vec::new();
    }
    real_children(&root.join(&tasks_rel), false)
        .iter()
        .filter_map(|name| {
            let stem = name.strip_suffix(".md")?;
            Some((name, stem))
        })
        .map(|(name, stem)| match read_task(root, &format!("{tasks_rel}/{name}")) {
            Ok(task) => {
                let (analysis, options) = if task.frontmatter.status == TaskStatus::Blocked {
                    super::recovery::current_options_numbered(&task.body).map_or((None, Vec::new()), |(n, o)| (Some(n), o))
                } else {
                    (None, Vec::new())
                };
                let meta = task.frontmatter;
                TaskView {
                    id: id_view(secrets, &meta.id),
                    title: text_field(secrets, "title", &meta.title),
                    status: Some(meta.status),
                    attempts: meta.attempts,
                    max_attempts: meta.max_attempts,
                    depends_on: meta
                        .depends_on
                        .iter()
                        .map(|d| id_view(secrets, d))
                        .collect(),
                    last_review_verdict: Some(meta.last_verdict),
                    next_action: next_action_view(secrets, meta.next_action.as_deref()),
                    recovery_options: options,
                    recovery_options_analysis: analysis,
                    criteria: criteria_view(secrets, &task.body, crate::agents::runner::ACCEPTANCE),
                    error: None,
                }
            }
            Err(_) => TaskView {
                id: id_view(secrets, stem),
                title: String::new(),
                status: None,
                attempts: 0,
                max_attempts: 0,
                depends_on: Vec::new(),
                last_review_verdict: None,
                next_action: None,
                recovery_options: Vec::new(),
                recovery_options_analysis: None,
                criteria: Vec::new(),
                error: Some(INVALID_FILE),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::{env, process};

    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, Response, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::*;
    use crate::config::{AgentConfig, Config, Connection, Effort};
    use crate::server::{router, AppState};

    const PORT: u16 = 3000;
    const HOST: &str = "127.0.0.1:3000";
    const SECRET: &str = "sk-SEGREDO-LITERAL-1234";

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("nants-projects-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn state_in(dir: &Path, cfg: Config) -> AppState {
        AppState::new(dir.join("config.toml"), crate::server::readiness::chat_ready(cfg), PORT)
    }

    fn secret_config() -> Config {
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "literal".into(),
            name: "Literal".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        cfg.agents.architect = AgentConfig {
            connection_id: "literal".into(),
            model: "gpt-4o".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        cfg
    }

    fn post_project(name: &str, path: &str) -> Request<Body> {
        let body = json!({ "name": name, "path": path }).to_string();
        request(Method::POST, "/api/projects", &body, Some("application/json"))
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

    fn get_req(uri: &str) -> Request<Body> {
        request(Method::GET, uri, "", None)
    }

    async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, String) {
        let resp: Response<Body> = router(state.clone()).oneshot(req).await.expect("resposta");
        let status = resp.status();
        let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("corpo");
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    fn json_of(text: &str) -> Value {
        serde_json::from_str(text).expect("json")
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("pai")).expect("mkdir");
        fs::write(path, content).expect("escrever");
    }

    const SPEC_MD: &str = r#"---
id: "001-demo"
title: "Demo com SEGREDO-TITULO"
status: "IN_PROGRESS"
base_branch: "main"
created_at: "2026-10-13T21:00:00Z"
---

## Contexto
Corpo da spec.
"#;

    const TASK_T01: &str = r#"---
id: "T01"
spec_id: "001-demo"
title: "Primeira tarefa"
status: "IN_REVIEW"
depends_on: []
branch: "sdd/001-demo/T01"
attempts: 2
max_attempts: 3
last_review_verdict: "CHANGES_REQUESTED"
allowed_scope:
  - "src/**"
---

## Objetivo
Texto.
"#;

    #[test]
    fn utc_formatting_matches_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn project_id_is_slug_plus_stable_hash() {
        let id = project_id("Meu Projeto!! Ç", "/x/y");
        assert!(id.starts_with("meu-projeto-"), "{id}");
        assert_eq!(id, project_id("Meu Projeto!! Ç", "/x/y"));
        assert_ne!(id, project_id("Meu Projeto!! Ç", "/x/z"));
        assert!(validate_project_id(&id).is_ok());
        assert!(project_id("***", "/a").starts_with("projeto-"));
        let long = project_id(&"a".repeat(100), "/a");
        assert_eq!(long.len(), SLUG_MAX_CHARS + 1 + 8);
    }

    #[test]
    fn system_dirs_match_by_component() {
        assert!(is_system_dir(Path::new("/proc/self")));
        assert!(is_system_dir(Path::new("/etc")));
        assert!(!is_system_dir(Path::new("/etcetera")));
        assert!(!is_system_dir(Path::new("/home/u")));
    }

    #[test]
    fn path_shape_rejects_relative_dotdot_and_control() {
        assert!(validate_path_shape("relativo").is_err());
        assert!(validate_path_shape("/a/../b").is_err());
        assert!(validate_path_shape("/a/\u{1}b").is_err());
        assert!(validate_path_shape(&format!("/{}", "a".repeat(MAX_PATH_BYTES))).is_err());
        assert!(validate_path_shape("/a/..b").is_ok());
        assert!(validate_path_shape("/tmp/ok").is_ok());
    }

    #[tokio::test]
    async fn create_is_idempotent_by_canonical_path() {
        let dir = temp_dir("idem");
        let proj = dir.join("proj");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let path = proj.to_str().expect("utf8");

        let (status, body) = call(&state, post_project("Alpha", path)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let first = json_of(&body);
        let (status, body) = call(&state, post_project("Outro nome", path)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["id"], first["id"]);

        let cfg = state.config().read().await;
        assert_eq!(cfg.projects.len(), 1, "não duplica");
        drop(cfg);
        let saved = Config::load_from(&dir.join("config.toml")).expect("load");
        assert_eq!(saved.projects.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("config.toml")).expect("m").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn invalid_paths_get_static_4xx_without_echo() {
        let dir = temp_dir("badpath");
        let file = dir.join("arquivo-xyz.txt");
        fs::write(&file, "x").expect("write");
        let state = state_in(&dir, Config::default());
        let missing = dir.join("nao-existe-xyz");
        let cases = [
            ("relativo-xyz/sub".to_string(), StatusCode::BAD_REQUEST),
            (format!("{}/../x", dir.display()), StatusCode::BAD_REQUEST),
            ("/tmp/\u{1}xyz".to_string(), StatusCode::BAD_REQUEST),
            (missing.display().to_string(), StatusCode::BAD_REQUEST),
            (file.display().to_string(), StatusCode::BAD_REQUEST),
            (String::new(), StatusCode::BAD_REQUEST),
        ];
        for (path, expected) in cases {
            let (status, body) = call(&state, post_project("P", &path)).await;
            assert_eq!(status, expected, "path={path:?} body={body}");
            assert!(json_of(&body)["error"].is_string(), "{body}");
            assert!(!body.contains("xyz"), "eco de entrada: {body}");
        }
        assert!(state.config().read().await.projects.is_empty());
    }

    #[tokio::test]
    async fn invalid_name_is_422() {
        let dir = temp_dir("badname");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let path = proj.to_str().expect("utf8");
        for name in ["   ".to_string(), "a\u{7}b".to_string(), "n".repeat(129)] {
            let (status, body) = call(&state, post_project(&name, path)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        }
        assert!(!proj.join(".sdd").exists());
    }

    #[tokio::test]
    async fn system_dirs_and_root_are_forbidden() {
        let state = state_in(&temp_dir("sys"), Config::default());
        for path in ["/proc", "/etc", "/", "/dev"] {
            let (status, body) = call(&state, post_project("P", path)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
        }
    }

    #[tokio::test]
    async fn existing_sdd_is_never_overwritten() {
        let dir = temp_dir("keepsdd");
        let proj = dir.join("p");
        let settings = proj.join(".sdd").join("settings.yaml");
        let custom = "base_branch: \"dev\"\nmerge_strategy: \"custom\"\ntest_command: \"make\"\n";
        write(&settings, custom);
        write(&proj.join(".sdd/specs/001-x/spec.md"), SPEC_MD);
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(fs::read_to_string(&settings).expect("read"), custom);
        assert!(proj.join(".sdd/specs/001-x/spec.md").is_file());
    }

    #[tokio::test]
    async fn new_sdd_gets_default_settings_and_empty_specs() {
        let dir = temp_dir("defaults");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(json_of(&body)["is_git_repo"], true);
        assert_eq!(json_of(&body)["git_initialized"], true);
        assert_eq!(json_of(&body)["initialized"], true);
        let s = crate::sdd::read_settings(&proj.join(".sdd/settings.yaml")).expect("settings");
        assert_eq!(s.base_branch, "main");
        assert_eq!(s.merge_strategy, "squash");
        assert_eq!(s.test_command, "");
        assert!(s.allowed_commands.is_empty() && s.denied_commands.is_empty());
        assert!(proj.join(".sdd/specs").is_dir());
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().expect("git");
        assert!(out.status.success(), "{args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn plain_dir_gets_init_with_main_and_one_empty_commit() {
        let dir = temp_dir("nongit");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(json_of(&body)["git_initialized"], true);
        assert_eq!(git_out(&proj, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
        assert_eq!(git_out(&proj, &["rev-list", "--count", "HEAD"]), "1");
        assert_eq!(git_out(&proj, &["log", "-1", "--format=%s"]), "chore: inicializa repositório (Nants)");
        assert!(git_out(&proj, &["status", "--porcelain"]).is_empty(), ".sdd não aparece como alteração");
        // Spec 115 T04 (C-09): em projeto en, o mesmo prefixo e o texto em inglês.
        let en_dir = temp_dir("nongit-en");
        let en_proj = en_dir.join("p");
        fs::create_dir_all(&en_proj).expect("mkdir");
        let en_state = state_in(&en_dir, Config::default());
        en_state.set_lang(None, crate::i18n::Lang::En);
        let (status, body) = call(&en_state, post_project("P", en_proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(git_out(&en_proj, &["log", "-1", "--format=%s"]), "chore: initialize repository (Nants)");
        assert!(!proj.join(".gitignore").exists());
        // Recadastrar não cria outro commit.
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["git_initialized"], false);
        assert_eq!(git_out(&proj, &["rev-list", "--count", "HEAD"]), "1");
    }

    #[tokio::test]
    async fn dir_inside_other_repo_is_409_and_nothing_is_created() {
        let dir = temp_dir("nested");
        git_out(&dir, &["init", "-q"]);
        let proj = dir.join("sub");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("dentro do repositório Git"), "{body}");
        // Spec 102: `code` e o caminho em `params`, para a interface mostrar no idioma ativo.
        assert!(body.contains("\"code\":\"project_nested_repo\""), "{body}");
        assert!(!proj.join(".git").exists() && !proj.join(".sdd").exists());
    }

    #[tokio::test]
    async fn unborn_repo_gets_only_the_initial_commit() {
        let dir = temp_dir("unborn");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        git_out(&proj, &["init", "-q"]);
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(json_of(&body)["git_initialized"], true);
        assert_eq!(git_out(&proj, &["rev-list", "--count", "HEAD"]), "1");
    }

    #[tokio::test]
    async fn repo_with_commits_keeps_head() {
        let dir = temp_dir("withcommits");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        git_out(&proj, &["init", "-q"]);
        git_out(&proj, &["-c", "user.name=T", "-c", "user.email=t@t", "commit", "--allow-empty", "-m", "x"]);
        let head = git_out(&proj, &["rev-parse", "HEAD"]);
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(json_of(&body)["git_initialized"], false);
        assert_eq!(git_out(&proj, &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn commit_uses_nants_identity_when_none_is_configured() {
        let dir = temp_dir("noident");
        let home = dir.join("home");
        fs::create_dir_all(&home).expect("home");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let home_text = home.to_str().expect("u").to_string();
        let repo = Git::new(&proj)
            .with_env("HOME", &home_text)
            .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
            .with_env("GIT_CONFIG_NOSYSTEM", "1");
        repo.init_main().expect("init");
        assert_eq!(repo.identity_fallback().len(), 2);
        repo.commit_empty("chore: x").expect("commit");
        assert_eq!(git_out(&proj, &["log", "-1", "--format=%an <%ae>"]), "Nants <nants@localhost>");
    }

    #[tokio::test]
    async fn git_repo_gets_sdd_exclude_once_and_keeps_gitignore() {
        let dir = temp_dir("gitrepo");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let ok = Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(&proj)
            .status()
            .expect("git init")
            .success();
        assert!(ok);
        write(&proj.join(".gitignore"), "target/\n");
        let state = state_in(&dir, Config::default());
        let path = proj.to_str().expect("u");
        let (status, body) = call(&state, post_project("P", path)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(json_of(&body)["is_git_repo"], true);
        let (status, _) = call(&state, post_project("P", path)).await;
        assert_eq!(status, StatusCode::OK);

        let exclude = fs::read_to_string(proj.join(".git/info/exclude")).expect("exclude");
        assert_eq!(exclude.lines().filter(|l| *l == EXCLUDE_LINE).count(), 1, "{exclude}");
        // Spec 082 T03: as cópias das capturas vistas pelo Revisor ficam fora do Git.
        assert_eq!(exclude.lines().filter(|l| *l == CAPTURES_EXCLUDE_LINE).count(), 1, "{exclude}");
        // Spec 122 T02: as íntegras das saídas de ferramenta também.
        assert_eq!(exclude.lines().filter(|l| *l == OUTPUTS_EXCLUDE_LINE).count(), 1, "{exclude}");
        // Spec 129 T01: a pasta pessoal do workspace fica fora do Git.
        assert_eq!(exclude.lines().filter(|l| *l == crate::harness::home::EXCLUDE_LINE).count(), 1, "{exclude}");
        assert_eq!(fs::read_to_string(proj.join(".gitignore")).expect("gi"), "target/\n");
    }

    fn mkfifo(path: &Path) -> bool {
        Command::new("mkfifo")
            .arg(path)
            .status()
            .is_ok_and(|s| s.success())
    }

    fn git_repo(dir: &Path) {
        assert!(Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(dir)
            .status()
            .expect("git init")
            .success());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_git_dir_is_rejected_without_writes() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("sym-git");
        let proj = dir.join("p");
        let outside = dir.join("outside");
        fs::create_dir_all(&proj).expect("p");
        fs::create_dir_all(outside.join("info")).expect("o");
        symlink(&outside, proj.join(".git")).expect("ln");
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("symlink"), "{body}");
        assert!(!proj.join(".sdd").exists());
        assert!(!outside.join("info/exclude").exists());
        assert!(state.config().read().await.projects.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_sdd_writes_nothing_outside() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("sym-sdd");
        let proj = dir.join("p");
        let outside = dir.join("outside");
        fs::create_dir_all(&proj).expect("p");
        fs::create_dir_all(&outside).expect("o");
        symlink(&outside, proj.join(".sdd")).expect("ln");
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(fs::read_dir(&outside).expect("ls").count(), 0, "nada criado fora");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dangling_settings_symlink_is_not_created_outside() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("dangling-settings");
        let proj = dir.join("p");
        let target = dir.join("alvo-settings.yaml");
        fs::create_dir_all(proj.join(".sdd")).expect("sdd");
        symlink(&target, proj.join(".sdd/settings.yaml")).expect("ln");
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(!target.exists(), "alvo não pode ser criado");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exclude_symlink_is_rejected_and_target_intact() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("exclude-sym");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        git_repo(&proj);
        let victim = dir.join("victim.txt");
        fs::write(&victim, "intacto").expect("v");
        fs::remove_file(proj.join(".git/info/exclude")).expect("rm");
        symlink(&victim, proj.join(".git/info/exclude")).expect("ln");
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(fs::read_to_string(&victim).expect("r"), "intacto");
        assert!(!proj.join(".sdd").exists(), "nada escrito antes da recusa");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exclude_fifo_post_is_fast_and_does_not_hold_config_lock() {
        let dir = temp_dir("exclude-fifo");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        git_repo(&proj);
        let exclude = proj.join(".git/info/exclude");
        fs::remove_file(&exclude).expect("rm");
        if !mkfifo(&exclude) {
            return; // mkfifo indisponível neste ambiente
        }
        let state = state_in(&dir, Config::default());
        let post = call(&state, post_project("P", proj.to_str().expect("u")));
        let other = call(&state, get_req("/api/config"));
        let (post_res, other_res) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(post, other)
        })
        .await
        .expect("POST com FIFO não pode travar");
        assert_eq!(post_res.0, StatusCode::CONFLICT, "{}", post_res.1);
        assert_eq!(other_res.0, StatusCode::OK, "GET /api/config deve responder");
        assert!(!proj.join(".sdd").exists(), "nada criado antes da recusa");
        assert!(state.config().read().await.projects.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_posts_for_same_path_create_one_project() {
        let dir = temp_dir("concurrent");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        let state = state_in(&dir, Config::default());
        let path = proj.to_str().expect("u").to_string();
        let a = call(&state, post_project("Alpha", &path));
        let b = call(&state, post_project("Beta", &path));
        let (ra, rb) = tokio::join!(a, b);
        let mut statuses = vec![ra.0, rb.0];
        statuses.sort();
        assert_eq!(statuses, vec![StatusCode::OK, StatusCode::CREATED]);
        assert_eq!(state.config().read().await.projects.len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spec_symlink_devzero_and_fifo_do_not_hang_state() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("state-special");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        let state = state_in(&dir, Config::default());
        let (_, created) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        let spec_dir = proj.join(".sdd/specs/001-demo");
        fs::create_dir_all(spec_dir.join("tasks")).expect("tasks");
        symlink("/dev/zero", spec_dir.join("spec.md")).expect("ln");
        let uri = format!("/api/projects/{id}/state");
        let (status, body) = tokio::time::timeout(Duration::from_secs(2), call(&state, get_req(&uri)))
            .await
            .expect("state não pode travar com /dev/zero");
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["specs"][0]["error"], INVALID_FILE);

        fs::remove_file(spec_dir.join("spec.md")).expect("rm");
        if !mkfifo(&spec_dir.join("spec.md")) {
            return;
        }
        let (status, body) = tokio::time::timeout(Duration::from_secs(2), call(&state, get_req(&uri)))
            .await
            .expect("state não pode travar com FIFO");
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["specs"][0]["error"], INVALID_FILE);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_spec_dir_is_ignored_and_oversized_file_is_invalid() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("state-links");
        let proj = dir.join("p");
        let outside = dir.join("outside");
        fs::create_dir_all(&proj).expect("p");
        write(&outside.join("spec.md"), SPEC_MD);
        let state = state_in(&dir, Config::default());
        let (_, created) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        fs::create_dir_all(proj.join(".sdd/specs")).expect("specs");
        symlink(&outside, proj.join(".sdd/specs/ext")).expect("ln");
        write(
            &proj.join(".sdd/specs/002-grande/spec.md"),
            &"x".repeat(1024 * 1024 + 1),
        );
        let (status, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        let ids: Vec<&str> = v["specs"]
            .as_array()
            .expect("arr")
            .iter()
            .map(|s| s["id"].as_str().expect("id"))
            .collect();
        assert_eq!(ids, vec!["002-grande"], "symlink ignorado: {body}");
        assert_eq!(v["specs"][0]["error"], INVALID_FILE);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn settings_is_not_read_unless_sdd_is_real_dir() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("settings-gate");
        let proj = dir.join("p");
        let outside = dir.join("outside");
        fs::create_dir_all(&proj).expect("p");
        fs::create_dir_all(&outside).expect("o");
        symlink("/dev/zero", outside.join("settings.yaml")).expect("ln");
        symlink(&outside, proj.join(".sdd")).expect("ln sdd");
        let mut cfg = Config::default();
        cfg.projects.push(Project {
            id: "p-0000aaaa".into(),
            name: "P".into(),
            path: proj.to_str().expect("u").into(),
            created_at: "2026-10-13T00:00:00Z".into(),
        });
        let state = state_in(&dir, cfg);
        let (status, body) = tokio::time::timeout(
            Duration::from_secs(2),
            call(&state, get_req("/api/projects/p-0000aaaa/state")),
        )
        .await
        .expect("não pode ler /dev/zero");
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        assert_eq!(v["initialized"], false);
        assert_eq!(v["base_branch"], "main");
    }

    #[tokio::test]
    async fn api_key_shaped_ids_are_redacted_in_state() {
        const KEY: &str = "0123456789abcdef0123456789";
        let dir = temp_dir("redact-ids");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "hex".into(),
            name: "Hex".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: KEY.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&dir, cfg);
        let (_, created) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        let spec = format!(
            "---\nid: \"{KEY}\"\ntitle: \"t\"\nstatus: \"IN_PROGRESS\"\nbase_branch: \"main\"\ncreated_at: \"x\"\n---\n"
        );
        write(&proj.join(".sdd/specs/001-demo/spec.md"), &spec);
        let task = format!(
            "---\nid: \"{KEY}\"\nspec_id: \"001-demo\"\ntitle: \"t\"\nstatus: \"TODO\"\ndepends_on: [\"{KEY}\"]\nbranch: \"\"\nattempts: 0\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope: []\n---\n"
        );
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), &task);
        let (status, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains(KEY), "chave vazou: {body}");
        assert!(body.contains(REDACTED_ID), "{body}");
    }

    /// Tarefa BLOCKED no estado: `next_action` redigido, limitado e omitido quando vazio/ausente.
    #[tokio::test]
    async fn blocked_task_next_action_is_scrubbed_bounded_and_optional() {
        const KEY: &str = "0123456789abcdef0123456789";
        let dir = temp_dir("next-action");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "hex".into(),
            name: "Hex".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: KEY.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&dir, cfg);
        let (_, created) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        let spec = "---\nid: \"001-demo\"\ntitle: \"t\"\nstatus: \"IN_PROGRESS\"\nbase_branch: \"main\"\ncreated_at: \"x\"\n---\n";
        write(&proj.join(".sdd/specs/001-demo/spec.md"), spec);
        let blocked = |n: &str, next: &str| {
            format!(
                "---\nid: \"{n}\"\nspec_id: \"001-demo\"\ntitle: \"t\"\nstatus: \"BLOCKED\"\ndepends_on: []\nbranch: \"\"\nattempts: 3\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope: []\nnext_action: \"{next}\"\n---\n"
            )
        };
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), &blocked("T01", &format!("vazou {KEY} aqui")));
        write(&proj.join(".sdd/specs/001-demo/tasks/T02.md"), &blocked("T02", ""));
        write(&proj.join(".sdd/specs/001-demo/tasks/T03.md"), &blocked("T03", &"x".repeat(129)));
        let (status, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains(KEY), "chave vazou: {body}");
        let v = json_of(&body);
        let tasks = &v["specs"][0]["tasks"];
        let next = tasks[0]["next_action"].as_str().expect("next_action");
        assert!(next.starts_with("vazou ") && next.ends_with(" aqui"), "{next}");
        assert!(tasks[1].get("next_action").is_none(), "vazio deve ser omitido: {body}");
        // 129 caracteres: abaixo do limite de 200 do motivo, passa intacto (nunca vira "[inválido]").
        assert_eq!(tasks[2]["next_action"], "x".repeat(129));
        assert_eq!(tasks[0]["status"], "BLOCKED");
        assert_eq!(tasks[0]["attempts"], 3);
        assert!(tasks[0].get("error").is_none());
    }

    /// Motivo do bloqueio: redige antes de truncar, trunca por caracteres com "…", nunca emite
    /// o marcador de inválido e omite só espaços.
    #[tokio::test]
    async fn next_action_truncation_and_redaction_rules() {
        const KEY: &str = "0123456789abcdef0123456789";
        let dir = temp_dir("next-action-bounds");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "hex".into(),
            name: "Hex".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: KEY.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&dir, cfg);
        let (_, created) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        let spec = "---\nid: \"001-demo\"\ntitle: \"t\"\nstatus: \"IN_PROGRESS\"\nbase_branch: \"main\"\ncreated_at: \"x\"\n---\n";
        write(&proj.join(".sdd/specs/001-demo/spec.md"), spec);
        let blocked = |n: &str, next: &str| {
            format!(
                "---\nid: \"{n}\"\nspec_id: \"001-demo\"\ntitle: \"t\"\nstatus: \"BLOCKED\"\ndepends_on: []\nbranch: \"\"\nattempts: 3\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope: []\nnext_action: \"{next}\"\n---\n"
            )
        };
        let long_secret_straddle = format!("{}{KEY} fim", "c".repeat(190));
        let emoji = "😀".repeat(201);
        let accents = "é".repeat(250);
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), &blocked("T01", &"a".repeat(53)));
        write(&proj.join(".sdd/specs/001-demo/tasks/T02.md"), &blocked("T02", &"b".repeat(300)));
        write(&proj.join(".sdd/specs/001-demo/tasks/T03.md"), &blocked("T03", &long_secret_straddle));
        write(&proj.join(".sdd/specs/001-demo/tasks/T04.md"), &blocked("T04", "   "));
        write(&proj.join(".sdd/specs/001-demo/tasks/T05.md"), &blocked("T05", &accents));
        write(&proj.join(".sdd/specs/001-demo/tasks/T06.md"), &blocked("T06", &emoji));
        write(&proj.join(".sdd/specs/001-demo/tasks/T07.md"), &blocked("T07", "linha1\tlinha2"));
        let (status, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains(KEY), "chave vazou: {body}");
        assert!(!body.contains("0123456789"), "prefixo do segredo vazou: {body}");
        assert!(!body.contains("[inválido]"), "marcador de inválido emitido: {body}");
        let v = json_of(&body);
        let tasks = &v["specs"][0]["tasks"];
        assert_eq!(tasks[0]["next_action"], "a".repeat(53), "53 caracteres intactos");
        assert_eq!(tasks[1]["next_action"], format!("{}…", "b".repeat(200)), "300 → 200 + reticências");
        let straddle = tasks[2]["next_action"].as_str().expect("next_action");
        assert!(straddle.starts_with(&"c".repeat(190)), "{straddle}");
        assert!(straddle.ends_with(" fim"), "redação antes do corte: {straddle}");
        assert!(tasks[3].get("next_action").is_none(), "só espaços é omitido");
        assert_eq!(tasks[4]["next_action"], format!("{}…", "é".repeat(200)), "multibyte no limite");
        assert_eq!(tasks[5]["next_action"], format!("{}…", "😀".repeat(200)), "emoji no limite");
        let tab = tasks[6]["next_action"].as_str().expect("next_action");
        assert!(tab.contains("linha1") && tab.contains("linha2"), "{tab}");
    }

    #[tokio::test]
    async fn state_exposes_coverage_warnings_only_for_plans_with_covers() {
        // Spec 086 T07: avisos de cobertura junto dos de escopo, só no plano e só com `covers`.
        let dir = temp_dir("coverage");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let (_, created) = call(&state, post_project("Demo", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        let spec = SPEC_MD.replace("IN_PROGRESS", "AWAITING_APPROVAL").replace(
            "Corpo da spec.\n",
            "Corpo da spec.\n\n## Critérios de Aceitação Globais\n- [ ] Login funciona.\n- [ ] Logout limpa a sessão.\n",
        );
        write(&proj.join(".sdd/specs/001-demo/spec.md"), &spec);
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), TASK_T01);
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert!(json_of(&body)["specs"][0].get("coverage_warnings").is_none(), "sem covers, sem aviso: {body}");

        let covered = TASK_T01.replace("allowed_scope:", "covers: [\"G1\", \"G7\"]\nallowed_scope:");
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), &covered);
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        let v = json_of(&body);
        assert_eq!(
            v["specs"][0]["coverage_warnings"],
            json!(["T01: covers cita G7, que não existe", "G2 (Logout limpa a sessão.) não é coberto por nenhuma tarefa"]),
            "{body}"
        );

        write(&proj.join(".sdd/specs/001-demo/spec.md"), &spec.replace("AWAITING_APPROVAL", "IN_PROGRESS"));
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert!(json_of(&body)["specs"][0].get("coverage_warnings").is_none(), "fora do plano, sem aviso: {body}");
    }

    #[tokio::test]
    async fn state_exposes_plan_review_only_for_current_plan_version() {
        // Spec 086 T08: `plan_review` só na spec AWAITING_APPROVAL e só da assinatura atual.
        let dir = temp_dir("plan-review");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let (_, created) = call(&state, post_project("Demo", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        write(&proj.join(".sdd/specs/001-demo/spec.md"), &SPEC_MD.replace("IN_PROGRESS", "AWAITING_APPROVAL"));
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), TASK_T01);
        let signature = super::super::plan_review::plan_signature(&proj, "001-demo").expect("sig");
        let record = json!({"signature": signature, "at": "2026-10-18T00:00:00Z", "status": "ok",
            "issues": [{"task": "T01", "kind": "scope", "text": "Fora do escopo."}]});
        write(&proj.join(".sdd/specs/001-demo/plan-review.json"), &record.to_string());
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        let v = json_of(&body);
        assert_eq!(v["specs"][0]["plan_review"]["status"], "ok", "{body}");
        assert_eq!(v["specs"][0]["plan_review"]["issues"][0]["task"], "T01");

        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), &TASK_T01.replace("Primeira tarefa", "Primeira tarefa alterada"));
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert!(json_of(&body)["specs"][0].get("plan_review").is_none(), "versão antiga some: {body}");
    }

    #[tokio::test]
    async fn state_exposes_pending_plan_review_only_with_reviewer() {
        // Spec 092 T02: com o Revisor configurado e sem resultado da versão atual, `pending`.
        let dir = temp_dir("plan-review-pending");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let mut cfg = secret_config();
        cfg.agents.reviewer = AgentConfig {
            connection_id: "literal".into(),
            model: "rev".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        let state = state_in(&dir, cfg);
        let (_, created) = call(&state, post_project("Demo", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        write(&proj.join(".sdd/specs/001-demo/spec.md"), &SPEC_MD.replace("IN_PROGRESS", "AWAITING_APPROVAL"));
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), TASK_T01);
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        let v = json_of(&body);
        assert_eq!(v["specs"][0]["plan_review"]["status"], "pending", "{body}");
        assert_eq!(v["specs"][0]["plan_review"]["issues"], json!([]));

        // Fora do plano, nada.
        write(&proj.join(".sdd/specs/001-demo/spec.md"), &SPEC_MD);
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert!(json_of(&body)["specs"][0].get("plan_review").is_none(), "{body}");

        // Sem Revisor configurado, nada (como antes).
        let dir2 = temp_dir("plan-review-pending-none");
        let proj2 = dir2.join("p");
        fs::create_dir_all(&proj2).expect("mkdir");
        let state2 = state_in(&dir2, secret_config());
        let (_, created) = call(&state2, post_project("Demo", proj2.to_str().expect("u"))).await;
        let id2 = json_of(&created)["id"].as_str().expect("id").to_string();
        write(&proj2.join(".sdd/specs/001-demo/spec.md"), &SPEC_MD.replace("IN_PROGRESS", "AWAITING_APPROVAL"));
        write(&proj2.join(".sdd/specs/001-demo/tasks/T01.md"), TASK_T01);
        let (_, body) = call(&state2, get_req(&format!("/api/projects/{id2}/state"))).await;
        assert!(json_of(&body)["specs"][0].get("plan_review").is_none(), "sem Revisor: {body}");
    }

    #[tokio::test]
    async fn state_exposes_plan_changes_only_for_current_plan_version() {
        // Spec 086 T09: `plan_changes` só na spec AWAITING_APPROVAL, da assinatura atual e não vazio.
        let dir = temp_dir("plan-changes");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let (_, created) = call(&state, post_project("Demo", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        write(&proj.join(".sdd/specs/001-demo/spec.md"), &SPEC_MD.replace("IN_PROGRESS", "AWAITING_APPROVAL"));
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), TASK_T01);
        let signature = super::super::plan_review::plan_signature(&proj, "001-demo").expect("sig");
        // Linha em texto (arquivo de antes da spec 131 T06) e linha com código: as duas chegam ao estado.
        let record = json!({"signature": signature, "lines": ["T02 removida", {"code": "task_new", "params": {"id": "T03", "title": "Nova"}}]});
        write(&proj.join(".sdd/specs/001-demo/plan-changes.json"), &record.to_string());
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(json_of(&body)["specs"][0]["plan_changes"], json!(["T02 removida", {"code": "task_new", "params": {"id": "T03", "title": "Nova"}}]), "{body}");

        write(&proj.join(".sdd/specs/001-demo/plan-changes.json"), &json!({"signature": signature, "lines": []}).to_string());
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert!(json_of(&body)["specs"][0].get("plan_changes").is_none(), "sem mudanças, omitido: {body}");

        write(&proj.join(".sdd/specs/001-demo/plan-changes.json"), &record.to_string());
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), &TASK_T01.replace("Primeira tarefa", "Primeira tarefa alterada"));
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert!(json_of(&body)["specs"][0].get("plan_changes").is_none(), "versão antiga some: {body}");
    }

    #[tokio::test]
    async fn coverage_warning_redacts_long_secret_before_cutting_the_criterion() {
        // Spec 086 T07 (F-01): o corte de 60 caracteres vem depois da redação.
        const LONG: &str = "sk-proj-LONGO-0123456789abcdefghijklmnopqrstuvwxyz";
        let dir = temp_dir("coverage-secret");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "longa".into(),
            name: "Chave longa".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: LONG.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = state_in(&dir, cfg);
        let (_, created) = call(&state, post_project("Demo", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        let spec = SPEC_MD.replace("IN_PROGRESS", "AWAITING_APPROVAL").replace(
            "Corpo da spec.\n",
            &format!("Corpo da spec.\n\n## Critérios de Aceitação Globais\n- [ ] Um.\n- [ ] O login aceita a chave de API {LONG} no header.\n"),
        );
        write(&proj.join(".sdd/specs/001-demo/spec.md"), &spec);
        let covered = TASK_T01.replace("allowed_scope:", "covers: [\"G1\"]\nallowed_scope:");
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), &covered);
        let (_, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        let warnings = json_of(&body)["specs"][0]["coverage_warnings"].to_string();
        assert!(warnings.contains("G2 (O login aceita a chave de API ***"), "{warnings}");
        for n in 8..=LONG.len() {
            assert!(!body.contains(&LONG[..n]), "prefixo de {n} caracteres vazou: {body}");
        }
    }

    #[tokio::test]
    async fn state_reflects_real_sdd_without_branch_or_paths() {
        let dir = temp_dir("state");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let path = proj.to_str().expect("u");
        let (_, created) = call(&state, post_project("Demo", path)).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        write(&proj.join(".sdd/specs/001-demo/spec.md"), SPEC_MD);
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), TASK_T01);

        let (status, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        assert_eq!(v["initialized"], true);
        assert_eq!(v["base_branch"], "main");
        assert_eq!(v["specs"][0]["id"], "001-demo");
        assert_eq!(v["specs"][0]["status"], "IN_PROGRESS");
        let task = &v["specs"][0]["tasks"][0];
        assert_eq!(task["id"], "T01");
        assert_eq!(task["status"], "IN_REVIEW");
        assert_eq!(task["attempts"], 2);
        assert_eq!(task["max_attempts"], 3);
        assert_eq!(task["last_review_verdict"], "CHANGES_REQUESTED");
        assert_eq!(task["depends_on"], json!([]));
        assert!(task.get("branch").is_none());
        assert!(!body.contains("\"branch\""), "{body}");
        assert!(!body.contains(proj.to_str().expect("u")), "caminho absoluto vazou: {body}");
        assert!(!body.contains("sdd/001-demo"), "{body}");
    }

    #[tokio::test]
    async fn state_without_sdd_is_uninitialized() {
        let dir = temp_dir("uninit");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let mut cfg = Config::default();
        cfg.projects.push(Project {
            id: "p-1234abcd".into(),
            name: "P".into(),
            path: proj.to_str().expect("u").into(),
            created_at: "2026-10-13T00:00:00Z".into(),
        });
        let state = state_in(&dir, cfg);
        let (status, body) = call(&state, get_req("/api/projects/p-1234abcd/state")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        assert_eq!(v["initialized"], false);
        assert_eq!(v["is_git_repo"], false);
        assert_eq!(v["specs"], json!([]));
    }

    #[tokio::test]
    async fn corrupted_files_do_not_break_state() {
        let dir = temp_dir("corrupt");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, Config::default());
        let (_, created) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        let id = json_of(&created)["id"].as_str().expect("id").to_string();
        write(&proj.join(".sdd/specs/001-demo/spec.md"), "sem frontmatter QUEBRADO-XYZ");
        write(&proj.join(".sdd/specs/001-demo/tasks/T01.md"), TASK_T01);
        write(&proj.join(".sdd/specs/001-demo/tasks/T02.md"), "---\n: : [ QUEBRADO-XYZ\n---\n");

        let (status, body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        assert_eq!(v["specs"][0]["error"], INVALID_FILE);
        assert_eq!(v["specs"][0]["tasks"][0]["id"], "T01");
        assert_eq!(v["specs"][0]["tasks"][1]["error"], INVALID_FILE);
        assert!(!body.contains("QUEBRADO"), "conteúdo ecoado: {body}");
    }

    #[tokio::test]
    async fn invalid_and_unknown_project_ids() {
        let state = state_in(&temp_dir("ids"), Config::default());
        let (status, body) = call(&state, get_req("/api/projects/bad%20id/state")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        let long = "a".repeat(65);
        let (status, _) = call(&state, get_req(&format!("/api/projects/{long}/state"))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, body) = call(&state, get_req("/api/projects/nao-existe/state")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }

    #[tokio::test]
    async fn secrets_in_name_and_titles_are_scrubbed_everywhere() {
        let dir = temp_dir("secret");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let state = state_in(&dir, secret_config());
        let name = format!("proj {SECRET} x");
        let (status, body) = call(&state, post_project(&name, proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(!body.contains(SECRET), "{body}");
        let id = json_of(&body)["id"].as_str().expect("id").to_string();
        assert!(!id.contains(SECRET));

        let spec = SPEC_MD.replace("SEGREDO-TITULO", SECRET);
        write(&proj.join(".sdd/specs/001-demo/spec.md"), &spec);
        let (_, state_body) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert!(!state_body.contains(SECRET), "{state_body}");
        assert!(state_body.contains("***"));

        let (_, cfg_body) = call(&state, get_req("/api/config")).await;
        assert!(!cfg_body.contains(SECRET), "{cfg_body}");
    }

    /// Repositório mínimo montado à mão (sem `git`): `.git/HEAD` e `.git/info/exclude`.
    fn head_repo(dir: &Path, head: &str) -> PathBuf {
        let proj = dir.join("p");
        write(&proj.join(".git/HEAD"), head);
        write(&proj.join(".git/info/exclude"), "");
        proj
    }

    #[tokio::test]
    async fn base_branch_comes_from_head_without_git() {
        let dir = temp_dir("head-branch");
        let proj = head_repo(&dir, "ref: refs/heads/develop\n");
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(json_of(&body)["is_git_repo"], true);
        assert_eq!(json_of(&body)["initialized"], true);
        let s = crate::sdd::read_settings(&proj.join(".sdd/settings.yaml")).expect("settings");
        assert_eq!(s.base_branch, "develop");
    }

    #[tokio::test]
    async fn state_reads_base_branch_from_head_without_git() {
        let dir = temp_dir("state-head");
        let proj = head_repo(&dir, "ref: refs/heads/develop\n");
        let mut cfg = Config::default();
        cfg.projects.push(Project {
            id: "p-0000dead".into(),
            name: "P".into(),
            path: proj.to_str().expect("u").into(),
            created_at: "2026-10-13T00:00:00Z".into(),
        });
        let state = state_in(&dir, cfg);
        let (status, body) = call(&state, get_req("/api/projects/p-0000dead/state")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        assert_eq!(v["base_branch"], "develop");
        assert_eq!(v["is_git_repo"], true);
        assert_eq!(v["initialized"], false);
    }

    #[test]
    fn read_head_branch_parses_only_strict_symbolic_refs() {
        let dir = temp_dir("head-unit");
        let proj = dir.join("p");
        let long_ok = format!("ref: refs/heads/{}\n", "a".repeat(100));
        let long_bad = format!("ref: refs/heads/{}\n", "a".repeat(101));
        let huge = format!("ref: refs/heads/{}\n", "a".repeat(4096));
        let cases: &[(&str, &str)] = &[
            ("ref: refs/heads/develop\n", "develop"),
            ("ref: refs/heads/develop", "develop"),
            ("ref: refs/heads/feature/x-1.2_y\n", "feature/x-1.2_y"),
            (long_ok.as_str(), &long_ok["ref: refs/heads/".len()..long_ok.len() - 1]),
            (long_bad.as_str(), "main"),
            (huge.as_str(), "main"),
            ("ref: refs/heads/../x\n", "main"),
            ("ref: refs/heads/-x\n", "main"),
            ("ref: refs/heads/x/\n", "main"),
            ("ref: refs/heads/x.lock\n", "main"),
            ("ref: refs/heads/a b\n", "main"),
            ("ref: refs/heads/\n", "main"),
            ("ref: refs/heads/a\nref: refs/heads/b\n", "main"),
            ("ref: refs/heads/.x\n", "main"),
            ("ref: refs/heads/.\n", "main"),
            ("ref: refs/heads//x\n", "main"),
            ("ref: refs/heads/a//b\n", "main"),
            ("ref: refs/heads/x.\n", "main"),
            ("ref: refs/heads/a/.b\n", "main"),
            ("ref: refs/heads/a.lock\n", "main"),
            ("ref: refs/heads/a/b.lock/c\n", "main"),
            ("ref: refs/heads/a@{b\n", "main"),
            ("ref: refs/heads/main\n", "main"),
            ("ref: refs/heads/develop\n", "develop"),
            ("ref: refs/heads/feature/x-1\n", "feature/x-1"),
            ("ref: refs/heads/release/1.2.3\n", "release/1.2.3"),
            ("ref: refs/heads/user/a_b\n", "user/a_b"),
            ("ref: refs/remotes/origin/x\n", "main"),
            ("0123456789abcdef0123456789abcdef01234567\n", "main"),
            ("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n", "main"),
        ];
        for &(head, expected) in cases {
            write(&proj.join(".git/HEAD"), head);
            assert_eq!(read_head_branch(&proj), expected, "HEAD={:?}", head.len());
        }
        fs::remove_file(proj.join(".git/HEAD")).expect("rm");
        assert_eq!(read_head_branch(&proj), "main", "HEAD ausente");
        assert_eq!(read_head_branch(&dir.join("inexistente")), "main");
    }

    #[test]
    fn read_head_branch_ignores_gitfile_and_missing_git() {
        let dir = temp_dir("head-gitfile");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        write(&proj.join(".git"), "gitdir: /algum/lugar\n");
        assert_eq!(read_head_branch(&proj), "main");
        assert!(is_git_repo(&proj));
        fs::remove_file(proj.join(".git")).expect("rm");
        assert_eq!(read_head_branch(&proj), "main");
        assert!(!is_git_repo(&proj));
    }

    #[cfg(unix)]
    #[test]
    fn read_head_branch_refuses_symlinked_git_and_head() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("head-symlink");
        let proj = dir.join("p");
        let outside = dir.join("outside");
        write(&outside.join("HEAD"), "ref: refs/heads/develop\n");
        fs::create_dir_all(&proj).expect("p");
        symlink(&outside, proj.join(".git")).expect("ln");
        assert_eq!(read_head_branch(&proj), "main");
        assert!(is_git_repo(&proj));
        fs::remove_file(proj.join(".git")).expect("rm");
        fs::create_dir_all(proj.join(".git")).expect("git dir");
        symlink(outside.join("HEAD"), proj.join(".git/HEAD")).expect("ln head");
        assert_eq!(read_head_branch(&proj), "main");
    }

    #[cfg(unix)]
    #[test]
    fn sdd_initialized_requires_regular_settings() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir("init-state");
        let proj = dir.join("p");
        fs::create_dir_all(proj.join(".sdd")).expect("sdd");
        assert!(!sdd_initialized(&proj), "sem settings");
        fs::create_dir_all(proj.join(".sdd/settings.yaml")).expect("dir");
        assert!(!sdd_initialized(&proj), "settings diretório");
        fs::remove_dir(proj.join(".sdd/settings.yaml")).expect("rm");
        if mkfifo(&proj.join(".sdd/settings.yaml")) {
            assert!(!sdd_initialized(&proj), "settings FIFO");
            fs::remove_file(proj.join(".sdd/settings.yaml")).expect("rm");
        }
        let other = dir.join("outro.yaml");
        fs::write(&other, "x").expect("w");
        symlink(&other, proj.join(".sdd/settings.yaml")).expect("ln");
        assert!(!sdd_initialized(&proj), "settings symlink");
        fs::remove_file(proj.join(".sdd/settings.yaml")).expect("rm");
        fs::write(proj.join(".sdd/settings.yaml"), "x").expect("w");
        assert!(sdd_initialized(&proj), "settings regular");
    }

    #[tokio::test]
    async fn initialized_flag_follows_real_settings_state() {
        let dir = temp_dir("init-flag");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        let state = state_in(&dir, Config::default());
        let path = proj.to_str().expect("u");
        let (status, body) = call(&state, post_project("P", path)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(json_of(&body)["initialized"], true);
        fs::remove_file(proj.join(".sdd/settings.yaml")).expect("rm");
        let (status, body) = call(&state, post_project("P", path)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["initialized"], false);
    }

    #[tokio::test]
    async fn oversized_exclude_is_409_before_any_write() {
        let dir = temp_dir("exclude-big");
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("p");
        git_repo(&proj);
        write(&proj.join(".git/info/exclude"), &"x\n".repeat(512 * 1024 + 1));
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(!proj.join(".sdd").exists(), "nada criado antes da recusa");
        assert!(state.config().read().await.projects.is_empty());
    }

    #[tokio::test]
    async fn settings_directory_is_409_and_not_touched() {
        let dir = temp_dir("settings-dir");
        let proj = dir.join("p");
        write(&proj.join(".sdd/settings.yaml/keep.txt"), "intacto");
        let state = state_in(&dir, Config::default());
        let (status, body) = call(&state, post_project("P", proj.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("arquivo de configuração do projeto inválido"), "{body}");
        assert_eq!(
            fs::read_to_string(proj.join(".sdd/settings.yaml/keep.txt")).expect("r"),
            "intacto"
        );
        assert!(!proj.join(".sdd/specs").exists(), "nada criado");
        assert!(state.config().read().await.projects.is_empty());
    }

    #[tokio::test]
    async fn regular_file_in_path_is_409_never_500() {
        let dir = temp_dir("component-file");
        let state = state_in(&dir, Config::default());

        // `.sdd` é arquivo regular.
        let p1 = dir.join("p1");
        write(&p1.join(".sdd"), "nao sou diretorio");
        let (status, body) = call(&state, post_project("P", p1.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("\"code\":\"project_structure_invalid\""), "{body}");
        assert_eq!(fs::read_to_string(p1.join(".sdd")).expect("r"), "nao sou diretorio");
        assert!(!p1.join(".sdd/settings.yaml").exists());

        // `.git/info` é arquivo regular.
        let p2 = dir.join("p2");
        write(&p2.join(".git/info"), "arquivo");
        let (status, body) = call(&state, post_project("P", p2.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("\"code\":\"project_structure_invalid\""), "{body}");
        assert!(!p2.join(".sdd").exists(), "nada criado");

        // `.sdd/specs` é arquivo regular.
        let p3 = dir.join("p3");
        write(&p3.join(".sdd/specs"), "arquivo");
        let (status, body) = call(&state, post_project("P", p3.to_str().expect("u"))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("\"code\":\"project_structure_invalid\""), "{body}");
        assert!(!p3.join(".sdd/settings.yaml").exists(), "nada criado");

        assert!(state.config().read().await.projects.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn settings_fifo_is_409_without_hanging() {
        let dir = temp_dir("settings-fifo");
        let proj = dir.join("p");
        fs::create_dir_all(proj.join(".sdd")).expect("sdd");
        if !mkfifo(&proj.join(".sdd/settings.yaml")) {
            return; // mkfifo indisponível neste ambiente
        }
        let state = state_in(&dir, Config::default());
        let (status, body) = tokio::time::timeout(
            Duration::from_secs(5),
            call(&state, post_project("P", proj.to_str().expect("u"))),
        )
        .await
        .expect("POST com settings FIFO não pode travar");
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("arquivo de configuração do projeto inválido"), "{body}");
        assert!(!proj.join(".sdd/specs").exists(), "nada criado");
    }
}

#[cfg(test)]
mod pending_tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use std::{env, fs, process};

    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, Response, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::config::{AgentConfig, Config, Connection, Effort};
    use crate::harness::tools::{ApprovalDecision, ApprovalRequest, Approver, Role};
    use crate::server::approvals::{ProjectApprover, MAX_PENDING_PER_PROJECT};
    use crate::server::{router, AppState};

    const HOST: &str = "127.0.0.1:3000";
    const SECRET: &str = "sk-SEGREDO-PENDING-1234";
    const LONG: Duration = Duration::from_secs(60);

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("nants-pending-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn secret_config() -> Config {
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "literal".into(),
            name: "Literal".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        cfg.agents.architect = AgentConfig {
            connection_id: "literal".into(),
            model: "gpt-4o".into(),
            effort: Effort::Low,
            max_steps: None,
        };
        cfg
    }

    fn request(method: Method, uri: &str, body: &str, json_body: bool) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, HOST);
        if json_body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        builder.body(Body::from(body.to_string())).expect("requisição")
    }

    async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Value) {
        let resp: Response<Body> = router(state.clone()).oneshot(req).await.expect("resposta");
        let status = resp.status();
        let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("corpo");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Cria dois projetos reais via POST: devolve (estado, id A, raiz A, id B, raiz B).
    async fn two_projects(tag: &str, cfg: Config) -> (AppState, String, PathBuf, String, PathBuf) {
        let dir = temp_dir(tag);
        let state = AppState::new(dir.join("config.toml"), crate::server::readiness::chat_ready(cfg), 3000);
        let mut made = Vec::new();
        for name in ["a", "b"] {
            let root = dir.join(name);
            fs::create_dir_all(&root).expect("mkdir");
            let body = json!({ "name": format!("Projeto {name}"), "path": root.to_str().expect("u") });
            let (status, created) = call(
                &state,
                request(Method::POST, "/api/projects", &body.to_string(), true),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{created}");
            made.push((created["id"].as_str().expect("id").to_string(), root));
        }
        let (id_b, root_b) = made.pop().expect("b");
        let (id_a, root_a) = made.pop().expect("a");
        (state, id_a, root_a, id_b, root_b)
    }

    async fn get_state(state: &AppState, id: &str) -> Value {
        let uri = format!("/api/projects/{id}/state");
        let (status, body) = call(state, request(Method::GET, &uri, "", false)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// Dispara um pedido HITL do projeto via `ProjectApprover` e espera o `permission_requested`.
    async fn pending_request(
        state: &AppState,
        id: &str,
        root: &Path,
        command: &str,
        timeout: Duration,
    ) -> (String, tokio::task::JoinHandle<ApprovalDecision>) {
        let mut rx = state.events().subscribe();
        let approver = ProjectApprover::new(
            id,
            root.to_path_buf(),
            state.approvals().clone(),
            state.events().clone(),
            state.secret_set().await,
        )
        .with_timeout(timeout);
        let req = ApprovalRequest {
            in_container: false,
            role: Role::Reviewer,
            task_id: Some("T02".into()),
            command: command.into(),
        };
        let handle = tokio::spawn(async move { approver.approve(&req).await });
        let payload = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let ev = rx.recv().await.expect("evento");
                if ev.event_type() == "permission_requested" {
                    return ev.payload;
                }
            }
        })
        .await
        .expect("evento em tempo hábil");
        let request_id = payload["request_id"].as_str().expect("id").to_string();
        (request_id, handle)
    }

    #[tokio::test]
    async fn state_has_empty_pending_permissions_without_requests() {
        let (state, id, _, _, _) = two_projects("pend-empty", Config::default()).await;
        let v = get_state(&state, &id).await;
        assert_eq!(v["pending_permissions"], json!([]));
    }

    #[tokio::test]
    async fn state_lists_pending_of_this_project_only() {
        let (state, id, root, other, other_root) = two_projects("pend-own", Config::default()).await;
        let (req_id, mine) = pending_request(&state, &id, &root, "echo mine", LONG).await;
        let (_, theirs) = pending_request(&state, &other, &other_root, "echo theirs", LONG).await;

        let v = get_state(&state, &id).await;
        let list = v["pending_permissions"].as_array().expect("lista");
        assert_eq!(list.len(), 1, "{v}");
        assert_eq!(list[0]["request_id"], req_id);
        assert_eq!(list[0]["agent"], "reviewer");
        assert_eq!(list[0]["command"], "echo mine");
        assert_eq!(list[0]["task_id"], "T02");
        assert!(!v.to_string().contains("echo theirs"));

        let w = get_state(&state, &other).await;
        assert_eq!(w["pending_permissions"][0]["command"], "echo theirs");
        mine.abort();
        theirs.abort();
    }

    #[tokio::test]
    async fn pending_command_is_masked_and_truncated() {
        let (state, id, root, _, _) = two_projects("pend-scrub", secret_config()).await;
        let long = "b".repeat(3000);
        let command = format!("echo {SECRET} {long}");
        let (_, handle) = pending_request(&state, &id, &root, &command, LONG).await;
        let v = get_state(&state, &id).await;
        let text = v["pending_permissions"][0]["command"].as_str().expect("comando");
        assert!(!v.to_string().contains(SECRET), "segredo vazou: {v}");
        assert!(text.starts_with("echo ***"), "{text}");
        assert!(text.len() <= 2 * 1024, "{}", text.len());
        assert!(text.ends_with("[truncado]"), "{text}");
        handle.abort();
    }

    #[tokio::test]
    async fn pending_disappears_after_resolution_over_http() {
        let (state, id, root, _, _) = two_projects("pend-resolve", Config::default()).await;
        let (req_id, handle) = pending_request(&state, &id, &root, "echo x", LONG).await;
        let before = get_state(&state, &id).await;
        assert_eq!(before["pending_permissions"].as_array().map(Vec::len), Some(1));
        let body = json!({ "decision": "once" }).to_string();
        let uri = format!("/api/projects/{id}/permissions/{req_id}");
        let (status, _) = call(&state, request(Method::POST, &uri, &body, true)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(handle.await.expect("join"), ApprovalDecision::Once);
        assert_eq!(get_state(&state, &id).await["pending_permissions"], json!([]));
    }

    #[tokio::test]
    async fn pending_disappears_after_timeout() {
        let (state, id, root, _, _) = two_projects("pend-timeout", Config::default()).await;
        let (_, handle) = pending_request(&state, &id, &root, "echo t", Duration::from_millis(50)).await;
        assert_eq!(handle.await.expect("join"), ApprovalDecision::Deny);
        assert_eq!(get_state(&state, &id).await["pending_permissions"], json!([]));
    }

    #[tokio::test]
    async fn pending_are_listed_in_creation_order_and_capped() {
        let (state, id, root, _, _) = two_projects("pend-order", Config::default()).await;
        let mut ids = Vec::new();
        let mut handles = Vec::new();
        for i in 0..MAX_PENDING_PER_PROJECT {
            let (req_id, handle) = pending_request(&state, &id, &root, &format!("echo {i}"), LONG).await;
            ids.push(req_id);
            handles.push(handle);
        }
        let v = get_state(&state, &id).await;
        let list = v["pending_permissions"].as_array().expect("lista");
        assert_eq!(list.len(), MAX_PENDING_PER_PROJECT);
        let got: Vec<&str> = list.iter().map(|p| p["request_id"].as_str().expect("id")).collect();
        let want: Vec<&str> = ids.iter().map(String::as_str).collect();
        assert_eq!(got, want);
        for handle in handles {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod settings_tests {
    #[test]
    fn role_guidance_view_lists_roles_and_masks_secrets() {
        let secrets = SecretSet::default();
        assert!(role_guidance_view(&secrets, "# T\n## Premissas\n- a\n").is_empty());
        let body = "## Orientações por papel\n### Revisor\nolhe deps\n";
        let v = role_guidance_view(&secrets, body);
        assert_eq!((v.len(), v[0].role, v[0].text.as_str()), (1, "Revisor", "olhe deps"));
        let keyed = SecretSet::new(["sk-segredo-longo-123".to_string()]);
        let body = "## Orientações por papel\n### Desenvolvedor\nusa sk-segredo-longo-123 no deploy\n";
        let v = role_guidance_view(&keyed, body);
        assert!(!v[0].text.contains("sk-segredo-longo-123"), "{}", v[0].text);
    }

    #[test]
    fn skills_view_reports_status_reason_and_masks_secrets() {
        let secret = "sk-segredo-longo-123";
        let secrets = SecretSet::new([secret.to_string()]);
        let text = format!(
            "---\nrole: developer\nstack: [clojure]\nupdated_at: 2026-10-16\nsource_spec: s1\nreason: stack mudou\n---\n## Papel\n- usa {secret}\n"
        );
        let profile = crate::sdd::skills::parse(&text).expect("perfil");
        let load = |head: Option<&str>| {
            vec![LoadedProfile { role: "developer", profile: profile.clone(), current: text.clone(), head: head.map(str::to_string) }]
        };
        let v = skills_view(&load(Some(&text)), &secrets, "s1");
        assert_eq!((v[0].role, v[0].status, v[0].reason.as_deref()), ("developer", "atualizado", Some("stack mudou")));
        assert!(!v[0].text.contains(secret) && v[0].text.contains("- usa"), "{}", v[0].text);
        assert_eq!(skills_view(&load(Some(&text)), &secrets, "s2")[0].status, "reaproveitado");
        assert_eq!(skills_view(&load(None), &secrets, "s2")[0].status, "criado");
        assert_eq!(skills_view(&load(Some("antigo")), &secrets, "s2")[0].status, "atualizado");
        assert!(serde_json::to_value(&v[0]).expect("json").get("updated_at").is_some());
    }

    #[test]
    fn premises_view_extracts_list_items_of_the_section_only() {
        let secrets = SecretSet::default();
        let body = "# T\n## Premissas\n- SQLite local\n2. Sem login\ntexto\n## Outro\n- fora\n";
        assert_eq!(premises_view(&secrets, body), vec!["SQLite local", "Sem login"]);
        assert!(premises_view(&secrets, "# T\n- x").is_empty());
        let many: String = (0..20).map(|i| format!("- p{i}\n")).collect();
        assert_eq!(premises_view(&secrets, &format!("## Premissas\n{many}")).len(), 12);
        let long = format!("## Premissas\n- {}\n", "a".repeat(500));
        assert_eq!(premises_view(&secrets, &long)[0].chars().count(), 300);
        let keyed = SecretSet::new(["sk-segredo-longo-123".to_string()]);
        let masked = premises_view(&keyed, "## Premissas\n- usa sk-segredo-longo-123 no deploy\n");
        assert!(!masked[0].contains("sk-segredo"), "{masked:?}");
        let crossing = format!("## Premissas\n- {}sk-segredo-longo-123\n", "a".repeat(290));
        let cut = &premises_view(&keyed, &crossing)[0];
        assert!(!cut.contains("sk-") && !cut.contains("segredo"), "{cut}");
    }

    #[test]
    fn criteria_view_reads_items_with_sublists_masks_and_caps() {
        let secrets = SecretSet::default();
        let body = "## Objetivo\nx\n## Critérios de Aceitação\n1. Cria a rota, como em kb:padroes-api-3f9a1c2e#erros.\n2. **Lista:**\n   - um\n   - dois\n## Outro\n- fora\n";
        assert_eq!(
            criteria_view(&secrets, body, "Critérios de Aceitação"),
            vec!["Cria a rota, como em kb:padroes-api-3f9a1c2e#erros.", "**Lista:** um dois"]
        );
        assert!(criteria_view(&secrets, "## Premissas\n- a\n", "Critérios de Aceitação").is_empty());
        let many: String = (0..40).map(|i| format!("- c{i}\n")).collect();
        assert_eq!(criteria_view(&secrets, &format!("## Critérios de Aceitação Globais\n{many}"), "Critérios de Aceitação Globais").len(), 24);
        let long = format!("## Critérios de Aceitação\n- {}\n", "a".repeat(900));
        let cut = &criteria_view(&secrets, &long, "Critérios de Aceitação")[0];
        assert_eq!(cut.chars().count(), 601);
        assert!(cut.ends_with('…'));
        let key = ["sk", "-segredo-longo-123"].concat();
        let keyed = SecretSet::new([key.clone()]);
        let crossing = format!("## Critérios de Aceitação\n- {}{key}\n", "a".repeat(590));
        let masked = &criteria_view(&keyed, &crossing, "Critérios de Aceitação")[0];
        assert!(!masked.contains("segredo"), "{masked}");
    }

    use std::os::unix::fs::symlink;
    use std::process::Command;
    use std::{env, process};

    use axum::body::Body;
    use axum::http::{header, Method, Request, Response};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::*;
    use crate::config::{Config, Connection};
    use crate::server::approvals::persist_always;
    use crate::server::{router, AppState};

    const HOST: &str = "127.0.0.1:3000";
    const CONFIG_MSG: &str = "arquivo de configuração do projeto inválido";
    const BUSY_MSG: &str = "já existe uma execução em andamento neste projeto";

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("nants-settings-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn state_in(dir: &Path) -> AppState {
        AppState::new(dir.join("config.toml"), crate::server::readiness::chat_ready(Config::default()), 3000)
    }

    fn request(method: Method, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("requisição")
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(header::HOST, HOST)
            .body(Body::empty())
            .expect("requisição")
    }

    fn put_settings(id: &str, body: &str) -> Request<Body> {
        request(Method::PUT, &format!("/api/projects/{id}/settings"), body)
    }

    async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, String) {
        let resp: Response<Body> = router(state.clone()).oneshot(req).await.expect("resposta");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("corpo");
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    fn json_of(text: &str) -> Value {
        serde_json::from_str(text).expect("json")
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("pai")).expect("mkdir");
        fs::write(path, content).expect("escrever");
    }

    fn settings_of(root: &Path) -> Settings {
        crate::sdd::read_settings(&root.join(SETTINGS_REL)).expect("settings")
    }

    /// Cria o projeto (POST) em `dir/p` com os arquivos dados; devolve (estado, id, raiz).
    async fn create_project(dir: &Path, files: &[(&str, &str)]) -> (AppState, String, PathBuf) {
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        for (rel, content) in files {
            write(&proj.join(rel), content);
        }
        let state = state_in(dir);
        let body = json!({ "name": "P", "path": proj.to_str().expect("u") }).to_string();
        let (status, text) = call(&state, request(Method::POST, "/api/projects", &body)).await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        let id = json_of(&text)["id"].as_str().expect("id").to_string();
        (state, id, proj)
    }

    async fn project_with(tag: &str, files: &[(&str, &str)]) -> (AppState, String, PathBuf) {
        create_project(&temp_dir(tag), files).await
    }

    #[tokio::test]
    async fn put_saves_trimmed_command_and_returns_state_view() {
        let (state, id, root) = project_with("put-ok", &[]).await;
        let (status, body) = call(&state, put_settings(&id, r#"{"test_command":"  npm run test  "}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        assert_eq!(v["test_command"], "npm run test");
        assert_eq!(v["initialized"], true);
        assert!(v.get("run").is_some() && v.get("specs").is_some(), "{body}");
        assert_eq!(settings_of(&root).test_command, "npm run test");
        let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(json_of(&get)["test_command"], "npm run test");
    }

    #[tokio::test]
    async fn put_limits_validates_range_and_keeps_test_command() {
        let (state, id, root) = project_with("put-limits", &[]).await;
        let (status, _) = call(&state, put_settings(&id, r#"{"test_command":"make"}"#)).await;
        assert_eq!(status, StatusCode::OK);
        for bad in [r#"{"max_attempts":0}"#, r#"{"max_attempts":11}"#, r#"{"max_test_cycles":0}"#, r#"{"max_test_cycles":11}"#] {
            let (status, _) = call(&state, put_settings(&id, bad)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        }
        let (status, _) = call(&state, put_settings(&id, r#"{"max_attempts":8,"max_test_cycles":2}"#)).await;
        assert_eq!(status, StatusCode::OK);
        let s = settings_of(&root);
        assert_eq!((s.max_attempts, s.max_test_cycles, s.test_command.as_str()), (8, 2, "make"));
        let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(json_of(&get)["max_attempts"], 8);
        assert_eq!(json_of(&get)["max_test_cycles"], 2);
    }

    #[tokio::test]
    async fn put_yolo_toggles_setting_and_state_exposes_it() {
        let (state, id, root) = project_with("put-yolo", &[]).await;
        let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(json_of(&get)["yolo"], false);
        assert!(!fs::read_to_string(root.join(SETTINGS_REL)).expect("ler").contains("yolo"));
        let put = |body: &str| request(Method::PUT, &format!("/api/projects/{id}/yolo"), body);
        let (status, body) = call(&state, put(r#"{"enabled":true}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["yolo"], true);
        assert!(settings_of(&root).yolo);
        // Os demais campos seguem intactos e o PUT de settings não derruba o yolo.
        let (status, _) = call(&state, put_settings(&id, r#"{"max_attempts":4}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(settings_of(&root).yolo);
        let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(json_of(&get)["yolo"], true);
        let (status, _) = call(&state, put(r#"{"enabled":false}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!settings_of(&root).yolo);
        assert!(!fs::read_to_string(root.join(SETTINGS_REL)).expect("ler").contains("yolo"));
        for (bad, want) in [("{}", StatusCode::UNPROCESSABLE_ENTITY), (r#"{"enabled":"sim"}"#, StatusCode::UNPROCESSABLE_ENTITY), ("x", StatusCode::BAD_REQUEST)] {
            let (status, _) = call(&state, put(bad)).await;
            assert_eq!(status, want, "{bad}");
        }
        let (status, _) = call(&state, request(Method::PUT, "/api/projects/nao-existe/yolo", r#"{"enabled":true}"#)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn put_internet_toggles_setting_and_state_exposes_it() {
        let (state, id, root) = project_with("put-internet", &[]).await;
        let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(json_of(&get)["architect_internet"], false);
        assert_eq!(json_of(&get)["web_search_available"], false);
        assert!(!fs::read_to_string(root.join(SETTINGS_REL)).expect("ler").contains("architect_internet"));
        let put = |body: &str| request(Method::PUT, &format!("/api/projects/{id}/internet"), body);
        let (status, body) = call(&state, put(r#"{"enabled":true}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["architect_internet"], true);
        assert!(settings_of(&root).architect_internet);
        // Os demais campos seguem intactos, e o PUT de settings e o yolo não derrubam o interruptor.
        let (status, _) = call(&state, put_settings(&id, r#"{"max_attempts":4}"#)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&state, request(Method::PUT, &format!("/api/projects/{id}/yolo"), r#"{"enabled":true}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(settings_of(&root).architect_internet);
        assert_eq!(settings_of(&root).max_attempts, 4);
        let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(json_of(&get)["architect_internet"], true);
        let (status, body) = call(&state, put(r#"{"enabled":false}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_of(&body)["architect_internet"], false);
        assert!(!fs::read_to_string(root.join(SETTINGS_REL)).expect("ler").contains("architect_internet"));
        for (bad, want) in [
            ("{}", StatusCode::UNPROCESSABLE_ENTITY),
            (r#"{"enabled":"sim"}"#, StatusCode::UNPROCESSABLE_ENTITY),
            ("x", StatusCode::BAD_REQUEST),
        ] {
            let (status, _) = call(&state, put(bad)).await;
            assert_eq!(status, want, "{bad}");
        }
        let (status, _) =
            call(&state, request(Method::PUT, "/api/projects/nao-existe/internet", r#"{"enabled":true}"#)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn web_search_available_follows_wire_api_and_recorded_rejection() {
        let (state, id, _root) = project_with("web-available", &[]).await;
        let state_of = |state: AppState, id: String| async move {
            let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
            json_of(&get)["web_search_available"].as_bool().expect("bool")
        };
        {
            let mut cfg = state.config().write().await;
            cfg.connections.push(Connection {
                id: "c-resp".into(),
                name: "R".into(),
                base_url: "https://api.example.com/v1".into(),
                api_key: "k".into(),
                wire_api: crate::config::WireApi::Responses,
                kind: Default::default(),
                base_url_override: None,
            });
            cfg.connections.push(Connection {
                id: "c-chat".into(),
                name: "C".into(),
                base_url: "https://api.example.com/v1".into(),
                api_key: "k".into(),
                wire_api: crate::config::WireApi::Chat,
                kind: Default::default(),
                base_url_override: None,
            });
            cfg.agents.architect.connection_id = "c-chat".into();
        }
        assert!(!state_of(state.clone(), id.clone()).await, "wire_api = chat: sem adaptador");
        state.config().write().await.agents.architect.connection_id = "c-resp".into();
        assert!(state_of(state.clone(), id.clone()).await);
        state.mark_web_search_rejected("c-resp");
        assert!(!state_of(state.clone(), id.clone()).await, "rejeição registrada nesta execução");
        // Spec 132 T05: a Anthropic tem a busca nativa pelo adaptador da Messages API.
        {
            let mut cfg = state.config().write().await;
            cfg.connections.push(Connection::native("c-anth", "A", crate::config::ConnectionKind::Anthropic, "k"));
            cfg.agents.architect.connection_id = "c-anth".into();
        }
        assert!(state_of(state.clone(), id.clone()).await, "Anthropic: busca nativa");
        // Spec 132 T07: o Gemini tem o `google_search` pelo adaptador da Interactions API.
        {
            let mut cfg = state.config().write().await;
            cfg.connections.push(Connection::native("c-gem", "G", crate::config::ConnectionKind::GoogleGemini, "k"));
            cfg.agents.architect.connection_id = "c-gem".into();
        }
        assert!(state_of(state.clone(), id.clone()).await, "Gemini: busca nativa");
        state.mark_web_search_rejected("c-gem");
        assert!(!state_of(state.clone(), id.clone()).await, "Gemini: rejeição registrada");
    }

    #[tokio::test]
    async fn image_generation_available_follows_developer_pair_and_rejection() {
        let (state, id, _root) = project_with("image-available", &[]).await;
        let state_of = |state: AppState, id: String| async move {
            let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
            json_of(&get)["image_generation_available"].as_bool().expect("bool")
        };
        {
            let mut cfg = state.config().write().await;
            for (cid, wire) in [("c-resp", crate::config::WireApi::Responses), ("c-chat", crate::config::WireApi::Chat)] {
                cfg.connections.push(Connection {
                    id: cid.into(),
                    name: cid.into(),
                    base_url: "https://api.example.com/v1".into(),
                    api_key: "k".into(),
                    wire_api: wire,
                    kind: Default::default(),
                    base_url_override: None,
                });
            }
            cfg.agents.developer.connection_id = "c-chat".into();
            cfg.agents.developer.model = "m-img".into();
            // O Arquiteto em responses não conta: vale o par do Desenvolvedor.
            cfg.agents.architect.connection_id = "c-resp".into();
        }
        assert!(!state_of(state.clone(), id.clone()).await, "wire_api = chat: sem geração");
        state.config().write().await.agents.developer.connection_id = "c-resp".into();
        assert!(state_of(state.clone(), id.clone()).await);
        state.mark_image_unsupported("c-resp", "outro-modelo");
        assert!(state_of(state.clone(), id.clone()).await, "a recusa é por modelo");
        state.mark_image_unsupported("c-resp", "m-img");
        assert!(!state_of(state.clone(), id.clone()).await, "recusa registrada");
        state.config().write().await.agents.developer.connection_id = "nao-existe".into();
        assert!(!state_of(state.clone(), id.clone()).await, "conexão inexistente");
    }

    #[tokio::test]
    async fn new_project_inherits_last_yolo_choice_and_defaults_to_false() {
        let (state, id, root) = project_with("yolo-inherit", &[]).await;
        assert!(!settings_of(&root).yolo, "padrão false");
        let create = |dir: &Path| {
            fs::create_dir_all(dir).expect("mkdir");
            let body = json!({ "name": "N", "path": dir.to_str().expect("u") }).to_string();
            request(Method::POST, "/api/projects", &body)
        };
        let base = root.parent().expect("pai").to_path_buf();
        let put = |on: bool| request(Method::PUT, &format!("/api/projects/{id}/yolo"), &format!(r#"{{"enabled":{on}}}"#));
        let (status, _) = call(&state, put(true)).await;
        assert_eq!(status, StatusCode::OK);
        let saved = Config::load_from(state.config_path()).expect("config");
        assert!(saved.last_yolo, "preferência gravada no config global");
        let (status, text) = call(&state, create(&base.join("p2"))).await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        assert!(settings_of(&base.join("p2")).yolo, "herda yolo=true");
        let (_, get) = call(&state, get_req(&format!("/api/projects/{}/state", json_of(&text)["id"].as_str().expect("id")))).await;
        assert_eq!(json_of(&get)["yolo"], true);
        let (status, _) = call(&state, put(false)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!Config::load_from(state.config_path()).expect("config").last_yolo);
        let (status, _) = call(&state, create(&base.join("p3"))).await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(!settings_of(&base.join("p3")).yolo, "herda false");
        assert_eq!(Config::load_from(state.config_path()).expect("config").projects.len(), 3, "outros campos preservados");
    }

    #[tokio::test]
    async fn put_combined_and_out_of_range_yaml_is_normalized() {
        let (state, id, root) = project_with("put-combo", &[]).await;
        let (status, _) = call(
            &state,
            put_settings(&id, r#"{"test_command":"make","max_attempts":3,"max_test_cycles":4}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let s = settings_of(&root);
        assert_eq!((s.max_attempts, s.max_test_cycles, s.test_command.as_str()), (3, 4, "make"));
        let yaml = fs::read_to_string(root.join(SETTINGS_REL)).expect("ler");
        let yaml = yaml.replace("max_attempts: 3", "max_attempts: 20").replace("max_test_cycles: 4", "max_test_cycles: 0");
        fs::write(root.join(SETTINGS_REL), yaml).expect("w");
        let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(json_of(&get)["max_attempts"], 10);
        assert_eq!(json_of(&get)["max_test_cycles"], 1);
        let (status, _) = call(&state, put_settings(&id, r#"{"max_attempts":6}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(settings_of(&root).max_attempts, 6);
    }

    #[tokio::test]
    async fn put_rejects_invalid_commands_without_echo_or_write() {
        let (state, id, root) = project_with("put-invalid", &[]).await;
        let before = fs::read_to_string(root.join(SETTINGS_REL)).expect("ler");
        let long = "a".repeat(201);
        let cases = [
            r#"{"test_command":""}"#.to_string(),
            r#"{"test_command":"   "}"#.to_string(),
            format!(r#"{{"test_command":"{long}"}}"#),
            r#"{"test_command":"SEGREDO-CMD\nsegunda"}"#.to_string(),
            r#"{"test_command":"SEGREDO-CMD\rx"}"#.to_string(),
            r#"{"test_command":"SEGREDO-CMD\u0000x"}"#.to_string(),
            r#"{"test_command":"SEGREDO-CMD\tx"}"#.to_string(),
            "{}".to_string(),
            r#"{"test_command":5}"#.to_string(),
        ];
        for body in cases {
            let (status, text) = call(&state, put_settings(&id, &body)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert!(!text.contains("SEGREDO") && !text.contains("aaaa"), "{text}");
        }
        let (_, empty) = call(&state, put_settings(&id, r#"{"test_command":" "}"#)).await;
        assert_eq!(json_of(&empty)["error"], "test_command é obrigatório");
        assert_eq!(fs::read_to_string(root.join(SETTINGS_REL)).expect("ler"), before);

        let exact = "b".repeat(200);
        let (status, text) = call(&state, put_settings(&id, &format!(r#"{{"test_command":"{exact}"}}"#))).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(settings_of(&root).test_command, exact);
    }

    #[tokio::test]
    async fn put_keeps_other_policy_fields() {
        let (state, id, root) = project_with("put-keep", &[]).await;
        let custom = "base_branch: \"dev\"\nmerge_strategy: \"squash\"\ntest_command: \"make\"\n\
                      allowed_commands:\n  - \"cargo fmt\"\ndenied_commands:\n  - \"rm -rf\"\n";
        write(&root.join(SETTINGS_REL), custom);
        let (status, body) = call(&state, put_settings(&id, r#"{"test_command":"cargo test"}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let s = settings_of(&root);
        assert_eq!(s.test_command, "cargo test");
        assert_eq!(s.base_branch, "dev");
        assert_eq!(s.merge_strategy, "squash");
        assert_eq!(s.allowed_commands, vec!["cargo fmt".to_string()]);
        assert_eq!(s.denied_commands, vec!["rm -rf".to_string()]);
        assert_eq!(json_of(&body)["base_branch"], "dev");
    }

    #[tokio::test]
    async fn put_is_409_while_run_is_active_and_writes_nothing() {
        let (state, id, root) = project_with("put-busy", &[]).await;
        let before = fs::read_to_string(root.join(SETTINGS_REL)).expect("ler");
        let _run = state.runs().try_start(&id, "001-demo").expect("reserva");
        let (status, body) = call(&state, put_settings(&id, r#"{"test_command":"cargo test"}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["error"], BUSY_MSG);
        assert_eq!(fs::read_to_string(root.join(SETTINGS_REL)).expect("ler"), before);
    }

    #[tokio::test]
    async fn put_on_non_regular_settings_is_409_and_untouched() {
        // Diretório no lugar do arquivo.
        let (state, id, root) = project_with("put-dir", &[]).await;
        fs::remove_file(root.join(SETTINGS_REL)).expect("rm");
        fs::create_dir_all(root.join(SETTINGS_REL)).expect("dir");
        let (status, body) = call(&state, put_settings(&id, r#"{"test_command":"cargo test"}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["error"], CONFIG_MSG);
        assert!(root.join(SETTINGS_REL).is_dir());

        // Settings ausente: mesma recusa estática, nada criado.
        let (state, id, root) = project_with("put-missing", &[]).await;
        fs::remove_file(root.join(SETTINGS_REL)).expect("rm");
        let (status, body) = call(&state, put_settings(&id, r#"{"test_command":"cargo test"}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(json_of(&body)["error"], CONFIG_MSG);
        assert!(!root.join(SETTINGS_REL).exists());

        // Symlink: 409 seguro e o alvo externo intacto.
        let dir = temp_dir("put-link");
        let (state, id, root) = create_project(&dir, &[]).await;
        let outside = dir.join("fora.yaml");
        fs::write(&outside, "fora").expect("fora");
        fs::remove_file(root.join(SETTINGS_REL)).expect("rm");
        symlink(&outside, root.join(SETTINGS_REL)).expect("link");
        let (status, body) = call(&state, put_settings(&id, r#"{"test_command":"cargo test"}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(fs::read_to_string(&outside).expect("fora"), "fora");

        // FIFO: recusa sem bloquear.
        let (state, id, root) = project_with("put-fifo", &[]).await;
        fs::remove_file(root.join(SETTINGS_REL)).expect("rm");
        let fifo = root.join(SETTINGS_REL);
        assert!(Command::new("mkfifo").arg(&fifo).status().is_ok_and(|s| s.success()));
        let (status, body) = call(&state, put_settings(&id, r#"{"test_command":"cargo test"}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }

    #[tokio::test]
    async fn put_unknown_project_is_404_and_bad_id_is_422() {
        let (state, _, _) = project_with("put-unknown", &[]).await;
        let (status, _) = call(&state, put_settings("nao-existe", r#"{"test_command":"cargo test"}"#)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&state, put_settings("bad.id", r#"{"test_command":"cargo test"}"#)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn put_and_always_persist_keep_both_writes() {
        for iteration in 0..20 {
            let (state, id, root) = project_with(&format!("put-race-{iteration}"), &[]).await;
            let put_state = state.clone();
            let put_id = id.clone();
            let put = tokio::spawn(async move {
                call(&put_state, put_settings(&put_id, r#"{"test_command":"cargo test"}"#)).await
            });
            let persist_root = root.clone();
            let always = tokio::task::spawn_blocking(move || persist_always(&persist_root, "echo alfa"));
            let (status, body) = put.await.expect("put");
            assert_eq!(status, StatusCode::OK, "{body}");
            assert!(always.await.expect("always"), "iteração {iteration}");
            let s = settings_of(&root);
            assert_eq!(s.test_command, "cargo test", "iteração {iteration}");
            assert_eq!(s.allowed_commands, vec!["echo alfa".to_string()], "iteração {iteration}");
        }
    }

    #[tokio::test]
    async fn test_command_at_limits_round_trips_intact_on_put_get_and_file() {
        for len in [129usize, 200] {
            let (state, id, root) = project_with(&format!("put-len-{len}"), &[]).await;
            let cmd = "c".repeat(len);
            let body = json!({ "test_command": cmd }).to_string();
            let (status, text) = call(&state, put_settings(&id, &body)).await;
            assert_eq!(status, StatusCode::OK, "{text}");
            assert_eq!(json_of(&text)["test_command"], cmd.as_str(), "PUT {len}");
            let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
            assert_eq!(json_of(&get)["test_command"], cmd.as_str(), "GET {len}");
            assert_eq!(settings_of(&root).test_command, cmd, "arquivo {len}");
        }
    }

    #[tokio::test]
    async fn test_command_over_limit_on_disk_is_shown_as_invalid_marker() {
        let (state, id, root) = project_with("get-over", &[]).await;
        let long = "d".repeat(201);
        write(
            &root.join(SETTINGS_REL),
            &format!("base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"{long}\"\n"),
        );
        let (status, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{get}");
        assert_eq!(json_of(&get)["test_command"], "[inválido]");
    }

    #[tokio::test]
    async fn put_refuses_redacted_or_invalid_markers_without_write() {
        let (state, id, root) = project_with("put-markers", &[]).await;
        let before = fs::read_to_string(root.join(SETTINGS_REL)).expect("ler");
        for cmd in ["[inválido]"] {
            let body = json!({ "test_command": cmd }).to_string();
            let (status, text) = call(&state, put_settings(&id, &body)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{cmd}");
            assert_eq!(
                json_of(&text)["error"],
                "test_command contém valor redigido; digite o comando completo",
                "{cmd}"
            );
        }
        assert_eq!(fs::read_to_string(root.join(SETTINGS_REL)).expect("ler"), before);
    }

    /// Projeto com a api_key `test` cadastrada (segredo curto, de 4 caracteres).
    async fn project_with_secret_test(tag: &str, stored: &str) -> (AppState, String, PathBuf) {
        let dir = temp_dir(tag);
        let proj = dir.join("p");
        fs::create_dir_all(&proj).expect("mkdir");
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "c".into(),
            name: "C".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: "test".into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        let state = AppState::new(dir.join("config.toml"), crate::server::readiness::chat_ready(cfg), 3000);
        let body = json!({ "name": "P", "path": proj.to_str().expect("u") }).to_string();
        let (status, text) = call(&state, request(Method::POST, "/api/projects", &body)).await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        let id = json_of(&text)["id"].as_str().expect("id").to_string();
        write(
            &proj.join(SETTINGS_REL),
            &format!("base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"{stored}\"\n"),
        );
        (state, id, proj)
    }

    #[tokio::test]
    async fn echo_of_redacted_view_is_refused_but_typed_text_is_accepted() {
        // (i) api_key "test" e comando "cargo test": a leitura mostra "cargo ***" (redigido).
        let (state, id, root) = project_with_secret_test("put-echo", "cargo test").await;
        let (_, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(json_of(&get)["test_command"], "cargo ***");
        assert_eq!(json_of(&get)["test_command_redacted"], true);

        let (status, text) = call(&state, put_settings(&id, r#"{"test_command":"cargo ***"}"#)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
        assert_eq!(
            json_of(&text)["error"],
            "test_command contém valor redigido; digite o comando completo"
        );
        assert_eq!(settings_of(&root).test_command, "cargo test");

        let (status, text) = call(&state, put_settings(&id, r#"{"test_command":"cargo test"}"#)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(settings_of(&root).test_command, "cargo test");
        assert_eq!(json_of(&text)["test_command"], "cargo ***");
        assert_eq!(json_of(&text)["test_command_redacted"], true);

        // (ii) texto digitado com URL userinfo ou `***` é aceito e gravado como digitado.
        for typed in ["ssh://git@host", "pytest -k \"x***y\"", "npm test"] {
            let body = json!({ "test_command": typed }).to_string();
            let (status, text) = call(&state, put_settings(&id, &body)).await;
            assert_eq!(status, StatusCode::OK, "{typed}: {text}");
            assert_eq!(settings_of(&root).test_command, typed, "{typed}");
        }
    }

    #[tokio::test]
    async fn stored_value_without_secret_is_not_redacted_and_round_trips() {
        let (state, id, root) = project_with("put-plain", &[]).await;
        // Sem segredo, o texto digitado é gravado como veio e a leitura não o marca.
        let body = json!({ "test_command": "pytest -k \"x***y\"" }).to_string();
        let (status, text) = call(&state, put_settings(&id, &body)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(json_of(&text)["test_command"], "pytest -k \"x***y\"");
        assert_eq!(json_of(&text)["test_command_redacted"], false);
        assert_eq!(settings_of(&root).test_command, "pytest -k \"x***y\"");

        // URL com userinfo é redigida pela leitura mesmo sem segredo: a forma exibida é o eco
        // recusado, e o texto digitado original continua aceito e gravado intacto.
        let body = json!({ "test_command": "ssh://git@host" }).to_string();
        let (status, text) = call(&state, put_settings(&id, &body)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(json_of(&text)["test_command"], "ssh://***@host");
        assert_eq!(json_of(&text)["test_command_redacted"], true);
        assert_eq!(settings_of(&root).test_command, "ssh://git@host");
        let echo = json!({ "test_command": "ssh://***@host" }).to_string();
        let (status, _) = call(&state, put_settings(&id, &echo)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn init_leaves_test_command_empty_even_with_manifests() {
        // O comando é decidido pelo Arquiteto na spec; o init não adivinha mais a partir de manifestos.
        let (_, _, root) = project_with(
            "init-sem-sugestao",
            &[("Cargo.toml", "[package]\n"), ("package.json", r#"{"scripts":{"test":"jest"}}"#)],
        )
        .await;
        assert_eq!(settings_of(&root).test_command, "");
    }

    #[tokio::test]
    async fn reregistering_keeps_existing_command_and_get_state_never_writes() {
        let dir = temp_dir("sg-keep");
        let (state, id, root) = create_project(&dir, &[]).await;
        assert_eq!(settings_of(&root).test_command, "");
        write(&root.join("Cargo.toml"), "[package]\n");
        write(&root.join(SETTINGS_REL), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"make\"\n");
        let before = fs::read_to_string(root.join(SETTINGS_REL)).expect("ler");
        let body = json!({ "name": "P", "path": root.to_str().expect("u") }).to_string();
        let (status, text) = call(&state, request(Method::POST, "/api/projects", &body)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(fs::read_to_string(root.join(SETTINGS_REL)).expect("ler"), before);

        // Settings vazio em projeto existente: GET não sugere nem grava.
        write(&root.join(SETTINGS_REL), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"\"\n");
        let before = fs::read_to_string(root.join(SETTINGS_REL)).expect("ler");
        let (status, get) = call(&state, get_req(&format!("/api/projects/{id}/state"))).await;
        assert_eq!(status, StatusCode::OK, "{get}");
        assert_eq!(json_of(&get)["test_command"], "");
        assert_eq!(fs::read_to_string(root.join(SETTINGS_REL)).expect("ler"), before);
    }
}

#[cfg(test)]
mod plan_warning_tests {
    use super::*;

    const SPEC_REL: &str = ".sdd/specs/001-exemplo";

    fn root_with_task(tag: &str, task_body: &str) -> PathBuf {
        root_with(tag, "run_command: \"cargo run -- exemplo\"\n", task_body)
    }

    /// Spec de exemplo com `extra` no frontmatter e a tarefa T01 com `task_body` nos critérios.
    fn root_with(tag: &str, extra: &str, task_body: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("nants-plan-warnings-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(SPEC_REL).join("tasks")).expect("mkdir");
        let spec = crate::agents::prompts::EXAMPLE_SPEC_MD.replace("created_at", &format!("{extra}created_at"));
        fs::write(root.join(SPEC_REL).join("spec.md"), spec).expect("spec");
        let task = crate::agents::prompts::EXAMPLE_TASK_MD.replace("- [ ] Critério verificável.", task_body);
        fs::write(root.join(SPEC_REL).join("tasks/T01.md"), task).expect("task");
        root
    }

    #[test]
    fn plan_check_uses_filtered_and_grouped_warnings() {
        // Spec 094 T02: a UI e a autocorreção do Arquiteto (092 T01) recebem os avisos já filtrados.
        let root = root_with_task(
            "filtered",
            "- [ ] Mapas com extensão `.svg`; `.gitignore` ignora `node_modules/` e `dist/`.\n- [ ] `deps.edn` lista a dependência.",
        );
        let expected = vec![
            "T01: critério cita `.gitignore`, fora do escopo".to_string(),
            "T01: critério cita `deps.edn`, fora do escopo".to_string(),
        ];
        assert_eq!(plan_scope_warnings(&root, SPEC_REL), expected);
        assert_eq!(changed_plan_warnings(&root, &BTreeMap::new(), &SecretSet::default()), expected);
        let _ = fs::remove_dir_all(&root);

        let root = root_with_task("grouped", "- [ ] `a.md`, `b.md`, `docs/c.md` e `docs/d.md`.");
        let line = "T01: 4 caminhos citados fora do escopo (`a.md`, `b.md`, `docs/c.md` e mais 1)".to_string();
        assert_eq!(plan_scope_warnings(&root, SPEC_REL), vec![line.clone()]);
        assert_eq!(changed_plan_warnings(&root, &BTreeMap::new(), &SecretSet::default()), vec![line]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Spec 131 T04: sem `run_command`, o aviso do plano pergunta como o usuário vê o produto
    /// funcionando, no mesmo turno (a conferência do plano), sem rodada de consenso.
    #[test]
    fn plan_without_run_command_asks_how_the_user_sees_the_product() {
        let root = root_with("no-run", "", "- [ ] Os testes passam.");
        let warnings = changed_plan_warnings(&root, &BTreeMap::new(), &SecretSet::default());
        assert_eq!(warnings, vec![RUN_COMMAND_MISSING_WARNING.to_string()]);
        assert!(RUN_COMMAND_MISSING_WARNING.contains("como o usuário vê o produto funcionando"));
        assert!(RUN_COMMAND_MISSING_WARNING.contains("`nothing_to_run`"));
        // Vazio equivale a ausente.
        let root2 = root_with("empty-run", "run_command: \"  \"\n", "- [ ] Os testes passam.");
        assert_eq!(changed_plan_warnings(&root2, &BTreeMap::new(), &SecretSet::default()), warnings);
        // Spec fora do plano não recebe aviso.
        let done = fs::read_to_string(root.join(SPEC_REL).join("spec.md")).expect("spec").replace("AWAITING_APPROVAL", "COMPLETED");
        fs::write(root.join(SPEC_REL).join("spec.md"), done).expect("spec");
        assert!(changed_plan_warnings(&root, &BTreeMap::new(), &SecretSet::default()).is_empty());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&root2);
    }

    /// Spec 131 T04: a decisão "nada a executar" registrada no frontmatter cala o aviso.
    #[test]
    fn nothing_to_run_decision_silences_the_run_command_warning() {
        let extra = "nothing_to_run: \"Biblioteca sem executável: o uso é pelos testes.\"\n";
        let root = root_with("nothing", extra, "- [ ] Os testes passam.");
        assert!(changed_plan_warnings(&root, &BTreeMap::new(), &SecretSet::default()).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    /// Spec 131 T04: critério (da tarefa ou global) que cita um campo da spec vazio ou inexistente
    /// vira aviso do plano; citado e declarado, não.
    #[test]
    fn criterion_citing_an_empty_spec_field_is_a_plan_warning() {
        let extra = "nothing_to_run: \"Sem servidor.\"\nrun_url: \"\"\n";
        let root = root_with(
            "cites",
            extra,
            "- [ ] The spec run_command imports the checking example.\n- [ ] O `run_url` abre a tela.\n- [ ] O test_command passa.",
        );
        let spec_path = root.join(SPEC_REL).join("spec.md");
        let spec = fs::read_to_string(&spec_path).expect("spec").replace("- [ ] Critério verificável.", "- [ ] O `install_command` instala tudo.");
        fs::write(&spec_path, spec).expect("spec");
        let warnings = changed_plan_warnings(&root, &BTreeMap::new(), &SecretSet::default());
        let expected = vec![
            "G1: o critério \"O `install_command` instala tudo.\" cita `install_command`, que a spec não declara: declare o \
             campo ou reescreva o critério para citar só o que existe"
                .to_string(),
            "T01: o critério \"The spec run_command imports the checking example.\" cita `run_command`, que a spec não \
             declara: declare o campo ou reescreva o critério para citar só o que existe"
                .to_string(),
            "T01: o critério \"O `run_url` abre a tela.\" cita `run_url`, que a spec não declara: declare o campo ou \
             reescreva o critério para citar só o que existe"
                .to_string(),
        ];
        assert_eq!(warnings, expected);
        // O settings com instalação supre o campo herdável; o run_command não tem herança.
        fs::write(
            root.join(SETTINGS_REL),
            "base_branch: main\nmerge_strategy: squash\ntest_command: \"make check\"\ninstall_command: \"make deps\"\n",
        )
        .expect("settings");
        let warnings = changed_plan_warnings(&root, &BTreeMap::new(), &SecretSet::default());
        assert_eq!(warnings, expected[1..].to_vec());

        // Segredo que atravessa o corte do trecho (60 caracteres): redigido antes do corte, nenhum
        // prefixo dele chega ao Arquiteto.
        let secret = "valorconfidencialqwertyuiop";
        let head = "The spec run_command shows the report using the value ";
        assert!(head.chars().count() < 60 && head.chars().count() + secret.len() > 60);
        let task = crate::agents::prompts::EXAMPLE_TASK_MD.replace("- [ ] Critério verificável.", &format!("- [ ] {head}{secret}."));
        fs::write(root.join(SPEC_REL).join("tasks/T01.md"), task).expect("task");
        let secrets = SecretSet::new([secret.to_string()]);
        let warnings = changed_plan_warnings(&root, &BTreeMap::new(), &secrets);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("cita `run_command`"), "{warnings:?}");
        assert!(!warnings[0].contains("valorc"), "{warnings:?}");
        let _ = fs::remove_dir_all(&root);
    }
}
