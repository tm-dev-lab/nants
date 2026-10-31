//! Formatação segura do que sai para o Telegram (spec 138 T07, C-10 e C-12).
//!
//! - **Texto puro:** nada de `parse_mode`. `_`, `*` e colchetes chegam como estão, sem quebrar o
//!   envio nem virar formatação.
//! - **Sem link:** a prévia vai desligada no envio, e o texto passa por [`neutralize`]: o esquema
//!   das URLs (`https://`, `tg://`, `mailto:`) sai e os pontos dos domínios viram `[.]`, então o
//!   Telegram não monta link nenhum; `@nome` e `/comando` perdem o toque (o `@` e a barra viram os
//!   sinais de largura cheia), para um texto do modelo nunca abrir outro bot nem mandar um comando.
//! - **Sem código, sem log, sem segredo:** o bloco entre cercas de três crases sai inteiro, as
//!   crases soltas saem, os controles e a formatação invisível viram espaço
//!   (`events::normalize_text`) e o [`SecretSet`] troca qualquer segredo conhecido por `***`.
//! - **Limite:** [`fit`] corta no limite do Telegram (4096, contado como o Telegram conta, em
//!   unidades UTF-16), de preferência numa quebra de linha ou num espaço, e fecha com `…`.

use crate::server::events::{normalize_text, SecretSet};

/// Limite do texto de uma mensagem do Telegram.
pub const MAX_MESSAGE_CHARS: usize = 4096;
/// Limite da legenda de uma nota de voz (T09, C-12).
pub const MAX_CAPTION_CHARS: usize = 1024;
/// Marca do corte.
const CUT_MARK: char = '…';
/// Arroba e barra de largura cheia: aparecem iguais, mas o Telegram não as toma por menção nem
/// por comando.
const SAFE_AT: char = '\u{FF20}';
const SAFE_SLASH: char = '\u{FF0F}';

/// Todo texto que vai ao Telegram: segredos, código, controles, links e o limite.
pub fn clean(secrets: &SecretSet, text: &str) -> String {
    // O scrub roda de novo depois de cada etapa que apaga caracteres (as crases do `strip_code`, as
    // sequências de terminal e os controles do `normalize_text`): um segredo partido por elas volta
    // inteiro na etapa e não pode escapar. O `neutralize` só tira o começo de uma palavra ou insere
    // caracteres, e o `fit` só corta; mesmo assim o último scrub vem depois deles, antes do corte.
    let scrubbed = secrets.scrub_exact(text);
    let no_code = secrets.scrub_exact(&strip_code(&scrubbed));
    let normalized = secrets.scrub_exact(&normalize_text(&no_code));
    let neutral = secrets.scrub_exact(&neutralize(&normalized));
    fit(neutral.trim(), MAX_MESSAGE_CHARS)
}

/// Tira os blocos entre cercas de três crases (com as cercas) e as crases soltas.
pub fn strip_code(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push(line);
        }
    }
    out.join("\n").replace('`', "")
}

/// O ponto `.` entre duas letras ou dígitos, com uma letra depois (o jeito de um domínio).
fn defang_dots(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let mut out = String::with_capacity(word.len() + 8);
    for (i, c) in chars.iter().enumerate() {
        let before = i.checked_sub(1).and_then(|j| chars.get(j)).is_some_and(|p| p.is_alphanumeric());
        let after = chars.get(i + 1).is_some_and(|n| n.is_alphabetic());
        if *c == '.' && before && after {
            out.push_str("[.]");
        } else {
            out.push(*c);
        }
    }
    out
}

/// Uma palavra sem link, sem menção e sem comando.
fn neutral_word(word: &str) -> String {
    let lower = word.to_lowercase();
    let mut rest = word;
    if let Some(pos) = lower.find("://") {
        // Tira o esquema (e o que vier grudado antes dele).
        rest = word.get(pos + 3..).unwrap_or("");
    } else {
        for scheme in ["mailto:", "tg:"] {
            if lower.starts_with(scheme) {
                rest = word.get(scheme.len()..).unwrap_or("");
                break;
            }
        }
    }
    let mut out = defang_dots(rest);
    if out.starts_with('/') && out.chars().nth(1).is_some_and(|c| c.is_ascii_alphabetic()) {
        out.replace_range(..1, &SAFE_SLASH.to_string());
    }
    out.replace('@', &SAFE_AT.to_string())
}

/// Neutraliza links, menções e comandos, palavra por palavra, mantendo os espaços e as quebras.
pub fn neutralize(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut word = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            if !word.is_empty() {
                out.push_str(&neutral_word(&word));
                word.clear();
            }
            out.push(c);
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        out.push_str(&neutral_word(&word));
    }
    out
}

