//! Console do projeto (spec 130 T03; na tela, "Console"): o usuário digita um comando e o Nants o
//! roda no container do ambiente aprovado do projeto, nunca no host.
//!
//! - **Console de comando único (C-11):** `compose run --rm -T` no projeto `-run`, sem TTY e com o
//!   stdin fechado, um comando por projeto por vez (409 `app_exec_busy`). O mesmo override da
//!   partida do app: `.git` e `.nants` só leitura, uid do host e a pasta pessoal da 129.
//! - **As proteções da partida (`start_app`):** corpo `{confirm: true, spec_id, command}`, guarda de
//!   origin (`security::guard`), dispositivo autorizado em modo rede (`devices::require_device`),
//!   prontidão `execute` e a reserva interna `__app__` durante a preparação. O comando passa pela
//!   normalização do `docker_command` (nada de `docker` dentro do comando).
//! - **Limites:** tempo máximo ([`EXEC_TIMEOUT`]) e saída ao vivo limitada ([`STREAM_MAX_BYTES`]).
//!   Um programa que espera entrada não recebe nada (stdin fechado) e, se estoura o tempo, o
//!   resultado traz o aviso `app_exec_timeout` do dicionário.
//! - **Parar:** `POST .../app/exec/stop`, a remoção do projeto, o início de um run e o shutdown do
//!   Nants encerram o comando em curso (o cliente é morto e o container removido pelo nome).
//! - **Histórico local (C-12):** os últimos [`HISTORY_MAX`] comandos do projeto, redigidos pelo
//!   `SecretSet`, com a origem (dispositivo e hora), em `<dados do Nants>/console/<id>.json` (0600).
//!   Repetir um comando é um novo `POST`, com a confirmação de novo.
//!
//! Eventos: `app_exec_started {exec_id, spec_id, command, started_at, origin, timeout_secs}`,
//! `app_exec_output {exec_id, seq, text, truncated}` (texto redigido, por linhas) e
//! `app_exec_finished {exec_id, status, exit_code, ...}` (`status`: `exited`, `timed_out`,
//! `stopped` ou `failed`).

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::sync::watch;

use super::app_guide::error_action;
use super::devices::Access;
use super::error::{params, ApiError};
use super::events::{validate_project_id, ApiEvent, EventBus, EventType, SecretSet};
use super::projects::{find_project, format_utc, scrub_text};
use super::safe_fs::{self, O_NOFOLLOW};
use super::spec_result::validate_spec_id;
use super::state::{ReserveError, APP_RESERVATION, REPORT_BUSY_CODE};
use super::AppState;
use crate::harness::compose::{ComposeEnv, ComposeKind, ComposeOutput, OutputSink};
use crate::harness::tree_snapshot::TreeSnapshot;
use crate::sdd::Spec;

/// Tempo máximo de um comando do Console.
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(300);
/// Teto da saída enviada ao vivo (bytes); o que passar disso não é enviado e o último pedaço
/// avisa o corte (`truncated`).
pub const STREAM_MAX_BYTES: usize = 256 * 1024;
/// Intervalo entre os envios da saída ao vivo.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(200);
/// Espera máxima do parar pelo fim do comando.
pub const STOP_WAIT: Duration = Duration::from_secs(15);
/// Comandos guardados no histórico de cada projeto.
pub const HISTORY_MAX: usize = 50;
/// Tamanho máximo do comando digitado (caracteres).
pub const MAX_COMMAND_CHARS: usize = 4000;
/// Saída guardada no resultado (caracteres, contados do FIM).
const RESULT_OUTPUT_CHARS: usize = 16 * 1024;
/// Arquivos listados no resultado (o resto é contado e avisado).
const MAX_FILES: usize = 100;
/// Linha sem fim maior que isto é enviada mesmo assim (a saída não fica parada).
const PARTIAL_MAX: usize = 8 * 1024;
/// Teto do arquivo de histórico lido.
const HISTORY_MAX_BYTES: u64 = 1024 * 1024;
/// Pasta do histórico sob o diretório de dados do Nants.
const HISTORY_DIR_REL: &str = "console";
const MAX_SPEC_BYTES: u64 = 256 * 1024;
/// Limite (caracteres, contados do FIM) do detalhe de uma falha do Docker.
const DETAIL_CHARS: usize = 1600;

pub const STATUS_RUNNING: &str = "running";
pub const STATUS_EXITED: &str = "exited";
pub const STATUS_TIMED_OUT: &str = "timed_out";
pub const STATUS_STOPPED: &str = "stopped";
pub const STATUS_FAILED: &str = "failed";

