//! `deep_think`, o modo parceiro sem detecção por palavras (spec 140 T13, G7, C-25 e C-19).
//!
//! O modelo decide quando a pergunta pede reflexão de verdade e chama `deep_think`. O servidor não
//! procura palavras na mensagem nem troca de modelo: o pedido só sobe o esforço.
//!
//! - **Esforço maior:** um nível acima do configurado em `[nanty] effort` ([`raise`]), com teto no
//!   `high`, que é o máximo que os adaptadores mandam. O mesmo campo `ChatRequest::effort` vira o
//!   `reasoning_effort` (Chat Completions), o `reasoning.effort` (Responses e plano ChatGPT), o
//!   `output_config.effort` com o raciocínio adaptativo (Anthropic) e o `thinking_level` (Gemini).
//! - **Refaz o turno sem perder nada:** as rodadas seguintes da mesma mensagem vão com o esforço
//!   maior, sobre as mesmas mensagens: o contexto, a pergunta e as leituras já feitas, com os
//!   resultados, e nada é lido de novo. A procedência é a da [`super::agent::Session`], que a rodada
//!   nova não zera: um turno contaminado continua contaminado. Mesmo modelo, mesma conexão e mesmo
//!   `cache_key`.
//! - **Uma vez por mensagem da pessoa:** o desfecho fica na sessão, e o segundo pedido é recusado.
//! - **Recusa sem erro para a pessoa:** já no máximo, conexão ou modelo que não aceita o esforço (o
//!   adaptador recuou nesta mensagem ou numa rodada maior anterior deste processo), o teto de tokens da
//!   mensagem ou o orçamento do dia que não cobre a rodada. A ferramenta diz o motivo ao modelo, o
//!   código vai ao registro do turno (`deep_think`) e o turno segue no esforço normal.
//! - **Contabilidade (C-19):** a rodada maior passa pelo mesmo [`super::agent::account`], então conta
//!   no orçamento da conversa, em US$ ou em tokens (o raciocínio já vem dentro da saída). Antes de
//!   aceitar, a estimativa da rodada (a entrada atual e a saída reservada ao nível, [`reserve`]) precisa
//!   caber no `max_tokens_per_message` e no que resta do dia.
//! - **Só na conversa:** a ferramenta não existe no modo de análise ([`super::tools::Mode`]).

use super::agent::Session;
use super::{turn, Nanty};
use crate::agents::llm_client::{Billing, ChatMessage, ToolSpec, Usage};
use crate::config::{Config, Effort};
use crate::i18n::Lang;

/// O nome da ferramenta.
pub const DEEP_THINK: &str = "deep_think";

/// Um nível acima, com teto no máximo: `None` quando já está no `high`.
pub fn raise(effort: Effort) -> Option<Effort> {
    match effort {
        Effort::Low => Some(Effort::Medium),
        Effort::Medium => Some(Effort::High),
        Effort::High => None,
    }
}

/// A saída reservada a uma rodada no nível, em tokens: a mesma escada do orçamento de raciocínio da
/// Anthropic (2.048, 8.192 e 16.384), usada como o pior caso de qualquer adaptador.
pub fn reserve(effort: Effort) -> u64 {
    match effort {
        Effort::Low => 2_048,
        Effort::Medium => 8_192,
        Effort::High => 16_384,
    }
}

/// O texto do nível, como vai ao pedido.
fn level(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    }
}

/// Por que o pedido não foi aceito.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Já pedido nesta mensagem.
    Repeated,
    /// O esforço configurado já é o máximo.
    Max,
    /// A conexão ou o modelo não aceita escolher o esforço.
    Unsupported,
    /// A rodada maior passaria do teto de tokens da mensagem.
    Message,
    /// O orçamento do dia não cobre a rodada maior.
    Budget,
}

impl Refusal {
    /// O código, o do registro e o do resultado da ferramenta.
    pub fn code(self) -> &'static str {
        match self {
            Refusal::Repeated => "repeated",
            Refusal::Max => "max",
            Refusal::Unsupported => "unsupported",
            Refusal::Message => "message",
            Refusal::Budget => "budget",
        }
    }
}

/// O desfecho do pedido numa mensagem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Aceito: as rodadas seguintes vão neste nível.
    Raised(Effort),
    /// Recusado antes da rodada.
    Refused(Refusal),
    /// Aceito, e o adaptador recuou na rodada maior (o provedor recusou o nível): o turno voltou ao
    /// esforço normal.
    Rejected,
}

