//! Catálogo de ações do Nanty (spec 138 T03, C-02): fixo, no código. O modelo (T05) e os comandos
//! (T08) só escolhem ids daqui; o servidor valida e confere o estado.
//!
//! - **Um nível por id do catálogo, nunca por handler:** desligada, pergunta antes ou automática
//!   (`[nanty.actions]`, ausente = o padrão do catálogo). Não há regras concorrentes.
//! - **Pausar (T08):** só a pedido da pessoa, na conversa, e sempre com o cartão de confirmação.
//! - **Valores iniciais:** aprovar o plano, retomar e pausar começam em "pergunta antes"; tentar de novo,
//!   trocar para a reserva (que já inclui a retomada) e limpar órfãos começam em "automática".
//! - **Opções do cartão, sem nível:** lembrar depois (silêncio com validade), abrir no Nants e
//!   aumentar o limite (o orçamento estourado é só sinal: o runner já pausou, C-01).
//! - **Faixa "só a pessoa":** propriedade do catálogo. Nunca vira automática e o Nanty nunca a
//!   executa: liberar um comando "sempre" e mudar canal, token ou destinatário.
//!
//! **Tabela do código de parada** ([`stop_rule`]): nova tentativa só para `run.provider_failed` e
//! `run.provider_failed_detail` (no máximo [`MAX_RETRIES`] por parada, com espera); troca para a
//! reserva só para `run.billing` e `run.spend_limit`; `run.plan_limit`, `run.daily_quota`,
//! `run.plan_needs_login` (e qualquer outro) geram só aviso.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use super::signals::SignalKind;
use super::store::{Alert, RetryCount, RouteValue};
use crate::config::{ActionLevel, Config, NantyConfig};

/// Id de uma ação do catálogo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionId {
    ApprovePlan,
    ResumeRun,
    RetryProvider,
    SwapReserve,
    CleanOrphans,
    /// Pausar a execução em andamento (T08): só pelo pedido da pessoa (o comando ou o texto livre),
    /// sempre com o cartão de confirmação; nenhum observador a sugere.
    PauseRun,
    RemindLater,
    OpenNants,
    RaiseLimit,
    AllowCommandAlways,
    ChangeChannel,
}

/// Como a ação existe no catálogo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    /// O Nanty executa, pelo handler que já existe, conforme o nível.
    Executes,
    /// Opção de qualquer cartão (lembrar depois): sem nível.
    CardOption,
    /// Um link que abre o Nants: sem nível, nada é executado.
    Link,
    /// Faixa "só a pessoa": nunca automática e nunca executada pelo Nanty.
    PersonOnly,
}

/// Uma entrada do catálogo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogEntry {
    pub id: ActionId,
    pub kind: ActionKind,
    /// Nível inicial (só vale para [`ActionKind::Executes`] e [`ActionKind::PersonOnly`]).
    pub default_level: ActionLevel,
    /// Há desfazer depois de feita.
    pub undoable: bool,
}

const fn entry_of(id: ActionId, kind: ActionKind, default_level: ActionLevel, undoable: bool) -> CatalogEntry {
    CatalogEntry { id, kind, default_level, undoable }
}

/// O catálogo inteiro. Nenhuma ação fora daqui.
pub const CATALOG: [CatalogEntry; 11] = [
    entry_of(ActionId::ApprovePlan, ActionKind::Executes, ActionLevel::Ask, false),
    entry_of(ActionId::ResumeRun, ActionKind::Executes, ActionLevel::Ask, false),
    entry_of(ActionId::RetryProvider, ActionKind::Executes, ActionLevel::Auto, false),
    entry_of(ActionId::SwapReserve, ActionKind::Executes, ActionLevel::Auto, true),
    entry_of(ActionId::CleanOrphans, ActionKind::Executes, ActionLevel::Auto, false),
    entry_of(ActionId::PauseRun, ActionKind::Executes, ActionLevel::Ask, false),
    entry_of(ActionId::RemindLater, ActionKind::CardOption, ActionLevel::Ask, false),
    entry_of(ActionId::OpenNants, ActionKind::Link, ActionLevel::Ask, false),
    entry_of(ActionId::RaiseLimit, ActionKind::Link, ActionLevel::Ask, false),
    entry_of(ActionId::AllowCommandAlways, ActionKind::PersonOnly, ActionLevel::Ask, false),
    entry_of(ActionId::ChangeChannel, ActionKind::PersonOnly, ActionLevel::Ask, false),
];

