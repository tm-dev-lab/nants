//! Cliente HTTP da API de bots do Telegram (spec 138 T06, C-09 e C-10).
//!
//! - **Dois clientes:** um para a escuta (`getUpdates`, com prazo igual à espera do long polling
//!   mais [`LISTEN_SLACK`]) e outro para o envio (prazo curto, [`SEND_TIMEOUT`]): um envio nunca
//!   espera atrás da escuta, e a escuta nunca cai pelo prazo do envio.
//! - **rustls, sem redirecionamento** (`Policy::none()`): o token vai no caminho da URL e não pode
//!   seguir para outro endereço indicado num `Location`.
//! - **O token nunca vaza (C-09):** a URL não é registrada em lugar nenhum; todo erro do reqwest
//!   passa por `without_url()` e, depois, pelo `scrub_exact` com o token, antes de virar texto.
//! - **Corpo limitado** a [`MAX_BODY_BYTES`].

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::events::SecretSet;

/// Folga do prazo da escuta além da espera do `getUpdates`.
pub const LISTEN_SLACK: Duration = Duration::from_secs(10);
/// Prazo de um envio (mensagem, sair do grupo, resposta ao clique, `getMe`).
pub const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// Prazo de conexão dos dois clientes.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Teto do corpo de uma resposta.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Teto do texto técnico de um erro de rede.
const MAX_DETAIL_CHARS: usize = 300;
/// Prazo do download de uma nota de voz e do envio de uma (T09): até 20 MB.
pub const FILE_TIMEOUT: Duration = Duration::from_secs(60);

/// Falha de uma chamada à API do Telegram. Nenhuma variante carrega a URL ou o token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TgError {
    /// 401 (ou 404, a resposta do Telegram a um token que não existe): o token não vale.
    Unauthorized,
    /// 409: outro programa escuta este bot (outro `getUpdates` ou um webhook).
    Conflict,
    /// 429: esperar os segundos indicados.
    RetryAfter(u64),
    /// Falha de rede ou prazo: texto técnico já sem URL e sem o token.
    Network(String),
    /// Outro status de erro do Telegram.
    Api(u16),
    /// Resposta fora do formato esperado.
    BadResponse,
}

impl TgError {
    /// Código curto para o registro e o status.
    pub fn code(&self) -> &'static str {
        match self {
            TgError::Unauthorized => "invalid_token",
            TgError::Conflict => "conflict",
            TgError::RetryAfter(_) => "rate_limited",
            TgError::Network(_) => "network_error",
            TgError::Api(_) => "api_error",
            TgError::BadResponse => "bad_response",
        }
    }
}

impl std::fmt::Display for TgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TgError::Network(detail) => write!(f, "network_error: {detail}"),
            TgError::RetryAfter(secs) => write!(f, "rate_limited: {secs}"),
            TgError::Api(status) => write!(f, "api_error: {status}"),
            other => f.write_str(other.code()),
        }
    }
}

/// Pessoa (ou bot) do Telegram.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub username: Option<String>,
}

/// Chat de uma mensagem (`private`, `group`, `supergroup` ou `channel`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type", default)]
    pub kind: String,
}

/// Mensagem recebida (só os campos usados).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub message_id: i64,
    /// Segundos desde a época Unix (0 numa mensagem inacessível).
    #[serde(default)]
    pub date: i64,
    pub chat: Chat,
    #[serde(default)]
    pub from: Option<User>,
    #[serde(default)]
    pub text: Option<String>,
    /// Nota de voz (T09).
    #[serde(default)]
    pub voice: Option<VoiceNote>,
}

/// Nota de voz recebida (T09): o id do arquivo, a duração (segundos) e o tamanho, quando o Telegram
/// informa. A duração e o tamanho são conferidos antes do download.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VoiceNote {
    pub file_id: String,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub file_size: Option<u64>,
}

