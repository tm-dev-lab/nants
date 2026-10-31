//! Ações do Nanty em execução (spec 138 T03): a oferta no cartão, o automático, a decisão, o
//! desfazer e o registro.
//!
//! - **Oferta ([`tick`]):** depois de cada rodada dos observadores, cada condição aberta ganha a
//!   ação do catálogo ([`super::actions::suggest`]) conforme o nível do id: desligada some do
//!   cartão; "pergunta antes" vai ao cartão (com o `spec_id` e, na aprovação, o hash do plano);
//!   automática roda depois da espera do grupo. A mesma oferta não se repete sem fato novo.
//! - **Chamadas por dentro, sem HTTP (C-03):** aprovar e retomar pelo `runs::start`; a troca pelo
//!   `config_api::set_role_connection`, sob o lock da configuração; a limpeza pelo
//!   `envs::clean_orphans` com a verificação estrita de atividade (C-16).
//! - **Cartão:** token aleatório no servidor e validade; o estado é conferido no clique (se não vale
//!   mais, o cartão expirou); a decisão é atômica (o segundo clique recebe "já decidido por ...").
//! - **Desfazer:** a troca de conexão volta à anterior só se o valor atual ainda for o que o Nanty
//!   gravou (comparar e trocar); se não for, o cartão diz que a configuração mudou.
//!
//! - **Pedido na conversa (T08, [`request_card`]):** a ação pedida pela pessoa (o comando pausar
//!   ou o id que o modelo propôs) sempre vira um cartão de confirmação, mesmo a automática.
//!
//! O registro recebe cada sugestão, cada decisão (com quem decidiu) e cada resultado, só com
//! códigos, ids e contagens (C-10).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use axum::http::StatusCode;
use serde::Serialize;
use serde_json::{json, Map, Value};

use super::actions::{self, ActionId, ActionKind, Suggestion};
use super::filter::{card_ttl, delivery_for};
use super::journal::{JournalEntry, JournalKind};
use super::signals::{urgency_of, SignalKind};
use super::store::{
    Alert, Card, Channel, DecidedBy, DecidedCard, Delivery, DoneItem, NantyState, Offer, PlannedItem, RetryCount,
    RouteValue, Silence, UndoEntry, UndoRef,
};
use super::{InstanceStatus, Nanty};
use crate::config::{ActionLevel, Config, Urgency};
use crate::i18n::Lang;
use crate::server::config_api::{set_role_connection, RoleSwap};
use crate::server::error::{fallback_text_in, ApiError};
use crate::server::state::AppState;

/// "Lembrar depois": silêncio do grupo por este tempo (provisório).
pub const REMIND_LATER_MS: u64 = 60 * 60 * 1000;
/// Validade do desfazer da troca de conexão (provisória).
pub const UNDO_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// Token aleatório de um cartão (32 hex, 128 bits).
pub fn new_token() -> String {
    format!("{}{}", super::new_id(), super::new_id())
}

/// Resposta de um cartão: sim, não ou lembrar depois.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Later,
}

impl Answer {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "yes" => Some(Answer::Yes),
            "no" => Some(Answer::No),
            "later" => Some(Answer::Later),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Answer::Yes => "yes",
            Answer::No => "no",
            Answer::Later => "later",
        }
    }
}

fn by_str(by: DecidedBy) -> &'static str {
    match by {
        DecidedBy::Auto => "auto",
        DecidedBy::Telegram => "telegram",
        DecidedBy::Web => "web",
    }
}

/// O que o cartão diz depois do clique: `code` (`nanty.outcome.*`) e `params`. A web traduz; o
/// Telegram (T07) usa [`outcome_text`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Outcome {
    pub outcome: &'static str,
    pub code: String,
    pub params: Map<String, Value>,
}

impl Outcome {
    fn new(outcome: &'static str) -> Self {
        Self { outcome, code: format!("nanty.outcome.{outcome}"), params: Map::new() }
    }

    fn with(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.params.insert(name.to_string(), value.into());
        self
    }

    fn already_decided(by: DecidedBy) -> Self {
        Self { outcome: "already_decided", code: format!("nanty.outcome.already_decided.{}", by_str(by)), params: Map::new() }
    }
}

/// Texto do resultado no idioma do Nanty, pelo dicionário: a recusa de uma rota (`error`, o `code`
/// do `ApiError`) sai pelo `serverError.<code>`.
pub fn outcome_text(lang: Lang, outcome: &Outcome) -> String {
    let error = outcome.params.get("error").and_then(Value::as_str).map(|code| {
        let params = outcome.params.get("error_params").and_then(Value::as_object).cloned().unwrap_or_default();
        fallback_text_in(lang, code, &params)
    });
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    if let Some(text) = &error {
        pairs.push(("error", text.as_str()));
    }
    crate::i18n::tr(lang, &outcome.code, &pairs)
}

/// Modo da chamada por dentro do `runs::start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Approve,
    Resume,
}

pub type StartFuture = Pin<Box<dyn Future<Output = Result<(), ApiError>> + Send>>;

/// Quem dispara a execução (produção: o `runs::start`; testes: um falso que só anota).
pub trait RunStarter: Send + Sync {
    fn start(&self, state: AppState, project_id: String, spec_id: String, mode: StartMode) -> StartFuture;
}

/// O `runs::start` por dentro, sem HTTP (C-03): 202 é sucesso; a recusa traz o `code`.
#[derive(Debug, Default)]
pub struct RealStarter;

