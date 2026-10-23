//! Diálogo dos agentes persistido por spec (`.sdd/specs/<id>/dialog.jsonl`, local e fora do Git).
//!
//! Cada mensagem de agente (Developer, Revisor, Testes) e do sistema do run vira uma linha
//! `{"seq","ts","agent","task_id","attempt","text"}`. Texto, `agent` e `task_id` passam pela mesma
//! normalização do evento ao vivo e do GET (`events::message_text` e `events::normalize_id_field`).
//!
//! Desacoplamento (sem I/O no caminho do runner):
//! - o `seq` é atribuído na emissão, em memória, por spec: `max(último + 1, microssegundos Unix)`.
//!   Continua crescendo após reinício sem ler o arquivo (µs desde 1970 cabem em inteiro seguro de
//!   JS até o ano ~2255);
//! - o evento é publicado de imediato, na ordem de emissão, já com o `seq`;
//! - a gravação é um efeito paralelo: uma thread por projeto consome uma fila limitada
//!   (`HARD_LIMIT`) com `try_send`. Fila cheia, falha de I/O ou thread morta descartam só a
//!   gravação: o arquivo fica com uma lacuna de `seq`, e o evento já foi publicado.
//!
//! A gravação nunca cria diretório de spec: só grava em spec existente (`spec.md` regular, sem
//! symlink). `GET /api/projects/:id/specs/:spec_id/dialog?after=&limit=` lê a janela final.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use axum::http::StatusCode;
use serde_json::{json, Map, Value};

use super::approvals::replace_atomic;
use super::error::ApiError;
use super::events::{
    message_params, message_text, normalize_id_field, normalize_id_text, normalize_text, truncate_text, valid_message_code,
    validate_project_id, ApiEvent, EventBus, EventType, SecretSet,
};
use super::projects::find_project;
use super::safe_fs::{self, O_NOFOLLOW, O_NONBLOCK};
use super::spec_result::validate_spec_id;
use super::state::AppState;

/// Tamanho máximo do arquivo; ao passar, mantém-se a metade mais recente.
pub const DIALOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Bytes mantidos do fim após um truncamento.
const DIALOG_KEEP_BYTES: u64 = DIALOG_MAX_BYTES / 2;
/// Linha do `.git/info/exclude`: o diálogo é local (pode conter trechos de código e saídas).
pub const DIALOG_EXCLUDE_LINE: &str = ".sdd/specs/*/dialog.jsonl";
/// Linha do `.git/info/exclude` para os temporários da rotação.
pub const DIALOG_TMP_EXCLUDE_LINE: &str = ".sdd/specs/*/.dialog.jsonl.*.tmp";
/// Linha-marcador no topo do arquivo após uma rotação: o início do histórico foi removido.
/// Não é uma mensagem (não tem `seq`); o leitor a ignora como linha inválida e a usa só para
/// `truncated_before`.
pub const ROTATED_MARKER: &str = r#"{"truncated":true}"#;
/// Capacidade da fila de gravação de cada projeto. Excedente: a gravação é descartada.
pub const HARD_LIMIT: usize = 4096;
/// Prazo do `flush` no encerramento: nunca trava o shutdown.
pub const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// Temporários da rotação mais velhos que isto são órfãos e são removidos.
const TMP_MAX_AGE: Duration = Duration::from_secs(60);
const SPECS_REL: &str = ".sdd/specs";
const DIALOG_FILE: &str = "dialog.jsonl";
const TMP_PREFIX: &str = ".dialog.jsonl.";
const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 500;
/// Espera de `flush_project` na escrita da fila de cada projeto (só testes e encerramento).
const SETTLE_STEP: Duration = Duration::from_millis(5);

/// Uma linha de `dialog.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogLine {
    pub seq: u64,
    pub ts: u64,
    pub agent: String,
    pub task_id: Option<String>,
    pub attempt: Option<u32>,
    pub text: String,
    /// `code` estável da mensagem do sistema (spec 102); ausente nas demais.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Parâmetros do `code` (textos e inteiros).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Map<String, Value>>,
}

/// Destino de gravação: projeto (raiz) e spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogTarget {
    root: PathBuf,
    spec_id: String,
}

impl DialogTarget {
    pub fn new(root: impl Into<PathBuf>, spec_id: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            spec_id: spec_id.into(),
        }
    }
}