/// Resposta do `getFile` (T09): o caminho do download e o tamanho.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileInfo {
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_path: Option<String>,
}

/// Caminho de arquivo do `getFile` aceito no download: relativo, sem `..`, só letras, dígitos e
/// `._/-`.
pub fn valid_file_path(path: &str) -> bool {
    (1..=256).contains(&path.len())
        && !path.starts_with('/')
        && !path.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
}

/// Clique num botão de uma mensagem do bot.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub data: Option<String>,
}

/// Uma atualização do `getUpdates`. O conteúdo é lido à parte: uma mensagem num formato
/// inesperado não impede o avanço do offset.
#[derive(Debug, Clone)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

impl Update {
    fn parse(value: &Value) -> Option<Self> {
        let update_id = value.get("update_id")?.as_i64()?;
        let field = |name: &str| value.get(name).cloned().filter(|v| !v.is_null());
        Some(Self {
            update_id,
            message: field("message").and_then(|v| serde_json::from_value(v).ok()),
            callback_query: field("callback_query").and_then(|v| serde_json::from_value(v).ok()),
        })
    }
}

/// Resposta da API: `{ok, result}` ou `{ok: false, error_code, description, parameters}`.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error_code: Option<u16>,
    #[serde(default)]
    parameters: Option<Parameters>,
}

#[derive(Debug, Deserialize)]
struct Parameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

/// Cliente de um bot: o endereço da API, o token e os dois clientes HTTP. Sem `Debug` derivado:
/// o token não aparece.
pub struct TelegramClient {
    base: String,
    token: String,
    listen: reqwest::Client,
    send: reqwest::Client,
    secrets: SecretSet,
}

impl std::fmt::Debug for TelegramClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TelegramClient(***)")
    }
}

fn build(timeout: Duration) -> Result<reqwest::Client, TgError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT)
        // O token vai no caminho: nunca seguir um `Location` para outro endereço.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| TgError::Network(e.without_url().to_string()))
}

/// Texto técnico curto, numa linha.
fn short(text: &str) -> String {
    text.replace(['\n', '\r'], " ").chars().take(MAX_DETAIL_CHARS).collect()
}

impl TelegramClient {
    /// `base`: o endereço da API (`AppState::telegram_api`); `poll`: a espera do `getUpdates`.
    pub fn new(base: &str, token: String, poll: Duration) -> Result<Self, TgError> {
        let secrets = SecretSet::new([token.clone()]);
        let scrub = |e: TgError| match e {
            TgError::Network(d) => TgError::Network(short(&secrets.scrub_exact(&d))),
            other => other,
        };
        let listen = build(poll.saturating_add(LISTEN_SLACK)).map_err(scrub)?;
        let send = build(SEND_TIMEOUT).map_err(scrub)?;
        Ok(Self { base: base.trim_end_matches('/').to_string(), token, listen, send, secrets })
    }

    /// Erro do reqwest sem a URL e sem o token.
    fn network(&self, e: reqwest::Error) -> TgError {
        let timeout = e.is_timeout();
        let text = e.without_url().to_string();
        let text = if timeout && !text.contains("timed out") { format!("timeout: {text}") } else { text };
        TgError::Network(short(&self.secrets.scrub_exact(&text)))
    }

    /// Chama `method` com o corpo JSON e devolve o `result`. A URL só existe aqui, em memória.
    async fn call(&self, listen: bool, method: &str, body: &Value) -> Result<Value, TgError> {
        let client = if listen { &self.listen } else { &self.send };
        let url = format!("{}/bot{}/{}", self.base, self.token, method);
        let resp = client.post(url).json(body).send().await.map_err(|e| self.network(e))?;
        self.envelope(resp).await
    }