impl RunStarter for RealStarter {
    fn start(&self, state: AppState, project_id: String, spec_id: String, mode: StartMode) -> StartFuture {
        Box::pin(async move {
            let mode = match mode {
                StartMode::Approve => crate::server::runs::Mode::Approve,
                StartMode::Resume => crate::server::runs::Mode::Resume,
            };
            let response = crate::server::runs::start(&state, &project_id, &spec_id, b"", mode).await?;
            match response.status() {
                StatusCode::ACCEPTED => Ok(()),
                // A prontidão da máquina devolve o 412 pronto (sem `ApiError`).
                StatusCode::PRECONDITION_FAILED => {
                    Err(ApiError::coded(StatusCode::PRECONDITION_FAILED, "prereq_missing", Map::new()))
                }
                _ => Err(ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new())),
            }
        })
    }
}

/// O que executar.
#[derive(Debug, Clone)]
struct Job {
    suggestion: Suggestion,
    group: String,
    by: DecidedBy,
    card_id: Option<String>,
    plan_hash: Option<String>,
}

/// Resultado da execução, para o "feito" e o registro.
#[derive(Debug)]
struct Done {
    result: &'static str,
    params: Map<String, Value>,
    undo: Option<UndoEntry>,
}

impl Done {
    fn ok(params: Map<String, Value>) -> Self {
        Self { result: "ok", params, undo: None }
    }

    fn failed(err: &ApiError, mut params: Map<String, Value>) -> Self {
        params.insert("error".into(), json!(err.code().unwrap_or("internal")));
        if let Some(p) = err.params() {
            params.insert("error_params".into(), Value::Object(p.clone()));
        }
        Self { result: "failed", params, undo: None }
    }

    fn expired(reason: &str) -> Self {
        let mut params = Map::new();
        params.insert("reason".into(), json!(reason));
        Self { result: "expired", params, undo: None }
    }
}

fn entry(at_ms: u64, kind: JournalKind, code: &str, project_id: Option<&str>, action: ActionId) -> JournalEntry {
    let mut e = JournalEntry::new(at_ms, kind, code);
    e.project_id = project_id.map(str::to_string);
    e.action = Some(action.as_str().to_string());
    e
}

fn project_root(cfg: &Config, project_id: Option<&str>) -> Option<PathBuf> {
    let id = project_id?;
    cfg.projects.iter().find(|p| p.id == id).map(|p| PathBuf::from(&p.path))
}

/// Hash do plano (o `spec.md` e as tarefas) agora; `None` se não dá para ler.
async fn plan_hash_now(cfg: &Config, project_id: Option<&str>, spec_id: Option<&str>) -> Option<String> {
    let root = project_root(cfg, project_id)?;
    let spec = spec_id?.to_string();
    tokio::task::spawn_blocking(move || crate::server::plan_review::plan_signature(&root, &spec)).await.ok().flatten()
}

fn base_params(s: &Suggestion) -> Map<String, Value> {
    let mut p = Map::new();
    if let Some(spec) = &s.spec_id {
        p.insert("spec_id".into(), json!(spec));
    }
    if let Some(role) = &s.role {
        p.insert("role".into(), json!(role));
    }
    p
}

/// Executa a ação (fora de qualquer lock do Nanty).
async fn execute(state: &AppState, nanty: &Arc<Nanty>, job: &Job) -> Done {
    let s = &job.suggestion;
    let cfg = state.config().read().await.clone();
    let params = base_params(s);
    let start = |mode| {
        let starter = nanty.starter();
        let (project, spec) = (s.project_id.clone().unwrap_or_default(), s.spec_id.clone().unwrap_or_default());
        starter.start(state.clone(), project, spec, mode)
    };
    match s.action {
        ActionId::ApprovePlan => {
            // C-03: o `ApproveInput` não tem versão; o hash do plano é conferido logo antes do start.
            let now = plan_hash_now(&cfg, s.project_id.as_deref(), s.spec_id.as_deref()).await;
            if now.is_none() || now != job.plan_hash {
                return Done::expired("plan_changed");
            }
            match start(StartMode::Approve).await {
                Ok(()) => Done::ok(params),
                Err(err) => Done::failed(&err, params),
            }
        }
        ActionId::ResumeRun | ActionId::RetryProvider => match start(StartMode::Resume).await {
            Ok(()) => Done::ok(params),
            Err(err) => Done::failed(&err, params),
        },
        ActionId::SwapReserve => {
            let (Some(role), Some(reserve)) = (s.role.as_deref(), s.reserve.as_ref()) else {
                return Done::expired("no_reserve");
            };
            let previous = match set_role_connection(state, role, &reserve.connection_id, &reserve.model, None).await {
                Ok(RoleSwap::Done { previous }) => RouteValue { connection_id: previous.0, model: previous.1 },
                Ok(RoleSwap::Changed { .. }) => return Done::expired("config_changed"),
                Err(err) => return Done::failed(&err, params),
            };
            let now = nanty.now().epoch_ms;
            let undo = UndoEntry {
                id: super::new_id(),
                token: new_token(),
                project_id: s.project_id.clone(),
                action: s.action.as_str().to_string(),
                role: role.to_string(),
                previous,
                applied: reserve.clone(),
                at_ms: now,
                expires_at_ms: now.saturating_add(UNDO_TTL_MS),
            };
            // A troca já inclui a retomada (C-02).
            let mut done = match start(StartMode::Resume).await {
                Ok(()) => Done::ok(params),
                Err(err) => Done::failed(&err, params),
            };
            done.undo = Some(undo);
            done
        }
        ActionId::CleanOrphans => {
            let projects = crate::server::envs::registered_projects(state).await;
            let provider = state.compose().clone();
            let st = state.clone();
            let job = tokio::task::spawn_blocking(move || {
                crate::server::envs::clean_orphans(&provider, &projects, &|id| st.project_busy_strict(id))
            });
            match tokio::time::timeout(crate::server::envs::SWEEP_TIMEOUT, job).await {
                Ok(Ok(Ok(report))) => {
                    let mut p = params;
                    p.insert("projects".into(), json!(report.cleaned.len()));
                    p.insert("networks".into(), json!(report.networks_removed));
                    p.insert("foreign".into(), json!(report.foreign));
                    Done::ok(p)
                }
                _ => Done::failed(&ApiError::coded(StatusCode::SERVICE_UNAVAILABLE, "docker_unavailable", Map::new()), params),
            }
        }
        // Pausar (T08): o pedido de parada da execução, o mesmo do `runs::stop`.
        ActionId::PauseRun => {
            let (project, spec) = (s.project_id.as_deref().unwrap_or_default(), s.spec_id.as_deref().unwrap_or_default());
            if state.runs().request_stop(project, spec) {
                Done::ok(params)
            } else {
                Done::expired("not_running")
            }
        }
        // Links, opções do cartão e a faixa "só a pessoa" nunca chegam aqui.
        _ => Done::expired("not_executable"),
    }
}

