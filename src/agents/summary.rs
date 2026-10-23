//! Compactação por resumo do histórico (spec 122 T04), a terceira camada da gestão de contexto.
//!
//! Depois da microcompactação (camada 2, o `Compactor`), se o contexto estimado ainda passa do
//! limiar, o próprio modelo do papel escreve, sem ferramentas, um retrato estruturado do estado
//! (`<state_snapshot>`), e o histórico é refeito com:
//! 1. as mensagens de sistema e o briefing original, intactos;
//! 2. o retrato, a lista dos arquivos tocados na sessão (só caminhos e tamanhos) e o fecho;
//! 3. o par chamada/resultado pendente (a última resposta com ferramentas e os resultados que o
//!    modelo ainda não viu), se houver;
//! 4. para o Desenvolvedor, o lembrete da tarefa.
//!
//! Também dispara de forma reativa quando o provedor recusa o pedido por tamanho do contexto
//! ([`is_context_overflow`]): resume e repete uma vez. Em qualquer falha (retrato vazio, cortado
//! pelo teto, histórico novo maior que o antigo, erro do provedor) o histórico original fica; depois
//! de [`SUMMARY_MAX_FAILURES`] falhas seguidas, o resumo é desligado até o fim da sessão.
//!
//! Valores iniciais declarados, pontos de calibração (eventos `summary`/`summary_failed` do
//! `context.jsonl` e as chamadas marcadas `summary` no `usage.jsonl`):
//! - limiar: o menor entre [`SUMMARY_WINDOW_PERCENT`]% da janela do modelo (catálogo de preços,
//!   ou [`DEFAULT_CONTEXT_WINDOW`]) e [`SUMMARY_AT_TOKENS`];
//! - teto do retrato: [`SUMMARY_MAX_BYTES`];
//! - depois de um resumo, o próximo só quando o contexto crescer metade do limiar.

use std::collections::BTreeSet;
use std::path::{Component, Path};

use serde_json::Value;

use crate::agents::llm_client::{ChatMessage, Role, ToolCall};
use crate::agents::prompts::{SUMMARY_FILES_INTRO, SUMMARY_INTRO, SUMMARY_NO_FILES, SUMMARY_RESUME};
use crate::agents::LlmError;

/// Fração da janela do modelo (em %) a partir da qual o histórico é resumido.
pub const SUMMARY_WINDOW_PERCENT: usize = 85;
/// Limiar absoluto inicial (tokens), ponto de calibração: com a microcompactação limpando até 12 k,
/// o que passa de 48 k é conversa e resultados recentes que ela não alcança. As medições da spec
/// (Test 3, T02 do Developer: contexto subindo até 52 k) mostram que sessões reais chegam lá.
pub const SUMMARY_AT_TOKENS: usize = 48_000;
/// Janela declarada quando o catálogo de preços não traz a do modelo (tokens).
pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;
/// Teto do retrato (bytes, ~4 k tokens); acima disso conta como cortado.
pub const SUMMARY_MAX_BYTES: usize = 16 * 1024;
/// Falhas seguidas que desligam o resumo até o fim da sessão (disjuntor).
pub const SUMMARY_MAX_FAILURES: u32 = 3;
/// Máximo de arquivos listados depois do retrato.
const FILES_MAX: usize = 60;
/// Tamanho máximo de um caminho listado.
const PATH_MAX_BYTES: usize = 300;

const SNAPSHOT_OPEN: &str = "<state_snapshot>";
const SNAPSHOT_CLOSE: &str = "</state_snapshot>";
const ANALYSIS_OPEN: &str = "<analysis>";
const ANALYSIS_CLOSE: &str = "</analysis>";

/// Política de resumo de um laço: o limiar em tokens. `None` no contexto = sem resumo (a conversa
/// do Arquiteto, que já limita os turnos).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryPolicy {
    pub threshold: usize,
}

impl SummaryPolicy {
    /// Limiar pela janela do modelo (ou a declarada): o menor entre a fração e o valor absoluto.
    pub fn for_window(window: Option<usize>) -> Self {
        let window = window.filter(|w| *w > 0).unwrap_or(DEFAULT_CONTEXT_WINDOW);
        let share = window / 100 * SUMMARY_WINDOW_PERCENT + window % 100 * SUMMARY_WINDOW_PERCENT / 100;
        Self { threshold: share.min(SUMMARY_AT_TOKENS) }
    }
}

