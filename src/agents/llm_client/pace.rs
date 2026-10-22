//! Ritmo automático por conexão+modelo (spec 089 T02).
//!
//! Um limitador no processo, chaveado por (id da conexão, modelo), compartilhado por todo
//! [`super::LlmClient`] criado de uma conexão. Nada é configurado: os tetos vêm só da observação.
//! - cabeçalhos `x-ratelimit-*` de toda resposta (restante, reset e limite);
//! - o próprio limite (HTTP 429 ou `throttled`): 70% do ritmo dos 60 s anteriores, relaxado em 25%
//!   a cada 5 min sem novo limite e descartado acima do observado + 100%.
//!
//! Sem sinal nenhum, a vaga é imediata (o comportamento de antes). O estado é só em memória.
//!
//! Cada pedido reserva a sua vaga sob o lock, na ordem de chegada, e só depois espera, fora do lock:
//! pedidos concorrentes não estouram o teto por corrida. O relógio é injetável ([`PaceClock`]) e todo
//! instante é uma `Duration` desde a origem do relógio, com contas saturadas (nada entra em pânico com
//! valores vindos de cabeçalho).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use reqwest::header::HeaderMap;

use super::log_safe;

/// Janela deslizante do ritmo observado.
pub const PACE_WINDOW: Duration = Duration::from_secs(60);
/// Teto da espera do limitador por requisição (reset de cabeçalho ou ritmo aprendido). Fica abaixo
/// dos prazos externos mais curtos (`JUDGE_TIMEOUT`, `ASK_TIMEOUT`, `REPORT_TIMEOUT`).
pub const PACE_MAX_WAIT: Duration = Duration::from_secs(60);
/// Intervalo sem novo limite depois do qual os tetos aprendidos sobem 25%.
pub const PACE_RELAX_EVERY: Duration = Duration::from_secs(5 * 60);
/// Esperas acima disto geram o aviso na atividade.
pub const PACE_NOTICE_AFTER: Duration = Duration::from_secs(1);

/// Relógio monotônico do limitador: tempo decorrido desde uma origem fixa.
pub trait PaceClock: Send + Sync {
    fn now(&self) -> Duration;
}

/// Relógio real: `Instant` da criação do limitador.
struct SystemClock(Instant);

impl PaceClock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Aviso de espera do limitador: `ajustando o ritmo ao limite do provedor: N s`.
pub fn pace_notice(wait: Duration) -> String {
    format!("{PACE_NOTICE_PREFIX}{} s", super::ceil_secs(wait))
}

const PACE_NOTICE_PREFIX: &str = "ajustando o ritmo ao limite do provedor: ";

/// Segundos de um aviso de [`pace_notice`] (spec 102: a atividade o publica com `code`); outro
/// texto devolve `None`.
pub fn pace_notice_secs(text: &str) -> Option<u64> {
    text.strip_prefix(PACE_NOTICE_PREFIX)?.strip_suffix(" s")?.parse().ok()
}

/// Cabeçalhos de cota de uma resposta. Valor ausente ou inválido = `None` (ignorado, sem erro).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub remaining_requests: Option<u64>,
    pub remaining_tokens: Option<u64>,
    pub reset_requests: Option<Duration>,
    pub reset_tokens: Option<Duration>,
    pub limit_requests: Option<u64>,
    pub limit_tokens: Option<u64>,
}

impl Quota {
    /// Lê os `x-ratelimit-*` conhecidos; `None` quando nenhum veio válido.
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        let count = |name: &str| text(name).and_then(|v| v.trim().parse::<u64>().ok());
        let quota = Self {
            remaining_requests: count("x-ratelimit-remaining-requests"),
            remaining_tokens: count("x-ratelimit-remaining-tokens"),
            reset_requests: text("x-ratelimit-reset-requests").and_then(parse_reset),
            reset_tokens: text("x-ratelimit-reset-tokens").and_then(parse_reset),
            // Limite zero não serve de teto (nenhum pedido passaria): ignorado.
            limit_requests: count("x-ratelimit-limit-requests").filter(|n| *n > 0),
            limit_tokens: count("x-ratelimit-limit-tokens").filter(|n| *n > 0),
        };
        (quota != Self::default()).then_some(quota)
    }
}

