//! As chamadas de voz aos provedores (spec 138 T09), com as conexões que a pessoa já tem:
//!
//! - **Fala pelo Gemini:** `POST {endpoint}/v1beta/interactions` (o mesmo caminho do adaptador
//!   nativo) com `response_format` de áudio WAV e a voz em `generation_config.speech_config`, como o
//!   vídeo do Nants faz. O áudio volta em base64 no passo `model_output` (ou em `output_audio`).
//! - **Fala pela OpenAI:** `POST {endpoint}/audio/speech` com `response_format: "opus"`: o corpo já
//!   é a nota de voz (Opus em OGG).
//! - **Transcrição pelo Gemini:** `POST {endpoint}/v1beta/models/{modelo}:generateContent` com a
//!   nota de voz em `inline_data` (`audio/ogg`) e o pedido de transcrição.
//!
//! **Nada vaza:** a chave vai só no cabeçalho, marcada como sensível; nenhum erro carrega texto do
//! provedor, do reqwest ou a URL, só um código ([`Fail`]). Sem redirecionamento.

use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};

use crate::config::{Connection, ConnectionKind};

/// Prazo de uma chamada de voz (provisório).
pub const VOICE_TIMEOUT: Duration = Duration::from_secs(60);
/// Teto do corpo de uma resposta (o áudio em base64 cabe com folga).
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// Caminho da fala e do `generateContent` do Gemini, a partir do endereço da conexão.
pub const GEMINI_INTERACTIONS_PATH: &str = "/v1beta/interactions";
const GEMINI_MODELS_PATH: &str = "/v1beta/models";
/// Caminho da fala da OpenAI, a partir do endereço da conexão (que já termina em `/v1`).
pub const OPENAI_SPEECH_PATH: &str = "/audio/speech";
/// Tipo do áudio da nota de voz.
pub const OGG_MIME: &str = "audio/ogg";

/// Falha de uma chamada, em código (o registro guarda só isto).
pub type Fail = &'static str;

/// Áudio devolvido por uma fala, com o uso informado pelo provedor (quando há).
#[derive(Debug, Clone)]
pub struct Audio {
    pub bytes: Vec<u8>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// Texto da transcrição, com o uso.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// Modelo e voz aceitos na URL e no corpo: letras, dígitos e `._:-`, até 128.
pub fn valid_name(name: &str) -> bool {
    (1..=128).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn client() -> Result<reqwest::Client, Fail> {
    reqwest::Client::builder()
        .timeout(VOICE_TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "network")
}

/// A chave da conexão (`no_key` sem chave utilizável).
fn key_of(conn: &Connection) -> Result<String, Fail> {
    conn.resolve_api_key().ok().filter(|k| !k.trim().is_empty()).ok_or("no_key")
}

fn sensitive(raw: &str) -> Result<reqwest::header::HeaderValue, Fail> {
    let mut value = reqwest::header::HeaderValue::from_str(raw).map_err(|_| "no_key")?;
    value.set_sensitive(true);
    Ok(value)
}

/// O código de um status de erro do provedor.
pub fn status_code(status: u16) -> Fail {
    match status {
        401 | 403 => "auth",
        402 => "billing",
        404 => "model_not_found",
        400 | 422 => "bad_request",
        429 => "rate_limited",
        500..=599 => "provider_error",
        _ => "http",
    }
}

fn transport(e: &reqwest::Error) -> Fail {
    if e.is_timeout() {
        "timeout"
    } else {
        "network"
    }
}

/// Manda o pedido e lê o corpo até [`MAX_RESPONSE_BYTES`].
async fn send(req: reqwest::RequestBuilder) -> Result<Vec<u8>, Fail> {
    let mut resp = req.send().await.map_err(|e| transport(&e))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(status_code(status));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| transport(&e))? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("too_big");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn endpoint(conn: &Connection) -> String {
    conn.endpoint().trim().trim_end_matches('/').to_string()
}

fn tokens(v: &Value) -> Option<u64> {
    v.as_u64()
}

/// Fala pelo Gemini: o WAV (ou o PCM cru) da Interactions API.
pub async fn gemini_speak(conn: &Connection, model: &str, voice: &str, text: &str) -> Result<Audio, Fail> {
    if conn.kind != ConnectionKind::GoogleGemini {
        return Err("unsupported");
    }
    if !valid_name(model) || !valid_name(voice) {
        return Err("config");
    }
    let key = key_of(conn)?;
    let body = json!({
        "model": model,
        "input": [{"type": "user_input", "content": [{"type": "text", "text": text}]}],
        "response_format": {"type": "audio", "mime_type": "audio/wav"},
        "generation_config": {"speech_config": [{"voice": voice}]},
        "store": false,
    });
    let url = format!("{}{GEMINI_INTERACTIONS_PATH}", endpoint(conn));
    let raw = send(client()?.post(url).header("x-goog-api-key", sensitive(&key)?).json(&body)).await?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| "bad_response")?;
    let mut data: Option<&str> = None;
    for step in value["steps"].as_array().into_iter().flatten() {
        if step["type"] != "model_output" {
            continue;
        }
        for part in step["content"].as_array().into_iter().flatten() {
            if part["type"] == "audio" {
                if let Some(d) = part["data"].as_str() {
                    data = Some(d);
                }
            }
        }
    }
    let data = data.or_else(|| value["output_audio"]["data"].as_str()).ok_or("empty")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|_| "bad_response")?;
    if bytes.is_empty() {
        return Err("empty");
    }
    let usage = &value["usage"];
    let output = match (tokens(&usage["total_output_tokens"]), tokens(&usage["total_thought_tokens"])) {
        (Some(o), t) => Some(o.saturating_add(t.unwrap_or(0))),
        (None, t) => t,
    };
    Ok(Audio { bytes, input_tokens: tokens(&usage["total_input_tokens"]), output_tokens: output })
}

