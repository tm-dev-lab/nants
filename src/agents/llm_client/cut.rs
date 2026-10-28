//! Diagnóstico de uma resposta em streaming cortada (spec 114 T05).
//!
//! Enquanto o stream chega, [`StreamDiag`] guarda só números (tempo, bytes, eventos e os tipos de
//! evento mais frequentes), o uso de tokens quando o provedor o informa e dois trechos curtos do
//! texto gerado: o início e o fim. A resposta inteira nunca é guardada. No corte (tamanho, tempo,
//! transmissão interrompida ou formato inválido), vira um [`StreamCut`], entregue como
//! [`super::StreamDelta::Cut`] para o registro de uso gravar o diagnóstico e o custo.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::Value;

use super::{nested_tokens, BadKind, Billing, LlmError, Usage};

/// Tamanho máximo (bytes) de cada trecho gravado: o início e o fim do texto gerado.
pub const CUT_SNIPPET_BYTES: usize = 2048;
/// Folga guardada além do trecho. A redação de segredos roda antes do recorte final, então um
/// segredo que cruza a borda do trecho é redigido inteiro, não pela metade.
pub const CUT_SNIPPET_MARGIN: usize = 512;
/// Tipos de evento mais frequentes informados no diagnóstico.
pub const CUT_TOP_EVENTS: usize = 8;
/// Tipos distintos contados; os demais somam em `other` (o nome vem do provedor).
const MAX_EVENT_KINDS: usize = 64;
/// Tamanho máximo do nome de um tipo de evento.
const EVENT_NAME_MAX: usize = 64;
/// Bytes guardados de cada ponta antes da redação.
const KEEP: usize = CUT_SNIPPET_BYTES + CUT_SNIPPET_MARGIN;

/// Diagnóstico de uma resposta cortada. Os trechos ainda não passaram pela redação completa (só a
/// chave da própria conexão): quem grava usa [`StreamCut::snippets`] com a redação do Nants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCut {
    /// Motivo estável do corte: `too_large`, `timeout`, `transport`, `no_final`, `invalid_json`...
    pub reason: &'static str,
    /// Tempo desde o envio do pedido até o corte.
    pub elapsed_ms: u64,
    /// Bytes do corpo recebidos.
    pub bytes: u64,
    /// Eventos SSE recebidos.
    pub events: u64,
    /// Tipos de evento mais frequentes, do maior para o menor (até [`CUT_TOP_EVENTS`]).
    pub top_events: Vec<(String, u64)>,
    /// Caracteres de texto gerado (texto, raciocínio e argumentos de ferramenta).
    pub generated_chars: u64,
    /// Início do texto gerado (com folga para a redação).
    pub head: String,
    /// Fim do texto gerado (com folga para a redação).
    pub tail: String,
    /// O texto gerado passou do início guardado: a ponta final de `head` pode ter um segredo pela
    /// metade, e [`Self::snippets`] a descarta.
    pub head_truncated: bool,
    /// Idem para o começo de `tail`.
    pub tail_truncated: bool,
    /// Tokens informados pelo provedor no stream, quando houver.
    pub reported: Option<Usage>,
    /// Estimativa quando o provedor não informou: a entrada pelo tamanho do pedido e a saída pelo
    /// maior entre os caracteres gerados / 4 e o número de pedaços de texto recebidos.
    pub estimated: Usage,
    /// Como a chamada é cobrada (spec 116 T02): o cliente marca o plano ChatGPT.
    pub billing: Billing,
}

impl StreamCut {
    /// Uso a registrar e se é estimado. Sem tokens informados e sem nenhum evento recebido (o
    /// provedor nem começou a responder), não há custo a registrar.
    pub fn usage(&self) -> Option<(Usage, bool)> {
        match self.reported {
            Some(u) => Some((u, false)),
            None if self.events > 0 => Some((self.estimated, true)),
            None => None,
        }
    }

