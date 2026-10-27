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
//! - Modo pelo comportamento (spec 130 T02), nunca pela tecnologia: depois do `up -d --build` (sem
//!   `--wait`), o servidor observa o `workspace` por uma janela curta. Saiu: `finished`, com o código
//!   (`ps --all`), a saída limitada e redigida (`logs`), os arquivos da pasta criados ou mudados (dois
//!   instantâneos) e o que foi gravado fora de `/work` (`docker diff`); o container encerrado fica
//!   até o app ser parado ou subir de novo. Uma porta responde HTTP: `page`. Vivo sem página:
//!   `running`. O modo é vivo: a cada consulta e por uma sondagem leve, `running` vira `page` e os
//!   dois viram `finished`. Sair com código 0 não é falha.
//!
//! Eventos aditivos: `app_started {spec_id, url, ports, notices, mode}`, `app_exited {code, reason,
//! mode}` (o código real; `null` quando o app vivo foi parado) e `app_mode_changed {spec_id, mode,
//! previous, exit_code}`.

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
use serde_json::{json, Map, Value};

use super::error::{params, ApiError};

use super::events::{validate_project_id, ApiEvent, EventBus, EventType};
use super::projects::{find_project, format_utc, scrub_text};
use super::safe_fs;
use super::spec_result::validate_spec_id;
use super::state::{ReserveError, APP_RESERVATION, REPORT_BUSY_CODE};
use super::AppState;
use crate::harness::compose::{written_outside_work, ComposeEnv, ComposeKind, PublishedPort, RunMode, RunState};
use crate::harness::tree_snapshot::{ChangedFiles, TreeSnapshot};
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

/// Códigos (spec 115 T02) dos erros do app; os textos ficam nos dicionários (`serverError.<code>`).
const ERR_CONFIRM: &str = "app_confirm_required";
const ERR_BODY: &str = "body_invalid";
const ERR_NOT_FOUND: &str = "project_not_found";
const ERR_SPEC_NOT_FOUND: &str = "spec_not_found";
const ERR_BUSY: &str = "run_active";
const ERR_NOT_COMPLETED: &str = "app_spec_not_completed";
const ERR_NO_COMMAND: &str = "app_no_run_command";
const ERR_RUNNING: &str = "app_already_running";
const ERR_INTERNAL: &str = "internal";
const ERR_RUN_DOCKER: &str = "app_run_command_docker";
/// Linhas de log do workspace anexadas ao detalhe de falha do `up`.
const FAIL_LOG_LINES: u32 = 50;
/// Janela de observação do `-run` após o `up` (053); ao fim dela sai a classificação inicial do
/// modo (spec 130 T02).
const RUN_OBSERVE_WINDOW: Duration = Duration::from_secs(8);
/// Intervalo da sondagem leve do modo enquanto o app está vivo (spec 130 T02, C-08).
pub const WATCH_INTERVAL: Duration = Duration::from_secs(3);
/// Linhas da saída de um "terminou" pedidas ao `logs`.
const OUTPUT_LINES: u32 = 400;
/// Limite (caracteres, contados do FIM) da saída de um "terminou".
const OUTPUT_CHARS: usize = 16 * 1024;
/// Arquivos da pasta listados num "terminou" (o resto é contado e avisado).
const MAX_FILES: usize = 100;
/// Caminhos fora de `/work` listados num "terminou".
const MAX_OUTSIDE: usize = 50;
/// Limite (caracteres, contados do FIM) do detalhe de falha devolvido ao front.
const DETAIL_CHARS: usize = 1600;
/// Código (spec 102) da porta publicada já em uso no host.
pub const CODE_PORT_IN_USE: &str = "app_port_in_use";
/// Código (spec 102) da faixa de portas de visualização esgotada (`params.start`, `params.end`).
pub const CODE_VIEW_PORTS_EXHAUSTED: &str = "view_ports_exhausted";
/// Código (spec 102) do aviso de porta trocada (`params.wanted`, `params.used`).
pub const CODE_PORT_REMAPPED: &str = "app_port_remapped";
const ERR_DOCKER_UP: &str = "app_docker_up_failed";
const ERR_DOCKER_DOWN: &str = "app_docker_down_failed";
/// Host usado nos links quando o `Host` da requisição não é aceito pela política do servidor.
pub const FALLBACK_HOST: &str = "127.0.0.1";