/// Tamanho como o Telegram conta (unidades UTF-16).
pub fn units(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Corta `text` em `max` unidades, com `…` no fim: de preferência na última quebra de linha (ou
/// no último espaço) da segunda metade, para não partir uma palavra.
pub fn fit(text: &str, max: usize) -> String {
    if units(text) <= max {
        return text.to_string();
    }
    let budget = max.saturating_sub(CUT_MARK.len_utf16());
    let mut end = 0;
    let mut used = 0;
    for (i, c) in text.char_indices() {
        if used + c.len_utf16() > budget {
            break;
        }
        used += c.len_utf16();
        end = i + c.len_utf8();
    }
    let head = &text[..end];
    let half = head.len() / 2;
    let cut = head.rfind('\n').filter(|p| *p >= half).or_else(|| head.rfind(' ').filter(|p| *p >= half)).unwrap_or(end);
    let mut out = head[..cut].trim_end().to_string();
    out.push(CUT_MARK);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_characters_pass_as_plain_text() {
        let text = "tarefa_2 *quase* pronta [ok] (x) _fim_";
        assert_eq!(clean(&SecretSet::default(), text), text);
    }

    #[test]
    fn links_mentions_and_commands_are_neutralized() {
        let out = clean(&SecretSet::default(), "veja https://evil.example/x?a=1 ou www.evil.com, fale com @evilbot e toque /approve");
        assert!(!out.contains("https") && !out.contains("://"), "{out}");
        assert!(out.contains("evil[.]example/x?a=1") && out.contains("www[.]evil[.]com,"), "{out}");
        assert!(!out.contains("@evilbot") && out.contains("\u{FF20}evilbot"), "{out}");
        assert!(!out.contains(" /approve") && out.contains("\u{FF0F}approve"), "{out}");
        assert_eq!(neutralize("tg://resolve?domain=x mailto:a@b.io"), "resolve?domain=x a\u{FF20}b[.]io");
        // Números e frases comuns ficam como estão.
        assert_eq!(neutralize("US$ 0.50, versão 1.2. Fim."), "US$ 0.50, versão 1.2. Fim.");
        assert_eq!(neutralize("a\n  b"), "a\n  b", "quebras e espaços mantidos");
    }

    #[test]
    fn code_blocks_and_backticks_leave() {
        let out = clean(&SecretSet::default(), "antes\n```sh\nrm -rf /\n```\nrode `cargo test` depois");
        assert_eq!(out, "antes\nrode cargo test depois");
    }

    #[test]
    fn secrets_are_redacted_before_anything_else() {
        let secret = format!("{}{}", "chave-", "x".repeat(24));
        let secrets = SecretSet::new([secret.clone()]);
        let out = clean(&secrets, &format!("a chave é {secret}."));
        assert!(!out.contains(&secret) && out.contains("***"), "{out}");
    }

    /// Um segredo partido por algo que uma etapa apaga (crase, sequência de terminal, controle,
    /// formatação invisível, cerca de código) não volta inteiro na saída.
    #[test]
    fn split_secret_never_comes_back_whole() {
        let (head, tail) = (format!("{}{}", "sk-", "A".repeat(12)), "B".repeat(12));
        let secret = format!("{head}{tail}");
        let secrets = SecretSet::new([secret.clone()]);
        for sep in ["`", "``", "\u{1b}[0m", "\u{1b}]0;x\u{7}", "\u{200b}", "\u{202e}"] {
            let out = clean(&secrets, &format!("a chave é {head}{sep}{tail}."));
            assert!(!out.contains(&secret), "{sep:?}: {out}");
        }
        // Partido por uma linha de cerca: o bloco some e as duas metades se juntam.
        let out = clean(&secrets, &format!("{head}\n```\nx\n```\n{tail}"));
        assert!(!out.contains(&secret), "{out}");
        // As etapas que só inserem ou cortam não juntam nada.
        let out = clean(&secrets, &format!("https://{head}{tail} @{secret}"));
        assert!(!out.contains(&secret), "{out}");
    }

    #[test]
    fn long_text_is_cut_cleanly_at_the_limit() {
        let line = "palavra ".repeat(100);
        let text = vec![line.trim_end(); 20].join("\n");
        assert!(units(&text) > MAX_MESSAGE_CHARS);
        let out = fit(&text, MAX_MESSAGE_CHARS);
        assert!(units(&out) <= MAX_MESSAGE_CHARS, "{}", units(&out));
        assert!(out.ends_with("palavra…"), "corta na quebra, sem partir palavra");
        // Sem espaço nenhum: corta no limite, sem partir um caractere de duas unidades.
        let wide = "\u{10400}".repeat(3000);
        let out = fit(&wide, MAX_MESSAGE_CHARS);
        assert!(units(&out) <= MAX_MESSAGE_CHARS && out.ends_with('…'));
        assert_eq!(fit("curto", 10), "curto");
        assert_eq!(units(&fit(&"a".repeat(2000), 1024)), 1024);
    }
}