    /// Lê a resposta da API: o `result`, ou o erro pelo status e pelo `error_code`.
    async fn envelope(&self, mut resp: reqwest::Response) -> Result<Value, TgError> {
        let status = resp.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| self.network(e))? {
            if bytes.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(TgError::BadResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let envelope: Option<Envelope> = serde_json::from_slice(&bytes).ok();
        let code = envelope.as_ref().and_then(|e| e.error_code).unwrap_or(status);
        match (status, code) {
            (401, _) | (_, 401) | (404, _) => return Err(TgError::Unauthorized),
            (409, _) | (_, 409) => return Err(TgError::Conflict),
            (429, _) | (_, 429) => {
                let after = envelope.as_ref().and_then(|e| e.parameters.as_ref()).and_then(|p| p.retry_after);
                return Err(TgError::RetryAfter(after.unwrap_or(1).max(1)));
            }
            _ => {}
        }
        if !(200..300).contains(&status) {
            return Err(TgError::Api(status));
        }
        match envelope {
            Some(e) if e.ok => Ok(e.result),
            _ => Err(TgError::BadResponse),
        }
    }

    /// `getMe`: o bot do token (o `username` monta o link de pareamento).
    pub async fn get_me(&self) -> Result<User, TgError> {
        let result = self.call(false, "getMe", &json!({})).await?;
        serde_json::from_value(result).map_err(|_| TgError::BadResponse)
    }

    /// `getUpdates` com espera de `timeout_secs` (0 = só o que já está na fila), a partir de
    /// `offset`, só mensagens e cliques.
    pub async fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>, TgError> {
        let mut body = json!({
            "timeout": timeout_secs,
            "allowed_updates": ["message", "callback_query"],
        });
        if offset > 0 {
            body["offset"] = json!(offset);
        }
        let result = self.call(true, "getUpdates", &body).await?;
        let list = result.as_array().ok_or(TgError::BadResponse)?;
        Ok(list.iter().filter_map(Update::parse).collect())
    }

    /// `sendMessage` em texto puro (sem `parse_mode`) e sem prévia de link.
    pub async fn send_message(&self, chat_id: i64, text: &str) -> Result<(), TgError> {
        self.send(chat_id, text, false, None).await.map(|_| ())
    }

    /// `sendMessage` (T07, C-12): texto puro, sem `parse_mode`, sem prévia de link e cortado no
    /// limite do Telegram ([`super::format::fit`]); `silent` manda sem som
    /// (`disable_notification`); `keyboard` são as linhas de botões inline. Devolve o `message_id`
    /// (`None` se o Telegram não o mandou).
    pub async fn send(&self, chat_id: i64, text: &str, silent: bool, keyboard: Option<Value>) -> Result<Option<i64>, TgError> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": super::format::fit(text, super::format::MAX_MESSAGE_CHARS),
            "link_preview_options": {"is_disabled": true},
        });
        if silent {
            body["disable_notification"] = json!(true);
        }
        if let Some(rows) = keyboard {
            body["reply_markup"] = json!({ "inline_keyboard": rows });
        }
        let result = self.call(false, "sendMessage", &body).await?;
        Ok(result.get("message_id").and_then(Value::as_i64))
    }

    /// `editMessageText` em texto puro: sem `reply_markup`, a mensagem perde os botões.
    pub async fn edit_text(&self, chat_id: i64, message_id: i64, text: &str) -> Result<(), TgError> {
        let body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": super::format::fit(text, super::format::MAX_MESSAGE_CHARS),
            "link_preview_options": {"is_disabled": true},
        });
        self.call(false, "editMessageText", &body).await.map(|_| ())
    }

    /// `editMessageReplyMarkup` sem teclado: tira os botões e mantém o texto.
    pub async fn clear_buttons(&self, chat_id: i64, message_id: i64) -> Result<(), TgError> {
        let body = json!({ "chat_id": chat_id, "message_id": message_id });
        self.call(false, "editMessageReplyMarkup", &body).await.map(|_| ())
    }

    /// `setMyCommands` (T08): a lista de comandos do bot, `(nome, descrição)`, para um idioma do
    /// Telegram (`language_code`, como `pt` ou `en`) ou, sem ele, para todos.
    pub async fn set_my_commands(&self, commands: &[(&str, String)], language_code: Option<&str>) -> Result<(), TgError> {
        let list: Vec<Value> = commands.iter().map(|(name, about)| json!({ "command": name, "description": about })).collect();
        let mut body = json!({ "commands": list });
        if let Some(code) = language_code {
            body["language_code"] = json!(code);
        }
        self.call(false, "setMyCommands", &body).await.map(|_| ())
    }

    /// `getFile` (T09): o caminho do download e o tamanho do arquivo.
    pub async fn get_file(&self, file_id: &str) -> Result<FileInfo, TgError> {
        let result = self.call(false, "getFile", &json!({ "file_id": file_id })).await?;
        serde_json::from_value(result).map_err(|_| TgError::BadResponse)
    }

    /// Baixa o arquivo do `getFile` (T09), até `max` bytes. O caminho do download também leva o
    /// token: a URL só existe aqui, e o erro passa pelo mesmo `without_url` e `scrub_exact`.
    pub async fn download(&self, file_path: &str, max: u64) -> Result<Vec<u8>, TgError> {
        if !valid_file_path(file_path) {
            return Err(TgError::BadResponse);
        }
        let url = format!("{}/file/bot{}/{}", self.base, self.token, file_path);
        let mut resp = self.send.get(url).timeout(FILE_TIMEOUT).send().await.map_err(|e| self.network(e))?;
        let status = resp.status().as_u16();
        match status {
            401 => return Err(TgError::Unauthorized),
            429 => return Err(TgError::RetryAfter(1)),
            200..=299 => {}
            _ => return Err(TgError::Api(status)),
        }
        if resp.content_length().is_some_and(|n| n > max) {
            return Err(TgError::BadResponse);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| self.network(e))? {
            if (bytes.len() + chunk.len()) as u64 > max {
                return Err(TgError::BadResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// `sendVoice` (T09, C-12): a nota de voz (Opus em OGG) como arquivo, com a legenda em texto
    /// puro (sem `parse_mode`), cortada no limite de 1024; `silent` manda sem som. Devolve o
    /// `message_id`.
    pub async fn send_voice(&self, chat_id: i64, ogg: Vec<u8>, caption: &str, duration_secs: u32, silent: bool) -> Result<Option<i64>, TgError> {
        let part = reqwest::multipart::Part::bytes(ogg)
            .file_name("nanty.ogg")
            .mime_str("audio/ogg")
            .map_err(|_| TgError::BadResponse)?;
        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .text("caption", super::format::fit(caption, super::format::MAX_CAPTION_CHARS))
            .text("duration", duration_secs.to_string())
            .part("voice", part);
        if silent {
            form = form.text("disable_notification", "true");
        }
        let url = format!("{}/bot{}/sendVoice", self.base, self.token);
        let resp = self.send.post(url).timeout(FILE_TIMEOUT).multipart(form).send().await.map_err(|e| self.network(e))?;
        let result = self.envelope(resp).await?;
        Ok(result.get("message_id").and_then(Value::as_i64))
    }

    /// `leaveChat`: o bot sai de um grupo.
    pub async fn leave_chat(&self, chat_id: i64) -> Result<(), TgError> {
        self.call(false, "leaveChat", &json!({ "chat_id": chat_id })).await.map(|_| ())
    }

    /// `answerCallbackQuery`: tira a espera do botão, com um texto curto opcional.
    pub async fn answer_callback(&self, query_id: &str, text: Option<&str>) -> Result<(), TgError> {
        let mut body = json!({ "callback_query_id": query_id });
        if let Some(text) = text {
            // O Telegram aceita até 200 caracteres na resposta ao clique.
            body["text"] = json!(text.chars().take(200).collect::<String>());
        }
        self.call(false, "answerCallbackQuery", &body).await.map(|_| ())
    }
}
