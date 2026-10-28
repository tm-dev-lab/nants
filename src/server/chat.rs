//! `POST /api/projects/:id/chat`: um turno de refinamento com o Arquiteto.
//!
//! Fluxo: validar id e corpo; checar o projeto; reservar o turno do projeto (um por vez,
//! 409 se ocupado); ler a configuração do Arquiteto (clone curto, sem I/O sob lock); rodar
//! [`LlmArchitect::reply`] com timeout total; publicar `agent_message` e, se houve artefatos,
//! `sdd_draft_updated`; persistir os dois turnos em `.sdd/specs/.chat.jsonl`.
//!
//! Respostas e erros nunca ecoam a mensagem de entrada nem texto de transporte ou do LLM.
//! O histórico é gravado redigido (substituição exata de segredos e URLs com userinfo).

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map};

use super::approvals::{random_hex_id, ProjectApprover};
use super::error::ApiError;
use super::events::{validate_project_id, ApiEvent, EventType, SecretSet, MAX_MESSAGE_BYTES};
use super::projects::scrub_text;
use super::safe_fs::{self, MAX_READ_BYTES, O_NOFOLLOW, O_NONBLOCK};
use super::snapshot::project_snapshot;
use super::state::{AppState, ArchitectTurn, ReserveError, REPORT_BUSY_CODE};
use crate::server::events::BusActivitySink;
use crate::agents::activity::{ActivityEvent, ActivityKind, ActivitySink};
use crate::agents::llm_client::{BadKind, ChatRequest, ChatResponse, ProviderFailure, StreamDelta};
use crate::agents::LlmError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Instant;
use crate::agents::architect::{
    truncate_bytes, ArchitectError, ArchitectReply, ChatTurn, InternetSetup, LlmArchitect, TurnNotice, TurnRole,
    CHAT_FILE_REL, MAX_HISTORY_TURNS, SDD_SPECS_REL,
};
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::LlmClient;
use super::chat_attachments::{attachment_exists, attachment_mime, MAX_MESSAGE_ATTACHMENTS};
use crate::config::{Effort, Project};
use crate::harness::allowlist::Policy;
use crate::sdd::{read_settings, sdd_dir};

/// Prazo de inatividade de um turno do Arquiteto (180 s), renovado a cada progresso.
pub const CHAT_TIMEOUT: Duration = Duration::from_secs(180);
/// Teto absoluto de um turno do Arquiteto (15 min), mesmo com progresso contínuo.
pub const CHAT_MAX_TURN: Duration = Duration::from_secs(15 * 60);
/// Limite de caracteres da mensagem do usuário.
pub const MAX_CHAT_MESSAGE_CHARS: usize = 8000;
/// Bytes lidos do fim do histórico (1 MiB).
const HISTORY_READ_BYTES: u64 = MAX_READ_BYTES;

const TIMEOUT_CODE: &str = "chat_timeout";
const TIMEOUT_TURN_TEXT: &str = "O turno foi cortado por inatividade; envie a mensagem novamente.";
const TIMEOUT_TURN_SAVED: &str =
    "O turno foi cortado por demora, mas o plano foi salvo; veja o cartão do plano.";
const ARCHITECT_AGENT: &str = "architect";

#[derive(Deserialize)]
struct ChatInput {
    message: String,
    /// Turno interno do sistema: persistido como `internal` e omitido do GET. Ausente = `false`.
    #[serde(default)]
    internal: bool,
    /// Imagens já enviadas por `POST /chat/attachments` (spec 082 T05), até 4.
    #[serde(default)]
    attachments: Vec<String>,
}

/// Mensagem validada do usuário (ou do sistema, quando `internal`).
struct Incoming {
    message: String,
    internal: bool,
    attachments: Vec<String>,
    /// Turno interno de uma rodada do consenso do plano (spec 128 T02): o uso vai com a etapa
    /// `plan_consensus` e a resposta do Arquiteto fica fora da Conversa, salvo uma pergunta ao
    /// usuário. Nunca vem do corpo do pedido.
    consensus_round: Option<u32>,
    /// Marca da mensagem do usuário em andamento (spec 128 T02): o ciclo de consenso não revisa
    /// versões pela metade nem abre rodada enquanto ela não termina.
    user_mark: Option<String>,
}

/// O que o turno deixa para quem o pediu, na extensão da resposta (spec 128 T02): o corpo do
/// bloco `nants-plan-decisions` (fora do texto mostrado), se a resposta pergunta algo ao usuário e
/// o texto final mostrado (spec 131 T03: o pedido curto do bloco leva esse texto).
#[derive(Debug, Clone, Default)]
struct TurnOutcome {
    decisions: Option<String>,
    asks_user: bool,
    reply: String,
}

/// Resultado de um turno interno do consenso.
pub(super) struct ConsensusTurnOutcome {
    pub status: StatusCode,
    /// Corpo do último bloco das decisões entre as mensagens do assistente no turno (spec 131 T03).
    pub decisions: Option<String>,
    pub asks_user: bool,
    /// Texto final mostrado do turno, redigido e sem o bloco.
    pub reply: String,
}

/// Esperas pela reserva do turno ocupado: o turno interno do consenso logo depois do turno do
/// usuário que terminava, e a mensagem do usuário durante o turno interno (C-11).
const BUSY_POLL: Duration = Duration::from_millis(250);
const CONSENSUS_BUSY_TRIES: u32 = 20;

/// Texto de uma mensagem só com imagem (sem texto).
const IMAGE_ONLY_TEXT: &str = "(imagem)";

/// Prazos do turno: inatividade (renovado a cada progresso) e teto absoluto.
#[derive(Clone, Copy)]
struct TurnLimits {
    idle: Duration,
    cap: Duration,
}

impl From<Duration> for TurnLimits {
    fn from(idle: Duration) -> Self {
        Self { idle, cap: CHAT_MAX_TURN }
    }
}

/// Instante do último progresso observável do Arquiteto.
struct Progress(Mutex<Instant>);

impl Progress {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Instant::now())))
    }
    fn touch(&self) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }
    fn last(&self) -> Instant {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Backend que registra progresso a cada resposta (ou delta) do modelo.
struct ProgressBackend {
    inner: Arc<dyn ChatBackend>,
    progress: Arc<Progress>,
}

impl ChatBackend for ProgressBackend {
    fn complete<'a>(
        &'a self,
        req: &'a ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let out = self.inner.complete(req).await;
            self.progress.touch();
            out
        })
    }

    fn complete_stream<'a>(
        &'a self,
        req: &'a ChatRequest,
        on_delta: &'a (dyn Fn(StreamDelta) + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let progress = &self.progress;
            // O tamanho recebido (spec 114 T04) só alimenta a Atividade: o prazo do turno segue
            // renovado pelos mesmos sinais de antes.
            let wrapped = move |delta: StreamDelta| {
                if !matches!(delta, StreamDelta::Received(_) | StreamDelta::Cut(_)) {
                    progress.touch();
                }
                on_delta(delta);
            };
            let out = self.inner.complete_stream(req, &wrapped).await;
            self.progress.touch();
            out
        })
    }
}

/// Sink de atividade que também registra progresso (chamadas de ferramenta, comandos).
struct ProgressSink {
    inner: Arc<dyn ActivitySink>,
    progress: Arc<Progress>,
}

impl ActivitySink for ProgressSink {
    fn emit(&self, event: ActivityEvent) {
        // Spec 114 T04: o acompanhamento da chamada ao modelo (início, sinal periódico e fim) só
        // informa a Atividade. Uma chamada muda não pode renovar o prazo de inatividade do turno;
        // o fim da chamada já renova pelo `ProgressBackend`.
        if !matches!(event.kind, ActivityKind::Model | ActivityKind::ModelBeat | ActivityKind::ModelEnd) {
            self.progress.touch();
        }
        self.inner.emit(event);
    }
    fn sanitize(&self, text: &str) -> String {
        self.inner.sanitize(text)
    }
}

/// Espera `fut` com prazo de inatividade e teto absoluto. `None` = expirou.
async fn with_idle_deadline<T>(
    fut: impl Future<Output = T>,
    limits: TurnLimits,
    progress: &Progress,
) -> Option<T> {
    progress.touch();
    let hard = tokio::time::Instant::now() + limits.cap;
    tokio::pin!(fut);
    loop {
        let seen = progress.last();
        let idle_at = tokio::time::Instant::from_std(seen + limits.idle);
        let at = idle_at.min(hard);
        tokio::select! {
            out = &mut fut => return Some(out),
            () = tokio::time::sleep_until(at) => {
                if tokio::time::Instant::now() >= hard || progress.last() == seen {
                    return None;
                }
            }
        }
    }
}

/// Marca dos `.md` em `.sdd/specs` (contagem, mtime mais recente): detecta gravação no turno.
fn specs_mark(root: &Path) -> (usize, Option<SystemTime>) {
    fn walk(dir: &Path, depth: usize, acc: &mut (usize, Option<SystemTime>)) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(meta) = path.symlink_metadata() else { continue };
            if meta.is_dir() && depth < 4 {
                walk(&path, depth + 1, acc);
            } else if meta.is_file() && path.extension().is_some_and(|e| e == "md") {
                acc.0 += 1;
                let m = meta.modified().ok();
                if m > acc.1 {
                    acc.1 = m;
                }
            }
        }
    }
    let mut acc = (0, None);
    walk(&root.join(SDD_SPECS_REL), 0, &mut acc);
    acc
}

/// `POST /api/projects/:id/chat` com corpo `{"message": "..."}`.
pub async fn post_chat(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    run_turn(&state, &id, &body, CHAT_TIMEOUT, state.llm_backend()).await
}

/// Turno interno de uma rodada do consenso do plano (spec 128 T02). Passa pelo mesmo caminho do
/// `POST /chat` com `internal: true` (reserva do turno, prontidão, persistência e eventos). Com o
/// turno ocupado logo ao fim de um turno do usuário (que ainda solta a reserva), tenta de novo por
/// alguns segundos, enquanto `gen` (as mensagens do usuário) não muda. Futuro em caixa: o turno
/// dispara a revisão do plano, que pede este turno (quebra o ciclo de tipos).
pub(super) fn consensus_turn(
    state: AppState,
    project_id: String,
    message: String,
    round: u32,
    gen: u64,
) -> Pin<Box<dyn Future<Output = ConsensusTurnOutcome> + Send>> {
    Box::pin(async move {
        let _running = state.plan_consensus().begin_consensus_turn(&project_id);
        let mut tries = 0;
        loop {
            let incoming = Incoming { message: message.clone(), internal: true, attachments: Vec::new(), consensus_round: Some(round), user_mark: None };
            let resp = match run_incoming(&state, &project_id, incoming, CHAT_TIMEOUT.into(), state.llm_backend()).await {
                Ok(resp) => resp,
                Err(err) => err.into_response(),
            };
            let status = resp.status();
            let consensus = state.plan_consensus();
            if status == StatusCode::CONFLICT
                && tries < CONSENSUS_BUSY_TRIES
                && consensus.user_gen(&project_id) == gen
                && !consensus.user_turn_active(&project_id)
            {
                tries += 1;
                tokio::time::sleep(BUSY_POLL).await;
                continue;
            }
            let outcome = resp.extensions().get::<TurnOutcome>().cloned().unwrap_or_default();
            return ConsensusTurnOutcome { status, decisions: outcome.decisions, asks_user: outcome.asks_user, reply: outcome.reply };
        }
    })
}

/// Turno interno "continue de onde parou", com a resposta inteira (o `POST /chat/continue` a
/// devolve à tela).
async fn internal_turn_response(state: &AppState, project_id: &str, message: String) -> Result<Response, ApiError> {
    let body = json!({ "message": message, "internal": true }).to_string();
    run_turn(state, project_id, body.as_bytes(), CHAT_TIMEOUT, state.llm_backend()).await
}

/// Turno interno do Arquiteto pedido por outra tela (spec 130 T04: "Pedir ao Arquiteto para
/// corrigir" da janela de execução). Mesmo caminho do `POST /chat` com `internal: true` (reserva do
/// turno, prontidão, ferramentas e aprovações de sempre); a mensagem não aparece no histórico.
pub(super) async fn internal_turn(state: &AppState, project_id: &str, message: String) -> Result<Response, ApiError> {
    internal_turn_response(state, project_id, message).await
}

/// Mensagem do sistema na Conversa (spec 128 T02: a linha de cada rodada do consenso): gravada no
/// histórico com `code` e `params` (o texto de reserva no idioma do projeto, pelo dicionário) e
/// publicada como `agent_message` do sistema.
pub(super) async fn system_notice(state: &AppState, project_id: &str, root: PathBuf, code: &str, params: Map<String, serde_json::Value>) {
    system_notice_with_tail(state, project_id, root, code, params, None).await;
}

/// [`system_notice`] com um conteúdo longo depois da frase fixa (spec 131 T03: a lista final das
/// tarefas): ele não cabe num param (cortado em 512 bytes) e vai no texto, depois da primeira
/// quebra de linha, como as mensagens com cauda da web (`SYSTEM_CODES_WITH_TAIL`).
pub(super) async fn system_notice_with_tail(
    state: &AppState,
    project_id: &str,
    root: PathBuf,
    code: &str,
    params: Map<String, serde_json::Value>,
    tail: Option<&str>,
) {
    let lang = state.project_lang(project_id).get();
    let secrets = state.secret_set().await;
    let pairs: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_string)))
        .collect();
    let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let fixed = crate::i18n::tr(lang, &format!("system.{code}"), &refs);
    let text = match tail.map(str::trim).filter(|t| !t.is_empty()) {
        Some(tail) => output_text(&secrets, &format!("{fixed}\n{tail}")),
        None => output_text(&secrets, &fixed),
    };
    let notice = TurnNotice { code: code.to_string(), params: params.clone() };
    let turn = ChatTurn {
        role: TurnRole::System,
        text: text.clone(),
        ts: unix_now(),
        turn_id: None,
        internal: false,
        attachments: Vec::new(),
        notice: Some(notice),
    };
    let _saved = tokio::task::spawn_blocking(move || append_turns(&root, &[turn])).await;
    state.events().publish(ApiEvent::new(
        EventType::AgentMessage,
        Some(project_id.to_string()),
        json!({
            "agent": "system",
            "text": text,
            "code": code,
            "params": super::events::message_params(&secrets, &params),
        }),
    ));
}

/// `GET /api/projects/:id/chat`: últimos turnos do histórico, em ordem cronológica, com texto
/// redigido. Sem efeitos colaterais; arquivo ausente ou inseguro vira lista vazia.
pub async fn get_chat(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let project = find_project(&state, &id).await?;
    let secrets = state.secret_set().await;
    let turns = load_visible_history(PathBuf::from(&project.path)).await;
    let turns: Vec<serde_json::Value> = turns
        .into_iter()
        .map(|turn| {
            let mut view = json!({
                "role": turn.role,
                "text": output_text(&secrets, &turn.text),
                "ts": turn.ts,
                "attachments": turn.attachments,
            });
            if let (Some(id), Some(map)) = (turn.turn_id, view.as_object_mut()) {
                map.insert("turn_id".into(), json!(id));
            }
            // Aviso do sistema (spec 127 T01): `code` válido e `params` saneados, como no evento.
            let notice = turn.notice.filter(|n| turn.role == TurnRole::System && super::events::valid_message_code(&n.code));
            if let (Some(notice), Some(map)) = (notice, view.as_object_mut()) {
                map.insert("code".into(), json!(notice.code));
                map.insert("params".into(), json!(super::events::message_params(&secrets, &notice.params)));
            }
            view
        })
        .collect();
    Ok(Json(json!({ "turns": turns })).into_response())
}

/// Roda o turno numa tarefa destacada: se o cliente desconectar (future do handler dropado), o
/// turno conclui, persiste e publica mesmo assim. Conta como execução ativa para o shutdown.
async fn run_turn(
    state: &AppState,
    id: &str,
    body: &[u8],
    timeout: impl Into<TurnLimits>,
    backend: Option<Arc<dyn ChatBackend>>,
) -> Result<Response, ApiError> {
    let timeout: TurnLimits = timeout.into();
    validate_project_id(id)?;
    let incoming = parse_message(body)?;
    run_incoming(state, id, incoming, timeout, backend).await
}

/// [`run_turn`] com a mensagem já validada.
async fn run_incoming(
    state: &AppState,
    id: &str,
    incoming: Incoming,
    timeout: TurnLimits,
    backend: Option<Arc<dyn ChatBackend>>,
) -> Result<Response, ApiError> {
    validate_project_id(id)?;
    // Prontidão `chat` (spec 093 T02): 412 `prereq_missing` antes do 422 de "não configurado".
    if let Err(resp) = super::readiness::require(state, super::readiness::Gate::Chat).await {
        return Ok(resp);
    }
    let active = super::runs::ActiveRun::enter();
    let state = state.clone();
    let id = id.to_string();
    let task = tokio::spawn(async move {
        let _active = active;
        run_turn_detached(&state, &id, incoming, timeout, backend).await
    });
    match task.await {
        Ok(result) => result,
        Err(_) => Err(ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "turn_id_failed", Map::new())),
    }
}

