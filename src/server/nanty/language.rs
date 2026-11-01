//! O idioma do Nanty (spec 139 G5, C-13).
//!
//! - **A conversa** responde no idioma de quem falou. O modelo é instruído a isso; a mensagem da
//!   pessoa grava o idioma resolvido em `lang`, e a resposta (a do modelo e a reserva fixa) sai nele.
//!   A resolução segue esta ordem ([`person_lang`]): a [`detect`] sobre o texto (acentos e palavras
//!   comuns, sem biblioteca); o último idioma da pessoa; o `language_code` do Telegram; o idioma do
//!   Nanty ([`super::nanty_lang`]). Spec 139 T12: a rodada real mostrou que uma mensagem curta só
//!   com o nome de um projeto ("My Shop", no meio de uma conversa em português) trocava o idioma da
//!   pessoa. Para sair do último idioma dela, valem [`SWITCH_CLUES`] pistas numa mensagem ou uma
//!   pista em duas mensagens seguidas, com a primeira guardada em `person_lang_pending`.
//! - **Os avisos proativos e o resumo** seguem o `[nanty] language`: `follow` (o padrão) é o último
//!   idioma da pessoa e, sem ele, o da interface lembrado (`[nanty] lang`, do `server::lang`, que não
//!   muda); `pt-BR` e `en` fixam.
//! - **O `render`** de cada mensagem usa o `lang` gravado nela, então o texto fixo e o do modelo saem
//!   no mesmo idioma no Telegram e na janela.

use super::store::{Channel, NantyState};
use crate::config::NantyConfig;
use crate::i18n::Lang;

/// Palavras comuns só do português (já sem acento, como em `chat::words`).
const PT_WORDS: &[&str] = &[
    "o", "os", "e", "de", "da", "das", "dos", "na", "nas", "nos", "um", "uma", "que", "nao", "sim", "com", "para",
    "pra", "por", "como", "esta", "estao", "isso", "isto", "voce", "meu", "minha", "projeto", "projetos", "agora",
    "qual", "quais", "quando", "onde", "porque", "tudo", "ainda", "ja", "pode", "tem", "sao", "mas", "muito", "oi",
    "ola", "obrigado", "obrigada", "bom", "hoje", "ontem", "silencio", "ajuda",
];

/// Palavras comuns só do inglês.
const EN_WORDS: &[&str] = &[
    "the", "is", "are", "was", "what", "how", "and", "you", "your", "this", "that", "with", "for", "please", "thanks",
    "thank", "hello", "hi", "hey", "yes", "now", "which", "when", "where", "why", "all", "still", "already", "does",
    "can", "have", "has", "project", "projects", "of", "to", "in", "on", "it", "my", "there", "any", "about", "pause",
    "summary", "help", "today", "yesterday", "good", "morning", "quiet", "going",
];

/// Pistas que o texto precisa ter para trocar o último idioma da pessoa. Uma só pode ser o nome de
/// um projeto ou uma palavra emprestada.
const SWITCH_CLUES: usize = 2;

/// Letras que só o português usa (entre pt e en).
fn pt_letter(c: char) -> bool {
    matches!(c, 'ã' | 'õ' | 'ç' | 'â' | 'ê' | 'ô' | 'á' | 'é' | 'í' | 'ó' | 'ú' | 'à')
}

/// Os pontos de cada idioma no texto, `(pt, en)`: cada palavra comum de um idioma conta um ponto
/// para ele, e cada palavra com letra acentuada do português conta mais um.
fn clues(text: &str) -> (usize, usize) {
    let mut pt = 0usize;
    let mut en = 0usize;
    for raw in text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let lower = raw.to_lowercase();
        if lower.chars().any(pt_letter) {
            pt += 1;
        }
        let word = super::chat::fold_word(&lower);
        if PT_WORDS.contains(&word.as_str()) {
            pt += 1;
        } else if EN_WORDS.contains(&word.as_str()) {
            en += 1;
        }
    }
    (pt, en)
}

/// O idioma do texto, entre pt e en, pela contagem de [`clues`]. Empate ou nenhum sinal ("ok", um
/// número) é `None`.
pub fn detect(text: &str) -> Option<Lang> {
    let (pt, en) = clues(text);
    match pt.cmp(&en) {
        std::cmp::Ordering::Greater => Some(Lang::PtBr),
        std::cmp::Ordering::Less => Some(Lang::En),
        std::cmp::Ordering::Equal => None,
    }
}

/// O texto mostra `lang` com força para, sozinho, trocar de idioma: [`SWITCH_CLUES`] pistas ou um
/// comando (`/silencio`, a escolha de um dos dois conjuntos).
fn firm(text: &str, lang: Lang) -> bool {
    let (pt, en) = clues(text);
    let points = if lang == Lang::PtBr { pt } else { en };
    points >= SWITCH_CLUES || text.trim_start().starts_with('/')
}

