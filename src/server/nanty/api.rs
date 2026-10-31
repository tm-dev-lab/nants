//! API do Nanty (spec 138 T01 e T03).
//!
//! - `GET /api/nanty/config`: a configuração, com o token do bot só mascarado, a hora local do Nants
//!   e o catálogo com o nível de cada ação; `PUT` e `PUT /api/nanty/actions/:id` (T11), em
//!   [`super::settings`];
//! - `GET /api/nanty/feed`: esperando, feito, previsto e o resumo de hoje;
//! - `GET /api/nanty/conversation`: a conversa única (web e Telegram); `POST` (T08): `{text}`, a
//!   pessoa escreve pela janela (a resposta fica na janela);
//! - `GET /api/nanty/status`: a instância, o estado do avatar e o orçamento do dia (T05: em US$ ou
//!   em tokens, se o modelo não tem preço, e se a cota do plano é a mesma dos papéis);
//! - `GET /api/nanty/usage` (T05): o gasto do Nanty, à parte dos projetos, de hoje e dos dias
//!   anteriores, para a janela Custos;
//! - `POST /api/nanty/cards/:id/decide` (T03): `{token, answer}` com `answer` = `yes`, `no` ou
//!   `later`; responde `{outcome, code, params}` (o que o cartão diz, traduzido pela web);
//! - `POST /api/nanty/undo/:id` (T03): `{token}`; desfaz a troca de conexão (comparar e trocar);
//! - `/api/nanty/telegram*` (T06): o canal do Telegram, em [`super::telegram::api`];
//! - `POST /api/nanty/presence` (T07, C-07): `{tab, visible}`; com uma aba visível, o envio ao
//!   Telegram espera (ver [`super::telegram::outbox`]).
//!
//! As duas rotas de decisão passam pelas mesmas proteções de toda rota que muda algo: a guarda de
//! Host/Origin (`security::guard`) e, em modo rede, o dispositivo autorizado
//! (`devices::require_device`).
//!
//! A janela acompanha pelo stream global (`GET /api/events`, evento `nanty_changed` com `part`):
//! uma só conexão SSE por aba, como o resto da interface.

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::act::{self, Answer};
use super::actions::{self, ActionKind};
use super::clock::LocalTime;
use super::store::{Card, Channel, ConversationMessage, DecidedBy, DoneItem, PlannedItem, Spend};
use super::turn::{budget_view, BudgetView};
use super::{nanty_lang, Avatar, InstanceStatus};
use crate::config::{ActionLevel, DigestEmpty, Effort, NantyConfig, NantyRoute, VoiceRoute};
use crate::server::config_api::{mask_api_key, sanitize_for_view, validate_connection_id, validate_text_field};
use crate::server::error::ApiError;
use crate::server::events::validate_project_id;
use crate::server::state::AppState;

#[derive(Debug, Serialize)]
struct RouteView {
    connection_id: String,
    model: String,
}

#[derive(Debug, Serialize)]
struct VoiceRouteView {
    connection_id: String,
    model: String,
    voice: String,
}

#[derive(Debug, Serialize)]
struct VoiceView {
    enabled: bool,
    order: Vec<VoiceRouteView>,
    /// A conexão da transcrição (T09); vazia = a primeira do Google Gemini.
    transcribe: RouteView,
    max_input_secs: u64,
}

#[derive(Debug, Serialize)]
struct TelegramView {
    /// Máscara do token (`env:NOME` aparece como está); vazio sem token.
    bot_token_masked: String,
    /// Há um dono pareado (o chat em si não sai).
    paired: bool,
    /// Com uma aba visível, o envio espera (T07, C-07).
    hold_while_visible: bool,
    visible_hold_secs: u64,
}

/// Uma ação do catálogo com nível (T11): as que o Nanty executa e a faixa "só a pessoa".
#[derive(Debug, Serialize)]
struct CatalogView {
    id: &'static str,
    /// `executes` (o seletor de três posições) ou `person_only` (a faixa travada).
    kind: &'static str,
    /// O nível efetivo (o configurado, ou o inicial do catálogo).
    level: ActionLevel,
    default_level: ActionLevel,
}