/// O que disparou o resumo (motivo do evento `summary`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// O contexto estimado passou do limiar depois da microcompactação.
    Threshold,
    /// O provedor recusou o pedido por tamanho do contexto.
    Overflow,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Threshold => "threshold",
            Self::Overflow => "overflow",
        }
    }
}

/// Motivo de uma falha do resumo (evento `summary_failed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryFailure {
    /// Retrato vazio (ou só o rascunho descartado).
    Empty,
    /// Retrato cortado: pelo teto de bytes, pelo limite de saída do provedor ou sem o fecho.
    Truncated,
    /// O histórico novo não ficou menor que o antigo.
    Larger,
    /// A chamada de resumo falhou no provedor.
    Provider,
}

impl SummaryFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Truncated => "truncated",
            Self::Larger => "larger",
            Self::Provider => "provider",
        }
    }
}

/// Estado do resumo numa sessão: política, falhas seguidas (disjuntor), o degrau do próximo resumo
/// e os arquivos tocados (acumulados entre resumos: o histórico resumido some, a lista fica).
#[derive(Debug, Default)]
pub struct Summarizer {
    policy: Option<SummaryPolicy>,
    failures: u32,
    next_at: usize,
    touched: BTreeSet<String>,
}

impl Summarizer {
    pub fn new(policy: Option<SummaryPolicy>) -> Self {
        Self { policy, ..Self::default() }
    }

    /// Ligado e sem o disjuntor aberto.
    pub fn enabled(&self) -> bool {
        self.policy.is_some() && self.failures < SUMMARY_MAX_FAILURES
    }

    /// O contexto estimado (`tokens`) pede um resumo.
    pub fn due(&self, tokens: usize) -> bool {
        self.policy
            .filter(|_| self.enabled())
            .is_some_and(|p| tokens > p.threshold.max(self.next_at))
    }

    /// Resumo feito: zera as falhas; o próximo só depois de o contexto crescer metade do limiar.
    pub fn succeeded(&mut self, after_tokens: usize) {
        self.failures = 0;
        let step = self.policy.map_or(0, |p| p.threshold / 2);
        self.next_at = after_tokens.saturating_add(step);
    }

    /// Resumo falho: conta para o disjuntor.
    pub fn failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    /// Acrescenta os caminhos que as chamadas do histórico tocaram (qualquer argumento `path`).
    pub fn note_touched(&mut self, messages: &[ChatMessage]) {
        for call in messages.iter().flat_map(|m| m.tool_calls.iter()) {
            touched_paths(call, &mut self.touched);
        }
    }

    pub fn touched(&self) -> &BTreeSet<String> {
        &self.touched
    }
}

/// Caminhos (argumento `path`, em qualquer nível) de uma chamada, relativos e sem `..`.
fn touched_paths(call: &ToolCall, out: &mut BTreeSet<String>) {
    fn visit(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, v) in map {
                    match (key.as_str(), v) {
                        ("path", Value::String(p)) => {
                            if let Some(p) = clean_path(p) {
                                out.insert(p);
                            }
                        }
                        _ => visit(v, out),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|v| visit(v, out)),
            _ => {}
        }
    }
    if let Ok(args) = serde_json::from_str::<Value>(&call.arguments) {
        visit(&args, out);
    }
}

/// Caminho relativo normalizado (`a/b`), ou `None` se for absoluto, sair da raiz ou for vazio.
fn clean_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > PATH_MAX_BYTES {
        return None;
    }
    let mut parts = Vec::new();
    for c in Path::new(raw).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Lista dos arquivos tocados que ainda existem, com o tamanho atual em bytes.
pub fn files_section(root: &Path, touched: &BTreeSet<String>) -> String {
    let files: Vec<String> = touched
        .iter()
        .filter_map(|p| {
            let meta = std::fs::symlink_metadata(root.join(p)).ok()?;
            meta.file_type().is_file().then(|| format!("- {p} ({} bytes)", meta.len()))
        })
        .collect();
    if files.is_empty() {
        return SUMMARY_NO_FILES.to_string();
    }
    let mut lines = vec![SUMMARY_FILES_INTRO.to_string()];
    lines.extend(files.iter().take(FILES_MAX).cloned());
    if files.len() > FILES_MAX {
        lines.push(format!("- ... (+{})", files.len() - FILES_MAX));
    }
    lines.join("\n")
}