    /// Início e fim do texto gerado: `scrub` (a redação de segredos) roda primeiro e o resultado é
    /// recortado em até [`CUT_SNIPPET_BYTES`] cada.
    /// Na borda em que o guardado foi recortado, um segredo pode ter ficado pela metade (a redação não
    /// o reconhece): essa borda perde [`CUT_SNIPPET_MARGIN`] bytes antes do recorte final.
    pub fn snippets(&self, scrub: impl Fn(&str) -> String) -> (String, String) {
        let head = scrub(&self.head);
        let head = if self.head_truncated { head_bytes(&head, head.len().saturating_sub(CUT_SNIPPET_MARGIN)) } else { &head };
        let tail = scrub(&self.tail);
        let tail = if self.tail_truncated { tail_bytes(&tail, tail.len().saturating_sub(CUT_SNIPPET_MARGIN)) } else { &tail };
        (head_bytes(head, CUT_SNIPPET_BYTES).to_string(), tail_bytes(tail, CUT_SNIPPET_BYTES).to_string())
    }
}

/// Motivo do corte para um erro do stream, ou `None` quando não é corte: o provedor recusou ou
/// falhou por conta própria (`provider_error`, `throttled`), sem resposta a diagnosticar.
pub fn cut_reason(err: &LlmError) -> Option<&'static str> {
    match err {
        LlmError::Timeout => Some("timeout"),
        LlmError::Transport(_) => Some("transport"),
        LlmError::Parse(r) => match r.kind {
            BadKind::ProviderError | BadKind::Throttled => None,
            kind => Some(kind.as_str()),
        },
        LlmError::Http { .. }
        | LlmError::Config(_)
        | LlmError::Plan(_)
        | LlmError::Limit(_)
        | LlmError::KindNotSupported(_) => None,
    }
}

/// Acumulador do diagnóstico durante o stream. Memória limitada: contadores, até
/// [`MAX_EVENT_KINDS`] tipos e duas pontas de [`KEEP`] bytes.
pub(super) struct StreamDiag {
    started: Instant,
    prompt_estimate: u32,
    bytes: u64,
    events: u64,
    kinds: HashMap<String, u64>,
    chars: u64,
    pieces: u64,
    head: String,
    tail: String,
    head_truncated: bool,
    tail_truncated: bool,
    usage: Option<Usage>,
    /// Uso da Anthropic acumulado de `message_start` e `message_delta` (spec 132 T04).
    anthropic_usage: serde_json::Map<String, Value>,
}

impl StreamDiag {
    pub(super) fn new(prompt_estimate: u64) -> Self {
        Self {
            started: Instant::now(),
            prompt_estimate: u32::try_from(prompt_estimate).unwrap_or(u32::MAX),
            bytes: 0,
            events: 0,
            kinds: HashMap::new(),
            chars: 0,
            pieces: 0,
            head: String::new(),
            tail: String::new(),
            head_truncated: false,
            tail_truncated: false,
            usage: None,
            anthropic_usage: serde_json::Map::new(),
        }
    }

    /// Total de bytes do corpo recebidos até agora.
    pub(super) fn received(&mut self, total: usize) {
        self.bytes = u64::try_from(total).unwrap_or(u64::MAX);
    }