#[derive(Debug, Serialize)]
struct ConfigView {
    /// Hora local do Nants agora (`HH:MM`, C-15), a mesma do silêncio e do resumo.
    now: String,
    connection_id: String,
    model: String,
    effort: Effort,
    daily_budget_usd: f64,
    daily_budget_tokens: u64,
    quiet_start: String,
    quiet_end: String,
    digest_time: String,
    digest_empty: DigestEmpty,
    /// Idioma efetivo das mensagens (`pt-BR` ou `en`).
    lang: &'static str,
    excluded_projects: Vec<String>,
    actions: BTreeMap<String, ActionLevel>,
    /// O catálogo com nível, na ordem dele (T11).
    catalog: Vec<CatalogView>,
    reserve: BTreeMap<String, RouteView>,
    voice: VoiceView,
    telegram: TelegramView,
}

fn catalog_view(cfg: &NantyConfig) -> Vec<CatalogView> {
    actions::CATALOG
        .iter()
        .filter_map(|entry| {
            let kind = match entry.kind {
                ActionKind::Executes => "executes",
                ActionKind::PersonOnly => "person_only",
                ActionKind::CardOption | ActionKind::Link => return None,
            };
            let level = actions::level(cfg, entry.id)?;
            Some(CatalogView { id: entry.id.as_str(), kind, level, default_level: entry.default_level })
        })
        .collect()
}

fn connection_id_view(id: &str) -> String {
    sanitize_for_view(id, |v| if v.is_empty() { Ok(()) } else { validate_connection_id(v) })
}

fn route_view(route: &NantyRoute) -> RouteView {
    RouteView {
        connection_id: connection_id_view(&route.connection_id),
        model: sanitize_for_view(&route.model, |v| validate_text_field("model", v)),
    }
}

fn voice_route_view(route: &VoiceRoute) -> VoiceRouteView {
    VoiceRouteView {
        connection_id: connection_id_view(&route.connection_id),
        model: sanitize_for_view(&route.model, |v| validate_text_field("model", v)),
        voice: sanitize_for_view(&route.voice, |v| validate_text_field("voice", v)),
    }
}

fn text_view(field: &str, value: &str) -> String {
    sanitize_for_view(value, |v| validate_text_field(field, v))
}

fn config_view(cfg: &NantyConfig, now: LocalTime) -> ConfigView {
    ConfigView {
        now: format!("{:02}:{:02}", now.hour, now.minute),
        connection_id: connection_id_view(&cfg.connection_id),
        model: text_view("model", &cfg.model),
        effort: cfg.effort,
        daily_budget_usd: cfg.budget_usd(),
        daily_budget_tokens: cfg.daily_budget_tokens,
        quiet_start: text_view("quiet_start", &cfg.quiet_start),
        quiet_end: text_view("quiet_end", &cfg.quiet_end),
        digest_time: text_view("digest_time", &cfg.digest_time),
        digest_empty: cfg.digest_empty,
        lang: nanty_lang(cfg).tag(),
        excluded_projects: cfg
            .excluded_projects
            .iter()
            .map(|p| sanitize_for_view(p, validate_project_id))
            .collect(),
        actions: cfg.actions.clone(),
        catalog: catalog_view(cfg),
        reserve: cfg.reserve.iter().map(|(role, r)| (text_view("role", role), route_view(r))).collect(),
        voice: VoiceView {
            enabled: cfg.voice.enabled,
            order: cfg.voice.order.iter().map(voice_route_view).collect(),
            transcribe: route_view(&cfg.voice.transcribe),
            max_input_secs: cfg.voice.max_input_secs,
        },
        telegram: TelegramView {
            bot_token_masked: if cfg.telegram.bot_token.is_empty() {
                String::new()
            } else {
                mask_api_key(&cfg.telegram.bot_token)
            },
            paired: cfg.telegram.is_paired(),
            hold_while_visible: cfg.telegram.hold_while_visible,
            visible_hold_secs: cfg.telegram.visible_hold_secs,
        },
    }
}

/// A configuração como a tela a vê (o `GET` e a resposta das gravações da T11).
pub(super) fn config_json(cfg: &NantyConfig, now: LocalTime) -> Json<Value> {
    Json(serde_json::to_value(config_view(cfg, now)).unwrap_or_default())
}

/// `GET /api/nanty/config`.
pub async fn get_config(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = state.config().read().await;
    config_json(&cfg.nanty, state.nanty().now())
}

#[derive(Debug, Serialize)]
struct Today {
    /// Dia local (`AAAA-MM-DD`).
    day: String,
    spend: Spend,
    budget_usd: f64,
    waiting: usize,
    done: usize,
}