/// Itens da fila de gravação de um projeto.
enum Job {
    /// Acrescenta `line` ao diálogo de `spec_id`.
    Write { spec_id: String, line: DialogLine },
    /// Confirma que os itens anteriores foram processados.
    Flush(SyncSender<()>),
}

/// Estado compartilhado: último seq por (projeto, spec) e fila de gravação por projeto.
#[derive(Default)]
struct Registry {
    seqs: HashMap<(PathBuf, String), u64>,
    writers: HashMap<PathBuf, (SyncSender<Job>, JoinHandle<()>)>,
    /// Projetos removidos (lápides): entregas tardias deles não recriam fila, thread nem seq.
    /// Teto de `MAX_REMOVED`; ao exceder, a mais antiga é descartada.
    removed: VecDeque<PathBuf>,
}

/// Teto de projetos removidos lembrados como lápides.
pub const MAX_REMOVED: usize = 1024;

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

/// Trava curta do registro (sem I/O sob a trava). Veneno é ignorado: o dado continua válido.
fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY
        .get_or_init(|| Mutex::new(Registry::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Publica `event`. Mensagens de agente com `target` recebem `seq` agora e são gravadas em
/// segundo plano. Os demais eventos saem direto. Nunca faz I/O nem espera a gravação.
pub fn deliver(bus: &EventBus, target: Option<&DialogTarget>, mut event: ApiEvent) {
    if event.event_type() != EventType::AgentMessage.as_str() {
        bus.publish(event);
        return;
    }
    let Some(target) = target else {
        // Sem destino: a chave `spec_id` existe e é nula, para o cliente nunca deduzir a spec.
        if let Some(map) = event.payload.as_object_mut() {
            map.insert("spec_id".into(), Value::Null);
        }
        bus.publish(event);
        return;
    };
    // A trava cobre atribuição, publicação e enfileiramento: seq, ordem do evento e ordem da
    // gravação coincidem por spec.
    let mut reg = registry();
    let seq = assign_seq(&mut reg, target);
    let line = seq.map(|seq| build_line(seq, &event.payload));
    if let Some(map) = event.payload.as_object_mut() {
        // Spec do destino: a mesma da linha gravada (`.sdd/specs/<spec_id>/dialog.jsonl`).
        map.insert("spec_id".into(), Value::from(target.spec_id.clone()));
        map.insert("seq".into(), seq.map_or(Value::Null, Value::from));
    }
    bus.publish(event);
    if let Some(line) = line {
        enqueue(&mut reg, &target.root, &target.spec_id, line);
    }
}

/// Próximo seq da spec: `max(último + 1, µs Unix)`. `None` só se o contador esgotar `u64`.
fn assign_seq(reg: &mut Registry, target: &DialogTarget) -> Option<u64> {
    validate_spec_id(&target.spec_id).ok()?;
    if is_removed(reg, &target.root) {
        return None;
    }
    let key = (target.root.clone(), target.spec_id.clone());
    let last = reg.seqs.get(&key).copied().unwrap_or(0);
    let seq = last.checked_add(1)?.max(now_micros());
    reg.seqs.insert(key, seq);
    Some(seq)
}

/// Linha gravada a partir do payload já normalizado (sem I/O).
fn build_line(seq: u64, payload: &Value) -> DialogLine {
    DialogLine {
        seq,
        ts: unix_now(),
        agent: normalize_id_text(payload["agent"].as_str().unwrap_or_default()),
        task_id: payload["task_id"].as_str().map(normalize_id_text),
        attempt: payload["attempt"].as_u64().and_then(|n| u32::try_from(n).ok()),
        text: truncate_text(normalize_text(payload["text"].as_str().unwrap_or_default())),
        code: payload["code"].as_str().filter(|c| valid_message_code(c)).map(str::to_string),
        params: payload["params"]
            .as_object()
            .filter(|_| payload["code"].as_str().is_some_and(valid_message_code))
            .map(|p| message_params(&SecretSet::default(), p)),
    }
}

/// Enfileira a gravação no projeto (fila limitada, `try_send`). Excedente ou falha: descarta.
fn enqueue(reg: &mut Registry, root: &Path, spec_id: &str, line: DialogLine) {
    let Some(tx) = writer_for(reg, root) else {
        return;
    };
    let job = Job::Write {
        spec_id: spec_id.to_string(),
        line,
    };
    if let Err(TrySendError::Disconnected(_)) = tx.try_send(job) {
        // Thread morta: a próxima emissão cria outra.
        reg.writers.remove(root);
    }
}

/// Fila do projeto; cria a thread de gravação na primeira vez. `None` se a thread não subir.
fn writer_for(reg: &mut Registry, root: &Path) -> Option<SyncSender<Job>> {
    if let Some((tx, _)) = reg.writers.get(root) {
        return Some(tx.clone());
    }
    if is_removed(reg, root) {
        return None;
    }
    let (tx, rx) = sync_channel(HARD_LIMIT);
    let owned = root.to_path_buf();
    let handle = std::thread::Builder::new()
        .name("nants-dialog".into())
        .spawn(move || run_writer(owned, rx))
        .ok()?;
    reg.writers.insert(root.to_path_buf(), (tx.clone(), handle));
    Some(tx)
}

/// Libera a fila, a thread e os seqs de um projeto removido do Nants. A thread encerra depois de
/// drenar a fila (quando o último emissor cai). Devolve o handle para quem quiser aguardá-la.
pub fn forget_project(root: &Path) -> Option<JoinHandle<()>> {
    let mut reg = registry();
    reg.seqs.retain(|(owner, _), _| owner != root);
    if !is_removed(&reg, root) {
        reg.removed.push_back(root.to_path_buf());
        if reg.removed.len() > MAX_REMOVED {
            reg.removed.pop_front();
        }
    }
    reg.writers.remove(root).map(|(_, handle)| handle)
}

/// Projeto (re)admitido: limpa a lápide para voltar a gravar. Chamado ao criar/reimportar o
/// projeto e antes de um run dele.
pub fn revive_project(root: &Path) {
    registry().removed.retain(|owner| owner != root);
}

fn is_removed(reg: &Registry, root: &Path) -> bool {
    reg.removed.iter().any(|owner| owner == root)
}

/// Há fila ou seq registrado para o projeto (testes).
#[cfg(test)]
pub fn registry_has(root: &Path) -> bool {
    let reg = registry();
    reg.writers.contains_key(root) || reg.seqs.keys().any(|(owner, _)| owner == root)
}

/// Quantidade de lápides de projetos removidos (testes).
#[cfg(test)]
pub fn removed_count() -> usize {
    registry().removed.len()
}

/// Semeia o seq da spec com o maior seq já gravado no arquivo (após reinício). Feito antes do
/// primeiro evento do run, fora do emissor. Falha de leitura: segue só com o relógio.
pub fn seed_seq(root: &Path, spec_id: &str) {
    if validate_spec_id(spec_id).is_err() {
        return;
    }
    let Some(window) = read_window(root, &dialog_rel(spec_id)) else {
        return;
    };
    let Some(max) = parse_window(&window).iter().map(|l| l.seq).max() else {
        return;
    };
    let mut reg = registry();
    if is_removed(&reg, root) {
        return;
    }
    let entry = reg
        .seqs
        .entry((root.to_path_buf(), spec_id.to_string()))
        .or_insert(0);
    *entry = (*entry).max(max);
}

/// Espera, no máximo `timeout`, até as filas de gravação processarem o que já foi enfileirado.
/// Nunca trava: thread ausente ou sem confirmação no prazo retorna sem erro.
pub fn flush(timeout: Duration) {
    let senders: Vec<SyncSender<Job>> = registry()
        .writers
        .values()
        .map(|(tx, _)| tx.clone())
        .collect();
    flush_senders(&senders, timeout);
}

/// Como `flush`, só para a fila de um projeto (testes).
#[cfg(test)]
pub fn flush_project(root: &Path, timeout: Duration) {
    let sender = registry().writers.get(root).map(|(tx, _)| tx.clone());
    if let Some(tx) = sender {
        flush_senders(std::slice::from_ref(&tx), timeout);
    }
}

fn flush_senders(senders: &[SyncSender<Job>], timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut acks = Vec::new();
    for tx in senders {
        let (ack_tx, ack_rx) = sync_channel(1);
        if send_until(tx, Job::Flush(ack_tx), deadline) {
            acks.push(ack_rx);
        }
    }
    for ack in acks {
        let left = deadline.saturating_duration_since(Instant::now());
        let _ = ack.recv_timeout(left);
    }
}

/// `true` quando a confirmação chegou no prazo (testes).
#[cfg(test)]
fn flush_on(tx: &SyncSender<Job>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let (ack_tx, ack_rx) = sync_channel(1);
    if !send_until(tx, Job::Flush(ack_tx), deadline) {
        return false;
    }
    ack_rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .is_ok()
}

/// Envia com `try_send` até o prazo (fila cheia espera em passos curtos). `false` se não coube.
fn send_until(tx: &SyncSender<Job>, mut job: Job, deadline: Instant) -> bool {
    loop {
        match tx.try_send(job) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(back)) => {
                if Instant::now() >= deadline {
                    return false;
                }
                job = back;
                std::thread::sleep(SETTLE_STEP);
            }
        }
    }
}