/// Tempo até o reset: segundos inteiros (`30`) ou componentes com unidade (`6m0s`, `1s`, `20ms`,
/// `1.5s`, `1h2m`). Qualquer outra coisa (vazio, sinal, unidade desconhecida, valor enorme) = `None`.
pub fn parse_reset(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return value.parse::<u64>().ok().map(Duration::from_secs);
    }
    let mut total = 0f64;
    let mut rest = value;
    while !rest.is_empty() {
        let num_len = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        let (num, tail) = rest.split_at(num_len);
        if num.is_empty() {
            return None;
        }
        let n: f64 = num.parse().ok()?;
        let unit_len = tail.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(tail.len());
        let (unit, tail) = tail.split_at(unit_len);
        let factor = match unit {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            "ms" => 0.001,
            _ => return None,
        };
        total += n * factor;
        rest = tail;
    }
    Duration::try_from_secs_f64(total).ok()
}

/// Pedido na janela: instante da vaga e custo em tokens (estimado; depois, o `usage` real).
struct Entry {
    id: u64,
    at: Duration,
    tokens: u64,
}

/// Restante informado por cabeçalho e o instante do reset (teto [`PACE_MAX_WAIT`]).
struct Gate {
    remaining: u64,
    reset_at: Option<Duration>,
}

/// Tetos aprendidos com o limite e o ritmo observado que os originou.
struct Learned {
    rpm: u64,
    tpm: u64,
    seen_rpm: u64,
    seen_tpm: u64,
    /// Último limite ou último relaxamento.
    since: Duration,
}

#[derive(Default)]
struct PaceState {
    entries: Vec<Entry>,
    /// Vaga mais tardia já reservada (ordem de chegada).
    last_slot: Duration,
    requests: Option<Gate>,
    tokens: Option<Gate>,
    header_rpm: Option<u64>,
    header_tpm: Option<u64>,
    learned: Option<Learned>,
}

/// Menor dos dois tetos presentes.
fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// +25%, arredondando para cima (sempre sobe ao menos 1).
fn grow(n: u64) -> u64 {
    n.saturating_mul(5).saturating_add(3) / 4
}

/// 70% de `n`, no mínimo 1.
fn seventy(n: u64) -> u64 {
    (n.saturating_mul(7) / 10).max(1)
}

impl PaceState {
    /// Tetos efetivos: o menor entre o do cabeçalho e o aprendido.
    fn ceilings(&self) -> (Option<u64>, Option<u64>) {
        let learned = self.learned.as_ref();
        (
            min_opt(self.header_rpm, learned.map(|l| l.rpm)),
            min_opt(self.header_tpm, learned.map(|l| l.tpm)),
        )
    }

    /// Tira da janela o que já saiu dela e esquece resets vencidos.
    fn prune(&mut self, now: Duration) {
        self.entries.retain(|e| e.at.saturating_add(PACE_WINDOW) > now);
        for gate in [&mut self.requests, &mut self.tokens] {
            if gate.as_ref().is_some_and(|g| g.reset_at.is_none_or(|r| r <= now)) {
                *gate = None;
            }
        }
    }

    /// Relaxa os tetos aprendidos: +25% a cada [`PACE_RELAX_EVERY`] desde o último limite; acima do
    /// observado + 100% (nos dois), o aprendizado é descartado. `true` quando algo mudou.
    fn relax(&mut self, now: Duration) -> bool {
        let Some(l) = self.learned.as_mut() else {
            return false;
        };
        let mut changed = false;
        let mut discard = false;
        while let Some(next) = l.since.checked_add(PACE_RELAX_EVERY).filter(|n| *n <= now) {
            l.since = next;
            l.rpm = grow(l.rpm);
            l.tpm = grow(l.tpm);
            changed = true;
            if l.rpm > l.seen_rpm.saturating_mul(2) && l.tpm > l.seen_tpm.saturating_mul(2) {
                discard = true;
                break;
            }
        }
        if discard {
            self.learned = None;
        }
        changed
    }

