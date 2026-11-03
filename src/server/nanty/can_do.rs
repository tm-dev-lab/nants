//! O que existe de verdade agora (spec 140 T03, G2, C-30), calculado pelo servidor por canal e turno. A
//! pessoa pode responder a uma oferta só com um "sim", e esse "sim" precisa ter efeito naquela resposta:
//! por isso os prompts só deixam oferecer o que está em `actions` e `conversation`
//! ([`crate::agents::prompts::NANTY_OFFER_RULE`]). O que está em `buttons` só se cumpre pelo toque.
//!
//! - **`buttons`:** o que os cartões do turno têm de botão ([`card_buttons`], o mesmo do teclado do
//!   Telegram e da janela). Sem token, nenhum. O modelo pode lembrar que o botão existe, mas não oferece
//!   fazer aquilo por ela: `remind_later` e `raise_limit` não têm ferramenta, e `propose_action` só
//!   aceita o que executa.
//! - **`actions`:** o que `propose_action` aceita agora ([`act::requestable`]), nos projetos observados.
//! - **`conversation`:** as ferramentas que a resposta dela pode acionar. `open_nants` só entra quando
//!   leva a algum lugar: na janela, sempre; no Telegram, com endereço ([`super::link::address`]).

use serde::Serialize;

use super::act;
use super::actions::{self, ActionId, ActionKind};
use super::store::{Card, Channel};
use super::{chat, tools, Nanty};
use crate::config::{Config, Urgency};
use crate::server::state::AppState;

/// Ações propostas levadas a um turno, no máximo.
const MAX_ACTIONS: usize = 20;

/// Uma ação que `propose_action` aceita agora.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Offer {
    pub action: ActionId,
    /// O projeto; sem ele, é da máquina.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// O que existe de verdade neste turno.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanDo {
    /// Onde a pessoa está.
    pub channel: Channel,
    pub buttons: Vec<ActionId>,
    pub actions: Vec<Offer>,
    pub conversation: Vec<&'static str>,
}

impl Default for CanDo {
    fn default() -> Self {
        Self { channel: Channel::Web, buttons: Vec::new(), actions: Vec::new(), conversation: Vec::new() }
    }
}

/// Os botões que um cartão tem: a mesma regra do teclado do Telegram, só com as ações. O "não" não é
/// oferta, e os botões do silêncio são do silêncio (`set_silence`, na conversa).
pub fn card_buttons(card: &Card) -> Vec<ActionId> {
    if card.token.is_none() {
        return Vec::new();
    }
    let action = card.action.as_deref().and_then(actions::parse);
    match action.map(|a| (a, a.entry().kind)) {
        // Fora do catálogo (os cartões da memória, spec 140 T07): o botão é só do toque.
        None if card.action.is_some() => Vec::new(),
        Some((_, ActionKind::Silence)) => Vec::new(),
        Some((a, ActionKind::Executes | ActionKind::Link)) => vec![a, ActionId::RemindLater],
        _ => vec![ActionId::RemindLater],
    }
}

/// O canal dos avisos, que vão aos dois: o Telegram, com o dono pareado; sem ele, a janela.
pub fn notice_channel(cfg: &Config) -> Channel {
    let tg = &cfg.nanty.telegram;
    if tg.chat_id.is_some() && tg.owner_id.is_some() {
        Channel::Telegram
    } else {
        Channel::Web
    }
}

/// O que `propose_action` aceita agora, nos projetos observados: as condições abertas e, onde há
/// execução em andamento, pausar. Sem repetição, até [`MAX_ACTIONS`].
fn requestable(state: &AppState, cfg: &Config, nanty: &Nanty) -> Vec<Offer> {
    let snap = nanty.snapshot();
    let mut scopes: Vec<Option<&str>> = vec![None];
    scopes.extend(chat::observed(cfg).into_iter().filter(|p| state.runs().active(&p.id).is_some()).map(|p| Some(p.id.as_str())));
    let mut out: Vec<Offer> = Vec::new();
    for scope in scopes {
        for item in act::requestable(state, cfg, &snap, scope) {
            let offer = Offer { action: item.action, project_id: item.project_id };
            let observed = offer.project_id.as_deref().is_none_or(|p| cfg.nanty.observes(p));
            if observed && !out.contains(&offer) {
                out.push(offer);
            }
        }
    }
    out.truncate(MAX_ACTIONS);
    out
}

/// As ferramentas que a resposta da pessoa pode acionar neste canal.
fn conversation(state: &AppState, cfg: &Config, channel: Channel) -> Vec<&'static str> {
    let mut out = vec![
        tools::PROJECT_STATUS,
        tools::PROJECT_DETAILS,
        super::deep::AGENT_LOG,
        super::deep::SDD_DOCS,
        super::deep::PROJECT_FILES,
        super::deep::GIT_HISTORY,
        tools::SYSTEM_STATUS,
        tools::NANTY_ACTIVITY,
        tools::SET_SILENCE,
        tools::REMEMBER,
        tools::RECALL,
        tools::FORGET,
        // Spec 140 T15: o cartão de levar ao Arquiteto (o toque só deixa o rascunho).
        tools::PROPOSE_TO_ARCHITECT,
    ];
    if channel == Channel::Web || super::link::address(state, cfg).is_some() {
        out.push(tools::OPEN_NANTS);
    }
    out
}

impl CanDo {
    /// Para os cartões deste turno.
    pub fn of(state: &AppState, cfg: &Config, nanty: &Nanty, channel: Channel, cards: &[&Card]) -> Self {
        let mut buttons: Vec<ActionId> = Vec::new();
        for id in cards.iter().flat_map(|c| card_buttons(c)) {
            if !buttons.contains(&id) {
                buttons.push(id);
            }
        }
        Self { channel, buttons, actions: requestable(state, cfg, nanty), conversation: conversation(state, cfg, channel) }
    }

    /// Para os cartões abertos agora (a conversa, os atalhos e o resumo da manhã): os mesmos que a
    /// conversa recebe como avisos.
    pub fn now(state: &AppState, cfg: &Config, nanty: &Nanty, channel: Channel) -> Self {
        let now_ms = nanty.now().epoch_ms;
        let snap = nanty.snapshot();
        let open: Vec<&Card> = snap
            .cards
            .iter()
            .filter(|c| {
                c.expires_at_ms > now_ms
                    && c.urgency != Some(Urgency::Digest)
                    && c.awaits_person()
                    && c.project_id.as_deref().is_none_or(|p| cfg.nanty.observes(p))
            })
            .collect();
        Self::of(state, cfg, nanty, channel, &open)
    }
}

#[cfg(test)]
#[path = "can_do_tests.rs"]
mod tests;
