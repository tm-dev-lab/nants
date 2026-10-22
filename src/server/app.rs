//! Sistema do projeto em execução para teste manual (spec 021, item 4; Docker desde a spec 074).
//!
//! Todo projeto do Nants é Docker: o app sobe no projeto Compose `nants-<id>-run`, sempre por um
//! clique humano confirmado na UI. O corpo de `POST /api/projects/:id/app/start` exige
//! `{confirm: true, spec_id}` e o guard global só aceita requisições do mesmo origin. O comando vem
//! do `run_command` da spec COMPLETED (nunca do corpo). Projeto sem ambiente docker é recusado com a
//! mensagem fixa de "replaneje" (409 `environment_not_docker`).
//!
//! - Parar: `down` sem `-v`. Encerrado também na remoção do projeto, no shutdown do Nants e quando
//!   um run do projeto inicia.
//! - A partida roda sob a reserva interna `__app__` do projeto: exclui run, remoção, relatório,
//!   ajuste de settings e outra partida; remoção e run encerram o app depois de reservar.
//! - Sem a prontidão `execute` (spec 093 T02), a partida é 412 `prereq_missing`.
//! - `embeddable` (spec 093 T02, decisão 12): ao subir e a cada consulta do estado, um GET curto em
//!   `http://127.0.0.1:<porta publicada>` (nunca o `Host` da requisição), sem redirect, com timeout
//!   de 2 s e corpo descartado, decide se a página pode ser embutida (`true|false|null`).
//! - A porta do próprio Nants nunca vira link nem é sondada.
//!
//! Eventos aditivos: `app_started {spec_id, url, ports, notices}` e `app_exited {code, reason}`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::events::{validate_project_id, ApiEvent, EventBus, EventType};
use super::projects::{find_project, format_utc, scrub_text};
use super::safe_fs;
use super::spec_result::validate_spec_id;
use super::state::{ReserveError, APP_RESERVATION, REPORT_BUSY_MSG};
use super::AppState;
use crate::harness::compose::{ComposeEnv, ComposeKind, PublishedPort};
use crate::sdd::{Spec, SpecStatus};

/// Carência do `down` do `-run` (`--timeout 5`); base da espera do shutdown.
pub const STOP_GRACE: Duration = Duration::from_secs(5);
/// Timeout da sonda `embeddable`.
pub const EMBED_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SPEC_BYTES: u64 = 256 * 1024;

pub const REASON_EXITED: &str = "exited";
pub const REASON_STOPPED: &str = "stopped";
pub const REASON_RUN_STARTED: &str = "run_started";
pub const REASON_PROJECT_REMOVED: &str = "project_removed";
pub const REASON_SHUTDOWN: &str = "shutdown";

const MSG_CONFIRM: &str = "confirmação humana necessária para executar o sistema";
const MSG_BODY: &str = "corpo JSON inválido";
const MSG_NOT_FOUND: &str = "projeto não encontrado";
const MSG_SPEC_NOT_FOUND: &str = "spec não encontrada";
const MSG_BUSY: &str = "já existe uma execução em andamento neste projeto";
const MSG_NOT_COMPLETED: &str = "a spec precisa estar COMPLETED para executar o sistema";
const MSG_NO_COMMAND: &str = "A especificação não declara como executar";
const MSG_RUNNING: &str = "o sistema já está em execução neste projeto";
const MSG_INTERNAL: &str = "falha interna";
const MSG_RUN_DOCKER: &str = "em modo docker, o run_command roda DENTRO do serviço workspace: não invoque docker, \
docker compose nem podman; use o comando interno que sobe o app ouvindo 0.0.0.0 (por exemplo: npm ci && npm run dev -- --host 0.0.0.0 --port 5173)";
/// Linhas de log do workspace anexadas ao detalhe de falha do `up`.
const FAIL_LOG_LINES: u32 = 50;
/// Janela de observação do `-run` após o `up` (053).
const RUN_OBSERVE_WINDOW: Duration = Duration::from_secs(8);
/// Limite (caracteres, contados do FIM) do detalhe de falha devolvido ao front.
const DETAIL_CHARS: usize = 1600;
const MSG_DOCKER_PORT: &str = "uma porta publicada pelo ambiente Docker já está em uso no host";
/// Código (spec 102) da porta publicada já em uso no host.
pub const CODE_PORT_IN_USE: &str = "app_port_in_use";
/// Código (spec 102) da faixa de portas de visualização esgotada (`params.start`, `params.end`).
pub const CODE_VIEW_PORTS_EXHAUSTED: &str = "view_ports_exhausted";
/// Código (spec 102) do aviso de porta trocada (`params.wanted`, `params.used`).
pub const CODE_PORT_REMAPPED: &str = "app_port_remapped";
const MSG_DOCKER_UP: &str = "não foi possível subir o ambiente Docker do projeto";
const MSG_DOCKER_DOWN: &str = "não foi possível derrubar o ambiente Docker do projeto";
/// Host usado nos links quando o `Host` da requisição não é aceito pela política do servidor.
pub const FALLBACK_HOST: &str = "127.0.0.1";