/// O idioma que o texto mostra para quem falava `last`: o mesmo idioma, nenhum antes, um texto
/// [`firm`] ou a segunda mensagem seguida com pista do idioma `pending`.
fn shown(text: &str, last: Option<Lang>, pending: Option<Lang>) -> Option<Lang> {
    let lang = detect(text)?;
    (last.is_none_or(|l| l == lang) || pending == Some(lang) || firm(text, lang)).then_some(lang)
}

/// A pista que fica pendente depois desta mensagem: o outro idioma, visto sem força para trocar.
/// A mensagem que troca, a que fica no idioma atual e a sem pista zeram.
pub fn pending_after(st: &NantyState, text: &str) -> Option<Lang> {
    let last = tagged(st.person_lang.as_deref())?;
    let pending = tagged(st.person_lang_pending.as_deref());
    detect(text).filter(|lang| *lang != last && pending != Some(*lang) && !firm(text, *lang))
}

/// O `language_code` do Telegram (IETF: `pt-br`, `pt`, `en`, `en-US`) como idioma do Nanty; outro
/// idioma é `None`.
pub fn from_language_code(code: &str) -> Option<Lang> {
    let primary = code.trim().split(['-', '_']).next().unwrap_or_default().to_ascii_lowercase();
    match primary.as_str() {
        "pt" => Some(Lang::PtBr),
        "en" => Some(Lang::En),
        _ => None,
    }
}

/// O idioma de uma etiqueta gravada (`pt-BR` ou `en`).
pub fn tagged(tag: Option<&str>) -> Option<Lang> {
    tag.and_then(Lang::from_tag)
}

/// O idioma da interface lembrado (`[nanty] lang`); sem ele, pt-BR.
pub fn interface_lang(cfg: &NantyConfig) -> Lang {
    tagged(cfg.lang.as_deref()).unwrap_or_default()
}

/// O idioma do Nanty: o escolhido; com `follow`, o último idioma da pessoa e, sem ele, o da
/// interface.
pub fn resolve(cfg: &NantyConfig, st: &NantyState) -> Lang {
    cfg.language.fixed().or_else(|| tagged(st.person_lang.as_deref())).unwrap_or_else(|| interface_lang(cfg))
}

/// O idioma da mensagem da pessoa e se ele é um sinal dela (o texto ou o `language_code`), que vira
/// o último idioma da pessoa. A ordem: o texto ([`shown`]: trocar de idioma pede mais de uma pista,
/// na mesma mensagem ou em duas seguidas),
/// o último idioma da pessoa, o `language_code` do Telegram (só na mensagem que veio por ele) e o
/// idioma do Nanty.
pub fn person_lang(cfg: &NantyConfig, st: &NantyState, channel: Channel, text: Option<&str>) -> (Lang, bool) {
    let last = tagged(st.person_lang.as_deref());
    let pending = tagged(st.person_lang_pending.as_deref());
    if let Some(lang) = text.and_then(|t| shown(t, last, pending)) {
        return (lang, true);
    }
    if let Some(lang) = last {
        return (lang, false);
    }
    if let Some(lang) = (channel == Channel::Telegram).then(|| tagged(st.telegram_lang.as_deref())).flatten() {
        return (lang, true);
    }
    (resolve(cfg, st), false)
}

