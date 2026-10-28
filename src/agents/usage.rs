//! Registro de uso (tokens) das chamadas ao modelo, por agente/tarefa/spec.
//!
//! Um [`UsageBackend`] envolve um [`ChatBackend`] e entrega cada `Usage` reportado a um
//! [`UsageSink`] injetável; a persistência fica fora dos agentes.

use std::future::Future;
use std::pin::Pin;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::chat::ChatBackend;
use super::llm_client::{Billing, ChatMessage, ChatRequest, ChatResponse, StreamCut, StreamDelta, Usage};
use super::LlmError;

/// Uso de uma chamada ao modelo, com o contexto do agente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEvent {
    pub agent: String,
    pub model: String,
    pub spec: Option<String>,
    pub task: Option<String>,
    pub usage: Usage,
    /// Chamadas de busca na web da resposta (cobradas por chamada).
    pub web_search_calls: u32,
    /// Imagens geradas pela chamada (spec 090).
    pub images: u32,
    /// Qualidade das imagens geradas (`low|medium|high`, spec 092 T03); `None` sem imagem.
    pub image_quality: Option<String>,
    /// Tokens estimados pelo Nants, não informados pelo provedor (spec 114 T05: resposta cortada).
    pub estimated: bool,
    /// Motivo do corte da resposta (`too_large`, `timeout`...), quando a chamada foi cortada.
    pub cut: Option<String>,
    /// Como a chamada é cobrada (spec 116 T02): pelo plano ChatGPT, tokens sem US$.
    pub billing: Billing,
    /// Tamanho do contexto enviado e o passo da sessão (spec 122 T01); `None` fora de uma sessão
    /// (geração de imagem).
    pub context: Option<ContextStats>,
}

/// Medida do contexto de uma chamada (spec 122 T01): só números e o id da sessão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextStats {
    /// Bytes do histórico enviado ([`context_bytes`]).
    pub bytes: u64,
    /// Mensagens do histórico enviado.
    pub messages: u32,
    /// Passo da sessão (0-based: a ordem da chamada dentro da sessão).
    pub step: u32,
    /// Id estável da sessão ([`session_id`]).
    pub session: String,
    /// Chamada de resumo do histórico (spec 122 T04), marcada pela sessão antes da chamada.
    pub summary: bool,
}

/// Bytes de um histórico: conteúdos e argumentos das chamadas de ferramenta (a medida do
/// `Compactor` e do `usage.jsonl`, spec 122 T01). Imagens ficam fora.
pub fn context_bytes(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| m.content.len() + m.tool_calls.iter().map(|c| c.name.len() + c.arguments.len()).sum::<usize>())
        .sum()
}

/// Id estável de uma sessão (spec 122 T01): papel, tarefa (ou `-`) e a tentativa tirada da época
/// (`ask_epoch` do runner: recuperações nos 32 bits altos, `attempts + 1` nos baixos). Ex.:
/// `developer:T02:a1`, `reviewer:T03:a2r1`, `architect:-`.
pub fn session_id(role: &str, task: Option<&str>, epoch: Option<u64>) -> String {
    let task = task.filter(|t| !t.is_empty()).unwrap_or("-");
    match epoch {
        Some(e) => {
            let attempt = e & u64::from(u32::MAX);
            let recovery = e >> 32;
            if recovery > 0 {
                format!("{role}:{task}:a{attempt}r{recovery}")
            } else {
                format!("{role}:{task}:a{attempt}")
            }
        }
        None => format!("{role}:{task}"),
    }
}

/// Teto da `prompt_cache_key` (spec 122 T05): acima dele, a parte legível vira hash.
pub const CACHE_KEY_MAX: usize = 64;

