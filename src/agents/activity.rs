//! Atividade ao vivo dos agentes: eventos efêmeros (nunca persistidos) sobre o que o agente
//! está fazendo. O módulo não conhece o servidor: o consumidor implementa [`ActivitySink`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::llm_client::StreamDelta;
use crate::harness::progress::{CommandProgress, ProgressSnapshot};

/// Intervalo mínimo entre envios de deltas de uma atividade (~4 eventos/s).
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
/// Tamanho máximo do texto de um evento de atividade.
pub const MAX_EVENT_TEXT_BYTES: usize = 2 * 1024;
/// Tamanho máximo do rótulo de ferramenta.
const MAX_LABEL_BYTES: usize = 160;
/// Tamanho máximo do comando exibido na atividade.
const MAX_COMMAND_BYTES: usize = 200;
/// Intervalo padrão do heartbeat de comando em curso (<= 30 s).
pub const COMMAND_HEARTBEAT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Start,
    Reasoning,
    Text,
    Tool,
    /// Início de um comando longo (texto = comando redigido).
    Command,
    /// Sinal periódico de que o comando em curso segue rodando.
    Heartbeat,
    /// Fim do comando em curso.
    CommandEnd,
    /// Início de uma chamada ao modelo (spec 114 T04): o servidor anota o instante.
    Model,
    /// Sinal periódico da chamada ao modelo em curso (com os bytes recebidos, quando há). Sai como
    /// `heartbeat`, mas é um tipo próprio: não é progresso do agente e não renova prazo de
    /// inatividade nenhum (ao contrário do heartbeat de comando).
    ModelBeat,
    /// Fim da chamada ao modelo em curso.
    ModelEnd,
    End,
}

impl ActivityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Reasoning => "reasoning",
            Self::Text => "text",
            Self::Tool => "tool",
            Self::Command => "command",
            Self::Heartbeat => "heartbeat",
            Self::CommandEnd => "command_end",
            Self::Model => "model",
            Self::ModelBeat => "heartbeat",
            Self::ModelEnd => "model_end",
            Self::End => "end",
        }
    }
}

/// `code` estável e `params` neutros de idioma do texto de um evento (spec 102): a interface
/// mostra o rótulo do dicionário no idioma ativo; `text` continua como a reserva em pt-BR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityCode {
    pub code: String,
    pub params: serde_json::Map<String, Value>,
}

impl ActivityCode {
    fn new(code: impl Into<String>, pairs: &[(&str, String)]) -> Self {
        Self {
            code: code.into(),
            params: pairs.iter().map(|(k, v)| ((*k).to_string(), Value::from(v.clone()))).collect(),
        }
    }
}

/// Evento de atividade, ainda sem redação (o sink do consumidor redige).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEvent {
    pub agent: String,
    pub spec_id: Option<String>,
    pub task_id: Option<String>,
    pub activity_id: String,
    pub kind: ActivityKind,
    pub text: Option<String>,
    /// `code`/`params` do texto (rótulo de ferramenta, aviso do limitador), quando há.
    pub code: Option<ActivityCode>,
    /// Bytes recebidos da resposta do modelo em curso (só no `heartbeat` da chamada, quando há).
    pub received_bytes: Option<u64>,
    /// Progresso real do comando em curso (spec 123 T01, só no `heartbeat` do comando): bytes de
    /// saída capturados até agora e a última linha não vazia, já redigida e cortada.
    pub output_bytes: Option<u64>,
    pub last_line: Option<String>,
    /// Limite de tempo do comando que começa (spec 123 T02, só no `command`), em ms: a tela mede o
    /// tempo contra ele. `None` quando o caminho não tem limite conhecido.
    pub command_limit_ms: Option<u64>,
    /// Tipo da chamada de saída estruturada (spec 121), quando o passo pede JSON ao modelo: a
    /// interface mostra um texto do dicionário no lugar do texto cru (ver [`structured`]).
    pub structured: Option<&'static str>,
}

/// Códigos estáveis das chamadas de saída estruturada (spec 121): o texto que o modelo escreve
/// nelas é JSON para o servidor, não texto de pessoa; a Atividade mostra o rótulo do código.
pub mod structured {
    /// Revisão do plano pelo Revisor (`plan_review`, JSON `{"issues": [...]}`).
    pub const PLAN_REVIEW: &str = "plan_review";
    /// Veredito do Revisor numa tarefa (JSON do contrato da revisão).
    pub const VERDICT: &str = "verdict";
    /// Verificação final dos critérios globais pelo Revisor (mesmo contrato, tarefa `FINAL`).
    pub const FINAL_CHECK: &str = "final_check";
    /// Diagnóstico de bloqueio de ambiente pelo Arquiteto (JSON `{"root_cause","what_to_do"}`).
    pub const DIAGNOSIS: &str = "diagnosis";
    /// Análise de recuperação de tarefa bloqueada pelo Arquiteto (JSON da decisão).
    pub const RECOVERY: &str = "recovery";
    /// Resumo do histórico da sessão pelo próprio modelo do papel (spec 122 T04): o retrato é
    /// para o modelo, não texto de pessoa.
    pub const SUMMARY: &str = "summary";
    /// Decisão do Arquiteto sobre apontamentos contestados pelo Desenvolvedor (spec 124 T02, JSON
    /// `{"decisions": [...]}`).
    pub const ARBITRATION: &str = "arbitration";
    /// Todos os códigos (o servidor só publica estes).
    pub const ALL: [&str; 7] = [PLAN_REVIEW, VERDICT, FINAL_CHECK, DIAGNOSIS, RECOVERY, SUMMARY, ARBITRATION];
}

/// Destino dos eventos de atividade.
pub trait ActivitySink: Send + Sync {
    fn emit(&self, event: ActivityEvent);

    /// Redige segredos de um texto inteiro (antes de qualquer corte). Padrão: identidade.
    fn sanitize(&self, text: &str) -> String {
        text.to_string()
    }
}

/// Sink que descarta tudo (padrão).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl ActivitySink for NullSink {
    fn emit(&self, _event: ActivityEvent) {}
}

/// Identidade de quem emite: sink + agente + spec/tarefa.
#[derive(Clone)]
pub struct ActivityScope {
    sink: Arc<dyn ActivitySink>,
    agent: String,
    spec_id: Option<String>,
    task_id: Option<String>,
    heartbeat: Duration,
    /// Código da saída estruturada pedida neste escopo (spec 121), levado em todo evento.
    structured: Option<&'static str>,
}