/// App no modo docker: projeto Compose `nants-<id>-run` (spec 030).
#[derive(Clone)]
struct DockerApp {
    spec_id: String,
    started_at: String,
    root: std::path::PathBuf,
    compose: Arc<ComposeEnv>,
    ports: Vec<PublishedPort>,
    /// Avisos curtos (porta do host trocada).
    notices: Vec<String>,
    /// Os mesmos avisos com `code` e `params` (spec 102), na mesma ordem.
    notice_codes: Vec<Value>,
}

fn ports_json(ports: &[PublishedPort]) -> Value {
    Value::Array(
        ports
            .iter()
            .map(|p| json!({"service": p.service, "target": p.target, "published": p.published, "url": null}))
            .collect(),
    )
}

impl DockerApp {
    fn view(&self) -> Value {
        json!({
            "state": "running", "mode": "docker", "spec_id": self.spec_id, "url": null,
            "started_at": self.started_at, "exit_code": null, "ports": ports_json(&self.ports), "notices": self.notices,
            "notice_codes": self.notice_codes,
        })
    }
}

/// Marca de start em andamento; some ao sair de escopo.
struct StartMark {
    set: Arc<Mutex<std::collections::HashSet<String>>>,
    id: String,
}

impl Drop for StartMark {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.id);
    }
}

/// Gerenciador dos apps de teste (um por projeto). Clones compartilham o mapa.
#[derive(Clone)]
pub struct AppManager {
    docker: Arc<Mutex<HashMap<String, DockerApp>>>,
    /// Starts docker em andamento (marca de partida): evita start concorrente e protege do sweep.
    starting: Arc<Mutex<std::collections::HashSet<String>>>,
    events: EventBus,
    observe: Duration,
    /// Timeout da sonda `embeddable` ([`EMBED_PROBE_TIMEOUT`]; testes usam prazo curto).
    probe_timeout: Duration,
}

impl std::fmt::Debug for AppManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppManager")
    }
}

/// Falha da partida.
#[derive(Debug, PartialEq, Eq)]
pub enum LaunchError {
    AlreadyRunning,
    Spawn,
    /// Falha do Docker/Compose (mensagem técnica; redigir antes de exibir).
    Docker(String),
}

impl AppManager {
    pub fn new(events: EventBus) -> Self {
        Self {
            docker: Arc::new(Mutex::new(HashMap::new())),
            starting: Arc::new(Mutex::new(std::collections::HashSet::new())),
            events,
            observe: RUN_OBSERVE_WINDOW,
            probe_timeout: EMBED_PROBE_TIMEOUT,
        }
    }

    /// Janela de observação pós-`up` do `-run`.
    pub fn with_observe_window(mut self, window: Duration) -> Self {
        self.observe = window;
        self
    }

    /// Timeout da sonda `embeddable` (testes).
    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// Timeout atual da sonda `embeddable`.
    pub fn probe_timeout(&self) -> Duration {
        self.probe_timeout
    }