impl ActionId {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionId::ApprovePlan => "approve_plan",
            ActionId::ResumeRun => "resume_run",
            ActionId::RetryProvider => "retry_provider",
            ActionId::SwapReserve => "swap_reserve",
            ActionId::CleanOrphans => "clean_orphans",
            ActionId::PauseRun => "pause_run",
            ActionId::RemindLater => "remind_later",
            ActionId::OpenNants => "open_nants",
            ActionId::RaiseLimit => "raise_limit",
            ActionId::AllowCommandAlways => "allow_command_always",
            ActionId::ChangeChannel => "change_channel",
        }
    }

    /// A entrada do catálogo.
    pub fn entry(self) -> CatalogEntry {
        CATALOG.iter().copied().find(|e| e.id == self).unwrap_or(entry_of(self, ActionKind::Link, ActionLevel::Off, false))
    }

    /// Código do nome da ação no dicionário (`nanty.action.<id>`).
    pub fn label_code(self) -> String {
        format!("nanty.action.{}", self.as_str())
    }
}

/// Id do catálogo a partir do texto; `None` para qualquer coisa fora dele.
pub fn parse(raw: &str) -> Option<ActionId> {
    CATALOG.iter().map(|e| e.id).find(|id| id.as_str() == raw)
}

/// Nível efetivo de uma ação: o configurado por id, ou o inicial do catálogo. `None` para as opções
/// do cartão e os links (sem nível). A faixa "só a pessoa" nunca passa de "pergunta antes".
pub fn level(cfg: &NantyConfig, id: ActionId) -> Option<ActionLevel> {
    let entry = id.entry();
    let configured = cfg.actions.get(id.as_str()).copied().unwrap_or(entry.default_level);
    match entry.kind {
        ActionKind::CardOption | ActionKind::Link => None,
        ActionKind::Executes => Some(configured),
        ActionKind::PersonOnly => Some(match configured {
            ActionLevel::Auto => ActionLevel::Ask,
            other => other,
        }),
    }
}

/// O que um código de parada do provedor permite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopRule {
    /// Nova tentativa (falha passageira).
    Retry,
    /// Troca para a reserva e segue (crédito acabou).
    Swap,
    /// Só aviso.
    NoticeOnly,
}

/// Códigos de parada com nova tentativa.
pub const RETRY_CODES: [&str; 2] = ["run.provider_failed", "run.provider_failed_detail"];
/// Códigos de parada com troca para a reserva.
pub const SWAP_CODES: [&str; 2] = ["run.billing", "run.spend_limit"];
/// Códigos de parada que só geram aviso (o resto também).
pub const NOTICE_ONLY_CODES: [&str; 3] = ["run.plan_limit", "run.daily_quota", "run.plan_needs_login"];

/// A tabela do código de parada para ação.
pub fn stop_rule(code: &str) -> StopRule {
    if RETRY_CODES.contains(&code) {
        StopRule::Retry
    } else if SWAP_CODES.contains(&code) {
        StopRule::Swap
    } else {
        StopRule::NoticeOnly
    }
}

/// Novas tentativas por parada.
pub const MAX_RETRIES: u32 = 2;
/// Espera antes da primeira nova tentativa (provisória); a segunda espera o dobro.
pub const RETRY_BASE_WAIT_MS: u64 = 60_000;
/// Sem nova parada por este tempo, a contagem da parada recomeça (provisório).
pub const RETRY_RESET_MS: u64 = 6 * 60 * 60 * 1000;

/// Espera antes da nova tentativa número `done + 1`.
pub fn retry_wait_ms(done: u32) -> u64 {
    RETRY_BASE_WAIT_MS.saturating_mul(1u64 << done.min(8))
}