/// Grava o resultado: o "feito", o registro da decisão e o do resultado.
fn record(nanty: &Nanty, job: &Job, done: Done) {
    let now = nanty.now().epoch_ms;
    let s = &job.suggestion;
    let action = s.action;
    let undo_ref = done.undo.as_ref().map(|u| UndoRef { id: u.id.clone(), token: u.token.clone(), expires_at_ms: u.expires_at_ms });
    let mut decision = entry(now, JournalKind::Decision, "yes", s.project_id.as_deref(), action);
    decision.decided_by = Some(job.by);
    decision.params.insert("group".into(), json!(job.group));
    let mut result = entry(now, JournalKind::Result, done.result, s.project_id.as_deref(), action);
    result.decided_by = Some(job.by);
    result.params = done.params.clone();
    if undo_ref.is_some() {
        result.params.insert("undo".into(), json!(true));
    }
    nanty.update(|st| {
        if let Some(undo) = done.undo {
            st.undo.push(undo);
        }
        st.push_done(DoneItem {
            at_ms: now,
            project_id: s.project_id.clone(),
            code: action.label_code(),
            params: done.params,
            action: Some(action.as_str().to_string()),
            decided_by: Some(job.by),
            result: done.result.to_string(),
            card_id: job.card_id.clone(),
            undo: undo_ref,
        });
        true
    });
    nanty.record(&decision);
    nanty.record(&result);
}

/// Parâmetros do cartão criado para uma oferta: os fatos, o observador, o horário e a idade.
fn offer_params(alert: &Alert, kind: SignalKind, now_ms: u64) -> Map<String, Value> {
    let mut p = alert.facts.clone();
    p.insert("observer".into(), json!(kind.observer().as_str()));
    p.insert("since".into(), json!(alert.since));
    p.insert("age_min".into(), json!(now_ms.saturating_sub(alert.first_seen_ms) / 60_000));
    p
}

/// Tira a ação oferecida do cartão do grupo (o aviso fica). O token morre com a oferta: um token
/// só decide a oferta que ele acompanhou. `true` se mudou.
fn detach(st: &mut NantyState, group: &str) -> bool {
    let mut changed = false;
    for card in st.cards.iter_mut().filter(|c| c.group == group && c.action.is_some()) {
        card.action = None;
        card.plan_hash = None;
        card.spec_id = None;
        card.token = None;
        changed = true;
    }
    changed
}

/// O que fazer com uma condição nesta rodada.
enum Plan {
    /// Nada (ou tirar a ação do cartão).
    Detach,
    /// Ação no cartão, com o hash do plano (aprovação).
    Offer(Suggestion, Option<String>),
    /// Automática: rodar agora (com o hash do plano, na aprovação).
    Run(Suggestion, Option<String>),
    /// Automática com espera (nova tentativa).
    Schedule(Suggestion, u64),
    /// Esperar (espera do grupo, ou já feita com os mesmos fatos).
    Wait,
}

/// Limpeza de cada rodada: ofertas e previstos de condições que sumiram e contagens velhas.
fn housekeeping(st: &mut NantyState, now_ms: u64) -> bool {
    let before = (st.declined.len(), st.handled.len(), st.planned.len(), st.retries.len());
    let alerts = &st.alerts;
    st.declined.retain(|g, _| alerts.contains_key(g));
    st.handled.retain(|g, _| alerts.contains_key(g));
    st.planned.retain(|p| p.group.as_deref().is_none_or(|g| alerts.contains_key(g)));
    st.retries.retain(|_, r| r.last_ms.saturating_add(actions::RETRY_RESET_MS) > now_ms);
    let pruned = st.prune_expired(now_ms);
    before != (st.declined.len(), st.handled.len(), st.planned.len(), st.retries.len()) || pruned
}