#[derive(Debug, Serialize)]
struct FeedView {
    /// Cartões abertos (ainda válidos), do mais novo para o mais velho.
    waiting: Vec<Card>,
    /// O que foi feito, do mais novo para o mais velho.
    done: Vec<DoneItem>,
    planned: Vec<PlannedItem>,
    today: Today,
}

/// `GET /api/nanty/feed`.
pub async fn get_feed(State(state): State<AppState>) -> Json<serde_json::Value> {
    let budget = state.config().read().await.nanty.budget_usd();
    let nanty = state.nanty();
    let now = nanty.now();
    let snapshot = nanty.snapshot();
    // Início do dia local: a hora local menos os minutos do dia e os segundos do minuto.
    let day_start = now
        .epoch_ms
        .saturating_sub(u64::from(now.minute_of_day()) * 60_000 + now.epoch_ms % 60_000);
    let spend = snapshot.spend_on(&now.day_key());
    let mut waiting: Vec<Card> = snapshot.cards.into_iter().filter(|c| c.expires_at_ms > now.epoch_ms).collect();
    waiting.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
    let mut done = snapshot.done;
    done.reverse();
    let today = Today {
        day: now.day_key(),
        spend,
        budget_usd: budget,
        waiting: waiting.len(),
        done: done.iter().filter(|d| d.at_ms >= day_start).count(),
    };
    let view = FeedView { waiting, done, planned: snapshot.planned, today };
    Json(serde_json::to_value(view).unwrap_or_default())
}

#[derive(Debug, Serialize)]
struct ConversationView {
    messages: Vec<ConversationMessage>,
}

/// `GET /api/nanty/conversation`.
pub async fn get_conversation(State(state): State<AppState>) -> Json<serde_json::Value> {
    let nanty = std::sync::Arc::clone(state.nanty());
    let messages = tokio::task::spawn_blocking(move || nanty.conversation()).await.unwrap_or_default();
    Json(serde_json::to_value(ConversationView { messages }).unwrap_or_default())
}

#[derive(Debug, Serialize)]
struct StatusView {
    instance: InstanceStatus,
    avatar: Avatar,
    /// O modelo do Nanty está escolhido (sem ele, só o texto fixo).
    model_configured: bool,
    telegram_configured: bool,
    telegram_paired: bool,
    /// Saúde do canal do Telegram (T06): `off` fora do dono da trava.
    telegram: super::telegram::Health,
    /// O orçamento do dia (T05).
    budget: BudgetView,
    /// Turnos do modelo na fila agora (T05).
    turns_queued: usize,
    /// Projetos cadastrados que o Nanty observa (os que não estão nas exclusões), para o cabeçalho
    /// da janela (T10).
    projects_observed: usize,
}

/// `GET /api/nanty/status`.
pub async fn get_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let full = state.config().read().await.clone();
    let cfg = &full.nanty;
    let nanty = state.nanty();
    let spend = nanty.snapshot().spend_on(&nanty.now().day_key());
    let view = StatusView {
        instance: nanty.status(),
        avatar: nanty.avatar(cfg),
        model_configured: !cfg.model.trim().is_empty() && !cfg.connection_id.trim().is_empty(),
        telegram_configured: !cfg.telegram.bot_token.trim().is_empty(),
        telegram_paired: cfg.telegram.is_paired(),
        telegram: if nanty.status() == InstanceStatus::Active {
            nanty.telegram().health()
        } else {
            super::telegram::Health::Off
        },
        budget: budget_view(&full, &spend),
        turns_queued: nanty.turns().queued(),
        projects_observed: full.projects.iter().filter(|p| cfg.observes(&p.id)).count(),
    };
    Json(serde_json::to_value(view).unwrap_or_default())
}

#[derive(Debug, Serialize)]
struct UsageView {
    budget: BudgetView,
    today: Spend,
    /// Dias anteriores, do mais novo para o mais velho.
    history: Vec<Spend>,
}