/// Chave da contagem de novas tentativas: `<projeto>:<spec>:<código>`.
pub fn retry_key(project_id: &str, spec_id: &str, stop: &str) -> String {
    format!("{project_id}:{spec_id}:{stop}")
}

/// Uma ação sugerida para uma condição, com o que a execução precisa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub action: ActionId,
    pub project_id: Option<String>,
    pub spec_id: Option<String>,
    /// Papel da troca para a reserva.
    pub role: Option<String>,
    /// A reserva escolhida para o papel.
    pub reserve: Option<RouteValue>,
    /// Chave da contagem de novas tentativas.
    pub retry_key: Option<String>,
}

fn fact<'a>(alert: &'a Alert, name: &str) -> Option<&'a str> {
    alert.facts.get(name).and_then(Value::as_str).filter(|v| !v.is_empty())
}

/// A ação do catálogo para uma condição aberta (função pura), antes do nível. `None` = só aviso.
pub fn suggest(alert: &Alert, cfg: &Config, retries: &BTreeMap<String, RetryCount>) -> Option<Suggestion> {
    let kind = SignalKind::parse(&alert.kind)?;
    let spec = fact(alert, "spec_id").map(str::to_string);
    let base = |action| Suggestion {
        action,
        project_id: alert.project_id.clone(),
        spec_id: spec.clone(),
        role: None,
        reserve: None,
        retry_key: None,
    };
    match kind {
        SignalKind::PlanWaiting => spec.is_some().then(|| base(ActionId::ApprovePlan)),
        SignalKind::RunBlocked => spec.is_some().then(|| base(ActionId::ResumeRun)),
        SignalKind::Budget => Some(base(ActionId::RaiseLimit)),
        SignalKind::DockerOrphans => Some(base(ActionId::CleanOrphans)),
        SignalKind::ProviderStopped | SignalKind::NoCredit => {
            let stop = fact(alert, "stop")?;
            let (project, spec_id) = (alert.project_id.as_deref()?, spec.as_deref()?);
            match stop_rule(stop) {
                StopRule::Retry => {
                    let key = retry_key(project, spec_id, stop);
                    let done = retries.get(&key).map_or(0, |r| r.count);
                    (done < MAX_RETRIES).then(|| Suggestion { retry_key: Some(key), ..base(ActionId::RetryProvider) })
                }
                StopRule::Swap => {
                    let role = fact(alert, "role")?;
                    let reserve = reserve_for(cfg, role)?;
                    Some(Suggestion {
                        role: Some(role.to_string()),
                        reserve: Some(reserve),
                        ..base(ActionId::SwapReserve)
                    })
                }
                StopRule::NoticeOnly => None,
            }
        }
        _ => None,
    }
}

/// A reserva escolhida para o papel, se existe, se a conexão está cadastrada e se o papel ainda não
/// está nela.
pub fn reserve_for(cfg: &Config, role: &str) -> Option<RouteValue> {
    let route = cfg.nanty.reserve.get(role)?;
    if route.connection_id.trim().is_empty() || route.model.trim().is_empty() {
        return None;
    }
    if !cfg.connections.iter().any(|c| c.id == route.connection_id) {
        return None;
    }
    let current = match role {
        "architect" => &cfg.agents.architect,
        "developer" => &cfg.agents.developer,
        "reviewer" => &cfg.agents.reviewer,
        _ => return None,
    };
    if current.connection_id == route.connection_id && current.model == route.model {
        return None;
    }
    Some(RouteValue { connection_id: route.connection_id.clone(), model: route.model.clone() })
}

/// Impressão da oferta: os fatos da condição (e o hash do plano, na aprovação). Depois de "não" ou
/// de feita, a mesma oferta só volta quando isto muda.
pub fn fingerprint(facts: &Map<String, Value>, plan_hash: Option<&str>) -> String {
    let facts = serde_json::to_string(facts).unwrap_or_default();
    format!("{facts}|{}", plan_hash.unwrap_or_default())
}
