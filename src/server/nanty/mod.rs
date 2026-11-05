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
//! citado ([`read`]), como dado não confiável. Desde a spec 139 T03, o texto livre vai a um laço de
//! agente ([`agent`]) com ferramentas só de leitura ([`tools`]), memória e resposta em texto puro.
//!
//! **Voz (T09; spec 139 T07 e T08):** a nota de voz do dono é transcrita e segue como texto. Com a
//! voz ligada, toda saída do Nanty fala, só pelo Gemini e com volta para o texto ([`voice`]). Os
//! áudios ficam na pasta do Nanty e saem junto com as mensagens antigas da conversa, ou antes delas,
//! pelo teto próprio (spec 140 T05).
//!
//! **Guarda em disco (spec 140 T05):** a conversa gira com o teto de `[nanty.storage]` e nunca tira
//! uma linha que o resumo ([`summary`], em arquivo próprio) ainda não cobriu, salvo com o resumo
//! desligado, quando entra a linha do servidor ([`store::Conversation`]).
//!
//! **Cenário proativo (spec 140 T12):** nos avisos importantes, depois do cartão enviado, uma análise
//! só de leitura do projeto do aviso, no lugar dela ([`analysis`]), vira uma mensagem à parte
//! ([`scenario`]).
//!
//! **Leitura profunda (spec 140 T09):** o leitor de arquivos próprio ([`files`], `openat2` ou
//! caminhada por `openat`, sem link, com bloqueio, tetos e redator) e o histórico do Git sem a
//! configuração do repositório ([`git_log`]) são a base das ferramentas de leitura da T10.

pub mod act;
pub mod agent;
pub mod analysis;
pub mod actions;
pub mod api;
pub mod can_do;
pub mod chat;
pub mod clock;
pub mod deep;
pub mod digest;
pub mod docs;
pub mod draft;
pub mod files;
pub mod filter;
pub mod git_log;
pub mod history;
pub mod instance;
pub mod journal;
pub mod jsonl;
pub mod language;
pub mod link;
pub mod memory;
pub mod notice;
pub mod observe;
pub mod provenance;
pub mod read;
pub mod redact;
pub mod render;
pub mod scenario;
pub mod settings;
pub mod signals;
pub mod silence;
pub mod store;
pub mod summarize;
pub mod summary;
pub mod telegram;
pub mod think;
pub mod tools;
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
use self::provenance::Scoped;
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

/// Quem está pensando e quem está falando agora (spec 140 T11, C-13).
#[derive(Debug, Default)]
struct Busy {
    thinking: usize,
    speaking: usize,
}

impl Busy {
    fn add(&mut self, speaking: bool, up: bool) {
        let n = if speaking { &mut self.speaking } else { &mut self.thinking };
        *n = if up { n.saturating_add(1) } else { n.saturating_sub(1) };
    }

    /// A fala vence o "pensando"; sem nenhum dos dois, vale o estado derivado.
    fn avatar(&self) -> Option<Avatar> {
        if self.speaking > 0 {
            Some(Avatar::Speaking)
        } else if self.thinking > 0 {
            Some(Avatar::Thinking)
        } else {
            None
        }
    }
}