impl Outcome {
    /// O código que vai ao registro do turno.
    pub fn code(self) -> &'static str {
        match self {
            Outcome::Raised(_) => "raised",
            Outcome::Refused(r) => r.code(),
            Outcome::Rejected => "rejected",
        }
    }
}

/// O que o laço sabe da rodada em curso, posto antes das ferramentas dela.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Round {
    pub connection_id: String,
    pub model: String,
    /// A conexão é pelo plano ChatGPT: o gasto conta em tokens.
    pub plan: bool,
    /// O esforço configurado.
    pub effort: Effort,
    /// A estimativa da entrada da próxima rodada (as mensagens e as ferramentas).
    pub input_tokens: u64,
    /// O que a mensagem já gastou.
    pub spent_tokens: u64,
    /// O `max_tokens_per_message`.
    pub max_tokens: u64,
    /// O adaptador recuou do esforço nesta mensagem, ou numa rodada maior deste processo.
    pub rejected: bool,
}

/// O estado do `deep_think` numa mensagem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Think {
    pub round: Option<Round>,
    /// O primeiro desfecho da mensagem; `None` nunca pedido.
    pub outcome: Option<Outcome>,
}

/// A chave da lembrança de recuo deste processo.
pub fn rejection_key(connection_id: &str, model: &str) -> String {
    format!("{connection_id}\n{model}")
}

/// A estimativa da entrada de uma rodada: as mensagens e as ferramentas, pela regra de 4 caracteres
/// por token.
pub fn estimate_input(messages: &[ChatMessage], tools: &[ToolSpec]) -> u64 {
    let text: u64 = messages.iter().map(|m| u64::from(turn::estimate_tokens(&m.content))).sum();
    let specs: u64 = tools
        .iter()
        .map(|t| u64::from(turn::estimate_tokens(&format!("{} {} {}", t.name, t.description, t.parameters))))
        .sum();
    text.saturating_add(specs)
}

/// Decide o pedido, sem efeito: o nível aceito ou o motivo da recusa.
pub fn decide(nanty: &Nanty, cfg: &Config, think: &Think) -> Result<Effort, Refusal> {
    if think.outcome.is_some() {
        return Err(Refusal::Repeated);
    }
    // Sem a rodada, o laço não passou por aqui: nada a subir.
    let Some(round) = &think.round else {
        return Err(Refusal::Unsupported);
    };
    let Some(raised) = raise(round.effort) else {
        return Err(Refusal::Max);
    };
    if round.rejected {
        return Err(Refusal::Unsupported);
    }
    let output = reserve(raised);
    let cost = round.input_tokens.saturating_add(output);
    if round.spent_tokens.saturating_add(cost) > round.max_tokens {
        return Err(Refusal::Message);
    }
    let spend = nanty.snapshot().spend_on(&nanty.now().day_key());
    let view = turn::budget_view(cfg, &spend);
    let fits = match view.mode {
        turn::BudgetMode::Usd => {
            let usage = Usage::new(u32::try_from(round.input_tokens).unwrap_or(u32::MAX), u32::try_from(output).unwrap_or(u32::MAX));
            let billing = if round.plan { Billing::ChatgptPlan } else { Billing::Metered };
            // Sem preço, a estimativa não tem como caber em US$: recusa.
            turn::turn_cost(cfg, &round.model, &usage, billing).is_some_and(|usd| view.spent_usd + usd <= view.budget_usd)
        }
        turn::BudgetMode::Tokens => view.tokens.saturating_add(cost) <= view.budget_tokens,
    };
    if !fits {
        return Err(Refusal::Budget);
    }
    Ok(raised)
}

/// A ferramenta: decide, guarda o desfecho na sessão e devolve o texto para o modelo.
pub fn call(nanty: &Nanty, cfg: &Config, lang: Lang, session: &mut Session) -> String {
    match decide(nanty, cfg, &session.think) {
        Ok(raised) => {
            session.think.outcome = Some(Outcome::Raised(raised));
            crate::i18n::tr(lang, "nanty.tool.deep_think.raised", &[("effort", level(raised))])
        }
        Err(refusal) => {
            session.think.outcome.get_or_insert(Outcome::Refused(refusal));
            crate::i18n::tr(lang, &format!("nanty.tool.deep_think.{}", refusal.code()), &[])
        }
    }
}

#[cfg(test)]
#[path = "think_tests.rs"]
mod tests;
