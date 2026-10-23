//! Motivo fechado de uma resposta ruim do provedor (spec 088 T01).
//!
//! O motivo nunca carrega texto remoto livre, corpo, URL ou chave: só um tipo do vocabulário e,
//! quando houver, um código curto do provedor saneado para `^[a-z0-9_.-]{1,40}$`.

use std::fmt;

use serde_json::Value;

use crate::i18n::{self, Lang};

/// Prefixo das chaves do dicionário com o rótulo do motivo (`provider.kind.<kind>`), o mesmo da web.
pub const LABEL_PREFIX: &str = "provider.kind.";
/// Tamanho máximo do código do provedor.
pub const PROVIDER_CODE_MAX: usize = 40;
/// Prefixos de chave/token conhecidos: código assim é omitido.
const SECRET_PREFIXES: [&str; 5] = ["sk-", "sk_", "key-", "ghp_", "xai-"];
/// Trecho alfanumérico contínuo, com letras e dígitos misturados, a partir do qual o código tem
/// cara de segredo e é omitido.
const SECRET_RUN_MIN: usize = 20;
/// Códigos do provedor que indicam limite de ritmo ou de capacidade (spec 089, decisão 1). Lista
/// genérica, sem nada por provedor; `insufficient_quota` (crédito acabou) fica de fora de propósito.
pub const THROTTLE_CODES: [&str; 11] = [
    "no_capacity",
    "rate_limit_exceeded",
    "rate_limited",
    "too_many_requests",
    "overloaded",
    "server_overloaded",
    "capacity_exceeded",
    "engine_overloaded",
    "model_overloaded",
    "tokens_per_minute_exceeded",
    "requests_per_minute_exceeded",
];

/// Tipo da resposta ruim. Vocabulário fechado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadKind {
    /// Corpo que não é o JSON esperado.
    InvalidJson,
    /// `error` no corpo ou `response.failed`/`error` no stream.
    ProviderError,
    /// `status: incomplete` por motivo que não seja `max_output_tokens`.
    Incomplete,
    /// `status` diferente de `completed`/`incomplete`.
    UnsupportedStatus,
    /// Sem texto nem chamada de ferramenta.
    Empty,
    /// Stream que terminou sem evento final.
    NoFinal,
    /// Resposta acima do teto de bytes.
    TooLarge,
    /// Laço de ferramentas sem resposta final (não é falha do provedor).
    StepsExhausted,
    /// Provedor no limite (código da lista [`THROTTLE_CODES`], spec 089).
    Throttled,
    /// O provedor (ou o protocolo da conexão) não gera imagens (spec 090). Não se repete.
    ImageUnsupported,
    /// Resposta `completed` da geração de imagem sem imagem legível (spec 094). Não se repete: a
    /// geração pode ter sido cobrada.
    ImageUnreadable,
}

impl BadKind {
    /// Nome estável do motivo (`invalid_json`, `provider_error`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            BadKind::InvalidJson => "invalid_json",
            BadKind::ProviderError => "provider_error",
            BadKind::Incomplete => "incomplete",
            BadKind::UnsupportedStatus => "unsupported_status",
            BadKind::Empty => "empty",
            BadKind::NoFinal => "no_final",
            BadKind::TooLarge => "too_large",
            BadKind::StepsExhausted => "steps_exhausted",
            BadKind::Throttled => "throttled",
            BadKind::ImageUnsupported => "image_unsupported",
            BadKind::ImageUnreadable => "image_unreadable",
        }
    }

    /// Todos os tipos, na ordem da declaração.
    pub const ALL: [BadKind; 11] = [
        BadKind::InvalidJson,
        BadKind::ProviderError,
        BadKind::Incomplete,
        BadKind::UnsupportedStatus,
        BadKind::Empty,
        BadKind::NoFinal,
        BadKind::TooLarge,
        BadKind::StepsExhausted,
        BadKind::Throttled,
        BadKind::ImageUnsupported,
        BadKind::ImageUnreadable,
    ];

    /// Rótulo curto no idioma `lang`, do dicionário (`provider.kind.<kind>`, spec 119).
    pub fn label(self, lang: Lang) -> String {
        i18n::text(lang, &format!("{LABEL_PREFIX}{}", self.as_str()))
    }

    /// Tipo pelo rótulo em qualquer idioma (spec 119): lê de volta o motivo de um texto de reserva
    /// ou de um aviso já montado, para mandar o código à tela.
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| [Lang::PtBr, Lang::En].into_iter().any(|lang| k.label(lang) == label))
    }

    /// Só estes motivos aceitam código do provedor.
    fn takes_code(self) -> bool {
        matches!(self, BadKind::ProviderError | BadKind::Incomplete | BadKind::UnsupportedStatus | BadKind::Throttled)
    }
}