/// `prompt_cache_key` de um histórico (spec 122 T05):
/// `nants-<projeto>-<sessão>-<abertura>`, com o projeto e a abertura em hash (sem caminho nem dado
/// pessoal). Ex.: `nants-3f9a1c2b4d5e-developer:T05:a1-9c0d1e2f`.
///
/// A sessão (`papel:tarefa:tentativa`, [`session_id`]) pode juntar vários históricos: o
/// Desenvolvedor recomeça o laço a cada ciclo de teste falho, com um briefing novo. Por isso a
/// chave leva o hash da abertura do histórico: as mensagens `system` iniciais e a primeira mensagem
/// seguinte (o briefing). Essa abertura não muda dentro do histórico (a microcompactação, o corte
/// de [`trim_history`](crate::agents::llm_agents) e o resumo a preservam), então a chave é estável
/// nele e muda quando um histórico novo começa. Só o texto entra no hash (as imagens não).
pub fn cache_key(project: &std::path::Path, session: &str, messages: &[ChatMessage]) -> String {
    use sha2::{Digest, Sha256};
    let project = short_hash(Sha256::digest(project.to_string_lossy().as_bytes()).as_slice(), 12);
    let systems = messages.iter().take_while(|m| m.role == super::llm_client::Role::System).count();
    let opening = (systems + 1).min(messages.len());
    let mut hasher = Sha256::new();
    for m in &messages[..opening] {
        hasher.update(m.content.as_bytes());
        hasher.update([0u8]);
    }
    let opening = short_hash(hasher.finalize().as_slice(), 8);
    let key = format!("nants-{project}-{session}-{opening}");
    if key.len() <= CACHE_KEY_MAX {
        return key;
    }
    let session = short_hash(Sha256::digest(session.as_bytes()).as_slice(), 12);
    format!("nants-{project}-{session}-{opening}")
}

/// Os primeiros `chars` dígitos hexadecimais de um hash.
fn short_hash(bytes: &[u8], chars: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out.truncate(chars);
    out
}

/// Sessão de um agente (spec 122 T01): o id estável e o contador de chamadas, que dá o passo de
/// cada chamada e de cada evento de contexto.
#[derive(Debug)]
pub struct Session {
    id: String,
    calls: AtomicU32,
    /// A próxima chamada é o resumo do histórico (spec 122 T04): vai marcada ao `usage.jsonl`.
    summary_next: AtomicBool,
}

impl Session {
    pub fn new(id: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { id: id.into(), calls: AtomicU32::new(0), summary_next: AtomicBool::new(false) })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Passo da próxima chamada, sem avançar: o de um evento de contexto antes da chamada.
    pub fn step(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    /// Reserva o passo de uma chamada que começa.
    fn begin(&self) -> u32 {
        self.calls.fetch_add(1, Ordering::SeqCst)
    }

    /// Marca (ou desmarca) a próxima chamada como resumo do histórico (spec 122 T04).
    pub fn mark_summary(&self, on: bool) {
        self.summary_next.store(on, Ordering::SeqCst);
    }

    /// Consome a marca de resumo da chamada que começa.
    fn take_summary(&self) -> bool {
        self.summary_next.swap(false, Ordering::SeqCst)
    }
}

/// Tipo de um evento de contexto (spec 122): microcompactação, saída guardada em arquivo,
/// releitura deduplicada e o resumo do histórico (feito ou falho).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextKind {
    Compact,
    Spill,
    Dedup,
    Summary,
    SummaryFailed,
}

impl ContextKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Spill => "spill",
            Self::Dedup => "dedup",
            Self::Summary => "summary",
            Self::SummaryFailed => "summary_failed",
        }
    }
}

/// Um evento de contexto (spec 122 T01) para o `context.jsonl`: só números e nomes de
/// ferramentas, nunca conteúdo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEvent {
    pub kind: ContextKind,
    pub agent: String,
    pub spec: Option<String>,
    pub task: Option<String>,
    pub session: String,
    pub step: u32,
    pub before_bytes: u64,
    pub after_bytes: u64,
    /// Itens afetados (ex.: saídas de ferramenta compactadas).
    pub items: u32,
    /// Itens afetados por ferramenta (só nomes).
    pub tools: BTreeMap<String, u32>,
    /// Tokens de entrada e de saída da chamada de resumo (spec 122 T04); `None` nos demais.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Motivo de vocabulário fechado (`[a-z_]`): o gatilho do resumo ou o motivo da falha.
    pub reason: Option<&'static str>,
}

