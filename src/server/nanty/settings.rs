//! Gravação dos Ajustes e das Regras do Nanty (spec 138 T11), pela janela:
//!
//! - `PUT /api/nanty/config`: só os campos enviados mudam (os ausentes ficam como estão). Os
//!   campos são os da aba Ajustes: modelo e esforço, orçamento do dia, horários, projetos
//!   excluídos, reserva por papel, voz e a espera do Telegram com a aba aberta. O token, o chat e o
//!   dono do Telegram **nunca** passam por aqui (`deny_unknown_fields`): só pelas rotas do canal
//!   ([`super::telegram::api`]). O idioma também não: ele segue o da interface (C-08,
//!   `server::lang`);
//! - `PUT /api/nanty/actions/:id`: `{level}` com `off`, `ask` ou `auto`, por id do catálogo (C-02).
//!   Só as ações que o Nanty executa mudam de nível; a faixa "só a pessoa" fica travada.
//!
//! Nada muda se o pedido não muda a configuração; o arquivo só guarda o que difere do padrão
//! (`config::nanty`). As duas respondem a configuração inteira, como o `GET`. São rotas que mudam
//! algo: passam pela guarda de Host/Origin e, em modo rede, pelo dispositivo autorizado.

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::actions::{self, ActionKind};
use super::voice;
use super::api::{config_json, parse_json};
use super::Part;
use crate::config::{parse_hhmm, ActionLevel, Config, ConnectionKind, DigestEmpty, Effort, NantyConfig, NantyRoute, VoiceRoute};
use crate::server::config_api::{persist, spawn_pricing_autofill, validate_text_field};
use crate::server::error::{params, ApiError};
use crate::server::events::validate_project_id;
use crate::server::state::AppState;

