//! O histórico da conversa do Nanty como mensagens de verdade (spec 140 T06, G3, C-17 e C-18).
//!
//! - **Ordem do contexto (C-17):** sistema, ferramentas, o índice da memória de longo prazo
//!   ([`super::memory::index`], spec 140 T07), o resumo, o histórico e, por último, a mensagem com `now`,
//!   os projetos, os avisos e o texto da pessoa ([`super::agent::input`]). O que muda a cada mensagem
//!   fica no fim, e o começo só muda no degrau da janela: o cache de prefixo do provedor vale.
//! - **Janela em degraus, sem limite de tempo:** entram as mensagens que o resumo ainda não cobriu
//!   ([`Summary::covers`]), até `[nanty.chat] window_messages` (provisório: 60). Ao chegar ao teto, as
//!   `window_step_messages` (provisório: 20) mais antigas viram resumo de uma vez
//!   ([`super::summarize`]), e a janela volta a 40. O teto de tokens (`history_max_tokens`,
//!   provisório: 12 mil, pela estimativa de [`turn::estimate_tokens`]) vale sobre o número: passou,
//!   saem as mais antigas.
//! - **Quarentena (spec 140 T04, C-02) dentro da janela:** o texto contaminado fica inteiro só nas
//!   últimas `quarantine_messages`; antes delas, vira a linha do servidor ([`provenance`]).
//! - **Cada mensagem no próprio idioma, com data e hora:** o texto fixo do Nanty sai no `lang`
//!   gravado nela, e toda mensagem leva `AAAA-MM-DD HH:MM` ([`stamp`]): a da pessoa em `at`, a do
//!   Nanty entre colchetes no começo. A resposta do modelo perde esse começo se o imitar
//!   ([`strip_stamp`]).
//! - **Resposta longa (spec 140 T14, C-27):** a conversa guarda a resposta inteira, e o histórico
//!   leva dela o começo e o fim, com a marca do servidor no lugar do meio ([`trim_reply`]).
//! - **O que a pessoa viu (spec 141 T06, C-07, [`notice`]):** a mensagem do Nanty sobre um aviso abre
//!   com o cabeçalho de fatos do servidor, que fica mesmo com o texto em quarentena; a decisão, a
//!   resolução e a expiração entram como eventos, do lado da pessoa e como dado; a linha do Nanty sem
//!   procedência (de antes da 140) sai da janela; e o `focus` vai no fim, em `last`.
//! - **Normalização (C-18, [`normalize`]):** uma função pura que tira as mensagens só de controle,
//!   reduz o aviso fixo a uma linha, junta as falas seguidas do mesmo papel e garante que a lista
//!   começa pela pessoa. O resultado alterna os papéis e não tem `provider_items` nem chamadas de
//!   ferramenta, o que serve aos quatro formatos (Chat Completions, Responses e o plano ChatGPT,
//!   Anthropic e Gemini). O envelope do texto de terceiro fica no conteúdo da mensagem histórica.

use serde_json::{json, Value};

use super::provenance::{self, Scoped, Taint};
use super::store::{ConversationMessage, Speaker};
use super::summary::{self, Summary, SummarySource};
use super::{agent, chat, language, notice, render, tools, turn, Nanty};
use crate::agents::llm_client::{ChatMessage, Role};
use crate::config::Config;
use crate::i18n::Lang;

/// Teto de cada mensagem do histórico, em caracteres.
pub const MAX_MESSAGE_CHARS: usize = 1500;
/// A resposta longa do Nanty no histórico (spec 140 T14, C-27): o começo e o fim que ficam, em
/// caracteres, com a marca do servidor no lugar do meio ([`trim_reply`]).
pub const TRIM_HEAD_CHARS: usize = 1000;
pub const TRIM_TAIL_CHARS: usize = 400;

/// Os textos fixos que não dizem nada da conversa: a ajuda, o comando desconhecido e as recusas do
/// laço (sem modelo, sem orçamento, fila cheia ou falha).
const CONTROL_CODES: [&str; 7] = [
    "nanty.chat.help",
    "nanty.chat.help_no_model",
    "nanty.chat.unknown_command",
    "nanty.chat.busy",
    "nanty.chat.budget",
    "nanty.chat.no_model",
    "nanty.chat.failed",
];