/// Um turno pensando ou falando ([`Nanty::thinking`], [`Nanty::speaking`]): solto em qualquer saída.
pub struct BusyGuard<'a> {
    nanty: &'a Nanty,
    speaking: bool,
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        lock(&self.nanty.busy).add(self.speaking, false);
        self.nanty.changed(Part::Avatar);
    }
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
    /// A memória de longo prazo (spec 140 T07), na pasta `memory` do Nanty.
    memory: Option<memory::store::MemoryStore>,
    clock: NantyClock,
    events: EventBus,
    state: Mutex<NantyState>,
    status: Mutex<InstanceStatus>,
    lock: Mutex<Option<InstanceLock>>,
    /// Pensando ou falando (turno do modelo e voz, T05/T09); vence o estado derivado. Conta quem está
    /// em cada estado (spec 140 T11, C-13): a conversa e a análise rodam juntas, e o fim de uma não
    /// apaga o "pensando" da outra.
    busy: Mutex<Busy>,
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
    /// O lugar da análise (spec 140 T11, C-13): uma por vez, fora da fila da conversa.
    analysis: analysis::Lane,
    /// O cenário proativo em andamento e as recusas já anotadas (spec 140 T12).
    scenario: scenario::Runtime,
    /// Modelo simulado (testes); `None` = o cliente da conexão do `[nanty]`.
    backend: Mutex<Option<Arc<dyn crate::agents::chat::ChatBackend>>>,
    /// Modelo simulado só do resumo (testes); `None` = o mesmo da conversa.
    summary_backend: Mutex<Option<Arc<dyn crate::agents::chat::ChatBackend>>>,
    /// O resumo da conversa está rodando (spec 140 T06): uma execução por vez.
    summarizing: std::sync::atomic::AtomicBool,
    /// O degrau do resumo parou numa geração ilegível (revisão 1 da T06, F-01).
    summary_blocked: std::sync::atomic::AtomicBool,
    /// O canal do Telegram (T06): saúde, bot e pareamento, em memória.
    telegram: telegram::Link,
    /// A lista de vozes do Gemini, guardada por pouco tempo (spec 139 T07).
    voices: voice::VoicesCache,
    /// A vez de cada mensagem que fala (spec 139 T08): saem na ordem em que entraram na conversa.
    speech_turn: tokio::sync::Mutex<()>,
    /// Conexão e modelo que recuaram de um esforço maior do `deep_think` (spec 140 T13), só neste
    /// processo: o próximo pedido é recusado antes da rodada.
    effort_rejected: Mutex<std::collections::HashSet<String>>,
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
            memory: dir.as_ref().map(|d| memory::store::MemoryStore::new(d.join(memory::MEMORY_DIR))),
            journal: dir.map(Journal::new),
            clock,
            events,
            state: Mutex::new(state),
            status: Mutex::new(InstanceStatus::NotStarted),
            lock: Mutex::new(None),
            busy: Mutex::new(Busy::default()),
            sweeps: AtomicU64::new(0),
            probe: Mutex::new(observe::default_probe()),
            scans: AtomicU64::new(0),
            starter: Mutex::new(Arc::new(act::RealStarter)),
            turns: turn::TurnQueue::default(),
            analysis: analysis::Lane::default(),
            scenario: scenario::Runtime::default(),
            backend: Mutex::new(None),
            summary_backend: Mutex::new(None),
            summarizing: std::sync::atomic::AtomicBool::new(false),
            summary_blocked: std::sync::atomic::AtomicBool::new(false),
            telegram: telegram::Link::default(),
            voices: voice::VoicesCache::default(),
            speech_turn: tokio::sync::Mutex::new(()),
            effort_rejected: Mutex::new(std::collections::HashSet::new()),
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

    /// Anota que `key` ([`think::rejection_key`]) recuou de um esforço maior.
    pub fn note_effort_rejected(&self, key: String) {
        lock(&self.effort_rejected).insert(key);
    }

    /// `key` já recuou de um esforço maior neste processo.
    pub fn effort_rejected(&self, key: &str) -> bool {
        lock(&self.effort_rejected).contains(key)
    }

    /// O lugar da análise, à parte da fila da conversa.
    pub fn analysis(&self) -> &analysis::Lane {
        &self.analysis
    }

    /// O cenário proativo deste processo (spec 140 T12).
    pub fn scenario(&self) -> &scenario::Runtime {
        &self.scenario
    }

    /// O cartão `id` segue aberto (nem decidido, nem resolvido, nem vencido).
    pub fn card_open(&self, id: &str) -> bool {
        let now = self.now().epoch_ms;
        lock(&self.state).cards.iter().any(|c| c.id == id && c.expires_at_ms > now)
    }

    /// O modelo simulado, se houver (testes).
    pub fn backend(&self) -> Option<Arc<dyn crate::agents::chat::ChatBackend>> {
        lock(&self.backend).clone()
    }

    /// Troca o modelo por um simulado (testes: nunca chave real).
    /// O modelo do resumo: o simulado só dele, ou o da conversa.
    pub fn summary_backend(&self) -> Option<Arc<dyn crate::agents::chat::ChatBackend>> {
        lock(&self.summary_backend).clone().or_else(|| self.backend())
    }

    /// Troca o modelo só do resumo (testes).
    pub fn set_summary_backend(&self, backend: Arc<dyn crate::agents::chat::ChatBackend>) {
        *lock(&self.summary_backend) = Some(backend);
    }

    /// A vez do resumo, se ninguém o está rodando: solta ao cair a guarda.
    pub fn summary_turn(&self) -> Option<summarize::Running<'_>> {
        summarize::Running::take(&self.summarizing)
    }

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
        let before = guard.cards.clone();
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
        // Spec 141 T06: o cartão decidido, resolvido ou vencido vira um evento na conversa.
        let events = notice::events(&before, &guard, self.now().epoch_ms);
        drop(guard);
        self.keep_events(events);
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
        let before = guard.cards.clone();
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
        let events = notice::events(&before, &guard, self.now().epoch_ms);
        drop(guard);
        self.keep_events(events);
        true
    }

    /// Grava os eventos de cartão na conversa (spec 141 T06, C-07), sem ir ao Telegram: lá, a
    /// mensagem do cartão já ganha a linha final. A decisão entra sempre; a resolução e a expiração,
    /// só do cartão que a pessoa viu (citado na conversa ou enviado ao Telegram).
    fn keep_events(&self, events: Vec<notice::Pending>) {
        if events.is_empty() || !self.owns() {
            return;
        }
        let unseen = events.iter().any(|e| e.needs_seen && !e.sent);
        let cited: Vec<String> = if unseen {
            self.conversation().iter().flat_map(|m| m.card_ids.iter().cloned().chain(m.notices.iter().map(|f| f.card_id.clone()))).collect()
        } else {
            Vec::new()
        };
        for event in events.iter().filter(|e| !e.needs_seen || e.sent || cited.contains(&e.fact.card_id)) {
            self.keep_message(&notice::event_message(event));
        }
    }

    /// Guarda o `Host` de um aparelho autorizado (spec 140 T03), só quando muda: esta chamada corre a
    /// cada requisição do aparelho.
    pub fn note_device_host(&self, host: &str) {
        self.update_quiet(|st| {
            if st.device_host.as_deref() == Some(host) {
                return false;
            }
            st.device_host = Some(host.to_string());
            true
        });
    }

    /// Tira tudo o que é do projeto removido: o estado, as linhas do registro e da conversa (todas
    /// as gerações) e os itens do resumo, inclusive o que também é de outro projeto (o texto não se
    /// separa por projeto, C-16). Spec 140 T07: também as entradas da memória e os pedidos do cartão de
    /// confirmação dela, pelo mesmo [`Scoped`].
    pub fn forget_project(&self, project_id: &str) {
        self.update(|st| {
            let state = st.forget_project(project_id);
            memory::card::retain_in(st, |p| !p.touches(project_id)) || state
        });
        if !self.owns() {
            return;
        }
        self.forget_project_memory(project_id);
        let journal = self.journal.as_ref().is_some_and(|j| j.forget_project(project_id).is_err());
        let conversation =
            self.store.as_ref().is_some_and(|s| s.conversation().retain(|m| !m.touches(project_id)).is_err());
        let summary = self.store.as_ref().and_then(|s| s.summary().forget_project(project_id).err());
        if journal || conversation {
            terminal_notice("nanty.log.save_failed");
        }
        self.summary_failed(summary);
        voice::prune(self);
        self.changed(Part::Conversation);
    }

    /// Varredura completa (a inicial e a do `Lagged`). Nesta tarefa, tira os itens vencidos e
    /// tudo o que é de projetos que não existem mais (estado, registro, conversa e resumo): cobre o
    /// `ProjectRemoved` engolido por um `Lagged` e o projeto removido enquanto este processo não
    /// era o dono. Os observadores entram na T02.
    pub fn full_sweep(&self, known_projects: &[String]) {
        let now = self.now().epoch_ms;
        self.update(|st| {
            let expired = st.prune_expired(now);
            let pending = memory::card::retain_in(st, |p| p.within(|id| known_projects.iter().any(|k| k == id)));
            st.retain_projects(known_projects) || expired || pending
        });
        if self.owns() {
            // Spec 140 T07: a memória dos projetos que não existem mais.
            self.retain_known_memory(known_projects);
            let known = |p: &Option<String>| p.as_ref().is_none_or(|id| known_projects.contains(id));
            let known_id = |id: &str| known_projects.iter().any(|k| k == id);
            let journal = self.journal.as_ref().is_some_and(|j| j.retain_known(&known).is_err());
            let conversation = self.store.as_ref().is_some_and(|s| s.conversation().retain(|m| m.within(known_id)).is_err());
            let summary = self.store.as_ref().and_then(|s| s.summary().retain(|item| item.within(known_id)).err());
            if journal || conversation {
                terminal_notice("nanty.log.save_failed");
            }
            self.summary_failed(summary);
            // T09: os áudios cuja mensagem saiu da conversa.
            voice::prune(self);
        }
        self.sweeps.fetch_add(1, Ordering::Relaxed);
    }

    /// A limpeza do resumo falhou: o motivo, em código, vai ao registro.
    fn summary_failed(&self, error: Option<summary::SummaryError>) {
        if let Some(e) = error {
            let mut entry =
                JournalEntry::new(self.now().epoch_ms, journal::JournalKind::Conversation, "conversation.summary_unsaved");
            entry.params.insert("reason".into(), json!(e.code()));
            self.record(&entry);
        }
    }

    /// Aplica a guarda em disco da configuração (`[nanty.storage]`, spec 140 T05): a rotação da
    /// conversa e os tetos dos áudios.
    pub fn apply_storage(&self, cfg: &NantyConfig) {
        if let Some(store) = &self.store {
            store.apply_storage(&cfg.storage);
        }
    }

    /// Liga ou desliga o resumo da conversa (a T06 decide, pelo modelo e pelo orçamento). Desligado,
    /// a rotação tira a linha não resumida e deixa a linha do servidor no resumo.
    pub fn set_summarizer(&self, summarizer: store::Summarizer) {
        if let Some(store) = &self.store {
            store.conversation().set_summarizer(summarizer);
        }
    }

    /// O resumo da conversa (vazio sem pasta de dados).
    pub fn summary(&self) -> summary::Summary {
        self.store.as_ref().map(|s| s.summary().load().summary()).unwrap_or_default()
    }

    /// Muda o resumo (só o dono grava); `Ok(false)` sem mudança ou sem pasta.
    pub fn update_summary(&self, f: impl FnOnce(&mut summary::Summary) -> bool) -> Result<bool, summary::SummaryError> {
        if !self.owns() {
            return Ok(false);
        }
        match &self.store {
            Some(store) => store.summary().update(f),
            None => Ok(false),
        }
    }

    /// Todas as linhas da conversa, fundidas por id, de todas as gerações: o degrau do resumo lê
    /// tudo, para nunca pular uma mensagem que ficou além da cauda. Erro se alguma geração existe e
    /// não dá para ler (spec 140 T06, revisão 1, F-01): o degrau não move a marca por cima dela.
    pub fn whole_conversation(&self) -> Result<Vec<ConversationMessage>, jsonl::JsonlError> {
        match &self.store {
            Some(store) => store.conversation().all_strict().map(store::merged_lines),
            None => Ok(Vec::new()),
        }
    }

    /// Anota se o degrau do resumo está parado por uma geração ilegível; `true` só na mudança (o
    /// registro ganha uma linha por vez que trava, não uma por degrau).
    pub fn note_summary_blocked(&self, blocked: bool) -> bool {
        self.summary_blocked.swap(blocked, Ordering::AcqRel) != blocked
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
        self.keep_message(message);
        self.send_out(message);
    }

    /// Só a fila de saída, para a mensagem já gravada (T07: a fala do Nanty também vai ao Telegram).
    pub fn send_out(&self, message: &ConversationMessage) {
        if self.owns() {
            self.telegram.outbox().push_message(message, self.now().epoch_ms);
        }
    }

    /// Só grava na conversa, sem passar pela fila de saída (só o dono grava): o que já saiu no
    /// Telegram, como a nota de voz de um aviso, ou o que espera a fala (spec 139 T08). Gravar de novo
    /// o mesmo id atualiza a mensagem ([`Self::conversation`]).
    pub fn keep_message(&self, message: &ConversationMessage) {
        if !self.owns() {
            return;
        }
        // Spec 141 T06 (C-07): a mensagem do Nanty que cita cartões grava os fatos deles agora, enquanto
        // os cartões estão no estado. A regravação do mesmo id (o áudio, a fala do aviso) pode vir de um
        // objeto sem eles (revisão 1, F-02): valem os da linha já gravada, e só sem ela os do estado.
        let filled;
        let message = if message.from == store::Speaker::Nanty && message.notices.is_empty() && !message.card_ids.is_empty() {
            let mut m = message.clone();
            let stored = self.conversation().into_iter().rfind(|s| s.id == m.id).map(|s| s.notices).filter(|n| !n.is_empty());
            m.notices = stored.unwrap_or_else(|| notice::facts_of(&lock(&self.state), &m.card_ids));
            filled = m;
            &filled
        } else {
            message
        };
        match self.store.as_ref().map(|s| s.conversation().append(message)) {
            Some(Ok(notes)) => notes.iter().for_each(|note| self.record_rotation(note)),
            Some(Err(_)) => terminal_notice("nanty.log.save_failed"),
            None => {}
        }
        self.changed(Part::Conversation);
    }

    /// O que a rotação da conversa fez fora do comum vai ao registro, só com códigos e números.
    fn record_rotation(&self, note: &store::RotationNote) {
        let at = self.now().epoch_ms;
        let kind = journal::JournalKind::Conversation;
        let entry = match note {
            store::RotationNote::Unsummarized { messages, reason } => {
                let mut e = JournalEntry::new(at, kind, "conversation.unsummarized");
                e.params.insert("messages".into(), json!(messages));
                e.params.insert("reason".into(), json!(reason));
                e
            }
            store::RotationNote::Held => JournalEntry::new(at, kind, "conversation.held"),
            store::RotationNote::Unreadable => JournalEntry::new(at, kind, "conversation.unreadable"),
            store::RotationNote::SummaryFailed(reason) => {
                let mut e = JournalEntry::new(at, kind, "conversation.summary_unsaved");
                e.params.insert("reason".into(), json!(reason));
                e
            }
        };
        self.record(&entry);
    }

    /// A conversa (as últimas mensagens, da mais velha para a mais nova), com as linhas do mesmo id
    /// fundidas ([`store::merged_lines`]). Cada mensagem tem no máximo duas linhas, então o dobro do
    /// limite sempre traz as últimas [`CONVERSATION_TAIL`]. O áudio que já saiu pelo teto dos
    /// áudios (spec 140 T05) some da mensagem: o texto continua.
    pub fn conversation(&self) -> Vec<ConversationMessage> {
        let lines = self.store.as_ref().map(|s| s.conversation().tail(CONVERSATION_TAIL * 2)).unwrap_or_default();
        let mut merged = store::merged_lines(lines);
        merged.drain(..merged.len().saturating_sub(CONVERSATION_TAIL));
        voice::drop_missing_audio(self, &mut merged);
        merged
    }

    /// Pensando enquanto a guarda existir. Cada turno tem a sua: o avatar só sai de "pensando" quando
    /// a última cai (spec 140 T11, C-13).
    pub fn thinking(&self) -> BusyGuard<'_> {
        self.busy_as(false)
    }

    /// Falando enquanto a guarda existir; a fala vence o "pensando", que volta quando ela acaba.
    pub fn speaking(&self) -> BusyGuard<'_> {
        self.busy_as(true)
    }

    fn busy_as(&self, speaking: bool) -> BusyGuard<'_> {
        lock(&self.busy).add(speaking, true);
        self.changed(Part::Avatar);
        BusyGuard { nanty: self, speaking }
    }

    /// Turnos pensando agora (a conversa, a análise, a transcrição).
    pub fn thinking_turns(&self) -> usize {
        lock(&self.busy).thinking
    }

    /// Estado do avatar: desligado dorme; pensando ou falando vence; no silêncio dorme; com
    /// cartão aberto espera; senão atento.
    pub fn avatar(&self, cfg: &NantyConfig) -> Avatar {
        if self.status() != InstanceStatus::Active {
            return Avatar::Sleeping;
        }
        if let Some(busy) = lock(&self.busy).avatar() {
            return busy;
        }
        let now = self.now();
        if now.within(cfg.quiet_window()) {
            return Avatar::Sleeping;
        }
        if lock(&self.state).cards.iter().any(|c| c.expires_at_ms > now.epoch_ms && c.awaits_person()) {
            return Avatar::Waiting;
        }
        Avatar::Attentive
    }
}