/// O que uma operação sobre o contexto mudou, antes de virar [`ContextEvent`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextChange {
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub items: u32,
    pub tools: BTreeMap<String, u32>,
    /// Tokens da chamada de resumo (spec 122 T04).
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Gatilho do resumo ou motivo da falha (vocabulário fechado).
    pub reason: Option<&'static str>,
}

/// Diagnóstico de uma resposta cortada, com o contexto do agente (spec 114 T05).
#[derive(Debug, Clone)]
pub struct CutEvent {
    pub agent: String,
    pub model: String,
    pub spec: Option<String>,
    pub task: Option<String>,
    pub cut: Arc<StreamCut>,
}

/// Destino dos registros de uso. Nunca deve falhar para o chamador.
pub trait UsageSink: Send + Sync {
    fn record(&self, event: UsageEvent);
    /// Grava o diagnóstico de uma resposta cortada. Padrão: nada (o custo vai por [`Self::record`]).
    fn record_cut(&self, _event: CutEvent) {}
    /// Grava um evento de contexto (spec 122 T01). Padrão: nada.
    fn record_context(&self, _event: ContextEvent) {}
}

/// Linha do log do servidor para uma resposta cortada: papel, modelo e só números. Os trechos do
/// texto ficam no registro do projeto, nunca no log.
pub fn cut_log_line(agent: &str, model: &str, cut: &StreamCut) -> String {
    let safe = |t: &str| -> String {
        t.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-')).take(80).collect()
    };
    let tokens = match cut.usage() {
        Some((u, true)) => format!("saida_estimada={}", u.completion),
        Some((u, false)) => format!("saida={} raciocinio={}", u.completion, u.reasoning.unwrap_or(0)),
        None => "saida=0".to_string(),
    };
    let top = cut.top_events.first().map(|(k, n)| format!("{k}:{n}")).unwrap_or_default();
    format!(
        "nants: resposta cortada papel={} modelo={} motivo={} tempo_ms={} bytes={} eventos={} {tokens} evento_mais_comum={top}",
        safe(agent),
        safe(model),
        cut.reason,
        cut.elapsed_ms,
        cut.bytes,
        cut.events,
    )
}

/// Orçamento da spec, consultado antes de cada chamada de agente.
pub trait BudgetGuard: Send + Sync {
    /// `Some(motivo)` quando o limite da spec foi atingido, com o motivo no idioma `lang`.
    fn exceeded(&self, spec: &str, lang: crate::i18n::Lang) -> Option<String>;
}

/// Contexto de registro de um agente.
#[derive(Clone)]
pub struct UsageScope {
    pub sink: Arc<dyn UsageSink>,
    pub agent: String,
    pub spec: Option<String>,
    pub task: Option<String>,
    /// Sessão do agente (spec 122 T01): id e passo de cada chamada e evento de contexto.
    pub session: Arc<Session>,
}

impl UsageScope {
    /// Escopo com a sessão padrão (`papel:tarefa`, sem tentativa).
    pub fn new(sink: Arc<dyn UsageSink>, agent: &str, spec: Option<String>, task: Option<String>) -> Self {
        let session = Session::new(session_id(agent, task.as_deref(), None));
        Self { sink, agent: agent.to_string(), spec, task, session }
    }

    /// Troca a sessão por uma nova com o id dado.
    pub fn with_session(mut self, id: impl Into<String>) -> Self {
        self.session = Session::new(id);
        self
    }

    /// Grava um evento de contexto no passo corrente da sessão.
    pub fn record_context(&self, kind: ContextKind, change: ContextChange) {
        self.sink.record_context(ContextEvent {
            kind,
            agent: self.agent.clone(),
            spec: self.spec.clone(),
            task: self.task.clone(),
            session: self.session.id().to_string(),
            step: self.session.step(),
            before_bytes: change.before_bytes,
            after_bytes: change.after_bytes,
            items: change.items,
            tools: change.tools,
            input_tokens: change.input_tokens,
            output_tokens: change.output_tokens,
            reason: change.reason,
        });
    }

