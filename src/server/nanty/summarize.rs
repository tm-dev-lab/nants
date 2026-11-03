//! O resumo limpo da conversa (spec 140 T06, G3, C-03): o degrau da janela do histórico.
//!
//! - **Quando:** depois de cada resposta à pessoa, já entregue, e depois das falas proativas
//!   ([`after_reply`]). Fora da fila dos turnos ([`super::turn::TurnQueue`]): a próxima mensagem da
//!   pessoa nunca espera o resumo. Uma execução por vez ([`Running`]); a que chega com outra rodando
//!   não faz nada, e a seguinte retoma.
//! - **O degrau:** com `[nanty.chat] window_messages` (provisório: 60) mensagens fora do resumo, as
//!   `window_step_messages` (provisório: 20) mais antigas viram um item de uma vez, e a janela volta
//!   a 40 ([`super::history`]). Segue o contrato de [`super::summary`]: na ordem de `seq`, sem pular
//!   nenhuma (a leitura é da conversa inteira, todas as gerações), e só então a marca anda, nunca
//!   para trás e só se ninguém a moveu no meio. A leitura é estrita (revisão 1, F-01): com uma
//!   geração que existe e não dá para ler, o degrau não grava nada (nem item, nem linha do
//!   servidor, nem marca) e devolve `Failed("unreadable")`, com uma linha no registro só quando
//!   trava; quando a leitura volta, o degrau seguinte resume tudo, inclusive aquela geração.
//! - **Só texto limpo:** o modelo recebe o texto da pessoa e as respostas limpas do Nanty. A resposta
//!   contaminada entra como a linha do servidor ([`provenance::server_line`]), e o texto de terceiro
//!   (encaminhado ou citado) também; as palavras da pessoa ficam. As mensagens só de controle e as dos
//!   projetos que o Nanty não observa ficam de fora (C-16): a marca passa por elas, e nada delas entra
//!   no item.
//! - **A chamada:** sem ferramentas, com o JSON sem `<` nem `>` ([`tools::seal`]), com prazo e com
//!   teto do texto ([`MAX_TEXT_CHARS`]). O texto passa pelo redator ([`redact::scrub`]) antes de ser
//!   gravado. O item leva todos os projetos das mensagens que entraram (`project_ids`). Conta no
//!   orçamento do dia como os outros turnos, com a linha `turn.*` do registro.
//! - **Sem modelo ou sem orçamento:** o resumo fica desligado ([`sync`], que a rotação também
//!   respeita), e o degrau deixa a linha do servidor ([`SummarySource::Server`]), só com campos
//!   estruturados. A falha do modelo não move a marca: o próximo degrau tenta de novo.
//! - **Nunca alimenta a memória de longo prazo:** nada daqui chama ferramenta, e o texto só volta ao
//!   contexto como dado, depois do sistema.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use super::agent::{self, Spent};
use super::journal::{JournalEntry, JournalKind};
use super::provenance::{self, unique, Scoped};
use super::store::{self, ConversationMessage, Speaker, Summarizer};
use super::summary::{SummaryItem, SummarySource};
use super::turn::{self, TurnKind};
use super::{history, language, nanty_lang, redact, render, tools, Nanty};
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::{ChatMessage, ChatRequest, Role};
use crate::config::{Config, ConnectionKind};
use crate::i18n::Lang;
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Maior texto de um item do resumo, em caracteres.
pub const MAX_TEXT_CHARS: usize = 1200;

/// A vez do resumo: enquanto existir, outro resumo não começa.
#[derive(Debug)]
pub struct Running<'a>(&'a AtomicBool);