    /// Conta um evento SSE: o tipo, o texto gerado que ele traz e o uso, quando informado.
    pub(super) fn observe(&mut self, event: &str, data: &str) {
        self.events = self.events.saturating_add(1);
        if data.trim() == "[DONE]" {
            self.count("done");
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            self.count("invalid_json");
            return;
        };
        if let Some(kind) = v["event_type"].as_str() {
            // Gemini (spec 132 T07): `step.delta` com o texto, o resumo do raciocínio ou os argumentos
            // da chamada; o uso só vem no `interaction.completed`.
            self.count(kind);
            let delta = &v["delta"];
            for piece in [delta["text"].as_str(), delta["content"]["text"].as_str(), delta["arguments"].as_str()].into_iter().flatten() {
                self.text(piece);
            }
            if let Some(u) = super::gemini::usage(&v["interaction"]["usage"]) {
                self.usage = Some(u);
            }
            return;
        }
        if let Some(kind) = v["type"].as_str().or((!event.is_empty()).then_some(event)) {
            // Responses: `type` (ou o nome do evento SSE), `delta` e o `usage` da resposta.
            self.count(kind);
            if let Some(piece) = v["delta"].as_str() {
                self.text(piece);
            }
            if let Some(u) = v["response"].get("usage").filter(|u| u.is_object()) {
                self.usage = Some(responses_usage(u));
            }
            // Anthropic (spec 132 T04): o `delta` é um objeto com o texto, o thinking ou o JSON
            // parcial da ferramenta, e o uso vem em `message_start` e `message_delta`.
            for key in ["text", "thinking", "partial_json"] {
                if let Some(piece) = v["delta"][key].as_str() {
                    self.text(piece);
                }
            }
            if let Some(u) = super::anthropic::stream_usage(&mut self.anthropic_usage, kind, &v) {
                self.usage = Some(u);
            }
            return;
        }
        // Chat Completions: o tipo é o campo do `delta` que veio no pedaço.
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.count("usage");
            self.usage = Some(chat_usage(u));
        }
        let delta = &v["choices"][0]["delta"];
        let mut any = false;
        for key in ["content", "reasoning_content", "reasoning"] {
            if let Some(piece) = delta[key].as_str() {
                self.count(&format!("delta.{key}"));
                self.text(piece);
                any = true;
            }
        }
        for tc in delta["tool_calls"].as_array().into_iter().flatten() {
            self.count("delta.tool_calls");
            if let Some(piece) = tc["function"]["arguments"].as_str() {
                self.text(piece);
            }
            any = true;
        }
        if !any && v.get("usage").is_none_or(Value::is_null) {
            self.count("chunk");
        }
    }

    /// Fecha o diagnóstico para o erro `err`; `None` quando o erro não é corte ([`cut_reason`]).
    /// A chave da conexão (`secret`) já sai redigida dos trechos.
    pub(super) fn finish(mut self, err: &LlmError, secret: &str) -> Option<StreamCut> {
        let reason = cut_reason(err)?;
        if self.tail.len() > KEEP {
            let keep = tail_bytes(&self.tail, KEEP).len();
            let cut = self.tail.len() - keep;
            self.tail.drain(..cut);
            self.tail_truncated = true;
        }
        let mut top: Vec<(String, u64)> = self.kinds.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top.truncate(CUT_TOP_EVENTS);
        let redact = |s: String| if secret.is_empty() { s } else { s.replace(secret, "***") };
        let completion = (self.chars.div_ceil(4)).max(self.pieces);
        Some(StreamCut {
            reason,
            elapsed_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            bytes: self.bytes,
            events: self.events,
            top_events: top,
            generated_chars: self.chars,
            head: redact(self.head),
            tail: redact(self.tail),
            head_truncated: self.head_truncated,
            tail_truncated: self.tail_truncated,
            reported: self.usage,
            estimated: Usage::new(self.prompt_estimate, u32::try_from(completion).unwrap_or(u32::MAX)),
            billing: Billing::Metered,
        })
    }

    fn count(&mut self, raw: &str) {
        let name: String = raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
            .take(EVENT_NAME_MAX)
            .collect();
        let name = if name.is_empty() { "unknown".to_string() } else { name };
        let key = if self.kinds.contains_key(&name) || self.kinds.len() < MAX_EVENT_KINDS {
            name
        } else {
            "other".to_string()
        };
        let n = self.kinds.entry(key).or_insert(0);
        *n = n.saturating_add(1);
    }

    fn text(&mut self, piece: &str) {
        if piece.is_empty() {
            return;
        }
        self.pieces = self.pieces.saturating_add(1);
        self.chars = self.chars.saturating_add(u64::try_from(piece.chars().count()).unwrap_or(u64::MAX));
        if !self.head_truncated {
            let room = KEEP - self.head.len();
            let part = head_bytes(piece, room);
            self.head_truncated = part.len() < piece.len();
            self.head.push_str(part);
        }
        if piece.len() >= KEEP {
            self.tail_truncated |= piece.len() > KEEP || !self.tail.is_empty();
            self.tail.clear();
            self.tail.push_str(tail_bytes(piece, KEEP));
        } else {
            self.tail.push_str(piece);
            if self.tail.len() > 2 * KEEP {
                let keep = tail_bytes(&self.tail, KEEP).len();
                let cut = self.tail.len() - keep;
                self.tail.drain(..cut);
                self.tail_truncated = true;
            }
        }
    }
}

