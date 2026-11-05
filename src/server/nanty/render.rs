//! Texto do Nanty para fora da janela (spec 138 T05, C-08): a cópia que vai ao Telegram (T07) é
//! montada aqui, na hora do envio, com o `tr` do dicionário no idioma do Nanty. O feed e a conversa
//! guardam `code` + `params` (a web traduz) e o texto do modelo como veio.

use serde_json::{Map, Value};

use super::store::{Card, ConversationMessage};
use crate::i18n::{format_count, lookup, tr, tr_plural, Lang};

/// Valor de um parâmetro no formato do idioma: inteiro com separador de milhar, decimal com duas
/// casas e a vírgula ou o ponto do idioma, texto como está.
fn param_text(lang: Lang, value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => match n.as_u64() {
            Some(int) => Some(format_count(lang, int)),
            None => n.as_f64().map(|f| {
                let text = format!("{f:.2}");
                match lang {
                    Lang::PtBr => text.replace('.', ","),
                    Lang::En => text,
                }
            }),
        },
        _ => None,
    }
}

/// Texto de um código do dicionário com os parâmetros, no idioma: com `count` e as formas `.one` e
/// `.other`, o plural do idioma.
pub fn code_text(lang: Lang, code: &str, params: &Map<String, Value>) -> String {
    // A lista de comandos segue o idioma do texto, não o do momento em que a mensagem foi gravada.
    let owned: Vec<(String, String)> = params
        .iter()
        .filter_map(|(k, v)| {
            if k == "commands" {
                return Some((k.clone(), super::chat::command_names(lang)));
            }
            // Spec 141 T06: a resposta do evento de cartão é um código; sai a palavra do idioma.
            if k == "answer" && code.starts_with(super::notice::EVENT_PREFIX) {
                return v.as_str().map(|a| (k.clone(), tr(lang, &format!("nanty.event.answer.{a}"), &[])));
            }
            param_text(lang, v).map(|text| (k.clone(), text))
        })
        .collect();
    let pairs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let count = params.get("count").and_then(Value::as_u64);
    match count {
        Some(count) if lookup(lang, code).is_none() && lookup(lang, &format!("{code}.one")).is_some() => {
            let rest: Vec<(&str, &str)> = pairs.into_iter().filter(|(k, _)| *k != "count").collect();
            tr_plural(lang, code, count, &rest)
        }
        _ => tr(lang, code, &pairs),
    }
}

/// As linhas extras de uma mensagem (`params.<field>`: cada uma `{code, params}`), no idioma. A
/// linha com o nome do projeto ([`super::digest::PROJECT_NAME`], spec 141 T10) sai com ele na frente.
fn lines_text(lang: Lang, params: &Map<String, Value>, field: &str) -> Vec<String> {
    let Some(lines) = params.get(field).and_then(Value::as_array) else { return Vec::new() };
    lines
        .iter()
        .filter_map(|line| {
            let code = line.get("code").and_then(Value::as_str)?;
            let params = line.get("params").and_then(Value::as_object).cloned().unwrap_or_default();
            let text = code_text(lang, code, &params);
            Some(match params.get(super::digest::PROJECT_NAME).and_then(Value::as_str) {
                Some(project) => tr(lang, "nanty.digest.item", &[("project", project), ("text", &text)]),
                None => text,
            })
        })
        .collect()
}

/// Texto de uma mensagem do Nanty para o Telegram, no idioma gravado nela (`message.lang`, spec 139
/// T05); `lang` só vale para a mensagem sem idioma (as gravadas antes).
///
/// Spec 141 T10 (C-10): com o texto do modelo, a mensagem é o texto dele, como veio, e nenhuma frase
/// fixa a fecha (a confirmação, a memória, o caminho até o Nants e o porquê dos avisos guardados ficam
/// nos cartões, nos botões e nos dados da janela). Só o selo dos títulos abre o texto, numa linha
/// curta de dado ([`seal`]): o do resumo ([`super::digest::LEAD`]) e o do cenário proativo
/// ([`super::scenario::LEAD`], spec 140 T12). A exceção é a rede de segurança ([`KEPT`]): a linha
/// curta que diz o que fazer quando não há outro meio (sem link, o caminho para abrir o Nants) fecha o
/// texto do modelo. Sem o texto do modelo, a reserva: o texto fixo do dicionário e as linhas extras.
pub fn message_text(lang: Lang, message: &ConversationMessage) -> String {
    let lang = super::language::tagged(message.lang.as_deref()).unwrap_or(lang);
    match model_text(message) {
        Some(text) => {
            let mut parts: Vec<String> = seal(lang, message).into_iter().collect();
            parts.push(text.to_string());
            parts.extend(kept(lang, message));
            parts.join("\n")
        }
        None => fixed_parts(lang, message).join("\n"),
    }
}

/// As linhas do servidor que ficam mesmo com o texto do modelo (spec 141 T10, revisão 1, F-01): o
/// efeito que não tem cartão nem botão para levar o que a pessoa pode fazer. Sem endereço que o
/// aparelho alcance, a linha é a única saída dela.
pub const KEPT: [&str; 1] = ["nanty.chat.open_computer"];

/// As linhas mantidas ([`KEPT`]) da mensagem: o código dela ou as linhas extras do porquê.
fn kept(lang: Lang, message: &ConversationMessage) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(code) = message.code.as_deref().filter(|c| KEPT.contains(c)) {
        out.push(code_text(lang, code, &message.params));
    }
    let lines = message.params.get(super::digest::WHY).and_then(Value::as_array).cloned().unwrap_or_default();
    let kept: Vec<Value> = lines.into_iter().filter(|l| l.get("code").and_then(Value::as_str).is_some_and(|c| KEPT.contains(&c))).collect();
    if !kept.is_empty() {
        let mut only = Map::new();
        only.insert("kept".into(), Value::Array(kept));
        out.extend(lines_text(lang, &only, "kept"));
    }
    out
}

fn model_text(message: &ConversationMessage) -> Option<&str> {
    message.text.as_deref().filter(|t| !t.trim().is_empty())
}

/// Os títulos que viram selo: uma linha curta de dado antes do texto (spec 141 T10).
const SEALS: [&str; 2] = [super::digest::LEAD, super::scenario::LEAD];

/// O selo da mensagem, quando o código é um título ([`SEALS`]).
fn seal(lang: Lang, message: &ConversationMessage) -> Option<String> {
    let code = message.code.as_deref().filter(|code| SEALS.contains(code))?;
    Some(code_text(lang, code, &Map::new()))
}

/// O texto fixo da reserva: a frase do código, as linhas extras e o porquê dos avisos guardados.
fn fixed_parts(lang: Lang, message: &ConversationMessage) -> Vec<String> {
    let Some(code) = &message.code else { return Vec::new() };
    let mut parts = vec![code_text(lang, code, &message.params)];
    parts.extend(lines_text(lang, &message.params, "lines"));
    parts.extend(lines_text(lang, &message.params, super::digest::WHY));
    parts
}

/// Texto do porquê de um cartão para o Telegram (o texto fixo do observador, C-06).
pub fn card_text(lang: Lang, card: &Card) -> String {
    // Spec 140 T07: as linhas extras do cartão (o texto exato e a fonte do pedido à memória).
    let mut parts = vec![code_text(lang, &card.code, &card.params)];
    parts.extend(lines_text(lang, &card.params, super::memory::card::CARD_LINES));
    // Spec 140 T04 (C-09): o texto de cartão passa pelo redator único, como a resposta final.
    super::redact::patterns(&parts.join("\n"))
}