/// Data e hora locais `AAAA-MM-DD HH:MM` de um instante.
pub fn stamp(at_ms: u64) -> String {
    super::clock::local_time_at(at_ms)
        .map(|t| format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute))
        .unwrap_or_default()
}

/// O começo `[AAAA-MM-DD HH:MM ...]` que o modelo copiou do histórico, fora da resposta, e as
/// primeiras linhas que imitam o dado do servidor (`{"server_facts"` ou `{"server_event"`, spec 141
/// T06, revisão 1, F-01), com ou sem o carimbo: só o servidor escreve esse dado.
pub fn strip_stamp(text: &str) -> String {
    let trimmed = text.trim_start();
    if starts_server_data(trimmed) {
        return strip_server_lines(trimmed);
    }
    let Some(rest) = trimmed.strip_prefix('[') else { return text.to_string() };
    let Some(end) = rest.find(']') else { return text.to_string() };
    let head = rest[..end].as_bytes();
    let digit = |i: usize| head.get(i).is_some_and(u8::is_ascii_digit);
    let shaped = (0..4).all(digit)
        && head.get(4) == Some(&b'-')
        && (5..7).all(digit)
        && head.get(7) == Some(&b'-')
        && (8..10).all(digit)
        && head.get(10) == Some(&b' ')
        && (11..13).all(digit)
        && head.get(13) == Some(&b':')
        && (14..16).all(digit);
    if !shaped {
        return text.to_string();
    }
    // Spec 141 T06: o cabeçalho de fatos copiado também sai; ele é do servidor.
    strip_server_lines(rest[end + 1..].trim_start())
}

/// As chaves que só o servidor escreve ([`notice::header`] e [`notice::event_entry`]).
const SERVER_KEYS: [&str; 2] = ["\"server_facts\"", "\"server_event\""];

/// O texto abre com o dado do servidor: `{` e uma das [`SERVER_KEYS`], com ou sem espaço entre eles.
fn starts_server_data(text: &str) -> bool {
    text.strip_prefix('{').is_some_and(|rest| SERVER_KEYS.iter().any(|k| rest.trim_start().starts_with(k)))
}

/// Tira as primeiras linhas que imitam o dado do servidor.
fn strip_server_lines(text: &str) -> String {
    let mut rest = text.trim_start();
    while starts_server_data(rest) {
        rest = rest.find('\n').map_or("", |nl| rest[nl + 1..].trim_start());
    }
    rest.to_string()
}

/// O texto do modelo no histórico não carrega as chaves do servidor (spec 141 T06, revisão 1, F-01):
/// uma imitação, copiada de um arquivo lido ou escrita pelo modelo, nunca se passa pelo cabeçalho de
/// fatos nem por um evento. O nome da chave vira outro, que não é dado.
pub fn neutralize_server_keys(text: &str) -> String {
    text.replace("server_facts", "model_facts_text").replace("server_event", "model_event_text")
}

/// Uma mensagem do histórico antes da normalização.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub role: Role,
    pub content: String,
    /// Só de controle (um comando, a ajuda, uma recusa, a nota sem texto): sai.
    pub control: bool,
    /// Aviso fixo do servidor: fica só a primeira linha do texto.
    pub fixed: bool,
    /// Bytes do começo do `content` que são do servidor (a data, os cartões e o cabeçalho de fatos,
    /// spec 141 T06): ficam inteiros mesmo no aviso fixo.
    pub head_len: usize,
}

impl Entry {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self { role, content: content.into(), control: false, fixed: false, head_len: 0 }
    }
}