/// Uma rodada das ações, logo depois da rodada dos observadores.
pub async fn tick(state: &AppState, nanty: &Arc<Nanty>) {
    let cfg = state.config().read().await.clone();
    let now = nanty.now();
    let now_ms = now.epoch_ms;
    nanty.update(|st| housekeeping(st, now_ms));
    let snap = nanty.snapshot();
    let group_wait = cfg.nanty.filter.group_wait_ms();
    let mut plans: Vec<(String, Plan)> = Vec::new();
    for (group, alert) in &snap.alerts {
        if SignalKind::parse(&alert.kind).is_none() {
            continue;
        }
        let Some(s) = actions::suggest(alert, &cfg, &snap.retries) else {
            plans.push((group.clone(), Plan::Detach));
            continue;
        };
        let kind = s.action.entry().kind;
        let level = actions::level(&cfg.nanty, s.action);
        let auto = kind == ActionKind::Executes && level == Some(ActionLevel::Auto);
        let offered = matches!(kind, ActionKind::Executes | ActionKind::Link | ActionKind::PersonOnly)
            && level != Some(ActionLevel::Off);
        if !auto && !offered {
            plans.push((group.clone(), Plan::Detach));
            continue;
        }
        // Só a ação automática roda antes do aviso; a oferta espera o aviso e não vai se está calado.
        let waiting = if auto {
            now_ms < alert.first_seen_ms.saturating_add(group_wait)
        } else {
            alert.notified.is_none() || alert.suppressed.is_some()
        };
        if waiting {
            plans.push((group.clone(), Plan::Wait));
            continue;
        }
        // C-03: a aprovação leva o hash do plano (o `spec.md` e as tarefas).
        let hash = if s.action == ActionId::ApprovePlan {
            match plan_hash_now(&cfg, s.project_id.as_deref(), s.spec_id.as_deref()).await {
                Some(hash) => Some(hash),
                None => {
                    plans.push((group.clone(), Plan::Detach));
                    continue;
                }
            }
        } else {
            None
        };
        let plan = if !auto {
            Plan::Offer(s, hash)
        } else {
            let offer = Offer {
                action: s.action.as_str().to_string(),
                fingerprint: actions::fingerprint(&alert.facts, hash.as_deref()),
            };
            if snap.handled.get(group) == Some(&offer) {
                Plan::Wait
            } else if s.action == ActionId::RetryProvider {
                let done = s.retry_key.as_ref().and_then(|k| snap.retries.get(k)).map_or(0, |r| r.count);
                Plan::Schedule(s, now_ms.saturating_add(actions::retry_wait_ms(done)))
            } else {
                Plan::Run(s, hash)
            }
        };
        plans.push((group.clone(), plan));
    }

    let mut run_now: Vec<Job> = Vec::new();
    let mut journal: Vec<JournalEntry> = Vec::new();
    let quiet = now.within(cfg.nanty.quiet_window());
    nanty.update(|st| {
        let mut changed = false;
        for (group, plan) in plans {
            let Some(alert) = st.alerts.get(&group).cloned() else { continue };
            match plan {
                Plan::Wait => {}
                Plan::Detach => changed |= detach(st, &group),
                Plan::Offer(s, hash) => {
                    let offer = Offer {
                        action: s.action.as_str().to_string(),
                        fingerprint: actions::fingerprint(&alert.facts, hash.as_deref()),
                    };
                    if st.declined.get(&group) == Some(&offer) || st.handled.get(&group) == Some(&offer) {
                        changed |= detach(st, &group);
                        continue;
                    }
                    let action = Some(s.action.as_str().to_string());
                    if let Some(card) = st.cards.iter_mut().find(|c| c.group == group) {
                        if card.action == action && card.plan_hash == hash && card.spec_id == s.spec_id && card.token.is_some() {
                            continue;
                        }
                        // Toda oferta que entra no cartão (nova, que mudou de ação, de spec ou de
                        // plano, ou que volta depois de sair) ganha id e token novos. O botão já
                        // enviado passa a receber "expirou" e nunca decide sobre o que a pessoa não
                        // viu (C-03). Só a mesma oferta, intacta, mantém id e token (acima).
                        card.id = super::new_id();
                        card.token = Some(new_token());
                        card.created_at_ms = now_ms;
                        card.action = action;
                        card.plan_hash = hash;
                        card.spec_id = s.spec_id.clone();
                    } else {
                        let Some(kind) = SignalKind::parse(&alert.kind) else { continue };
                        let urgency = urgency_of(kind, &cfg.nanty);
                        let shown = if urgency == Urgency::Digest { Urgency::Normal } else { urgency };
                        st.cards.push(Card {
                            id: super::new_id(),
                            project_id: alert.project_id.clone(),
                            group: group.clone(),
                            code: kind.why_code(),
                            params: offer_params(&alert, kind, now_ms),
                            action,
                            plan_hash: hash,
                            spec_id: s.spec_id.clone(),
                            token: Some(new_token()),
                            created_at_ms: now_ms,
                            expires_at_ms: now_ms.saturating_add(card_ttl(&cfg.nanty, false)),
                            urgency: Some(urgency),
                            delivery: Some(delivery_for(shown, quiet)),
                            requested: None,
                        });
                    }
                    let mut e = entry(now_ms, JournalKind::Suggestion, s.action.as_str(), s.project_id.as_deref(), s.action);
                    e.params.insert("group".into(), json!(group));
                    journal.push(e);
                    changed = true;
                }
                Plan::Run(s, hash) => {
                    detach(st, &group);
                    st.handled.insert(
                        group.clone(),
                        Offer {
                            action: s.action.as_str().to_string(),
                            fingerprint: actions::fingerprint(&alert.facts, hash.as_deref()),
                        },
                    );
                    changed = true;
                    run_now.push(Job { suggestion: s, group, by: DecidedBy::Auto, card_id: None, plan_hash: hash });
                }
                Plan::Schedule(s, at_ms) => {
                    detach(st, &group);
                    st.handled.insert(
                        group.clone(),
                        Offer { action: s.action.as_str().to_string(), fingerprint: actions::fingerprint(&alert.facts, None) },
                    );
                    let mut params = base_params(&s);
                    params.insert("attempt".into(), json!(retry_count(st, &s) + 1));
                    st.planned.retain(|p| p.group.as_deref() != Some(group.as_str()));
                    st.planned.push(PlannedItem {
                        project_id: s.project_id.clone(),
                        group: Some(group.clone()),
                        action: s.action.as_str().to_string(),
                        code: s.action.label_code(),
                        params: params.clone(),
                        at_ms,
                    });
                    let mut e = entry(now_ms, JournalKind::Planned, s.action.as_str(), s.project_id.as_deref(), s.action);
                    e.params = params;
                    e.params.insert("at_ms".into(), json!(at_ms));
                    journal.push(e);
                    changed = true;
                }
            }
        }
        // Previstos vencidos: a nova tentativa sai agora, se a condição ainda é a mesma.
        let due: Vec<PlannedItem> = st.planned.iter().filter(|p| p.at_ms <= now_ms).cloned().collect();
        if !due.is_empty() {
            st.planned.retain(|p| p.at_ms > now_ms);
            changed = true;
        }
        for item in due {
            let Some(group) = item.group.clone() else { continue };
            let Some(alert) = st.alerts.get(&group) else { continue };
            let Some(s) = actions::suggest(alert, &cfg, &st.retries) else { continue };
            if s.action.as_str() != item.action || actions::level(&cfg.nanty, s.action) != Some(ActionLevel::Auto) {
                continue;
            }
            bump_retry(st, &s, now_ms);
            run_now.push(Job { suggestion: s, group, by: DecidedBy::Auto, card_id: None, plan_hash: None });
        }
        changed
    });
    for e in &journal {
        nanty.record(e);
    }
    for job in run_now {
        let done = execute(state, nanty, &job).await;
        record(nanty, &job, done);
    }
}

