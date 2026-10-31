//! Nanty (spec 138): o agente de nível de sistema que observa os projetos e a máquina, avisa a
//! pessoa (na janela e pelo Telegram), sugere e age dentro de um catálogo fixo.
//!
//! Esta é a base (T01): configuração (`config::NantyConfig`), estado persistido ([`store`]),
//! registro ([`journal`]), relógio local ([`clock`]), a trava de instância única por pasta de dados
//! ([`instance`]), o laço de fundo e a API de leitura ([`api`]).
//!
//! **Ciclo de vida (C-05):** o laço nasce no `main.rs` por [`start`], no padrão do `startup_sweep`,
//! nunca no `AppState::new` (os testes montam o estado sem laço). Ele só sobe se este processo
//! pegar a trava da pasta de dados; para no `shutdown_signal()`; e trata o `Lagged` do `EventBus`
//! com uma varredura completa.
//!
//! **Observação (T02):** o laço olha os projetos e a máquina numa varredura periódica leve
//! (`[nanty.filter] scan_interval_secs`) e logo depois dos eventos que mudam o estado (com uma
//! espera curta para juntar a rajada). Os observadores ([`observe`]) são determinísticos, sem
//! modelo; o filtro ([`filter`]) decide o que vira aviso. Uma rodada sem novidade não grava nada.
//!
//! **Ações (T03):** depois de cada rodada, o catálogo fixo ([`actions`]) e a execução ([`act`])
//! oferecem a ação no cartão ou a fazem sozinhos, conforme o nível de cada id.
//!
//! **Turno do modelo (T05):** depois de cada rodada, os cartões que acabaram de sair passam por um
//! turno proativo ([`turn::proactive`]) com só sinais estruturados, na fila única dos turnos e
//! dentro do orçamento do dia; o resumo da manhã ([`digest`]) sai na hora marcada. Os dois rodam em
//! tarefas à parte, para a chamada ao modelo não segurar a observação.
//!
//! **Telegram (T06 e T07):** a escuta por long polling ([`telegram`]) e a fila de saída
//! ([`telegram::outbox`]) nascem no [`start`], ao lado do laço, só no dono da trava. Com o Nants fechado o Nanty dorme; na volta, o que o Telegram acumulou vira
//! cartão marcado como antigo, nunca ação imediata (ver [`telegram`]).
//!
//! **Conversa (T08):** a mesma no Telegram e na janela ([`chat`]): comandos fixos sem modelo, texto
//! livre com o modelo e cartão de confirmação para toda ação pedida, e a leitura a pedido do projeto
//! citado ([`read`]), como dado não confiável.
//!
//! **Voz (T09):** a nota de voz do dono é transcrita e segue como texto; a resposta a ela sai em voz,
//! com legenda, pela ordem das conexões ([`voice`]). Os áudios ficam na pasta do Nanty e saem junto
//! com as mensagens antigas da conversa.

pub mod act;
pub mod actions;
pub mod api;
pub mod chat;
pub mod clock;
pub mod digest;
pub mod filter;
pub mod instance;
pub mod journal;
pub mod jsonl;
pub mod observe;
pub mod read;
pub mod render;
pub mod settings;
pub mod signals;
pub mod store;
pub mod telegram;
pub mod turn;
pub mod voice;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use self::clock::{LocalTime, NantyClock};
use self::instance::{InstanceLock, LockOutcome};
use self::journal::{Journal, JournalEntry};
use self::observe::NantyProbe;
use self::signals::{Signal, SignalKind};
use self::store::{ConversationMessage, NantyState, Store, CONVERSATION_TAIL};
use super::events::{ApiEvent, EventBus, EventType};
use super::state::AppState;
use crate::config::{Config, NantyConfig};
use crate::i18n::Lang;

/// Pasta do Nanty dentro da pasta de dados do Nants.
pub const NANTY_DIR: &str = "nanty";
/// Espera máxima pelo fim do laço no encerramento.
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
/// Espera depois de um evento antes de olhar os projetos (junta a rajada de eventos).
const EVENT_DEBOUNCE: Duration = Duration::from_secs(2);

/// Situação da instância do Nanty neste processo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    /// O laço não foi iniciado (testes, ou antes do `start`).
    NotStarted,
    /// Este processo tem a trava e o laço está rodando.
    Active,
    /// Outro processo com a mesma pasta de dados tem o Nanty ligado.
    OtherInstance,
    /// Sem pasta de dados utilizável (ou a trava falhou): o Nanty fica desligado.
    Unavailable,
    /// O laço terminou (encerramento do servidor).
    Stopped,
}