/// Normaliza o contexto depois do sistema (C-18), sem efeito colateral: `lead` (os dados antes do
/// histórico, como o resumo), o histórico e a última mensagem da pessoa (`last`). Tira as mensagens só
/// de controle e as vazias, reduz o aviso fixo à primeira linha, junta as falas seguidas do mesmo
/// papel e, se a lista abriria pelo Nanty, põe antes o resumo vazio ([`EMPTY_SUMMARY`]). Sai uma lista
/// que começa e termina pela pessoa e alterna os papéis, sem `provider_items` nem chamadas de
/// ferramenta.
pub fn normalize(lead: Vec<String>, entries: Vec<Entry>, last: String) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::new();
    let mut push = |role: Role, content: String| {
        if content.trim().is_empty() {
            return;
        }
        match out.last_mut() {
            Some(prev) if prev.role == role => {
                prev.content.push_str("\n\n");
                prev.content.push_str(&content);
            }
            _ => out.push(ChatMessage::text(role, content)),
        }
    };
    for data in lead {
        push(Role::User, data);
    }
    for entry in entries.into_iter().filter(|e| !e.control) {
        let content = if entry.fixed {
            let at = if entry.content.is_char_boundary(entry.head_len) { entry.head_len } else { 0 };
            let (head, body) = entry.content.split_at(at);
            format!("{head}{}", body.lines().find(|l| !l.trim().is_empty()).unwrap_or_default())
        } else {
            entry.content
        };
        push(entry.role, content);
    }
    push(Role::User, last);
    // A janela que começa pelo Nanty (os avisos antes de ela escrever) abre com o resumo vazio, um
    // dado do servidor: nada se perde e a lista começa pela pessoa.
    if out.first().is_some_and(|m| m.role != Role::User) {
        out.insert(0, ChatMessage::text(Role::User, EMPTY_SUMMARY));
    }
    out
}

/// O bloco do resumo sem itens: abre o histórico que começa pelo Nanty.
pub const EMPTY_SUMMARY: &str = "{\"conversation_summary\":[]}";

/// O texto cortado no teto.
fn cut(text: &str) -> String {
    text.trim().chars().take(MAX_MESSAGE_CHARS).collect()
}

/// A resposta do Nanty como entra no histórico (spec 140 T14, C-27). A conversa guarda a resposta
/// inteira, mas reenviar o texto longo a cada mensagem custaria caro e tiraria do teto de tokens as
/// mensagens mais antigas. Acima de [`MAX_MESSAGE_CHARS`], ficam o começo, que o prompt pede que
/// traga o essencial, e o fim, com a conclusão e a frase do servidor, e o meio vira a marca do
/// servidor (`nanty.history.trimmed`), no idioma da mensagem. Os cortes caem em espaço.
pub fn trim_reply(lang: Lang, text: &str) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= MAX_MESSAGE_CHARS {
        return text.to_string();
    }
    let at = |n: usize| text.char_indices().nth(n).map_or(text.len(), |(i, _)| i);
    let head_end = at(TRIM_HEAD_CHARS);
    let head = &text[..head_end];
    let head = head.rfind(char::is_whitespace).filter(|p| *p >= head_end / 2).map_or(head, |p| &head[..p]);
    let tail = &text[at(count - TRIM_TAIL_CHARS)..];
    let tail = tail.find(char::is_whitespace).filter(|p| *p <= tail.len() / 2).map_or(tail, |p| &tail[p..]);
    format!("{}\n{}\n{}", head.trim_end(), crate::i18n::tr(lang, "nanty.history.trimmed", &[]), tail.trim_start())
}

/// O idioma gravado na mensagem; sem ele, `lang`.
fn own_lang(lang: Lang, m: &ConversationMessage) -> Lang {
    language::tagged(m.lang.as_deref()).unwrap_or(lang)
}

/// O texto do modelo na mensagem do Nanty, quando há.
fn model_text(m: &ConversationMessage) -> Option<&str> {
    m.text.as_deref().filter(|t| !t.trim().is_empty())
}

/// Só de controle: o comando da pessoa (a resposta do servidor já diz o que houve), a nota sem
/// texto, e do Nanty a ajuda, o comando desconhecido e as recusas do laço, sem texto do modelo.
pub fn is_control(m: &ConversationMessage) -> bool {
    match m.from {
        Speaker::Person => {
            let text = m.text.as_deref().unwrap_or_default();
            let forwarded = m.relayed.as_ref().is_some_and(|r| r.forwarded);
            text.trim().is_empty() || (!forwarded && chat::parse(text) != chat::Parsed::Text)
        }
        Speaker::Nanty => {
            model_text(m).is_none() && m.code.as_deref().is_none_or(|code| CONTROL_CODES.contains(&code))
        }
    }
}