/// Laço da thread de gravação de um projeto: grava na ordem de chegada.
fn run_writer(root: PathBuf, rx: Receiver<Job>) {
    let mut tails: HashMap<PathBuf, Tail> = HashMap::new();
    while let Ok(job) = rx.recv() {
        match job {
            Job::Flush(ack) => {
                let _ = ack.send(());
            }
            Job::Write { spec_id, line } => {
                stall_point(&root);
                // Falha (inclusive panic inesperado) só descarta esta gravação.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    write_line(&mut tails, &root, &spec_id, &line)
                }));
            }
        }
    }
}

/// Estado conhecido do arquivo: tamanho esperado e se termina em `\n`.
#[derive(Debug, Clone, Copy)]
struct Tail {
    len: u64,
    clean_end: bool,
}

/// Acrescenta a linha ao diálogo da spec, se a spec existir. `None` quando não gravou.
fn write_line(
    tails: &mut HashMap<PathBuf, Tail>,
    root: &Path,
    spec: &str,
    line: &DialogLine,
) -> Option<()> {
    validate_spec_id(spec).ok()?;
    // Nunca cria diretório de spec: spec removida ou inexistente não é recriada pela gravação.
    if !spec_exists(root, spec) {
        return None;
    }
    let rel = dialog_rel(spec);
    let path = safe_fs::check_chain(root, &rel).ok()?;
    let len = current_len(&path)?;
    let tail = match tails.get(&path) {
        Some(tail) if tail.len == len => *tail,
        _ => derive_tail(root, &rel, len),
    };
    let mut out = String::new();
    if !tail.clean_end {
        out.push('\n');
    }
    out.push_str(&serde_json::to_string(line).ok()?);
    out.push('\n');
    let Some(new_len) = append(&path, out.as_bytes()) else {
        // Escrita possivelmente parcial: o próximo acesso relê o fim do arquivo.
        tails.remove(&path);
        return None;
    };
    let mut next = Tail {
        len: new_len,
        clean_end: true,
    };
    if new_len > DIALOG_MAX_BYTES {
        match rotate(root, &spec_dir_rel(spec), &rel) {
            Some(kept) => next.len = kept,
            None => {
                tails.remove(&path);
                return Some(());
            }
        }
    }
    tails.insert(path, next);
    Some(())
}