/// Núcleo do turno. `backend` substitui o cliente HTTP real (testes com fake).
async fn run_turn_detached(
    state: &AppState,
    id: &str,
    incoming: Incoming,
    timeout: TurnLimits,
    backend: Option<Arc<dyn ChatBackend>>,
) -> Result<Response, ApiError> {
    // Mensagem do usuário durante o ciclo de consenso do plano (spec 128 T02, C-11): o contador de
    // rodadas zera e nenhuma rodada nova é aberta; com o turno interno em curso, ela espera por ele
    // e roda normalmente em seguida.
    let runtime = Arc::clone(state.plan_consensus());
    let mut incoming = incoming;
    let user_turn = (!incoming.internal).then(|| runtime.begin_user_turn(id));
    incoming.user_mark = user_turn.as_ref().map(|guard| guard.mark().to_string());
    let mut reserved = state.reserve_chat(id).await;
    let waited_since = Instant::now();
    while matches!(reserved, Err(ReserveError::Busy))
        && !incoming.internal
        && runtime.consensus_turn_running(id)
        && waited_since.elapsed() < CHAT_MAX_TURN
        && !*state.shutdown_signal().borrow()
    {
        tokio::time::sleep(BUSY_POLL).await;
        reserved = state.reserve_chat(id).await;
    }
    // Projeto e turno na mesma seção sob o read lock da configuração (ver `AppState::reserve_chat`).
    let (project, reservation) = match reserved {
        Ok(reserved) => reserved,
        Err(ReserveError::NotFound) => return Err(ApiError::coded(StatusCode::NOT_FOUND, "project_not_found", Map::new())),
        Err(ReserveError::Busy) => return Err(ApiError::coded(StatusCode::CONFLICT, "chat_active", Map::new())),
        Err(ReserveError::ReportInProgress) => {
            return Err(ApiError::coded(StatusCode::CONFLICT, REPORT_BUSY_CODE, Map::new()).with_reason("report_in_progress"))
        }
    };
    // Imagens anexadas precisam existir no projeto (sem symlink): senão 422, sem abrir o turno.
    if !incoming.attachments.is_empty() {
        let (root, names) = (PathBuf::from(&project.path), incoming.attachments.clone());
        let all_exist = tokio::task::spawn_blocking(move || names.iter().all(|n| attachment_exists(&root, n)))
            .await
            .unwrap_or(false);
        if !all_exist {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "attachment_invalid", Map::new()));
        }
    }
    let turn_id = random_hex_id().map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "turn_id_failed", Map::new()))?;
    let secrets = state.secret_set().await;
    let mut scope = state.begin_architect_turn(
        id,
        ArchitectTurn {
            turn_id: turn_id.clone(),
            user_text: if incoming.internal { String::new() } else { output_text(&secrets, &incoming.message) },
            started_at: unix_now(),
        },
        !incoming.internal,
    );
    scope.hold(reservation);
    let result = run_turn_body(state, id, incoming, timeout, backend, project, turn_id, secrets).await;
    if result.as_ref().is_ok_and(|r| r.status().is_success()) {
        scope.succeed();
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_turn_body(
    state: &AppState,
    id: &str,
    incoming: Incoming,
    timeout: TurnLimits,
    backend: Option<Arc<dyn ChatBackend>>,
    project: Project,
    turn_id: String,
    secrets: Arc<SecretSet>,
) -> Result<Response, ApiError> {
    let (model, effort, client, connection) = architect_client_with_connection(state).await?;
    // Spec 133 T01: o papel como foi montado no disparo, para a evidência de uma falha.
    let stage = super::provider_errors::StageSetup { role: "architect", model: model.clone(), connection: Some(connection) };
    let backend: Arc<dyn ChatBackend> = match backend {
        Some(backend) => backend,
        None => Arc::new(client),
    };
    let progress = Progress::new();
    let backend: Arc<dyn ChatBackend> =
        Arc::new(ProgressBackend { inner: backend, progress: Arc::clone(&progress) });

    let root = PathBuf::from(&project.path);
    let policy = load_policy(root.clone()).await;
    let history = load_history(root.clone()).await;
    // Estado do projeto deste turno (fonte da verdade para o Arquiteto). Só vai ao modelo.
    let active_run = state.runs().active(id).is_some();
    let snapshot = project_snapshot(root.clone(), Arc::clone(&secrets), active_run).await;
    let approver = Arc::new(ProjectApprover::new(
        id,
        root.clone(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::clone(&secrets),
    )
    .with_lang(state.project_lang(id)));
    let activity = Arc::new(ProgressSink {
        inner: Arc::new(BusActivitySink::new(state.events().clone(), id, Arc::clone(&secrets))),
        progress: Arc::clone(&progress),
    });
    let prereqs = state.prereqs().clone();
    let items = tokio::task::spawn_blocking(move || prereqs.check())
        .await
        .unwrap_or_default();
    let docker = ["docker_cli", "docker_daemon", "docker_permission", "docker_compose"]
        .iter()
        .all(|id| items.iter().any(|i| i.id == *id && i.ok));
    let host_facts = scrub_text(
        &secrets,
        &crate::agents::prompts::host_facts_note(
            &items.iter().map(|i| (i.label.to_string(), i.ok, i.version.clone())).collect::<Vec<_>>(),
        ),
    );
    let internet = internet_setup(state, root.clone(), &secrets).await;
    // Spec 128 T02: o turno de uma rodada do consenso grava o uso com a etapa própria.
    let usage = match incoming.consensus_round {
        Some(round) => super::usage::FileUsageSink::redacting_staged(
            root.clone(),
            id,
            Arc::clone(&secrets),
            super::plan_consensus::CONSENSUS_STAGE,
            round,
        ),
        None => super::usage::FileUsageSink::redacting(root.clone(), id, Arc::clone(&secrets)),
    };
    let mut architect = LlmArchitect::new(backend, model, Some(effort), policy, approver)
        .with_lang(state.project_lang(id))
        .with_activity(activity, None, None)
        .with_usage(usage)
        .with_memory(Arc::clone(&secrets))
        .with_docker(docker)
        .with_host_facts(host_facts)
        .with_nants_port(state.bound_port())
        .with_image_generation(state.developer_image_generation_available().await);
    if let Some(setup) = internet {
        architect = architect.with_internet(setup);
    }

    // A mensagem do usuário é persistida já no início do turno (sobrevive a timeout/queda).
    let user_turn = ChatTurn {
        role: TurnRole::User,
        text: output_text(&secrets, &incoming.message),
        ts: unix_now(),
        turn_id: Some(turn_id.clone()),
        internal: incoming.internal,
        attachments: incoming.attachments.clone(),
        notice: None,
    };
    let (mark_root, save_root, snap_secrets) = (root.clone(), root.clone(), Arc::clone(&secrets));
    // Retrato do plano antes do turno (spec 086 T09): base do "o que mudou nesta versão".
    let (before, plans_before) = tokio::task::spawn_blocking(move || {
        append_turns(&save_root, &[user_turn]);
        (specs_mark(&mark_root), super::plan_changes::snapshots(&mark_root, &snap_secrets))
    })
    .await
    .unwrap_or_default();
    // Spec 092 T01: ao fim do turno, os avisos do plano (só nas specs que o turno alterou) voltam
    // ao Arquiteto uma vez, para o plano chegar limpo ao usuário.
    let signatures: std::collections::BTreeMap<String, String> =
        plans_before.iter().map(|(spec, (sig, _))| (spec.clone(), sig.clone())).collect();
    let check_secrets = Arc::clone(&secrets);
    let first_reply: crate::agents::architect::FirstReplySlot = Arc::default();
    let architect = architect
        .with_plan_check(Arc::new(move |root: &Path| {
            super::projects::changed_plan_warnings(root, &signatures, &check_secrets)
        }))
        .with_first_reply_slot(Arc::clone(&first_reply));

    // Spec 133 T01: o rastro das chamadas do turno dá a evidência da falha final do provedor.
    let (outcome, trace) = Box::pin(crate::agents::llm_client::evidence::with_trace_scope(async {
        let outcome = with_idle_deadline(
            architect.reply_with_attachments(root.as_path(), &history, &incoming.message, &incoming.attachments, &snapshot),
            timeout,
            &progress,
        )
        .await;
        (outcome, super::provider_errors::StageTrace::current())
    }))
    .await;
    let reply = match outcome {
        None => {
            // Corte durante a continuação dos avisos do plano: vale a primeira resposta (spec 092 T01).
            let first = first_reply.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
            match first {
                Some(first) => {
                    crate::agents::architect::log_plan_check_failure("tempo esgotado");
                    first
                }
                None => return Ok(timeout_response(&secrets, root, turn_id, before).await),
            }
        }
        Some(Ok(reply)) => reply,
        Some(Err(err)) => {
            let ArchitectError::Backend(failure) = &err;
            let evidence = super::provider_errors::record_stage_failure(&secrets, id, &root, None, "chat", &stage, failure, trace).await;
            let failed = FailedTurn { turn_id, plans_before: &plans_before, evidence };
            return Err(failed_turn(state, id, &secrets, root, failed, err).await);
        }
    };
    let kind = TurnKind { consensus: incoming.consensus_round.is_some(), user_mark: incoming.user_mark };
    Ok(finish_turn(state, id, &secrets, root, reply, turn_id, plans_before, kind).await)
}

/// Tipo do turno, para o fim dele.
#[derive(Debug, Clone)]
struct TurnKind {
    /// Rodada do consenso do plano (spec 128 T02).
    consensus: bool,
    /// Marca da mensagem do usuário (turno do usuário).
    user_mark: Option<String>,
}

/// Código da mensagem de sistema gravada quando a conexão caiu no meio do turno (spec 127 T01).
pub const CONNECTION_LOST_CODE: &str = "chat.connection_lost";
/// Idem, quando o turno já tinha gravado parte do plano (`params.spec`).
pub const CONNECTION_LOST_SAVED_CODE: &str = "chat.connection_lost_saved";
/// `reason` do 502 quando a mensagem de sistema foi gravada: a tela recarrega o histórico.
pub const CONNECTION_LOST_REASON: &str = "connection_lost";
/// Código da mensagem de sistema gravada quando o provedor recusou o turno (spec 133 T11), com a
/// evidência nos `params` (papel, conexão, modelo, status, tipo, código e categoria).
pub const PROVIDER_FAILED_NOTICE_CODE: &str = "chat.provider_failed";
/// `reason` do 502 quando a falha do provedor ficou gravada no histórico (spec 133 T11): a tela
/// recarrega o histórico em vez de repetir o aviso.
pub const FAILURE_RECORDED_REASON: &str = "failure_recorded";

/// Falha que é queda da conexão com o provedor: conexão perdida (inclusive no meio da resposta,
/// depois das novas tentativas), tempo esgotado ou transmissão sem evento final.
fn is_connection_lost(failure: &ProviderFailure) -> bool {
    match failure {
        ProviderFailure::ConnectFailed | ProviderFailure::Timeout => true,
        ProviderFailure::BadResponse(reason) => matches!(reason.kind, BadKind::NoFinal | BadKind::Transport),
        _ => false,
    }
}

/// Specs em plano que o turno gravou: assinatura diferente da de antes do turno, ou spec nova.
fn saved_specs(root: &Path, before: &std::collections::BTreeMap<String, String>, secrets: &SecretSet) -> Vec<String> {
    super::plan_changes::snapshots(root, secrets)
        .into_iter()
        .filter(|(spec, (sig, _))| before.get(spec) != Some(sig))
        .map(|(spec, _)| spec)
        .collect()
}

/// O turno que falhou: o id, o retrato do plano antes dele e a evidência da falha do provedor.
struct FailedTurn<'a> {
    turn_id: String,
    plans_before: &'a super::plan_changes::Snapshots,
    evidence: crate::agents::llm_client::FailureEvidence,
}

/// Turno que falhou pelo provedor. O 502 leva nos `params` a causa fixa e a evidência da T01 (spec
/// 133 T11: papel, conexão, modelo, status, tipo, código e categoria). Queda da conexão (spec 127
/// T01): grava no histórico o aviso `chat.connection_lost*` (o que ficou gravado vai nos `params`)
/// e devolve `reason: connection_lost`. Outras falhas (spec 133 T11): grava a mensagem de sistema
/// `chat.provider_failed` com os mesmos `params` e devolve `reason: failure_recorded`. A reserva do
/// texto gravado sai no idioma do projeto; a tela traduz pelo `code`.
async fn failed_turn(state: &AppState, id: &str, secrets: &Arc<SecretSet>, root: PathBuf, failed: FailedTurn<'_>, err: ArchitectError) -> ApiError {
    let ArchitectError::Backend(failure) = &err;
    let lost = is_connection_lost(failure);
    let params = super::provider_errors::stage_failure_params(secrets, failure, &failed.evidence);
    let api = ApiError::coded(StatusCode::BAD_GATEWAY, CODE_PROVIDER_FAILED, params.clone());
    let lang = state.project_lang(id).get();
    let before: std::collections::BTreeMap<String, String> =
        failed.plans_before.iter().map(|(spec, (sig, _))| (spec.clone(), sig.clone())).collect();
    let secrets = Arc::clone(secrets);
    let turn_id = failed.turn_id;
    let saved = tokio::task::spawn_blocking(move || {
        let notice = if lost {
            let spec = saved_specs(&root, &before, &secrets).into_iter().next();
            connection_lost_notice(spec.as_deref())
        } else {
            TurnNotice { code: PROVIDER_FAILED_NOTICE_CODE.to_string(), params }
        };
        let text = output_text(&secrets, &crate::i18n::tr(lang, &format!("system.{}", notice.code), &notice_pairs(&notice)));
        let turn = ChatTurn {
            role: TurnRole::System,
            text,
            ts: unix_now(),
            turn_id: Some(turn_id),
            internal: false,
            attachments: Vec::new(),
            notice: Some(notice),
        };
        append_turns(&root, &[turn])
    })
    .await
    .unwrap_or(false);
    match (saved, lost) {
        (true, true) => api.with_reason(CONNECTION_LOST_REASON),
        (true, false) => api.with_reason(FAILURE_RECORDED_REASON),
        (false, _) => api,
    }
}

/// Aviso da queda: com a spec gravada no turno, `connection_lost_saved` com `params.spec`.
fn connection_lost_notice(spec: Option<&str>) -> TurnNotice {
    match spec {
        Some(spec) => TurnNotice {
            code: CONNECTION_LOST_SAVED_CODE.to_string(),
            params: [("spec".to_string(), json!(spec))].into_iter().collect(),
        },
        None => TurnNotice { code: CONNECTION_LOST_CODE.to_string(), params: Map::new() },
    }
}

/// `params` de texto do aviso, para o [`crate::i18n::tr`] da reserva.
fn notice_pairs(notice: &TurnNotice) -> Vec<(&str, &str)> {
    notice.params.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.as_str(), v))).collect()
}

/// Pedido de continuação ao Arquiteto (spec 127 T01): o mesmo turno interno da divisão das
/// tarefas (`internal_turn`), pelo botão "Tentar de novo" da Conversa ou "Pedir ao Arquiteto para
/// terminar" do cartão do plano. Corpo opcional `{"spec_id": "..."}` (plano sem tarefas).
#[derive(Deserialize, Default)]
struct ContinueInput {
    #[serde(default)]
    spec_id: Option<String>,
}

/// `POST /api/projects/:id/chat/continue`: turno interno "continue de onde parou". Mesma resposta
/// do `POST /chat` (`reply`, `draft_updated`, `turn_id`).
pub async fn post_chat_continue(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input: ContinueInput = if body.iter().all(u8::is_ascii_whitespace) {
        ContinueInput::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|_| ApiError::coded(StatusCode::BAD_REQUEST, "json_malformed", Map::new()))?
    };
    if let Some(spec) = &input.spec_id {
        super::spec_result::validate_spec_id(spec)?;
    }
    let message = crate::agents::prompts::continue_turn_request(input.spec_id.as_deref(), state.project_lang(&id).get());
    internal_turn_response(&state, &id, message).await
}