    /// Instante da vaga de um pedido de `est` tokens chegado em `now`: depois da última vaga
    /// reservada, dos resets dos cabeçalhos com restante esgotado e de quando a janela comporta o
    /// pedido nos tetos. Janela vazia sempre comporta (pedido maior que o teto não trava). Teto de
    /// [`PACE_MAX_WAIT`] a partir de `now`.
    fn slot(&self, now: Duration, est: u64) -> Duration {
        let mut t = now.max(self.last_slot);
        if let Some(Gate { remaining, reset_at: Some(reset) }) = &self.requests {
            if *remaining <= 1 {
                t = t.max(*reset);
            }
        }
        if let Some(Gate { remaining, reset_at: Some(reset) }) = &self.tokens {
            if *remaining < est {
                t = t.max(*reset);
            }
        }
        let (rpm, tpm) = self.ceilings();
        if rpm.is_some() || tpm.is_some() {
            loop {
                let window = self.entries.iter().filter(|e| e.at.saturating_add(PACE_WINDOW) > t);
                let (count, tokens, oldest) = window.fold((0u64, 0u64, None::<Duration>), |(c, k, o), e| {
                    (c.saturating_add(1), k.saturating_add(e.tokens), Some(o.map_or(e.at, |o: Duration| o.min(e.at))))
                });
                let Some(oldest) = oldest else { break };
                let fits_rpm = rpm.is_none_or(|r| count.saturating_add(1) <= r);
                let fits_tpm = tpm.is_none_or(|m| tokens.saturating_add(est) <= m);
                if fits_rpm && fits_tpm {
                    break;
                }
                // Espera o mais antigo da janela sair dela; cada volta tira ao menos um pedido.
                t = t.max(oldest.saturating_add(PACE_WINDOW));
            }
        }
        t.min(now.saturating_add(PACE_MAX_WAIT))
    }
}

/// Linha de log de um teto aprendido, vindo de cabeçalho ou relaxado.
struct PaceLog {
    conn: String,
    model: String,
    rpm: Option<u64>,
    tpm: Option<u64>,
    origin: &'static str,
}

impl PaceLog {
    fn new(key: &(String, String), st: &PaceState, origin: &'static str) -> Self {
        let (rpm, tpm) = st.ceilings();
        Self { conn: key.0.clone(), model: key.1.clone(), rpm, tpm, origin }
    }

    /// `nants: ritmo conexão=<id> modelo=<filtrado> rpm=<n> tpm=<n> origem=<…>`; teto ausente = `-`.
    fn emit(&self) {
        let n = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
        eprintln!(
            "nants: ritmo conexão={} modelo={} rpm={} tpm={} origem={}",
            log_safe(&self.conn),
            log_safe(&self.model),
            n(self.rpm),
            n(self.tpm),
            self.origin
        );
    }
}

#[derive(Default)]
struct Pairs {
    next_id: u64,
    map: HashMap<(String, String), PaceState>,
}

/// Registro de ritmo por (id da conexão, modelo).
pub struct Pacer {
    clock: Arc<dyn PaceClock>,
    pairs: Mutex<Pairs>,
}

impl Pacer {
    pub fn new(clock: Arc<dyn PaceClock>) -> Arc<Self> {
        Arc::new(Self { clock, pairs: Mutex::new(Pairs::default()) })
    }

