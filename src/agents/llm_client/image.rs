//! Geração de imagens pela ferramenta hospedada `image_generation` da API Responses (spec 090 T01).
//!
//! O modelo do agente recebe o prompt e é obrigado a chamar a ferramenta (`tool_choice`); o provedor
//! gera a imagem e a devolve num item `image_generation_call` com `result` em base64. Só existe em
//! `wire_api = responses`. A recusa do provedor à ferramenta vira o motivo fechado
//! `image_unsupported` (não se repete); o pedido segue o ritmo (089) e as novas tentativas (088/089)
//! como qualquer outro. A OpenRouter devolve a imagem num item `openrouter:image_generation` com
//! `result` em data URL (spec 094 T01); a resposta `completed` sem imagem legível vira
//! `image_unreadable`, que também não se repete (a geração pode ter sido cobrada).

use std::fmt;
use std::time::Duration;

use serde_json::{json, Value};

use super::{item_str, BadKind, BadReason, Failed, LlmClient, LlmError, ProviderCode, Usage, WireResponsesReply};
use crate::config::WireApi;

/// Custo estimado da saída de uma imagem, em tokens, para o ritmo (corrigido depois pelo `usage`).
pub const IMAGE_OUTPUT_TOKENS: u64 = 1500;
/// Teto de caracteres do `revised_prompt` guardado.
pub const REVISED_PROMPT_MAX_CHARS: usize = 500;
/// Prazo mínimo de cada pedido de imagem: a geração costuma passar do timeout padrão do texto.
pub const IMAGE_TIMEOUT: Duration = Duration::from_secs(300);

/// Motivo da recusa do provedor (ou do protocolo) à geração de imagens.
pub const IMAGE_UNSUPPORTED: BadReason = BadReason::new(BadKind::ImageUnsupported);

/// Tamanho pedido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageSize {
    Square,
    Landscape,
    Portrait,
    #[default]
    Auto,
}

impl ImageSize {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageSize::Square => "1024x1024",
            ImageSize::Landscape => "1536x1024",
            ImageSize::Portrait => "1024x1536",
            ImageSize::Auto => "auto",
        }
    }

    /// Valor aceito (`1024x1024`, `1536x1024`, `1024x1536`, `auto`).
    pub fn parse(s: &str) -> Option<Self> {
        [ImageSize::Square, ImageSize::Landscape, ImageSize::Portrait, ImageSize::Auto]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

/// Fundo pedido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageBackground {
    Transparent,
    Opaque,
    #[default]
    Auto,
}

impl ImageBackground {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageBackground::Transparent => "transparent",
            ImageBackground::Opaque => "opaque",
            ImageBackground::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [ImageBackground::Transparent, ImageBackground::Opaque, ImageBackground::Auto]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

/// Qualidade pedida.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageQuality {
    Low,
    #[default]
    Medium,
    High,
}

impl ImageQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageQuality::Low => "low",
            ImageQuality::Medium => "medium",
            ImageQuality::High => "high",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [ImageQuality::Low, ImageQuality::Medium, ImageQuality::High]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

/// Formato do arquivo gerado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    Png,
    #[default]
    Webp,
    Jpeg,
}

impl ImageFormat {
    /// Valor de `output_format` na API.
    pub fn as_str(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Webp => "webp",
            ImageFormat::Jpeg => "jpeg",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [ImageFormat::Png, ImageFormat::Webp, ImageFormat::Jpeg]
            .into_iter()
            .find(|v| v.as_str() == s)
    }

    /// Tipo MIME.
    pub fn media_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Webp => "image/webp",
            ImageFormat::Jpeg => "image/jpeg",
        }
    }
}

/// Opções da imagem, todas de vocabulário fechado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageOptions {
    pub size: ImageSize,
    pub background: ImageBackground,
    pub quality: ImageQuality,
    pub format: ImageFormat,
}

/// Imagem gerada.
#[derive(Clone, PartialEq, Eq)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
    /// Prompt reescrito pelo provedor, truncado em [`REVISED_PROMPT_MAX_CHARS`]. Texto remoto: só
    /// dado (manifesto redigido); nunca vai para log, evento ou erro.
    pub revised_prompt: Option<String>,
    pub usage: Option<Usage>,
}