impl<'a> Running<'a> {
    /// A vez, se ninguém a tem.
    pub fn take(flag: &'a AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).ok().map(|_| Self(flag))
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// O que o degrau fez.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A janela ainda não chegou ao teto.
    Idle,
    /// Outro resumo está rodando.
    Busy,
    /// O modelo resumiu `messages` mensagens, e a marca andou.
    Model { messages: usize },
    /// Sem modelo ou sem orçamento: a linha do servidor cobre `messages` mensagens.
    Server { messages: usize },
    /// O degrau não terminou (o motivo em código): a marca ficou onde estava.
    Failed(&'static str),
}

/// Liga ou desliga o resumo pelo modelo e pelo orçamento do dia; devolve se está ligado.
pub fn sync(nanty: &Nanty, cfg: &Config) -> bool {
    let on = turn::can_run(cfg, nanty);
    nanty.set_summarizer(if on { Summarizer::Active } else { Summarizer::Off });
    on
}

/// O degrau depois da resposta (ou da fala proativa), com a configuração de agora.
pub async fn after_reply(state: &AppState, nanty: &Arc<Nanty>) -> Step {
    let cfg = state.config().read().await.clone();
    step(state, nanty, &cfg).await
}

/// As mensagens que o resumo ainda não cobriu, na ordem de `seq` (linha sem número nunca é coberta e
/// fica de fora do degrau).
fn uncovered(lines: Vec<ConversationMessage>, nanty: &Nanty) -> Vec<ConversationMessage> {
    let summary = nanty.summary();
    let mut out: Vec<ConversationMessage> = lines.into_iter().filter(|m| m.seq.is_some() && !summary.covers(m)).collect();
    out.sort_by_key(|m| m.seq);
    out
}

/// Um degrau, se a janela chegou ao teto.
pub async fn step(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config) -> Step {
    let Some(_running) = nanty.summary_turn() else { return Step::Busy };
    let on = sync(nanty, cfg);
    let window = cfg.nanty.chat.window();
    // A cauda basta para saber se chegou ao teto (ela tem mais que o maior teto); só então lê tudo.
    if uncovered(nanty.conversation(), nanty).len() < window {
        return Step::Idle;
    }
    let mark = nanty.summary().covered_through_seq;
    // Leitura estrita: com uma geração ilegível, o degrau não sabe o que pularia, e nada é gravado.
    let Ok(whole) = nanty.whole_conversation() else {
        if nanty.note_summary_blocked(true) {
            let mut entry = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Conversation, "conversation.summary_blocked");
            entry.params.insert("reason".into(), json!("unreadable"));
            nanty.record(&entry);
        }
        return Step::Failed("unreadable");
    };
    nanty.note_summary_blocked(false);
    let mut batch = uncovered(whole, nanty);
    if batch.len() < window {
        return Step::Idle;
    }
    batch.truncate(cfg.nanty.chat.window_step());
    let Some(through) = batch.iter().filter_map(|m| m.seq).max() else { return Step::Idle };
    let messages = batch.len();
    let item = if on {
        match model_item(state, nanty, cfg, &batch).await {
            Ok(item) => item,
            Err(reason) => return Step::Failed(reason),
        }
    } else {
        SummaryItem::server(super::new_id(), &batch, "summary_off")
    };
    let source = item.as_ref().map(|i| i.source);
    let moved = nanty.update_summary(|s| {
        // Só o degrau que leu esta marca a move, e sempre para a frente.
        if s.covered_through_seq != mark || mark.is_some_and(|m| through <= m) {
            return false;
        }
        match item {
            Some(item) if item.source == SummarySource::Server => store::add_server_item(s, item),
            Some(item) => s.items.push(item),
            None => {}
        }
        s.covered_through_seq = Some(through);
        true
    });
    match moved {
        Ok(true) if source == Some(SummarySource::Server) => Step::Server { messages },
        Ok(true) => Step::Model { messages },
        Ok(false) => Step::Failed("moved"),
        Err(e) => {
            nanty.summary_failed(Some(e));
            Step::Failed(e.code())
        }
    }
}

/// A mensagem como o modelo do resumo a recebe: só texto limpo. `None` para a só de controle e a de
/// projeto não observado.
fn clean_line(cfg: &Config, lang: Lang, m: &ConversationMessage) -> Option<Value> {
    if history::is_control(m) || !m.within(|id| cfg.nanty.observes(id)) {
        return None;
    }
    let at = history::stamp(m.at_ms);
    let own = language::tagged(m.lang.as_deref()).unwrap_or(lang);
    let cut = |t: &str| t.trim().chars().take(history::MAX_MESSAGE_CHARS).collect::<String>();
    let line = match m.from {
        Speaker::Person => {
            let relayed = m.relayed.as_ref();
            let text = if relayed.is_some_and(|r| r.forwarded) {
                provenance::relayed_line(own, &at)
            } else {
                cut(m.text.as_deref().unwrap_or_default())
            };
            let mut line = json!({ "at": at, "from": "person", "text": text });
            if relayed.is_some_and(|r| r.third_party && !r.forwarded) {
                line["quote"] = json!(provenance::relayed_line(own, &at));
            }
            line
        }
        Speaker::Nanty => {
            let text = if m.sources().is_empty() { cut(&render::message_text(own, m)) } else { provenance::server_line(own, &at, m) };
            json!({ "at": at, "from": "nanty", "text": text })
        }
    };
    Some(line)
}

/// O item do modelo para as mensagens do degrau. `Ok(None)` quando nenhuma delas tem o que resumir
/// (todas de controle ou de projeto não observado): a marca anda sem item.
async fn model_item(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, batch: &[ConversationMessage]) -> Result<Option<SummaryItem>, &'static str> {
    let Some((conn, model)) = turn::model_route(cfg).map(|(c, m)| (c.clone(), m.to_string())) else { return Err("no_model") };
    let lang = nanty_lang(&cfg.nanty, nanty);
    let included: Vec<&ConversationMessage> = batch.iter().filter(|m| clean_line(cfg, lang, m).is_some()).collect();
    let lines: Vec<Value> = included.iter().filter_map(|m| clean_line(cfg, lang, m)).collect();
    if lines.is_empty() {
        return Ok(None);
    }
    let backend: Arc<dyn ChatBackend> = match nanty.summary_backend() {
        Some(fake) => fake,
        None => match crate::server::chatgpt::plan_client::llm_client(state, &conn).and_then(|c| c.with_timeout(turn::TURN_TIMEOUT)) {
            Ok(client) => Arc::new(client),
            Err(_) => return Err("failed"),
        },
    };
    let secrets = SecretSet::from_config(cfg);
    let input = secrets.scrub_exact(&tools::seal(&json!({ "messages": lines }).to_string()));
    let request = ChatRequest {
        model: model.clone(),
        messages: vec![ChatMessage::text(Role::System, crate::agents::prompts::nanty_summary_system(lang, MAX_TEXT_CHARS)), ChatMessage::text(Role::User, input)],
        tools: Vec::new(),
        effort: Some(cfg.nanty.effort),
        response_json: false,
        web_search: false,
        web_search_max_uses: None,
        cache_key: None,
    };
    let mut spent = Spent::default();
    let response = match tokio::time::timeout(turn::TURN_TIMEOUT, backend.complete(&request)).await {
        Err(_) => {
            record(nanty, "turn.skipped", Some("deadline"), &spent);
            return Err("deadline");
        }
        Ok(Err(err)) => {
            let reason = crate::agents::llm_client::provider_failure(&err).reason_kind().map(|(r, _)| r);
            record(nanty, "turn.failed", reason, &spent);
            return Err("failed");
        }
        Ok(Ok(response)) => response,
    };
    agent::account(nanty, cfg, conn.kind == ConnectionKind::ChatgptPlan, &model, &request, &response, &mut spent, false);
    let text: String = redact::scrub(&secrets, response.message.content.trim()).trim().chars().take(MAX_TEXT_CHARS).collect();
    if text.trim().is_empty() {
        record(nanty, "turn.invalid", Some("empty"), &spent);
        return Err("invalid");
    }
    record(nanty, "turn.ok", None, &spent);
    let first = batch.iter().map(|m| m.at_ms).min().unwrap_or_default();
    let last = batch.iter().map(|m| m.at_ms).max().unwrap_or_default();
    let project_ids = unique(included.iter().flat_map(|m| m.projects().into_iter().map(str::to_string)).collect::<Vec<_>>().into_iter());
    Ok(Some(SummaryItem {
        id: super::new_id(),
        source: SummarySource::Model,
        from_ms: first,
        to_ms: last,
        messages: u32::try_from(batch.len()).unwrap_or(u32::MAX),
        project_ids,
        // O texto saiu só de texto limpo: a procedência do item é limpa.
        taint: provenance::Taint::default(),
        text: Some(text),
        lang: Some(lang.tag().to_string()),
        reason: None,
    }))
}

fn record(nanty: &Nanty, code: &str, reason: Option<&str>, spent: &Spent) {
    let mut entry = turn::turn_entry(nanty, code, TurnKind::ConversationSummary, reason);
    entry.cost_usd = spent.usd;
    entry.input_tokens = Some(spent.input);
    entry.output_tokens = Some(spent.output);
    nanty.record(&entry);
}

#[cfg(test)]
#[path = "summarize_tests.rs"]
mod tests;