/// App no modo docker: projeto Compose `nants-<id>-run` (spec 030).
#[derive(Clone)]
struct DockerApp {
    /// Identifica esta partida: uma mudança de modo só vale para a partida que a observou.
    generation: u64,
    spec_id: String,
    started_at: String,
    /// Comando que roda no `workspace` (o `run_command` normalizado); sai redigido pela API.
    command: String,
    root: std::path::PathBuf,
    compose: Arc<ComposeEnv>,
    ports: Vec<PublishedPort>,
    /// Avisos curtos (porta do host trocada).
    notices: Vec<String>,
    /// Os mesmos avisos com `code` e `params` (spec 102), na mesma ordem.
    notice_codes: Vec<Value>,
    /// Modo observado (spec 130 T02); muda de `running` para `page` e de ambos para `finished`.
    mode: RunMode,
    /// Porta do próprio Nants: nunca sondada.
    nants_port: u16,
    /// Arquivos da pasta antes da partida (lista do que um "terminou" criou ou mudou).
    before: Option<Arc<TreeSnapshot>>,
    /// Resultado do "terminou".
    finished: Option<FinishedRun>,
    /// Parada em andamento (`down`): o `ps --all` desse intervalo não vira "terminou" nem "página".
    stopping: bool,
}

/// O que um programa que rodou e terminou deixou (spec 130 T02, C-09 e C-10).
#[derive(Clone, Debug, Default)]
struct FinishedRun {
    /// Código de saída do `workspace` (`ps --all`); `None` se o container sumiu.
    exit_code: Option<i64>,
    /// Saída do `workspace` (`logs`), limitada pelo fim; redigida ao sair pela API.
    output: Option<String>,
    /// Arquivos da pasta do projeto criados ou mudados.
    files: ChangedFiles,
    /// Caminhos gravados fora de `/work` (`docker diff`), limitados.
    outside_work: Vec<String>,
    outside_work_total: usize,
}

fn ports_json(ports: &[PublishedPort]) -> Value {
    Value::Array(
        ports
            .iter()
            .map(|p| json!({"service": p.service, "target": p.target, "published": p.published, "url": null}))
            .collect(),
    )
}

impl FinishedRun {
    fn view(&self) -> Value {
        let files: Vec<Value> = self.files.files.iter().map(|(path, change)| json!({"path": path, "change": change.as_str()})).collect();
        json!({
            "exit_code": self.exit_code, "output": self.output, "files": files,
            "files_total": self.files.total, "files_truncated": self.files.truncated,
            "outside_work": self.outside_work, "outside_work_total": self.outside_work_total,
            "outside_work_truncated": self.outside_work_total > self.outside_work.len(),
        })
    }
}

impl DockerApp {
    fn exit_code(&self) -> Option<i64> {
        self.finished.as_ref().and_then(|f| f.exit_code)
    }

    /// `state` segue o contrato antigo (`running` | `exited`, este com o `exit_code`); `mode` diz a
    /// forma (`page | running | finished`) e `finished` traz a saída e os arquivos.
    fn view(&self) -> Value {
        let done = self.mode == RunMode::Finished;
        json!({
            "state": if done { "exited" } else { "running" }, "mode": self.mode.as_str(), "spec_id": self.spec_id, "url": null,
            "command": self.command,
            "started_at": self.started_at, "exit_code": self.exit_code(), "ports": ports_json(&self.ports), "notices": self.notices,
            "notice_codes": self.notice_codes,
            "finished": self.finished.as_ref().map_or(Value::Null, FinishedRun::view),
        })
    }
}

/// Programa de init que o Docker injeta com `init: true` (o `-run` usa): não foi o app que gravou.
const DOCKER_INIT: &str = "/usr/sbin/docker-init";