fn retry_count(st: &NantyState, s: &Suggestion) -> u32 {
    s.retry_key.as_ref().and_then(|k| st.retries.get(k)).map_or(0, |r| r.count)
}

/// Conta uma nova tentativa da parada (antes de executar).
fn bump_retry(st: &mut NantyState, s: &Suggestion, now_ms: u64) {
    if let Some(key) = &s.retry_key {
        let entry = st.retries.entry(key.clone()).or_insert_with(RetryCount::default);
        entry.count = entry.count.saturating_add(1);
        entry.last_ms = now_ms;
    }
}

/// O Nanty deste processo não pode decidir: outra instância tem a pasta de dados.
fn unavailable(nanty: &Nanty) -> bool {
    matches!(nanty.status(), InstanceStatus::OtherInstance | InstanceStatus::Unavailable | InstanceStatus::Stopped)
}

/// Erro de token: o cartão (ou o desfazer) não existe com esse token.
fn not_found() -> ApiError {
    ApiError::coded(StatusCode::NOT_FOUND, "not_found", Map::new())
}

/// O que a decisão atômica separou.
enum Claim {
    BadToken,
    Expired,
    Already(DecidedBy),
    Invalid,
    Snoozed(Card),
    Declined(Card),
    Link(ActionId, Card),
    PersonOnly,
    Execute(Card),
}

/// Decide um cartão: `yes`, `no` ou `later` (web ou Telegram). O primeiro clique vale; o segundo
/// recebe "já decidido por ...". No `yes`, o estado é conferido antes de executar.
pub async fn decide(
    state: &AppState,
    card_id: &str,
    token: &str,
    answer: Answer,
    by: DecidedBy,
) -> Result<Outcome, ApiError> {
    let nanty = Arc::clone(state.nanty());
    if unavailable(&nanty) {
        return Ok(Outcome::new("unavailable"));
    }
    let now_ms = nanty.now().epoch_ms;
    let mut claim = Claim::Expired;
    nanty.update(|st| {
        let Some(i) = st.cards.iter().position(|c| c.id == card_id) else {
            claim = match st.decided.iter().rev().find(|d| d.card_id == card_id) {
                Some(d) if d.token == token => Claim::Already(d.decided_by),
                Some(_) => Claim::BadToken,
                None => Claim::Expired,
            };
            return false;
        };
        let card = st.cards[i].clone();
        if card.token.as_deref() != Some(token) {
            claim = Claim::BadToken;
            return false;
        }
        if card.expires_at_ms <= now_ms {
            st.cards.remove(i);
            claim = Claim::Expired;
            return true;
        }
        let action = card.action.as_deref().and_then(actions::parse);
        let decided = |st: &mut NantyState| {
            st.cards.remove(i);
            st.push_decided(DecidedCard {
                card_id: card.id.clone(),
                token: token.to_string(),
                project_id: card.project_id.clone(),
                decided_by: by,
                answer: answer.as_str().to_string(),
                at_ms: now_ms,
            });
        };
        match answer {
            Answer::Later => {
                decided(st);
                st.silences.push(Silence {
                    project_id: None,
                    group: Some(card.group.clone()),
                    until_ms: now_ms.saturating_add(REMIND_LATER_MS),
                });
                // Volta a avisar quando o silêncio acabar.
                if let Some(alert) = st.alerts.get_mut(&card.group) {
                    alert.notified = None;
                }
                claim = Claim::Snoozed(card);
                true
            }
            Answer::No => {
                let Some(action) = action else {
                    claim = Claim::Invalid;
                    return false;
                };
                decided(st);
                let facts = st.alerts.get(&card.group).map(|a| a.facts.clone()).unwrap_or_default();
                st.declined.insert(
                    card.group.clone(),
                    Offer {
                        action: action.as_str().to_string(),
                        fingerprint: actions::fingerprint(&facts, card.plan_hash.as_deref()),
                    },
                );
                claim = Claim::Declined(card);
                true
            }
            Answer::Yes => match action.map(|a| (a, a.entry().kind)) {
                None | Some((_, ActionKind::CardOption)) => {
                    claim = Claim::Invalid;
                    false
                }
                Some((a, ActionKind::Link)) => {
                    claim = Claim::Link(a, card);
                    false
                }
                Some((_, ActionKind::PersonOnly)) => {
                    claim = Claim::PersonOnly;
                    false
                }
                Some((a, ActionKind::Executes)) => {
                    decided(st);
                    // Na mesma reivindicação: a oferta conta como feita antes de qualquer espera, e
                    // nenhuma rodada a oferece de novo enquanto o "sim" executa.
                    let facts = st.alerts.get(&card.group).map(|al| al.facts.clone()).unwrap_or_default();
                    st.handled.insert(
                        card.group.clone(),
                        Offer {
                            action: a.as_str().to_string(),
                            fingerprint: actions::fingerprint(&facts, card.plan_hash.as_deref()),
                        },
                    );
                    claim = Claim::Execute(card);
                    true
                }
            },
        }
    });
    let journal_decision = |action: Option<&str>, project: Option<&str>| {
        let mut e = JournalEntry::new(now_ms, JournalKind::Decision, answer.as_str());
        e.project_id = project.map(str::to_string);
        e.action = action.map(str::to_string);
        e.decided_by = Some(by);
        e.params.insert("card_id".into(), json!(card_id));
        nanty.record(&e);
    };
    match claim {
        Claim::BadToken => Err(not_found()),
        Claim::Invalid => Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "nanty_action_invalid", Map::new())),
        Claim::Expired => Ok(Outcome::new("expired")),
        Claim::Already(who) => Ok(Outcome::already_decided(who)),
        Claim::Snoozed(card) => {
            journal_decision(card.action.as_deref(), card.project_id.as_deref());
            Ok(Outcome::new("snoozed"))
        }
        Claim::Declined(card) => {
            journal_decision(card.action.as_deref(), card.project_id.as_deref());
            Ok(Outcome::new("declined"))
        }
        Claim::PersonOnly => Ok(Outcome::new("person_only")),
        Claim::Link(a, card) => {
            let mut out = Outcome::new("open_nants").with("action", a.as_str());
            if let Some(p) = &card.project_id {
                out = out.with("project_id", p.as_str());
            }
            Ok(out)
        }
        Claim::Execute(card) => Ok(execute_card(state, &nanty, card, by).await),
    }
}