/// `Debug` manual: sem os bytes e sem o `revised_prompt` (texto remoto).
impl fmt::Debug for GeneratedImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedImage")
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .field("revised_prompt", &self.revised_prompt.as_ref().map(|_| "…"))
            .field("usage", &self.usage)
            .finish()
    }
}

/// O erro é a recusa da geração de imagens ([`IMAGE_UNSUPPORTED`])?
pub fn is_image_unsupported(err: &LlmError) -> bool {
    matches!(err, LlmError::Parse(reason) if reason.kind == BadKind::ImageUnsupported)
}

/// `param` que aponta para a própria ferramenta (e não para uma opção dela).
const TOOL_PARAMS: [&str; 3] = ["tools[0].type", "tools", "tool_choice"];
/// Expressões de falta de capacidade (a ferramenta não existe para o modelo ou a implantação).
const CAPABILITY_PHRASES: [&str; 5] = ["not supported", "unsupported", "not enabled", "not available", "does not support"];
/// Códigos/tipos de moderação ou política: o conteúdo foi barrado, a ferramenta existe.
const POLICY_CODES: [&str; 3] = ["moderation_blocked", "content_policy_violation", "content_filter"];
/// Códigos de valor inválido: só contam como recusa quando o `param` é a própria ferramenta.
const VALUE_CODES: [&str; 2] = ["unsupported_value", "invalid_value"];

/// HTTP 400 que recusa a capacidade da ferramenta `image_generation` (não suportada, não
/// habilitada ou indisponível para o modelo/implantação). Lê só `error.message`, `error.param`,
/// `error.code` e `error.type` do JSON, nunca o corpo cru. Exige a menção à ferramenta (na
/// mensagem ou no `param`) e uma expressão de capacidade; erros de opção (`tools[0].size` etc.),
/// de valor e de moderação/política ficam de fora (seguem `provider_http_400`).
fn rejects_image_generation(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let err = if v["error"].is_object() { &v["error"] } else { &v };
    let field = |key: &str| err[key].as_str().unwrap_or_default().to_ascii_lowercase();
    let (message, param, code, kind) = (field("message"), field("param"), field("code"), field("type"));
    let tool_param = TOOL_PARAMS.contains(&param.as_str());
    if param.starts_with("tools[") && !tool_param {
        return false;
    }
    if POLICY_CODES.iter().any(|c| code == *c || kind == *c) {
        return false;
    }
    if VALUE_CODES.contains(&code.as_str()) && !tool_param {
        return false;
    }
    let mentions = message.contains("image_generation") || message.contains("image generation") || tool_param;
    mentions && CAPABILITY_PHRASES.iter().any(|p| message.contains(p))
}

/// Código saneado de um erro HTTP do caminho de imagem (spec 091): `error.code` ou, na falta (ou se
/// não passar no saneamento), `error.type` do objeto `error` do JSON, por [`ProviderCode::parse`].
/// Nunca a mensagem nem outro campo; corpo sem JSON ou sem código válido = `None`.
fn http_error_code(body: &str) -> Option<ProviderCode> {
    let v = serde_json::from_str::<Value>(body).ok()?;
    let err = v.get("error").filter(|e| e.is_object())?;
    let field = |key: &str| match &err[key] {
        Value::String(s) => ProviderCode::parse(s),
        Value::Number(n) => ProviderCode::parse(&n.to_string()),
        _ => None,
    };
    field("code").or_else(|| field("type"))
}

/// Formato efetivo: fundo transparente não existe em JPEG; a combinação vira PNG antes do envio
/// (o provedor recusaria com HTTP 400).
fn effective_format(opts: &ImageOptions) -> ImageFormat {
    match (opts.background, opts.format) {
        (ImageBackground::Transparent, ImageFormat::Jpeg) => ImageFormat::Png,
        (_, format) => format,
    }
}