/// Código curto do provedor, já saneado. Guardado em linha para manter `Copy`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ProviderCode {
    len: u8,
    buf: [u8; PROVIDER_CODE_MAX],
}

impl ProviderCode {
    /// Aceita o código só se, em minúsculas, casar com `^[a-z0-9_.-]{1,40}$` e não tiver cara de
    /// chave ([`looks_secret`]).
    pub fn parse(raw: &str) -> Option<Self> {
        let lower = raw.to_ascii_lowercase();
        let ok = !lower.is_empty()
            && lower.len() <= PROVIDER_CODE_MAX
            && lower
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
            && !looks_secret(&lower);
        if !ok {
            return None;
        }
        let mut buf = [0u8; PROVIDER_CODE_MAX];
        buf[..lower.len()].copy_from_slice(lower.as_bytes());
        Some(Self { len: u8::try_from(lower.len()).ok()?, buf })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or_default()
    }

    /// Código da lista de limite do provedor ([`THROTTLE_CODES`]).
    pub fn is_throttle(&self) -> bool {
        THROTTLE_CODES.contains(&self.as_str())
    }
}

/// Código (já em minúsculas) com cara de chave/token: prefixo conhecido ou trecho alfanumérico
/// contínuo de [`SECRET_RUN_MIN`]+ caracteres com letras e dígitos misturados. Não há no projeto
/// detector por padrão reutilizável (o `SecretSet` só redige segredos conhecidos).
fn looks_secret(code: &str) -> bool {
    SECRET_PREFIXES.iter().any(|p| code.starts_with(p))
        || code.split(['_', '.', '-']).any(|run| {
            run.len() >= SECRET_RUN_MIN
                && run.bytes().any(|b| b.is_ascii_digit())
                && run.bytes().any(|b| b.is_ascii_alphabetic())
        })
}

impl fmt::Debug for ProviderCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// Motivo de uma resposta ruim: tipo + código saneado opcional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadReason {
    pub kind: BadKind,
    pub code: Option<ProviderCode>,
}

impl BadReason {
    /// Motivo sem código.
    pub const fn new(kind: BadKind) -> Self {
        Self { kind, code: None }
    }

    /// Motivo com o código do provedor, se o tipo aceita código e o texto passa no saneamento.
    /// Erro do provedor ou resposta incompleta com código da lista de limite viram `throttled`.
    pub fn with_code(kind: BadKind, raw: Option<&str>) -> Self {
        let code = raw.filter(|_| kind.takes_code()).and_then(ProviderCode::parse);
        Self { kind, code }.classified()
    }

    /// Promove a `throttled` o erro do provedor ou a resposta incompleta cujo código está em
    /// [`THROTTLE_CODES`] (spec 089); os demais ficam como estão.
    fn classified(self) -> Self {
        let throttle = matches!(self.kind, BadKind::ProviderError | BadKind::Incomplete)
            && self.code.is_some_and(|c| c.is_throttle());
        if throttle {
            Self { kind: BadKind::Throttled, ..self }
        } else {
            self
        }
    }

    /// Provedor no limite (spec 089): segue o cronograma longo de espera.
    pub fn is_throttled(&self) -> bool {
        self.kind == BadKind::Throttled
    }