impl Nanty {
    /// O idioma da mensagem da pessoa ([`language::person_lang`]); o que veio de um sinal dela vira
    /// o último idioma da pessoa (grava só se mudou, sem avisar a janela).
    pub fn note_person_lang(&self, cfg: &NantyConfig, channel: store::Channel, text: Option<&str>) -> Lang {
        let (lang, signal, pending) = {
            let st = lock(&self.state);
            let (lang, signal) = language::person_lang(cfg, &st, channel, text);
            // Sem texto dela (a encaminhada, a nota sem fala), a pendência fica como está.
            let pending = text.map_or_else(|| st.person_lang_pending.clone(), |t| language::pending_after(&st, t).map(|l| l.tag().to_string()));
            (lang, signal, pending)
        };
        self.update_quiet(|st| {
            let spoken = if signal { Some(lang.tag().to_string()) } else { st.person_lang.clone() };
            let changed = st.person_lang != spoken || st.person_lang_pending != pending;
            st.person_lang = spoken;
            st.person_lang_pending = pending;
            changed
        });
        lang
    }

    /// Guarda o `language_code` do dono no Telegram (spec 139 T05), quando é pt ou en.
    pub fn note_telegram_lang(&self, code: Option<&str>) {
        let Some(lang) = code.and_then(language::from_language_code) else { return };
        let tag = lang.tag();
        self.update_quiet(|st| {
            let changed = st.telegram_lang.as_deref() != Some(tag);
            st.telegram_lang = Some(tag.to_string());
            changed
        });
    }
}

