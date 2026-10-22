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
//!    `.sdd/specs/<spec>/plan-review.json` (`{signature, at, status, issues}`; escrita atômica, sem
//!    seguir symlink, local via `.git/info/exclude`);
//! 4. se o plano mudou durante a revisão, revisa de novo a versão nova (uma revisão por vez);
//! 5. ao concluir (com ou sem gravação; só o shutdown não publica), publica `sdd_draft_updated`
//!    para a UI recarregar e sair de "revisando…".
//!
//! JSON inválido, falha do provedor ou tempo esgotado viram `status: "unavailable"`. O estado do
//! projeto expõe `plan_review` só quando a assinatura gravada bate com o conteúdo atual, ou
//! `status: "running"` enquanto a revisão roda. Com o Revisor configurado e sem resultado nem
//! revisão em curso da versão atual (spec 092 T02), expõe `pending` até [`PENDING_WINDOW`] depois
//! da última alteração do plano, e `stale` depois disso (a UI libera a aprovação com aviso).

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use super::approvals::ProjectApprover;
use super::chat::load_policy;
use super::events::{ApiEvent, BusActivitySink, EventType, SecretSet};
use super::projects::{ensure_local_excludes, format_utc, real_children, scrub_text};
use super::report::{exclude_guaranteed, wait_shutdown};
use super::safe_fs::{self, MAX_READ_BYTES, O_NOFOLLOW};
use super::spec_result::validate_spec_id;
use super::state::AppState;
use crate::agents::architect::truncate_bytes;
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::LlmClient;
use crate::agents::plan_review::PlanReviewer;
use crate::config::Effort;
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
pub const ISSUE_KINDS: [&str; 5] = ["contradiction", "untestable", "scope", "uncovered_criterion", "missing_dependency"];

pub const STATUS_OK: &str = "ok";
pub const STATUS_UNAVAILABLE: &str = "unavailable";
pub const STATUS_RUNNING: &str = "running";
/// Versão atual sem resultado e sem revisão em curso, alterada há menos de [`PENDING_WINDOW`].
pub const STATUS_PENDING: &str = "pending";
/// Versão atual sem resultado depois de [`PENDING_WINDOW`]: a aprovação libera com aviso.
pub const STATUS_STALE: &str = "stale";
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
const MAX_RECORD_BYTES: u64 = 64 * 1024;
const MAX_TASK_ID_BYTES: usize = 16;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Revisões em curso, por `<raiz>\0<spec>`. Uma por spec; a vaga sai no `Drop` do [`Claim`].
static RUNNING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn running_key(root: &Path, spec_id: &str) -> String {
    format!("{}\0{spec_id}", root.display())
}

/// Vaga de revisão em curso de uma spec (liberada no `Drop`, inclusive em pânico).
struct Claim(String);

impl Claim {
    fn take(root: &Path, spec_id: &str) -> Option<Self> {
        let key = running_key(root, spec_id);
        let mut guard = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
        if !guard.get_or_insert_with(HashSet::new).insert(key.clone()) {
            return None;
        }
        Some(Self(key))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut guard = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(set) = guard.as_mut() {
            set.remove(&self.0);
        }
    }
}

fn is_running(root: &Path, spec_id: &str) -> bool {
    let guard = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    guard.as_ref().is_some_and(|set| set.contains(&running_key(root, spec_id)))
}

/// Apontamento da revisão do plano.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanIssue {
    /// Tarefa apontada (`T02`), ou `None` quando o problema é da spec.
    pub task: Option<String>,
    pub kind: String,
    pub text: String,
}

/// Conteúdo de `plan-review.json`.
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
}

/// `plan_review` exposto no estado do projeto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanReviewView {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub issues: Vec<PlanIssue>,
}

/// Configuração do Revisor para a revisão do plano. `None` sem o papel configurado.
struct ReviewerSetup {
    model: String,
    effort: Effort,
    backend: Arc<dyn ChatBackend>,
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
    let client = LlmClient::from_connection(&connection).ok()?;
    let backend: Arc<dyn ChatBackend> = match state.report_backend() {
        Some(backend) => backend,
        None => Arc::new(client),
    };
    Some(ReviewerSetup { model: agent.model, effort: agent.effort, backend })
}

/// `true` quando o papel `reviewer` está configurado (a revisão do plano vai rodar).
pub async fn reviewer_configured(state: &AppState) -> bool {
    reviewer_setup(state).await.is_some()
}

/// Gatilho depois de um turno do chat que alterou artefatos. A parte síncrona é curta (lê as
/// specs e reserva a vaga); a revisão roda em segundo plano. Devolve as tasks disparadas (os
/// testes as aguardam; o chat as ignora).
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
                let outcome = review_loop(&state, &project_id, &root, &spec_id, &setup).await;
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
    /// Shutdown do servidor: nada gravado e nada publicado.
    Shutdown,
}

/// Revisa enquanto a versão atual do plano não tem resultado.
async fn review_loop(state: &AppState, project_id: &str, root: &Path, spec_id: &str, setup: &ReviewerSetup) -> Stop {
    loop {
        let (check_root, sid) = (root.to_path_buf(), spec_id.to_string());
        let pending = tokio::task::spawn_blocking(move || {
            awaiting(&check_root, &sid) && needs_review(&check_root, &sid)
        })
        .await
        .unwrap_or(false);
        if !pending {
            return Stop::Done;
        }
        if let Err(stop) = review_once(state, project_id, root, spec_id, setup).await {
            return stop;
        }
    }
}