/// Relê a janela final para saber se o arquivo termina em quebra de linha.
fn derive_tail(root: &Path, rel: &str, len: u64) -> Tail {
    let clean_end = read_window(root, rel).is_none_or(|w| w.bytes.last().is_none_or(|b| *b == b'\n'));
    Tail { len, clean_end }
}

/// Tamanho do arquivo: 0 se ausente; `None` se não for arquivo regular.
fn current_len(path: &Path) -> Option<u64> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Some(meta.len()),
        Ok(_) => None,
        Err(e) if e.kind() == ErrorKind::NotFound => Some(0),
        Err(_) => None,
    }
}

/// Acrescenta `bytes` com `O_NOFOLLOW | O_NONBLOCK`. Devolve o novo tamanho.
fn append(path: &Path, bytes: &[u8]) -> Option<u64> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    file.write_all(bytes).ok()?;
    Some(meta.len() + bytes.len() as u64)
}

/// Mantém a metade mais recente, de forma atômica. O corte vai à próxima quebra de linha; sem
/// quebra na metade final, corta no ponto exato (a linha parcial é ignorada pelo leitor), para o
/// arquivo nunca crescer sem limite. Remove temporários órfãos antigos. Devolve o novo tamanho.
fn rotate(root: &Path, spec_dir: &str, rel: &str) -> Option<u64> {
    let window = read_window(root, rel)?;
    let bytes = window.bytes.as_slice();
    let start = bytes.len().saturating_sub(DIALOG_KEEP_BYTES as usize);
    let cut = match bytes[start..].iter().position(|b| *b == b'\n') {
        Some(i) => start + i + 1,
        None => start,
    };
    // Marcador no topo; marcadores antigos da janela não são repetidos.
    let marker_line = format!("{ROTATED_MARKER}\n");
    let mut out = marker_line.clone().into_bytes();
    for line in bytes[cut..].split_inclusive(|b| *b == b'\n') {
        if line != marker_line.as_bytes() {
            out.extend_from_slice(line);
        }
    }
    replace_atomic(root, rel, &out).ok()?;
    remove_stale_tmp(root, spec_dir);
    Some(out.len() as u64)
}