/// Turno cortado: persiste um turno curto do Arquiteto informando o corte e devolve o 504,
/// com `draft_saved` quando spec/rascunho foi gravado durante o turno.
async fn timeout_response(
    secrets: &Arc<SecretSet>,
    root: PathBuf,
    turn_id: String,
    before: (usize, Option<SystemTime>),
) -> Response {
    let saved_root = root.clone();
    let draft_saved = tokio::task::spawn_blocking(move || specs_mark(&saved_root) != before)
        .await
        .unwrap_or(false);
    let text = output_text(
        secrets,
        if draft_saved { TIMEOUT_TURN_SAVED } else { TIMEOUT_TURN_TEXT },
    );
    let turn = ChatTurn {
        role: TurnRole::Architect,
        text,
        ts: unix_now(),
        turn_id: Some(turn_id),
        internal: false,
        attachments: Vec::new(),
        notice: None,
    };
    let _saved = tokio::task::spawn_blocking(move || append_turns(&root, &[turn])).await;
    let err = ApiError::coded(StatusCode::GATEWAY_TIMEOUT, TIMEOUT_CODE, Map::new());
    let err = if draft_saved { err.with_field("draft_saved", json!(true)).with_reason("draft_saved") } else { err };
    err.into_response()
}

/// Redige, publica eventos, persiste o turno e monta a resposta 200.
/// `turn_id` identifica o turno: vai no `agent_message` publicado e na resposta.
///
/// Spec 128 T02: o bloco `nants-plan-decisions` sai do texto mostrado e vai na extensão da resposta
/// ([`TurnOutcome`]). A resposta de uma rodada do consenso fica fora da Conversa (gravada como
/// `internal`, sem `agent_message`), salvo quando pergunta algo ao usuário (`nants-choices`).
#[allow(clippy::too_many_arguments)]
async fn finish_turn(
    state: &AppState,
    id: &str,
    secrets: &Arc<SecretSet>,
    root: PathBuf,
    reply: ArchitectReply,
    turn_id: String,
    plans_before: super::plan_changes::Snapshots,
    kind: TurnKind,
) -> Response {
    // Spec 131 T03: o bloco vale em qualquer mensagem do assistente no turno (o último dito).
    let (shown, last) = super::plan_consensus::split_decisions(&reply.text);
    let decisions = super::plan_consensus::decisions_in(&reply.said).or(last);
    let asks_user = super::choices::parse_choices(&shown).is_some();
    let hidden = kind.consensus && !asks_user;
    let text = output_text(secrets, &shown);
    let now = unix_now();
    let turns = [
        ChatTurn {
            role: TurnRole::Architect,
            text: text.clone(),
            ts: now,
            turn_id: Some(turn_id.clone()),
            internal: hidden,
            attachments: Vec::new(),
            notice: None,
        },
    ];
    // Falha de persistência não derruba o turno: o histórico é melhor esforço.
    let review_root = root.clone();
    let _saved = tokio::task::spawn_blocking(move || append_turns(&root, &turns))
        .await
        .unwrap_or(false);

    if !hidden {
        state.events().publish(ApiEvent::new(
            EventType::AgentMessage,
            Some(id.to_string()),
            json!({"agent": ARCHITECT_AGENT, "text": text, "turn_id": turn_id}),
        ));
    }
    // Fim do turno do usuário antes do gatilho da revisão: o ciclo de consenso volta a revisar.
    let deferred = match &kind.user_mark {
        Some(mark) => {
            state.plan_consensus().end_user_turn(id, mark);
            state.plan_consensus().take_deferred(id)
        }
        None => false,
    };
    if deferred && !reply.artifacts_touched {
        // O ciclo parou por causa deste turno: a versão atual ainda precisa da revisão.
        let _reviews = super::plan_review::after_chat_turn(state, id, review_root.clone()).await;
    }
    if reply.artifacts_touched {
        // O que mudou no plano (spec 086 T09): gravado antes do evento, para a UI já recarregar com ele.
        let (changes_root, changes_secrets) = (review_root.clone(), Arc::clone(secrets));
        let _changed = tokio::task::spawn_blocking(move || {
            super::plan_changes::record_changes(&changes_root, &plans_before, &changes_secrets)
        })
        .await;
        // Revisão do plano (spec 086 T08): reserva a vaga antes do evento (a UI já vê "revisando…")
        // e roda em segundo plano; o turno não espera.
        let _reviews = super::plan_review::after_chat_turn(state, id, review_root).await;
        state.events().publish(ApiEvent::new(
            EventType::SddDraftUpdated,
            Some(id.to_string()),
            json!({"project_id": id}),
        ));
    }
    let mut response = Json(json!({
        "reply": text,
        "draft_updated": reply.artifacts_touched,
        "turn_id": turn_id,
    }))
    .into_response();
    response.extensions_mut().insert(TurnOutcome { decisions, asks_user, reply: text });
    response
}

/// Texto de saída: redação de segredos e URLs, depois limite de 8 KiB.
fn output_text(secrets: &SecretSet, text: &str) -> String {
    truncate_bytes(&scrub_text(secrets, text), MAX_MESSAGE_BYTES)
}

/// Decodifica o corpo e valida a mensagem. JSON malformado vira 400; campos inválidos, 422.
fn parse_message(body: &[u8]) -> Result<Incoming, ApiError> {
    let input: ChatInput = serde_json::from_slice(body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields", Map::new())
        } else {
            ApiError::coded(StatusCode::BAD_REQUEST, "json_malformed", Map::new())
        }
    })?;
    let attachments = validate_attachments(input.attachments)?;
    // Mensagem só com imagem: o texto vazio vira "(imagem)".
    let message = if input.message.trim().is_empty() && !attachments.is_empty() {
        IMAGE_ONLY_TEXT.to_string()
    } else {
        input.message
    };
    validate_message(&message)?;
    Ok(Incoming {
        message,
        internal: input.internal,
        attachments,
        consensus_round: None,
        user_mark: None,
    })
}

/// Até [`MAX_MESSAGE_ATTACHMENTS`] nomes válidos, sem repetição (a ordem é mantida). 422 se não.
fn validate_attachments(names: Vec<String>) -> Result<Vec<String>, ApiError> {
    if names.len() > MAX_MESSAGE_ATTACHMENTS {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "attachments_max", Map::new()));
    }
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        if attachment_mime(&name).is_none() {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "attachment_invalid", Map::new()));
        }
        if !out.contains(&name) {
            out.push(name);
        }
    }
    Ok(out)
}

/// Mensagem não vazia, com até [`MAX_CHAT_MESSAGE_CHARS`] caracteres e sem controles
/// além de `\n` e `\t`. Mensagens de erro são estáticas.
fn validate_message(message: &str) -> Result<(), ApiError> {
    if message.trim().is_empty() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "message_required", Map::new()));
    }
    if message.chars().count() > MAX_CHAT_MESSAGE_CHARS {
        return Err(ApiError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "message_too_long",
            super::error::params([("max", json!(MAX_CHAT_MESSAGE_CHARS))]),
        ));
    }
    if message
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "message_control_chars", Map::new()));
    }
    Ok(())
}

/// Projeto configurado (clone; o lock de leitura é solto antes de qualquer I/O).
async fn find_project(state: &AppState, id: &str) -> Result<Project, ApiError> {
    let found = {
        let cfg = state.config().read().await;
        cfg.projects.iter().find(|p| p.id == id).cloned()
    };
    found.ok_or_else(|| ApiError::coded(StatusCode::NOT_FOUND, "project_not_found", Map::new()))
}

/// Modelo, esforço e cliente do Arquiteto. Qualquer falha de configuração vira 422 estático.
pub(super) async fn architect_client(state: &AppState) -> Result<(String, Effort, LlmClient), ApiError> {
    architect_client_with_connection(state).await.map(|(model, effort, client, _)| (model, effort, client))
}

/// [`architect_client`] com a conexão usada (spec 133 T01): lidos na mesma leitura da
/// configuração, para a evidência de uma falha registrar a conexão e o modelo do disparo.
async fn architect_client_with_connection(
    state: &AppState,
) -> Result<(String, Effort, LlmClient, crate::agents::llm_client::ConnectionRef), ApiError> {
    let (agent, connection) = {
        let cfg = state.config().read().await;
        let agent = cfg.agents.architect.clone();
        let connection = cfg
            .connections
            .iter()
            .find(|c| c.id == agent.connection_id)
            .cloned();
        (agent, connection)
    };
    let not_configured = || ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "architect_not_configured", Map::new());
    let connection = connection.ok_or_else(not_configured)?;
    if agent.model.trim().is_empty() {
        return Err(not_configured());
    }
    let client = super::chatgpt::plan_client::llm_client(state, &connection).map_err(|_| not_configured())?;
    Ok((agent.model, agent.effort, client, super::provider_errors::connection_ref(&connection)))
}

/// Código (spec 102) da falha do provedor no turno do Arquiteto (`params.provider`: a causa fixa,
/// ex.: `provider_http_400`; `params.kind`/`params.provider_code`: o motivo neutro, quando houver;
/// spec 133 T11: a evidência, ver [`failed_turn`]). Spec 119: sem o `detail` em pt-BR; a tela
/// monta o motivo pelo dicionário, no idioma dela.
pub const CODE_PROVIDER_FAILED: &str = "provider_failed";

/// Internet do Arquiteto (spec 076) para um turno de plano/recuperação: `None` com o interruptor do
/// projeto desligado (padrão). Liga a busca nativa só se a conexão a suporta e não a rejeitou.
pub(super) async fn internet_setup(
    state: &AppState,
    root: PathBuf,
    secrets: &Arc<SecretSet>,
) -> Option<InternetSetup> {
    let enabled = tokio::task::spawn_blocking(move || {
        read_settings(&sdd_dir(&root).join("settings.yaml")).is_ok_and(|s| s.architect_internet)
    })
    .await
    .unwrap_or(false);
    if !enabled {
        return None;
    }
    let (plan, recovery, connection_id) = {
        let cfg = state.config().read().await;
        (
            cfg.architect_internet.plan(),
            cfg.architect_internet.recovery(),
            cfg.agents.architect.connection_id.clone(),
        )
    };
    let hook_state = state.clone();
    Some(InternetSetup {
        web_search: state.web_search_available().await,
        plan,
        recovery,
        secrets: Arc::clone(secrets),
        on_web_rejected: Arc::new(move || hook_state.mark_web_search_rejected(&connection_id)),
    })
}

/// Políticas do projeto (`allowed_commands`/`denied_commands`). Ausentes ou inválidas: vazias.
pub(super) async fn load_policy(root: PathBuf) -> Policy {
    tokio::task::spawn_blocking(move || {
        match read_settings(&sdd_dir(&root).join("settings.yaml")) {
            Ok(settings) => Policy::from_settings(&settings),
            Err(_) => Policy::new(Vec::new(), Vec::new()),
        }
    })
    .await
    .unwrap_or_else(|_| Policy::new(Vec::new(), Vec::new()))
}

async fn load_history(root: PathBuf) -> Vec<ChatTurn> {
    tokio::task::spawn_blocking(move || read_history(&root))
        .await
        .unwrap_or_default()
}

async fn load_visible_history(root: PathBuf) -> Vec<ChatTurn> {
    tokio::task::spawn_blocking(move || read_visible_history(&root))
        .await
        .unwrap_or_default()
}

/// Contexto do Arquiteto: os turnos mais recentes, inclusive os internos. Os avisos do sistema
/// (spec 127 T01) são da pessoa, não do modelo: ficam fora e não ocupam vaga do limite.
pub fn read_history(root: &Path) -> Vec<ChatTurn> {
    last_turns(read_turns(root), |turn| turn.role != TurnRole::System)
}

/// Histórico exibido ao usuário: os turnos mais recentes sem os internos (o limite conta só os visíveis).
pub fn read_visible_history(root: &Path) -> Vec<ChatTurn> {
    last_turns(read_turns(root), |turn| !turn.internal)
}

/// Os últimos [`MAX_HISTORY_TURNS`] turnos que passam em `keep`, na ordem do arquivo.
fn last_turns(turns: Vec<ChatTurn>, keep: impl Fn(&ChatTurn) -> bool) -> Vec<ChatTurn> {
    let mut kept: Vec<ChatTurn> = turns.into_iter().filter(|turn| keep(turn)).collect();
    let keep_from = kept.len().saturating_sub(MAX_HISTORY_TURNS);
    kept.split_off(keep_from)
}

/// Lê todos os turnos da janela final do arquivo. Arquivo ausente, inseguro (symlink, não regular)
/// ou ilegível vira lista vazia. Lê no máximo 1 MiB do fim; linhas inválidas são ignoradas.
fn read_turns(root: &Path) -> Vec<ChatTurn> {
    let Ok(path) = safe_fs::check_chain(root, CHAT_FILE_REL) else {
        return Vec::new();
    };
    let Ok(mut file) = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(&path)
    else {
        return Vec::new();
    };
    let Ok(meta) = file.metadata() else {
        return Vec::new();
    };
    if !meta.is_file() {
        return Vec::new();
    }
    let start = meta.len().saturating_sub(HISTORY_READ_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if (&mut file)
        .take(HISTORY_READ_BYTES)
        .read_to_end(&mut buf)
        .is_err()
    {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if start > 0 {
        // A primeira linha pode ter sido cortada pelo início da janela.
        lines.next();
    }
    lines
        .filter_map(|line| serde_json::from_str(line).ok())
        .map(sanitize_turn)
        .collect()
}

/// Identidade de turno válida: 1 a 64 caracteres hexadecimais minúsculos. Inválida vira `None`.
fn valid_turn_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Descarta um `turn_id` inválido e nomes de anexo inválidos sem descartar o turno.
fn sanitize_turn(mut turn: ChatTurn) -> ChatTurn {
    if turn.turn_id.as_deref().is_some_and(|id| !valid_turn_id(id)) {
        turn.turn_id = None;
    }
    turn.attachments.retain(|name| attachment_mime(name).is_some());
    turn.attachments.truncate(MAX_MESSAGE_ATTACHMENTS);
    turn
}

/// Anexos citados nos últimos `turns` turnos do histórico (inclusive internos).
pub(super) fn recent_attachments(root: &Path, turns: usize) -> std::collections::HashSet<String> {
    let all = read_turns(root);
    let from = all.len().saturating_sub(turns);
    all[from..].iter().flat_map(|t| t.attachments.iter().cloned()).collect()
}

/// Anexos citados em TODOS os turnos do histórico (spec 085 T02), lidos linha a linha sem a janela
/// de [`HISTORY_READ_BYTES`]. Histórico ausente = nenhum. `None` quando o arquivo existe mas não dá
/// para lê-lo por inteiro com segurança (symlink, não regular, falha de I/O): quem limpa anexos não
/// pode concluir que algo é órfão. Linhas inválidas são ignoradas.
pub(super) fn all_attachments(root: &Path) -> Option<std::collections::HashSet<String>> {
    use std::io::BufRead;

    /// Só o campo que interessa: evita montar o turno inteiro.
    #[derive(Deserialize)]
    struct Line {
        #[serde(default)]
        attachments: Vec<String>,
    }

    let mut names = std::collections::HashSet::new();
    let path = safe_fs::check_chain(root, CHAT_FILE_REL).ok()?;
    let file = match OpenOptions::new().read(true).custom_flags(O_NOFOLLOW | O_NONBLOCK).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(names),
        Err(_) => return None,
    };
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut reader = std::io::BufReader::new(file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf).ok()? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        if !line.contains("attachments") {
            continue;
        }
        if let Ok(parsed) = serde_json::from_str::<Line>(line.trim_end()) {
            names.extend(parsed.attachments.into_iter().filter(|n| attachment_mime(n).is_some()));
        }
    }
    Some(names)
}