/// Uma revisão da versão atual: grava o resultado. `Err` em shutdown, plano ilegível ou falha de
/// escrita (nada gravado).
async fn review_once(state: &AppState, project_id: &str, root: &Path, spec_id: &str, setup: &ReviewerSetup) -> Result<(), Stop> {
    let secrets = state.secret_set().await;
    let (plan_root, sid, plan_secrets) = (root.to_path_buf(), spec_id.to_string(), Arc::clone(&secrets));
    let (signature, plan) = tokio::task::spawn_blocking(move || {
        Some((plan_signature(&plan_root, &sid)?, build_plan(&plan_root, &sid, &plan_secrets)?))
    })
    .await
    .ok()
    .flatten()
    .ok_or(Stop::Failed)?;

    let policy = load_policy(root.to_path_buf()).await;
    let approver = Arc::new(ProjectApprover::new(
        project_id,
        root.to_path_buf(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::clone(&secrets),
    ));
    let activity = Arc::new(
        BusActivitySink::new(state.events().clone(), project_id, Arc::clone(&secrets)).with_stage("plan_review", PLAN_REVIEW_STAGE),
    );
    let reviewer = PlanReviewer::new(Arc::clone(&setup.backend), setup.model.clone(), Some(setup.effort), policy, approver)
        .with_activity(activity, Some(spec_id.to_string()))
        .with_usage(super::usage::FileUsageSink::new(root.to_path_buf(), project_id))
        .with_lang(state.project_lang(project_id));
    let mut shutdown = state.shutdown_signal();
    let turn = reviewer.review(root, &plan);
    let answer = tokio::select! {
        biased;
        _ = wait_shutdown(&mut shutdown) => return Err(Stop::Shutdown),
        joined = tokio::time::timeout(state.report_timeout(), turn) => match joined {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(failure)) => Err(failure.describe()),
            Err(_) => Err(DETAIL_TIMEOUT.to_string()),
        },
    };
    let issues = answer.as_deref().ok().and_then(|raw| parse_issues(raw, &secrets));
    let detail = match (&issues, answer) {
        (Some(_), _) => None,
        (None, Ok(_)) => Some(DETAIL_INVALID_JSON.to_string()),
        (None, Err(why)) => Some(why),
    };
    let record = PlanReviewRecord {
        signature,
        at: format_utc(unix_now()),
        status: if issues.is_some() { STATUS_OK } else { STATUS_UNAVAILABLE }.to_string(),
        issues: issues.unwrap_or_default(),
        detail,
    };
    let (write_root, sid) = (root.to_path_buf(), spec_id.to_string());
    tokio::task::spawn_blocking(move || persist(&write_root, &sid, &record)).await.ok().flatten().ok_or(Stop::Failed)
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
    if !record_path_safe(root, spec_id) {
        return false;
    }
    match plan_signature(root, spec_id) {
        Some(sig) => read_record(root, spec_id).is_none_or(|r| r.signature != sig),
        None => false,
    }
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
    Some(out)
}

/// Valida a resposta do modelo: o objeto JSON (entre o primeiro `{` e o último `}`) com `issues`
/// em lista. Itens com `kind` desconhecido ou sem texto são descartados; texto redigido, numa linha
/// e cortado em [`MAX_ISSUE_TEXT_BYTES`]; no máximo [`MAX_ISSUES`]. `None` = revisão indisponível
/// (JSON inválido, sem `issues`, ou só itens inválidos).
pub fn parse_issues(raw: &str, secrets: &SecretSet) -> Option<Vec<PlanIssue>> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end < start {
        return None;
    }
    let value: Value = serde_json::from_str(&raw[start..=end]).ok()?;
    let list = value.get("issues")?.as_array()?;
    let issues: Vec<PlanIssue> = list.iter().filter_map(|item| issue_of(item, secrets)).take(MAX_ISSUES).collect();
    if issues.is_empty() && !list.is_empty() {
        return None;
    }
    Some(issues)
}

fn issue_of(item: &Value, secrets: &SecretSet) -> Option<PlanIssue> {
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
    Some(PlanIssue { task, kind: kind.to_string(), text })
}

/// `T` seguido de letras/dígitos, curto (ex.: `T02`).
fn valid_task_id(id: &str) -> bool {
    id.len() >= 2
        && id.len() <= MAX_TASK_ID_BYTES
        && id.starts_with('T')
        && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Redige, junta em uma linha e corta (nessa ordem: segredo nunca vaza pelo corte).
fn clean_text(secrets: &SecretSet, text: &str) -> String {
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

/// `plan_review` de uma spec aguardando aprovação: `running` enquanto a revisão roda; senão o
/// resultado gravado, só se a assinatura bate com o conteúdo atual (redigido de novo na leitura).
pub fn view(root: &Path, spec_id: &str, secrets: &SecretSet) -> Option<PlanReviewView> {
    if is_running(root, spec_id) {
        return Some(PlanReviewView { status: STATUS_RUNNING, at: None, issues: Vec::new() });
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
    let issues = record
        .issues
        .iter()
        .filter(|i| ISSUE_KINDS.contains(&i.kind.as_str()))
        .take(MAX_ISSUES)
        .map(|i| PlanIssue {
            task: i.task.as_deref().filter(|t| valid_task_id(t)).map(str::to_string),
            kind: i.kind.clone(),
            text: clean_text(secrets, &i.text),
        })
        .collect();
    Some(PlanReviewView { status, at: Some(scrub_text(secrets, &record.at)), issues })
}

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
    Some(PlanReviewView { status, at: None, issues: Vec::new() })
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