/// Fim do prefixo intacto: as mensagens de sistema do início e o briefing (a primeira mensagem do
/// usuário logo depois delas).
pub fn prefix_end(messages: &[ChatMessage]) -> usize {
    let systems = messages.iter().take_while(|m| m.role == Role::System).count();
    match messages.get(systems) {
        Some(m) if m.role == Role::User => systems + 1,
        _ => systems,
    }
}

/// Início da cauda preservada: a última resposta com ferramentas e o que veio depois dela (o par
/// pendente); se a última resposta não pediu ferramentas, só as mensagens depois dela.
pub fn tail_start(messages: &[ChatMessage]) -> usize {
    match messages.iter().rposition(|m| m.role == Role::Assistant) {
        Some(i) if !messages[i].tool_calls.is_empty() => i,
        Some(i) => i + 1,
        None => messages.len(),
    }
}

/// Há o que resumir entre o prefixo e a cauda.
pub fn has_middle(messages: &[ChatMessage]) -> bool {
    tail_start(messages) > prefix_end(messages)
}

/// Resposta cortada pelo limite de saída do provedor.
pub fn is_truncated(finish_reason: Option<&str>) -> bool {
    matches!(finish_reason, Some("length" | "max_tokens" | "max_output_tokens"))
}

/// Tira os blocos de rascunho `<analysis>`; um bloco sem fecho leva o resto do texto.
fn strip_analysis(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(ANALYSIS_OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(ANALYSIS_CLOSE) {
            Some(end) => rest = &rest[start + end + ANALYSIS_CLOSE.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Há texto fora das marcas `<...>`.
fn has_content(text: &str) -> bool {
    let mut in_tag = false;
    text.chars().any(|c| match c {
        '<' => {
            in_tag = true;
            false
        }
        '>' => {
            in_tag = false;
            false
        }
        c => !in_tag && !c.is_whitespace(),
    })
}

/// O retrato da resposta do modelo: sem o rascunho, só o bloco `<state_snapshot>` quando houver.
/// Vazio, aberto sem fecho ou acima do teto contam como falha.
pub fn snapshot_text(raw: &str) -> Result<String, SummaryFailure> {
    let stripped = strip_analysis(raw);
    let snapshot = match stripped.find(SNAPSHOT_OPEN) {
        Some(start) => match stripped[start..].find(SNAPSHOT_CLOSE) {
            Some(end) => stripped[start..start + end + SNAPSHOT_CLOSE.len()].to_string(),
            None => return Err(SummaryFailure::Truncated),
        },
        None => stripped.trim().to_string(),
    };
    if !has_content(&snapshot) {
        return Err(SummaryFailure::Empty);
    }
    if snapshot.len() > SUMMARY_MAX_BYTES {
        return Err(SummaryFailure::Truncated);
    }
    Ok(snapshot)
}

/// Histórico novo: prefixo intacto, o retrato com os arquivos e o fecho, a cauda (par pendente) e,
/// com `reminder`, o lembrete da tarefa no fim.
pub fn rebuild(messages: &[ChatMessage], snapshot: &str, files: &str, reminder: Option<String>) -> Vec<ChatMessage> {
    let prefix = prefix_end(messages);
    let tail = tail_start(messages).max(prefix);
    let mut out: Vec<ChatMessage> = messages[..prefix].to_vec();
    out.push(ChatMessage::text(
        Role::User,
        format!("{SUMMARY_INTRO}\n\n{snapshot}\n\n{files}\n\n{SUMMARY_RESUME}"),
    ));
    out.extend_from_slice(&messages[tail..]);
    if let Some(text) = reminder {
        out.push(ChatMessage::text(Role::User, text));
    }
    out
}

/// Trechos (em minúsculas) das mensagens de erro por tamanho do contexto dos provedores.
const OVERFLOW_PATTERNS: [&str; 10] = [
    "context_length_exceeded",
    "context length",
    "maximum context",
    "context window",
    "prompt is too long",
    "prompt too long",
    "input is too long",
    "too many tokens",
    "exceeds the available context",
    "range of input length",
];

/// O provedor recusou o pedido por tamanho do contexto: HTTP 413, ou 400/422 com o código ou a
/// mensagem de contexto excedido, ou a falha do stream com o código `context_length_exceeded`.
pub fn is_context_overflow(err: &LlmError) -> bool {
    match err {
        LlmError::Http { status: 413, .. } => true,
        LlmError::Http { status: 400 | 422, body_snippet, code } => {
            code.is_some_and(|c| c.as_str() == OVERFLOW_PATTERNS[0]) || {
                let body = body_snippet.to_ascii_lowercase();
                OVERFLOW_PATTERNS.iter().any(|p| body.contains(p))
            }
        }
        LlmError::Parse(reason) => reason.code.is_some_and(|c| c.as_str() == OVERFLOW_PATTERNS[0]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::llm_client::{BadKind, BadReason};

    fn call(id: &str, name: &str, args: &str) -> ChatMessage {
        let mut m = ChatMessage::text(Role::Assistant, "");
        m.tool_calls = vec![ToolCall { id: id.into(), name: name.into(), arguments: args.into() }];
        m
    }

    fn result(id: &str, text: &str) -> ChatMessage {
        let mut m = ChatMessage::text(Role::Tool, text);
        m.tool_call_id = Some(id.into());
        m
    }

    #[test]
    fn threshold_is_the_smaller_of_the_window_share_and_the_absolute_value() {
        assert_eq!(SummaryPolicy::for_window(None).threshold, SUMMARY_AT_TOKENS, "padrão declarado: 85% de 128 k > 48 k");
        assert_eq!(SummaryPolicy::for_window(Some(32_000)).threshold, 27_200, "janela pequena: 85%");
        assert_eq!(SummaryPolicy::for_window(Some(1_000_000)).threshold, SUMMARY_AT_TOKENS);
        assert_eq!(SummaryPolicy::for_window(Some(0)).threshold, SUMMARY_AT_TOKENS, "janela inválida: o padrão");
    }

    #[test]
    fn breaker_opens_after_three_failures_and_success_resets() {
        let mut s = Summarizer::new(Some(SummaryPolicy { threshold: 100 }));
        assert!(!s.due(100) && s.due(101));
        s.failed();
        s.failed();
        assert!(s.due(101), "duas falhas: ainda tenta");
        s.succeeded(80);
        assert!(!s.due(130) && s.due(131), "o próximo só depois de crescer metade do limiar");
        s.succeeded(10);
        assert!(s.due(101), "nunca acima do limiar");
        for _ in 0..SUMMARY_MAX_FAILURES {
            s.failed();
        }
        assert!(!s.enabled() && !s.due(10_000), "disjuntor aberto até o fim da sessão");
        assert!(!Summarizer::new(None).due(usize::MAX), "sem política, nunca");
    }

    #[test]
    fn snapshot_drops_the_analysis_and_checks_empty_cut_and_ceiling() {
        let raw = "<analysis>rascunho longo</analysis>\nsobra\n<state_snapshot>\n<estado_atual>pronto</estado_atual>\n</state_snapshot>\nfim";
        let snap = snapshot_text(raw).expect("retrato");
        assert!(snap.starts_with(SNAPSHOT_OPEN) && snap.ends_with(SNAPSHOT_CLOSE), "{snap}");
        assert!(!snap.contains("rascunho") && !snap.contains("sobra"));
        assert_eq!(snapshot_text("só texto livre").as_deref(), Ok("só texto livre"));
        assert_eq!(snapshot_text("  "), Err(SummaryFailure::Empty));
        assert_eq!(snapshot_text("<analysis>só rascunho"), Err(SummaryFailure::Empty));
        assert_eq!(snapshot_text("<state_snapshot><a></a>\n</state_snapshot>"), Err(SummaryFailure::Empty));
        assert_eq!(snapshot_text("<state_snapshot><a>x</a>"), Err(SummaryFailure::Truncated));
        let big = format!("<state_snapshot>{}</state_snapshot>", "x".repeat(SUMMARY_MAX_BYTES));
        assert_eq!(snapshot_text(&big), Err(SummaryFailure::Truncated));
        assert!(is_truncated(Some("length")) && !is_truncated(Some("stop")) && !is_truncated(None));
    }

    #[test]
    fn rebuild_keeps_prefix_snapshot_files_pending_pair_and_reminder() {
        let history = vec![
            ChatMessage::text(Role::System, "regras"),
            ChatMessage::text(Role::System, "perfil"),
            ChatMessage::text(Role::User, "briefing"),
            call("c1", "read_file", r#"{"path":"a.rs"}"#),
            result("c1", "conteúdo antigo"),
            ChatMessage::text(Role::Assistant, "pensando"),
            call("c2", "write_file", r#"{"path":"b.rs","content":"x"}"#),
            result("c2", "ok"),
        ];
        assert_eq!((prefix_end(&history), tail_start(&history)), (3, 6));
        let new = rebuild(&history, "<state_snapshot>x</state_snapshot>", "FILES", Some("lembrete".into()));
        let roles: Vec<Role> = new.iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::System, Role::System, Role::User, Role::User, Role::Assistant, Role::Tool, Role::User]);
        assert_eq!(new[2].content, "briefing");
        assert!(new[3].content.starts_with(SUMMARY_INTRO) && new[3].content.contains("FILES") && new[3].content.ends_with(SUMMARY_RESUME));
        assert_eq!(new[4].tool_calls[0].id, "c2", "par pendente preservado");
        assert_eq!(new[5].tool_call_id.as_deref(), Some("c2"));
        assert_eq!(new[6].content, "lembrete");
        // Última resposta sem ferramentas: só o que veio depois dela fica.
        let mut done = history.clone();
        done.push(ChatMessage::text(Role::Assistant, "final"));
        done.push(ChatMessage::text(Role::User, "Antes de concluir"));
        assert_eq!(tail_start(&done), done.len() - 1);
        // Só o briefing e a cauda: nada a resumir.
        assert!(!has_middle(&history[..3]) && has_middle(&history));
    }

    #[test]
    fn touched_files_are_relative_paths_with_current_size() {
        let root = std::env::temp_dir().join(format!("nants-summary-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(root.join("src/a.rs"), "12345").expect("write");
        let mut s = Summarizer::new(None);
        s.note_touched(&[
            call("c1", "read_file", r#"{"path":"./src/a.rs"}"#),
            call("c2", "write_many", r#"{"files":[{"path":"src/gone.rs","content":"x"}]}"#),
            call("c3", "read_file", r#"{"path":"../fora.txt"}"#),
            call("c4", "read_file", r#"{"path":"/etc/passwd"}"#),
            call("c5", "list_dir", r#"{"path":"src"}"#),
        ]);
        let listed: Vec<&str> = s.touched().iter().map(String::as_str).collect();
        assert_eq!(listed, ["src", "src/a.rs", "src/gone.rs"]);
        let text = files_section(&root, s.touched());
        assert_eq!(text, format!("{SUMMARY_FILES_INTRO}\n- src/a.rs (5 bytes)"), "pasta e arquivo apagado ficam fora");
        assert_eq!(files_section(&root, &BTreeSet::new()), SUMMARY_NO_FILES);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn context_overflow_is_detected_by_status_code_and_message() {
        let http = |status: u16, body: &str| LlmError::Http { status, body_snippet: body.into(), code: None };
        assert!(is_context_overflow(&http(400, r#"{"error":{"code":"context_length_exceeded"}}"#)));
        assert!(is_context_overflow(&http(400, "This model's maximum context length is 128000 tokens")));
        assert!(is_context_overflow(&http(400, "prompt is too long: 210000 tokens > 200000 maximum")));
        assert!(is_context_overflow(&http(413, "")));
        assert!(!is_context_overflow(&http(400, "invalid tool schema")));
        assert!(!is_context_overflow(&http(429, "too many tokens per minute")), "limite de ritmo não é tamanho");
        assert!(!is_context_overflow(&http(500, "context length")));
        let parse = |code| LlmError::Parse(BadReason::with_code(BadKind::ProviderError, Some(code)));
        assert!(is_context_overflow(&parse("context_length_exceeded")));
        assert!(!is_context_overflow(&parse("server_error")));
        assert!(!is_context_overflow(&LlmError::Timeout));
    }
}