/// Corpo de `POST /responses` que força a ferramenta `image_generation`. Sem `reasoning` nem
/// streaming.
fn image_body(model: &str, prompt: &str, opts: &ImageOptions) -> Value {
    json!({
        "model": model,
        "input": [{ "role": "user", "content": [{ "type": "input_text", "text": prompt }] }],
        "store": false,
        "tools": [{
            "type": "image_generation",
            "size": opts.size.as_str(),
            "background": opts.background.as_str(),
            "quality": opts.quality.as_str(),
            "output_format": effective_format(opts).as_str(),
        }],
        "tool_choice": { "type": "image_generation" },
    })
}

/// Tipos de item com a imagem, na ordem de preferência: o da OpenAI/Azure e o server tool da
/// OpenRouter (spec 094 T01).
const IMAGE_ITEM_TYPES: [&str; 2] = ["image_generation_call", "openrouter:image_generation"];

/// Motivo da resposta `completed` sem imagem legível (spec 094 T01): não se repete.
pub const IMAGE_UNREADABLE: BadReason = BadReason::new(BadKind::ImageUnreadable);

/// O erro é a resposta sem imagem legível ([`IMAGE_UNREADABLE`])?
pub fn is_image_unreadable(err: &LlmError) -> bool {
    matches!(err, LlmError::Parse(reason) if reason.kind == BadKind::ImageUnreadable)
}

/// Decodifica o conteúdo de uma imagem: data URL `data:image/(png|webp|jpeg|jpg);base64,…` (formato
/// do MIME) ou, se `bare` permitir, base64 puro (sem formato). Outro data URL, URL http(s) ou base64
/// inválido = `None`. Nunca baixa nada.
fn decode_image(text: &str, bare: bool) -> Option<(Vec<u8>, Option<ImageFormat>)> {
    let Some(rest) = text.strip_prefix("data:") else {
        let looks_url = text.contains("://");
        return if bare && !looks_url { base64_decode(text).map(|b| (b, None)) } else { None };
    };
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    let format = match mime.to_ascii_lowercase().as_str() {
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::Webp,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        _ => return None,
    };
    base64_decode(data).map(|b| (b, Some(format)))
}

/// Imagem legível de um item: `result` (base64 puro ou data URL) ou, na falta dele, `imageUrl` só
/// quando for data URL.
fn item_image(item: &Value) -> Option<(Vec<u8>, Option<ImageFormat>)> {
    match item["result"].as_str() {
        Some(result) => decode_image(result, true),
        None => item["imageUrl"].as_str().and_then(|url| decode_image(url, false)),
    }
}

/// Interpreta a resposta: o primeiro item de imagem ([`IMAGE_ITEM_TYPES`], na ordem de
/// preferência) com `status` diferente de `failed`. O formato vem de `output_format`, depois do MIME
/// do data URL, depois do pedido. `completed` sem imagem legível = [`IMAGE_UNREADABLE`]. Erros com
/// motivo fechado; nunca ecoa o corpo.
fn parse_image(body: &str, requested: ImageFormat) -> Result<GeneratedImage, LlmError> {
    let wire: WireResponsesReply = serde_json::from_str(body)
        .map_err(|_| LlmError::Parse(BadReason::new(BadKind::InvalidJson)))?;
    if let Some(error) = wire.error.as_ref().filter(|e| !e.is_null()) {
        return Err(LlmError::Parse(BadReason::provider_error(error)));
    }
    match wire.status.as_deref() {
        Some("completed") => {}
        Some("incomplete") => {
            let reason = wire.incomplete_details.as_ref().map(|d| item_str(d, "reason"));
            return Err(LlmError::Parse(BadReason::with_code(BadKind::Incomplete, reason)));
        }
        status => return Err(LlmError::Parse(BadReason::with_code(BadKind::UnsupportedStatus, status))),
    }
    let (item, (bytes, mime)) = IMAGE_ITEM_TYPES
        .iter()
        .flat_map(|kind| wire.output.iter().filter(move |i| item_str(i, "type") == *kind))
        .filter(|i| item_str(i, "status") != "failed")
        .find_map(|i| item_image(i).map(|img| (i, img)))
        .ok_or(LlmError::Parse(IMAGE_UNREADABLE))?;
    let format = ImageFormat::parse(item_str(item, "output_format")).or(mime).unwrap_or(requested);
    let revised_prompt = item["revised_prompt"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(REVISED_PROMPT_MAX_CHARS).collect());
    let usage = wire.usage.map(|u| Usage::new(u.input_tokens, u.output_tokens));
    Ok(GeneratedImage { bytes, format, revised_prompt, usage })
}