/// Códigos (spec 115) do Console; os textos ficam nos dicionários (`serverError.<code>`).
pub const CODE_EXEC_BUSY: &str = "app_exec_busy";
pub const CODE_EXEC_COMMAND_INVALID: &str = "app_exec_command_invalid";
pub const CODE_EXEC_COMMAND_DOCKER: &str = "app_exec_command_docker";
/// Aviso do resultado: o comando passou do tempo (`params.seconds`); explica que o Console não tem
/// entrada.
pub const CODE_EXEC_TIMEOUT: &str = "app_exec_timeout";
/// Aviso do resultado: o Docker falhou ao rodar o comando (detalhe redigido em `detail`).
pub const CODE_EXEC_DOCKER_FAILED: &str = "app_exec_docker_failed";
const ERR_CONFIRM: &str = "app_confirm_required";
const ERR_BODY: &str = "body_invalid";
const ERR_NOT_FOUND: &str = "project_not_found";
const ERR_SPEC_NOT_FOUND: &str = "spec_not_found";
const ERR_BUSY: &str = "run_active";
const ERR_INTERNAL: &str = "internal";

/// Limites do Console (os testes usam prazos curtos).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleLimits {
    pub timeout: Duration,
    pub stream_bytes: usize,
    pub flush: Duration,
    pub stop_wait: Duration,
}

impl Default for ConsoleLimits {
    fn default() -> Self {
        Self { timeout: EXEC_TIMEOUT, stream_bytes: STREAM_MAX_BYTES, flush: FLUSH_INTERVAL, stop_wait: STOP_WAIT }
    }
}

/// Comando em curso num projeto.
#[derive(Clone)]
struct Active {
    view: Value,
    container: String,
    root: PathBuf,
    compose: Arc<ComposeEnv>,
    cancel: Arc<AtomicBool>,
    /// Por que foi parado (o primeiro motivo vale).
    reason: Arc<Mutex<Option<&'static str>>>,
    done: watch::Receiver<bool>,
}

#[derive(Default)]
struct Inner {
    active: HashMap<String, Active>,
    /// Resultado do último comando de cada projeto (com a saída redigida), só em memória.
    last: HashMap<String, Value>,
    /// Projetos cujo `-run` o Console levantou (o `compose run` sobe os serviços de apoio): o
    /// próximo parar do app, run ou remoção os derruba mesmo sem app registrado.
    raised: HashMap<String, (PathBuf, Arc<ComposeEnv>)>,
}

/// Gerenciador do Console (um comando por projeto). Clones compartilham o estado.
#[derive(Clone)]
pub struct ConsoleManager {
    inner: Arc<Mutex<Inner>>,
    /// Serializa as gravações do histórico.
    history: Arc<Mutex<()>>,
    events: EventBus,
    limits: ConsoleLimits,
    /// Última falha que pede o Arquiteto (spec 130 T04), partilhada com o app.
    failures: super::app_guide::FailureLog,
}

impl std::fmt::Debug for ConsoleManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConsoleManager")
    }
}

/// O que um comando precisa para rodar (montado pelo handler depois das proteções).
pub struct ExecRequest {
    pub project_id: String,
    pub spec_id: String,
    /// Comando que roda (já normalizado).
    pub command: String,
    /// Comando como o usuário digitou, redigido (histórico, eventos e visão).
    pub shown: String,
    /// A redação mudou o texto: repetir não reproduz o comando original.
    pub redacted: bool,
    pub origin: Value,
    pub root: PathBuf,
    pub compose: Arc<ComposeEnv>,
    pub secrets: Arc<SecretSet>,
    pub data_dir: Option<PathBuf>,
}

/// Já há um comando em curso no projeto.
#[derive(Debug, PartialEq, Eq)]
pub struct ExecBusy;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Identificador do comando: hora (ms, hexadecimal) e um contador do processo.
fn next_exec_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("{ms:x}-{}", SEQ.fetch_add(1, Ordering::SeqCst))
}

/// Saída ao vivo: junta os pedaços de cada fluxo (stdout e stderr, cada um com a sua linha em
/// formação, para uma escrita de um nunca partir a linha do outro) e entrega linhas inteiras, até
/// o teto total.
#[derive(Debug, Default)]
struct Stream {
    bufs: [Vec<u8>; 2],
    /// Bytes já entregues.
    sent: usize,
    seq: u64,
    /// O teto foi atingido: o resto é descartado.
    overflow: bool,
    /// O corte já foi avisado (nada mais sai).
    closed: bool,
}