    /// Erro do provedor com o código de um objeto `error` (`code` ou, na falta, `type`). Aceita
    /// texto ou número; `error` só texto não vira código (seria texto livre). Quando o objeto é o
    /// próprio evento SSE, o `type` é o tipo do evento (`error`, `response.failed`) e não serve de
    /// código: vale o `error` aninhado, se houver, senão fica sem código.
    pub fn provider_error(error: &Value) -> Self {
        let field = |obj: &Value, key: &str| match &obj[key] {
            Value::String(s) if key == "type" && is_event_type(s) => None,
            Value::String(s) => ProviderCode::parse(s),
            Value::Number(n) => ProviderCode::parse(&n.to_string()),
            _ => None,
        };
        let nested = error.get("error").filter(|e| e.is_object());
        let code = field(error, "code")
            .or_else(|| nested.and_then(|e| field(e, "code").or_else(|| field(e, "type"))))
            .or_else(|| field(error, "type"));
        Self { kind: BadKind::ProviderError, code }.classified()
    }

    /// Detalhe curto no idioma `lang`, ex.: `incompleta: content_filter` / `incomplete:
    /// content_filter` (spec 119: o rótulo vem do dicionário).
    pub fn detail_in(&self, lang: Lang) -> String {
        match &self.code {
            Some(code) => format!("{}: {}", self.kind.label(lang), code.as_str()),
            None => self.kind.label(lang),
        }
    }

    /// Detalhe do texto de reserva e do log técnico, em pt-BR (o de sempre, que
    /// [`Self::parse_detail`] lê de volta). A tela não o mostra: ela monta o motivo pelo `kind` e
    /// pelo `provider_code`, no idioma dela (spec 102 e 119).
    pub fn detail(&self) -> String {
        self.detail_in(Lang::PtBr)
    }

    /// Lê um detalhe (`<rótulo>` ou `<rótulo>: <código>`, em qualquer idioma) de volta para o
    /// motivo neutro. Outro texto devolve `None`.
    pub fn parse_detail(detail: &str) -> Option<Self> {
        if let Some(kind) = BadKind::from_label(detail) {
            return Some(Self::new(kind));
        }
        let (label, code) = detail.rsplit_once(": ")?;
        Some(Self { kind: BadKind::from_label(label)?, code: Some(ProviderCode::parse(code)?) })
    }

    /// Motivo que vale repetir: lado do provedor, possivelmente passageiro (`throttled` também, mas
    /// com o cronograma próprio de limite). `too_large`,
    /// `unsupported_status`, `steps_exhausted`, `image_unsupported` e `image_unreadable` (spec 094:
    /// nunca repetir uma geração possivelmente cobrada) não se repetem; `max_output_tokens` nem chega aqui
    /// (vira resposta truncada).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            BadKind::ProviderError
                | BadKind::Incomplete
                | BadKind::Empty
                | BadKind::NoFinal
                | BadKind::InvalidJson
                | BadKind::Throttled
        )
    }
}

/// Tipo de evento SSE (não é código de erro do provedor).
fn is_event_type(kind: &str) -> bool {
    kind == "error" || kind.starts_with("response.")
}