impl ActivityScope {
    pub fn new(
        sink: Arc<dyn ActivitySink>,
        agent: impl Into<String>,
        spec_id: Option<String>,
        task_id: Option<String>,
    ) -> Self {
        Self {
            sink,
            agent: agent.into(),
            spec_id,
            task_id,
            heartbeat: COMMAND_HEARTBEAT,
            structured: None,
        }
    }

    /// Marca o escopo como chamada de saída estruturada (spec 121; código de [`structured`]).
    pub fn with_structured(mut self, code: &'static str) -> Self {
        self.structured = Some(code);
        self
    }

    /// Intervalo do heartbeat de comando (injetável em testes).
    pub fn with_heartbeat(mut self, interval: Duration) -> Self {
        self.heartbeat = interval;
        self
    }

    pub fn null(agent: impl Into<String>) -> Self {
        Self::new(Arc::new(NullSink), agent, None, None)
    }

    /// Inicia uma atividade (emite `start`). O `end` sai em [`ActivityRun::end`] ou no `drop`.
    pub fn begin(&self) -> ActivityRun {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let run = ActivityRun {
            scope: self.clone(),
            id: format!("act-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            state: Mutex::new(State::new()),
        };
        run.send(ActivityKind::Start, None);
        run
    }
}

/// Acumulado máximo por tipo (mantém o fim).
const MAX_ACC_BYTES: usize = 64 * 1024;
/// Cauda sem espaço retida no meio do stream (o segredo pode ainda estar chegando).
const MAX_HELD_TAIL_BYTES: usize = 256;
const MASK: &str = "***";

struct State {
    /// Texto acumulado: [reasoning, text].
    acc: [String; 2],
    /// Último snapshot emitido por tipo (evita repetição).
    emitted: [String; 2],
    last_kind: Option<usize>,
    last_flush: Instant,
    ended: bool,
    /// Bytes recebidos da resposta em curso (`StreamDelta::Received`); exibidos no ritmo do heartbeat.
    received: u64,
}

impl State {
    fn new() -> Self {
        Self {
            acc: [String::new(), String::new()],
            emitted: [String::new(), String::new()],
            last_kind: None,
            last_flush: Instant::now(),
            ended: false,
            received: 0,
        }
    }
}

const KINDS: [ActivityKind; 2] = [ActivityKind::Reasoning, ActivityKind::Text];

/// Uma atividade em andamento (um passo do agente).
///
/// Eventos `reasoning` e `text` são SNAPSHOTS: o acumulado do tipo, redigido inteiro
/// ([`ActivitySink::sanitize`]) e só depois cortado mantendo o fim (<= 2 KiB). Quem consome
/// substitui, não concatena.
pub struct ActivityRun {
    scope: ActivityScope,
    id: String,
    state: Mutex<State>,
}

impl ActivityRun {
    fn send(&self, kind: ActivityKind, text: Option<String>) {
        self.send_coded(kind, text, None);
    }

    fn send_coded(&self, kind: ActivityKind, text: Option<String>, code: Option<ActivityCode>) {
        self.send_full(kind, text, code, None);
    }

    fn send_full(&self, kind: ActivityKind, text: Option<String>, code: Option<ActivityCode>, received_bytes: Option<u64>) {
        self.send_event(kind, text, code, received_bytes, None, None);
    }

    fn send_event(
        &self,
        kind: ActivityKind,
        text: Option<String>,
        code: Option<ActivityCode>,
        received_bytes: Option<u64>,
        output: Option<ProgressSnapshot>,
        command_limit_ms: Option<u64>,
    ) {
        // Os `params` passam pela mesma redação do texto (o servidor ainda os saneia de novo).
        let code = code.map(|mut c| {
            for value in c.params.values_mut() {
                if let Value::String(text) = value {
                    *text = self.scope.sink.sanitize(text);
                }
            }
            c
        });
        self.scope.sink.emit(ActivityEvent {
            agent: self.scope.agent.clone(),
            spec_id: self.scope.spec_id.clone(),
            task_id: self.scope.task_id.clone(),
            activity_id: self.id.clone(),
            kind,
            text,
            code,
            received_bytes,
            output_bytes: output.as_ref().map(|o| o.bytes),
            last_line: output.and_then(|o| o.last_line),
            command_limit_ms,
            structured: self.scope.structured,
        });
    }

    /// Acumula o delta e, no máximo ~4 vezes por segundo, emite o snapshot do tipo.
    pub fn delta(&self, delta: StreamDelta) {
        let (i, text) = match delta {
            StreamDelta::Reasoning(t) => (0, t),
            StreamDelta::Text(t) => (1, t),
            StreamDelta::Retry(notice) => return self.retry(&notice),
            // Só guarda o tamanho: quem o mostra é o heartbeat de [`Self::track_model`].
            StreamDelta::Received(n) => {
                self.state.lock().unwrap_or_else(PoisonError::into_inner).received = n;
                return;
            }
            // O diagnóstico do corte vai para o registro de uso (spec 114 T05), não para a tela.
            StreamDelta::Cut(_) => return,
        };
        if text.is_empty() {
            return;
        }
        let mut out = Vec::new();
        {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.ended {
                return;
            }
            if let Some(prev) = st.last_kind.filter(|k| *k != i) {
                self.snapshot(&mut st, prev, true, &mut out);
            }
            st.last_kind = Some(i);
            st.acc[i].push_str(&text);
            if st.acc[i].len() > MAX_ACC_BYTES {
                let cut = st.acc[i].len() - MAX_ACC_BYTES;
                let mut at = cut;
                while !st.acc[i].is_char_boundary(at) {
                    at += 1;
                }
                st.acc[i].drain(..at);
            }
            if st.last_flush.elapsed() >= FLUSH_INTERVAL {
                self.snapshot(&mut st, i, false, &mut out);
                st.last_flush = Instant::now();
            }
        }
        self.send_all(out);
    }