/// O aviso fixo: do Nanty, só com o texto do dicionário, sem responder à pessoa.
fn is_fixed_notice(m: &ConversationMessage) -> bool {
    m.from == Speaker::Nanty && model_text(m).is_none() && m.reply_to.is_none()
}

/// A mensagem como entra no histórico, sem os nomes dos projetos ([`entry_with`]).
pub fn entry(lang: Lang, m: &ConversationMessage, keeps: bool) -> Entry {
    entry_with(lang, m, keeps, &notice::Names::new())
}

/// A mensagem como entra no histórico. `keeps`: o texto fica inteiro ([`provenance::keeps_text`]);
/// sem ele, a linha do servidor. Da pessoa, um JSON com `at`, o texto, o de terceiro no envelope e,
/// se ela respondeu a um aviso no Telegram, os fatos dele (`replied_to`); do Nanty, a data e a hora
/// entre colchetes (com os cartões citados), o cabeçalho de fatos do servidor ([`notice::header`],
/// spec 141 T06, que fica mesmo com o texto em quarentena) e o texto no idioma dela. O evento de
/// cartão é dado do servidor, do lado da pessoa ([`notice::event_entry`]).
pub fn entry_with(lang: Lang, m: &ConversationMessage, keeps: bool, names: &notice::Names) -> Entry {
    let at = stamp(m.at_ms);
    let own = own_lang(lang, m);
    if notice::is_event(m) {
        return Entry::new(Role::User, notice::event_entry(&at, m, names));
    }
    let mut head_len = 0;
    let content = match m.from {
        Speaker::Person => {
            let text = cut(m.text.as_deref().unwrap_or_default());
            let mut item = json!({ "at": at, "text": text });
            if keeps {
                agent::relay_fields(&mut item, "text", m.relayed.as_ref(), &text, MAX_MESSAGE_CHARS);
            } else {
                agent::quarantine_relayed(&mut item, own, &at, m.relayed.as_ref());
            }
            if !m.notices.is_empty() {
                item["replied_to"] = notice::facts_value(names, &m.notices);
            }
            serde_json::to_string(&item).unwrap_or_default()
        }
        Speaker::Nanty => {
            let text = if keeps { neutralize_server_keys(&trim_reply(own, &render::message_text(own, m))) } else { provenance::server_line(own, &at, m) };
            let cards: Vec<&str> = m.card_ids.iter().map(String::as_str).filter(|id| turn::is_code(id)).collect();
            let stamp = if cards.is_empty() { format!("[{at}]") } else { format!("[{at} card_ids: {}]", cards.join(", ")) };
            let head = match notice::header(names, &m.notices) {
                Some(facts) => format!("{stamp} {facts}\n"),
                None => format!("{stamp} "),
            };
            head_len = head.len();
            format!("{head}{text}")
        }
    };
    let role = if m.from == Speaker::Person { Role::User } else { Role::Assistant };
    Entry { role, content, control: is_control(m), fixed: is_fixed_notice(m), head_len }
}

/// As mensagens ainda fora do resumo, antes da mensagem atual, sem as dos projetos que o Nanty não
/// observa (C-16), da mais velha para a mais nova.
pub fn pending(nanty: &Nanty, cfg: &Config, person: &ConversationMessage, summary: &Summary) -> Vec<ConversationMessage> {
    let all = nanty.conversation();
    let end = all.iter().position(|m| m.id == person.id).unwrap_or(all.len());
    all[..end]
        .iter()
        .filter(|m| !summary.covers(m))
        .filter(|m| m.within(|id| cfg.nanty.observes(id)))
        .cloned()
        .collect()
}