/// Fala pela OpenAI: o corpo já é Opus em OGG.
pub async fn openai_speak(conn: &Connection, model: &str, voice: &str, text: &str) -> Result<Audio, Fail> {
    if conn.kind != ConnectionKind::ApiKey {
        return Err("unsupported");
    }
    if !valid_name(model) || !valid_name(voice) {
        return Err("config");
    }
    let key = key_of(conn)?;
    let body = json!({"model": model, "input": text, "voice": voice, "response_format": "opus"});
    let url = format!("{}{OPENAI_SPEECH_PATH}", endpoint(conn));
    let bytes = send(client()?.post(url).header(reqwest::header::AUTHORIZATION, sensitive(&format!("Bearer {key}"))?).json(&body))
        .await?;
    if bytes.is_empty() {
        return Err("empty");
    }
    if !bytes.starts_with(b"OggS") {
        return Err("format");
    }
    Ok(Audio { bytes, input_tokens: None, output_tokens: None })
}

/// Transcreve a nota de voz (`audio/ogg`) pelo Gemini.
pub async fn gemini_transcribe(conn: &Connection, model: &str, prompt: &str, audio: &[u8]) -> Result<Transcript, Fail> {
    if conn.kind != ConnectionKind::GoogleGemini {
        return Err("unsupported");
    }
    if !valid_name(model) {
        return Err("config");
    }
    let key = key_of(conn)?;
    let data = base64::engine::general_purpose::STANDARD.encode(audio);
    let body = json!({
        "contents": [{"role": "user", "parts": [
            {"text": prompt},
            {"inline_data": {"mime_type": OGG_MIME, "data": data}},
        ]}],
    });
    let url = format!("{}{GEMINI_MODELS_PATH}/{model}:generateContent", endpoint(conn));
    let raw = send(client()?.post(url).header("x-goog-api-key", sensitive(&key)?).json(&body)).await?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| "bad_response")?;
    let text: String = value["candidates"][0]["content"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["thought"] != true)
        .filter_map(|p| p["text"].as_str())
        .collect();
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("empty");
    }
    let meta = &value["usageMetadata"];
    let output = match (tokens(&meta["candidatesTokenCount"]), tokens(&meta["thoughtsTokenCount"])) {
        (Some(o), t) => Some(o.saturating_add(t.unwrap_or(0))),
        (None, t) => t,
    };
    Ok(Transcript { text, input_tokens: tokens(&meta["promptTokenCount"]), output_tokens: output })
}

/// Duração de um OGG Opus em segundos (arredondada para cima): o grânulo da última página menos
/// o `pre_skip` do `OpusHead`, a 48 kHz. `None` fora do formato.
pub fn ogg_duration_secs(bytes: &[u8]) -> Option<u32> {
    let head = bytes.windows(8).position(|w| w == b"OpusHead")?;
    let pre_skip = u64::from(u16::from_le_bytes([*bytes.get(head + 10)?, *bytes.get(head + 11)?]));
    let last = bytes.windows(4).rposition(|w| w == b"OggS")?;
    let granule = bytes.get(last + 6..last + 14)?;
    let granule = u64::from_le_bytes(granule.try_into().ok()?);
    let samples = granule.saturating_sub(pre_skip);
    u32::try_from(samples.div_ceil(48_000)).ok()
}
