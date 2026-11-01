//! As chamadas de voz ao Gemini (spec 138 T09; só Gemini desde a spec 139 T07, G6), pela conexão
//! do Google Gemini que a pessoa já tem:
//!
//! - **Fala:** `POST {endpoint}/v1beta/interactions` (o mesmo caminho do adaptador nativo), no
//!   formato atual da documentação: o texto em `input[].content[]` (o estilo, quando há, em
//!   `annotations` com `speech_metadata.style`), `response_format` de áudio WAV (24 kHz, mono, PCM
//!   de 16 bits) e a voz em `generation_config.speech_config[].voice`. O áudio volta em base64 num
//!   item `type: "audio"` de `steps[].content[]` (ou em `output_audio`). `store: false`.
//! - **Transcrição:** `POST {endpoint}/v1beta/models/{modelo}:generateContent` com a nota de voz em
//!   `inline_data` (`audio/ogg`), o pedido de transcrição e **`store: false`**: desde 2026-10-26 o
//!   `generateContent` guarda os pedidos por padrão nos projetos pagos novos, e o campo do pedido
//!   vale mais que a configuração do projeto. Ficou no `generateContent` (e não na Interactions) por
//!   ser o formato de entrada de áudio já validado de verdade na rc.22.
//! - **Vozes:** `GET {endpoint}/v1beta/voices`, página a página (`page_size`, `page_token`), lido de
//!   forma tolerante (o idioma, o gênero e a descrição direto na voz ou dentro de `prompted`). Só as
//!   vozes dos idiomas do Nanty ficam: a lista real tem milhares, por idioma e em ordem alfabética, e
//!   o teto antigo cortava em 2000 antes de chegar ao português (visto com a chave real, spec 139 T12).
//! - **Modelos de fala:** `GET {endpoint}/v1beta/models`, página a página, só os nomes com a parte
//!   `tts`. A lista de modelos das conexões é só de texto e os deixa de fora, então o autocomplete da
//!   fala não teria o que oferecer (visto com a chave real, spec 139 T12).
//!
//! **Nada vaza:** a chave vai só no cabeçalho, marcada como sensível; nenhum erro carrega texto do
//! provedor, do reqwest ou a URL, só um código ([`Fail`]). Sem redirecionamento. O corpo de um 400 é
//! lido só para reconhecer a chave recusada, que o Gemini devolve assim, e nunca é guardado.

use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};

use crate::config::{Connection, ConnectionKind};
use crate::server::models_native::key_rejected;

/// Prazo de uma chamada de voz (provisório).
pub const VOICE_TIMEOUT: Duration = Duration::from_secs(60);
/// Teto do corpo de uma resposta (o áudio em base64 cabe com folga).
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// Caminho da fala do Gemini, a partir do endereço da conexão.
pub const GEMINI_INTERACTIONS_PATH: &str = "/v1beta/interactions";
const GEMINI_MODELS_PATH: &str = "/v1beta/models";
/// Caminho da lista de vozes do Gemini.
pub const GEMINI_VOICES_PATH: &str = "/v1beta/voices";
/// Vozes por página pedidas ao `/voices` (o máximo da documentação).
pub const VOICES_PAGE_SIZE: u32 = 1000;
/// Páginas lidas no máximo (um teto contra uma paginação sem fim).
pub const MAX_VOICE_PAGES: usize = 10;
/// Teto do corpo de um 400 lido para achar a chave recusada (o erro do Gemini é curto).
const MAX_ERROR_BYTES: usize = 16 * 1024;
/// Modelos pedidos por página à lista do Gemini (o máximo da API).
const MODELS_PAGE_SIZE: u32 = 1000;
/// Vozes guardadas no máximo.
pub const MAX_VOICES: usize = 2000;
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