/// Estados das linhas de progresso que o `compose run` escreve no stderr ao levantar a rede, os
/// volumes e os serviços de apoio (" Network x  Creating"). Um estado de erro não está aqui: a
/// linha fica.
const COMPOSE_PROGRESS_STATES: &[&str] = &[
    "Creating", "Created", "Starting", "Started", "Running", "Waiting", "Healthy", "Recreate", "Recreated", "Stopping",
    "Stopped", "Removing", "Removed", "Pulling", "Pulled", "Built",
];

/// `0.1s`, o tempo que algumas versões do Compose põem no fim da linha de progresso.
fn is_seconds(token: &str) -> bool {
    token.strip_suffix('s').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// Linha de progresso do Compose (spec 130 T05): " Network nants-x-run_default  Creating",
/// "✔ Container nants-x-run-db-1  Started 0.4s" ou "[+] Running 2/2". Os erros do Compose (estado
/// `Error`, `Error response from daemon`, a saída do build) não são progresso e ficam.
pub fn is_compose_progress_line(line: &str) -> bool {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('✔').map_or(trimmed, str::trim_start);
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    match tokens.as_slice() {
        ["[+]", verb, count] => {
            matches!(*verb, "Running" | "Creating" | "Starting" | "Stopping" | "Removing" | "Pulling")
                && count.split_once('/').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty() && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit()))
        }
        [kind, _name, state, rest @ ..] => {
            matches!(*kind, "Network" | "Container" | "Volume" | "Image")
                && COMPOSE_PROGRESS_STATES.contains(state)
                && (rest.is_empty() || (rest.len() == 1 && rest.first().is_some_and(|t| is_seconds(t))))
        }
        _ => false,
    }
}

/// Tira as linhas de progresso do Compose de um texto, mantendo o resto como veio.
pub fn strip_compose_progress(text: &str) -> String {
    text.split_inclusive('\n').filter(|line| !is_compose_progress_line(line)).collect()
}

/// [`strip_compose_progress`] sobre bytes (linhas inválidas em UTF-8 ficam).
fn strip_progress_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if std::str::from_utf8(line).is_ok_and(is_compose_progress_line) {
            continue;
        }
        out.extend_from_slice(line);
    }
    out
}

fn line_end(buf: &[u8]) -> Option<usize> {
    buf.iter().rposition(|b| *b == b'\n' || *b == b'\r').map(|i| i + 1)
}

impl Stream {
    fn push(&mut self, stream: usize, data: &[u8], cap: usize) {
        if self.overflow || self.closed {
            return;
        }
        let used = self.sent + self.bufs.iter().map(Vec::len).sum::<usize>();
        let room = cap.saturating_sub(used);
        if data.len() > room {
            self.overflow = true;
        }
        self.bufs[stream.min(1)].extend_from_slice(&data[..room.min(data.len())]);
    }

    /// Próximo pedaço a enviar: as linhas inteiras de cada fluxo (com `force`, no fim, também a
    /// linha sem fim; uma linha sem fim maior que [`PARTIAL_MAX`] sai mesmo assim). No teto, a
    /// linha parcial de cada fluxo é descartada (um segredo cortado nunca sai pela metade) e o
    /// pedaço avisa o corte. `(seq, texto, truncated)`.
    fn take(&mut self, force: bool) -> Option<(u64, String, bool)> {
        if self.closed {
            return None;
        }
        let truncated = self.overflow;
        let mut out = Vec::new();
        let mut drained = 0;
        for (which, buf) in self.bufs.iter_mut().enumerate() {
            let cut = if truncated {
                line_end(buf).unwrap_or(0)
            } else if force {
                buf.len()
            } else {
                match line_end(buf) {
                    Some(n) => n,
                    None if buf.len() > PARTIAL_MAX => buf.len(),
                    None => 0,
                }
            };
            let part: Vec<u8> = buf.drain(..cut).collect();
            drained += part.len();
            // O progresso do Compose (rede, volumes e serviços de apoio subindo) sai no stderr.
            if which == 1 {
                out.extend(strip_progress_bytes(&part));
            } else {
                out.extend(part);
            }
            if truncated {
                buf.clear();
            }
        }
        // O teto conta o que saiu do fluxo, mostrado ou não.
        self.sent += drained;
        if out.is_empty() && !truncated {
            return None;
        }
        if truncated {
            self.closed = true;
        }
        self.seq += 1;
        Some((self.seq, String::from_utf8_lossy(&out).into_owned(), truncated))
    }
}