    /// `true` se há app em execução no projeto.
    pub fn is_running(&self, project_id: &str) -> bool {
        self.get_docker(project_id).is_some()
    }

    /// App em execução ou start em andamento.
    pub fn is_busy(&self, project_id: &str) -> bool {
        self.is_running(project_id)
            || self.starting.lock().unwrap_or_else(PoisonError::into_inner).contains(project_id)
    }

    fn get_docker(&self, project_id: &str) -> Option<DockerApp> {
        self.docker.lock().unwrap_or_else(PoisonError::into_inner).get(project_id).cloned()
    }

    /// Estado do projeto: `stopped` quando nunca iniciado.
    pub fn view(&self, project_id: &str) -> Value {
        match self.get_docker(project_id) {
            Some(d) => d.view(),
            None => json!({
                "state": "stopped", "spec_id": null, "url": null,
                "started_at": null, "exit_code": null,
            }),
        }
    }

    /// Sobe o projeto `-run` (`up -d --build --wait`) e registra as portas publicadas. Em falha,
    /// tenta derrubar o que subiu (sem `-v`) e devolve a mensagem.
    pub async fn launch_docker(
        &self,
        project_id: &str,
        root: &Path,
        spec_id: &str,
        compose: Arc<ComposeEnv>,
        run_command: &str,
    ) -> Result<Value, LaunchError> {
        let _mark = {
            let mut starting = self.starting.lock().unwrap_or_else(PoisonError::into_inner);
            if self.is_running(project_id) || !starting.insert(project_id.to_string()) {
                return Err(LaunchError::AlreadyRunning);
            }
            StartMark { set: Arc::clone(&self.starting), id: project_id.to_string() }
        };
        let (c, r, cmd) = (Arc::clone(&compose), root.to_path_buf(), run_command.to_string());
        let window = self.observe;
        let up = tokio::task::spawn_blocking(move || {
            let attempt = c.up_run(&r, &cmd).and_then(|()| c.observe_run(&r, window)).and_then(|()| c.published_ports(&r));
            let attempt = attempt.map_err(|e| match c.workspace_logs(&r, FAIL_LOG_LINES) {
                Ok(logs) if !logs.trim().is_empty() => format!("{e}\n--- logs do workspace ---\n{}", logs.trim_end()),
                _ => e,
            });
            if attempt.is_err() {
                let _ = c.down_with(&r, false);
            }
            attempt
        })
        .await;
        let remaps = compose.port_remaps();
        let notices: Vec<String> = remaps.iter().map(|(want, used)| format!("Porta {want} ocupada; usando {used}")).collect();
        let notice_codes: Vec<Value> = remaps
            .iter()
            .map(|(want, used)| json!({"code": CODE_PORT_REMAPPED, "params": {"wanted": want, "used": used}}))
            .collect();
        let ports = match up {
            Ok(Ok(ports)) => ports,
            Ok(Err(e)) => return Err(LaunchError::Docker(e)),
            Err(_) => return Err(LaunchError::Spawn),
        };
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let app = DockerApp {
            spec_id: spec_id.to_string(),
            started_at: format_utc(secs),
            root: root.to_path_buf(),
            compose,
            ports,
            notices,
            notice_codes,
        };
        let view = app.view();
        self.docker.lock().unwrap_or_else(PoisonError::into_inner).insert(project_id.to_string(), app);
        self.events.publish(ApiEvent::new(
            EventType::AppStarted,
            Some(project_id.to_string()),
            json!({
                "spec_id": spec_id, "url": null, "ports": view["ports"], "notices": view["notices"],
                "notice_codes": view["notice_codes"],
            }),
        ));
        Ok(view)
    }

    /// Logs recentes do `-run` (`logs --tail`), se o app do projeto é docker.
    pub async fn docker_logs(&self, project_id: &str) -> Option<String> {
        let app = self.get_docker(project_id)?;
        let out = tokio::task::spawn_blocking(move || app.compose.logs(&app.root)).await;
        Some(match out {
            Ok(Ok(text)) => text,
            Ok(Err(e)) => format!("[logs indisponíveis: {e}]"),
            Err(_) => String::new(),
        })
    }