/// Valor de um caractere do alfabeto base64 padrão.
fn sextet(c: u8) -> Option<u32> {
    let v = match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(v))
}

/// Decodifica base64 padrão (RFC 4648, `=` opcional no fim; espaços e quebras de linha ignorados).
/// Inválido ou vazio = `None`. O tamanho já vem limitado pelo teto do corpo da resposta.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let clean: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let data = clean.strip_suffix(b"==").or_else(|| clean.strip_suffix(b"=")).unwrap_or(&clean);
    // Padding fora do lugar ou tamanho impossível (sobra de 1 caractere).
    if data.is_empty() || data.len() % 4 == 1 || (data.len() != clean.len() && clean.len() % 4 != 0) {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 2);
    for chunk in data.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= sextet(*c)? << (18 - 6 * i);
        }
        let [_, a, b, c] = n.to_be_bytes();
        out.push(a);
        if chunk.len() > 2 {
            out.push(b);
        }
        if chunk.len() > 3 {
            out.push(c);
        }
    }
    Some(out)
}

impl LlmClient {
    /// Gera uma imagem com o modelo `model` pela ferramenta hospedada `image_generation`
    /// (`wire_api = responses`). Em `chat`, devolve de imediato [`IMAGE_UNSUPPORTED`], sem
    /// requisição. Avisos de espera/repetição são descartados (ver
    /// [`Self::generate_image_noticed`]).
    pub async fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        opts: &ImageOptions,
    ) -> Result<GeneratedImage, LlmError> {
        self.generate_image_noticed(model, prompt, opts, |_| {}).await
    }

    /// Como [`Self::generate_image`], entregando em `on_notice` os avisos fixos de ritmo, limite e
    /// nova tentativa (sem texto remoto).
    pub async fn generate_image_noticed(
        &self,
        model: &str,
        prompt: &str,
        opts: &ImageOptions,
        on_notice: impl Fn(String),
    ) -> Result<GeneratedImage, LlmError> {
        if self.wire_api != WireApi::Responses {
            return Err(LlmError::Parse(IMAGE_UNSUPPORTED));
        }
        let estimate = || {
            let text = u64::try_from(prompt.len()).unwrap_or(u64::MAX) / 4;
            text.saturating_add(IMAGE_OUTPUT_TOKENS)
        };
        self.with_retry(
            || {
                self.paced_call(model, estimate, &on_notice, self.image_once(model, prompt, opts), |img: &GeneratedImage| {
                    img.usage
                })
            },
            || false,
            &on_notice,
        )
        .await
    }

    /// Uma tentativa: `POST /responses` com a ferramenta forçada. A recusa (HTTP 400 que
    /// [`rejects_image_generation`] reconhece) vira [`IMAGE_UNSUPPORTED`].
    async fn image_once(&self, model: &str, prompt: &str, opts: &ImageOptions) -> Result<GeneratedImage, Failed> {
        let builder = self
            .http
            .post(format!("{}/responses", self.base_url))
            .timeout(self.timeout.max(IMAGE_TIMEOUT))
            .json(&image_body(model, prompt, opts));
        let (status, body, after) = self.send_capped_with_headers(builder, Some(model)).await?;
        if status.is_success() {
            return Ok(parse_image(&body, effective_format(opts))?);
        }
        if status.as_u16() == 400 && rejects_image_generation(&body) {
            return Err(LlmError::Parse(IMAGE_UNSUPPORTED).into());
        }
        // Só o status e o código saneado (spec 091): o corpo do erro não sai daqui (nenhum texto
        // remoto no erro da imagem).
        let err = LlmError::Http { status: status.as_u16(), body_snippet: String::new(), code: http_error_code(&body) };
        Err(Failed { err, retry_after: after })
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