    /// Reserva o passo de uma chamada e mede o contexto enviado.
    fn begin_call(&self, req: &ChatRequest) -> ContextStats {
        ContextStats {
            bytes: u64::try_from(context_bytes(&req.messages)).unwrap_or(u64::MAX),
            messages: u32::try_from(req.messages.len()).unwrap_or(u32::MAX),
            step: self.session.begin(),
            session: self.session.id().to_string(),
            summary: self.session.take_summary(),
        }
    }
}

/// Decorador de [`ChatBackend`]: repassa as chamadas e registra o `Usage` das respostas.
/// Sem escopo, é transparente.
pub struct UsageBackend<'a> {
    inner: &'a dyn ChatBackend,
    scope: Option<UsageScope>,
}

impl<'a> UsageBackend<'a> {
    pub fn new(inner: &'a dyn ChatBackend, scope: Option<UsageScope>) -> Self {
        Self { inner, scope }
    }

    /// Passo e medida do contexto da chamada que começa; `None` sem escopo.
    fn begin(&self, req: &ChatRequest) -> Option<ContextStats> {
        self.scope.as_ref().map(|s| s.begin_call(req))
    }

    fn note(&self, model: &str, resp: &ChatResponse, context: Option<ContextStats>) {
        let usage = resp.usage.or_else(|| (resp.web_search_calls > 0).then(|| Usage::new(0, 0)));
        if let (Some(scope), Some(usage)) = (&self.scope, usage) {
            scope.sink.record(UsageEvent {
                agent: scope.agent.clone(),
                model: model.to_string(),
                spec: scope.spec.clone(),
                task: scope.task.clone(),
                usage,
                web_search_calls: resp.web_search_calls,
                images: 0,
                image_quality: None,
                estimated: false,
                cut: None,
                billing: resp.billing,
                context,
            });
        }
    }

    /// Resposta cortada (spec 114 T05): a linha no log, o diagnóstico e o custo conhecido ou
    /// estimado, mesmo sem a resposta final.
    fn note_cut(&self, model: &str, cut: &Arc<StreamCut>, context: Option<&ContextStats>) {
        let agent = self.scope.as_ref().map_or("?", |s| s.agent.as_str());
        eprintln!("{}", cut_log_line(agent, model, cut));
        let Some(scope) = &self.scope else {
            return;
        };
        scope.sink.record_cut(CutEvent {
            agent: scope.agent.clone(),
            model: model.to_string(),
            spec: scope.spec.clone(),
            task: scope.task.clone(),
            cut: Arc::clone(cut),
        });
        if let Some((usage, estimated)) = cut.usage() {
            scope.sink.record(UsageEvent {
                agent: scope.agent.clone(),
                model: model.to_string(),
                spec: scope.spec.clone(),
                task: scope.task.clone(),
                usage,
                web_search_calls: 0,
                images: 0,
                image_quality: None,
                estimated,
                cut: Some(cut.reason.to_string()),
                billing: cut.billing,
                context: context.cloned(),
            });
        }
    }
}