    /// Para o app docker (`down`, sem `-v`). O registro só some se o `down` deu certo.
    async fn stop_docker(&self, project_id: &str, reason: &'static str) -> bool {
        let Some(app) = self.get_docker(project_id) else {
            return true;
        };
        let down = tokio::task::spawn_blocking(move || app.compose.down_with(&app.root, false)).await;
        match down {
            Ok(Ok(())) => {
                self.docker.lock().unwrap_or_else(PoisonError::into_inner).remove(project_id);
                self.events.publish(ApiEvent::new(
                    EventType::AppExited,
                    Some(project_id.to_string()),
                    json!({"code": 0, "reason": reason}),
                ));
                true
            }
            Ok(Err(e)) => {
                eprintln!("aviso: down do -run de {project_id} falhou: {e}");
                false
            }
            Err(_) => false,
        }
    }

    /// `true` se há app docker registrado (ainda no ar) no projeto.
    pub fn is_docker_running(&self, project_id: &str) -> bool {
        self.get_docker(project_id).is_some()
    }

    /// Encerra o app do projeto (se houver): `down` do `-run`, sem `-v`. Idempotente.
    pub async fn stop(&self, project_id: &str, reason: &'static str) {
        if self.is_docker_running(project_id) {
            self.stop_docker(project_id, reason).await;
        }
    }

    /// Encerra todos os apps (shutdown), em paralelo.
    pub async fn stop_all(&self, reason: &'static str) {
        let ids: Vec<String> = self.docker.lock().unwrap_or_else(PoisonError::into_inner).keys().cloned().collect();
        let jobs: Vec<_> = ids
            .into_iter()
            .map(|id| {
                let me = self.clone();
                tokio::spawn(async move { me.stop(&id, reason).await })
            })
            .collect();
        for job in jobs {
            let _ = job.await;
        }
    }
}

fn reject(status: StatusCode, message: &str, reason: &'static str) -> Response {
    (status, Json(json!({"error": message, "reason": reason}))).into_response()
}