    /// Nova tentativa da resposta (spec 088): descarta o acumulado da tentativa anterior e mostra
    /// o aviso fixo como texto do passo; os deltas da nova tentativa o substituem.
    fn retry(&self, notice: &str) {
        let shown = cut_head(&self.scope.sink.sanitize(notice).replace(['\n', '\r'], " "), MAX_EVENT_TEXT_BYTES);
        {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.ended || shown.is_empty() {
                return;
            }
            st.acc = [String::new(), String::new()];
            st.emitted = [String::new(), shown.clone()];
            st.last_kind = None;
            st.received = 0;
        }
        self.send_coded(ActivityKind::Text, Some(shown), notice_code(notice));
    }

    /// Aviso fixo durante uma ferramenta que chama o provedor (spec 090 T02: espera de ritmo ou nova
    /// tentativa da geração de imagem), exibido como o texto do passo, como na nova tentativa.
    pub fn notice(&self, text: &str) {
        self.retry(text);
    }

    /// Snapshot do tipo `i`. No meio do stream retém a última sequência sem espaço.
    fn snapshot(&self, st: &mut State, i: usize, fin: bool, out: &mut Vec<(ActivityKind, String)>) {
        let acc = &st.acc[i];
        if acc.is_empty() {
            return;
        }
        let sink = &self.scope.sink;
        let shown = if fin {
            sink.sanitize(acc)
        } else {
            let split = acc
                .char_indices()
                .rev()
                .find(|(_, c)| c.is_whitespace())
                .map_or(0, |(idx, c)| idx + c.len_utf8());
            let (visible, tail) = acc.split_at(split);
            let mut shown = sink.sanitize(visible);
            if tail.len() > MAX_HELD_TAIL_BYTES {
                shown.push_str(MASK);
            }
            shown
        };
        let shown = cut_tail(&shown, MAX_EVENT_TEXT_BYTES);
        if shown.is_empty() || st.emitted[i] == shown {
            return;
        }
        st.emitted[i] = shown.clone();
        out.push((KINDS[i], shown));
    }

    /// Rótulo curto de uma chamada de ferramenta (redigido por inteiro antes do corte).
    pub fn tool(&self, name: &str, arguments: &str) {
        let mut out = Vec::new();
        {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.ended {
                return;
            }
            for i in 0..2 {
                self.snapshot(&mut st, i, true, &mut out);
            }
        }
        let label = self.scope.sink.sanitize(&tool_label_full(name, arguments));
        let label = cut_head(&label.replace(['\n', '\r'], " "), MAX_LABEL_BYTES);
        self.send_all(out);
        self.send_coded(ActivityKind::Tool, Some(label), Some(tool_code(name, arguments)));
    }

    /// Executa `fut` (um comando longo) emitindo `command` ao iniciar, `heartbeat` a cada
    /// intervalo do escopo e `command_end` ao terminar (também se o futuro for descartado).
    pub async fn track_command<F: std::future::Future>(&self, command: &str, fut: F) -> F::Output {
        self.track_command_with(command, None, None, fut).await
    }

    /// Como [`Self::track_command`], com o progresso real da saída (spec 123 T01): cada
    /// `heartbeat` leva os bytes capturados até agora e a última linha não vazia, redigida e
    /// cortada como o comando. `limit` é o limite de tempo do caminho que vai rodar o comando
    /// (spec 123 T02), quando há: vai no `command` para a tela medir o tempo contra ele.
    pub async fn track_command_progress<F: std::future::Future>(&self, command: &str, progress: &CommandProgress, limit: Option<Duration>, fut: F) -> F::Output {
        self.track_command_with(command, Some(progress), limit, fut).await
    }

    async fn track_command_with<F: std::future::Future>(&self, command: &str, progress: Option<&CommandProgress>, limit: Option<Duration>, fut: F) -> F::Output {
        let shown = cut_head(&self.scope.sink.sanitize(command).replace(['\n', '\r'], " "), MAX_COMMAND_BYTES);
        let limit_ms = limit.and_then(|l| u64::try_from(l.as_millis()).ok()).filter(|ms| *ms > 0);
        // Só fecha o comando que chegou a abrir (o guarda também cobre o futuro descartado).
        struct EndGuard<'a>(&'a ActivityRun, bool);
        impl Drop for EndGuard<'_> {
            fn drop(&mut self) {
                if self.1 {
                    self.0.send(ActivityKind::CommandEnd, None);
                }
            }
        }
        let mut guard = EndGuard(self, false);
        // Spec 123 T02: com largada, o `command` (início do relógio e o limite) só sai quando o
        // processo vai rodar, depois da aprovação e do download da imagem. Até lá, um heartbeat
        // simples mantém o passo vivo, sem comando na tela. Recusa ou negação: nenhum `command`.
        let open = |guard: &mut EndGuard<'_>| {
            self.send_event(ActivityKind::Command, Some(shown.clone()), None, None, None, limit_ms);
            guard.1 = true;
        };
        if progress.is_none_or(CommandProgress::has_started) {
            open(&mut guard);
        }
        let mut ticker = tokio::time::interval(self.scope.heartbeat.max(Duration::from_millis(1)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        tokio::pin!(fut);
        loop {
            let waiting = !guard.1;
            // `biased`: a largada vem antes do fim do futuro. Um comando curto marca a largada e
            // termina na mesma rodada; sem a ordem, o sorteio do `select!` podia devolver o
            // resultado sem `command` nem `command_end` (spec 123 T02, F-02).
            tokio::select! {
                biased;
                _ = async { if let Some(p) = progress { p.wait_started().await } }, if waiting => open(&mut guard),
                out = &mut fut => {
                    // Segunda defesa: o processo largou e o futuro já terminou: abre agora, e o
                    // guarda fecha com `command_end`.
                    if !guard.1 && progress.is_some_and(CommandProgress::has_started) {
                        open(&mut guard);
                    }
                    return out;
                }
                _ = ticker.tick() => {
                    let output = if guard.1 { progress.map(|p| self.shown_progress(p.snapshot())) } else { None };
                    self.send_event(ActivityKind::Heartbeat, None, None, None, output, None);
                }
            }
        }
    }

    /// Progresso para a tela: a última linha passa pela mesma redação e corte do comando.
    fn shown_progress(&self, snap: ProgressSnapshot) -> ProgressSnapshot {
        let last_line = snap
            .last_line
            .map(|line| cut_line_tail(&self.scope.sink.sanitize(&line).replace(['\n', '\r'], " ")))
            .filter(|line| !line.is_empty());
        ProgressSnapshot { bytes: snap.bytes, last_line }
    }

    /// Executa `fut` (uma chamada ao modelo, spec 114 T04) emitindo `model` ao iniciar, um
    /// `heartbeat` a cada intervalo do escopo (com os bytes já recebidos, quando há) e `model_end`
    /// ao terminar (também se o futuro for descartado). Ritmo: um evento por intervalo, nunca por
    /// pedaço da resposta.
    pub async fn track_model<F: std::future::Future>(&self, fut: F) -> F::Output {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).received = 0;
        self.send(ActivityKind::Model, None);
        struct EndGuard<'a>(&'a ActivityRun);
        impl Drop for EndGuard<'_> {
            fn drop(&mut self) {
                self.0.send(ActivityKind::ModelEnd, None);
            }
        }
        let _guard = EndGuard(self);
        let mut ticker = tokio::time::interval(self.scope.heartbeat.max(Duration::from_millis(1)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        tokio::pin!(fut);
        loop {
            tokio::select! {
                out = &mut fut => return out,
                _ = ticker.tick() => {
                    let received = self.state.lock().unwrap_or_else(PoisonError::into_inner).received;
                    self.send_full(ActivityKind::ModelBeat, None, None, (received > 0).then_some(received));
                }
            }
        }
    }

    /// Encerra a atividade (idempotente) depois do snapshot final de cada tipo.
    pub fn end(&self) {
        let mut out = Vec::new();
        {
            let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.ended {
                return;
            }
            for i in 0..2 {
                self.snapshot(&mut st, i, true, &mut out);
            }
            st.ended = true;
        }
        self.send_all(out);
        self.send(ActivityKind::End, None);
    }

    fn send_all(&self, items: Vec<(ActivityKind, String)>) {
        for (kind, text) in items {
            self.send(kind, Some(text));
        }
    }
}