/// O código de uma resposta de erro: o Gemini recusa a chave com 400 `API_KEY_INVALID`, não com 401.
/// O corpo só escolhe o código e fica aqui.
async fn refusal(mut resp: reqwest::Response, status: u16) -> Fail {
    if status != 400 {
        return status_code(status);
    }
    let mut body = Vec::new();
    while body.len() < MAX_ERROR_BYTES {
        match resp.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            _ => break,
        }
    }
    if key_rejected(&String::from_utf8_lossy(&body)) {
        "auth"
    } else {
        status_code(status)
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
        return Err(refusal(resp, status).await);
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

/// O corpo da fala, no formato atual da Interactions API. O estilo, quando há, vai como anotação
/// `speech_metadata` do texto.
pub fn speech_body(model: &str, voice: &str, text: &str, style: Option<&str>) -> Value {
    let mut part = json!({"type": "text", "text": text});
    if let Some(style) = style.map(str::trim).filter(|s| !s.is_empty()) {
        part["annotations"] = json!([{"type": "speech_metadata", "style": style}]);
    }
    json!({
        "model": model,
        "input": [{"type": "user_input", "content": [part]}],
        "response_format": {"type": "audio", "mime_type": "audio/wav"},
        "generation_config": {"speech_config": [{"voice": voice}]},
        "store": false,
    })
}

/// Fala pelo Gemini: o WAV (ou o PCM cru) da Interactions API.
pub async fn gemini_speak(conn: &Connection, model: &str, voice: &str, text: &str, style: Option<&str>) -> Result<Audio, Fail> {
    if conn.kind != ConnectionKind::GoogleGemini {
        return Err("unsupported");
    }
    if !valid_name(model) || !valid_name(voice) {
        return Err("config");
    }
    let key = key_of(conn)?;
    let body = speech_body(model, voice, text, style);
    let url = format!("{}{GEMINI_INTERACTIONS_PATH}", endpoint(conn));
    let raw = send(client()?.post(url).header("x-goog-api-key", sensitive(&key)?).json(&body)).await?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| "bad_response")?;
    let mut data: Option<&str> = None;
    // O áudio é o item `audio` de qualquer passo (o tipo do passo muda entre as versões da API).
    for step in value["steps"].as_array().into_iter().flatten() {
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
        // Privacidade (spec 139 T07, C-17): o provedor não guarda a nota de voz nem a transcrição.
        "store": false,
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

/// O nome de um modelo de fala: tem a parte `tts`, sem o prefixo `models/`.
fn speech_model_name(raw: &str) -> Option<&str> {
    let name = raw.strip_prefix("models/").unwrap_or(raw);
    (valid_name(name) && name.split('-').any(|part| part == "tts")).then_some(name)
}

/// Os modelos de fala da conexão do Gemini, em ordem de nome.
pub async fn gemini_speech_models(conn: &Connection) -> Result<Vec<String>, Fail> {
    if conn.kind != ConnectionKind::GoogleGemini {
        return Err("unsupported");
    }
    let key = key_of(conn)?;
    let client = client()?;
    let url = format!("{}{GEMINI_MODELS_PATH}", endpoint(conn));
    let mut out: Vec<String> = Vec::new();
    let mut token = String::new();
    for _ in 0..MAX_VOICE_PAGES {
        let mut query = vec![("pageSize", MODELS_PAGE_SIZE.to_string())];
        if !token.is_empty() {
            query.push(("pageToken", token.clone()));
        }
        let raw = send(client.get(&url).query(&query).header("x-goog-api-key", sensitive(&key)?)).await?;
        let value: Value = serde_json::from_slice(&raw).map_err(|_| "bad_response")?;
        out.extend(value["models"].as_array().into_iter().flatten().filter_map(|m| m["name"].as_str()).filter_map(speech_model_name).map(str::to_string));
        let next = value["nextPageToken"].as_str().unwrap_or_default();
        if next.is_empty() || next == token {
            break;
        }
        token = next.to_string();
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// A voz fala um idioma do Nanty (pt ou en). Sem idioma informado, fica: não há como saber.
fn speaks_nanty_language(voice: &VoiceInfo) -> bool {
    voice.language_code.is_empty() || crate::server::nanty::language::from_language_code(&voice.language_code).is_some()
}

/// Uma voz da lista do Gemini (`/v1beta/voices`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VoiceInfo {
    /// O nome usado na fala (nas `prebuilt`, o nome da voz, por exemplo `Kore`).
    pub id: String,
    pub display_name: String,
    /// `prebuilt`, `prompted` ou `replicated`.
    #[serde(rename = "type")]
    pub kind: String,
    pub language_code: String,
    pub gender: String,
    pub description: String,
}

/// Texto curto de um campo da voz: o direto na voz, ou o de dentro de `prompted` (a referência e o
/// exemplo do SDK divergem); só texto simples, até 500 caracteres.
fn voice_field(voice: &Value, name: &str) -> String {
    let raw = voice[name].as_str().or_else(|| voice["prompted"][name].as_str()).unwrap_or_default();
    raw.chars().filter(|c| !c.is_control()).take(500).collect::<String>().trim().to_string()
}

/// Lê uma voz da resposta; sem um id que sirva na fala, fica de fora.
pub fn voice_of(voice: &Value) -> Option<VoiceInfo> {
    let id = voice_field(voice, "id");
    let id = if id.is_empty() { voice_field(voice, "name") } else { id };
    if !valid_name(&id) {
        return None;
    }
    let display_name = voice_field(voice, "display_name");
    Some(VoiceInfo {
        display_name: if display_name.is_empty() { id.clone() } else { display_name },
        kind: voice_field(voice, "type"),
        language_code: voice_field(voice, "language_code"),
        gender: voice_field(voice, "gender"),
        description: voice_field(voice, "description"),
        id,
    })
}

/// A lista de vozes da conexão do Gemini nos idiomas do Nanty, todas as páginas (até
/// [`MAX_VOICE_PAGES`]).
pub async fn gemini_voices(conn: &Connection) -> Result<Vec<VoiceInfo>, Fail> {
    if conn.kind != ConnectionKind::GoogleGemini {
        return Err("unsupported");
    }
    let key = key_of(conn)?;
    let client = client()?;
    let url = format!("{}{GEMINI_VOICES_PATH}", endpoint(conn));
    let mut out: Vec<VoiceInfo> = Vec::new();
    let mut token = String::new();
    for _ in 0..MAX_VOICE_PAGES {
        let mut query = vec![("page_size", VOICES_PAGE_SIZE.to_string())];
        if !token.is_empty() {
            query.push(("page_token", token.clone()));
        }
        let raw = send(client.get(&url).query(&query).header("x-goog-api-key", sensitive(&key)?)).await?;
        let value: Value = serde_json::from_slice(&raw).map_err(|_| "bad_response")?;
        for voice in value["voices"].as_array().into_iter().flatten() {
            if let Some(info) = voice_of(voice).filter(speaks_nanty_language) {
                if out.len() < MAX_VOICES && !out.iter().any(|v| v.id == info.id) {
                    out.push(info);
                }
            }
        }
        let next = value["next_page_token"].as_str().or_else(|| value["nextPageToken"].as_str()).unwrap_or_default();
        if next.is_empty() || next == token || out.len() >= MAX_VOICES {
            break;
        }
        token = next.to_string();
    }
    Ok(out)
}