fn plain(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartInput {
    confirm: bool,
    spec_id: String,
}

/// `POST /api/projects/:id/app/start`
pub async fn start_app(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(e) = validate_project_id(&id) {
        return e.into_response();
    }
    let Ok(input) = serde_json::from_slice::<StartInput>(&body) else {
        return plain(StatusCode::BAD_REQUEST, MSG_BODY);
    };
    if !input.confirm {
        return plain(StatusCode::BAD_REQUEST, MSG_CONFIRM);
    }
    if let Err(e) = validate_spec_id(&input.spec_id) {
        return e.into_response();
    }
    // Prontidão `execute` (spec 093 T02), com o Docker conferido na hora, antes de reservar.
    if let Err(resp) = super::readiness::require(&state, super::readiness::Gate::Execute).await {
        return resp;
    }
    // Reserva interna do projeto durante toda a partida (exclui run, remoção e outra partida).
    let (project, _guard) = match state.reserve_run(&id, APP_RESERVATION).await {
        Ok(ok) => ok,
        Err(ReserveError::NotFound) => return plain(StatusCode::NOT_FOUND, MSG_NOT_FOUND),
        Err(ReserveError::Busy) => {
            return reject(StatusCode::CONFLICT, MSG_BUSY, "busy");
        }
        Err(ReserveError::ReportInProgress) => {
            return reject(StatusCode::CONFLICT, REPORT_BUSY_MSG, "report_in_progress");
        }
    };
    let root = std::path::PathBuf::from(&project.path);
    let spec_root = root.clone();
    let rel = format!(".sdd/specs/{}/spec.md", input.spec_id);
    let read = tokio::task::spawn_blocking(move || {
        safe_fs::read_regular(&spec_root, &rel, MAX_SPEC_BYTES)
            .ok()
            .and_then(|text| Spec::parse(&text).ok())
    })
    .await;
    let Ok(spec) = read else {
        return plain(StatusCode::INTERNAL_SERVER_ERROR, MSG_INTERNAL);
    };
    let Some(spec) = spec else {
        return plain(StatusCode::NOT_FOUND, MSG_SPEC_NOT_FOUND);
    };
    if spec.frontmatter.status != SpecStatus::Completed {
        return reject(StatusCode::CONFLICT, MSG_NOT_COMPLETED, "spec_not_completed");
    }
    if state.apps().is_running(&id) {
        return reject(StatusCode::CONFLICT, MSG_RUNNING, "already_running");
    }
    // Projeto Compose `-run` na raiz do projeto; projeto sem ambiente docker é recusado (spec 074).
    let Some(compose) = state.compose().for_project_lan(&root, &id, ComposeKind::Run, state.lan_view_ports()) else {
        return reject(
            StatusCode::CONFLICT,
            super::runs::PROJECT_NOT_DOCKER_MSG,
            super::runs::REASON_ENV_NOT_DOCKER,
        );
    };
    let Some(raw) = spec.frontmatter.run_command.as_deref() else {
        return reject(StatusCode::UNPROCESSABLE_ENTITY, MSG_NO_COMMAND, "no_run_command");
    };
    let Ok(inner) = crate::sdd::docker_command::normalize(raw) else {
        return reject(StatusCode::UNPROCESSABLE_ENTITY, MSG_RUN_DOCKER, "run_command_docker");
    };
    match state.apps().launch_docker(&id, &root, &input.spec_id, compose, &inner).await {
        Ok(view) => Json(app_view(&state, view, &headers, true).await).into_response(),
        Err(LaunchError::AlreadyRunning) => reject(StatusCode::CONFLICT, MSG_RUNNING, "already_running"),
        Err(LaunchError::Docker(detail)) => {
            let secrets = state.secret_set().await;
            let clean = scrub_text(&secrets, &detail);
            let skip = clean.chars().count().saturating_sub(DETAIL_CHARS);
            let detail: String = clean.chars().skip(skip).collect();
            // Faixa de visualização esgotada (spec 097): mesmo caminho do conflito de porta, com a
            // mensagem da faixa e os números reais. `code`/`params` (spec 102) para a interface traduzir.
            if let Some(line) = clean.lines().find(|l| l.starts_with(crate::config::VIEW_PORTS_EXHAUSTED_PREFIX)) {
                let range = state.lan_view_ports();
                let params = json!({
                    "start": range.map(|r| r.start),
                    "end": range.map(|r| r.end),
                });
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "error": line, "reason": "docker_port_in_use", "detail": line,
                        "code": CODE_VIEW_PORTS_EXHAUSTED, "params": params,
                    })),
                )
                    .into_response();
            }
            if crate::harness::compose::is_port_conflict(&detail) {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "error": MSG_DOCKER_PORT, "reason": "docker_port_in_use", "detail": detail,
                        "code": CODE_PORT_IN_USE,
                    })),
                )
                    .into_response();
            }
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": MSG_DOCKER_UP, "reason": "docker_failed", "detail": detail})),
            )
                .into_response()
        }
        Err(LaunchError::Spawn) => plain(StatusCode::INTERNAL_SERVER_ERROR, MSG_INTERNAL),
    }
}

/// `POST /api/projects/:id/app/stop`: sem app em execução é um no-op (200, `stopped`).
pub async fn stop_app(State(state): State<AppState>, AxPath(id): AxPath<String>, headers: HeaderMap) -> Response {
    if let Err(e) = validate_project_id(&id) {
        return e.into_response();
    }
    if let Err(e) = find_project(&state, &id).await {
        return e.into_response();
    }
    state.apps().stop(&id, REASON_STOPPED).await;
    if state.apps().is_docker_running(&id) {
        return plain(StatusCode::INTERNAL_SERVER_ERROR, MSG_DOCKER_DOWN);
    }
    Json(app_view(&state, state.apps().view(&id), &headers, false).await).into_response()
}