#[cfg(test)]
#[path = "lang_tests.rs"]
mod flow_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NantyLanguage;

    #[test]
    fn detects_portuguese_and_english() {
        for pt in ["Como está o projeto 14?", "silencia uma hora", "não", "e a Loja agora?", "oi, tudo bem?", "Ação"] {
            assert_eq!(detect(pt), Some(Lang::PtBr), "{pt}");
        }
        for en in ["How is project 14 going?", "what is the status", "thanks", "pause the run please", "Hi there"] {
            assert_eq!(detect(en), Some(Lang::En), "{en}");
        }
        for none in ["ok", "14", "", "Loja", "status"] {
            assert_eq!(detect(none), None, "{none}");
        }
    }

    #[test]
    fn reads_the_telegram_language_code() {
        assert_eq!(from_language_code("pt-br"), Some(Lang::PtBr));
        assert_eq!(from_language_code("pt"), Some(Lang::PtBr));
        assert_eq!(from_language_code("en-US"), Some(Lang::En));
        assert_eq!(from_language_code("en"), Some(Lang::En));
        assert_eq!(from_language_code("fr"), None);
        assert_eq!(from_language_code(""), None);
    }

    #[test]
    fn follow_uses_the_person_then_the_interface() {
        let mut cfg = NantyConfig::default();
        let mut st = NantyState::default();
        assert_eq!(resolve(&cfg, &st), Lang::PtBr, "sem nada: pt-BR");
        cfg.lang = Some("en".into());
        assert_eq!(resolve(&cfg, &st), Lang::En, "sem histórico: a interface");
        st.person_lang = Some("pt-BR".into());
        assert_eq!(resolve(&cfg, &st), Lang::PtBr, "a conversa vence a interface");
        cfg.language = NantyLanguage::En;
        assert_eq!(resolve(&cfg, &st), Lang::En, "o escolhido vence a conversa");
    }

    #[test]
    fn person_language_follows_the_order() {
        let mut cfg = NantyConfig::default();
        cfg.lang = Some("en".into());
        let mut st = NantyState::default();
        assert_eq!(person_lang(&cfg, &st, Channel::Web, Some("e agora?")), (Lang::PtBr, true), "1: o texto");
        assert_eq!(person_lang(&cfg, &st, Channel::Telegram, Some("ok")), (Lang::En, false), "4: o Nanty");
        st.telegram_lang = Some("pt-BR".into());
        assert_eq!(person_lang(&cfg, &st, Channel::Telegram, Some("ok")), (Lang::PtBr, true), "3: o language_code");
        assert_eq!(person_lang(&cfg, &st, Channel::Web, Some("ok")), (Lang::En, false), "o language_code é só do Telegram");
        st.person_lang = Some("en".into());
        assert_eq!(person_lang(&cfg, &st, Channel::Telegram, Some("ok")), (Lang::En, false), "2: o último da pessoa");
        assert_eq!(person_lang(&cfg, &st, Channel::Telegram, Some("está tudo certo?")), (Lang::PtBr, true), "o texto vence");
    }

    /// Spec 139 T12, critério 7: visto na rodada real. O nome de um projeto em outro idioma, numa
    /// mensagem curta, não troca o idioma da pessoa; duas pistas trocam.
    #[test]
    fn one_clue_does_not_switch_the_language() {
        let cfg = NantyConfig::default();
        let mut st = NantyState::default();
        st.person_lang = Some("pt-BR".into());
        assert_eq!(person_lang(&cfg, &st, Channel::Telegram, Some("My Shop")), (Lang::PtBr, false), "uma pista: fica no da pessoa");
        assert_eq!(person_lang(&cfg, &st, Channel::Telegram, Some("what about my shop?")), (Lang::En, true), "várias pistas: troca");
        st.person_lang = Some("en".into());
        assert_eq!(person_lang(&cfg, &st, Channel::Web, Some("Projeto")), (Lang::En, false), "vale nos dois sentidos");
        assert_eq!(person_lang(&cfg, &st, Channel::Web, Some("não")), (Lang::PtBr, true), "a palavra acentuada conta duas");
        assert_eq!(person_lang(&cfg, &st, Channel::Web, Some("/silencio 0")), (Lang::PtBr, true), "o comando escolhe o conjunto do idioma");
        st.person_lang = None;
        assert_eq!(person_lang(&cfg, &st, Channel::Telegram, Some("My Shop")), (Lang::En, true), "sem histórico, a pista basta");
    }

    /// A conversa inteira: o que `note_person_lang` faz a cada mensagem.
    fn talk(st: &mut NantyState, text: &str) -> Lang {
        let cfg = NantyConfig::default();
        let (lang, signal) = person_lang(&cfg, st, Channel::Telegram, Some(text));
        let pending = pending_after(st, text);
        if signal {
            st.person_lang = Some(lang.tag().to_string());
        }
        st.person_lang_pending = pending.map(|l| l.tag().to_string());
        lang
    }

    fn speaking(tag: &str) -> NantyState {
        NantyState { person_lang: Some(tag.into()), ..NantyState::default() }
    }

    /// Spec 139 T12, F-02: as frases de uma pista não prendem a pessoa no idioma antigo.
    #[test]
    fn two_messages_in_a_row_switch_the_language() {
        let mut st = speaking("pt-BR");
        assert_eq!(talk(&mut st, "how's 14 doing?"), Lang::PtBr, "a primeira só fica pendente");
        assert_eq!(st.person_lang_pending.as_deref(), Some("en"));
        assert_eq!(talk(&mut st, "why?"), Lang::En, "a segunda troca");
        assert_eq!((st.person_lang.as_deref(), st.person_lang_pending.as_deref()), (Some("en"), None));

        // O inverso.
        let mut st = speaking("en");
        assert_eq!(talk(&mut st, "tudo certo?"), Lang::En);
        assert_eq!(talk(&mut st, "pausa o 14"), Lang::PtBr);
        assert_eq!(st.person_lang.as_deref(), Some("pt-BR"));

        // O nome de um projeto, isolado, não troca, e a mensagem no idioma atual zera a pendência.
        let mut st = speaking("pt-BR");
        assert_eq!(talk(&mut st, "My Shop"), Lang::PtBr);
        assert_eq!(st.person_lang_pending.as_deref(), Some("en"));
        assert_eq!(talk(&mut st, "e o 14?"), Lang::PtBr);
        assert_eq!(st.person_lang_pending, None);
        assert_eq!(talk(&mut st, "My Shop"), Lang::PtBr, "de novo isolado: fica");

        // Uma mensagem sem pista entre as duas: deixam de ser seguidas.
        let mut st = speaking("pt-BR");
        talk(&mut st, "My Shop");
        assert_eq!(talk(&mut st, "14"), Lang::PtBr);
        assert_eq!(talk(&mut st, "why?"), Lang::PtBr);
        assert_eq!(st.person_lang_pending.as_deref(), Some("en"));
    }
}