/// Remove temporários `.dialog.jsonl.*.tmp` da spec com mais de `TMP_MAX_AGE` (ignora links).
fn remove_stale_tmp(root: &Path, spec_dir: &str) {
    let Ok(dir) = safe_fs::check_chain(root, spec_dir) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !(name.starts_with(TMP_PREFIX) && name.ends_with(".tmp")) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > TMP_MAX_AGE);
        if meta.is_file() && old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Janela final do arquivo (até `DIALOG_MAX_BYTES`). `None` se ausente, symlink ou não regular.
struct Window {
    bytes: Vec<u8>,
    /// `true` quando a primeira linha da janela está cortada e deve ser descartada.
    partial_first: bool,
    /// `true` quando a janela não começa no byte 0 do arquivo (o início do histórico ficou de fora).
    cut: bool,
    /// `true` quando o arquivo traz o marcador de rotação (o início do histórico foi removido).
    rotated: bool,
}

/// Lê a janela final. Se o início da janela cai exatamente numa fronteira de linha (o byte
/// anterior é `\n`), a primeira linha é completa e não é descartada.
fn read_window(root: &Path, rel: &str) -> Option<Window> {
    let path = safe_fs::check_chain(root, rel).ok()?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(&path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let len = meta.len();
    let start = len.saturating_sub(DIALOG_MAX_BYTES);
    if start == 0 {
        let mut bytes = Vec::new();
        (&mut file).take(len).read_to_end(&mut bytes).ok()?;
        let rotated = contains_marker(&bytes);
        return Some(Window {
            bytes,
            partial_first: false,
            cut: false,
            rotated,
        });
    }
    // Lê também o byte anterior ao início para saber se a janela começa numa fronteira.
    let probe = start - 1;
    file.seek(SeekFrom::Start(probe)).ok()?;
    let mut raw = Vec::new();
    (&mut file).take(len - probe).read_to_end(&mut raw).ok()?;
    let boundary = raw.first() == Some(&b'\n');
    Some(Window {
        bytes: raw.get(1..).unwrap_or_default().to_vec(),
        partial_first: !boundary,
        cut: start > 0,
        rotated: contains_marker(&raw),
    })
}

/// `true` se alguma linha da janela é exatamente o marcador de rotação.
fn contains_marker(bytes: &[u8]) -> bool {
    bytes
        .split(|b| *b == b'\n')
        .any(|line| line == ROTATED_MARKER.as_bytes())
}

/// Linhas válidas da janela. Linhas inválidas são ignoradas; a primeira pode estar cortada.
fn parse_window(window: &Window) -> Vec<DialogLine> {
    let text = String::from_utf8_lossy(&window.bytes);
    let mut lines = text.lines();
    if window.partial_first {
        lines.next();
    }
    lines
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn spec_dir_rel(spec_id: &str) -> String {
    format!("{SPECS_REL}/{spec_id}")
}

/// Caminho relativo ao projeto do diálogo de uma spec.
pub fn dialog_rel(spec_id: &str) -> String {
    format!("{SPECS_REL}/{spec_id}/{DIALOG_FILE}")
}

/// A spec existe quando `spec.md` é arquivo regular sem symlink no caminho.
fn spec_exists(root: &Path, spec_id: &str) -> bool {
    let rel = format!("{}/spec.md", spec_dir_rel(spec_id));
    safe_fs::check_chain(root, &rel)
        .ok()
        .and_then(|path| fs::symlink_metadata(path).ok())
        .is_some_and(|meta| meta.is_file())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Microssegundos Unix, como base do seq (cabem em inteiro seguro de JS até ~2255).
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}

/// Gancho de teste: enquanto o projeto estiver na lista, a gravação dele espera.
#[cfg(test)]
static STALLED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

#[cfg(test)]
fn stalled(root: &Path) -> bool {
    STALLED
        .lock()
        .map(|list| list.iter().any(|p| p == root))
        .unwrap_or(false)
}

#[cfg(not(test))]
fn stalled(_root: &Path) -> bool {
    false
}

fn stall_point(root: &Path) {
    while stalled(root) {
        std::thread::sleep(SETTLE_STEP);
    }
}

/// Página de leitura: mensagens com `seq > after` em ordem crescente, até `limit`.
#[derive(Debug, PartialEq, Eq)]
pub struct Page {
    pub messages: Vec<DialogLine>,
    /// Maior seq presente no arquivo (0 se vazio).
    pub last_seq: u64,
    /// `true` quando o início do histórico foi removido (rotação) e `after` é anterior à primeira
    /// mensagem disponível. Não se baseia em números de seq (são em µs).
    pub truncated_before: bool,
}

/// Lê uma página do diálogo. Arquivo ausente, inseguro ou ilegível vira página vazia.
pub fn read_page(root: &Path, spec_id: &str, after: u64, limit: usize) -> Page {
    let empty = Page {
        messages: Vec::new(),
        last_seq: 0,
        truncated_before: false,
    };
    let Some(window) = read_window(root, &dialog_rel(spec_id)) else {
        return empty;
    };
    let mut lines = parse_window(&window);
    lines.sort_by_key(|l| l.seq);
    let (Some(first), Some(last)) = (lines.first().map(|l| l.seq), lines.last().map(|l| l.seq))
    else {
        return empty;
    };
    // Truncado = a janela não começa no início do histórico (rotação ou leitura a partir do meio)
    // e o cursor é anterior à primeira mensagem disponível. Sem depender de números de seq.
    let history_cut = window.rotated || window.cut;
    Page {
        messages: lines
            .into_iter()
            .filter(|l| l.seq > after)
            .take(limit)
            .collect(),
        last_seq: last,
        truncated_before: history_cut && after < first,
    }
}

/// Query de `GET .../dialog`. Valores brutos: a validação devolve 422 estático.
#[derive(Debug, Deserialize)]
pub struct DialogQuery {
    after: Option<String>,
    limit: Option<String>,
}

/// `GET /api/projects/:id/specs/:spec_id/dialog?after=<seq>&limit=<n>`: somente leitura.
pub async fn get_dialog(
    State(state): State<AppState>,
    AxPath((id, spec_id)): AxPath<(String, String)>,
    Query(query): Query<DialogQuery>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    validate_spec_id(&spec_id)?;
    let after = parse_after(query.after.as_deref())?;
    let limit = parse_limit(query.limit.as_deref())?;
    let project = find_project(&state, &id).await?;
    let secrets = state.secret_set().await;
    let root = PathBuf::from(&project.path);
    let page = tokio::task::spawn_blocking(move || -> Result<Page, ApiError> {
        if !spec_exists(&root, &spec_id) {
            return Err(ApiError::coded(axum::http::StatusCode::NOT_FOUND, "spec_not_found", Map::new()));
        }
        Ok(read_page(&root, &spec_id, after, limit))
    })
    .await
    .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))??;
    Ok(Json(page_view(&secrets, page)).into_response())
}