/// O "sim" de um cartão: confere o estado (a condição e a ação ainda valem) e executa.
async fn execute_card(state: &AppState, nanty: &Arc<Nanty>, card: Card, by: DecidedBy) -> Outcome {
    let cfg = state.config().read().await.clone();
    let snap = nanty.snapshot();
    let action = card.action.as_deref().and_then(actions::parse);
    // Pausar (T08) não nasce de uma condição: vale enquanto a mesma spec estiver em execução.
    let pause = (action == Some(ActionId::PauseRun)).then(|| pause_suggestion(state, &cfg, &card)).flatten();
    let current = pause.or_else(|| snap.alerts.get(&card.group).and_then(|alert| {
        let s = actions::suggest(alert, &cfg, &snap.retries)?;
        let valid = Some(s.action) == action
            && s.spec_id == card.spec_id
            && actions::level(&cfg.nanty, s.action).is_some_and(|l| l != ActionLevel::Off);
        valid.then_some(s)
    }));
    let Some(action) = action else { return Outcome::new("expired") };
    let job_of = |s: Suggestion| Job {
        suggestion: s,
        group: card.group.clone(),
        by,
        card_id: Some(card.id.clone()),
        plan_hash: card.plan_hash.clone(),
    };
    let Some(s) = current else {
        let stale = Suggestion {
            action,
            project_id: card.project_id.clone(),
            spec_id: card.spec_id.clone(),
            role: None,
            reserve: None,
            retry_key: None,
        };
        // Nada foi executado: a oferta reivindicada volta a valer se a condição pedir de novo.
        nanty.update(|st| {
            let mine = st.handled.get(&card.group).is_some_and(|o| o.action == action.as_str());
            if mine {
                st.handled.remove(&card.group);
            }
            mine
        });
        record(nanty, &job_of(stale), Done::expired("state_changed"));
        return Outcome::new("expired");
    };
    let now_ms = nanty.now().epoch_ms;
    if action == ActionId::RetryProvider {
        nanty.update(|st| {
            bump_retry(st, &s, now_ms);
            true
        });
    }
    let job = job_of(s);
    let done = execute(state, nanty, &job).await;
    let outcome = match done.result {
        "ok" => Outcome::new("done"),
        "expired" => Outcome::new("expired"),
        _ => {
            let mut out = Outcome::new("failed");
            for key in ["error", "error_params"] {
                if let Some(v) = done.params.get(key) {
                    out.params.insert(key.into(), v.clone());
                }
            }
            out
        }
    };
    let outcome = match &done.undo {
        Some(u) => outcome.with("undo_id", u.id.as_str()).with("undo_token", u.token.as_str()),
        None => outcome,
    };
    record(nanty, &job, done);
    outcome
}

/// O que a parte atômica do desfazer separou.
enum UndoClaim {
    BadToken,
    Expired,
    Already(DecidedBy),
    Proceed(UndoEntry),
}