/// Aviso do resultado quando o Docker falhou ao rodar o comando, com o próximo passo.
fn docker_failed_notice() -> Value {
    json!({"code": CODE_EXEC_DOCKER_FAILED, "params": {}, "action": error_action(CODE_EXEC_DOCKER_FAILED)})
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Fim do texto com no máximo `max` caracteres.
fn tail_chars(text: &str, max: usize) -> (String, bool) {
    let count = text.chars().count();
    let skip = count.saturating_sub(max);
    (text.chars().skip(skip).collect(), skip > 0)
}

impl ConsoleManager {
    pub fn new(events: EventBus) -> Self {
        Self { inner: Arc::default(), history: Arc::default(), events, limits: ConsoleLimits::default(), failures: Default::default() }
    }

    /// Registro de falhas partilhado com o app (spec 130 T04).
    pub fn with_failures(mut self, failures: super::app_guide::FailureLog) -> Self {
        self.failures = failures;
        self
    }

    /// Limites (testes).
    pub fn with_limits(mut self, limits: ConsoleLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn limits(&self) -> ConsoleLimits {
        self.limits
    }

    /// Tira a marca de "o Console levantou o `-run`" do projeto, devolvendo a raiz e o Compose.
    pub fn take_raised(&self, project_id: &str) -> Option<(PathBuf, Arc<ComposeEnv>)> {
        lock(&self.inner).raised.remove(project_id)
    }

    /// Recoloca a marca (o `down` falhou).
    pub fn restore_raised(&self, project_id: &str, raised: (PathBuf, Arc<ComposeEnv>)) {
        lock(&self.inner).raised.entry(project_id.to_string()).or_insert(raised);
    }

    /// `true` se o Console levantou o `-run` do projeto e ninguém o derrubou ainda.
    pub fn has_raised(&self, project_id: &str) -> bool {
        lock(&self.inner).raised.contains_key(project_id)
    }

    /// `true` se há comando em curso no projeto.
    pub fn is_busy(&self, project_id: &str) -> bool {
        lock(&self.inner).active.contains_key(project_id)
    }

    fn publish(&self, kind: EventType, project_id: &str, payload: Value) {
        self.events.publish(ApiEvent::new(kind, Some(project_id.to_string()), payload));
    }

    /// Registra o comando (um por projeto), grava a entrada do histórico e o põe para rodar.
    /// Devolve a visão do comando em curso.
    pub async fn launch(&self, req: ExecRequest) -> Result<Value, ExecBusy> {
        let exec_id = next_exec_id();
        let container = req.compose.console_container_name(&exec_id);
        let started_at = format_utc(now_secs());
        let view = json!({
            "exec_id": exec_id, "spec_id": req.spec_id, "command": req.shown, "redacted": req.redacted,
            "started_at": started_at, "origin": req.origin, "status": STATUS_RUNNING,
            "timeout_secs": self.limits.timeout.as_secs(), "stoppable": true,
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = watch::channel(false);
        {
            let mut inner = lock(&self.inner);
            if inner.active.contains_key(&req.project_id) {
                return Err(ExecBusy);
            }
            inner.raised.insert(req.project_id.clone(), (req.root.clone(), Arc::clone(&req.compose)));
            inner.active.insert(
                req.project_id.clone(),
                Active {
                    view: view.clone(),
                    container: container.clone(),
                    root: req.root.clone(),
                    compose: Arc::clone(&req.compose),
                    cancel: Arc::clone(&cancel),
                    reason: Arc::default(),
                    done: done_rx,
                },
            );
        }
        let entry = json!({
            "exec_id": exec_id, "spec_id": req.spec_id, "command": req.shown, "redacted": req.redacted,
            "origin": req.origin, "at": started_at, "finished_at": null, "status": STATUS_RUNNING, "exit_code": null,
        });
        self.record(req.data_dir.clone(), req.project_id.clone(), entry).await;
        self.publish(
            EventType::AppExecStarted,
            &req.project_id,
            json!({
                "exec_id": exec_id, "spec_id": req.spec_id, "command": req.shown, "started_at": started_at,
                "origin": req.origin, "timeout_secs": self.limits.timeout.as_secs(),
            }),
        );
        let me = self.clone();
        tokio::spawn(async move { me.run(req, exec_id, container, cancel, done_tx).await });
        Ok(view)
    }

    /// Roda o comando, envia a saída ao vivo e registra o resultado.
    async fn run(&self, req: ExecRequest, exec_id: String, container: String, cancel: Arc<AtomicBool>, done: watch::Sender<bool>) {
        let started = Instant::now();
        let scan_root = req.root.clone();
        let before = tokio::task::spawn_blocking(move || TreeSnapshot::scan(&scan_root)).await.ok();
        let stream = Arc::new(Mutex::new(Stream::default()));
        let cap = self.limits.stream_bytes;
        let sink: OutputSink = {
            let stream = Arc::clone(&stream);
            Arc::new(move |which: usize, data: &[u8]| lock(&stream).push(which, data, cap))
        };
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let ticker = {
            let (me, stream, id, exec, secrets) = (self.clone(), Arc::clone(&stream), req.project_id.clone(), exec_id.clone(), Arc::clone(&req.secrets));
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(me.limits.flush) => {}
                        _ = stop_rx.changed() => return,
                    }
                    me.flush(&stream, &id, &exec, &secrets, false);
                }
            })
        };
        let ran: Option<Result<ComposeOutput, String>> = if cancel.load(Ordering::SeqCst) {
            None
        } else {
            let (c, root, name, cmd, timeout, flag) =
                (Arc::clone(&req.compose), req.root.clone(), container.clone(), req.command.clone(), self.limits.timeout, Arc::clone(&cancel));
            tokio::task::spawn_blocking(move || c.console_run(&root, &name, &cmd, timeout, sink, flag)).await.ok()
        };
        let _ = stop_tx.send(true);
        let _ = ticker.await;
        self.flush(&stream, &req.project_id, &exec_id, &req.secrets, true);
        let stopped = cancel.load(Ordering::SeqCst);
        let clean_exit = matches!(&ran, Some(Ok(o)) if !o.timed_out && o.code.is_some());
        if !clean_exit || stopped {
            // O cliente foi morto (tempo, parar) ou falhou: o container pode seguir vivo.
            let (c, root, name) = (Arc::clone(&req.compose), req.root.clone(), container.clone());
            let _ = tokio::task::spawn_blocking(move || c.remove_container(&root, &name)).await;
        }
        let files = match before {
            Some(before) => {
                let scan_root = req.root.clone();
                tokio::task::spawn_blocking(move || before.changes(&TreeSnapshot::scan(&scan_root), MAX_FILES)).await.unwrap_or_default()
            }
            None => Default::default(),
        };
        let reason = self.active_reason(&req.project_id);
        let (status, exit_code, output, notice, detail) = match &ran {
            _ if stopped => (STATUS_STOPPED, ran.as_ref().and_then(|r| r.as_ref().ok()).and_then(|o| o.code), ran_output(&ran), Value::Null, None),
            Some(Ok(o)) if o.timed_out => {
                let notice = json!({"code": CODE_EXEC_TIMEOUT, "params": {"seconds": self.limits.timeout.as_secs()}, "action": error_action(CODE_EXEC_TIMEOUT)});
                (STATUS_TIMED_OUT, None, Some(o.output.clone()), notice, None)
            }
            Some(Ok(o)) => {
                // Spec 130 T05: o ambiente rodou um comando, então a falha antiga do Console não vale mais.
                self.failures.clear_code(&req.project_id, CODE_EXEC_DOCKER_FAILED);
                (STATUS_EXITED, o.code, Some(o.output.clone()), Value::Null, None)
            }
            Some(Err(e)) => {
                let (tail, _) = tail_chars(&scrub_text(&req.secrets, e), DETAIL_CHARS);
                // Spec 130 T04: o diagnóstico fica guardado para o "Pedir ao Arquiteto para corrigir".
                self.failures.record(&req.project_id, &req.spec_id, CODE_EXEC_DOCKER_FAILED, &tail);
                (STATUS_FAILED, None, None, docker_failed_notice(), Some(tail))
            }
            None => {
                self.failures.record(&req.project_id, &req.spec_id, CODE_EXEC_DOCKER_FAILED, "");
                (STATUS_FAILED, None, None, docker_failed_notice(), None)
            }
        };
        let (output, output_truncated) = match output {
            Some(text) => {
                let (tail, cut) = tail_chars(&scrub_text(&req.secrets, &strip_compose_progress(&text)), RESULT_OUTPUT_CHARS);
                (Value::String(tail), cut)
            }
            None => (Value::Null, false),
        };
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let finished_at = format_utc(now_secs());
        let file_list: Vec<Value> = files.files.iter().map(|(p, c)| json!({"path": p, "change": c.as_str()})).collect();
        let summary = json!({
            "exec_id": exec_id, "status": status, "exit_code": exit_code, "duration_ms": duration_ms,
            "finished_at": finished_at, "reason": if stopped { reason.map_or(Value::Null, |r| Value::String(r.into())) } else { Value::Null },
            "files": file_list, "files_total": files.total, "files_truncated": files.truncated,
            "notice": notice, "detail": detail,
        });
        let mut last = lock(&self.inner).active.get(&req.project_id).map(|a| a.view.clone()).unwrap_or_else(|| json!({}));
        if let (Some(obj), Some(extra)) = (last.as_object_mut(), summary.as_object()) {
            obj.extend(extra.clone());
            obj.insert("stoppable".into(), Value::Bool(false));
            obj.insert("output".into(), output);
            obj.insert("output_truncated".into(), Value::Bool(output_truncated));
        }
        self.finish_history(req.data_dir.clone(), req.project_id.clone(), &exec_id, status, exit_code, &finished_at).await;
        {
            let mut inner = lock(&self.inner);
            inner.active.remove(&req.project_id);
            inner.last.insert(req.project_id.clone(), last);
        }
        self.publish(EventType::AppExecFinished, &req.project_id, summary);
        let _ = done.send(true);
    }

    fn active_reason(&self, project_id: &str) -> Option<&'static str> {
        let reason = lock(&self.inner).active.get(project_id).map(|a| Arc::clone(&a.reason))?;
        let got = *lock(&reason);
        got
    }

    /// Envia o pedaço pronto da saída, redigido.
    fn flush(&self, stream: &Mutex<Stream>, project_id: &str, exec_id: &str, secrets: &SecretSet, force: bool) {
        let Some((seq, text, truncated)) = lock(stream).take(force) else { return };
        let text = scrub_text(secrets, &text);
        self.publish(EventType::AppExecOutput, project_id, json!({"exec_id": exec_id, "seq": seq, "text": text, "truncated": truncated}));
    }

    /// Para o comando em curso do projeto (se houver) e espera o fim dele por até
    /// [`ConsoleLimits::stop_wait`]. `true` se havia comando.
    pub async fn stop(&self, project_id: &str, reason: &'static str) -> bool {
        let Some(active) = lock(&self.inner).active.get(project_id).cloned() else {
            return false;
        };
        lock(&active.reason).get_or_insert(reason);
        active.cancel.store(true, Ordering::SeqCst);
        let (c, root, name) = (Arc::clone(&active.compose), active.root.clone(), active.container.clone());
        let _ = tokio::task::spawn_blocking(move || c.remove_container(&root, &name)).await;
        let mut done = active.done;
        let _ = tokio::time::timeout(self.limits.stop_wait, done.wait_for(|d| *d)).await;
        true
    }

    /// Para todos os comandos em curso (shutdown), em paralelo.
    pub async fn stop_all(&self, reason: &'static str) {
        let ids: Vec<String> = lock(&self.inner).active.keys().cloned().collect();
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

    /// Esquece o projeto removido: o último resultado e o histórico gravado.
    pub async fn forget(&self, data_dir: Option<PathBuf>, project_id: &str) {
        {
            let mut inner = lock(&self.inner);
            inner.last.remove(project_id);
            inner.raised.remove(project_id);
        }
        let (lock_ref, id) = (Arc::clone(&self.history), project_id.to_string());
        let _ = tokio::task::spawn_blocking(move || {
            let _g = lock(&lock_ref);
            if let Some(dir) = data_dir {
                let _ = safe_fs::remove_regular(&dir, &history_rel(&id));
            }
        })
        .await;
    }

    /// Visão do Console do projeto: o comando em curso, o último resultado, o histórico e os limites.
    pub async fn view(&self, data_dir: Option<PathBuf>, project_id: &str) -> Value {
        let (active, last) = {
            let inner = lock(&self.inner);
            (inner.active.get(project_id).map(|a| a.view.clone()), inner.last.get(project_id).cloned())
        };
        let id = project_id.to_string();
        let history = tokio::task::spawn_blocking(move || data_dir.map(|d| read_history(&d, &id)).unwrap_or_default()).await.unwrap_or_default();
        let newest_first: Vec<Value> = history.into_iter().rev().collect();
        json!({
            "active": active.unwrap_or(Value::Null),
            "last": last.unwrap_or(Value::Null),
            "history": newest_first,
            "limits": {"timeout_secs": self.limits.timeout.as_secs(), "output_bytes": self.limits.stream_bytes},
        })
    }

    /// Acrescenta `entry` ao histórico do projeto (mantém os [`HISTORY_MAX`] mais novos).
    async fn record(&self, data_dir: Option<PathBuf>, project_id: String, entry: Value) {
        self.edit_history(data_dir, project_id, move |list| {
            list.push(entry);
            let extra = list.len().saturating_sub(HISTORY_MAX);
            list.drain(..extra);
        })
        .await;
    }

    /// Fecha a entrada `exec_id` do histórico com o resultado.
    async fn finish_history(&self, data_dir: Option<PathBuf>, project_id: String, exec_id: &str, status: &str, code: Option<i32>, at: &str) {
        let (exec_id, status, at) = (exec_id.to_string(), status.to_string(), at.to_string());
        self.edit_history(data_dir, project_id, move |list| {
            if let Some(obj) = list.iter_mut().rev().find(|e| e.get("exec_id").and_then(Value::as_str) == Some(exec_id.as_str())).and_then(Value::as_object_mut) {
                obj.insert("status".into(), Value::String(status));
                obj.insert("exit_code".into(), code.map_or(Value::Null, Value::from));
                obj.insert("finished_at".into(), Value::String(at));
            }
        })
        .await;
    }

    async fn edit_history<F>(&self, data_dir: Option<PathBuf>, project_id: String, edit: F)
    where
        F: FnOnce(&mut Vec<Value>) + Send + 'static,
    {
        let Some(dir) = data_dir else { return };
        let lock_ref = Arc::clone(&self.history);
        let _ = tokio::task::spawn_blocking(move || {
            let _g = lock(&lock_ref);
            let mut list = read_history(&dir, &project_id);
            edit(&mut list);
            write_history(&dir, &project_id, &list)
        })
        .await;
    }
}