/// O resumo como dado (C-03): os itens dos projetos observados, os mais recentes que cabem em
/// [`summary::MAX_CONTEXT_CHARS`], num JSON sem `<` nem `>` ([`tools::seal`]). O texto do modelo como
/// foi gravado; a linha do servidor no idioma dela ou no da conversa. `None` sem item.
pub fn summary_block(lang: Lang, cfg: &Config, summary: &Summary) -> Option<String> {
    let mut items: Vec<Value> = Vec::new();
    let mut used = 0usize;
    for item in summary.items.iter().rev().filter(|i| i.within(|id| cfg.nanty.observes(id))) {
        let (from, to) = (stamp(item.from_ms), stamp(item.to_ms));
        let text = match (item.source, item.text.as_deref()) {
            (SummarySource::Model, Some(text)) => text.to_string(),
            _ => summary::server_line(language::tagged(item.lang.as_deref()).unwrap_or(lang), &from, &to, item),
        };
        used = used.saturating_add(text.chars().count());
        if used > summary::MAX_CONTEXT_CHARS && !items.is_empty() {
            break;
        }
        items.push(json!({ "from": from, "to": to, "text": text }));
    }
    if items.is_empty() {
        return None;
    }
    items.reverse();
    Some(tools::seal(&json!({ "conversation_summary": items }).to_string()))
}


/// O contexto depois do sistema e o que ele traz de não confiável.
#[derive(Debug, Clone)]
pub struct Context {
    pub messages: Vec<ChatMessage>,
    pub taint: Taint,
}

/// A linha do Nanty de antes da procedência (spec 140): sai da janela (spec 141 T06, C-07). O evento
/// e a mensagem de agora sempre gravam a procedência.
fn without_provenance(m: &ConversationMessage) -> bool {
    m.from == Speaker::Nanty && m.taint.is_none()
}

/// Monta o contexto da conversa depois do sistema: o índice da memória, o resumo, o histórico da
/// janela e, por último, `last` (a mensagem com `now`, os projetos, os avisos e o texto da pessoa),
/// com o `focus` (spec 141 T06): o projeto do assunto corrente, só como fato ([`notice::focus`]).
/// A procedência devolvida é a das mensagens que ficam inteiras no histórico.
pub fn context(nanty: &Nanty, cfg: &Config, lang: Lang, person: &ConversationMessage, mut last: Value) -> Context {
    let chat = &cfg.nanty.chat;
    let summary = nanty.summary();
    let names = notice::names(cfg);
    let mut lines: Vec<ConversationMessage> =
        pending(nanty, cfg, person, &summary).into_iter().filter(|m| !is_control(m) && !without_provenance(m)).collect();
    lines.drain(..lines.len().saturating_sub(chat.window()));
    if let (Some(focus), Some(body)) = (notice::focus(&lines, person, &names), last.as_object_mut()) {
        body.insert("focus".into(), focus);
    }
    let last = serde_json::to_string(&last).unwrap_or_else(|_| "{}".to_string());
    let recent: Vec<String> = lines.iter().rev().take(chat.quarantine()).rev().map(|m| m.id.clone()).collect();
    let mut rendered: Vec<(Entry, bool, &ConversationMessage)> = lines
        .iter()
        .map(|m| {
            let keeps = provenance::keeps_text(m, &recent);
            (entry_with(lang, m, keeps, &names), keeps, m)
        })
        .collect();
    // O teto de tokens vale sobre o número: as mais antigas saem primeiro.
    let budget = chat.history_tokens();
    let mut total: u64 = rendered.iter().map(|(e, _, _)| u64::from(turn::estimate_tokens(&e.content))).sum();
    let mut first = 0usize;
    while total > budget && first < rendered.len() {
        total = total.saturating_sub(u64::from(turn::estimate_tokens(&rendered[first].0.content)));
        first += 1;
    }
    let kept = rendered.split_off(first);
    let mut taint = Taint::default();
    for (_, keeps, m) in &kept {
        if *keeps {
            taint.absorb(m, &recent);
        }
    }
    // Spec 140 T07: o índice da memória vem antes do resumo, como dado (nunca no sistema), com o teto
    // próprio; a entrada importada que ele lista contamina o turno.
    let index = nanty.memory_index(cfg);
    if let Some(i) = &index {
        for source in &i.taint.sources {
            taint.add(source.kind, source.project_id.as_deref());
        }
    }
    let lead: Vec<String> = index.map(|i| i.block).into_iter().chain(summary_block(lang, cfg, &summary)).collect();
    let entries = kept.into_iter().map(|(e, _, _)| e).collect();
    Context { messages: normalize(lead, entries, last), taint }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