/// Desfaz a troca de conexão (comparar e trocar): volta à anterior só se o valor atual ainda for o
/// que o Nanty gravou; senão, o cartão diz que a configuração mudou.
pub async fn undo(state: &AppState, undo_id: &str, token: &str, by: DecidedBy) -> Result<Outcome, ApiError> {
    let nanty = Arc::clone(state.nanty());
    if unavailable(&nanty) {
        return Ok(Outcome::new("unavailable"));
    }
    let now_ms = nanty.now().epoch_ms;
    let mut claim = UndoClaim::Expired;
    nanty.update(|st| {
        let Some(i) = st.undo.iter().position(|u| u.id == undo_id) else {
            claim = match st.decided.iter().rev().find(|d| d.card_id == undo_id) {
                Some(d) if d.token == token => UndoClaim::Already(d.decided_by),
                Some(_) => UndoClaim::BadToken,
                None => UndoClaim::Expired,
            };
            return false;
        };
        if st.undo[i].token != token {
            claim = UndoClaim::BadToken;
            return false;
        }
        let entry = st.undo.remove(i);
        for item in st.done.iter_mut().filter(|d| d.undo.as_ref().is_some_and(|u| u.id == undo_id)) {
            item.undo = None;
        }
        if entry.expires_at_ms <= now_ms {
            claim = UndoClaim::Expired;
            return true;
        }
        st.push_decided(DecidedCard {
            card_id: entry.id.clone(),
            token: token.to_string(),
            project_id: entry.project_id.clone(),
            decided_by: by,
            answer: "undo".into(),
            at_ms: now_ms,
        });
        claim = UndoClaim::Proceed(entry);
        true
    });
    let entry = match claim {
        UndoClaim::BadToken => return Err(not_found()),
        UndoClaim::Expired => return Ok(Outcome::new("expired")),
        UndoClaim::Already(who) => return Ok(Outcome::already_decided(who)),
        UndoClaim::Proceed(entry) => entry,
    };
    let expected = (entry.applied.connection_id.as_str(), entry.applied.model.as_str());
    let swapped = set_role_connection(
        state,
        &entry.role,
        &entry.previous.connection_id,
        &entry.previous.model,
        Some(expected),
    )
    .await;
    let (result, outcome) = match swapped {
        Ok(RoleSwap::Done { .. }) => ("undone", Outcome::new("undone")),
        Ok(RoleSwap::Changed { .. }) => ("config_changed", Outcome::new("config_changed")),
        Err(err) => {
            let mut out = Outcome::new("failed").with("error", err.code().unwrap_or("internal"));
            if let Some(p) = err.params() {
                out = out.with("error_params", Value::Object(p.clone()));
            }
            ("failed", out)
        }
    };
    let action = actions::parse(&entry.action).unwrap_or(ActionId::SwapReserve);
    let mut params = Map::new();
    params.insert("role".into(), json!(entry.role));
    let mut decision = entry_line(now_ms, JournalKind::Decision, "undo", &entry, action, by);
    decision.params.insert("undo_id".into(), json!(entry.id));
    let mut res = entry_line(now_ms, JournalKind::Result, result, &entry, action, by);
    res.params = params.clone();
    nanty.update(|st| {
        st.push_done(DoneItem {
            at_ms: now_ms,
            project_id: entry.project_id.clone(),
            code: "nanty.action.undo".into(),
            params,
            action: Some(action.as_str().to_string()),
            decided_by: Some(by),
            result: result.to_string(),
            card_id: None,
            undo: None,
        });
        true
    });
    nanty.record(&decision);
    nanty.record(&res);
    Ok(outcome)
}

fn entry_line(at_ms: u64, kind: JournalKind, code: &str, u: &UndoEntry, action: ActionId, by: DecidedBy) -> JournalEntry {
    let mut e = entry(at_ms, kind, code, u.project_id.as_deref(), action);
    e.decided_by = Some(by);
    e
}

// ---- Pedido na conversa (T08) ----

/// Grupo do cartão de pausar de um projeto (não é condição de observador).
pub fn pause_group(project_id: &str) -> String {
    format!("{project_id}:pause_run")
}

/// O pausar de um cartão: vale enquanto a mesma spec estiver em execução e a ação não estiver
/// desligada.
fn pause_suggestion(state: &AppState, cfg: &Config, card: &Card) -> Option<Suggestion> {
    let project = card.project_id.as_deref()?;
    let spec = card.spec_id.as_deref()?;
    let running = state.runs().active(project).is_some_and(|r| r.spec_id == spec);
    let on = actions::level(&cfg.nanty, ActionId::PauseRun).is_some_and(|l| l != ActionLevel::Off);
    (running && on && card.group == pause_group(project)).then(|| Suggestion {
        action: ActionId::PauseRun,
        project_id: Some(project.to_string()),
        spec_id: Some(spec.to_string()),
        role: None,
        reserve: None,
        retry_key: None,
    })
}

/// Uma ação que a pessoa pode pedir na conversa agora: o id, o grupo do cartão e o projeto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requestable {
    pub action: ActionId,
    pub group: String,
    pub project_id: Option<String>,
}

/// O que a conversa pode oferecer agora: as ações do catálogo que executam, para as condições
/// abertas (as do projeto citado e as da máquina; sem projeto citado, todas), fora as desligadas; e
/// pausar, para o projeto citado com execução em andamento. Opções do cartão, links e a faixa "só a
/// pessoa" nunca entram.
pub fn requestable(state: &AppState, cfg: &Config, st: &NantyState, project: Option<&str>) -> Vec<Requestable> {
    let mut out = Vec::new();
    for (group, alert) in &st.alerts {
        if project.is_some_and(|p| alert.project_id.as_deref().is_some_and(|other| other != p)) {
            continue;
        }
        let Some(s) = actions::suggest(alert, cfg, &st.retries) else { continue };
        let on = actions::level(&cfg.nanty, s.action).is_some_and(|l| l != ActionLevel::Off);
        if s.action.entry().kind == ActionKind::Executes && on {
            out.push(Requestable { action: s.action, group: group.clone(), project_id: alert.project_id.clone() });
        }
    }
    if let Some(p) = project {
        let on = actions::level(&cfg.nanty, ActionId::PauseRun).is_some_and(|l| l != ActionLevel::Off);
        if on && state.runs().active(p).is_some() {
            out.push(Requestable { action: ActionId::PauseRun, group: pause_group(p), project_id: Some(p.to_string()) });
        }
    }
    out
}