impl fmt::Display for BadReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn code_is_sanitized_or_omitted() {
        let ok = |raw: &str| ProviderCode::parse(raw).map(|c| c.as_str().to_string());
        assert_eq!(ok("content_filter").as_deref(), Some("content_filter"));
        assert_eq!(ok("Server_Error").as_deref(), Some("server_error"));
        assert_eq!(ok("rate.limit-1").as_deref(), Some("rate.limit-1"));
        assert_eq!(ok(&"a".repeat(40)).map(|s| s.len()), Some(40));
        for bad in ["", "tem espaço", "sk-x/y", "https://x", "ação", "a:b", &"a".repeat(41)] {
            assert_eq!(ok(bad), None, "{bad}");
        }
    }

    #[test]
    fn details_follow_the_vocabulary() {
        let d = |kind, code: Option<&str>| BadReason::with_code(kind, code).detail();
        assert_eq!(d(BadKind::Incomplete, Some("content_filter")), "incompleta: content_filter");
        assert_eq!(d(BadKind::ProviderError, Some("server_error")), "erro do provedor: server_error");
        assert_eq!(d(BadKind::Empty, None), "sem texto nem ação");
        assert_eq!(d(BadKind::NoFinal, None), "transmissão sem evento final");
        assert_eq!(d(BadKind::InvalidJson, None), "JSON inválido");
        assert_eq!(d(BadKind::UnsupportedStatus, Some("in_progress")), "status não suportado: in_progress");
        assert_eq!(d(BadKind::TooLarge, None), "resposta grande demais");
        assert_eq!(d(BadKind::StepsExhausted, None), "passos esgotados");
        // Tipos sem código ignoram o código; código inválido é omitido.
        assert_eq!(d(BadKind::Empty, Some("x")), "sem texto nem ação");
        assert_eq!(d(BadKind::Incomplete, Some("motivo com espaço")), "incompleta");
    }

    /// Spec 119: o rótulo de cada motivo sai do dicionário nos dois idiomas (nunca a própria chave),
    /// e o detalhe em qualquer idioma é lido de volta para o motivo neutro.
    #[test]
    fn labels_come_from_both_dictionaries_and_parse_back() {
        for kind in BadKind::ALL {
            let key = format!("{LABEL_PREFIX}{}", kind.as_str());
            let (pt, en) = (kind.label(Lang::PtBr), kind.label(Lang::En));
            assert_eq!(i18n::lookup(Lang::PtBr, &key), Some(pt.as_str()), "{key}");
            assert_eq!(i18n::lookup(Lang::En, &key), Some(en.as_str()), "{key}");
            assert_ne!(pt, en, "{key}");
            for lang in [Lang::PtBr, Lang::En] {
                let plain = BadReason::new(kind);
                assert_eq!(BadReason::parse_detail(&plain.detail_in(lang)), Some(plain), "{key}");
            }
        }
        let coded = BadReason::with_code(BadKind::Incomplete, Some("content_filter"));
        assert_eq!(coded.detail_in(Lang::En), "incomplete: content_filter");
        assert_eq!(BadReason::parse_detail("incomplete: content_filter"), Some(coded));
        assert_eq!(BadReason::parse_detail(&coded.detail()), Some(coded));
        assert_eq!(BadReason::new(BadKind::NoFinal).detail_in(Lang::En), "stream without a final event");
        for other in ["", "texto livre", "incompleta: com espaço", "x: y"] {
            assert_eq!(BadReason::parse_detail(other), None, "{other}");
        }
    }

    #[test]
    fn provider_error_takes_code_then_type() {
        let r = BadReason::provider_error(&json!({"code": "server_error", "type": "x", "message": "SEGREDO"}));
        assert_eq!(r.detail(), "erro do provedor: server_error");
        let r = BadReason::provider_error(&json!({"code": null, "type": "invalid_request_error"}));
        assert_eq!(r.detail(), "erro do provedor: invalid_request_error");
        let r = BadReason::provider_error(&json!({"code": 500}));
        assert_eq!(r.detail(), "erro do provedor: 500");
        let r = BadReason::provider_error(&json!("texto livre do provedor"));
        assert_eq!(r.detail(), "erro do provedor");
    }

    #[test]
    fn sse_event_type_is_not_a_code() {
        // Evento `error` da Responses com `code` nulo: o tipo do evento não vira código.
        let r = BadReason::provider_error(&json!({"type": "error", "code": null, "message": "x"}));
        assert_eq!(r.detail(), "erro do provedor");
        let r = BadReason::provider_error(&json!({"type": "response.failed", "response": {}}));
        assert_eq!(r.detail(), "erro do provedor");
        // Com `error` aninhado, vale o código (ou o tipo) de dentro.
        let r = BadReason::provider_error(&json!({"type": "error", "error": {"code": "server_error"}}));
        assert_eq!(r.detail(), "erro do provedor: server_error");
        let r = BadReason::provider_error(&json!({"type": "error", "error": {"type": "invalid_request_error"}}));
        assert_eq!(r.detail(), "erro do provedor: invalid_request_error");
        // O `code` do próprio evento continua valendo (aqui, da lista de limite: `throttled`).
        let r = BadReason::provider_error(&json!({"type": "error", "code": "rate_limit_exceeded"}));
        assert_eq!(r.detail(), "provedor no limite: rate_limit_exceeded");
    }

    #[test]
    fn secret_like_codes_are_omitted() {
        for raw in [
            "sk-abc",
            "SK_live",
            "key-123",
            "ghp_abc",
            "xai-1",
            "abc123def456ghi789jkl",
            "err_a1b2c3d4e5f6g7h8i9j0",
        ] {
            assert_eq!(ProviderCode::parse(raw), None, "{raw}");
        }
        // Trechos longos só de letras (ou só de dígitos) e códigos comuns seguem aceitos.
        for raw in ["context_length_exceeded", "insufficient_quota", "content_filter", "12345678901234567890", "server_error.v2"] {
            assert!(ProviderCode::parse(raw).is_some(), "{raw}");
        }
        let r = BadReason::with_code(BadKind::Incomplete, Some("a1b2c3d4e5f6g7h8i9j0k1"));
        assert_eq!(r.detail(), "incompleta");
    }

    #[test]
    fn throttle_codes_become_throttled_from_every_source() {
        for code in THROTTLE_CODES {
            let upper = code.to_ascii_uppercase();
            let sources = [
                BadReason::provider_error(&json!({"code": code, "message": "SEGREDO"})),
                BadReason::provider_error(&json!({"type": "error", "error": {"code": upper}})),
                BadReason::provider_error(&json!({"code": null, "type": code})),
                BadReason::with_code(BadKind::Incomplete, Some(code)),
                BadReason::with_code(BadKind::ProviderError, Some(code)),
            ];
            for r in sources {
                assert_eq!(r.kind, BadKind::Throttled, "{code}");
                assert!(r.is_throttled());
                assert_eq!(r.detail(), format!("provedor no limite: {code}"));
            }
        }
        // Crédito esgotado e demais códigos seguem a classificação da 088.
        let quota = BadReason::provider_error(&json!({"code": "insufficient_quota"}));
        assert_eq!(quota.kind, BadKind::ProviderError);
        assert_eq!(quota.detail(), "erro do provedor: insufficient_quota");
        let filter = BadReason::with_code(BadKind::Incomplete, Some("content_filter"));
        assert_eq!(filter.kind, BadKind::Incomplete);
        // Status não suportado não vira limite, nem código parecido fora da lista.
        let odd = BadReason::with_code(BadKind::UnsupportedStatus, Some("no_capacity"));
        assert_eq!(odd.kind, BadKind::UnsupportedStatus);
        let near = BadReason::provider_error(&json!({"code": "no_capacity_x"}));
        assert_eq!(near.kind, BadKind::ProviderError);
        assert_eq!(BadReason::new(BadKind::Throttled).detail(), "provedor no limite");
        assert_eq!(BadKind::Throttled.as_str(), "throttled");
    }

    #[test]
    fn retryable_kinds() {
        for k in [BadKind::ProviderError, BadKind::Incomplete, BadKind::Empty, BadKind::NoFinal, BadKind::InvalidJson, BadKind::Throttled] {
            assert!(BadReason::new(k).is_retryable(), "{k:?}");
        }
        for k in [BadKind::TooLarge, BadKind::UnsupportedStatus, BadKind::StepsExhausted] {
            assert!(!BadReason::new(k).is_retryable(), "{k:?}");
        }
    }

    /// Spec 094 T01: o motivo próprio da imagem ilegível não se repete e tem rótulo próprio; o
    /// `Empty` do caminho de texto continua repetível e com o rótulo de sempre.
    #[test]
    fn image_unreadable_is_not_retryable_and_empty_is_unchanged() {
        let unreadable = BadReason::new(BadKind::ImageUnreadable);
        assert!(!unreadable.is_retryable());
        assert!(!BadReason::new(BadKind::ImageUnsupported).is_retryable());
        assert_eq!(unreadable.detail(), "o provedor não devolveu uma imagem legível (pode ter cobrado)");
        assert_eq!(BadReason::with_code(BadKind::ImageUnreadable, Some("x")).detail(), unreadable.detail(), "sem código");
        assert_eq!(BadKind::ImageUnreadable.as_str(), "image_unreadable");
        let empty = BadReason::new(BadKind::Empty);
        assert!(empty.is_retryable());
        assert_eq!(empty.detail(), "sem texto nem ação");
    }
}
