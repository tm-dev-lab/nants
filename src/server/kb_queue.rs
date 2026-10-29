//! Fila de extração da base de conhecimento (spec 134/T02).
//!
//! - **Fila única** por processo, em segundo plano: um documento por vez, na ordem de chegada. O
//!   trabalho pesado (ler o PDF, converter o HTML) roda em `spawn_blocking`.
//! - **Tempo máximo por arquivo** ([`EXTRACT_TIMEOUT`]), como proteção contra um arquivo
//!   patológico: passou do tempo, o documento fica `error` com o motivo `kb_extract_timeout` na
//!   hora. A thread bloqueante não se interrompe; a fila espera ela terminar antes do próximo
//!   documento (medido na T03: sem a espera, duas extrações rodavam juntas), e o resultado dela é
//!   descartado (só a fila grava).
//! - **Eventos:** cada mudança de estado publica `kb_updated` com a entrada redigida
//!   (`pending`, `extracting`, `ready`, `no_text`, `error`).
//! - **Retomada:** um documento que ficou `pending` ou `extracting` (servidor reiniciado no meio)
//!   volta para a fila na próxima listagem da base ([`KbQueue::enqueue`] ignora repetidos).

use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::mpsc;

use super::events::{ApiEvent, EventBus, EventType, SecretSet};
use super::projects::now_rfc3339;
use crate::harness::web::{WebClient, WebError};
use crate::kb::{self, extract, Entry, Kind, Outcome};

/// Tempo máximo da extração de um documento (leitura do link incluída).
pub const EXTRACT_TIMEOUT: Duration = Duration::from_secs(120);

/// Um documento a extrair.
struct Job {
    project_id: String,
    root: PathBuf,
    doc: String,
    secrets: Arc<SecretSet>,
}

struct Inner {
    events: EventBus,
    web: WebClient,
    timeout: Duration,
    tx: Mutex<Option<mpsc::UnboundedSender<Job>>>,
    /// (raiz, documento) na fila ou em extração: a mesma entrada não entra duas vezes.
    queued: Mutex<HashSet<(PathBuf, String)>>,
    /// Extrações rodando agora e o maior número visto ao mesmo tempo (a medida do "um por vez").
    workers: Arc<Workers>,
}

/// Contagem das extrações em andamento, inclusive as que passaram do tempo.
#[derive(Default)]
struct Workers {
    active: AtomicUsize,
    peak: AtomicUsize,
}

/// Marca uma extração em andamento enquanto vive (também em pânico).
struct Working(Arc<Workers>);

impl Working {
    fn start(workers: &Arc<Workers>) -> Self {
        let now = workers.active.fetch_add(1, Ordering::SeqCst) + 1;
        workers.peak.fetch_max(now, Ordering::SeqCst);
        Self(workers.clone())
    }
}

impl Drop for Working {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A fila. Clones compartilham a mesma fila.
#[derive(Clone)]
pub struct KbQueue {
    inner: Option<Arc<Inner>>,
}

impl fmt::Debug for KbQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KbQueue").field("active", &self.inner.is_some()).finish()
    }
}

impl KbQueue {
    /// Fila ativa: o trabalhador sobe no primeiro documento.
    pub fn new(events: EventBus, web: WebClient, timeout: Duration) -> Self {
        Self {
            inner: Some(Arc::new(Inner {
                events,
                web,
                timeout,
                tx: Mutex::new(None),
                queued: Mutex::new(HashSet::new()),
                workers: Arc::new(Workers::default()),
            })),
        }
    }

    /// Fila parada: nada é extraído (testes que não usam a rede nem a fila).
    pub fn idle() -> Self {
        Self { inner: None }
    }

    /// Maior número de extrações rodando ao mesmo tempo desde o início (testes).
    #[cfg(test)]
    fn peak_workers(&self) -> usize {
        self.inner.as_ref().map_or(0, |i| i.workers.peak.load(Ordering::SeqCst))
    }

    /// Põe o documento na fila. `false` se a fila está parada ou o documento já está nela.
    pub fn enqueue(&self, project_id: &str, root: PathBuf, doc: &str, secrets: Arc<SecretSet>) -> bool {
        let Some(inner) = &self.inner else {
            return false;
        };
        {
            let mut queued = inner.queued.lock().unwrap_or_else(PoisonError::into_inner);
            if !queued.insert((root.clone(), doc.to_string())) {
                return false;
            }
        }
        let job = Job { project_id: project_id.to_string(), root, doc: doc.to_string(), secrets };
        let mut tx = inner.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let sender = tx.get_or_insert_with(|| {
            let (sender, rx) = mpsc::unbounded_channel();
            tokio::spawn(run(inner.clone(), rx));
            sender
        });
        if let Err(mpsc::error::SendError(job)) = sender.send(job) {
            // Trabalhador encerrado (runtime anterior): sobe outro.
            let (sender, rx) = mpsc::unbounded_channel();
            tokio::spawn(run(inner.clone(), rx));
            let sent = sender.send(job).is_ok();
            *tx = Some(sender);
            return sent;
        }
        true
    }
}

/// Publica `kb_updated` com a entrada redigida.
pub fn publish(events: &EventBus, project_id: &str, entry: &Entry, secrets: &SecretSet) {
    let payload = json!({
        "id": entry.id,
        "status": entry.status.as_str(),
        "status_reason": entry.status_reason,
        "doc": super::kb::view(entry, secrets),
    });
    events.publish(ApiEvent::new(EventType::KbUpdated, Some(project_id.to_string()), payload));
}