fn ran_output(ran: &Option<Result<ComposeOutput, String>>) -> Option<String> {
    ran.as_ref().and_then(|r| r.as_ref().ok()).map(|o| o.output.clone())
}

fn history_rel(project_id: &str) -> String {
    format!("{HISTORY_DIR_REL}/{project_id}.json")
}

/// Histórico gravado do projeto (vazio se ausente ou ilegível).
fn read_history(data_dir: &Path, project_id: &str) -> Vec<Value> {
    if validate_project_id(project_id).is_err() {
        return Vec::new();
    }
    safe_fs::read_regular(data_dir, &history_rel(project_id), HISTORY_MAX_BYTES)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.get("entries").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

/// Grava o histórico de forma atômica (temporário 0600 sem seguir link e `rename`), numa pasta
/// 0700 sem symlink no caminho.
fn write_history(data_dir: &Path, project_id: &str, entries: &[Value]) -> Result<(), safe_fs::SafeFsError> {
    if validate_project_id(project_id).is_err() {
        return Err(safe_fs::SafeFsError::Invalid);
    }
    fs::create_dir_all(data_dir).map_err(|_| safe_fs::SafeFsError::Io)?;
    safe_fs::ensure_dirs(data_dir, HISTORY_DIR_REL)?;
    let dir = safe_fs::check_chain(data_dir, HISTORY_DIR_REL)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|_| safe_fs::SafeFsError::Io)?;
    let target = safe_fs::check_chain(data_dir, &history_rel(project_id))?;
    match fs::symlink_metadata(&target) {
        Ok(m) if !m.file_type().is_file() => return Err(safe_fs::SafeFsError::NotRegular),
        _ => {}
    }
    let bytes = serde_json::to_vec(&json!({"version": 1, "entries": entries})).map_err(|_| safe_fs::SafeFsError::Io)?;
    let tmp = dir.join(format!(".{project_id}.{}.tmp", next_exec_id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut f| f.write_all(&bytes).and_then(|()| f.sync_all()))
        .and_then(|()| fs::rename(&tmp, &target));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(safe_fs::SafeFsError::Io);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Rotas.

fn plain(status: StatusCode, code: &'static str) -> Response {
    ApiError::coded(status, code, Map::new()).into_response()
}

fn reject(status: StatusCode, code: &'static str, reason: &'static str) -> Response {
    ApiError::coded(status, code, Map::new()).with_reason(reason).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecInput {
    confirm: bool,
    spec_id: String,
    command: String,
}

/// Comando aceito: não vazio, de uma linha só (sem caractere de controle além da tabulação) e
/// até [`MAX_COMMAND_CHARS`].
fn valid_command(command: &str) -> bool {
    let trimmed = command.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= MAX_COMMAND_CHARS && !trimmed.chars().any(|c| c.is_control() && c != '\t')
}

/// Origem do comando para o histórico (C-12): o dispositivo autorizado (id e nome) e se veio do
/// loopback do próprio computador.
fn origin_of(state: &AppState, access: Option<&Access>) -> Value {
    let loopback = access.is_some_and(|a| a.loopback);
    let device = access.and_then(|a| a.device.as_deref()).map(|id| {
        let name = state.devices().list().into_iter().find(|d| d.id == id).map(|d| d.name);
        json!({"id": id, "name": name})
    });
    json!({"device": device.unwrap_or(Value::Null), "loopback": loopback})
}

/// `POST /api/projects/:id/app/exec`: roda um comando digitado no container do projeto.
/// Responde 202 com a visão do comando em curso; a saída e o resultado chegam pelos eventos e
/// por `GET /api/projects/:id/app/exec`.
pub async fn exec_app(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    access: Option<Extension<Access>>,
    body: Bytes,
) -> Response {
    if let Err(e) = validate_project_id(&id) {
        return e.into_response();
    }
    let Ok(input) = serde_json::from_slice::<ExecInput>(&body) else {
        return plain(StatusCode::BAD_REQUEST, ERR_BODY);
    };
    if !input.confirm {
        return plain(StatusCode::BAD_REQUEST, ERR_CONFIRM);
    }
    if let Err(e) = validate_spec_id(&input.spec_id) {
        return e.into_response();
    }
    if !valid_command(&input.command) {
        let p = params([("max", json!(MAX_COMMAND_CHARS))]);
        return ApiError::coded(StatusCode::BAD_REQUEST, CODE_EXEC_COMMAND_INVALID, p).into_response();
    }
    let Ok(inner) = crate::sdd::docker_command::normalize(&input.command) else {
        return reject(StatusCode::UNPROCESSABLE_ENTITY, CODE_EXEC_COMMAND_DOCKER, "command_docker");
    };
    // Prontidão `execute` (spec 093 T02), com o Docker conferido na hora, antes de reservar.
    if let Err(resp) = super::readiness::require(&state, super::readiness::Gate::Execute).await {
        return resp;
    }
    // Reserva interna do projeto durante a preparação: exclui run, remoção, relatório e a partida.
    let (project, _guard) = match state.reserve_run(&id, APP_RESERVATION).await {
        Ok(ok) => ok,
        Err(ReserveError::NotFound) => return plain(StatusCode::NOT_FOUND, ERR_NOT_FOUND),
        Err(ReserveError::Busy) => return reject(StatusCode::CONFLICT, ERR_BUSY, "busy"),
        Err(ReserveError::ReportInProgress) => return reject(StatusCode::CONFLICT, REPORT_BUSY_CODE, "report_in_progress"),
    };
    let root = PathBuf::from(&project.path);
    let (spec_root, rel) = (root.clone(), format!(".sdd/specs/{}/spec.md", input.spec_id));
    let read = tokio::task::spawn_blocking(move || {
        safe_fs::read_regular(&spec_root, &rel, MAX_SPEC_BYTES).ok().is_some_and(|text| Spec::parse(&text).is_ok())
    })
    .await;
    match read {
        Ok(true) => {}
        Ok(false) => return plain(StatusCode::NOT_FOUND, ERR_SPEC_NOT_FOUND),
        Err(_) => return plain(StatusCode::INTERNAL_SERVER_ERROR, ERR_INTERNAL),
    }
    if state.apps().console().is_busy(&id) {
        return reject(StatusCode::CONFLICT, CODE_EXEC_BUSY, "exec_busy");
    }
    let Some(compose) = state.compose().for_project(&root, &id, ComposeKind::Run) else {
        state.apps().failures().record(&id, &input.spec_id, super::runs::PROJECT_NOT_DOCKER_CODE, "");
        return reject(StatusCode::CONFLICT, super::runs::PROJECT_NOT_DOCKER_CODE, super::runs::REASON_ENV_NOT_DOCKER);
    };
    let secrets = state.secret_set().await;
    let shown = scrub_text(&secrets, input.command.trim());
    let request = ExecRequest {
        project_id: id.clone(),
        spec_id: input.spec_id,
        redacted: shown != input.command.trim(),
        command: inner,
        shown,
        origin: origin_of(&state, access.as_ref().map(|Extension(a)| a)),
        root,
        compose,
        secrets,
        data_dir: state.data_dir().map(Path::to_path_buf),
    };
    match state.apps().console().launch(request).await {
        Ok(view) => (StatusCode::ACCEPTED, Json(view)).into_response(),
        Err(ExecBusy) => reject(StatusCode::CONFLICT, CODE_EXEC_BUSY, "exec_busy"),
    }
}

/// `GET /api/projects/:id/app/exec`: comando em curso, último resultado, histórico e limites.
pub async fn get_exec(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Response {
    if let Err(e) = validate_project_id(&id) {
        return e.into_response();
    }
    if let Err(e) = find_project(&state, &id).await {
        return e.into_response();
    }
    Json(state.apps().console().view(state.data_dir().map(Path::to_path_buf), &id).await).into_response()
}

/// `POST /api/projects/:id/app/exec/stop`: para o comando em curso; sem comando é um no-op (200).
pub async fn stop_exec(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Response {
    if let Err(e) = validate_project_id(&id) {
        return e.into_response();
    }
    if let Err(e) = find_project(&state, &id).await {
        return e.into_response();
    }
    state.apps().console().stop(&id, super::app::REASON_STOPPED).await;
    Json(state.apps().console().view(state.data_dir().map(Path::to_path_buf), &id).await).into_response()
}

#[cfg(test)]
#[path = "console_tests.rs"]
mod tests;