    /// Registro do processo, com o relógio real. Nos testes do crate é um por thread: cada teste
    /// (com o seu runtime) tem o seu, sem interferir nos demais.
    pub fn global() -> Arc<Self> {
        #[cfg(test)]
        {
            thread_local! {
                static LOCAL: Arc<Pacer> = Pacer::new(Arc::new(SystemClock(Instant::now())));
            }
            LOCAL.with(Arc::clone)
        }
        #[cfg(not(test))]
        {
            static GLOBAL: std::sync::OnceLock<Arc<Pacer>> = std::sync::OnceLock::new();
            Arc::clone(GLOBAL.get_or_init(|| Pacer::new(Arc::new(SystemClock(Instant::now())))))
        }
    }

    fn lock(&self) -> MutexGuard<'_, Pairs> {
        self.pairs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserva a vaga de um pedido de `est` tokens e devolve o bilhete e a espera até ela (zero sem
    /// sinal). A reserva entra na janela já agora, na ordem de chegada; o lock não passa daqui.
    pub fn reserve(self: &Arc<Self>, conn: &str, model: &str, est: u64) -> (PaceTicket, Duration) {
        let now = self.clock.now();
        let key = (conn.to_string(), model.to_string());
        let mut log = None;
        let (id, wait) = {
            let mut pairs = self.lock();
            let id = pairs.next_id;
            pairs.next_id = pairs.next_id.wrapping_add(1);
            let st = pairs.map.entry(key.clone()).or_default();
            st.prune(now);
            if st.relax(now) {
                log = Some(PaceLog::new(&key, st, "relaxado"));
            }
            let at = st.slot(now, est);
            st.entries.push(Entry { id, at, tokens: est });
            st.last_slot = st.last_slot.max(at);
            // A reserva consome o restante informado até o próximo cabeçalho atualizá-lo.
            if let Some(g) = st.requests.as_mut() {
                g.remaining = g.remaining.saturating_sub(1);
            }
            if let Some(g) = st.tokens.as_mut() {
                g.remaining = g.remaining.saturating_sub(est);
            }
            (id, at.saturating_sub(now))
        };
        if let Some(log) = log {
            log.emit();
        }
        (PaceTicket { pacer: Arc::clone(self), key, id, sent: false }, wait)
    }

    /// Registra agora um pedido que saiu sem reserva própria (reenvio interno da mesma tentativa):
    /// entra na janela e consome o restante informado, sem espera.
    pub fn record(&self, conn: &str, model: &str, tokens: u64) {
        let now = self.clock.now();
        let mut pairs = self.lock();
        let id = pairs.next_id;
        pairs.next_id = pairs.next_id.wrapping_add(1);
        let st = pairs.map.entry((conn.to_string(), model.to_string())).or_default();
        st.prune(now);
        st.entries.push(Entry { id, at: now, tokens });
        if let Some(g) = st.requests.as_mut() {
            g.remaining = g.remaining.saturating_sub(1);
        }
        if let Some(g) = st.tokens.as_mut() {
            g.remaining = g.remaining.saturating_sub(tokens);
        }
    }

    /// Cabeçalhos de cota de uma resposta (sucesso, erro ou streaming) do par.
    pub fn observe(&self, conn: &str, model: &str, quota: &Quota) {
        let now = self.clock.now();
        let key = (conn.to_string(), model.to_string());
        let reset_at = |d: Option<Duration>| d.map(|d| now.saturating_add(d.min(PACE_MAX_WAIT)));
        let mut log = None;
        {
            let mut pairs = self.lock();
            let st = pairs.map.entry(key.clone()).or_default();
            if let Some(remaining) = quota.remaining_requests {
                st.requests = Some(Gate { remaining, reset_at: reset_at(quota.reset_requests) });
            }
            if let Some(remaining) = quota.remaining_tokens {
                st.tokens = Some(Gate { remaining, reset_at: reset_at(quota.reset_tokens) });
            }
            let rpm = quota.limit_requests.or(st.header_rpm);
            let tpm = quota.limit_tokens.or(st.header_tpm);
            if (rpm, tpm) != (st.header_rpm, st.header_tpm) {
                st.header_rpm = rpm;
                st.header_tpm = tpm;
                log = Some(PaceLog::new(&key, st, "cabecalho"));
            }
        }
        if let Some(log) = log {
            log.emit();
        }
    }

    /// O provedor limitou o par (HTTP 429 ou `throttled`): tetos de 70% das requisições e dos tokens
    /// da janela até agora (mínimo 1), recalculados a cada novo limite.
    pub fn on_limit(&self, conn: &str, model: &str) {
        let now = self.clock.now();
        let key = (conn.to_string(), model.to_string());
        let log = {
            let mut pairs = self.lock();
            let st = pairs.map.entry(key.clone()).or_default();
            st.prune(now);
            let (count, tokens) = st
                .entries
                .iter()
                .filter(|e| e.at <= now)
                .fold((0u64, 0u64), |(c, k), e| (c.saturating_add(1), k.saturating_add(e.tokens)));
            st.learned = Some(Learned {
                rpm: seventy(count),
                tpm: seventy(tokens),
                seen_rpm: count.max(1),
                seen_tpm: tokens.max(1),
                since: now,
            });
            PaceLog::new(&key, st, "limite")
        };
        log.emit();
    }

    /// Desfaz uma reserva cujo pedido não chegou a sair (parada ou futuro descartado na espera).
    fn release(&self, key: &(String, String), id: u64) {
        let mut pairs = self.lock();
        if let Some(st) = pairs.map.get_mut(key) {
            st.entries.retain(|e| e.id != id);
            st.last_slot = st.entries.iter().map(|e| e.at).max().unwrap_or_default();
        }
    }

    /// Troca o custo estimado do pedido pelo `usage` real.
    fn settle(&self, key: &(String, String), id: u64, tokens: u64) {
        let mut pairs = self.lock();
        if let Some(e) = pairs.map.get_mut(key).and_then(|st| st.entries.iter_mut().find(|e| e.id == id)) {
            e.tokens = tokens;
        }
    }

    /// Tetos efetivos do par (testes).
    #[cfg(test)]
    pub fn ceilings(&self, conn: &str, model: &str) -> (Option<u64>, Option<u64>) {
        let now = self.clock.now();
        let mut pairs = self.lock();
        pairs.map.get_mut(&(conn.to_string(), model.to_string())).map_or((None, None), |st| {
            st.relax(now);
            st.ceilings()
        })
    }

    /// `true` quando o par tem algum estado (testes).
    #[cfg(test)]
    pub fn knows(&self, conn: &str, model: &str) -> bool {
        self.lock().map.contains_key(&(conn.to_string(), model.to_string()))
    }

    /// Vagas e custos da janela do par (testes).
    #[cfg(test)]
    pub fn window(&self, conn: &str, model: &str) -> Vec<(Duration, u64)> {
        self.lock()
            .map
            .get(&(conn.to_string(), model.to_string()))
            .map(|st| st.entries.iter().map(|e| (e.at, e.tokens)).collect())
            .unwrap_or_default()
    }
}

/// Reserva de um pedido. Descartada antes de [`PaceTicket::sent`], a vaga é devolvida.
pub struct PaceTicket {
    pacer: Arc<Pacer>,
    key: (String, String),
    id: u64,
    sent: bool,
}

impl PaceTicket {
    /// O pedido saiu: a vaga fica na janela, mesmo que o futuro seja descartado depois.
    pub fn sent(&mut self) {
        self.sent = true;
    }

    /// Corrige o custo pelo `usage` real (entrada + saída).
    pub fn settle(&self, tokens: u64) {
        self.pacer.settle(&self.key, self.id, tokens);
    }
}

impl Drop for PaceTicket {
    fn drop(&mut self) {
        if !self.sent {
            self.pacer.release(&self.key, self.id);
        }
    }
}

#[cfg(test)]
#[path = "pace_tests.rs"]
mod tests;