impl Drop for ActivityRun {
    fn drop(&mut self) {
        self.end();
    }
}

/// Última linha de saída para a tela: o FIM da linha (onde costuma estar o que mudou), até
/// [`MAX_COMMAND_BYTES`], em fronteira de caractere.
fn cut_line_tail(text: &str) -> String {
    let text = text.trim();
    if text.len() <= MAX_COMMAND_BYTES {
        return text.to_string();
    }
    let mut start = text.len() - MAX_COMMAND_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

/// Primeiros `max` bytes, em fronteira de caractere.
fn cut_head(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Últimos `max` bytes, em fronteira de caractere.
fn cut_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

/// Rótulo curto: "lendo src/x.rs", "escrevendo …", "rodando `cmd`". O consumidor redige segredos.
pub fn tool_label(name: &str, arguments: &str) -> String {
    cut_head(&tool_label_full(name, arguments).replace(['\n', '\r'], " "), MAX_LABEL_BYTES)
}

/// Rótulo sem corte (para redigir por inteiro antes de truncar).
fn tool_label_full(name: &str, arguments: &str) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let arg = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let label = match name {
        "read_file" => format!("lendo {}", arg("path")),
        "list_dir" => format!("listando {}", arg_or(arg("path"), ".")),
        "search_code" => format!("buscando `{}`", arg("pattern")),
        "write_file" | "apply_patch" => format!("escrevendo {}", arg("path")),
        "write_sdd_artifacts" => "escrevendo artefatos SDD".to_string(),
        "run_command" => format!("rodando `{}`", arg("command")),
        "fetch_url" => format!("lendo página {}", arg("url")),
        "package_versions" => format!("consultando versões de {} ({})", arg("name"), arg("ecosystem")),
        "web_source" => format!("fonte consultada: {} ({})", arg("title"), arg("url")),
        "web_search_unavailable" => "busca na web indisponível neste provedor (seguindo sem ela)".to_string(),
        // Spec 084 T02: `referencia/<arquivo>` é imagem de referência do usuário, não captura.
        "view_screenshot" => match arg("name").strip_prefix(crate::sdd::references::VIEW_PREFIX) {
            Some(file) => format!("vendo a referência {file}"),
            None => format!("vendo a captura {}", arg("name")),
        },
        "images_refused" => "o modelo não aceitou imagens; seguindo sem capturas".to_string(),
        // Spec 090 T02: só o caminho; o prompt não vai para a atividade nem para a trilha.
        "generate_image" => format!("gerando imagem {}", arg("path")),
        // Spec 091: falha final da geração, só com o motivo fixo (sem texto remoto).
        "image_failed" => format!("geração de imagem falhou ({})", arg("reason")),
        other => format!("usando {other}"),
    };
    label
}

/// `code` e `params` de um aviso fixo (spec 102): a espera do limitador e do limite do provedor
/// (089), a espera da geração de imagem e a conferência do plano. Os textos vêm das funções e
/// constantes que os montam. A nova tentativa (088) vai com o motivo neutro desde a spec 119.
pub fn notice_code(notice: &str) -> Option<ActivityCode> {
    use super::llm_client::{pace_notice_secs, throttle_notice_parts};
    if let Some(secs) = pace_notice_secs(notice) {
        return Some(ActivityCode::new("pace.waiting", &[("seconds", secs.to_string())]));
    }
    if let Some((label, secs, attempt, total)) = throttle_notice_parts(notice) {
        let mut code = ActivityCode::new("pace.throttled", &[("label", label)]);
        for (k, v) in [("seconds", secs), ("attempt", attempt), ("total", total)] {
            code.params.insert(k.into(), Value::from(v));
        }
        return Some(code);
    }
    // Spec 119: a nova tentativa vai com o motivo neutro; a tela monta o rótulo no idioma dela.
    if let Some((reason, attempt, total)) = super::llm_client::retry_notice_parts(notice) {
        let mut code = ActivityCode::new("provider.retry", &[("kind", reason.kind.as_str().to_string())]);
        if let Some(provider_code) = reason.code {
            code.params.insert("provider_code".into(), Value::from(provider_code.as_str()));
        }
        for (k, v) in [("attempt", attempt), ("total", total)] {
            code.params.insert(k.into(), Value::from(v));
        }
        return Some(code);
    }
    if notice == crate::harness::tools::IMAGE_PACE_NOTICE {
        return Some(ActivityCode::new("pace.queued", &[]));
    }
    if notice == super::architect::PLAN_CHECK_ACTIVITY {
        return Some(ActivityCode::new("plan.checking", &[]));
    }
    None
}

/// `code` e `params` do rótulo de uma ferramenta (spec 102): os mesmos valores de
/// [`tool_label_full`], sem o texto em pt-BR. O sink do consumidor redige e corta os `params`.
pub fn tool_code(name: &str, arguments: &str) -> ActivityCode {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let arg = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .replace(['\n', '\r'], " ")
    };
    let c = ActivityCode::new;
    match name {
        "read_file" => c("tool.read_file", &[("path", arg("path"))]),
        "list_dir" => c("tool.list_dir", &[("path", arg_or(arg("path"), "."))]),
        "search_code" => c("tool.search_code", &[("pattern", arg("pattern"))]),
        "write_file" | "apply_patch" => c("tool.write_file", &[("path", arg("path"))]),
        "write_sdd_artifacts" => c("tool.write_sdd_artifacts", &[]),
        "run_command" => c("tool.run_command", &[("command", arg("command"))]),
        "fetch_url" => c("tool.fetch_url", &[("url", arg("url"))]),
        "package_versions" => c("tool.package_versions", &[("name", arg("name")), ("ecosystem", arg("ecosystem"))]),
        "web_source" => c("tool.web_source", &[("title", arg("title")), ("url", arg("url"))]),
        "web_search_unavailable" => c("tool.web_search_unavailable", &[]),
        "view_screenshot" => match arg("name").strip_prefix(crate::sdd::references::VIEW_PREFIX) {
            Some(file) => c("tool.view_reference", &[("file", file.to_string())]),
            None => c("tool.view_screenshot", &[("name", arg("name"))]),
        },
        "images_refused" => c("tool.images_refused", &[]),
        "generate_image" => c("tool.generate_image", &[("path", arg("path"))]),
        // Spec 119: o motivo vai neutro (`provider`, `kind`, `provider_code`) e a tela monta o
        // `failure` no idioma dela; fora do vocabulário, o motivo como veio.
        "image_failed" => match super::llm_client::described_params(&arg("reason")) {
            Some(params) => ActivityCode { code: "tool.image_failed".into(), params },
            None => c("tool.image_failed", &[("failure", arg("reason"))]),
        },
        other => c("tool.other", &[("name", other.to_string())]),
    }
}