/// Estado do avatar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Avatar {
    Attentive,
    Sleeping,
    Thinking,
    Speaking,
    Waiting,
}

/// O que mudou na janela (o `part` do evento `nanty_changed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Feed,
    Conversation,
    Avatar,
    Status,
}

impl Part {
    fn as_str(self) -> &'static str {
        match self {
            Part::Feed => "feed",
            Part::Conversation => "conversation",
            Part::Avatar => "avatar",
            Part::Status => "status",
        }
    }
}

/// O Nanty deste processo: estado em memória e os arquivos da pasta de dados.
pub struct Nanty {
    /// Arquivos na pasta do Nanty; `None` sem pasta de dados (tudo só em memória, desligado).
    store: Option<Store>,
    journal: Option<Journal>,
    clock: NantyClock,
    events: EventBus,
    state: Mutex<NantyState>,
    status: Mutex<InstanceStatus>,
    lock: Mutex<Option<InstanceLock>>,
    /// Pensando ou falando (turno do modelo e voz, T05/T09); vence o estado derivado.
    busy: Mutex<Option<Avatar>>,
    /// Varreduras completas feitas (a inicial e as do `Lagged`).
    sweeps: AtomicU64,
    /// Sonda do disco e do Docker (T02; os testes trocam).
    probe: Mutex<Arc<dyn NantyProbe>>,
    /// Rodadas dos observadores feitas (T02).
    scans: AtomicU64,
    /// Quem dispara aprovar e retomar (T03; os testes trocam).
    starter: Mutex<Arc<dyn act::RunStarter>>,
    /// A fila única dos turnos do modelo (T05): Telegram e janela.
    turns: turn::TurnQueue,
    /// Modelo simulado (testes); `None` = o cliente da conexão do `[nanty]`.
    backend: Mutex<Option<Arc<dyn crate::agents::chat::ChatBackend>>>,
    /// O canal do Telegram (T06): saúde, bot e pareamento, em memória.
    telegram: telegram::Link,
}

impl std::fmt::Debug for Nanty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nanty").field("status", &self.status()).finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Aviso no terminal pelo dicionário (o idioma do terminal).
fn terminal_notice(key: &str) {
    eprintln!("{}", crate::i18n::text(Lang::from_env(), key));
}