/// `GET /api/projects/:id/app`: estado, links e `embeddable` (sondado de novo a cada consulta).
pub async fn get_app(State(state): State<AppState>, AxPath(id): AxPath<String>, headers: HeaderMap) -> Response {
    if let Err(e) = validate_project_id(&id) {
        return e.into_response();
    }
    if let Err(e) = find_project(&state, &id).await {
        return e.into_response();
    }
    Json(app_view(&state, state.apps().view(&id), &headers, true).await).into_response()
}

/// Nome do host da requisição (sem porta), para os links das portas do `-run`: assim o link
/// funciona também no celular, pela LAN. O valor é validado como a `HostPolicy` faz; um `Host`
/// recusado vira loopback.
fn request_host(state: &AppState, headers: &HeaderMap) -> String {
    let Some(raw) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return FALLBACK_HOST.to_string();
    };
    if !super::security::host_allowed(raw, state.bound_port(), state.host_policy()) {
        return FALLBACK_HOST.to_string();
    }
    host_name(raw)
}

/// Parte de nome de `request_host` (sem a validação, que depende do `State`): o host sem a porta;
/// IPv6 fica entre colchetes; vazio vira loopback. Usada também pela CSP da UI (`static_ui`), cujo
/// handler não tem `State`: lá o `Host` já passou pelo `security::guard` com a mesma validação.
pub fn host_name(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix('[') {
        return rest.split_once(']').map_or(FALLBACK_HOST.to_string(), |(ip, _)| format!("[{ip}]"));
    }
    raw.split(':').next().filter(|h| !h.is_empty()).unwrap_or(FALLBACK_HOST).to_string()
}

/// Dados de rede para a interface (spec 097): porta do Nants, faixa das portas de visualização
/// (`{start, end}` só em `--lan`, senão `null`) e se o servidor roda no WSL. Nunca inclui
/// endereço interno do WSL.
pub fn network_info(state: &AppState) -> Value {
    json!({
        "nants_port": state.bound_port(),
        "lan_view_ports": state.lan_view_ports().map(|r| json!({"start": r.start, "end": r.end})),
        "wsl": super::netinfo::running_on_wsl(),
    })
}