fn arg_or(value: String, default: &str) -> String {
    if value.is_empty() {
        default.to_string()
    } else {
        value
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Sink que guarda os eventos em memória.
    #[derive(Default)]
    pub struct Recorder(pub Mutex<Vec<ActivityEvent>>);

    impl ActivitySink for Recorder {
        fn emit(&self, event: ActivityEvent) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
        }
    }

    impl Recorder {
        pub fn events(&self) -> Vec<ActivityEvent> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Recorder;
    use super::*;

    fn scope(rec: &Arc<Recorder>) -> ActivityScope {
        ActivityScope::new(rec.clone(), "developer", Some("s".into()), Some("T01".into()))
    }

    /// Sink de teste que redige um segredo literal, como o do servidor.
    struct Redacting(Recorder, &'static str);

    impl ActivitySink for Redacting {
        fn emit(&self, event: ActivityEvent) {
            self.0.emit(event);
        }
        fn sanitize(&self, text: &str) -> String {
            text.replace(self.1, "***")
        }
    }

    const SECRET: &str = "sk-abcdefghijklmnopqrstuvwxyz0123456789";

    fn leaks(events: &[ActivityEvent]) -> bool {
        // Qualquer fragmento do segredo com >= 6 caracteres, no texto ou nos `params`.
        let params = events
            .iter()
            .filter_map(|e| e.code.as_ref())
            .flat_map(|c| c.params.values())
            .filter_map(Value::as_str);
        events.iter().filter_map(|e| e.text.as_deref()).chain(params).any(|t| {
            SECRET
                .as_bytes()
                .windows(6)
                .any(|w| t.contains(std::str::from_utf8(w).unwrap_or("\u{0}")))
        })
    }

    fn redacting() -> (Arc<Redacting>, ActivityScope) {
        let sink = Arc::new(Redacting(Recorder::default(), SECRET));
        let scope = ActivityScope::new(sink.clone(), "developer", None, None);
        (sink, scope)
    }

    #[test]
    fn structured_scope_marks_every_event_and_plain_scope_none() {
        let rec = Arc::new(Recorder::default());
        let run = scope(&rec).with_structured(structured::PLAN_REVIEW).begin();
        run.delta(StreamDelta::Text("{\"issues\": []}".into()));
        run.tool("read_file", "{\"path\": \"a.rs\"}");
        run.end();
        let ev = rec.events();
        assert!(ev.len() >= 4, "{ev:?}");
        assert!(ev.iter().all(|e| e.structured == Some("plan_review")), "{ev:?}");
        let rec = Arc::new(Recorder::default());
        scope(&rec).begin().end();
        assert!(rec.events().iter().all(|e| e.structured.is_none()));
        assert!(structured::ALL.iter().all(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_')));
    }

    #[test]
    fn snapshots_are_coalesced_and_replace() {
        let rec = Arc::new(Recorder::default());
        let run = scope(&rec).begin();
        for i in 0..200 {
            run.delta(StreamDelta::Reasoning(format!("p{i} ")));
        }
        run.end();
        let ev = rec.events();
        assert!(ev.len() < 20, "coalescência: {} eventos", ev.len());
        assert_eq!(ev.last().map(|e| e.kind), Some(ActivityKind::End));
        let last = ev.iter().rev().find(|e| e.kind == ActivityKind::Reasoning).expect("snapshot");
        assert!(last.text.as_deref().is_some_and(|t| t.starts_with("p0 p1 ") && t.ends_with("p199 ")));
        assert!(ev.iter().all(|e| e.text.as_ref().is_none_or(|t| t.len() <= MAX_EVENT_TEXT_BYTES)));
    }

    #[test]
    fn secret_crossing_2048_without_spaces_is_masked() {
        let (sink, scope) = redacting();
        let run = scope.begin();
        run.delta(StreamDelta::Reasoning(format!("{}{SECRET}{}", "a".repeat(2040), "b".repeat(100))));
        run.end();
        assert!(!leaks(&sink.0.events()));
    }

    #[test]
    fn secret_inside_single_big_delta_is_masked() {
        let (sink, scope) = redacting();
        let run = scope.begin();
        run.delta(StreamDelta::Text(format!("{} {SECRET} {}", "x ".repeat(3000), "y ".repeat(10))));
        run.end();
        assert!(!leaks(&sink.0.events()));
    }

    #[test]
    fn secret_split_across_deltas_with_flush_between_is_masked() {
        let (sink, scope) = redacting();
        let run = scope.begin();
        std::thread::sleep(FLUSH_INTERVAL + Duration::from_millis(20));
        run.delta(StreamDelta::Reasoning("usar sk-abcde".into()));
        std::thread::sleep(FLUSH_INTERVAL + Duration::from_millis(20));
        run.delta(StreamDelta::Reasoning("fghijklmnopqrstuvwxyz0123456789 ok".into()));
        run.end();
        let ev = sink.0.events();
        assert!(!leaks(&ev));
        assert!(ev.iter().any(|e| e.text.as_deref().is_some_and(|t| t.contains("usar"))));
    }

    #[test]
    fn retry_notice_replaces_partial_text_and_next_attempt_starts_clean() {
        let rec = Arc::new(Recorder::default());
        let run = scope(&rec).begin();
        run.delta(StreamDelta::Text("parcial ".into()));
        let notice = "resposta do provedor falhou (incompleta: content_filter); repetindo (1/2)";
        run.delta(StreamDelta::Retry(notice.into()));
        run.delta(StreamDelta::Text("nova".into()));
        run.end();
        let texts: Vec<String> = rec
            .events()
            .into_iter()
            .filter(|e| e.kind == ActivityKind::Text)
            .filter_map(|e| e.text)
            .collect();
        assert!(texts.iter().any(|t| t == notice), "{texts:?}");
        assert_eq!(texts.last().map(String::as_str), Some("nova"), "{texts:?}");
    }

    #[test]
    fn throttle_notices_are_shown_and_renewed() {
        // Spec 089: cada aviso da espera de limite substitui o anterior no texto do passo.
        let rec = Arc::new(Recorder::default());
        let run = scope(&rec).begin();
        let first = crate::agents::llm_client::throttle_notice("no_capacity", Duration::from_secs(30), 3, 6);
        let renewed = crate::agents::llm_client::throttle_notice("no_capacity", Duration::from_secs(15), 3, 6);
        run.delta(StreamDelta::Retry(first.clone()));
        run.delta(StreamDelta::Retry(renewed.clone()));
        run.end();
        let texts: Vec<String> = rec
            .events()
            .into_iter()
            .filter(|e| e.kind == ActivityKind::Text)
            .filter_map(|e| e.text)
            .collect();
        assert_eq!(texts, vec![first, renewed]);
        assert_eq!(texts[0], "provedor no limite (no_capacity); aguardando 30 s (tentativa 3/6)");
    }

    #[test]
    fn secret_in_command_beyond_160_bytes_is_masked() {
        let (sink, scope) = redacting();
        let run = scope.begin();
        let cmd = format!(r#"{{"command":"echo {} {SECRET}"}}"#, "z".repeat(140));
        run.tool("run_command", &cmd);
        run.end();
        assert!(!leaks(&sink.0.events()));
    }

    /// Spec 102: o rótulo da ferramenta sai com `code` e `params` (sem o texto pt-BR), e os avisos
    /// fixos do limitador, da imagem e do plano também; o texto continua como reserva.
    #[test]
    fn tool_and_notices_carry_codes() {
        let rec = Arc::new(Recorder::default());
        let run = scope(&rec).begin();
        run.tool("generate_image", r#"{"path":"web/img/gato.webp","prompt":"um gato"}"#);
        run.tool("read_file", r#"{"path":"src/a.rs"}"#);
        run.tool("misterio", "{}");
        run.notice(&crate::agents::llm_client::pace_notice(Duration::from_millis(14_200)));
        run.notice(crate::harness::tools::IMAGE_PACE_NOTICE);
        run.notice(crate::agents::architect::PLAN_CHECK_ACTIVITY);
        run.delta(StreamDelta::Retry(crate::agents::llm_client::throttle_notice("HTTP 429", Duration::from_secs(5), 2, 6)));
        run.delta(StreamDelta::Retry("resposta do provedor falhou (x); repetindo (1/2)".into()));
        run.end();
        let coded: Vec<(Option<String>, Option<String>, serde_json::Map<String, Value>)> = rec
            .events()
            .into_iter()
            .filter(|e| matches!(e.kind, ActivityKind::Tool | ActivityKind::Text))
            .map(|e| {
                let (code, params) = e.code.map_or((None, serde_json::Map::new()), |c| (Some(c.code), c.params));
                (e.text, code, params)
            })
            .collect();
        let code_of = |i: usize| coded[i].1.as_deref();
        assert_eq!(coded[0].0.as_deref(), Some("gerando imagem web/img/gato.webp"));
        assert_eq!(code_of(0), Some("tool.generate_image"));
        assert_eq!(coded[0].2.get("path"), Some(&Value::from("web/img/gato.webp")));
        assert!(!coded[0].2.contains_key("prompt"), "o prompt não vai para a atividade");
        assert_eq!(code_of(1), Some("tool.read_file"));
        assert_eq!(code_of(2), Some("tool.other"));
        assert_eq!(coded[2].2.get("name"), Some(&Value::from("misterio")));
        assert_eq!(code_of(3), Some("pace.waiting"));
        assert_eq!(coded[3].2.get("seconds"), Some(&Value::from("15")));
        assert_eq!(code_of(4), Some("pace.queued"));
        assert_eq!(code_of(5), Some("plan.checking"));
        assert_eq!(code_of(6), Some("pace.throttled"));
        assert_eq!(coded[6].2.get("attempt"), Some(&Value::from(2)));
        assert_eq!(code_of(7), None, "motivo fora do vocabulário: só o texto");
    }

    /// Spec 119: a nova tentativa e a falha da imagem vão com o motivo neutro (`kind`,
    /// `provider_code`), sem o rótulo em pt-BR nos `params`.
    #[test]
    fn retry_and_image_failure_carry_the_neutral_reason() {
        use crate::agents::llm_client::{retry_notice, BadKind, BadReason};
        let reason = BadReason::with_code(BadKind::Incomplete, Some("content_filter"));
        let code = notice_code(&retry_notice(&reason, 1)).expect("código");
        assert_eq!(code.code, "provider.retry");
        assert_eq!(code.params.get("kind"), Some(&Value::from("incomplete")));
        assert_eq!(code.params.get("provider_code"), Some(&Value::from("content_filter")));
        assert_eq!(code.params.get("attempt"), Some(&Value::from(1)));
        assert_eq!(code.params.get("total"), Some(&Value::from(2)));
        let plain = notice_code(&retry_notice(&BadReason::new(BadKind::NoFinal), 2)).expect("código");
        assert_eq!(plain.params.get("kind"), Some(&Value::from("no_final")));
        assert!(!plain.params.contains_key("provider_code"));

        let image = tool_code("image_failed", r#"{"reason":"provider_bad_response (transmissão sem evento final)"}"#);
        assert_eq!(image.code, "tool.image_failed");
        assert_eq!(image.params.get("provider"), Some(&Value::from("provider_bad_response")));
        assert_eq!(image.params.get("kind"), Some(&Value::from("no_final")));
        let http = tool_code("image_failed", r#"{"reason":"provider_http_400 (código: invalid_value)"}"#);
        assert_eq!(http.params.get("kind"), Some(&Value::from("http_code")));
        assert_eq!(http.params.get("provider_code"), Some(&Value::from("invalid_value")));
        let bare = tool_code("image_failed", r#"{"reason":"provider_timeout"}"#);
        assert_eq!(bare.params.get("provider"), Some(&Value::from("provider_timeout")));
        assert!(!bare.params.contains_key("kind"));
        for params in [&code.params, &plain.params, &image.params, &http.params] {
            let text = Value::Object(params.clone()).to_string();
            assert!(!text.contains("incompleta") && !text.contains("transmissão") && !text.contains("código"), "{text}");
        }
        // Fora do vocabulário: o motivo como veio, em `failure`.
        let other = tool_code("image_failed", r#"{"reason":"limite de imagens da spec"}"#);
        assert_eq!(other.params.get("failure"), Some(&Value::from("limite de imagens da spec")));
    }

    #[test]
    fn tool_params_are_redacted() {
        let (sink, scope) = redacting();
        let run = scope.begin();
        run.tool("run_command", &format!(r#"{{"command":"curl -H {SECRET} x"}}"#));
        run.end();
        let events = sink.0.events();
        assert!(events.iter().any(|e| e.code.as_ref().is_some_and(|c| c.code == "tool.run_command")));
        assert!(!leaks(&events));
    }

    #[test]
    fn keeps_order_between_kinds() {
        let rec = Arc::new(Recorder::default());
        let run = scope(&rec).begin();
        run.delta(StreamDelta::Reasoning("pensando".into()));
        run.delta(StreamDelta::Text("resposta".into()));
        run.tool("run_command", r#"{"command":"cargo test"}"#);
        drop(run);
        let kinds: Vec<_> = rec.events().iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ActivityKind::Start,
                ActivityKind::Reasoning,
                ActivityKind::Text,
                ActivityKind::Tool,
                ActivityKind::End
            ]
        );
    }

    #[tokio::test]
    async fn long_command_emits_start_heartbeats_and_end_with_redaction() {
        let (sink, scope) = redacting();
        let scope = scope.with_heartbeat(Duration::from_millis(20));
        let run = scope.begin();
        let cmd = format!("echo {} {SECRET}", "z".repeat(400));
        let out = run
            .track_command(&cmd, async {
                tokio::time::sleep(Duration::from_millis(110)).await;
                7
            })
            .await;
        assert_eq!(out, 7);
        let ev = sink.0.events();
        let kinds: Vec<_> = ev.iter().map(|e| e.kind).collect();
        assert_eq!(kinds[1], ActivityKind::Command);
        assert!(kinds.iter().filter(|k| **k == ActivityKind::Heartbeat).count() >= 2);
        assert_eq!(*kinds.last().unwrap_or(&ActivityKind::Start), ActivityKind::CommandEnd);
        assert!(!leaks(&ev));
        let shown = ev[1].text.as_deref().unwrap_or_default();
        assert!(shown.len() <= MAX_COMMAND_BYTES);
    }

    /// Spec 123 T02 (F-02): com largada, um comando curto que larga e termina na mesma rodada
    /// sempre abre e fecha o par `command`/`command_end` (50 vezes, nos dois sabores de runtime,
    /// para pegar a corrida do `select!`). O que nunca larga (recusa, negação) não abre o par.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gated_command_that_starts_and_ends_in_one_poll_always_opens_and_closes() {
        for i in 0..50 {
            let (sink, scope) = redacting();
            let run = scope.with_heartbeat(Duration::from_millis(5)).begin();
            let progress = CommandProgress::gated();
            // Um pouco de espera antes de largar em parte das voltas: o laço já está no `select!`.
            let pause = i % 2 == 1;
            let out = run
                .track_command_progress("ls", &progress, Some(Duration::from_secs(300)), async {
                    if pause {
                        tokio::task::yield_now().await;
                    }
                    progress.mark_started();
                    7
                })
                .await;
            assert_eq!(out, 7);
            let kinds: Vec<_> = sink.0.events().iter().map(|e| e.kind).filter(|k| matches!(k, ActivityKind::Command | ActivityKind::CommandEnd)).collect();
            assert_eq!(kinds, vec![ActivityKind::Command, ActivityKind::CommandEnd], "volta {i}");
            let command = sink.0.events().into_iter().find(|e| e.kind == ActivityKind::Command);
            assert_eq!(command.and_then(|e| e.command_limit_ms), Some(300_000), "volta {i}");
        }
        // Largada no meio de uma espera de verdade (o comando roda depois da aprovação).
        let (sink, scope) = redacting();
        let run = scope.with_heartbeat(Duration::from_millis(5)).begin();
        let progress = CommandProgress::gated();
        run.track_command_progress("npm test", &progress, None, async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            progress.mark_started();
            tokio::time::sleep(Duration::from_millis(20)).await;
        })
        .await;
        let events = sink.0.events();
        let at = |k: ActivityKind| events.iter().position(|e| e.kind == k);
        assert!(at(ActivityKind::Heartbeat) < at(ActivityKind::Command), "o passo segue vivo antes da largada");
        assert!(at(ActivityKind::Command) < at(ActivityKind::CommandEnd));
        // O que nunca larga não abre o par.
        let (sink, scope) = redacting();
        let run = scope.with_heartbeat(Duration::from_millis(5)).begin();
        let progress = CommandProgress::gated();
        run.track_command_progress("echo oi", &progress, Some(Duration::from_secs(300)), async {
            tokio::time::sleep(Duration::from_millis(20)).await;
        })
        .await;
        assert!(sink.0.events().iter().all(|e| !matches!(e.kind, ActivityKind::Command | ActivityKind::CommandEnd)));
    }

    /// Spec 123 T01: com progresso, cada heartbeat do comando leva os bytes e a última linha,
    /// redigida e cortada (mantém o fim); sem saída ainda, só os bytes (zero) e nenhuma linha.
    #[tokio::test]
    async fn command_progress_heartbeats_carry_bytes_and_redacted_last_line() {
        let (sink, scope) = redacting();
        let run = scope.with_heartbeat(Duration::from_millis(20)).begin();
        let progress = CommandProgress::new();
        run.track_command_progress("npm test", &progress, Some(Duration::from_secs(300)), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            progress.push(format!("{} chave {SECRET} passo final\n", "y".repeat(300)).as_bytes());
            tokio::time::sleep(Duration::from_millis(50)).await;
        })
        .await;
        let beats: Vec<_> = sink.0.events().into_iter().filter(|e| e.kind == ActivityKind::Heartbeat).collect();
        assert!(beats.len() >= 3, "{beats:?}");
        assert_eq!((beats[0].output_bytes, beats[0].last_line.as_deref()), (Some(0), None));
        let last = beats.last().cloned().unwrap_or_else(|| beats[0].clone());
        assert!(last.output_bytes.is_some_and(|b| b > 300));
        let line = last.last_line.unwrap_or_default();
        assert!(line.len() <= MAX_COMMAND_BYTES && line.ends_with("passo final"), "{line}");
        assert!(!SECRET.as_bytes().windows(6).any(|w| line.as_bytes().windows(6).any(|x| x == w)), "{line}");
        // O heartbeat sem progresso (comando sem captura) segue sem campos.
        assert!(sink.0.events().iter().filter(|e| e.kind != ActivityKind::Heartbeat).all(|e| e.output_bytes.is_none() && e.last_line.is_none()));
    }

    /// Spec 114 T04: a chamada longa ao modelo emite `model`, heartbeats no ritmo do escopo (nunca
    /// um por pedaço) com os bytes já recebidos, e `model_end`; o tamanho sozinho não vira texto.
    #[tokio::test]
    async fn model_call_emits_start_paced_heartbeats_with_bytes_and_end() {
        let rec = Arc::new(Recorder::default());
        let run = scope(&rec).with_heartbeat(Duration::from_millis(30)).begin();
        let out = run
            .track_model(async {
                tokio::time::sleep(Duration::from_millis(45)).await;
                for n in 1..=500u64 {
                    run.delta(StreamDelta::Received(n * 100));
                }
                tokio::time::sleep(Duration::from_millis(60)).await;
                3
            })
            .await;
        assert_eq!(out, 3);
        let ev = rec.events();
        let kinds: Vec<_> = ev.iter().map(|e| e.kind).collect();
        assert_eq!(kinds[0], ActivityKind::Start);
        assert_eq!(kinds[1], ActivityKind::Model);
        assert_eq!(*kinds.last().unwrap_or(&ActivityKind::Start), ActivityKind::ModelEnd);
        let beats: Vec<_> = ev.iter().filter(|e| e.kind == ActivityKind::ModelBeat).collect();
        assert_eq!(ActivityKind::ModelBeat.as_str(), "heartbeat");
        assert!((2..=5).contains(&beats.len()), "{kinds:?}");
        // Antes dos bytes, o heartbeat não traz tamanho; depois, o último recebido.
        assert_eq!(beats[0].received_bytes, None);
        assert_eq!(beats.last().and_then(|b| b.received_bytes), Some(50_000));
        assert!(ev.iter().all(|e| e.text.is_none()));
    }

    #[tokio::test]
    async fn dropped_model_call_still_ends() {
        let rec = Arc::new(Recorder::default());
        let run = scope(&rec).begin();
        let fut = run.track_model(std::future::pending::<()>());
        let _ = tokio::time::timeout(Duration::from_millis(10), fut).await;
        let kinds: Vec<_> = rec.events().iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![ActivityKind::Start, ActivityKind::Model, ActivityKind::ModelEnd]);
    }

    #[test]
    fn view_screenshot_label_names_the_capture() {
        assert_eq!(tool_label("view_screenshot", r#"{"name":"shots/home.png"}"#), "vendo a captura shots/home.png");
        assert_eq!(tool_label("view_screenshot", r#"{"name":"referencia/ab.png"}"#), "vendo a referência ab.png");
        assert_eq!(
            tool_label("generate_image", r#"{"path":"web/img/gato.webp","prompt":"segredo do prompt"}"#),
            "gerando imagem web/img/gato.webp"
        );
    }

    #[test]
    fn image_failed_label_shows_the_fixed_reason() {
        assert_eq!(
            tool_label("image_failed", r#"{"reason":"provider_http_400 (código: invalid_value)"}"#),
            "geração de imagem falhou (provider_http_400 (código: invalid_value))"
        );
    }

    #[test]
    fn images_refused_label_is_readable() {
        let label = tool_label("images_refused", "{}");
        assert_eq!(label, "o modelo não aceitou imagens; seguindo sem capturas");
        assert!(!label.contains("usando"));
    }

    #[test]
    fn labels() {
        assert_eq!(tool_label("read_file", r#"{"path":"src/x.rs"}"#), "lendo src/x.rs");
        assert_eq!(tool_label("list_dir", "{}"), "listando .");
        assert_eq!(tool_label("run_command", r#"{"command":"ls"}"#), "rodando `ls`");
        let long = format!(r#"{{"path":"{}"}}"#, "x".repeat(500));
        assert!(tool_label("write_file", &long).len() <= MAX_LABEL_BYTES);
    }
}