/// Publica `kb_updated` da remoção de `id`.
pub fn publish_removed(events: &EventBus, project_id: &str, id: &str) {
    let payload = json!({ "id": id, "removed": true });
    events.publish(ApiEvent::new(EventType::KbUpdated, Some(project_id.to_string()), payload));
}

async fn run(inner: Arc<Inner>, mut rx: mpsc::UnboundedReceiver<Job>) {
    while let Some(job) = rx.recv().await {
        process(&inner, &job).await;
        let mut queued = inner.queued.lock().unwrap_or_else(PoisonError::into_inner);
        queued.remove(&(job.root.clone(), job.doc.clone()));
    }
}

/// Trabalho que passou do tempo e ainda roda: a fila espera por ele antes do próximo documento.
pub type Leftover<T> = tokio::task::JoinHandle<Result<T, &'static str>>;

/// Roda `work` em `spawn_blocking` até `deadline`. Pânico → `kb_extract_failed`; passou do tempo
/// → `kb_extract_timeout`, com o trabalho ainda em andamento devolvido (uma thread bloqueante não
/// se interrompe): quem chama marca o documento e espera ele terminar, e o resultado é descartado.
pub async fn run_limited<T: Send + 'static>(
    deadline: Instant,
    work: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
) -> (Result<T, &'static str>, Option<Leftover<T>>) {
    let left = deadline.saturating_duration_since(Instant::now());
    let mut handle = tokio::task::spawn_blocking(work);
    match tokio::time::timeout(left, &mut handle).await {
        Ok(Ok(result)) => (result, None),
        Ok(Err(_)) => (Err(extract::reason::FAILED), None),
        Err(_) => (Err(extract::reason::TIMEOUT), Some(handle)),
    }
}

/// Motivo codificado de uma falha ao ler o link.
fn web_reason(e: &WebError) -> &'static str {
    match e {
        WebError::Blocked(_) | WebError::Scheme | WebError::Credentials => extract::reason::LINK_BLOCKED,
        WebError::Status(_) => extract::reason::LINK_HTTP,
        WebError::Binary(_) => extract::reason::LINK_TYPE,
        _ => extract::reason::LINK_UNREACHABLE,
    }
}

/// Resultado de uma extração, com o trabalho que passou do tempo e ainda roda.
type Extraction = (Outcome, Option<Leftover<(extract::Extracted, Option<extract::LinkSource>)>>);

/// Extrai a entrada já em `extracting`.
async fn extract_entry(inner: &Inner, job: &Job, entry: &Entry) -> Extraction {
    let deadline = Instant::now() + inner.timeout;
    match entry.kind {
        Kind::Link => {
            let Some(url) = entry.source_url.clone() else {
                return (Err(extract::reason::FAILED), None);
            };
            let left = deadline.saturating_duration_since(Instant::now());
            // A leitura do link é assíncrona: passou do tempo, ela é cancelada aqui mesmo.
            let page = match tokio::time::timeout(left, inner.web.fetch_page(&url)).await {
                Ok(Ok(page)) => page,
                Ok(Err(e)) => return (Err(web_reason(&e)), None),
                Err(_) => return (Err(extract::reason::TIMEOUT), None),
            };
            let workers = inner.workers.clone();
            run_limited(deadline, move || {
                let _working = Working::start(&workers);
                extract::page(&page).map(|(ex, source)| (ex, Some(source)))
            })
            .await
        }
        Kind::Pdf | Kind::Markdown | Kind::Text => {
            let root = job.root.clone();
            let entry = entry.clone();
            let workers = inner.workers.clone();
            run_limited(deadline, move || {
                let _working = Working::start(&workers);
                let bytes = kb::source_bytes(&root, &entry).map_err(|_| extract::reason::FAILED)?;
                match entry.kind {
                    Kind::Pdf => extract::pdf(&bytes).map(|ex| (ex, None)),
                    kind => {
                        let text = std::str::from_utf8(&bytes).map_err(|_| extract::reason::FAILED)?;
                        Ok((extract::text(text, kind == Kind::Markdown), None))
                    }
                }
            })
            .await
        }
    }
}

async fn process(inner: &Inner, job: &Job) {
    let root = job.root.clone();
    let doc = job.doc.clone();
    let begun = tokio::task::spawn_blocking(move || kb::begin_extraction(&root, &doc)).await;
    let Ok(Ok(Some(entry))) = begun else {
        return;
    };
    publish(&inner.events, &job.project_id, &entry, &job.secrets);
    let (outcome, leftover) = extract_entry(inner, job, &entry).await;
    let root = job.root.clone();
    let doc = job.doc.clone();
    let added_at = entry.added_at.clone();
    let secrets = job.secrets.clone();
    let finished = tokio::task::spawn_blocking(move || {
        kb::finish_extraction(&root, &doc, &added_at, outcome, &secrets, &now_rfc3339())
    })
    .await;
    if let Ok(Ok(Some(updated))) = finished {
        publish(&inner.events, &job.project_id, &updated, &job.secrets);
    }
    // O documento já está `error` (tempo esgotado); o próximo só começa quando a thread
    // abandonada terminar de verdade, para vários arquivos patológicos não rodarem juntos.
    if let Some(handle) = leftover {
        let limit = inner.timeout.as_secs_f64();
        eprintln!(
            "nants: kb: extração de {} no projeto {} passou de {limit:.1}s; a fila espera a thread terminar",
            job.doc, job.project_id
        );
        let waited = Instant::now();
        let _ = handle.await;
        eprintln!(
            "nants: kb: thread de {} no projeto {} terminou {:.1}s depois do tempo máximo",
            job.doc,
            job.project_id,
            waited.elapsed().as_secs_f64()
        );
    }
}

#[cfg(test)]
#[path = "kb_queue_tests.rs"]
mod tests;