/// Id aleatório curto (16 hex) para mensagens e cartões.
pub fn new_id() -> String {
    let mut buf = [0u8; 8];
    if getrandom::getrandom(&mut buf).is_err() {
        // Sem gerador do sistema: o relógio basta para separar ids de uma pessoa só.
        return format!("{:016x}", clock::now_ms());
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

impl Nanty {
    /// `dir` é a pasta do Nanty (`<pasta de dados>/nanty`); o estado guardado é lido aqui.
    pub fn new(dir: Option<PathBuf>, events: EventBus, clock: NantyClock) -> Self {
        let store = dir.clone().map(Store::new);
        let state = store.as_ref().map(Store::load).unwrap_or_default();
        Self {
            store,
            journal: dir.map(Journal::new),
            clock,
            events,
            state: Mutex::new(state),
            status: Mutex::new(InstanceStatus::NotStarted),
            lock: Mutex::new(None),
            busy: Mutex::new(None),
            sweeps: AtomicU64::new(0),
            probe: Mutex::new(observe::default_probe()),
            scans: AtomicU64::new(0),
            starter: Mutex::new(Arc::new(act::RealStarter)),
            turns: turn::TurnQueue::default(),
            backend: Mutex::new(None),
            telegram: telegram::Link::default(),
        }
    }

    /// O canal do Telegram deste processo.
    pub fn telegram(&self) -> &telegram::Link {
        &self.telegram
    }

    /// A fila única dos turnos do modelo.
    pub fn turns(&self) -> &turn::TurnQueue {
        &self.turns
    }

    /// O modelo simulado, se houver (testes).
    pub fn backend(&self) -> Option<Arc<dyn crate::agents::chat::ChatBackend>> {
        lock(&self.backend).clone()
    }

    /// Troca o modelo por um simulado (testes: nunca chave real).
    pub fn set_backend(&self, backend: Arc<dyn crate::agents::chat::ChatBackend>) {
        *lock(&self.backend) = Some(backend);
    }

    /// Quem dispara aprovar e retomar.
    pub fn starter(&self) -> Arc<dyn act::RunStarter> {
        Arc::clone(&lock(&self.starter))
    }

    /// Troca quem dispara aprovar e retomar (testes).
    pub fn set_starter(&self, starter: Arc<dyn act::RunStarter>) {
        *lock(&self.starter) = starter;
    }

    /// Sonda do disco e do Docker.
    pub fn probe(&self) -> Arc<dyn NantyProbe> {
        Arc::clone(&lock(&self.probe))
    }

    /// Troca a sonda (testes).
    pub fn set_probe(&self, probe: Arc<dyn NantyProbe>) {
        *lock(&self.probe) = probe;
    }

    /// Uma rodada do filtro com o que os observadores viram (`covered`: os tipos observados por
    /// inteiro, ver [`filter::apply`]). Grava o estado e o registro só se algo mudou. `true` se mudou.
    pub fn observe(&self, cfg: &NantyConfig, signals: &[Signal], covered: &[SignalKind]) -> bool {
        let now = self.now();
        let mut journal = Vec::new();
        let changed = self.update(|st| {
            let out = filter::apply(st, signals, covered, cfg, now);
            journal = out.journal;
            // Progresso no projeto (tarefa entregue, spec concluída): a contagem de novas tentativas
            // por parada recomeça (T03).
            let mut reset = false;
            for s in signals.iter().filter(|s| matches!(s.kind, SignalKind::TaskDelivered | SignalKind::SpecDone)) {
                if let Some(p) = &s.project_id {
                    let before = st.retries.len();
                    st.retries.retain(|k, _| !store::group_of_project(k, p));
                    reset |= before != st.retries.len();
                }
            }
            out.changed || reset
        });
        for entry in &journal {
            self.record(entry);
        }
        changed
    }

    /// Rodadas dos observadores feitas.
    pub fn scans(&self) -> u64 {
        self.scans.load(Ordering::Relaxed)
    }

    /// Quanto falta para alguma condição sair da espera do grupo.
    fn next_due(&self, cfg: &NantyConfig) -> Option<Duration> {
        let now = self.now().epoch_ms;
        let st = lock(&self.state);
        // A ação prevista (a nova tentativa com espera, T03) também acorda o laço na hora.
        let planned = st.planned.iter().map(|p| p.at_ms.saturating_sub(now)).min();
        let due = filter::next_due_ms(&st, cfg, now);
        // A hora do resumo da manhã (T05).
        let digest = digest::next_due_ms(cfg, &st, &self.now());
        [due, planned, digest].into_iter().flatten().min().map(Duration::from_millis)
    }

    pub fn clock(&self) -> NantyClock {
        Arc::clone(&self.clock)
    }

    pub fn now(&self) -> LocalTime {
        (self.clock)()
    }

    pub fn status(&self) -> InstanceStatus {
        *lock(&self.status)
    }

    fn set_status(&self, status: InstanceStatus) {
        *lock(&self.status) = status;
        self.changed(Part::Status);
    }

    /// Só o dono da trava grava na pasta do Nanty.
    fn owns(&self) -> bool {
        self.status() == InstanceStatus::Active
    }

    /// Tenta virar o dono do Nanty desta pasta de dados.
    pub fn acquire(&self) -> InstanceStatus {
        let outcome = match &self.store {
            Some(store) => instance::try_lock(store.dir()),
            None => LockOutcome::Failed,
        };
        let status = match outcome {
            LockOutcome::Acquired(held) => {
                *lock(&self.lock) = Some(held);
                InstanceStatus::Active
            }
            LockOutcome::Busy => InstanceStatus::OtherInstance,
            LockOutcome::Failed => InstanceStatus::Unavailable,
        };
        self.set_status(status);
        status
    }

    /// Solta a trava (fim do laço).
    pub fn release(&self) {
        lock(&self.lock).take();
        self.set_status(InstanceStatus::Stopped);
    }

    /// Publica `nanty_changed` (global): a janela relê a parte pela API.
    pub fn changed(&self, part: Part) {
        self.events.publish(ApiEvent::new(EventType::NantyChanged, None, json!({ "part": part.as_str() })));
    }

    /// Cópia do estado atual.
    pub fn snapshot(&self) -> NantyState {
        lock(&self.state).clone()
    }

    /// Muda o estado com `f` e, se mudou, grava (só o dono) e avisa a janela.
    pub fn update(&self, f: impl FnOnce(&mut NantyState) -> bool) -> bool {
        let mut guard = lock(&self.state);
        if !f(&mut guard) {
            return false;
        }
        if self.owns() {
            if let Some(store) = &self.store {
                if store.save(&guard).is_err() {
                    terminal_notice("nanty.log.save_failed");
                }
            }
        }
        drop(guard);
        self.changed(Part::Feed);
        self.changed(Part::Avatar);
        // T07: a fila de saída do Telegram confere o que mudou (cartão novo, decidido, resolvido).
        self.telegram.outbox().wake();
        true
    }

    /// Como [`Self::update`], sem avisar a janela nem a fila de saída: o registro dos envios ao
    /// Telegram (T07) não muda nada que a tela mostre.
    pub fn update_quiet(&self, f: impl FnOnce(&mut NantyState) -> bool) -> bool {
        let mut guard = lock(&self.state);
        if !f(&mut guard) {
            return false;
        }
        if self.owns() {
            if let Some(store) = &self.store {
                if store.save(&guard).is_err() {
                    terminal_notice("nanty.log.save_failed");
                }
            }
        }
        true
    }

    /// Tira tudo o que é do projeto removido: o estado e as linhas do registro e da conversa.
    pub fn forget_project(&self, project_id: &str) {
        self.update(|st| st.forget_project(project_id));
        if !self.owns() {
            return;
        }
        let failed = self.journal.as_ref().is_some_and(|j| j.forget_project(project_id).is_err())
            || self.store.as_ref().is_some_and(|s| {
                s.conversation()
                    .retain::<ConversationMessage>(|m| m.project_id.as_deref() != Some(project_id))
                    .is_err()
            });
        if failed {
            terminal_notice("nanty.log.save_failed");
        }
        voice::prune(self);
        self.changed(Part::Conversation);
    }

    /// Varredura completa (a inicial e a do `Lagged`). Nesta tarefa, tira os itens vencidos e
    /// tudo o que é de projetos que não existem mais (estado, registro e conversa): cobre o
    /// `ProjectRemoved` engolido por um `Lagged` e o projeto removido enquanto este processo não
    /// era o dono. Os observadores entram na T02.
    pub fn full_sweep(&self, known_projects: &[String]) {
        let now = self.now().epoch_ms;
        self.update(|st| {
            let expired = st.prune_expired(now);
            st.retain_projects(known_projects) || expired
        });
        if self.owns() {
            let known = |p: &Option<String>| p.as_ref().is_none_or(|id| known_projects.contains(id));
            let failed = self.journal.as_ref().is_some_and(|j| j.retain_known(&known).is_err())
                || self.store.as_ref().is_some_and(|s| {
                    s.conversation().retain::<ConversationMessage>(|m| known(&m.project_id)).is_err()
                });
            if failed {
                terminal_notice("nanty.log.save_failed");
            }
            // T09: os áudios cuja mensagem saiu da conversa.
            voice::prune(self);
        }
        self.sweeps.fetch_add(1, Ordering::Relaxed);
    }

    pub fn sweeps(&self) -> u64 {
        self.sweeps.load(Ordering::Relaxed)
    }

    /// Acrescenta uma linha ao registro (só o dono grava).
    pub fn record(&self, entry: &JournalEntry) {
        if !self.owns() {
            return;
        }
        if self.journal.as_ref().is_some_and(|j| j.append(entry).is_err()) {
            terminal_notice("nanty.log.save_failed");
        }
    }

    /// As últimas linhas do registro.
    pub fn journal_tail(&self, limit: usize) -> Vec<JournalEntry> {
        self.journal.as_ref().map(|j| j.tail(limit)).unwrap_or_default()
    }

    /// Acrescenta uma mensagem à conversa única (só o dono grava).
    pub fn push_message(&self, message: &ConversationMessage) {
        if !self.owns() {
            return;
        }
        if self.store.as_ref().is_some_and(|s| s.conversation().append(message).is_err()) {
            terminal_notice("nanty.log.save_failed");
        }
        self.changed(Part::Conversation);
        // T07: a fala do Nanty também vai ao Telegram, pela fila de saída.
        self.telegram.outbox().push_message(message, self.now().epoch_ms);
    }

    /// A conversa (as últimas mensagens, da mais velha para a mais nova).
    pub fn conversation(&self) -> Vec<ConversationMessage> {
        self.store.as_ref().map(|s| s.conversation().tail(CONVERSATION_TAIL)).unwrap_or_default()
    }

    /// Pensando ou falando (`None` volta ao estado derivado).
    pub fn set_busy(&self, avatar: Option<Avatar>) {
        *lock(&self.busy) = avatar;
        self.changed(Part::Avatar);
    }

    /// Estado do avatar: desligado dorme; pensando ou falando vence; no silêncio dorme; com
    /// cartão aberto espera; senão atento.
    pub fn avatar(&self, cfg: &NantyConfig) -> Avatar {
        if self.status() != InstanceStatus::Active {
            return Avatar::Sleeping;
        }
        if let Some(busy) = *lock(&self.busy) {
            return busy;
        }
        let now = self.now();
        if now.within(cfg.quiet_window()) {
            return Avatar::Sleeping;
        }
        if lock(&self.state).cards.iter().any(|c| c.expires_at_ms > now.epoch_ms) {
            return Avatar::Waiting;
        }
        Avatar::Attentive
    }
}

/// Idioma das mensagens do Nanty (o gravado na configuração; sem ele, pt-BR).
pub fn nanty_lang(cfg: &NantyConfig) -> Lang {
    cfg.lang.as_deref().and_then(Lang::from_tag).unwrap_or_default()
}

/// Grava o idioma da interface na configuração do Nanty quando ele muda (spec 138 T01, C-08): o
/// Nanty fala no idioma da pessoa mesmo depois de um reinício. Falha de gravação não muda nada.
pub async fn remember_lang(state: &AppState, lang: Lang) {
    let tag = lang.tag();
    if state.config().read().await.nanty.lang.as_deref() == Some(tag) {
        return;
    }
    let mut guard = state.config().write().await;
    if guard.nanty.lang.as_deref() == Some(tag) {
        return;
    }
    let mut next = guard.clone();
    next.nanty.lang = Some(tag.to_string());
    if super::config_api::persist(state, &next).await.is_ok() {
        *guard = next;
    }
}

/// O laço de fundo do Nanty e a escuta do Telegram (T06), devolvidos por [`start`].
#[derive(Debug)]
pub struct NantyLoop {
    handle: Option<JoinHandle<()>>,
    telegram: Option<JoinHandle<()>>,
}

impl NantyLoop {
    /// Espera o laço e a escuta terminarem (os dois param sozinhos no `shutdown_signal()`), por no
    /// máximo [`STOP_TIMEOUT`].
    pub async fn stop(self) {
        let both = async {
            if let Some(handle) = self.handle {
                let _ = handle.await;
            }
            if let Some(handle) = self.telegram {
                let _ = handle.await;
            }
        };
        let _ = tokio::time::timeout(STOP_TIMEOUT, both).await;
    }

    /// O laço está rodando (este processo é o dono).
    pub fn running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }
}

/// Liga o Nanty deste processo (chamado pelo `main.rs`). Sem a trava da pasta de dados, não sobe
/// o laço e avisa no terminal; a janela mostra "Nanty ativo em outra instância".
pub fn start(state: &AppState) -> NantyLoop {
    let nanty = Arc::clone(state.nanty());
    match nanty.acquire() {
        InstanceStatus::Active => {}
        InstanceStatus::OtherInstance => {
            terminal_notice("nanty.log.other_instance");
            return NantyLoop { handle: None, telegram: None };
        }
        _ => {
            terminal_notice("nanty.log.unavailable");
            return NantyLoop { handle: None, telegram: None };
        }
    }
    // Assina antes de soltar a tarefa: nenhum evento escapa entre o início e o laço.
    let rx = state.events().subscribe();
    let shutdown = state.shutdown_signal();
    // T06: só o dono da trava escuta o Telegram (dois processos na mesma pasta nunca disputam o bot).
    // T07: a fila de saída roda ao lado da escuta, com o mesmo encerramento.
    let (listen_state, out_state) = (state.clone(), state.clone());
    let telegram = tokio::spawn(async move {
        tokio::join!(
            telegram::listen(listen_state.clone(), listen_state.shutdown_signal()),
            telegram::outbox::run(out_state.clone(), out_state.shutdown_signal()),
        );
    });
    let state = state.clone();
    NantyLoop { handle: Some(tokio::spawn(run(state, rx, shutdown))), telegram: Some(telegram) }
}

async fn known_projects(state: &AppState) -> Vec<String> {
    state.config().read().await.projects.iter().map(|p| p.id.clone()).collect()
}

/// Uma rodada dos observadores: os projetos (sempre), o disco e a pasta temporária (sempre) e o
/// Docker (a cada [`observe::DOCKER_EVERY`] rodadas). Só os tipos realmente observados contam como
/// cobertos: uma fonte que falhou não "resolve" o que estava aberto.
async fn scan(state: &AppState, nanty: &Arc<Nanty>, round: u64) {
    let cfg: Config = state.config().read().await.clone();
    let mut signals = Vec::new();
    let mut covered = Vec::new();
    if let Some(found) = observe::observe_projects(state, &cfg).await {
        signals.extend(found);
        covered.extend(SignalKind::PROJECT_CONDITIONS);
    }
    let probe = nanty.probe();
    let docker = round % observe::DOCKER_EVERY == 0;
    let machine_cfg = cfg.clone();
    // Verificação estrita de atividade (C-16): o recurso de projeto ativo não é órfão.
    let busy: Vec<String> =
        cfg.projects.iter().map(|p| p.id.clone()).filter(|id| state.project_busy_strict(id)).collect();
    let machine =
        tokio::task::spawn_blocking(move || observe::observe_machine(probe.as_ref(), &machine_cfg, docker, &busy))
        .await
        .unwrap_or_default();
    signals.extend(machine.signals);
    covered.extend(machine.covered);
    let n = Arc::clone(nanty);
    let _ = tokio::task::spawn_blocking(move || n.observe(&cfg.nanty, &signals, &covered)).await;
    // T03: as ações do catálogo para o que está aberto agora.
    act::tick(state, nanty).await;
    nanty.scans.fetch_add(1, Ordering::Relaxed);
}

/// T05: o turno proativo dos cartões novos e o resumo da manhã, em tarefas à parte (a chamada ao
/// modelo não segura a observação; a fila única põe os turnos em ordem).
fn model_turns(state: &AppState, nanty: &Arc<Nanty>) {
    let (s, n) = (state.clone(), Arc::clone(nanty));
    tokio::spawn(async move {
        turn::proactive(&s, &n).await;
        digest::maybe_send(&s, &n).await;
    });
}

/// Corpo do laço: varredura inicial, a observação periódica e os eventos até o encerramento.
async fn run(
    state: AppState,
    mut rx: tokio::sync::broadcast::Receiver<ApiEvent>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let nanty = Arc::clone(state.nanty());
    let sweep = |nanty: Arc<Nanty>, known: Vec<String>| async move {
        let _ = tokio::task::spawn_blocking(move || nanty.full_sweep(&known)).await;
    };
    sweep(Arc::clone(&nanty), known_projects(&state).await).await;
    let mut round: u64 = 0;
    let mut next_scan = tokio::time::Instant::now();
    loop {
        tokio::select! {
            biased;
            () = super::events::wait_shutdown(&mut shutdown) => break,
            () = tokio::time::sleep_until(next_scan) => {
                scan(&state, &nanty, round).await;
                round = round.wrapping_add(1);
                model_turns(&state, &nanty);
                let cfg = state.config().read().await.nanty.clone();
                let mut wait = cfg.filter.scan_interval();
                // Uma condição sai da espera do grupo antes da próxima varredura: olha na hora.
                if let Some(due) = nanty.next_due(&cfg) {
                    wait = wait.min(due.max(Duration::from_secs(1)));
                }
                next_scan = tokio::time::Instant::now() + wait;
            }
            received = rx.recv() => match received {
                Ok(event) => {
                    if event.event_type() == EventType::ProjectRemoved.as_str() {
                        if let Some(id) = event.project_id.clone() {
                            let n = Arc::clone(&nanty);
                            let _ = tokio::task::spawn_blocking(move || n.forget_project(&id)).await;
                        }
                    }
                    let cfg = state.config().read().await.nanty.clone();
                    if let Some(signal) = observe::milestone(&event, &cfg) {
                        let n = Arc::clone(&nanty);
                        let _ = tokio::task::spawn_blocking(move || n.observe(&cfg, &[signal], &[])).await;
                    }
                    if observe::wants_scan(&event) {
                        next_scan = next_scan.min(tokio::time::Instant::now() + EVENT_DEBOUNCE);
                    }
                }
                // Perdeu eventos: refaz a varredura completa e olha tudo de novo.
                Err(RecvError::Lagged(_)) => {
                    sweep(Arc::clone(&nanty), known_projects(&state).await).await;
                    next_scan = tokio::time::Instant::now();
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
    nanty.release();
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod t02_tests;

#[cfg(test)]
mod t03_tests;

#[cfg(test)]
mod t05_tests;
