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

/// As linhas extras de uma mensagem (`params.lines`: cada uma `{code, params}`), no idioma.
fn lines_text(lang: Lang, params: &Map<String, Value>) -> Vec<String> {
    let Some(lines) = params.get("lines").and_then(Value::as_array) else { return Vec::new() };
    lines
        .iter()
        .filter_map(|line| {
            let code = line.get("code").and_then(Value::as_str)?;
            let params = line.get("params").and_then(Value::as_object).cloned().unwrap_or_default();
            Some(code_text(lang, code, &params))
        })
        .collect()
}

/// Texto de uma mensagem do Nanty para o Telegram: o texto fixo do dicionário, as linhas extras e o
/// texto do modelo, como veio.
pub fn message_text(lang: Lang, message: &ConversationMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(code) = &message.code {
        parts.push(code_text(lang, code, &message.params));
        parts.extend(lines_text(lang, &message.params));
    }
    if let Some(text) = message.text.as_deref().filter(|t| !t.trim().is_empty()) {
        parts.push(text.to_string());
    }
    parts.join("\n")
}

/// Texto do porquê de um cartão para o Telegram (o texto fixo do observador, C-06).
pub fn card_text(lang: Lang, card: &Card) -> String {
    code_text(lang, &card.code, &card.params)
}