/// Acrescenta turnos como linhas JSON em `.sdd/specs/.chat.jsonl`, sem seguir symlinks.
/// `true` se gravou. Qualquer falha (inclusive symlink) retorna `false` sem efeito.
pub fn append_turns(root: &Path, turns: &[ChatTurn]) -> bool {
    if safe_fs::ensure_dirs(root, SDD_SPECS_REL).is_err() {
        return false;
    }
    let Ok(path) = safe_fs::check_chain(root, CHAT_FILE_REL) else {
        return false;
    };
    let mut out = String::new();
    for turn in turns {
        let Ok(line) = serde_json::to_string(turn) else {
            return false;
        };
        out.push_str(&line);
        out.push('\n');
    }
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(&path)
    else {
        return false;
    };
    match file.metadata() {
        Ok(meta) if meta.is_file() => {}
        _ => return false,
    }
    file.write_all(out.as_bytes()).is_ok()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::future::Future;
    use std::os::unix::fs::symlink;
    use std::pin::Pin;
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use serde_json::Value;
    use axum::http::{header, Method, Request};
    use tokio::sync::Notify;
    use tower::ServiceExt;

    use super::*;
    use crate::agents::architect::testing::{calls, text, tmp_root, Reply, Scripted};
    use crate::agents::llm_client::{ChatRequest, Role as ChatRole};
    use crate::agents::LlmError;
    use crate::config::{AgentConfig, Config, Connection};
    use crate::agents::prompts::EXAMPLE_SPEC_MD;
    use crate::server::router;

    const HOST: &str = "127.0.0.1:3000";
    const SECRET: &str = "sk-SEGREDO-CHAT-1234";
    const PROJECT_ID: &str = "proj-chat";

    fn config_for(root: &Path, configured: bool) -> Config {
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "literal".into(),
            name: "Conexão literal".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        cfg.agents.architect = if configured {
            AgentConfig {
                connection_id: "literal".into(),
                model: "gpt-4o".into(),
                effort: Effort::Low,
                max_steps: None,
            }
        } else {
            AgentConfig {
                connection_id: "inexistente".into(),
                model: "gpt-4o".into(),
                effort: Effort::Low,
                max_steps: None,
            }
        };
        cfg.projects.push(Project {
            id: PROJECT_ID.into(),
            name: "Projeto de chat".into(),
            path: root.to_str().expect("utf8").into(),
            created_at: "2026-10-14T00:00:00Z".into(),
        });
        cfg
    }

    fn state_for(root: &Path, cfg: Config) -> AppState {
        AppState::new(root.join("config.toml"), cfg, 3000)
    }

    fn backend(replies: Vec<Reply>) -> (Arc<Scripted>, Option<Arc<dyn ChatBackend>>) {
        let scripted = Scripted::new(replies);
        let dynamic: Arc<dyn ChatBackend> = scripted.clone();
        (scripted, Some(dynamic))
    }

    /// Executa um turno direto no núcleo e devolve `(status, corpo JSON)`.
    async fn turn(
        state: &AppState,
        id: &str,
        message: &str,
        backend: Option<Arc<dyn ChatBackend>>,
        timeout: impl Into<TurnLimits>,
    ) -> (StatusCode, Value) {
        let body = json!({ "message": message }).to_string();
        // O erro passa pela mesma resposta HTTP do servidor (inclui `detail`, spec 088).
        let resp = match run_turn(state, id, body.as_bytes(), timeout, backend).await {
            Ok(resp) => resp,
            Err(e) => e.into_response(),
        };
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
        (status, serde_json::from_slice(&bytes).expect("json"))
    }

    async fn post_raw(state: &AppState, uri: &str, body: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("requisição");
        let resp = router(state.clone()).oneshot(req).await.expect("resposta");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn has_tool_msg(req: &ChatRequest, id: &str, needle: &str) -> bool {
        req.messages.iter().any(|m| {
            m.role == ChatRole::Tool
                && m.tool_call_id.as_deref() == Some(id)
                && m.content.contains(needle)
        })
    }

    fn chat_lines(root: &Path) -> Vec<Value> {
        fs::read_to_string(root.join(CHAT_FILE_REL))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).expect("linha json"))
            .collect()
    }

    fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<ApiEvent>) -> Vec<ApiEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    /// Fake que segura a primeira chamada até ser liberado (para testar concorrência).
    struct Gate {
        entered: Notify,
        release: Notify,
    }

    impl ChatBackend for Gate {
        fn complete<'a>(
            &'a self,
            _req: &'a ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                text("ok")
            })
        }
    }

    /// Fake que nunca responde.
    struct Hang;

    impl ChatBackend for Hang {
        fn complete<'a>(
            &'a self,
            _req: &'a ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
            Box::pin(async { std::future::pending::<Reply>().await })
        }
    }

    /// Spec de exemplo com `run_command` (spec 131 T04: sem ele, o plano recebe um aviso).
    fn spec_with_run() -> String {
        EXAMPLE_SPEC_MD.replace("created_at", "run_command: \"cargo run\"\ncreated_at")
    }

    fn write_spec_call() -> Reply {
        let args = json!({"path": ".sdd/specs/001-exemplo/spec.md", "content": spec_with_run()});
        calls(&[("c1", "write_sdd_artifacts", &args.to_string())])
    }

    // ---- Internet do Arquiteto (spec 076) ----

    fn enable_internet(root: &Path, on: bool) {
        let dir = root.join(".sdd");
        fs::create_dir_all(&dir).expect("mkdir");
        let flag = if on { "architect_internet: true\n" } else { "" };
        fs::write(
            dir.join("settings.yaml"),
            format!("base_branch: main\nmerge_strategy: squash\ntest_command: cargo test\n{flag}"),
        )
        .expect("settings");
    }

    fn responses_config(root: &Path) -> Config {
        let mut cfg = config_for(root, true);
        cfg.connections[0].wire_api = crate::config::WireApi::Responses;
        cfg
    }

    fn net_names(req: &ChatRequest) -> Vec<String> {
        req.tools
            .iter()
            .map(|t| t.name.clone())
            .filter(|n| n == "fetch_url" || n == "package_versions")
            .collect()
    }

    #[tokio::test]
    async fn chat_turn_with_switch_off_has_no_internet() {
        let root = tmp_root("chat-net-off");
        enable_internet(&root, false);
        let state = state_for(&root, responses_config(&root));
        let (scripted, dynb) = backend(vec![text("ok")]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let req = &scripted.requests()[0];
        assert!(!req.web_search && net_names(req).is_empty());
        assert!(!req.messages[0].content.contains("INTERNET (habilitada"));
    }

    #[tokio::test]
    async fn chat_turn_with_switch_on_uses_configured_limits_and_tools() {
        let root = tmp_root("chat-net-on");
        enable_internet(&root, true);
        let mut cfg = responses_config(&root);
        cfg.architect_internet.plan_searches = 1;
        let state = state_for(&root, cfg);
        let first = {
            let mut r = calls(&[("c1", "list_dir", r#"{"path":"."}"#)]);
            if let Ok(resp) = r.as_mut() {
                resp.web_search_calls = 1;
            }
            r
        };
        let (scripted, dynb) = backend(vec![first, text("ok")]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let reqs = scripted.requests();
        assert_eq!(reqs.iter().map(|r| r.web_search).collect::<Vec<_>>(), vec![true, false]);
        assert_eq!(net_names(&reqs[0]).len(), 2);
        assert!(reqs[0].messages[0].content.contains("Fontes consultadas"));
    }

    #[tokio::test]
    async fn chat_turn_on_chat_wire_has_tools_but_no_native_search() {
        let root = tmp_root("chat-net-wire");
        enable_internet(&root, true);
        let state = state_for(&root, config_for(&root, true));
        let (scripted, dynb) = backend(vec![text("ok")]);
        let (status, _) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK);
        let req = &scripted.requests()[0];
        assert!(!req.web_search);
        assert_eq!(net_names(req).len(), 2);
    }

    #[tokio::test]
    async fn provider_rejection_is_remembered_per_connection_until_restart() {
        let root = tmp_root("chat-net-reject");
        enable_internet(&root, true);
        let state = state_for(&root, responses_config(&root));
        assert!(state.web_search_available().await);
        let rejected = {
            let mut r = text("ok");
            if let Ok(resp) = r.as_mut() {
                resp.web_search_rejected = true;
            }
            r
        };
        let (_, dynb) = backend(vec![rejected]);
        let (status, _) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!state.web_search_available().await);
        assert!(state.web_search_rejected("literal"));
        let (scripted, dynb) = backend(vec![text("de novo")]);
        let (status, _) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK);
        let req = &scripted.requests()[0];
        assert!(!req.web_search, "o turno seguinte já vai sem a ferramenta");
        assert_eq!(net_names(req).len(), 2);
    }

    // ---- Geração de imagens do Desenvolvedor (spec 090) ----

    #[tokio::test]
    async fn chat_turn_carries_developer_image_generation_line() {
        let root = tmp_root("chat-image-gen");
        let mut cfg = config_for(&root, true);
        cfg.agents.developer.connection_id = "literal".into();
        cfg.agents.developer.model = "m-img".into();
        let state = state_for(&root, cfg);
        let (scripted, dynb) = backend(vec![text("ok")]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let first = &scripted.requests()[0].messages[0].content;
        assert!(first.contains("GERAÇÃO DE IMAGENS (DADO do Nants): disponível para o Developer: não."), "wire chat: {first}");

        state.config().write().await.connections[0].wire_api = crate::config::WireApi::Responses;
        let (scripted, dynb) = backend(vec![text("ok")]);
        let (status, _) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK);
        let first = &scripted.requests()[0].messages[0].content;
        assert!(first.contains("disponível para o Developer: sim."), "{first}");

        state.mark_image_unsupported("literal", "m-img");
        let (scripted, dynb) = backend(vec![text("ok")]);
        let (status, _) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK);
        let first = &scripted.requests()[0].messages[0].content;
        assert!(first.contains("disponível para o Developer: não."), "recusa registrada: {first}");
    }

    #[tokio::test]
    async fn text_turn_returns_reply_and_persists_both_turns() {
        let root = tmp_root("chat-text");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![text("Olá!")]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["reply"], "Olá!");
        assert_eq!(body["draft_updated"], false);
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["role"], "user");
        assert_eq!(lines[0]["text"], "oi");
        assert_eq!(lines[1]["role"], "architect");
        assert_eq!(lines[1]["text"], "Olá!");
        assert!(lines[1]["ts"].is_u64());
    }

    fn write_task_call(content: &str) -> Reply {
        let args = json!({"path": ".sdd/specs/001-exemplo/tasks/T01.md", "content": content});
        calls(&[("c2", "write_sdd_artifacts", &args.to_string())])
    }

    fn write_task_as(call_id: &str, spec: &str, content: &str) -> Reply {
        let args = json!({"path": format!(".sdd/specs/{spec}/tasks/T01.md"), "content": content});
        calls(&[(call_id, "write_sdd_artifacts", &args.to_string())])
    }

    fn task_with_deps_edn() -> String {
        crate::agents::prompts::EXAMPLE_TASK_MD.replace("- [ ] Critério verificável.", "- [ ] `deps.edn` lista a dependência.")
    }

    #[tokio::test]
    async fn plan_warning_is_corrected_in_the_same_turn() {
        // Spec 092 T01: o aviso volta ao Arquiteto uma vez; a resposta é a da continuação.
        let root = tmp_root("chat-scope-warning");
        let state = state_for(&root, config_for(&root, true));
        let (scripted, dynb) = backend(vec![
            write_spec_call(),
            write_task_call(&task_with_deps_edn()),
            text("Plano pronto."),
            write_task_as("c3", "001-exemplo", crate::agents::prompts::EXAMPLE_TASK_MD),
            text("Plano corrigido."),
        ]);
        let (status, body) = turn(&state, PROJECT_ID, "gera o plano", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let reqs = scripted.requests();
        assert_eq!(reqs.len(), 5);
        let asked = &reqs[3].messages.last().expect("mensagem").content;
        assert!(asked.starts_with(&crate::agents::architect::plan_check_intro(crate::i18n::Lang::PtBr)), "{asked}");
        assert!(asked.contains("T01: critério cita `deps.edn`, fora do escopo"), "{asked}");
        assert_eq!(body["reply"], "Plano corrigido.");
        assert_eq!(body["draft_updated"], true);
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2, "a correção não vira turno interno");
        assert_eq!(lines[1]["text"], "Plano corrigido.");
        assert!(crate::server::projects::plan_scope_warnings(&root, ".sdd/specs/001-exemplo").is_empty(), "plano limpo");
    }

    /// Fake que segue o roteiro nas `n` primeiras chamadas e depois nunca responde.
    struct ScriptThenHang {
        script: Arc<Scripted>,
        n: usize,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ChatBackend for ScriptThenHang {
        fn complete<'a>(&'a self, req: &'a ChatRequest) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
            Box::pin(async move {
                if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= self.n {
                    return std::future::pending::<Reply>().await;
                }
                self.script.complete(req).await
            })
        }
    }

    #[tokio::test]
    async fn stuck_correction_delivers_the_first_reply_and_runs_post_turn() {
        // Revisão da 092 T01 (F-02): a continuação que trava não derruba o turno em 504.
        let root = tmp_root("chat-scope-stuck");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let script = Scripted::new(vec![write_spec_call(), write_task_call(&task_with_deps_edn()), text("Plano pronto.")]);
        let dynb: Arc<dyn ChatBackend> = Arc::new(ScriptThenHang { script, n: 3, calls: Default::default() });
        let (status, body) = turn(&state, PROJECT_ID, "gera o plano", Some(dynb), Duration::from_millis(300)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["reply"], "Plano pronto.");
        assert_eq!(body["draft_updated"], true);
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[1]["text"], "Plano pronto.");
        let events = drain_events(&mut rx);
        assert!(events.iter().any(|e| e.event_type() == EventType::SddDraftUpdated.as_str()), "pós-turno rodou");
    }

    #[tokio::test]
    async fn plan_warning_correction_happens_at_most_once() {
        let root = tmp_root("chat-scope-once");
        let state = state_for(&root, config_for(&root, true));
        let (scripted, dynb) = backend(vec![
            write_spec_call(),
            write_task_call(&task_with_deps_edn()),
            text("Plano pronto."),
            write_task_as("c3", "001-exemplo", &task_with_deps_edn().replace("lista a dependência", "lista a biblioteca")),
            text("Mantido: o `deps.edn` é de outra tarefa."),
            text("não deveria ser pedido"),
        ]);
        let (status, body) = turn(&state, PROJECT_ID, "gera o plano", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(scripted.requests().len(), 5, "uma continuação só");
        assert_eq!(body["reply"], "Mantido: o `deps.edn` é de outra tarefa.");
        assert!(!crate::server::projects::plan_scope_warnings(&root, ".sdd/specs/001-exemplo").is_empty(), "o aviso fica para a UI");
    }

    #[tokio::test]
    async fn conversation_turn_and_untouched_specs_are_not_checked() {
        // Plano com aviso já em disco: turno só de conversa não confere nada.
        let root = tmp_root("chat-scope-talk");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![write_spec_call(), write_task_call(&task_with_deps_edn()), text("Plano pronto."), text("ok")]);
        let (status, _) = turn(&state, PROJECT_ID, "gera o plano", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK);
        let (scripted, dynb) = backend(vec![text("Qual o escopo?")]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(scripted.requests().len(), 1);
        assert_eq!(body["reply"], "Qual o escopo?");

        // Turno que altera outra spec (limpa): o aviso da spec não alterada não volta ao modelo.
        let other = json!({"path": ".sdd/specs/002-outra/spec.md", "content": spec_with_run().replace("001-exemplo", "002-outra")});
        let (scripted, dynb) = backend(vec![
            calls(&[("c9", "write_sdd_artifacts", &other.to_string())]),
            write_task_as("c10", "002-outra", &crate::agents::prompts::EXAMPLE_TASK_MD.replace("001-exemplo", "002-outra")),
            text("Outra pronta."),
        ]);
        let (status, body) = turn(&state, PROJECT_ID, "outra spec", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(scripted.requests().len(), 3, "{body}");
        assert_eq!(body["reply"], "Outra pronta.");
    }

    #[tokio::test]
    async fn no_scope_warning_means_no_internal_turn() {
        let root = tmp_root("chat-scope-clean");
        let state = state_for(&root, config_for(&root, true));
        let (scripted, dynb) = backend(vec![
            write_spec_call(),
            write_task_call(crate::agents::prompts::EXAMPLE_TASK_MD),
            text("Plano pronto."),
        ]);
        let (status, body) = turn(&state, PROJECT_ID, "gera o plano", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(scripted.requests().len(), 3);
        assert!(chat_lines(&root).iter().all(|l| l["internal"] != true));
    }

    fn write_spec_n(n: usize) -> Reply {
        let content = format!("{EXAMPLE_SPEC_MD}\n<!-- {n} -->\n");
        let args = json!({"path": ".sdd/specs/001-exemplo/spec.md", "content": content});
        calls(&[(&format!("c{n}"), "write_sdd_artifacts", &args.to_string())])
    }

    #[tokio::test]
    async fn plan_turn_triggers_background_plan_review_once() {
        // Spec 086 T08: turno que grava o plano AWAITING_APPROVAL dispara a revisão sem esperá-la.
        let root = tmp_root("chat-plan-review");
        let mut cfg = config_for(&root, true);
        cfg.agents.reviewer = AgentConfig { connection_id: "literal".into(), model: "rev".into(), effort: Effort::Medium, max_steps: None };
        let gate = Arc::new(Notify::new());
        let reviewer = Arc::new(GatedReview(Arc::clone(&gate)));
        let state = state_for(&root, cfg).with_report_backend(reviewer);
        // Plano com tarefa: sem nenhuma, a revisão não roda (spec 127 T01).
        let task = write_task_call(crate::agents::prompts::EXAMPLE_TASK_MD);
        let (_, dynb) = backend(vec![write_spec_call(), task, text("Plano pronto.")]);
        let (status, body) = turn(&state, PROJECT_ID, "gera o plano", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let secrets = SecretSet::default();
        let running = crate::server::plan_review::view(&root, "001-exemplo", &secrets).expect("revisando");
        assert_eq!(running.status, "running", "o turno não esperou a revisão");
        gate.notify_one();
        let mut done = None;
        for _ in 0..400 {
            done = crate::server::plan_review::view(&root, "001-exemplo", &secrets).filter(|v| v.status == "ok");
            if done.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(done.expect("revisão concluída").issues.len(), 1);
    }

    #[tokio::test]
    async fn plan_turn_records_what_changed_since_previous_version() {
        // Spec 086 T09: o primeiro plano não grava resumo; o turno que o altera grava as linhas.
        let root = tmp_root("chat-plan-changes");
        let state = state_for(&root, config_for(&root, true));
        let changes = root.join(".sdd/specs/001-exemplo/plan-changes.json");
        let (_, dynb) = backend(vec![write_spec_call(), write_task_call(crate::agents::prompts::EXAMPLE_TASK_MD), text("Plano pronto.")]);
        let (status, body) = turn(&state, PROJECT_ID, "gera o plano", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!changes.exists(), "primeiro plano: nada gravado");

        let edited = crate::agents::prompts::EXAMPLE_TASK_MD.replace("- [ ] Critério verificável.", "- [ ] Outro critério.\n- [ ] Mais um.");
        let (_, dynb) = backend(vec![write_task_call(&edited), text("Ajustado.")]);
        let (status, body) = turn(&state, PROJECT_ID, "ajusta", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let lines = crate::server::plan_changes::view(&root, "001-exemplo", &SecretSet::default()).expect("resumo");
        let lines: Vec<String> = lines.iter().map(|l| l.render(crate::i18n::Lang::PtBr)).collect();
        assert_eq!(lines, vec!["T01: critérios alterados (2 novos, 1 removido)"]);
    }

    /// Revisor do plano fake: espera a liberação e aponta um problema.
    struct GatedReview(Arc<Notify>);

    impl ChatBackend for GatedReview {
        fn complete<'a>(&'a self, _req: &'a ChatRequest) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
            Box::pin(async move {
                self.0.notified().await;
                text(r#"{"issues":[{"task":null,"kind":"untestable","text":"Critério vago."}]}"#)
            })
        }
    }

    #[tokio::test]
    async fn spec_write_sets_draft_flag_and_emits_events() {
        let root = tmp_root("chat-draft");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let (_, dynb) = backend(vec![write_spec_call(), text("Rascunho pronto.")]);
        let (status, body) = turn(&state, PROJECT_ID, "gera o spec", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["draft_updated"], true);
        assert!(root.join(".sdd/specs/001-exemplo/spec.md").exists());
        let events = drain_events(&mut rx);
        let draft = events
            .iter()
            .find(|e| e.event_type() == "sdd_draft_updated")
            .expect("sdd_draft_updated");
        assert_eq!(draft.project_id.as_deref(), Some(PROJECT_ID));
        assert_eq!(draft.payload["project_id"], PROJECT_ID);
        let msg = events
            .iter()
            .find(|e| e.event_type() == "agent_message")
            .expect("agent_message");
        assert_eq!(msg.payload["agent"], "architect");
        assert_eq!(msg.payload["text"], "Rascunho pronto.");
    }

    #[tokio::test]
    async fn write_outside_specs_is_blocked_and_reported_to_model() {
        let root = tmp_root("chat-scope");
        let state = state_for(&root, config_for(&root, true));
        let (scripted, dynb) = backend(vec![
            calls(&[("c1", "write_sdd_artifacts", r#"{"path":"src/a.rs","content":"x"}"#)]),
            text("Não posso escrever ali."),
        ]);
        let (status, body) = turn(&state, PROJECT_ID, "altera código", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["draft_updated"], false);
        assert!(!root.join("src/a.rs").exists());
        assert!(has_tool_msg(&scripted.requests()[1], "c1", "erro:"));
    }

    #[tokio::test]
    async fn history_is_resent_and_truncated_to_recent_turns() {
        let root = tmp_root("chat-history");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![text("resposta 1")]);
        turn(&state, PROJECT_ID, "pergunta 1", dynb, CHAT_TIMEOUT).await;
        let (second, dynb) = backend(vec![text("resposta 2")]);
        turn(&state, PROJECT_ID, "pergunta 2", dynb, CHAT_TIMEOUT).await;
        let msgs = &second.requests()[0].messages;
        // Spec 122 T05: system, histórico, estado do projeto e a mensagem do turno.
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[1].content, "pergunta 1");
        assert_eq!(msgs[2].role, ChatRole::Assistant);
        assert_eq!(msgs[2].content, "resposta 1");
        assert!(msgs[3].content.starts_with("ESTADO ATUAL DO PROJETO"), "{}", msgs[3].content);
        assert_eq!(msgs[4].content, "pergunta 2");

        // Histórico longo: só os 40 turnos mais recentes seguem para o modelo.
        let mut long = String::new();
        for i in 0..50 {
            let t = ChatTurn {
                role: TurnRole::User,
                text: format!("antigo {i}"),
                ts: 1,
                turn_id: None,
                internal: false,
                attachments: Vec::new(),
                notice: None,
            };
            long.push_str(&serde_json::to_string(&t).expect("json"));
            long.push('\n');
        }
        fs::write(root.join(CHAT_FILE_REL), long).expect("escrever histórico");
        let (third, dynb) = backend(vec![text("ok")]);
        turn(&state, PROJECT_ID, "atual", dynb, CHAT_TIMEOUT).await;
        let msgs = &third.requests()[0].messages;
        assert_eq!(msgs.len(), 1 + 1 + MAX_HISTORY_TURNS + 1);
        assert_eq!(msgs[1].content, "antigo 10");
    }

    /// Spec 127 T01: os avisos do sistema ficam fora do contexto do Arquiteto e não ocupam vaga do
    /// limite; o histórico exibido os mantém.
    #[test]
    fn read_history_leaves_system_notices_out_of_the_model_context() {
        let root = tmp_root("chat-read-system");
        let mut turns = Vec::new();
        for i in 0..MAX_HISTORY_TURNS {
            let role = if i % 2 == 0 { TurnRole::User } else { TurnRole::Architect };
            turns.push(ChatTurn { role, text: format!("real {i}"), ts: 1, turn_id: None, internal: false, attachments: Vec::new(), notice: None });
            let notice = Some(connection_lost_notice(None));
            turns.push(ChatTurn { role: TurnRole::System, text: "aviso".into(), ts: 1, turn_id: None, internal: false, attachments: Vec::new(), notice });
        }
        assert!(append_turns(&root, &turns));
        let context = read_history(&root);
        assert_eq!(context.len(), MAX_HISTORY_TURNS);
        assert!(context.iter().all(|t| t.role != TurnRole::System));
        assert_eq!(context[0].text, "real 0", "o turno real mais antigo continua no contexto");
        let visible = read_visible_history(&root);
        assert!(visible.iter().any(|t| t.role == TurnRole::System));
    }

    #[test]
    fn read_history_skips_corrupt_lines_and_reads_only_the_tail() {
        let root = tmp_root("chat-read");
        fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
        let good = r#"{"role":"user","text":"válido","ts":1}"#;
        fs::write(root.join(CHAT_FILE_REL), format!("{good}\nlixo{{\n{good}\n")).expect("w");
        assert_eq!(read_history(&root).len(), 2);

        let filler = "x".repeat(1000);
        let mut big = String::new();
        for _ in 0..2000 {
            big.push_str(&filler);
            big.push('\n');
        }
        big.push_str(good);
        big.push('\n');
        fs::write(root.join(CHAT_FILE_REL), big).expect("w");
        let turns = read_history(&root);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].text, "válido");
    }

    #[tokio::test]
    async fn symlinked_history_is_neither_read_nor_written() {
        let root = tmp_root("chat-link");
        let outside_dir = tmp_root("chat-link-target");
        let outside = outside_dir.join("alvo.jsonl");
        let original = "{\"role\":\"user\",\"text\":\"SEGREDO-HIST\",\"ts\":1}\n";
        fs::write(&outside, original).expect("alvo");
        fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
        symlink(&outside, root.join(CHAT_FILE_REL)).expect("symlink");
        let state = state_for(&root, config_for(&root, true));
        let (scripted, dynb) = backend(vec![text("ok")]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let sent = format!("{:?}", scripted.requests()[0].messages);
        assert!(!sent.contains("SEGREDO-HIST"));
        assert_eq!(fs::read_to_string(&outside).expect("r"), original);
    }

    #[tokio::test]
    async fn api_key_in_reply_and_message_is_redacted() {
        let root = tmp_root("chat-redact");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let (_, dynb) = backend(vec![text(&format!("use {SECRET} aqui"))]);
        let message = format!("minha chave é {SECRET}");
        let (status, body) = turn(&state, PROJECT_ID, &message, dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let reply = body["reply"].as_str().expect("texto");
        assert!(!reply.contains(SECRET), "{reply}");
        assert!(reply.contains("***"));
        let stored = fs::read_to_string(root.join(CHAT_FILE_REL)).expect("histórico");
        assert!(!stored.contains(SECRET));
        for ev in drain_events(&mut rx) {
            let raw = serde_json::to_string(&ev).expect("json");
            assert!(!raw.contains(SECRET), "{raw}");
        }
    }

    #[tokio::test]
    async fn concurrent_turn_for_same_project_gets_409() {
        let root = tmp_root("chat-busy");
        let state = state_for(&root, config_for(&root, true));
        let gate = Arc::new(Gate {
            entered: Notify::new(),
            release: Notify::new(),
        });
        let dynb: Arc<dyn ChatBackend> = gate.clone();
        let first_state = state.clone();
        let first = tokio::spawn(async move {
            turn(&first_state, PROJECT_ID, "primeira", Some(dynb), CHAT_TIMEOUT).await
        });
        gate.entered.notified().await;

        let (status, body) = turn(&state, PROJECT_ID, "segunda", None, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");

        gate.release.notify_one();
        let (first_status, first_body) = first.await.expect("join");
        assert_eq!(first_status, StatusCode::OK, "{first_body}");
        // A reserva é liberada ao terminar: novo turno volta a funcionar.
        let (_, dynb) = backend(vec![text("de novo")]);
        let (status, _) = turn(&state, PROJECT_ID, "terceira", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn turn_survives_client_disconnect_and_keeps_reservation() {
        let root = tmp_root("chat-abort");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let gate = Arc::new(Gate {
            entered: Notify::new(),
            release: Notify::new(),
        });
        let dynb: Arc<dyn ChatBackend> = gate.clone();
        let first_state = state.clone();
        let first = tokio::spawn(async move {
            turn(&first_state, PROJECT_ID, "primeira", Some(dynb), CHAT_TIMEOUT).await
        });
        gate.entered.notified().await;
        first.abort(); // cliente desconecta
        let _ = first.await;

        let (status, body) = turn(&state, PROJECT_ID, "segunda", None, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");

        gate.release.notify_one();
        // `wait_active_runs` conta runs de TODOS os testes do processo (contador global); aqui se
        // espera só o efeito deste turno: o fim do turno publicado (chat_turn_ended).
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while !events.iter().any(|e: &ApiEvent| e.event_type() == "chat_turn_ended") {
            assert!(tokio::time::Instant::now() < deadline, "turno não terminou: {events:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
            events.extend(drain_events(&mut rx));
        }
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(events.iter().any(|e| e.event_type() == "agent_message"));
    }

    #[tokio::test]
    async fn invalid_messages_are_rejected_before_the_model() {
        let root = tmp_root("chat-msg");
        let state = state_for(&root, config_for(&root, true));
        let bad = [
            String::new(),
            "   \n\t".to_string(),
            "a".repeat(MAX_CHAT_MESSAGE_CHARS + 1),
            "oi\u{7}".to_string(),
            "oi\r".to_string(),
        ];
        for message in bad {
            let (scripted, dynb) = backend(Vec::new());
            let (status, body) = turn(&state, PROJECT_ID, &message, dynb, CHAT_TIMEOUT).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert!(scripted.requests().is_empty());
            assert!(!body.to_string().contains(message.trim()) || message.trim().is_empty());
        }
        // Exatamente no limite é aceito pela validação (o fake sem resposta falha depois).
        let (_, dynb) = backend(vec![text("limite")]);
        let at_limit = "a".repeat(MAX_CHAT_MESSAGE_CHARS);
        let (status, _) = turn(&state, PROJECT_ID, &at_limit, dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn unconfigured_architect_or_unusable_key_is_412() {
        // Arquiteto apontando para conexão inexistente: a prontidão `chat` (spec 093 T02) recusa
        // com 412 antes do 422 de "não configurado", e nada é gravado.
        let root = tmp_root("chat-noconf");
        let state = state_for(&root, config_for(&root, false));
        let (status, body) = turn(&state, PROJECT_ID, "oi", None, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(body, json!({"error": "prereq_missing", "missing": ["agent_architect"], "code": "prereq_missing", "params": {"count": 1}}));
        assert!(chat_lines(&root).is_empty(), "nada persistido");
        // Conexão e modelo presentes, mas chave não resolvível: a prontidão também recusa com 412
        // (spec 093 T12), com o item da chave e nada gravado.
        let mut cfg = config_for(&root, true);
        cfg.connections.push(Connection {
            id: "envkey".into(),
            name: "Sem chave".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: "env:NANTS_CHAT_TEST_UNSET_KEY_5151".into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
            base_url_override: None,
        });
        cfg.agents.architect.connection_id = "envkey".into();
        let state = state_for(&root, cfg);
        let (status, body) = turn(&state, PROJECT_ID, "oi", None, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(body, json!({"error": "prereq_missing", "missing": ["connection_key:envkey"], "code": "prereq_missing", "params": {"count": 1}}));
        assert!(!body.to_string().contains("NANTS_CHAT_TEST_UNSET_KEY_5151"));
        assert!(chat_lines(&root).is_empty(), "nada persistido");
    }

    #[tokio::test]
    async fn model_that_never_answers_times_out_with_504() {
        let root = tmp_root("chat-timeout");
        let state = state_for(&root, config_for(&root, true));
        let dynb: Arc<dyn ChatBackend> = Arc::new(Hang);
        let (status, body) =
            turn(&state, PROJECT_ID, "oi", Some(dynb), Duration::from_millis(50)).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(body["code"], TIMEOUT_CODE);
        assert!(body.get("draft_saved").is_none());
        // A mensagem do usuário foi persistida no início; o corte virou um turno curto.
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0]["role"], "user");
        assert_eq!(lines[0]["text"], "oi");
        assert_eq!(lines[1]["role"], "architect");
        assert_eq!(lines[0]["turn_id"], lines[1]["turn_id"]);
    }

    /// Fake que espera `delay` antes de delegar ao roteiro (cada chamada é progresso).
    struct Slow {
        delay: Duration,
        inner: Arc<Scripted>,
    }

    impl ChatBackend for Slow {
        fn complete<'a>(
            &'a self,
            req: &'a ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                self.inner.complete(req).await
            })
        }
    }

    fn slow(delay_ms: u64, replies: Vec<Reply>) -> Option<Arc<dyn ChatBackend>> {
        let inner = Scripted::new(replies);
        Some(Arc::new(Slow { delay: Duration::from_millis(delay_ms), inner }))
    }

    fn limits(idle_ms: u64, cap_ms: u64) -> TurnLimits {
        TurnLimits { idle: Duration::from_millis(idle_ms), cap: Duration::from_millis(cap_ms) }
    }

    #[tokio::test]
    async fn continuous_progress_beyond_idle_deadline_completes() {
        let root = tmp_root("chat-progress");
        let state = state_for(&root, config_for(&root, true));
        let replies = vec![write_spec_n(1), write_spec_n(2), write_spec_n(3), text("pronto")];
        // 4 chamadas de 80 ms (320 ms) com prazo de inatividade de 150 ms.
        let (status, body) =
            turn(&state, PROJECT_ID, "gera", slow(80, replies), limits(150, 10_000)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["reply"], "pronto");
    }

    #[tokio::test]
    async fn stalled_turn_expires_by_idleness() {
        let root = tmp_root("chat-stalled");
        let state = state_for(&root, config_for(&root, true));
        let replies = vec![text("tarde")];
        let (status, body) =
            turn(&state, PROJECT_ID, "oi", slow(600, replies), limits(100, 10_000)).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(body["code"], TIMEOUT_CODE);
    }

    /// O turno como `run_turn_body` o monta: backend com `ProgressBackend` e atividade com
    /// `ProgressSink`, mas com o heartbeat do escopo injetado (menor que o prazo de inatividade).
    fn progress_parts(heartbeat_ms: u64) -> (Arc<Progress>, Arc<dyn ChatBackend>, crate::agents::activity::ActivityScope) {
        let progress = Progress::new();
        let backend: Arc<dyn ChatBackend> = Arc::new(ProgressBackend { inner: Arc::new(Hang), progress: Arc::clone(&progress) });
        let sink = Arc::new(ProgressSink {
            inner: Arc::new(crate::agents::activity::NullSink),
            progress: Arc::clone(&progress),
        });
        let scope = crate::agents::activity::ActivityScope::new(sink, "architect", None, None)
            .with_heartbeat(Duration::from_millis(heartbeat_ms));
        (progress, backend, scope)
    }

    fn silent_request() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            effort: None,
            response_json: false,
            web_search: false,
            cache_key: None,
            web_search_max_uses: None,
        }
    }

    /// Spec 114 T04 (F-01): os sinais da chamada ao modelo (`model`, o heartbeat dela e
    /// `model_end`) não são progresso do turno. Uma chamada muda é cortada pelo prazo de
    /// inatividade, não pelo teto absoluto, mesmo com o heartbeat do escopo menor que o prazo.
    #[tokio::test]
    async fn silent_model_call_expires_by_idleness_despite_model_heartbeats() {
        let (progress, backend, scope) = progress_parts(20);
        let run = scope.begin();
        let req = silent_request();
        let on_delta = |d| run.delta(d);
        let started = std::time::Instant::now();
        let out = with_idle_deadline(run.track_model(backend.complete_stream(&req, &on_delta)), limits(150, 3_000), &progress).await;
        assert!(out.is_none());
        let took = started.elapsed();
        assert!(took < Duration::from_millis(1_500), "cortada só pelo teto: {took:?}");
    }

    /// O heartbeat de comando continua renovando o prazo (só o teto corta o comando longo).
    #[tokio::test]
    async fn command_heartbeat_still_renews_idleness() {
        let (progress, _, scope) = progress_parts(20);
        let run = scope.begin();
        let started = std::time::Instant::now();
        let out = with_idle_deadline(run.track_command("npm test", std::future::pending::<()>()), limits(150, 700), &progress).await;
        assert!(out.is_none());
        assert!(started.elapsed() >= Duration::from_millis(650), "{:?}", started.elapsed());
    }

    /// Fake que, antes de responder, passa `waits` intervalos de `every` avisando a espera de
    /// limite do provedor, como o cliente faz (spec 089): só os avisos marcam progresso.
    struct Throttled {
        every: Duration,
        waits: usize,
        inner: Arc<Scripted>,
    }

    impl ChatBackend for Throttled {
        fn complete<'a>(
            &'a self,
            req: &'a ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
            self.inner.complete(req)
        }

        fn complete_stream<'a>(
            &'a self,
            req: &'a ChatRequest,
            on_delta: &'a (dyn Fn(StreamDelta) + Send + Sync),
        ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
            Box::pin(async move {
                for k in 0..self.waits {
                    on_delta(StreamDelta::Retry(format!(
                        "provedor no limite (no_capacity); aguardando {} s (tentativa 1/6)",
                        self.waits - k
                    )));
                    tokio::time::sleep(self.every).await;
                }
                self.inner.complete(req).await
            })
        }
    }

    #[tokio::test]
    async fn provider_throttle_notices_count_as_progress() {
        let root = tmp_root("chat-throttle");
        let state = state_for(&root, config_for(&root, true));
        // 8 avisos a cada 60 ms (~480 ms de espera) com prazo de inatividade de 150 ms.
        let inner = Scripted::new(vec![text("pronto")]);
        let dynb: Option<Arc<dyn ChatBackend>> =
            Some(Arc::new(Throttled { every: Duration::from_millis(60), waits: 8, inner }));
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, limits(150, 10_000)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["reply"], "pronto");
        // O teto absoluto continua valendo durante a espera.
        let inner = Scripted::new(vec![text("tarde")]);
        let dynb: Option<Arc<dyn ChatBackend>> =
            Some(Arc::new(Throttled { every: Duration::from_millis(60), waits: 20, inner }));
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, limits(150, 300)).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(body["code"], TIMEOUT_CODE);
    }

    #[tokio::test]
    async fn absolute_cap_is_enforced_despite_progress() {
        let root = tmp_root("chat-cap");
        let state = state_for(&root, config_for(&root, true));
        let replies: Vec<Reply> = (0..40).map(write_spec_n).collect();
        let (status, _) =
            turn(&state, PROJECT_ID, "oi", slow(40, replies), limits(150, 300)).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    }

    /// Roteiro que responde a primeira chamada e trava nas seguintes.
    struct Stall(Arc<Scripted>, std::sync::atomic::AtomicUsize);

    impl ChatBackend for Stall {
        fn complete<'a>(
            &'a self,
            req: &'a ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
            Box::pin(async move {
                if self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1 {
                    std::future::pending::<()>().await;
                }
                self.0.complete(req).await
            })
        }
    }

    #[tokio::test]
    async fn timeout_after_spec_written_reports_draft_saved() {
        let root = tmp_root("chat-draft-saved");
        let state = state_for(&root, config_for(&root, true));
        let inner = Scripted::new(vec![write_spec_call(), text("nunca")]);
        let dynb: Option<Arc<dyn ChatBackend>> =
            Some(Arc::new(Stall(inner, std::sync::atomic::AtomicUsize::new(0))));
        let (status, body) = turn(&state, PROJECT_ID, "gera", dynb, limits(150, 10_000)).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert!(root.join(".sdd/specs/001-exemplo/spec.md").exists(), "spec ausente");
        assert_eq!(body["draft_saved"], true);
        assert_eq!(body["reason"], "draft_saved");
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0]["role"], "user");
        assert!(lines[1]["text"].as_str().unwrap_or("").contains("plano foi salvo"));
        // O GET não duplica a mensagem do usuário.
        let visible = read_visible_history(&root);
        assert_eq!(visible.iter().filter(|t| matches!(t.role, TurnRole::User)).count(), 1);
    }

    #[tokio::test]
    async fn user_message_is_not_duplicated_when_turn_completes() {
        let root = tmp_root("chat-no-dup");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![text("ok")]);
        let (status, _) = turn(&state, PROJECT_ID, "uma vez", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK);
        let users = chat_lines(&root).iter().filter(|l| l["role"] == "user").count();
        assert_eq!(users, 1);
    }

    #[tokio::test]
    async fn backend_failure_is_fixed_code() {
        let root = tmp_root("chat-backend");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(Vec::new());
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["params"]["provider"], "provider_connect_failed");
        assert_eq!(body["code"], CODE_PROVIDER_FAILED);
        // Spec 133 T11: com a evidência do papel, da conexão e do modelo.
        assert_eq!(
            body["params"],
            json!({"provider": "provider_connect_failed", "role": "architect", "model": "gpt-4o", "connection_id": "literal",
                   "connection_name": "Conexão literal", "connection_kind": "api_key", "category": "transport"})
        );
    }

    #[tokio::test]
    async fn empty_final_reply_is_502_provider_bad_response() {
        let root = tmp_root("chat-empty-reply");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![text("   ")]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["params"]["provider"], "provider_bad_response");
        // Spec 119: nenhum rótulo em pt-BR no corpo; o motivo vai neutro nos `params`.
        assert!(body.get("detail").is_none(), "{body}");
        // Spec 102: `code` estável e `params` com a causa e o motivo.
        assert_eq!(body["code"], CODE_PROVIDER_FAILED);
        assert_eq!((&body["params"]["provider"], &body["params"]["kind"]), (&json!("provider_bad_response"), &json!("empty")));
        // Spec 133 T11: a mensagem do usuário e a falha, como mensagem do sistema com código.
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["notice"]["code"], PROVIDER_FAILED_NOTICE_CODE);
    }

    #[tokio::test]
    async fn provider_http_400_is_502_with_status_code_only() {
        let root = tmp_root("chat-http-400");
        let state = state_for(&root, config_for(&root, true));
        let remote = format!("erro {SECRET} em https://api.exemplo.test/v1/chat/completions");
        let (_, dynb) = backend(vec![Err(LlmError::Http {
            status: 400,
            body_snippet: remote,
            code: None,
        })]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["params"]["provider"], "provider_http_400");
        let text = body.to_string();
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains("api.exemplo"), "{text}");
        assert!(!text.contains("chat/completions"), "{text}");
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2, "a mensagem do usuário e a falha");
        assert!(!lines[1].to_string().contains(SECRET) && !lines[1].to_string().contains("api.exemplo"), "{}", lines[1]);
    }

    /// Servidor local que responde, em ordem, cada resposta HTTP bruta (uma por conexão).
    async fn raw_server(responses: Vec<String>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("endereço");
        tokio::spawn(async move {
            for raw in responses {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let Ok(n) = sock.read(&mut tmp).await else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                        let len = head
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
                            .unwrap_or(0);
                        if rest.len() >= len {
                            break;
                        }
                    }
                }
                let _ = sock.write_all(raw.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}/v1")
    }

    /// Spec 133 T11 (D5): o 404 do provedor na conversa (modelo inexistente) volta com os `params`
    /// completos da evidência (papel, conexão, modelo, status, tipo, código e categoria), sem a
    /// mensagem do provedor, e fica no histórico como mensagem do sistema com o código.
    #[tokio::test]
    async fn chat_404_carries_the_full_evidence_and_stays_in_the_history() {
        let root = tmp_root("chat-404-evidence");
        std::fs::create_dir_all(root.join(".sdd/specs")).expect("sdd");
        let body = r#"{"error":{"code":404,"message":"models/modelo-x is not found for API version v1","status":"NOT_FOUND"}}"#;
        let raw = format!(
            "HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\nx-request-id: req-404\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut cfg = config_for(&root, true);
        cfg.connections[0].base_url = raw_server(vec![raw]).await;
        let state = state_for(&root, cfg);
        let (status, resp) = turn(&state, PROJECT_ID, "oi", None, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{resp}");
        assert_eq!(resp["code"], CODE_PROVIDER_FAILED);
        assert_eq!(resp["reason"], FAILURE_RECORDED_REASON, "{resp}");
        let params = resp["params"].clone();
        assert_eq!(params["provider"], "provider_http_404", "{params}");
        assert_eq!((&params["role"], &params["model"]), (&json!("architect"), &json!("gpt-4o")), "{params}");
        assert_eq!((&params["connection_id"], &params["connection_name"]), (&json!("literal"), &json!("Conexão literal")), "{params}");
        assert_eq!((&params["status"], &params["category"]), (&json!(404), &json!("bad_request")), "{params}");
        assert_eq!((&params["error_type"], &params["error_code"]), (&json!("not_found"), &json!("404")), "{params}");
        assert_eq!(params["request_id"], "req-404", "{params}");
        assert!(!resp.to_string().contains("is not found"), "a mensagem do provedor não vai à tela: {resp}");
        // A mesma evidência no registro do projeto.
        let rows = super::super::provider_errors::load(&root, None);
        assert_eq!(rows.last().and_then(|r| r.error_type.clone()).as_deref(), Some("not_found"));
        // No histórico: a mensagem do sistema com o código, os mesmos `params` e a reserva no idioma do projeto.
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2, "mensagem do usuário + falha");
        let notice = &lines[1];
        assert_eq!(notice["role"], "system");
        assert_eq!(notice["turn_id"], lines[0]["turn_id"]);
        assert_eq!(notice["notice"]["code"], PROVIDER_FAILED_NOTICE_CODE);
        assert_eq!(notice["notice"]["params"], params);
        let lang = state.project_lang(PROJECT_ID).get();
        assert_eq!(notice["text"], crate::i18n::tr(lang, "system.chat.provider_failed", &[("provider", "provider_http_404")]));
        // O GET devolve o `code` e os `params`, para a tela traduzir.
        let (status, hist) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK);
        let last = hist["turns"].as_array().and_then(|t| t.last().cloned()).expect("falha");
        assert_eq!((&last["role"], &last["code"]), (&json!("system"), &json!(PROVIDER_FAILED_NOTICE_CODE)));
        assert_eq!(last["params"], params);
        // A falha fica fora do contexto do próximo turno do modelo.
        assert!(read_history(&root).iter().all(|t| t.role != TurnRole::System));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Backend que troca o modelo e a conexão do Arquiteto na configuração durante a chamada e
    /// depois falha com 400 (spec 133 T01, F-01).
    struct SwitchThenFail(AppState);

    impl ChatBackend for SwitchThenFail {
        fn complete<'a>(
            &'a self,
            _req: &'a crate::agents::llm_client::ChatRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<crate::agents::llm_client::ChatResponse, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                {
                    let mut cfg = self.0.config().write().await;
                    cfg.agents.architect.model = "modelo-trocado".into();
                    for conn in &mut cfg.connections {
                        conn.name = "Conexão trocada".into();
                    }
                }
                Err(LlmError::Http { status: 400, body_snippet: String::new(), code: None })
            })
        }
    }

    /// Spec 133 T01 (F-01): a falha da conversa grava a conexão e o modelo do disparo, mesmo que a
    /// configuração mude durante o turno.
    #[tokio::test]
    async fn chat_failure_records_the_connection_and_model_of_the_dispatch() {
        let root = tmp_root("chat-evidence-switch");
        std::fs::create_dir_all(root.join(".sdd/specs")).expect("sdd");
        let state = state_for(&root, config_for(&root, true));
        let dynb: Arc<dyn ChatBackend> = Arc::new(SwitchThenFail(state.clone()));
        let (status, _) = turn(&state, PROJECT_ID, "oi", Some(dynb), CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(state.config().read().await.agents.architect.model, "modelo-trocado", "a troca aconteceu");
        let rows = super::super::provider_errors::load(&root, None);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!((row.role.as_str(), row.stage.as_deref()), ("architect", Some("chat")));
        assert_eq!(row.model, "gpt-4o");
        assert_eq!((row.connection_id.as_deref(), row.connection_name.as_deref()), (Some("literal"), Some("Conexão literal")));
        assert_eq!((row.provider.as_str(), row.category.as_deref(), row.retries), ("provider_http_400", Some("bad_request"), 0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn provider_failures_map_to_fixed_502_codes() {
        let cases: Vec<(Reply, &str)> = vec![
            (Err(LlmError::Http { status: 401, body_snippet: SECRET.into(), code: None }), "provider_http_401"),
            (Err(LlmError::Http { status: 404, body_snippet: String::new(), code: None }), "provider_http_404"),
            (Err(LlmError::Http { status: 429, body_snippet: String::new(), code: None }), "provider_http_429"),
            (Err(LlmError::Http { status: 500, body_snippet: String::new(), code: None }), "provider_http_500"),
            (Err(LlmError::Timeout), "provider_timeout"),
            (Err(LlmError::Transport(SECRET.into())), "provider_connect_failed"),
            (
                Err(LlmError::Parse(crate::agents::llm_client::BadReason::with_code(
                    crate::agents::llm_client::BadKind::Incomplete,
                    Some(&format!("{SECRET}: texto livre")),
                ))),
                "provider_bad_response",
            ),
            (Err(LlmError::Config(SECRET.into())), "provider_invalid_config"),
        ];
        for (i, (reply, code)) in cases.into_iter().enumerate() {
            let root = tmp_root(&format!("chat-provider-{i}"));
            let state = state_for(&root, config_for(&root, true));
            let (_, dynb) = backend(vec![reply]);
            let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "caso {code}");
            assert_eq!(body["params"]["provider"], code);
            assert!(!body.to_string().contains(SECRET), "caso {code}: {body}");
            // Spec 119: nenhum caso leva `detail` em pt-BR; a resposta ruim leva o `kind` (o código
            // com caractere inválido é omitido).
            assert!(body.get("detail").is_none(), "caso {code}: {body}");
            if code == "provider_bad_response" {
                assert_eq!(body["params"]["kind"], "incomplete", "{body}");
                assert!(body["params"].get("provider_code").is_none(), "{body}");
            }
            // Spec 133 T11: toda falha leva o papel, a conexão e o modelo.
            assert_eq!((&body["params"]["role"], &body["params"]["model"]), (&json!("architect"), &json!("gpt-4o")), "caso {code}: {body}");
        }
    }

    #[tokio::test]
    async fn invalid_id_unknown_project_and_bad_body_over_http() {
        let root = tmp_root("chat-http");
        let state = state_for(&root, config_for(&root, true));
        let ok_body = r#"{"message":"oi"}"#;
        let (status, _) = post_raw(&state, "/api/projects/bad.id/chat", ok_body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = post_raw(&state, "/api/projects/nao-existe/chat", ok_body).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = post_raw(&state, "/api/projects/proj-chat/chat", "{}").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = post_raw(&state, "/api/projects/proj-chat/chat", "{nao json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    fn is_hex32(id: &str) -> bool {
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    #[tokio::test]
    async fn turn_returns_turn_id_and_agent_message_carries_the_same_id() {
        let root = tmp_root("chat-turn-id");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let (_, dynb) = backend(vec![text("Olá!")]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let turn_id = body["turn_id"].as_str().expect("turn_id");
        assert!(is_hex32(turn_id), "{turn_id}");
        let events = drain_events(&mut rx);
        let msg = events
            .iter()
            .find(|e| e.event_type() == "agent_message")
            .expect("agent_message");
        assert_eq!(msg.payload["turn_id"], turn_id);
        // Contrato antigo preservado: campos agent e text continuam presentes.
        assert_eq!(msg.payload["agent"], "architect");
        assert_eq!(msg.payload["text"], "Olá!");
    }

    #[tokio::test]
    async fn each_turn_gets_a_distinct_turn_id() {
        let root = tmp_root("chat-turn-distinct");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![text("um")]);
        let (_, first) = turn(&state, PROJECT_ID, "primeira", dynb, CHAT_TIMEOUT).await;
        let (_, dynb) = backend(vec![text("dois")]);
        let (_, second) = turn(&state, PROJECT_ID, "segunda", dynb, CHAT_TIMEOUT).await;
        let a = first["turn_id"].as_str().expect("turn_id");
        let b = second["turn_id"].as_str().expect("turn_id");
        assert!(is_hex32(a) && is_hex32(b));
        assert_ne!(a, b);
    }

    #[test]
    fn appended_turns_are_one_json_line_each() {
        let root = tmp_root("chat-append");
        let turns = [
            ChatTurn {
                role: TurnRole::User,
                text: "a\nb".into(),
                ts: 7,
                turn_id: None,
                internal: false,
                attachments: Vec::new(),
                notice: None,
            },
            ChatTurn {
                role: TurnRole::Architect,
                text: "c".into(),
                ts: 8,
                turn_id: None,
                internal: false,
                attachments: Vec::new(),
                notice: None,
            },
        ];
        assert!(append_turns(&root, &turns));
        assert!(append_turns(&root, &turns[..1]));
        let raw = fs::read_to_string(root.join(CHAT_FILE_REL)).expect("r");
        assert_eq!(raw.lines().count(), 3);
        assert_eq!(read_history(&root).len(), 3);
        assert_eq!(read_history(&root)[0].text, "a\nb");
    }

    async fn get_history(state: &AppState, id: &str, host: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!("/api/projects/{id}/chat"))
            .header(header::HOST, host)
            .body(Body::empty())
            .expect("requisição");
        let resp = router(state.clone()).oneshot(req).await.expect("resposta");
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn get_history_returns_latest_turns_in_chronological_order() {
        let root = tmp_root("chat-get-order");
        let state = state_for(&root, config_for(&root, true));
        let turns: Vec<ChatTurn> = (0..45u64)
            .map(|i| ChatTurn {
                role: if i % 2 == 0 { TurnRole::User } else { TurnRole::Architect },
                text: format!("t{i}"),
                ts: i,
                turn_id: None,
                internal: false,
                attachments: Vec::new(),
                notice: None,
            })
            .collect();
        assert!(append_turns(&root, &turns));
        let (status, body) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let list = body["turns"].as_array().expect("turns");
        // Limite: só os MAX_HISTORY_TURNS mais recentes, do mais antigo para o mais novo.
        assert_eq!(list.len(), MAX_HISTORY_TURNS);
        assert_eq!(list[0]["text"], "t5");
        assert_eq!(list[0]["role"], "architect");
        assert_eq!(list[0]["ts"], 5);
        assert_eq!(list[MAX_HISTORY_TURNS - 1]["text"], "t44");
        assert_eq!(list[MAX_HISTORY_TURNS - 1]["role"], "user");
        for pair in list.windows(2) {
            assert!(pair[0]["ts"].as_u64() < pair[1]["ts"].as_u64());
        }
    }

    #[tokio::test]
    async fn get_history_redacts_secret_written_in_clear() {
        let root = tmp_root("chat-get-redact");
        let state = state_for(&root, config_for(&root, true));
        // Gravado antes de o segredo existir: o arquivo guarda o valor em claro.
        let turns = [ChatTurn {
            role: TurnRole::Architect,
            text: format!("chave {SECRET} aqui"),
            ts: 3,
            turn_id: None,
            internal: false,
            attachments: Vec::new(),
            notice: None,
        }];
        assert!(append_turns(&root, &turns));
        let (status, body) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let raw = body.to_string();
        assert!(!raw.contains(SECRET), "{raw}");
        assert!(body["turns"][0]["text"].as_str().expect("texto").contains("***"));
    }

    #[tokio::test]
    async fn get_history_skips_invalid_lines() {
        let root = tmp_root("chat-get-lines");
        let state = state_for(&root, config_for(&root, true));
        fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
        let good = r#"{"role":"user","text":"válido","ts":1}"#;
        fs::write(root.join(CHAT_FILE_REL), format!("{good}\nlixo{{\n{{}}\n{good}\n")).expect("w");
        let (status, body) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["turns"].as_array().expect("turns").len(), 2);
    }

    #[tokio::test]
    async fn get_history_without_file_is_empty_list() {
        let root = tmp_root("chat-get-missing");
        let state = state_for(&root, config_for(&root, true));
        let (status, body) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"turns": []}));
    }

    #[tokio::test]
    async fn get_history_symlink_is_empty_and_never_touched() {
        let root = tmp_root("chat-get-link");
        let outside_dir = tmp_root("chat-get-link-target");
        let outside = outside_dir.join("alvo.jsonl");
        let original = "{\"role\":\"user\",\"text\":\"SEGREDO-GET\",\"ts\":1}\n";
        fs::write(&outside, original).expect("alvo");
        fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
        symlink(&outside, root.join(CHAT_FILE_REL)).expect("symlink");
        let state = state_for(&root, config_for(&root, true));
        let (status, body) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"turns": []}));
        assert_eq!(fs::read_to_string(&outside).expect("r"), original);
    }

    #[tokio::test]
    async fn get_history_unknown_project_is_404_and_bad_id_is_422() {
        let root = tmp_root("chat-get-errors");
        let state = state_for(&root, config_for(&root, true));
        let (status, body) = get_history(&state, "nao-existe", HOST).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "project_not_found");
        let (status, _) = get_history(&state, "bad.id", HOST).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn turn_id_is_persisted_on_both_turns_and_returned_by_get() {
        let root = tmp_root("chat-turnid-persist");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![text("Entendido.")]);
        let (status, body) = turn(&state, PROJECT_ID, "Quero X", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let turn_id = body["turn_id"].as_str().expect("turn_id").to_string();
        let lines = chat_lines(&root);
        assert_eq!(lines[0]["turn_id"], turn_id);
        assert_eq!(lines[1]["turn_id"], turn_id);
        let (_, history) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(history["turns"][0]["turn_id"], turn_id);
        assert_eq!(history["turns"][1]["turn_id"], turn_id);
    }

    #[tokio::test]
    async fn legacy_line_without_turn_id_loads_without_the_field() {
        let root = tmp_root("chat-turnid-legacy");
        let state = state_for(&root, config_for(&root, true));
        fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
        fs::write(root.join(CHAT_FILE_REL), "{\"role\":\"architect\",\"text\":\"antiga\",\"ts\":1}\n").expect("w");
        let (status, body) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["turns"][0]["text"], "antiga");
        assert!(body["turns"][0].get("turn_id").is_none(), "{body}");
    }

    #[tokio::test]
    async fn invalid_turn_id_is_dropped_but_turn_is_kept() {
        let root = tmp_root("chat-turnid-invalid");
        let state = state_for(&root, config_for(&root, true));
        fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
        let long = "a".repeat(65);
        let bad = format!(
            "{{\"role\":\"user\",\"text\":\"um\",\"ts\":1,\"turn_id\":\"NOT-HEX\"}}\n\
             {{\"role\":\"architect\",\"text\":\"dois\",\"ts\":2,\"turn_id\":\"{long}\"}}\n"
        );
        fs::write(root.join(CHAT_FILE_REL), bad).expect("w");
        let (status, body) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let list = body["turns"].as_array().expect("turns");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["text"], "um");
        assert_eq!(list[1]["text"], "dois");
        assert!(list[0].get("turn_id").is_none());
        assert!(list[1].get("turn_id").is_none());
    }

    #[tokio::test]
    async fn get_history_with_invalid_host_is_403() {
        let root = tmp_root("chat-get-host");
        let state = state_for(&root, config_for(&root, true));
        assert!(append_turns(&root, &[ChatTurn { role: TurnRole::User, text: "oi".into(), ts: 1, turn_id: None, internal: false, attachments: Vec::new(), notice: None }]));
        let (status, body) = get_history(&state, PROJECT_ID, "evil.example:3000").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(!body.to_string().contains("oi"));
    }

    const INTERNAL_MSG: &str =
        "Defina a stack e o comando de testes (test_command) da spec e atualize-a para aprovação.";

    /// Turno interno (`internal: true`) executado direto no núcleo.
    async fn internal_turn(
        state: &AppState,
        message: &str,
        backend: Option<Arc<dyn ChatBackend>>,
    ) -> (StatusCode, Value) {
        let body = json!({ "message": message, "internal": true }).to_string();
        match run_turn(state, PROJECT_ID, body.as_bytes(), CHAT_TIMEOUT, backend).await {
            Ok(resp) => {
                let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
                (StatusCode::OK, serde_json::from_slice(&bytes).expect("json"))
            }
            Err(e) => (e.status(), json!({ "error": e.message() })),
        }
    }

    #[tokio::test]
    async fn internal_turn_persists_user_flag_and_normal_architect_turn() {
        let root = tmp_root("chat-internal-persist");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![text("Plano atualizado.")]);
        let (status, body) = internal_turn(&state, INTERNAL_MSG, dynb).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["role"], "user");
        assert_eq!(lines[0]["internal"], true);
        assert_eq!(lines[1]["role"], "architect");
        assert!(lines[1].get("internal").is_none(), "{}", lines[1]);
        assert_eq!(body["turn_id"], lines[1]["turn_id"]);
    }

    #[tokio::test]
    async fn get_returns_only_the_architect_turn_of_an_internal_exchange() {
        let root = tmp_root("chat-internal-get");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![text("Plano atualizado.")]);
        let (status, body) = internal_turn(&state, INTERNAL_MSG, dynb).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, history) = get_history(&state, PROJECT_ID, HOST).await;
        let list = history["turns"].as_array().expect("turns");
        assert_eq!(list.len(), 1, "{history}");
        assert_eq!(list[0]["role"], "architect");
        assert_eq!(list[0]["text"], "Plano atualizado.");
        assert_eq!(list[0]["turn_id"], body["turn_id"]);
        let text = history.to_string();
        assert!(!text.contains(INTERNAL_MSG), "{history}");
        assert!(!text.contains("comando de testes"), "{history}");
    }

    #[tokio::test]
    async fn architect_context_on_next_turn_includes_the_internal_message() {
        let root = tmp_root("chat-internal-context");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![text("Plano atualizado.")]);
        internal_turn(&state, INTERNAL_MSG, dynb).await;
        let (second, dynb) = backend(vec![text("ok")]);
        let (status, body) = turn(&state, PROJECT_ID, "próxima", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let msgs = &second.requests()[0].messages;
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[1].role, ChatRole::User);
        assert_eq!(msgs[1].content, INTERNAL_MSG);
        assert_eq!(msgs[2].content, "Plano atualizado.");
        assert!(msgs[3].content.starts_with("ESTADO ATUAL DO PROJETO"), "{}", msgs[3].content);
        assert_eq!(msgs[4].content, "próxima");
    }

    /// Estado do projeto: spec com o status dado e uma tarefa DONE.
    fn write_project_state(root: &Path, spec_status: &str) {
        let dir = root.join(".sdd/specs/001-demo");
        fs::create_dir_all(dir.join("tasks")).expect("tasks");
        let spec = format!(
            "---\nid: \"001-demo\"\ntitle: \"Demo\"\nstatus: \"{spec_status}\"\nbase_branch: \"main\"\ntest_command: \"node --test\"\ncreated_at: \"2026-10-14T00:00:00Z\"\n---\nCorpo.\n"
        );
        fs::write(dir.join("spec.md"), spec).expect("spec");
        let task = "---\nid: \"T01\"\nspec_id: \"001-demo\"\ntitle: \"Primeira\"\nstatus: \"DONE\"\ndepends_on: []\nbranch: \"\"\nattempts: 1\nmax_attempts: 3\nlast_review_verdict: \"APPROVED\"\nallowed_scope:\n  - \"src/**\"\n---\nObjetivo.\n";
        fs::write(dir.join("tasks/T01.md"), task).expect("task");
    }

    #[tokio::test]
    async fn state_snapshot_is_sent_fresh_every_turn_and_never_persisted() {
        let root = tmp_root("chat-snapshot");
        let state = state_for(&root, config_for(&root, true));
        write_project_state(&root, "AWAITING_APPROVAL");
        let (first, dynb) = backend(vec![text("Aguardando aprovação.")]);
        let (status, body) = turn(&state, PROJECT_ID, "primeira", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // Spec 122 T05: o estado vai no fim, logo antes da mensagem do turno.
        let reqs = first.requests();
        let msg = &reqs[0].messages[reqs[0].messages.len() - 2];
        assert_eq!(msg.role, ChatRole::User);
        assert!(msg.content.starts_with("ESTADO ATUAL DO PROJETO"), "{}", msg.content);
        assert!(msg.content.contains("001-demo"), "{}", msg.content);
        assert!(msg.content.contains("AWAITING_APPROVAL"), "{}", msg.content);

        // O projeto muda entre os turnos: o segundo pedido vê o estado novo.
        write_project_state(&root, "COMPLETED");
        let (second, dynb) = backend(vec![text("Sistema construído.")]);
        let (status, body) = turn(&state, PROJECT_ID, "segunda", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let msgs = &second.requests()[0].messages;
        let state_msg = &msgs[msgs.len() - 2];
        assert!(state_msg.content.contains("COMPLETED"), "{}", state_msg.content);
        assert!(!state_msg.content.contains("AWAITING_APPROVAL"), "{}", state_msg.content);

        // O snapshot não entra no histórico persistido nem na resposta.
        let persisted = fs::read_to_string(root.join(CHAT_FILE_REL)).expect("histórico");
        assert!(!persisted.contains("ESTADO ATUAL"), "{persisted}");
        assert!(!persisted.contains("Projeto:"), "{persisted}");
        assert!(!persisted.contains("Data/hora"), "{persisted}");
    }

    #[tokio::test]
    async fn legacy_user_line_without_flag_stays_visible() {
        let root = tmp_root("chat-internal-legacy");
        let state = state_for(&root, config_for(&root, true));
        fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
        fs::write(root.join(CHAT_FILE_REL), "{\"role\":\"user\",\"text\":\"antiga\",\"ts\":1}\n").expect("w");
        let (status, body) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["turns"][0]["text"], "antiga");
    }

    #[tokio::test]
    async fn non_boolean_internal_is_422_before_the_model() {
        let root = tmp_root("chat-internal-invalid");
        let state = state_for(&root, config_for(&root, true));
        for raw in [
            r#"{"message":"oi","internal":"sim"}"#,
            r#"{"message":"oi","internal":1}"#,
            r#"{"message":"oi","internal":null}"#,
        ] {
            let (status, body) = post_raw(&state, "/api/projects/proj-chat/chat", raw).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        }
        assert!(!root.join(CHAT_FILE_REL).exists());
    }

    #[tokio::test]
    async fn get_limit_counts_only_visible_turns() {
        let root = tmp_root("chat-internal-limit");
        let state = state_for(&root, config_for(&root, true));
        fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
        let mut lines = String::new();
        for i in 0..40 {
            lines.push_str(&format!("{{\"role\":\"user\",\"text\":\"antigo {i}\",\"ts\":1}}\n"));
        }
        for i in 0..10 {
            lines.push_str(&format!("{{\"role\":\"user\",\"text\":\"interna {i}\",\"ts\":1,\"internal\":true}}\n"));
        }
        fs::write(root.join(CHAT_FILE_REL), lines).expect("w");
        let (status, history) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{history}");
        let list = history["turns"].as_array().expect("turns");
        assert_eq!(list.len(), MAX_HISTORY_TURNS, "{history}");
        assert_eq!(list[0]["text"], "antigo 0");
        assert_eq!(list[MAX_HISTORY_TURNS - 1]["text"], "antigo 39");
        assert!(!history.to_string().contains("interna"), "{history}");
    }

    fn turn_events(events: &[ApiEvent]) -> Vec<(String, Value)> {
        events
            .iter()
            .filter(|e| e.event_type().starts_with("chat_turn_"))
            .map(|e| (e.event_type().to_string(), e.payload.clone()))
            .collect()
    }

    #[tokio::test]
    async fn chat_turn_events_and_state_cover_success() {
        let root = tmp_root("chat-turn-events");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let gate = Arc::new(Gate { entered: Notify::new(), release: Notify::new() });
        let dynb: Arc<dyn ChatBackend> = gate.clone();
        let st = state.clone();
        let first = tokio::spawn(async move { turn(&st, PROJECT_ID, "olá mundo", Some(dynb), CHAT_TIMEOUT).await });
        gate.entered.notified().await;
        let live = state.architect_turn(PROJECT_ID).expect("turno em andamento");
        assert_eq!(live.user_text, "olá mundo");
        gate.release.notify_one();
        let (status, body) = first.await.expect("join");
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(state.architect_turn(PROJECT_ID).is_none());
        let evs = turn_events(&drain_events(&mut rx));
        assert_eq!(evs.len(), 2, "{evs:?}");
        assert_eq!(evs[0].0, "chat_turn_started");
        assert_eq!(evs[0].1["user_text"], "olá mundo");
        assert_eq!(evs[0].1["turn_id"], live.turn_id);
        assert_eq!(evs[1].0, "chat_turn_ended");
        assert_eq!(evs[1].1["ok"], true);
        assert_eq!(evs[1].1["turn_id"], live.turn_id);
    }

    #[tokio::test]
    async fn reservation_is_released_before_chat_turn_ended_is_published() {
        let root = tmp_root("chat-release-order");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let st = state.clone();
        let probe = tokio::task::spawn_blocking(move || {
            // Observador síncrono: ao ver o ended, a reserva já tem de estar livre.
            loop {
                match rx.try_recv() {
                    Ok(ev) if ev.event_type() == "chat_turn_ended" => {
                        return st.try_begin_chat(PROJECT_ID).is_some();
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => std::thread::yield_now(),
                    Err(_) => return false,
                }
            }
        });
        let (_, dynb) = backend(vec![text("ok")]);
        let (status, body) = turn(&state, PROJECT_ID, "olá", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(probe.await.expect("join"), "reserva ainda ocupada ao publicar o ended");
    }

    #[tokio::test]
    async fn chat_turn_ended_is_published_on_error_and_internal_hides_text() {
        let root = tmp_root("chat-turn-error");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let (_, dynb) = backend(Vec::new());
        let (status, _) = turn(&state, PROJECT_ID, "segredo?", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let evs = turn_events(&drain_events(&mut rx));
        assert_eq!(evs.len(), 2, "{evs:?}");
        assert_eq!(evs[1].0, "chat_turn_ended");
        assert_eq!(evs[1].1["ok"], false);
        assert!(state.architect_turn(PROJECT_ID).is_none());

        let (_, dynb) = backend(vec![text("ok")]);
        let (status, _) = internal_turn(&state, INTERNAL_MSG, dynb).await;
        assert_eq!(status, StatusCode::OK);
        let evs = turn_events(&drain_events(&mut rx));
        assert_eq!(evs.len(), 2, "{evs:?}");
        assert!(evs[0].1.get("user_text").is_none(), "{:?}", evs[0]);
    }

    #[tokio::test]
    async fn chat_turn_ended_is_published_on_timeout() {
        let root = tmp_root("chat-turn-timeout");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let dynb: Arc<dyn ChatBackend> = Arc::new(Hang);
        let (status, _) = turn(&state, PROJECT_ID, "oi", Some(dynb), Duration::from_millis(50)).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        let evs = turn_events(&drain_events(&mut rx));
        assert_eq!(evs.last().map(|e| e.0.as_str()), Some("chat_turn_ended"), "{evs:?}");
    }

    #[tokio::test]
    async fn chat_during_report_is_409_with_own_reason_and_no_turn_events() {
        let root = tmp_root("chat-report-409");
        let state = state_for(&root, config_for(&root, true));
        let mut rx = state.events().subscribe();
        let _guard = state
            .runs()
            .try_start(PROJECT_ID, crate::server::state::REPORT_RESERVATION)
            .expect("reserva");
        let body = json!({ "message": "oi" }).to_string();
        let resp = run_turn(&state, PROJECT_ID, body.as_bytes(), CHAT_TIMEOUT, None).await.unwrap_or_else(IntoResponse::into_response);
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
        let parsed: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["reason"], "report_in_progress");
        assert_eq!(parsed["code"], REPORT_BUSY_CODE);
        assert!(turn_events(&drain_events(&mut rx)).is_empty());
    }

    // ---- Imagens do usuário (spec 082 T05) ----

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimagem";

    /// Grava um anexo direto na pasta (como o upload faria) e devolve o nome.
    fn put_attachment(root: &Path, n: u32) -> String {
        let name = format!("{n:032x}.png");
        let dir = root.join(crate::server::chat_attachments::ATTACHMENTS_REL);
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join(&name), PNG).expect("png");
        name
    }

    async fn turn_with(
        state: &AppState,
        body: Value,
        backend: Option<Arc<dyn ChatBackend>>,
    ) -> (StatusCode, Value) {
        let body = body.to_string();
        match run_turn(state, PROJECT_ID, body.as_bytes(), CHAT_TIMEOUT, backend).await {
            Ok(resp) => {
                let status = resp.status();
                let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
                (status, serde_json::from_slice(&bytes).expect("json"))
            }
            Err(e) => (e.status(), json!({ "error": e.message() })),
        }
    }

    #[tokio::test]
    async fn attachments_round_trip_and_reach_the_model() {
        let root = tmp_root("chat-attach-rt");
        let state = state_for(&root, config_for(&root, true));
        // Upload real pelo roteador.
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("/api/projects/{PROJECT_ID}/chat/attachments"))
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "image/png")
            .body(Body::from(PNG.to_vec()))
            .expect("requisição");
        let resp = router(state.clone()).oneshot(req).await.expect("resposta");
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
        let up: Value = serde_json::from_slice(&bytes).expect("json");
        let name = up["name"].as_str().expect("name").to_string();

        let (scripted, dynb) = backend(vec![text("Vi a tela.")]);
        let (status, body) =
            turn_with(&state, json!({"message": "olha isso", "attachments": [name]}), dynb).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // A mensagem do turno vai ao modelo com a imagem.
        let req = &scripted.requests()[0];
        let last = req.messages.last().expect("mensagem");
        assert_eq!(last.role, ChatRole::User);
        assert!(last.content.starts_with("olha isso"));
        assert_eq!(last.images.len(), 1);
        assert_eq!(&*last.images[0].data, PNG);
        // Persistido no turno do usuário; o do Arquiteto não tem o campo.
        let lines = chat_lines(&root);
        assert_eq!(lines[0]["attachments"], json!([name]));
        assert!(lines[1].get("attachments").is_none(), "{}", lines[1]);
        // GET devolve `attachments` em cada turno.
        let (status, hist) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hist["turns"][0]["attachments"], json!([name]));
        assert_eq!(hist["turns"][1]["attachments"], json!([]));
    }

    #[tokio::test]
    async fn old_lines_without_attachments_still_load() {
        let root = tmp_root("chat-attach-old");
        let state = state_for(&root, config_for(&root, true));
        fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
        let lines = concat!(
            r#"{"role":"user","text":"antiga","ts":1}"#,
            "\n",
            r#"{"role":"user","text":"nomes ruins","ts":2,"attachments":["../x.png","0123456789abcdef0123456789abcdef.png"]}"#,
            "\n"
        );
        fs::write(root.join(CHAT_FILE_REL), lines).expect("w");
        let (status, hist) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK, "{hist}");
        assert_eq!(hist["turns"][0]["attachments"], json!([]));
        assert_eq!(hist["turns"][1]["attachments"], json!(["0123456789abcdef0123456789abcdef.png"]));
    }

    #[tokio::test]
    async fn invalid_or_missing_attachments_are_422_without_a_turn() {
        let root = tmp_root("chat-attach-422");
        let state = state_for(&root, config_for(&root, true));
        let names: Vec<String> = (1..=5).map(|n| put_attachment(&root, n)).collect();
        for body in [
            json!({"message": "x", "attachments": [format!("{:032x}.png", 99)]}),
            json!({"message": "x", "attachments": ["../../segredo.png"]}),
            json!({"message": "x", "attachments": ["A.png"]}),
            json!({"message": "x", "attachments": names}),
            json!({"message": "x", "attachments": "nao-lista"}),
            json!({"message": "   "}),
            json!({"message": "", "attachments": []}),
        ] {
            let (status, out) = turn_with(&state, body.clone(), None).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body} → {out}");
        }
        assert!(chat_lines(&root).is_empty());
        // Anexo symlink (nome válido) também é recusado.
        let outside = tmp_root("chat-attach-422-out").join("alvo.png");
        fs::write(&outside, PNG).expect("alvo");
        let link = format!("{:032x}.png", 77);
        symlink(&outside, root.join(crate::server::chat_attachments::ATTACHMENTS_REL).join(&link)).expect("link");
        let (status, _) = turn_with(&state, json!({"message": "x", "attachments": [link]}), None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn image_only_message_becomes_placeholder_text() {
        let root = tmp_root("chat-attach-only");
        let state = state_for(&root, config_for(&root, true));
        let name = put_attachment(&root, 1);
        let (scripted, dynb) = backend(vec![text("ok")]);
        let (status, body) = turn_with(&state, json!({"message": "  ", "attachments": [name]}), dynb).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(chat_lines(&root)[0]["text"], "(imagem)");
        let last = scripted.requests()[0].messages.last().cloned().expect("msg");
        assert!(last.content.starts_with("(imagem)"));
        assert_eq!(last.images.len(), 1);
    }

    #[tokio::test]
    async fn provider_refusing_images_answers_with_text_and_notice() {
        let root = tmp_root("chat-attach-refused");
        let state = state_for(&root, config_for(&root, true));
        let name = put_attachment(&root, 1);
        let (scripted, dynb) = backend(vec![
            Err(LlmError::Http { status: 400, body_snippet: "image_url not supported".into(), code: None }),
            text("Respondo pelo texto."),
        ]);
        let (status, body) = turn_with(&state, json!({"message": "veja a tela", "attachments": [name]}), dynb).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let reply = body["reply"].as_str().expect("reply");
        assert!(reply.contains("o modelo do Arquiteto não aceitou imagens; respondi só com o texto"), "{reply}");
        assert!(reply.contains("Respondo pelo texto."));
        let reqs = scripted.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].messages.last().expect("m").images.len(), 1);
        let retry = reqs[1].messages.last().expect("m");
        assert!(retry.images.is_empty());
        assert!(retry.content.starts_with("veja a tela"), "o texto do usuário é preservado: {}", retry.content);
    }

    // ---- Queda da conexão no meio do turno (spec 127 T01) ----

    #[tokio::test]
    async fn connection_lost_records_a_coded_system_notice_in_the_history() {
        let root = tmp_root("chat-conn-lost");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![Err(LlmError::Transport(SECRET.into()))]);
        let (status, body) = turn(&state, PROJECT_ID, "gera o plano", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["code"], CODE_PROVIDER_FAILED);
        assert_eq!(body["reason"], CONNECTION_LOST_REASON, "{body}");
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2, "mensagem do usuário + aviso do sistema");
        let notice = &lines[1];
        assert_eq!(notice["role"], "system");
        assert_eq!(notice["notice"]["code"], CONNECTION_LOST_CODE);
        assert_eq!(notice["turn_id"], lines[0]["turn_id"]);
        let lang = state.project_lang(PROJECT_ID).get();
        assert_eq!(notice["text"], crate::i18n::text(lang, "system.chat.connection_lost"));
        assert!(!notice.to_string().contains(SECRET));
        // O GET mostra o aviso com `code` e `params`, para a tela traduzir.
        let (status, hist) = get_history(&state, PROJECT_ID, HOST).await;
        assert_eq!(status, StatusCode::OK);
        let turns = hist["turns"].as_array().expect("turns");
        let last = turns.last().expect("aviso");
        assert_eq!(last["role"], "system");
        assert_eq!(last["code"], CONNECTION_LOST_CODE);
        assert_eq!(last["params"], json!({}));
    }

    #[tokio::test]
    async fn connection_lost_after_the_spec_was_written_names_what_was_saved() {
        let root = tmp_root("chat-conn-lost-saved");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![write_spec_call(), Err(LlmError::Transport("queda".into()))]);
        let (status, body) = turn(&state, PROJECT_ID, "gera o plano", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        let lines = chat_lines(&root);
        let notice = lines.last().expect("aviso");
        assert_eq!(notice["notice"]["code"], CONNECTION_LOST_SAVED_CODE);
        assert_eq!(notice["notice"]["params"], json!({"spec": "001-exemplo"}));
        let lang = state.project_lang(PROJECT_ID).get();
        assert_eq!(notice["text"], crate::i18n::tr(lang, "system.chat.connection_lost_saved", &[("spec", "001-exemplo")]));
    }

    #[tokio::test]
    async fn timeout_and_stream_without_final_also_count_as_connection_lost() {
        use crate::agents::llm_client::BadReason;
        for (i, reply) in [Err(LlmError::Timeout), Err(LlmError::Parse(BadReason::new(BadKind::NoFinal)))].into_iter().enumerate() {
            let root = tmp_root(&format!("chat-conn-lost-kind-{i}"));
            let state = state_for(&root, config_for(&root, true));
            let (_, dynb) = backend(vec![reply]);
            let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(body["reason"], CONNECTION_LOST_REASON, "caso {i}: {body}");
            assert_eq!(chat_lines(&root).len(), 2, "caso {i}");
        }
    }

    /// Spec 133 T11: as outras falhas do provedor também ficam no histórico, com o próprio código
    /// (sem o "Tentar de novo" da queda) e `reason: failure_recorded`.
    #[tokio::test]
    async fn other_provider_failures_record_a_coded_failure_notice() {
        let root = tmp_root("chat-conn-other");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![Err(LlmError::Http { status: 401, body_snippet: String::new(), code: None })]);
        let (status, body) = turn(&state, PROJECT_ID, "oi", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["reason"], FAILURE_RECORDED_REASON, "{body}");
        assert_eq!(body["params"]["category"], "auth", "{body}");
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["notice"]["code"], PROVIDER_FAILED_NOTICE_CODE);
        assert_eq!(lines[1]["notice"]["params"], body["params"]);
    }

    #[tokio::test]
    async fn continue_runs_an_internal_turn_and_the_notice_never_reaches_the_model() {
        let root = tmp_root("chat-continue");
        let state = state_for(&root, config_for(&root, true));
        let (_, dynb) = backend(vec![Err(LlmError::Transport("queda".into()))]);
        let (status, _) = turn(&state, PROJECT_ID, "gera o plano", dynb, CHAT_TIMEOUT).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);

        let scripted = Scripted::new(vec![text("Tarefas gravadas.")]);
        let state = state.with_llm_backend(scripted.clone());
        let (status, body) = post_raw(&state, &format!("/api/projects/{PROJECT_ID}/chat/continue"), "").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["reply"], "Tarefas gravadas.");
        let reqs = scripted.requests();
        let messages = &reqs[0].messages;
        let last = messages.last().expect("pedido");
        assert_eq!(last.content, crate::agents::prompts::continue_turn_request(None, crate::i18n::Lang::PtBr));
        let lang = state.project_lang(PROJECT_ID).get();
        let notice_text = crate::i18n::text(lang, "system.chat.connection_lost");
        assert!(messages.iter().all(|m| !m.content.contains(&notice_text)), "o aviso é da pessoa, não do modelo");
        assert!(messages.iter().any(|m| m.content == "gera o plano"), "o pedido original segue no contexto");
        // O pedido de continuação é interno: gravado, mas fora do GET.
        let lines = chat_lines(&root);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2]["internal"], true);
        let (_, hist) = get_history(&state, PROJECT_ID, HOST).await;
        let roles: Vec<&str> = hist["turns"].as_array().expect("turns").iter().filter_map(|t| t["role"].as_str()).collect();
        assert_eq!(roles, vec!["user", "system", "architect"]);
    }

    #[tokio::test]
    async fn continue_with_spec_asks_to_finish_that_plan_and_validates_the_body() {
        let root = tmp_root("chat-continue-spec");
        let scripted = Scripted::new(vec![text("Plano terminado.")]);
        let state = state_for(&root, config_for(&root, true)).with_llm_backend(scripted.clone());
        let uri = format!("/api/projects/{PROJECT_ID}/chat/continue");
        let (status, _) = post_raw(&state, &uri, r#"{"spec_id": "../fora"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, _) = post_raw(&state, &uri, "{não é json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(scripted.requests().is_empty(), "corpo inválido não chega ao modelo");
        let (status, body) = post_raw(&state, &uri, r#"{"spec_id": "001-exemplo"}"#).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let last = scripted.requests()[0].messages.last().cloned().expect("pedido");
        assert_eq!(last.content, crate::agents::prompts::continue_turn_request(Some("001-exemplo"), crate::i18n::Lang::PtBr));
        assert!(last.content.ends_with("Spec: 001-exemplo"));
    }
}