impl ChatBackend for UsageBackend<'_> {
    fn complete<'a>(
        &'a self,
        req: &'a ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let context = self.begin(req);
            let resp = self.inner.complete(req).await?;
            self.note(&req.model, &resp, context);
            Ok(resp)
        })
    }

    fn complete_stream<'a>(
        &'a self,
        req: &'a ChatRequest,
        on_delta: &'a (dyn Fn(StreamDelta) + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let context = self.begin(req);
            let wrapped = |d: StreamDelta| {
                if let StreamDelta::Cut(cut) = &d {
                    self.note_cut(&req.model, cut, context.as_ref());
                }
                on_delta(d);
            };
            let resp = self.inner.complete_stream(req, &wrapped).await?;
            self.note(&req.model, &resp, context);
            Ok(resp)
        })
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    /// Sink que guarda os eventos em memória: uso em `.0`, eventos de contexto em `.1`.
    #[derive(Default)]
    pub struct MemSink(pub Mutex<Vec<UsageEvent>>, pub Mutex<Vec<ContextEvent>>);

    impl UsageSink for MemSink {
        fn record(&self, event: UsageEvent) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
        }

        fn record_context(&self, event: ContextEvent) {
            self.1.lock().unwrap_or_else(|e| e.into_inner()).push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MemSink;
    use super::*;
    use crate::agents::llm_client::{ChatMessage, Role};

    struct Fake(Option<Usage>);

    impl ChatBackend for Fake {
        fn complete<'a>(
            &'a self,
            _req: &'a ChatRequest,
        ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
            let usage = self.0;
            Box::pin(async move {
                Ok(ChatResponse {
                    message: ChatMessage::text(Role::Assistant, "ok"),
                    finish_reason: None,
                    usage,
                    fallback_used: false,
                    web_search_calls: 0,
                    citations: Vec::new(),
                    web_search_rejected: false,
                    billing: Default::default(),
                })
            })
        }
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: "m1".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            effort: None,
            response_json: false,
            web_search: false,
            cache_key: None,
            web_search_max_uses: None,
        }
    }

    fn scope(sink: &Arc<MemSink>) -> UsageScope {
        UsageScope::new(sink.clone(), "developer", Some("s".into()), Some("T01".into()))
    }

    #[tokio::test]
    async fn records_complete_and_stream() {
        let sink = Arc::new(MemSink::default());
        let fake = Fake(Some(Usage { prompt: 3, completion: 4, cached: Some(1), reasoning: Some(2), cache_write: None }));
        let b = UsageBackend::new(&fake, Some(scope(&sink)));
        b.complete(&req()).await.expect("ok");
        b.complete_stream(&req(), &|_| {}).await.expect("ok");
        let ev = sink.0.lock().expect("lock");
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].model, "m1");
        assert_eq!(ev[0].task.as_deref(), Some("T01"));
        assert_eq!(ev[0].usage.cached, Some(1));
    }

    /// Spec 116 T02: a cobrança da resposta (plano ChatGPT) chega ao registro.
    #[tokio::test]
    async fn billing_of_the_response_reaches_the_record() {
        struct Plan;
        impl ChatBackend for Plan {
            fn complete<'a>(
                &'a self,
                req: &'a ChatRequest,
            ) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
                Box::pin(async move {
                    let mut resp = Fake(Some(Usage::new(5, 6))).complete(req).await?;
                    resp.billing = Billing::ChatgptPlan;
                    Ok(resp)
                })
            }
        }
        let sink = Arc::new(MemSink::default());
        UsageBackend::new(&Plan, Some(scope(&sink))).complete(&req()).await.expect("ok");
        UsageBackend::new(&Fake(Some(Usage::new(1, 1))), Some(scope(&sink))).complete(&req()).await.expect("ok");
        let ev = sink.0.lock().expect("lock");
        assert_eq!(ev[0].billing, Billing::ChatgptPlan);
        assert_eq!(ev[1].billing, Billing::Metered);
    }

    /// Spec 122 T05: a chave leva o projeto em hash, a sessão e o hash da abertura do histórico:
    /// estável enquanto o histórico cresce, outra com outra sessão, outro briefing ou outro projeto.
    #[test]
    fn cache_key_identifies_project_session_and_history() {
        let project = std::path::Path::new("/home/fulano/projetos/loja");
        let opening = vec![
            ChatMessage::text(Role::System, "regras"),
            ChatMessage::text(Role::System, "perfil"),
            ChatMessage::text(Role::User, "briefing"),
        ];
        let key = cache_key(project, "developer:T05:a1", &opening);
        assert!(key.starts_with("nants-") && key.contains("-developer:T05:a1-"), "{key}");
        assert!(!key.contains("fulano") && !key.contains("loja"), "sem caminho nem nome: {key}");
        assert!(key.len() <= CACHE_KEY_MAX);
        let mut grown = opening.clone();
        grown.push(ChatMessage::text(Role::Assistant, "lendo"));
        grown.push(ChatMessage::text(Role::User, "lembrete"));
        assert_eq!(cache_key(project, "developer:T05:a1", &grown), key, "estável no histórico");
        assert_ne!(cache_key(project, "developer:T05:a2", &opening), key);
        let mut other = opening.clone();
        other[2] = ChatMessage::text(Role::User, "briefing com o teste que falhou");
        assert_ne!(cache_key(project, "developer:T05:a1", &other), key, "outro histórico");
        assert_ne!(cache_key(std::path::Path::new("/outro"), "developer:T05:a1", &opening), key);
        // Sessão longa: a parte legível vira hash, sem passar do teto.
        let long = format!("developer:{}:a1", "T".repeat(80));
        let capped = cache_key(project, &long, &opening);
        assert!(capped.len() <= CACHE_KEY_MAX && capped.starts_with("nants-"), "{capped}");
        assert_eq!(cache_key(project, &long, &opening), capped);
        // Histórico vazio não quebra.
        assert!(cache_key(project, "architect:-", &[]).starts_with("nants-"));
    }

    /// Spec 122 T01: cada chamada leva o tamanho do contexto, as mensagens, o passo e a sessão.
    #[tokio::test]
    async fn records_context_size_step_and_session() {
        let sink = Arc::new(MemSink::default());
        let fake = Fake(Some(Usage::new(1, 1)));
        let scope = scope(&sink).with_session(session_id("developer", Some("T01"), Some((1 << 32) | 2)));
        let b = UsageBackend::new(&fake, Some(scope.clone()));
        let mut r = req();
        r.messages = vec![ChatMessage::text(Role::System, "abc"), ChatMessage::text(Role::User, "de")];
        b.complete(&r).await.expect("ok");
        scope.record_context(ContextKind::Compact, ContextChange { before_bytes: 9, after_bytes: 4, items: 1, ..Default::default() });
        b.complete_stream(&r, &|_| {}).await.expect("ok");
        let ev = sink.0.lock().expect("lock");
        let c0 = ev[0].context.clone().expect("contexto");
        assert_eq!(c0, ContextStats { bytes: 5, messages: 2, step: 0, session: "developer:T01:a2r1".into(), summary: false });
        assert_eq!(ev[1].context.as_ref().map(|c| c.step), Some(1));
        let ctx = sink.1.lock().expect("lock");
        assert_eq!((ctx[0].step, ctx[0].before_bytes, ctx[0].after_bytes), (1, 9, 4), "o evento leva o passo da próxima chamada");
        assert_eq!(ctx[0].session, "developer:T01:a2r1");
        assert_eq!(session_id("architect", None, None), "architect:-");
        assert_eq!(session_id("reviewer", Some("T03"), Some(1)), "reviewer:T03:a1");
    }

    /// Spec 122 T04: a marca de resumo vale só para a chamada seguinte da sessão.
    #[tokio::test]
    async fn summary_mark_goes_to_the_next_call_only() {
        let sink = Arc::new(MemSink::default());
        let fake = Fake(Some(Usage::new(1, 1)));
        let scope = scope(&sink);
        let b = UsageBackend::new(&fake, Some(scope.clone()));
        scope.session.mark_summary(true);
        b.complete(&req()).await.expect("ok");
        b.complete(&req()).await.expect("ok");
        scope.session.mark_summary(true);
        scope.session.mark_summary(false);
        b.complete_stream(&req(), &|_| {}).await.expect("ok");
        let ev = sink.0.lock().expect("lock");
        let marks: Vec<bool> = ev.iter().map(|e| e.context.as_ref().is_some_and(|c| c.summary)).collect();
        assert_eq!(marks, [true, false, false]);
    }

    #[tokio::test]
    async fn skips_without_usage_or_scope() {
        let sink = Arc::new(MemSink::default());
        let none = Fake(None);
        UsageBackend::new(&none, Some(scope(&sink))).complete(&req()).await.expect("ok");
        let some = Fake(Some(Usage::new(1, 1)));
        UsageBackend::new(&some, None).complete(&req()).await.expect("ok");
        assert!(sink.0.lock().expect("lock").is_empty());
    }
}