fn parse_after(raw: Option<&str>) -> Result<u64, ApiError> {
    match raw {
        None => Ok(0),
        Some(text) => text
            .parse::<u64>()
            .map_err(|_| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "after_invalid", Map::new())),
    }
}

fn parse_limit(raw: Option<&str>) -> Result<usize, ApiError> {
    match raw {
        None => Ok(DEFAULT_LIMIT),
        Some(text) => text
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or_else(|| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "limit_invalid", Map::new())),
    }
}

/// Saída do GET: mesma normalização do evento ao vivo (redação, normalização e limites).
fn page_view(secrets: &SecretSet, page: Page) -> Value {
    let messages: Vec<Value> = page
        .messages
        .into_iter()
        .map(|m| {
            let mut item = json!({
                "seq": m.seq,
                "ts": m.ts,
                "agent": normalize_id_field(secrets, &m.agent),
                "task_id": m.task_id.map(|t| normalize_id_field(secrets, &t)),
                "attempt": m.attempt,
                "text": message_text(secrets, &m.text),
            });
            if let (Some(code), Some(map)) = (m.code.as_deref().filter(|c| valid_message_code(c)), item.as_object_mut()) {
                map.insert("code".into(), Value::from(code));
                let params = m.params.as_ref().map(|p| message_params(secrets, p)).unwrap_or_default();
                map.insert("params".into(), Value::Object(params));
            }
            item
        })
        .collect();
    json!({
        "messages": messages,
        "last_seq": page.last_seq,
        "truncated_before": page.truncated_before,
    })
}

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod tests;