/// `GET /api/nanty/usage` (T05): o gasto do Nanty, à parte dos projetos.
pub async fn get_usage(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cfg = state.config().read().await.clone();
    let nanty = state.nanty();
    let day = nanty.now().day_key();
    let snapshot = nanty.snapshot();
    let today = snapshot.spend_on(&day);
    let mut history: Vec<Spend> = snapshot.spend_history.into_iter().filter(|s| s.day != day).collect();
    if !snapshot.spend.day.is_empty() && snapshot.spend.day != day {
        history.retain(|s| s.day != snapshot.spend.day);
        history.push(snapshot.spend);
    }
    history.sort_by(|a, b| b.day.cmp(&a.day));
    let view = UsageView { budget: budget_view(&cfg, &today), today, history };
    Json(serde_json::to_value(view).unwrap_or_default())
}

/// Corpo de `POST /api/nanty/cards/:id/decide`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideInput {
    token: String,
    answer: String,
}

/// Corpo de `POST /api/nanty/undo/:id`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UndoInput {
    token: String,
}

/// JSON malformado vira 400; campos errados, 422.
pub(super) fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice::<T>(body).map_err(|e| {
        if e.classify() == serde_json::error::Category::Data {
            ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields", Map::new())
        } else {
            ApiError::coded(StatusCode::BAD_REQUEST, "json_malformed", Map::new())
        }
    })
}

/// Id de cartão ou de desfazer: 16 hex (o `new_id`).
fn validate_id(id: &str) -> Result<(), ApiError> {
    if id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_id_invalid", Map::new()))
    }
}

fn outcome_json(outcome: &act::Outcome) -> Json<Value> {
    Json(serde_json::to_value(outcome).unwrap_or_default())
}

/// `POST /api/nanty/cards/:id/decide`.
pub async fn post_decide(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    validate_id(&id)?;
    let input: DecideInput = parse_json(&body)?;
    let answer = Answer::parse(&input.answer)
        .ok_or_else(|| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_answer_invalid", Map::new()))?;
    let outcome = act::decide(&state, &id, &input.token, answer, DecidedBy::Web).await?;
    Ok(outcome_json(&outcome))
}

/// `POST /api/nanty/undo/:id`.
pub async fn post_undo(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    validate_id(&id)?;
    let input: UndoInput = parse_json(&body)?;
    let outcome = act::undo(&state, &id, &input.token, DecidedBy::Web).await?;
    Ok(outcome_json(&outcome))
}

/// Corpo de `POST /api/nanty/presence`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresenceInput {
    tab: String,
    visible: bool,
}

/// `POST /api/nanty/presence` (T07, C-07): a aba avisa se está visível (no `visibilitychange` e,
/// enquanto visível, de tempos em tempos). Com uma aba visível, o envio ao Telegram espera.
pub async fn post_presence(State(state): State<AppState>, body: Bytes) -> Result<Json<Value>, ApiError> {
    let input: PresenceInput = parse_json(&body)?;
    if !super::telegram::outbox::valid_tab(&input.tab) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_tab_invalid", Map::new()));
    }
    let nanty = state.nanty();
    let now = nanty.now().epoch_ms;
    let outbox = nanty.telegram().outbox();
    outbox.presence(&input.tab, input.visible, now);
    Ok(Json(serde_json::json!({ "visible": outbox.visible(now) })))
}

/// Corpo de `POST /api/nanty/conversation`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageInput {
    text: String,
}

/// `POST /api/nanty/conversation` (T08): a pessoa escreve ao Nanty pela janela. O texto é redigido
/// pelo `SecretSet` antes de ser gravado; a resposta sai numa tarefa à parte, na conversa da janela
/// (nunca no Telegram), e a janela acompanha pelo `nanty_changed`. Responde 202 com o id da mensagem.
pub async fn post_conversation(State(state): State<AppState>, body: Bytes) -> Result<(StatusCode, Json<Value>), ApiError> {
    let input: MessageInput = parse_json(&body)?;
    let nanty = std::sync::Arc::clone(state.nanty());
    if nanty.status() != InstanceStatus::Active {
        return Err(ApiError::coded(StatusCode::CONFLICT, "nanty_unavailable", Map::new()));
    }
    let length = input.text.chars().count();
    if input.text.trim().is_empty() || length > super::chat::MAX_PERSON_CHARS {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_message_invalid", Map::new()));
    }
    let secrets = state.secret_set().await;
    let person = super::chat::person_message(&nanty, &secrets, Channel::Web, &input.text)
        .ok_or_else(|| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_message_invalid", Map::new()))?;
    let id = person.id.clone();
    super::chat::spawn_reply(&state, person);
    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({ "id": id }))))
}