/// Teto do orçamento do dia em US$ (um valor maior é quase certamente um erro de digitação).
pub const MAX_BUDGET_USD: f64 = 1000.0;
/// Teto do orçamento do dia em tokens.
pub const MAX_BUDGET_TOKENS: u64 = 100_000_000;
/// Faixa da nota de voz mais longa aceita, em segundos.
pub const VOICE_INPUT_SECS: std::ops::RangeInclusive<u64> = 5..=1800;
/// Teto da espera do Telegram com a aba aberta, em segundos.
pub const MAX_VISIBLE_HOLD_SECS: u64 = 3600;
/// Passos da voz, no máximo (fora o texto, que é sempre o último).
pub const MAX_VOICE_STEPS: usize = 5;
/// Projetos excluídos, no máximo.
pub const MAX_EXCLUDED: usize = 500;
/// Papéis com reserva.
const ROLES: [&str; 3] = ["architect", "developer", "reviewer"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteInput {
    connection_id: String,
    model: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VoiceRouteInput {
    connection_id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    voice: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VoiceInput {
    enabled: Option<bool>,
    order: Option<Vec<VoiceRouteInput>>,
    transcribe: Option<RouteInput>,
    max_input_secs: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TelegramInput {
    hold_while_visible: Option<bool>,
    visible_hold_secs: Option<u64>,
}

/// Corpo de `PUT /api/nanty/config`: todo campo é opcional.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigInput {
    connection_id: Option<String>,
    model: Option<String>,
    effort: Option<Effort>,
    daily_budget_usd: Option<f64>,
    daily_budget_tokens: Option<u64>,
    quiet_start: Option<String>,
    quiet_end: Option<String>,
    digest_time: Option<String>,
    digest_empty: Option<DigestEmpty>,
    excluded_projects: Option<Vec<String>>,
    reserve: Option<BTreeMap<String, RouteInput>>,
    voice: Option<VoiceInput>,
    telegram: Option<TelegramInput>,
}

/// Valor fora do aceito: 422 com o nome do campo.
fn invalid(field: &str) -> ApiError {
    ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_config_invalid", params([("field", json!(field))]))
}

/// Conexão cadastrada e de um tipo que o caminho usa (a fala ou a transcrição, pelas regras de
/// [`super::voice`]); outra é recusada com o campo.
fn voice_connection(cfg: &Config, field: &str, id: &str, usable: fn(ConnectionKind) -> bool) -> Result<String, ApiError> {
    let id = connection(cfg, field, id, true)?;
    if id.is_empty() || cfg.connections.iter().any(|c| c.id == id && usable(c.kind)) {
        Ok(id)
    } else {
        Err(invalid(field))
    }
}

/// Conexão vazia (sem) ou cadastrada.
fn connection(cfg: &Config, field: &str, id: &str, allow_empty: bool) -> Result<String, ApiError> {
    let id = id.trim();
    if id.is_empty() && allow_empty {
        return Ok(String::new());
    }
    if cfg.connections.iter().any(|c| c.id == id) {
        Ok(id.to_string())
    } else {
        Err(invalid(field))
    }
}

fn text(field: &str, value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    validate_text_field(field, value)?;
    Ok(value.to_string())
}

fn clock(field: &str, value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    parse_hhmm(value).map(|_| value.to_string()).ok_or_else(|| invalid(field))
}

/// Aplica o pedido sobre a configuração do Nanty (função pura, validada campo a campo).
fn apply(cfg: &Config, input: ConfigInput) -> Result<NantyConfig, ApiError> {
    let mut n = cfg.nanty.clone();
    if let Some(id) = input.connection_id {
        n.connection_id = connection(cfg, "connection_id", &id, true)?;
    }
    if let Some(model) = input.model {
        n.model = text("model", &model)?;
    }
    if let Some(effort) = input.effort {
        n.effort = effort;
    }
    if let Some(usd) = input.daily_budget_usd {
        if !usd.is_finite() || !(0.0..=MAX_BUDGET_USD).contains(&usd) {
            return Err(invalid("daily_budget_usd"));
        }
        n.daily_budget_usd = usd;
    }
    if let Some(tokens) = input.daily_budget_tokens {
        if tokens > MAX_BUDGET_TOKENS {
            return Err(invalid("daily_budget_tokens"));
        }
        n.daily_budget_tokens = tokens;
    }
    if let Some(v) = input.quiet_start {
        n.quiet_start = clock("quiet_start", &v)?;
    }
    if let Some(v) = input.quiet_end {
        n.quiet_end = clock("quiet_end", &v)?;
    }
    if let Some(v) = input.digest_time {
        n.digest_time = clock("digest_time", &v)?;
    }
    if let Some(v) = input.digest_empty {
        n.digest_empty = v;
    }
    if let Some(list) = input.excluded_projects {
        if list.len() > MAX_EXCLUDED {
            return Err(invalid("excluded_projects"));
        }
        let mut out: Vec<String> = Vec::new();
        for id in list {
            validate_project_id(&id).map_err(|_| invalid("excluded_projects"))?;
            if !out.contains(&id) {
                out.push(id);
            }
        }
        n.excluded_projects = out;
    }
    if let Some(reserve) = input.reserve {
        let mut out = BTreeMap::new();
        for (role, route) in reserve {
            if !ROLES.contains(&role.as_str()) {
                return Err(invalid("reserve"));
            }
            let field = format!("reserve.{role}");
            let conn = connection(cfg, &field, &route.connection_id, true)?;
            if conn.is_empty() {
                continue;
            }
            let model = text("model", &route.model)?;
            if model.is_empty() {
                return Err(invalid(&field));
            }
            out.insert(role, NantyRoute { connection_id: conn, model });
        }
        n.reserve = out;
    }
    if let Some(voice) = input.voice {
        if let Some(enabled) = voice.enabled {
            n.voice.enabled = enabled;
        }
        if let Some(order) = voice.order {
            if order.len() > MAX_VOICE_STEPS {
                return Err(invalid("voice.order"));
            }
            let mut out = Vec::new();
            for step in order {
                out.push(VoiceRoute {
                    connection_id: {
                        let id = connection(cfg, "voice.order", &step.connection_id, false)?;
                        voice_connection(cfg, "voice.order", &id, voice::speaks)?
                    },
                    model: text("model", &step.model)?,
                    voice: text("voice", &step.voice)?,
                });
            }
            n.voice.order = out;
        }
        if let Some(route) = voice.transcribe {
            let conn = voice_connection(cfg, "voice.transcribe", &route.connection_id, voice::transcribes)?;
            let model = if conn.is_empty() { String::new() } else { text("model", &route.model)? };
            n.voice.transcribe = NantyRoute { connection_id: conn, model };
        }
        if let Some(secs) = voice.max_input_secs {
            if !VOICE_INPUT_SECS.contains(&secs) {
                return Err(invalid("voice.max_input_secs"));
            }
            n.voice.max_input_secs = secs;
        }
    }
    if let Some(tg) = input.telegram {
        if let Some(hold) = tg.hold_while_visible {
            n.telegram.hold_while_visible = hold;
        }
        if let Some(secs) = tg.visible_hold_secs {
            if secs > MAX_VISIBLE_HOLD_SECS {
                return Err(invalid("telegram.visible_hold_secs"));
            }
            n.telegram.visible_hold_secs = secs;
        }
    }
    Ok(n)
}

/// Grava a configuração do Nanty nova, se mudou. Devolve a configuração vigente.
async fn save(state: &AppState, change: impl FnOnce(&Config) -> Result<NantyConfig, ApiError>) -> Result<NantyConfig, ApiError> {
    let mut guard = state.config().write().await;
    let nanty = change(&guard)?;
    if nanty == guard.nanty {
        return Ok(nanty);
    }
    let routes_changed = nanty.routes() != guard.nanty.routes() || nanty.voice != guard.nanty.voice;
    let mut next = guard.clone();
    next.nanty = nanty;
    persist(state, &next).await?;
    let saved = next.nanty.clone();
    *guard = next;
    drop(guard);
    if routes_changed {
        // Modelos novos do Nanty entram no `models_in_use`: a estimativa de preço sai em segundo plano.
        spawn_pricing_autofill(state);
    }
    // O orçamento e o silêncio mudam o avatar e o cabeçalho; o resumo e o nível, o feed.
    state.nanty().changed(Part::Status);
    state.nanty().changed(Part::Feed);
    Ok(saved)
}

/// `PUT /api/nanty/config`.
pub async fn put_config(State(state): State<AppState>, body: Bytes) -> Result<Json<Value>, ApiError> {
    let input: ConfigInput = parse_json(&body)?;
    let saved = save(&state, |cfg| apply(cfg, input)).await?;
    Ok(config_json(&saved, state.nanty().now()))
}

/// Corpo de `PUT /api/nanty/actions/:id`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LevelInput {
    level: ActionLevel,
}

/// `PUT /api/nanty/actions/:id`: o nível de uma ação do catálogo. O nível igual ao inicial sai do
/// arquivo (só o que a pessoa mudou fica gravado).
pub async fn put_action(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let action = actions::parse(&id)
        .ok_or_else(|| ApiError::coded(StatusCode::NOT_FOUND, "nanty_action_unknown", Map::new()))?;
    let entry = action.entry();
    if entry.kind != ActionKind::Executes {
        return Err(ApiError::coded(StatusCode::CONFLICT, "nanty_action_locked", Map::new()));
    }
    let input: LevelInput = parse_json(&body)?;
    let saved = save(&state, |cfg| {
        let mut n = cfg.nanty.clone();
        if input.level == entry.default_level {
            n.actions.remove(action.as_str());
        } else {
            n.actions.insert(action.as_str().to_string(), input.level);
        }
        Ok(n)
    })
    .await?;
    Ok(config_json(&saved, state.nanty().now()))
}

#[cfg(test)]
#[path = "t11_tests.rs"]
mod tests;