/// Por que o cartão pedido não saiu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoCard {
    /// A ação está desligada nas Regras.
    Off,
    /// A condição (ou a execução) não existe mais, ou a ação já foi feita com os mesmos fatos.
    Gone,
    /// Outra instância tem a pasta de dados.
    Unavailable,
}

/// Marca o cartão como pedido na conversa: pelo Telegram, ele volta ao chat logo (com som e sem a
/// espera da aba visível); pela janela, um cartão novo fica só nela.
fn mark_requested(card: &mut Card, channel: Channel, now_ms: u64, fresh: bool) {
    match channel {
        Channel::Telegram => {
            card.created_at_ms = now_ms;
            card.requested = Some(Channel::Telegram);
            card.delivery = Some(Delivery::Push);
        }
        Channel::Web if fresh => card.requested = Some(Channel::Web),
        Channel::Web => {}
    }
}

/// O cartão de confirmação de uma ação pedida na conversa (T08). Nunca executa nada, nem a ação
/// automática: a pessoa decide no cartão, com o estado conferido no clique ([`decide`]). Reaproveita
/// o cartão do grupo quando ele já oferece a mesma ação. Devolve o id do cartão.
pub async fn request_card(state: &AppState, req: &Requestable, channel: Channel) -> Result<String, NoCard> {
    let nanty = Arc::clone(state.nanty());
    if unavailable(&nanty) {
        return Err(NoCard::Unavailable);
    }
    let cfg = state.config().read().await.clone();
    if actions::level(&cfg.nanty, req.action).is_none_or(|l| l == ActionLevel::Off) {
        return Err(NoCard::Off);
    }
    let snap = nanty.snapshot();
    let now_ms = nanty.now().epoch_ms;
    // O que o cartão leva: a sugestão de agora (conferida de novo) e o cartão novo, se faltar.
    let (s, hash, fresh_card) = if req.action == ActionId::PauseRun {
        let project = req.project_id.clone().ok_or(NoCard::Gone)?;
        let run = state.runs().active(&project).ok_or(NoCard::Gone)?;
        let mut params = Map::new();
        params.insert("spec_id".into(), json!(run.spec_id));
        let card = Card {
            project_id: Some(project.clone()),
            group: pause_group(&project),
            code: "nanty.card.pause_run".into(),
            params,
            created_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(card_ttl(&cfg.nanty, false)),
            urgency: Some(Urgency::Normal),
            delivery: Some(Delivery::Push),
            ..Card::default()
        };
        let s = Suggestion {
            action: ActionId::PauseRun,
            project_id: Some(project),
            spec_id: Some(run.spec_id),
            role: None,
            reserve: None,
            retry_key: None,
        };
        (s, None, card)
    } else {
        let alert = snap.alerts.get(&req.group).ok_or(NoCard::Gone)?;
        let s = actions::suggest(alert, &cfg, &snap.retries).filter(|s| s.action == req.action).ok_or(NoCard::Gone)?;
        let kind = SignalKind::parse(&alert.kind).ok_or(NoCard::Gone)?;
        let hash = if s.action == ActionId::ApprovePlan {
            Some(plan_hash_now(&cfg, s.project_id.as_deref(), s.spec_id.as_deref()).await.ok_or(NoCard::Gone)?)
        } else {
            None
        };
        let offer =
            Offer { action: s.action.as_str().to_string(), fingerprint: actions::fingerprint(&alert.facts, hash.as_deref()) };
        if snap.handled.get(&req.group) == Some(&offer) {
            return Err(NoCard::Gone);
        }
        let urgency = urgency_of(kind, &cfg.nanty);
        let shown = if urgency == Urgency::Digest { Urgency::Normal } else { urgency };
        let card = Card {
            project_id: alert.project_id.clone(),
            group: req.group.clone(),
            code: kind.why_code(),
            params: offer_params(alert, kind, now_ms),
            created_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(card_ttl(&cfg.nanty, false)),
            urgency: Some(shown),
            delivery: Some(Delivery::Push),
            ..Card::default()
        };
        (s, hash, card)
    };
    let action = Some(s.action.as_str().to_string());
    let mut card_id = None;
    nanty.update(|st| {
        // O pedido explícito vale mais que o "não" de antes.
        if st.declined.get(&req.group).is_some_and(|o| o.action == s.action.as_str()) {
            st.declined.remove(&req.group);
        }
        if let Some(card) = st.cards.iter_mut().find(|c| c.group == req.group && c.expires_at_ms > now_ms) {
            let same = card.action == action && card.plan_hash == hash && card.spec_id == s.spec_id && card.token.is_some();
            if !same {
                // Oferta nova no cartão: id e token novos (o botão antigo nunca decide sobre ela).
                card.id = super::new_id();
                card.token = Some(new_token());
                card.created_at_ms = now_ms;
                card.action = action.clone();
                card.plan_hash = hash.clone();
                card.spec_id = s.spec_id.clone();
            }
            mark_requested(card, channel, now_ms, !same);
            card_id = Some(card.id.clone());
        } else {
            st.cards.retain(|c| c.group != req.group);
            let mut card = Card {
                id: super::new_id(),
                action: action.clone(),
                plan_hash: hash.clone(),
                spec_id: s.spec_id.clone(),
                token: Some(new_token()),
                ..fresh_card
            };
            mark_requested(&mut card, channel, now_ms, true);
            card_id = Some(card.id.clone());
            st.cards.push(card);
        }
        true
    });
    let mut e = entry(now_ms, JournalKind::Suggestion, s.action.as_str(), s.project_id.as_deref(), s.action);
    e.params.insert("group".into(), json!(req.group));
    e.params.insert("requested".into(), json!(channel));
    nanty.record(&e);
    card_id.ok_or(NoCard::Gone)
}
