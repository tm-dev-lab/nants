//! API do canal do Telegram (spec 138 T06), para a aba Ajustes (T11):
//!
//! - `GET /api/nanty/telegram`: a saúde, o bot, a máscara do token, se há dono pareado e a janela de
//!   pareamento (com o código e o link enquanto aberta). Troca o código vencido, como o
//!   `GET /api/devices/code`;
//! - `POST /api/nanty/telegram/token`: `{token}` grava o token (literal `<id>:<segredo>` ou
//!   `env:NOME`; vazio remove). Token novo limpa o chat, o dono e o offset. Responde a mesma forma
//!   do `GET`, só com a máscara;
//! - `POST /api/nanty/telegram/pair`: abre a janela com um código novo e devolve o link
//!   `t.me/<bot>?start=<código>`;
//! - `POST /api/nanty/telegram/unpair`: desparear (limpa o chat, o dono e o offset);
//! - `POST /api/nanty/telegram/test`: manda uma mensagem de teste ao dono.
//!
//! As rotas que mudam algo são POST e passam pelas proteções de toda rota da API: a guarda de
//! Host/Origin e, em modo rede, o dispositivo autorizado (`devices::require_device`).

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::client::{TelegramClient, TgError};
use super::pairing::PairState;
use super::{start_link, valid_token, Health};
use crate::config::NantyTelegram;
use crate::i18n::tr;
use crate::server::config_api::{mask_api_key, valid_env_name};
use crate::server::error::ApiError;
use crate::server::nanty::{nanty_lang, InstanceStatus};
use crate::server::state::AppState;

/// Teto do token recebido.
const MAX_TOKEN_CHARS: usize = 200;

#[derive(Debug, Serialize)]
struct PairingView {
    /// `closed`, `open` ou `locked` (5 erros: só um código novo reabre).
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seconds_to_next: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in: Option<u64>,
}

#[derive(Debug, Serialize)]
struct TelegramView {
    health: Health,
    /// `username` do bot, depois da primeira resposta do Telegram.
    #[serde(skip_serializing_if = "Option::is_none")]
    bot_username: Option<String>,
    bot_token_masked: String,
    configured: bool,
    paired: bool,
    pairing: PairingView,
}

async fn view(state: &AppState) -> TelegramView {
    let tg = state.config().read().await.nanty.telegram.clone();
    let nanty = state.nanty();
    let link = nanty.telegram();
    let bot = link.bot();
    let health = if nanty.status() == InstanceStatus::Active { link.health() } else { Health::Off };
    let pairing = match link.pair_state(nanty.now().epoch_ms) {
        PairState::Closed => PairingView { state: "closed", code: None, link: None, seconds_to_next: None, expires_in: None },
        PairState::Locked => PairingView { state: "locked", code: None, link: None, seconds_to_next: None, expires_in: None },
        PairState::Open { code, seconds_to_next, expires_in } => PairingView {
            state: "open",
            link: bot.as_deref().map(|b| start_link(b, &code)),
            code: Some(code),
            seconds_to_next: Some(seconds_to_next),
            expires_in: Some(expires_in),
        },
    };
    TelegramView {
        health,
        bot_username: bot,
        bot_token_masked: if tg.bot_token.is_empty() { String::new() } else { mask_api_key(&tg.bot_token) },
        configured: !tg.bot_token.trim().is_empty(),
        paired: tg.is_paired(),
        pairing,
    }
}

async fn view_json(state: &AppState) -> Json<Value> {
    Json(serde_json::to_value(view(state).await).unwrap_or_default())
}

/// `GET /api/nanty/telegram`.
pub async fn get_telegram(State(state): State<AppState>) -> Json<Value> {
    view_json(&state).await
}

/// Corpo de `POST /api/nanty/telegram/token`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenInput {
    token: String,
}

fn coded(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::coded(status, code, Map::new())
}