/// Os caminhos que dizem onde o app gravou: sem as pastas que só aparecem por conter outro caminho
/// da lista (`/tmp` diante de `/tmp/cache.db`) e sem o init injetado pelo Docker.
fn outside_leaves(mut paths: Vec<String>) -> Vec<String> {
    // Ordenado por componentes, os descendentes de `p` vêm logo depois dele (`/a`, `/a/b`, `/a-b`;
    // pela ordem do texto, `/a-b` e `/a.txt` cairiam no meio): basta olhar o vizinho seguinte.
    // O(n log n), sem alocação no laço.
    paths.sort_unstable_by(|a, b| a.split('/').cmp(b.split('/')));
    paths.dedup();
    let is_ancestor = |p: &str, next: Option<&String>| {
        next.is_some_and(|n| n.len() > p.len() && n.starts_with(p) && n.as_bytes().get(p.len()) == Some(&b'/'))
    };
    let mut out = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        if p != DOCKER_INIT && !is_ancestor(p, paths.get(i + 1)) {
            out.push(p.clone());
        }
    }
    out
}

/// Coleta o resultado de um `workspace` que saiu: o código (`ps --all`), a saída (`logs`), os
/// arquivos da pasta (dois instantâneos) e o que foi gravado fora de `/work` (`docker diff`, a
/// função única da 129/T02). Falha de uma parte não derruba as outras.
fn collect_finished(compose: &ComposeEnv, root: &Path, state: &RunState, before: Option<&TreeSnapshot>) -> FinishedRun {
    let output = match compose.workspace_output(root, OUTPUT_LINES) {
        Ok(text) => {
            let skip = text.chars().count().saturating_sub(OUTPUT_CHARS);
            Some(text.chars().skip(skip).collect())
        }
        Err(e) => {
            eprintln!("aviso: saída do -run indisponível: {e}");
            None
        }
    };
    let files = before.map(|b| b.changes(&TreeSnapshot::scan(root), MAX_FILES)).unwrap_or_default();
    let outside = match state.container() {
        Some(name) => compose.container_diff(root, name).map(|d| outside_leaves(written_outside_work(&d))).unwrap_or_else(|e| {
            eprintln!("aviso: docker diff do -run indisponível: {e}");
            Vec::new()
        }),
        None => Vec::new(),
    };
    let outside_work_total = outside.len();
    FinishedRun {
        exit_code: state.exit_code(),
        output,
        files,
        outside_work: outside.into_iter().take(MAX_OUTSIDE).collect(),
        outside_work_total,
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
    /// Intervalo da sondagem leve do modo (zero desliga; [`WATCH_INTERVAL`]).
    watch: Duration,
    /// Console do projeto (spec 130 T03): encerrado junto com o app (parar, remoção, run, shutdown).
    console: super::console::ConsoleManager,
    /// Última falha que pede o Arquiteto, por projeto (spec 130 T04), partilhada com o Console.
    failures: super::app_guide::FailureLog,
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

/// Número da próxima partida.
fn next_generation() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static GEN: AtomicU64 = AtomicU64::new(1);
    GEN.fetch_add(1, Ordering::SeqCst)
}

impl AppManager {
    pub fn new(events: EventBus) -> Self {
        let failures = super::app_guide::FailureLog::default();
        Self {
            docker: Arc::new(Mutex::new(HashMap::new())),
            starting: Arc::new(Mutex::new(std::collections::HashSet::new())),
            observe: RUN_OBSERVE_WINDOW,
            probe_timeout: EMBED_PROBE_TIMEOUT,
            watch: WATCH_INTERVAL,
            console: super::console::ConsoleManager::new(events.clone()).with_failures(failures.clone()),
            failures,
            events,
        }
    }

    /// Última falha da janela por projeto (spec 130 T04).
    pub fn failures(&self) -> &super::app_guide::FailureLog {
        &self.failures
    }

    /// Console do projeto (spec 130 T03).
    pub fn console(&self) -> &super::console::ConsoleManager {
        &self.console
    }

    /// Limites do Console (testes).
    pub fn with_console_limits(mut self, limits: super::console::ConsoleLimits) -> Self {
        self.console = self.console.with_limits(limits);
        self
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

    /// Intervalo da sondagem leve do modo (testes; zero desliga).
    pub fn with_watch_interval(mut self, interval: Duration) -> Self {
        self.watch = interval;
        self
    }

    /// Timeout atual da sonda `embeddable`.
    pub fn probe_timeout(&self) -> Duration {
        self.probe_timeout
    }

    /// `true` se há app vivo no projeto (`page` ou `running`). Um "terminou" mantido só para
    /// mostrar o resultado não conta: outra partida o substitui.
    pub fn is_running(&self, project_id: &str) -> bool {
        self.get_docker(project_id).is_some_and(|a| a.mode != RunMode::Finished)
    }

    /// App vivo ou start em andamento.
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

    /// Sobe o projeto `-run` (`up -d --build`), observa o `workspace` pela janela curta e registra o
    /// modo (spec 130 T02): `finished` (saiu, com o código, a saída e os arquivos), `page` (uma porta
    /// responde HTTP) ou `running`. Em falha do `up`, tenta derrubar o que subiu (sem `-v`) e devolve
    /// a mensagem. Um "terminou" anterior do projeto é derrubado antes.
    pub async fn launch_docker(
        &self,
        project_id: &str,
        root: &Path,
        spec_id: &str,
        compose: Arc<ComposeEnv>,
        run_command: &str,
        nants_port: u16,
    ) -> Result<Value, LaunchError> {
        let _mark = {
            let mut starting = self.starting.lock().unwrap_or_else(PoisonError::into_inner);
            if self.is_running(project_id) || !starting.insert(project_id.to_string()) {
                return Err(LaunchError::AlreadyRunning);
            }
            StartMark { set: Arc::clone(&self.starting), id: project_id.to_string() }
        };
        // O container encerrado de um "terminou" fica até o app subir de novo (C-09).
        if self.get_docker(project_id).is_some() && !self.stop_docker(project_id, REASON_STOPPED).await {
            return Err(LaunchError::Docker("docker compose down do -run anterior falhou".into()));
        }
        let scan_root = root.to_path_buf();
        let before = tokio::task::spawn_blocking(move || TreeSnapshot::scan(&scan_root)).await.ok().map(Arc::new);
        let (c, r, cmd) = (Arc::clone(&compose), root.to_path_buf(), run_command.to_string());
        let window = self.observe;
        let snapshot = before.clone();
        let up = tokio::task::spawn_blocking(move || {
            let attempt = c.up_run(&r, &cmd).and_then(|()| c.observe_run(&r, window, &[nants_port]));
            let attempt = attempt.map_err(|e| match c.workspace_logs(&r, FAIL_LOG_LINES) {
                Ok(logs) if !logs.trim().is_empty() => format!("{e}\n--- logs do workspace ---\n{}", logs.trim_end()),
                _ => e,
            });
            if attempt.is_err() {
                let _ = c.down_with(&r, false);
            }
            attempt.map(|seen| {
                let finished = (seen.mode == RunMode::Finished).then(|| collect_finished(&c, &r, &seen.state, snapshot.as_deref()));
                (seen, finished)
            })
        })
        .await;
        let remaps = compose.port_remaps();
        let notices: Vec<String> = remaps.iter().map(|(want, used)| format!("Porta {want} ocupada; usando {used}")).collect();
        let notice_codes: Vec<Value> = remaps
            .iter()
            .map(|(want, used)| json!({"code": CODE_PORT_REMAPPED, "params": {"wanted": want, "used": used}}))
            .collect();
        let (seen, finished) = match up {
            Ok(Ok(ok)) => ok,
            Ok(Err(e)) => return Err(LaunchError::Docker(e)),
            Err(_) => return Err(LaunchError::Spawn),
        };
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let generation = next_generation();
        let app = DockerApp {
            generation,
            spec_id: spec_id.to_string(),
            started_at: format_utc(secs),
            command: run_command.to_string(),
            root: root.to_path_buf(),
            compose,
            ports: seen.state.ports,
            notices,
            notice_codes,
            mode: seen.mode,
            nants_port,
            before,
            finished,
            stopping: false,
        };
        let view = app.view();
        let exit_code = app.exit_code();
        self.docker.lock().unwrap_or_else(PoisonError::into_inner).insert(project_id.to_string(), app);
        self.events.publish(ApiEvent::new(
            EventType::AppStarted,
            Some(project_id.to_string()),
            json!({
                "spec_id": spec_id, "url": null, "ports": view["ports"], "notices": view["notices"],
                "notice_codes": view["notice_codes"], "mode": seen.mode.as_str(),
            }),
        ));
        if seen.mode == RunMode::Finished {
            self.publish_exited(project_id, exit_code, REASON_EXITED, RunMode::Finished);
        } else {
            self.spawn_watch(project_id, generation);
        }
        Ok(view)
    }

    fn publish_exited(&self, project_id: &str, code: Option<i64>, reason: &str, mode: RunMode) {
        self.events.publish(ApiEvent::new(
            EventType::AppExited,
            Some(project_id.to_string()),
            json!({"code": code, "reason": reason, "mode": mode.as_str()}),
        ));
    }

    fn publish_mode(&self, project_id: &str, spec_id: &str, mode: RunMode, previous: RunMode, exit_code: Option<i64>) {
        self.events.publish(ApiEvent::new(
            EventType::AppModeChanged,
            Some(project_id.to_string()),
            json!({"spec_id": spec_id, "mode": mode.as_str(), "previous": previous.as_str(), "exit_code": exit_code}),
        ));
    }

    /// Spec 130 T02 (C-08): confere o `workspace` pelo `ps --all`; se saiu, o app vira "terminou"
    /// (código real, saída, arquivos) com `app_mode_changed` e `app_exited`. Chamado a cada consulta
    /// do estado e pela sondagem leve.
    pub async fn refresh(&self, project_id: &str) {
        let Some(app) = self.get_docker(project_id) else { return };
        if app.mode == RunMode::Finished || app.stopping {
            return;
        }
        let (c, r) = (Arc::clone(&app.compose), app.root.clone());
        let before = app.before.clone();
        let done = tokio::task::spawn_blocking(move || {
            let state = c.run_state(&r).ok()?;
            state.workspace_exited().then(|| collect_finished(&c, &r, &state, before.as_deref()))
        })
        .await;
        if let Ok(Some(run)) = done {
            self.finish(project_id, app.generation, run);
        }
    }

    fn finish(&self, project_id: &str, generation: u64, run: FinishedRun) {
        let code = run.exit_code;
        let (spec_id, previous) = {
            let mut map = self.docker.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(app) = map.get_mut(project_id) else { return };
            if app.generation != generation || app.mode == RunMode::Finished || app.stopping {
                return;
            }
            let previous = app.mode;
            app.mode = RunMode::Finished;
            app.ports.clear();
            app.finished = Some(run);
            (app.spec_id.clone(), previous)
        };
        self.publish_mode(project_id, &spec_id, RunMode::Finished, previous, code);
        self.publish_exited(project_id, code, REASON_EXITED, RunMode::Finished);
    }

    /// Spec 130 T02 (C-08): uma porta respondeu HTTP; `running` vira `page`. `true` se mudou.
    pub fn promote_page(&self, project_id: &str) -> bool {
        let spec_id = {
            let mut map = self.docker.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(app) = map.get_mut(project_id) else { return false };
            if app.mode != RunMode::Running || app.stopping {
                return false;
            }
            app.mode = RunMode::Page;
            app.spec_id.clone()
        };
        self.publish_mode(project_id, &spec_id, RunMode::Page, RunMode::Running, None);
        true
    }

    /// Sondagem leve enquanto a partida `generation` está viva: `ps --all` e, em `running`, as
    /// portas publicadas (menos a do Nants). Termina quando o app sai, para ou é substituído.
    fn spawn_watch(&self, project_id: &str, generation: u64) {
        if self.watch.is_zero() {
            return;
        }
        let (me, id) = (self.clone(), project_id.to_string());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(me.watch).await;
                let live = |me: &Self| me.get_docker(&id).filter(|a| a.generation == generation && a.mode != RunMode::Finished && !a.stopping);
                if live(&me).is_none() {
                    return;
                }
                me.refresh(&id).await;
                let Some(app) = live(&me) else { return };
                if app.mode != RunMode::Running {
                    continue;
                }
                for port in app.ports.iter().map(|p| p.published).filter(|p| *p != app.nants_port) {
                    if probe_port(port, me.probe_timeout).await.answered {
                        me.promote_page(&id);
                        break;
                    }
                }
            }
        });
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

    /// Para o app docker (`down`, sem `-v`). O registro só some se o `down` deu certo. O
    /// `app_exited` sai uma vez por partida: um app vivo parado aqui o publica sem código (o
    /// processo não terminou sozinho); um "terminou" já o publicou ao sair.
    async fn stop_docker(&self, project_id: &str, reason: &'static str) -> bool {
        // Marca "parando" sob o lock antes do `down`: a consulta e a sondagem desse intervalo veem o
        // container sair (143/137 ou ausente) e não podem transformar a parada num "terminou".
        let app = {
            let mut map = self.docker.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(app) = map.get_mut(project_id) else {
                return true;
            };
            app.stopping = true;
            app.clone()
        };
        let generation = app.generation;
        let down = tokio::task::spawn_blocking(move || app.compose.down_with(&app.root, false)).await;
        let ok = matches!(down, Ok(Ok(())));
        if let Ok(Err(e)) = &down {
            eprintln!("aviso: down do -run de {project_id} falhou: {e}");
        }
        let removed = {
            let mut map = self.docker.lock().unwrap_or_else(PoisonError::into_inner);
            match map.get_mut(project_id) {
                Some(a) if a.generation == generation && ok => map.remove(project_id),
                Some(a) if a.generation == generation => {
                    // O `down` falhou: o app segue registrado e volta a ser observado.
                    a.stopping = false;
                    None
                }
                _ => None,
            }
        };
        if let Some(app) = removed.filter(|a| a.mode != RunMode::Finished) {
            self.publish_exited(project_id, None, reason, app.mode);
        }
        ok
    }

    /// `true` se há app docker registrado (vivo ou "terminou" mantido) no projeto.
    pub fn is_docker_running(&self, project_id: &str) -> bool {
        self.get_docker(project_id).is_some()
    }

    /// Encerra o comando do Console (spec 130 T03) e o app do projeto (se houver): `down` do `-run`,
    /// sem `-v`. Idempotente.
    pub async fn stop(&self, project_id: &str, reason: &'static str) {
        self.console.stop(project_id, reason).await;
        self.stop_app_only(project_id, reason).await;
        if reason == REASON_PROJECT_REMOVED {
            self.failures.clear(project_id);
        }
    }

    /// Encerra só o app do projeto (o botão parar do app): o comando do Console segue. Sem app
    /// registrado, os serviços de apoio que um comando do Console levantou no `-run` são
    /// derrubados (`down` sem `-v`), salvo com um comando em curso.
    pub async fn stop_app_only(&self, project_id: &str, reason: &'static str) {
        if self.is_docker_running(project_id) {
            if self.stop_docker(project_id, reason).await {
                // O `down` do app derrubou também o que o Console tinha levantado.
                self.console.take_raised(project_id);
            }
            return;
        }
        if self.console.is_busy(project_id) {
            return;
        }
        let Some(raised) = self.console.take_raised(project_id) else { return };
        let (root, compose) = (raised.0.clone(), Arc::clone(&raised.1));
        let down = tokio::task::spawn_blocking(move || compose.down_with(&root, false)).await;
        if !matches!(down, Ok(Ok(()))) {
            if let Ok(Err(e)) = &down {
                eprintln!("aviso: down do -run de {project_id} falhou: {e}");
            }
            self.console.restore_raised(project_id, raised);
        }
    }

    /// Encerra todos os comandos do Console e todos os apps (shutdown), em paralelo.
    pub async fn stop_all(&self, reason: &'static str) {
        self.console.stop_all(reason).await;
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

fn reject(status: StatusCode, code: &'static str, reason: &'static str) -> Response {
    ApiError::coded(status, code, Map::new()).with_reason(reason).into_response()
}

fn plain(status: StatusCode, code: &'static str) -> Response {
    ApiError::coded(status, code, Map::new()).into_response()
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
        return plain(StatusCode::BAD_REQUEST, ERR_BODY);
    };
    if !input.confirm {
        return plain(StatusCode::BAD_REQUEST, ERR_CONFIRM);
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
        Err(ReserveError::NotFound) => return plain(StatusCode::NOT_FOUND, ERR_NOT_FOUND),
        Err(ReserveError::Busy) => {
            return reject(StatusCode::CONFLICT, ERR_BUSY, "busy");
        }
        Err(ReserveError::ReportInProgress) => {
            return reject(StatusCode::CONFLICT, REPORT_BUSY_CODE, "report_in_progress");
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
        return plain(StatusCode::INTERNAL_SERVER_ERROR, ERR_INTERNAL);
    };
    let Some(spec) = spec else {
        return plain(StatusCode::NOT_FOUND, ERR_SPEC_NOT_FOUND);
    };
    if spec.frontmatter.status != SpecStatus::Completed {
        return reject(StatusCode::CONFLICT, ERR_NOT_COMPLETED, "spec_not_completed");
    }
    if state.apps().is_running(&id) {
        return reject(StatusCode::CONFLICT, ERR_RUNNING, "already_running");
    }
    // Projeto Compose `-run` na raiz do projeto; projeto sem ambiente docker é recusado (spec 074).
    let Some(compose) = state.compose().for_project_lan(&root, &id, ComposeKind::Run, state.lan_view_ports()) else {
        state.apps().failures().record(&id, &input.spec_id, super::runs::PROJECT_NOT_DOCKER_CODE, "");
        return reject(
            StatusCode::CONFLICT,
            super::runs::PROJECT_NOT_DOCKER_CODE,
            super::runs::REASON_ENV_NOT_DOCKER,
        );
    };
    let Some(raw) = spec.frontmatter.run_command.as_deref() else {
        return reject(StatusCode::UNPROCESSABLE_ENTITY, ERR_NO_COMMAND, "no_run_command");
    };
    let Ok(inner) = crate::sdd::docker_command::normalize(raw) else {
        return reject(StatusCode::UNPROCESSABLE_ENTITY, ERR_RUN_DOCKER, "run_command_docker");
    };
    match state.apps().launch_docker(&id, &root, &input.spec_id, compose, &inner, state.bound_port()).await {
        Ok(view) => {
            state.apps().failures().clear(&id);
            Json(app_view(&state, &id, view, &headers, true).await).into_response()
        }
        Err(LaunchError::AlreadyRunning) => reject(StatusCode::CONFLICT, ERR_RUNNING, "already_running"),
        Err(LaunchError::Docker(detail)) => {
            let secrets = state.secret_set().await;
            let clean = scrub_text(&secrets, &detail);
            let skip = clean.chars().count().saturating_sub(DETAIL_CHARS);
            let detail: String = clean.chars().skip(skip).collect();
            // Faixa de visualização esgotada (spec 097): mesmo caminho do conflito de porta, com a
            // mensagem da faixa e os números reais. `code`/`params` (spec 102) para a interface traduzir.
            if let Some(line) = clean.lines().find(|l| l.starts_with(crate::config::VIEW_PORTS_EXHAUSTED_PREFIX)) {
                let range = state.lan_view_ports();
                let params = params([("start", json!(range.map(|r| r.start))), ("end", json!(range.map(|r| r.end)))]);
                return ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, CODE_VIEW_PORTS_EXHAUSTED, params)
                    .with_reason("docker_port_in_use")
                    .with_detail(line)
                    .into_response();
            }
            if crate::harness::compose::is_port_conflict(&detail) {
                return ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, CODE_PORT_IN_USE, Map::new())
                    .with_reason("docker_port_in_use")
                    .with_detail(detail)
                    .into_response();
            }
            // Spec 130 T04: o diagnóstico fica guardado para o "Pedir ao Arquiteto para corrigir".
            state.apps().failures().record(&id, &input.spec_id, ERR_DOCKER_UP, &detail);
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, ERR_DOCKER_UP, Map::new())
                .with_reason("docker_failed")
                .with_detail(detail)
                .into_response()
        }
        Err(LaunchError::Spawn) => plain(StatusCode::INTERNAL_SERVER_ERROR, ERR_INTERNAL),
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
    state.apps().stop_app_only(&id, REASON_STOPPED).await;
    if state.apps().is_docker_running(&id) {
        return plain(StatusCode::INTERNAL_SERVER_ERROR, ERR_DOCKER_DOWN);
    }
    Json(app_view(&state, &id, state.apps().view(&id), &headers, false).await).into_response()
}