/// Uso no formato da Responses (`input_tokens`, `output_tokens` e detalhes).
fn responses_usage(u: &Value) -> Usage {
    let n = |k: &str| u32::try_from(u[k].as_u64().unwrap_or(0)).unwrap_or(u32::MAX);
    Usage {
        prompt: n("input_tokens"),
        completion: n("output_tokens"),
        cached: nested_tokens(u, "input_tokens_details", "cached_tokens"),
        reasoning: nested_tokens(u, "output_tokens_details", "reasoning_tokens"),
        cache_write: None,
    }
}

/// Uso no formato do Chat Completions.
fn chat_usage(u: &Value) -> Usage {
    let n = |k: &str| u32::try_from(u[k].as_u64().unwrap_or(0)).unwrap_or(u32::MAX);
    Usage {
        prompt: n("prompt_tokens"),
        completion: n("completion_tokens"),
        cached: nested_tokens(u, "prompt_tokens_details", "cached_tokens"),
        reasoning: nested_tokens(u, "completion_tokens_details", "reasoning_tokens"),
        cache_write: None,
    }
}

/// Até `max` bytes do início, sem partir caractere.
fn head_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut at = max;
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    &s[..at]
}

/// Até `max` bytes do fim, sem partir caractere.
fn tail_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut at = s.len() - max;
    while !s.is_char_boundary(at) {
        at += 1;
    }
    &s[at..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::llm_client::BadReason;

    fn resp_delta(text: &str) -> String {
        serde_json::json!({ "type": "response.output_text.delta", "delta": text }).to_string()
    }

    #[test]
    fn keeps_only_bounded_head_and_tail() {
        let mut d = StreamDiag::new(100);
        d.observe("", &resp_delta("INICIO "));
        for i in 0..5_000 {
            d.observe("", &resp_delta(&format!("parte{i} ")));
        }
        d.observe("", &resp_delta(" FIM"));
        d.received(9_999);
        let cut = d.finish(&LlmError::Parse(super::super::TOO_LARGE), "").expect("corte");
        assert_eq!(cut.reason, "too_large");
        assert!(cut.head.len() <= KEEP && cut.tail.len() <= KEEP);
        let (head, tail) = cut.snippets(str::to_string);
        assert!(head.starts_with("INICIO ") && head.len() <= CUT_SNIPPET_BYTES);
        assert!(tail.ends_with(" FIM") && tail.len() <= CUT_SNIPPET_BYTES);
        assert_eq!(cut.events, 5_002);
        assert_eq!(cut.bytes, 9_999);
        assert_eq!(cut.top_events[0], ("response.output_text.delta".to_string(), 5_002));
        let (usage, estimated) = cut.usage().expect("uso");
        assert!(estimated);
        assert_eq!(usage.prompt, 100);
        assert!(usage.completion >= 5_002, "{usage:?}");
    }

    #[test]
    fn huge_single_piece_and_multibyte_are_cut_on_char_boundary() {
        let mut d = StreamDiag::new(0);
        d.observe("", &resp_delta(&"é".repeat(10_000)));
        let cut = d.finish(&LlmError::Timeout, "").expect("corte");
        let (head, tail) = cut.snippets(str::to_string);
        assert!(head.len() <= CUT_SNIPPET_BYTES && tail.len() <= CUT_SNIPPET_BYTES);
        assert!(head.chars().all(|c| c == 'é') && tail.chars().all(|c| c == 'é'));
        assert_eq!(cut.reason, "timeout");
        assert_eq!(cut.generated_chars, 10_000);
    }

    #[test]
    fn secret_on_the_snippet_border_is_redacted_whole() {
        let secret = &["sk", "-segredo-de-teste-0123456789"].concat();
        let mut d = StreamDiag::new(0);
        // O segredo cruza o byte 2048: redigido antes do recorte, não sobra metade dele.
        d.observe("", &resp_delta(&format!("{}{secret}{}", "a".repeat(CUT_SNIPPET_BYTES - 10), "b".repeat(5_000))));
        let cut = d.finish(&LlmError::Timeout, "").expect("corte");
        let (head, _) = cut.snippets(|s| s.replace(secret, "***"));
        assert!(!head.contains("segredo-de-teste"), "{head}");
        let leak = secret.as_bytes().windows(8).any(|w| head.contains(std::str::from_utf8(w).unwrap_or("\u{0}")));
        assert!(!leak);
    }

    #[test]
    fn dense_secrets_never_leak_a_half_at_either_border() {
        let secret = &["sk", "-segredo-denso-0123456789abcdef"].concat();
        for offset in 0..40 {
            let mut d = StreamDiag::new(0);
            d.observe("", &resp_delta(&"z".repeat(offset)));
            for _ in 0..400 {
                d.observe("", &resp_delta(&format!("{secret} ")));
            }
            let cut = d.finish(&LlmError::Timeout, "").expect("corte");
            let (head, tail) = cut.snippets(|s| s.replace(secret, "***"));
            for text in [&head, &tail] {
                let leak = secret.as_bytes().windows(6).any(|w| text.contains(std::str::from_utf8(w).unwrap_or("\u{0}")));
                assert!(!leak, "offset {offset}: {text}");
            }
        }
    }

    #[test]
    fn connection_key_is_redacted_at_finish() {
        let mut d = StreamDiag::new(0);
        let key = ["sk", "-test-CHAVE-123"].concat();
        d.observe("", &resp_delta(&format!("chave {key} aqui")));
        let cut = d.finish(&LlmError::Timeout, &key).expect("corte");
        assert!(!cut.head.contains("CHAVE") && !cut.tail.contains("CHAVE"));
    }

    #[test]
    fn reported_usage_wins_and_provider_errors_are_not_cuts() {
        let mut d = StreamDiag::new(10);
        let ev = r#"{"type":"response.incomplete","response":{"usage":{"input_tokens":50,"output_tokens":7,"output_tokens_details":{"reasoning_tokens":3}}}}"#;
        d.observe("response.incomplete", ev);
        let cut = d.finish(&LlmError::Parse(BadReason::new(BadKind::Incomplete)), "").expect("corte");
        assert_eq!(cut.usage(), Some((Usage { prompt: 50, completion: 7, cached: None, reasoning: Some(3), cache_write: None }, false)));
        let d = StreamDiag::new(10);
        assert!(d.finish(&LlmError::Parse(BadReason::new(BadKind::ProviderError)), "").is_none());
        let d = StreamDiag::new(10);
        assert!(d.finish(&LlmError::Parse(BadReason::new(BadKind::Throttled)), "").is_none());
    }

    #[test]
    fn no_event_means_no_cost() {
        let d = StreamDiag::new(10);
        let cut = d.finish(&LlmError::Timeout, "").expect("corte");
        assert_eq!(cut.usage(), None);
    }

    #[test]
    fn chat_chunks_and_event_names_are_counted_and_bounded() {
        let mut d = StreamDiag::new(0);
        d.observe("", r#"{"choices":[{"delta":{"content":"oi"}}]}"#);
        d.observe("", r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":1}"}}]}}]}"#);
        d.observe("", r#"{"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#);
        d.observe("", "nao e json");
        for i in 0..100 {
            d.observe(&format!("tipo <script>{i}"), "{}");
        }
        let cut = d.finish(&LlmError::Parse(BadReason::new(BadKind::NoFinal)), "").expect("corte");
        assert_eq!(cut.reason, "no_final");
        assert_eq!(cut.reported, Some(Usage::new(5, 2)));
        assert_eq!(cut.top_events.len(), CUT_TOP_EVENTS);
        assert!(cut.top_events.iter().all(|(k, _)| !k.contains('<') && !k.contains(' ')));
        assert!(cut.top_events.iter().any(|(k, n)| k == "other" && *n > 1), "{:?}", cut.top_events);
        assert_eq!(cut.head, "oi{\"a\":1}");
    }
}