/// Token aceito: vazio (remove), `env:NOME` ou a forma do Telegram.
fn validate_token(token: &str) -> Result<(), ApiError> {
    let ok = token.is_empty()
        || token.strip_prefix("env:").is_some_and(valid_env_name)
        || (token.chars().count() <= MAX_TOKEN_CHARS && valid_token(token));
    if ok {
        Ok(())
    } else {
        Err(coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_telegram_token_invalid"))
    }
}

/// Muda a parte do Telegram na configuração e grava.
async fn save(state: &AppState, f: impl FnOnce(&mut NantyTelegram)) -> Result<(), ApiError> {
    let mut guard = state.config().write().await;
    let mut next = guard.clone();
    f(&mut next.nanty.telegram);
    if next.nanty.telegram == guard.nanty.telegram {
        return Ok(());
    }
    crate::server::config_api::persist(state, &next).await?;
    *guard = next;
    Ok(())
}

/// Limpa o chat, o dono e o offset.
fn clear_pairing(t: &mut NantyTelegram) {
    t.chat_id = None;
    t.owner_id = None;
    t.offset = 0;
}

/// `POST /api/nanty/telegram/token`.
pub async fn post_token(State(state): State<AppState>, body: Bytes) -> Result<Json<Value>, ApiError> {
    let input: TokenInput = crate::server::config_api::parse_body(&body)?;
    let token = input.token.trim().to_string();
    validate_token(&token)?;
    let changed = state.config().read().await.nanty.telegram.bot_token != token;
    if changed {
        save(&state, |t| {
            t.bot_token = token;
            clear_pairing(t);
        })
        .await?;
        state.nanty().telegram().reset();
        state.nanty().changed(crate::server::nanty::Part::Status);
    } else if !token.is_empty() && state.nanty().telegram().health() == Health::InvalidToken {
        // O mesmo token salvo de novo depois de um 401 (o bot foi reativado, por exemplo): a escuta
        // tenta outra vez.
        state.nanty().telegram().reset();
        state.nanty().changed(crate::server::nanty::Part::Status);
    }
    Ok(view_json(&state).await)
}

/// `POST /api/nanty/telegram/pair`.
pub async fn post_pair(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let tg = state.config().read().await.nanty.telegram.clone();
    if tg.resolve_token().is_none() {
        return Err(coded(StatusCode::CONFLICT, "nanty_telegram_not_configured"));
    }
    if tg.is_paired() {
        return Err(coded(StatusCode::CONFLICT, "nanty_telegram_already_paired"));
    }
    let nanty = state.nanty();
    if nanty.telegram().bot().is_none() {
        return Err(coded(StatusCode::CONFLICT, "nanty_telegram_not_connected"));
    }
    nanty
        .telegram()
        .open_pairing(nanty.now().epoch_ms)
        .ok_or_else(|| coded(StatusCode::INTERNAL_SERVER_ERROR, "nanty_telegram_code_failed"))?;
    nanty.changed(crate::server::nanty::Part::Status);
    Ok(view_json(&state).await)
}

/// `POST /api/nanty/telegram/unpair`.
pub async fn post_unpair(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    save(&state, clear_pairing).await?;
    state.nanty().telegram().unpaired();
    state.nanty().changed(crate::server::nanty::Part::Status);
    Ok(view_json(&state).await)
}

/// Falha do envio de teste: o token recusado tem código próprio; o resto, o genérico.
fn send_error(e: TgError) -> ApiError {
    match e {
        TgError::Unauthorized => coded(StatusCode::BAD_GATEWAY, "nanty_telegram_token_rejected"),
        _ => coded(StatusCode::BAD_GATEWAY, "nanty_telegram_send_failed"),
    }
}

/// `POST /api/nanty/telegram/test`: uma mensagem de teste ao dono, pelo cliente de envio.
pub async fn post_test(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let cfg = state.config().read().await.nanty.clone();
    let Some(token) = cfg.telegram.resolve_token().filter(|t| valid_token(t)) else {
        return Err(coded(StatusCode::CONFLICT, "nanty_telegram_not_configured"));
    };
    let Some(chat_id) = cfg.telegram.chat_id.filter(|_| cfg.telegram.owner_id.is_some()) else {
        return Err(coded(StatusCode::CONFLICT, "nanty_telegram_not_paired"));
    };
    let client = TelegramClient::new(state.telegram_api(), token, Duration::ZERO).map_err(send_error)?;
    let text = tr(nanty_lang(&cfg), "nanty.telegram.test", &[]);
    client.send_message(chat_id, &text).await.map_err(send_error)?;
    Ok(view_json(&state).await)
}