/// `GET /api/projects/:id/app`: estado, modo, links e `embeddable` (sondado de novo a cada
/// consulta). O `ps --all` de cada consulta atualiza o modo (spec 130 T02, C-08).
pub async fn get_app(State(state): State<AppState>, AxPath(id): AxPath<String>, headers: HeaderMap) -> Response {
    if let Err(e) = validate_project_id(&id) {
        return e.into_response();
    }
    if let Err(e) = find_project(&state, &id).await {
        return e.into_response();
    }
    state.apps().refresh(&id).await;
    Json(app_view(&state, &id, state.apps().view(&id), &headers, true).await).into_response()
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
/// porta (e o do app, que é o da primeira porta com link). A porta do Nants não vira link. Uma
/// porta que responde HTTP leva o modo `running` a `page` (spec 130 T02). A saída de um "terminou"
/// sai redigida.
async fn app_view(state: &AppState, project_id: &str, view: Value, headers: &HeaderMap, probe: bool) -> Value {
    let mut view = with_links(view, &request_host(state, headers), state.bound_port());
    for pointer in ["/finished/output", "/command"] {
        if let Some(field) = view.pointer_mut(pointer) {
            if let Some(text) = field.as_str() {
                let secrets = state.secret_set().await;
                *field = Value::String(scrub_text(&secrets, text));
            }
        }
    }
    if let (Some(obj), Value::Object(net)) = (view.as_object_mut(), network_info(state)) {
        obj.extend(net);
    }
    // Spec 130 T04: a última falha que pede o Arquiteto, com o próximo passo.
    let secrets = state.secret_set().await;
    let failure = super::app_guide::failure_view(state.apps().failures().get(project_id), &secrets);
    if let Some(obj) = view.as_object_mut() {
        obj.insert("failure".into(), failure);
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
    let mut answered = false;
    if probe && view.get("state").and_then(Value::as_str) == Some("running") {
        let timeout = state.apps().probe_timeout();
        // Sondas em paralelo (poucas portas, 2 s no máximo cada).
        let jobs: Vec<_> = ports.iter().map(|port| (*port, tokio::spawn(probe_port(*port, timeout)))).collect();
        for (port, job) in jobs {
            let got = job.await.unwrap_or_default();
            answered |= got.answered;
            results.insert(port, got.embeddable);
        }
    }
    if answered && view.get("mode").and_then(Value::as_str) == Some(RunMode::Running.as_str()) {
        state.apps().promote_page(project_id);
        if let Some(obj) = view.as_object_mut() {
            obj.insert("mode".into(), Value::String(RunMode::Page.as_str().into()));
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

/// Resultado da sonda de uma porta publicada.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PortProbe {
    /// A porta respondeu HTTP (modo `page`, spec 130 T02).
    pub answered: bool,
    /// Veredito `embeddable` (`None` = indeterminado).
    pub embeddable: Option<bool>,
}

/// Sonda da porta publicada: GET em `http://127.0.0.1:<porta>/` (nunca o host da requisição, contra
/// SSRF em `--lan`), sem proxy, sem redirect, com `timeout` e corpo descartado.
pub async fn probe_port(port: u16, timeout: Duration) -> PortProbe {
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(timeout)
        .build()
    else {
        return PortProbe::default();
    };
    let url = format!("http://{FALLBACK_HOST}:{port}/");
    let Ok(Ok(response)) = tokio::time::timeout(timeout, client.get(url).send()).await else {
        return PortProbe::default();
    };
    let embeddable = embeddable_from_headers(response.headers());
    // Corpo descartado: a resposta cai aqui sem ser lida.
    drop(response);
    PortProbe { answered: true, embeddable }
}

/// Sonda `embeddable` da porta publicada ([`probe_port`]). `None` = indeterminado (sem resposta,
/// erro ou timeout).
pub async fn probe_embeddable(port: u16, timeout: Duration) -> Option<bool> {
    probe_port(port, timeout).await.embeddable
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