/// Idioma do Nanty para os avisos proativos, o resumo e o que não responde a uma mensagem (spec 139
/// T05): o `[nanty] language` escolhido; com `follow`, o último idioma da pessoa e, sem ele, o da
/// interface lembrado ([`language::resolve`]).
pub fn nanty_lang(cfg: &NantyConfig, nanty: &Nanty) -> Lang {
    language::resolve(cfg, &lock(&nanty.state))
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
    // Spec 139 T07 (C-16): a voz antiga migrada ao carregar ganha a linha do registro, e o arquivo
    // passa à forma nova (os campos antigos nunca são gravados). Tudo com a trava de escrita (T08):
    // a varredura de início pode estar lendo a configuração, e nenhuma outra gravação passa no meio.
    let (st, n) = (state.clone(), Arc::clone(&nanty));
    tokio::spawn(async move {
        let mut cfg = st.config().write().await;
        let Some(done) = cfg.nanty.voice.migrated.take() else { return };
        n.record(&voice::migration_entry(&n, &done));
        if let Err(e) = super::config_api::persist(&st, &cfg).await {
            n.record(&voice::migration_unsaved(&n, e.code().unwrap_or("config_save_failed")));
        }
    });
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
    // Spec 140 T05: a guarda em disco segue a configuração, e os áudios vencidos saem a cada rodada.
    nanty.apply_storage(&cfg.nanty);
    // Spec 140 T06: o resumo liga e desliga com o modelo e o orçamento.
    summarize::sync(nanty, &cfg);
    let n = Arc::clone(nanty);
    let _ = tokio::task::spawn_blocking(move || voice::prune(&n)).await;
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
        let spoke = matches!(turn::proactive(&s, &n).await, Some(turn::TurnOutcome::Reply(_)));
        let sent = digest::maybe_send(&s, &n).await;
        // Spec 140 T06: as falas proativas também enchem a janela.
        if spoke || sent {
            summarize::after_reply(&s, &n).await;
        }
    });
    // Spec 140 T12: o cenário proativo, reavaliado a cada passagem; a análise roda no lugar dela, e
    // nada aqui espera por ela.
    let (s, n) = (state.clone(), Arc::clone(nanty));
    tokio::spawn(async move {
        let _ = scenario::tick(&s, &n).await;
    });
}

/// Corpo do laço: varredura inicial, a observação periódica e os eventos até o encerramento.
async fn run(
    state: AppState,
    mut rx: tokio::sync::broadcast::Receiver<ApiEvent>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let nanty = Arc::clone(state.nanty());
    {
        let cfg = state.config().read().await;
        nanty.apply_storage(&cfg.nanty);
        summarize::sync(&nanty, &cfg);
    }
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

#[cfg(test)]
mod t06_tests;

#[cfg(test)]
mod t09_tests;

#[cfg(test)]
mod t10_tests;

#[cfg(test)]
mod provenance_tests;

#[cfg(test)]
mod rotation_tests;