/// Visão do app para a API: links com o host da requisição e, com `probe`, o `embeddable` de cada
/// porta (e o do app, que é o da primeira porta com link). A porta do Nants não vira link.
async fn app_view(state: &AppState, view: Value, headers: &HeaderMap, probe: bool) -> Value {
    let mut view = with_links(view, &request_host(state, headers), state.bound_port());
    if let (Some(obj), Value::Object(net)) = (view.as_object_mut(), network_info(state)) {
        obj.extend(net);
    }
    let ports: Vec<u16> = view
        .get("ports")
        .and_then(Value::as_array)
        .map(|ports| {
            ports
                .iter()
                .filter(|p| p.get("url").is_some_and(Value::is_string))
                .filter_map(|p| p.get("published").and_then(Value::as_u64))
                .filter_map(|n| u16::try_from(n).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut results: HashMap<u16, Option<bool>> = HashMap::new();
    if probe && view.get("state").and_then(Value::as_str) == Some("running") {
        let timeout = state.apps().probe_timeout();
        // Sondas em paralelo (poucas portas, 2 s no máximo cada).
        let jobs: Vec<_> = ports.iter().map(|port| (*port, tokio::spawn(probe_embeddable(*port, timeout)))).collect();
        for (port, job) in jobs {
            results.insert(port, job.await.ok().flatten());
        }
    }
    let verdict_of = |n: Option<u64>| -> Value {
        n.and_then(|n| u16::try_from(n).ok())
            .and_then(|n| results.get(&n).copied().flatten())
            .map_or(Value::Null, Value::Bool)
    };
    let mut first: Option<Value> = None;
    if let Some(list) = view.get_mut("ports").and_then(Value::as_array_mut) {
        for p in list {
            let linked = p.get("url").is_some_and(Value::is_string);
            let verdict = if linked { verdict_of(p.get("published").and_then(Value::as_u64)) } else { Value::Null };
            if linked && first.is_none() {
                first = Some(verdict.clone());
            }
            if let Some(obj) = p.as_object_mut() {
                obj.insert("embeddable".into(), verdict);
            }
        }
    }
    if let Some(obj) = view.as_object_mut() {
        obj.insert("embeddable".into(), first.unwrap_or(Value::Null));
    }
    view
}

/// Preenche `ports[].url` com `http://<host>:<porta publicada>`. A porta do próprio Nants
/// (`nants_port`) fica sem link (`url: null`).
fn with_links(mut view: Value, host: &str, nants_port: u16) -> Value {
    if let Some(ports) = view.get_mut("ports").and_then(Value::as_array_mut) {
        for p in ports {
            let published = p.get("published").and_then(Value::as_u64);
            if let (Some(n), Some(obj)) = (published, p.as_object_mut()) {
                let url = if n == u64::from(nants_port) {
                    Value::Null
                } else {
                    Value::String(format!("http://{host}:{n}"))
                };
                obj.insert("url".into(), url);
            }
        }
    }
    view
}

/// Sonda `embeddable` da porta publicada: GET em `http://127.0.0.1:<porta>/` (nunca o host da
/// requisição, contra SSRF em `--lan`), sem proxy, sem redirect, com `timeout` e corpo descartado.
/// `None` = indeterminado (sem resposta, erro ou timeout).
pub async fn probe_embeddable(port: u16, timeout: Duration) -> Option<bool> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(timeout)
        .build()
        .ok()?;
    let url = format!("http://{FALLBACK_HOST}:{port}/");
    let response = tokio::time::timeout(timeout, client.get(url).send()).await.ok()?.ok()?;
    let verdict = embeddable_from_headers(response.headers());
    // Corpo descartado: a resposta cai aqui sem ser lida.
    drop(response);
    verdict
}

/// Decide pelo cabeçalho: `X-Frame-Options` DENY/SAMEORIGIN → `false`; CSP `frame-ancestors` com
/// `'none'`, vazio ou só `'self'` → `false`; `frame-ancestors` com outras origens (sem `*`) →
/// indeterminado; sem restrição → `true`.
pub fn embeddable_from_headers(headers: &reqwest::header::HeaderMap) -> Option<bool> {
    let xfo_blocks = headers.get_all("x-frame-options").iter().any(|v| {
        v.to_str().is_ok_and(|t| {
            let t = t.trim().to_ascii_lowercase();
            t == "deny" || t == "sameorigin"
        })
    });
    if xfo_blocks {
        return Some(false);
    }
    let mut verdict = Some(true);
    for value in headers.get_all("content-security-policy") {
        let Ok(text) = value.to_str() else { continue };
        for directive in text.split(';') {
            let mut tokens = directive.split_ascii_whitespace();
            if !tokens.next().is_some_and(|name| name.eq_ignore_ascii_case("frame-ancestors")) {
                continue;
            }
            let sources: Vec<String> = tokens.map(str::to_ascii_lowercase).collect();
            if sources.is_empty() || sources.iter().any(|s| s == "'none'") || sources.iter().all(|s| s == "'self'") {
                return Some(false);
            }
            if !sources.iter().any(|s| s == "*") {
                verdict = None;
            }
        }
    }
    verdict
}

/// `GET /api/projects/:id/app/logs`: texto (≤ 64 KiB) redigido por `scrub_text`.
pub async fn get_logs(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Response {
    if let Err(e) = validate_project_id(&id) {
        return e.into_response();
    }
    if let Err(e) = find_project(&state, &id).await {
        return e.into_response();
    }
    let secrets = state.secret_set().await;
    let raw = state.apps().docker_logs(&id).await.unwrap_or_default();
    let text = scrub_text(&secrets, &raw);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        text,
    )
        .into_response()
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
