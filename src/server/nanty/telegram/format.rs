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
//!   (`events::normalize_text`) e o redator ([`redact::scrub`]) troca por `***` os segredos conhecidos
//!   e o que tem cara de segredo.
//! - **Limite:** [`fit`] corta no limite do Telegram (4096, contado como o Telegram conta, em
//!   unidades UTF-16), de preferência numa quebra de linha ou num espaço, e fecha com `…`.
//! - **Resposta longa (spec 140 T14, C-27):** a limpeza é uma coisa e o corte é outra. A conversa
//!   grava o texto inteiro, limpo ([`clean_whole`]), e o Telegram o divide em até [`MAX_PARTS`]
//!   mensagens ([`split`]), numa quebra de parágrafo, de linha ou de frase, ou num espaço: nunca no
//!   meio de uma palavra nem de um segredo mascarado. Acima disso, a última parte termina com a marca
//!   do corte (spec 141 T10, C-10: sem frase fixa no fim).

use crate::server::events::{normalize_text, SecretSet};
use crate::server::nanty::redact;

/// Limite do texto de uma mensagem do Telegram.
pub const MAX_MESSAGE_CHARS: usize = 4096;
/// Limite da legenda de uma nota de voz (T09, C-12).
pub const MAX_CAPTION_CHARS: usize = 1024;
/// Mensagens de uma resposta longa no Telegram, no máximo (spec 140 T14, C-27).
pub const MAX_PARTS: usize = 3;
/// Folga da última mensagem com teclado (spec 140 T14): as linhas da recusa do teclado (o endereço do
/// Nants e a aba Memória) entram depois da divisão, e o corte do limite nunca chega ao texto dela. Vale
/// também para a mensagem única (revisão 1, F-01).
pub const LAST_PART_ROOM: usize = 512;
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
    // Spec 140 T04 (C-09): cada etapa passa pelo redator único, que soma os padrões ao `SecretSet`.
    let scrubbed = redact::scrub(secrets, text);
    let no_code = redact::scrub(secrets, &strip_code(&scrubbed));
    let normalized = redact::scrub(secrets, &normalize_text(&no_code));
    let neutral = redact::scrub(secrets, &neutralize(&normalized));
    fit(neutral.trim(), MAX_MESSAGE_CHARS)
}

/// Como [`clean`], sem o corte no limite da mensagem (spec 140 T07, revisão 2, F-04): quem decide o
/// que fazer com o texto longo confere o tamanho inteiro antes.
pub fn clean_whole(secrets: &SecretSet, text: &str) -> String {
    let scrubbed = redact::scrub(secrets, text);
    let no_code = redact::scrub(secrets, &strip_code(&scrubbed));
    let normalized = redact::scrub(secrets, &normalize_text(&no_code));
    redact::scrub(secrets, &neutralize(&normalized)).trim().to_string()
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

/// Como [`fit`], preservando o fecho do texto (a frase do servidor, spec 139 T12): o corte fica no
/// que vem antes dele. Sem o fecho no fim do texto, é o [`fit`].
pub fn fit_keeping(text: &str, tail: Option<&str>, max: usize) -> String {
    let head = tail.and_then(|t| Some((text.strip_suffix(t)?.trim_end(), t)));
    match head {
        Some((head, tail)) if units(text) > max && units(tail) < max => {
            format!("{}\n{tail}", fit(head, max.saturating_sub(units(tail) + 1)))
        }
        _ => fit(text, max),
    }
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

/// Divide o texto de uma mensagem longa em partes de até `max` unidades, no máximo `max_parts`
/// (spec 140 T14, C-27). O texto que cabe numa mensagem volta inteiro, numa parte só.
///
/// - **Onde divide ([`cut_point`]):** numa quebra de parágrafo, de linha ou de frase, ou num espaço;
///   nunca no meio de uma palavra, e por isso nunca no meio de um segredo mascarado (`***`).
/// - **A última parte** guarda `room` unidades (com teclado, [`LAST_PART_ROOM`]) para as linhas da
///   recusa do teclado, que vêm depois, e a mensagem única também: só fica inteira numa parte o texto
///   que cabe com essa folga.
/// - **Acima do teto:** o que não coube sai, e a última parte termina com a marca do corte (`…`).
///   Spec 141 T10 (C-10): sem frase fixa que aponte a janela nem frase do servidor que feche o texto;
///   a resposta inteira fica na conversa da janela.
pub fn split(text: &str, max: usize, room: usize, max_parts: usize) -> Vec<String> {
    split_parts(text, max, room, max_parts).0
}

/// O texto cabe inteiro nas partes (spec 141 T08, revisão 2, F-02): o mesmo laço do [`split`]
/// ([`split_parts`]), em mensagens de [`MAX_MESSAGE_CHARS`], entrega tudo, sem corte. É o critério
/// da fala: só é falado o que o Telegram mostra escrito por inteiro.
pub fn fits_parts(text: &str, room: usize, max_parts: usize) -> bool {
    split_parts(text, MAX_MESSAGE_CHARS, room, max_parts).1
}

/// O núcleo do [`split`] e do [`fits_parts`]: as partes e se o texto coube inteiro nelas. Com
/// `max_parts <= 1`, a parte única é o texto como veio (quem manda corta no limite), e só conta como
/// inteiro o que cabe numa mensagem.
fn split_parts(text: &str, max: usize, room: usize, max_parts: usize) -> (Vec<String>, bool) {
    if units(text) <= max.saturating_sub(room) {
        return (vec![text.to_string()], true);
    }
    if max_parts <= 1 {
        return (vec![text.to_string()], units(text) <= max);
    }
    let last_budget = max.saturating_sub(room).max(max / 4);
    let mut parts: Vec<String> = Vec::new();
    let mut whole = true;
    let mut rest = text.trim();
    while !rest.is_empty() {
        if units(rest) <= last_budget {
            parts.push(rest.to_string());
            break;
        }
        if parts.len() + 1 >= max_parts {
            let cut = cut_point(rest, last_budget.saturating_sub(CUT_MARK.len_utf16()));
            parts.push(format!("{}{CUT_MARK}", rest[..cut].trim_end()));
            whole = false;
            break;
        }
        // O que cabe numa mensagem mas não na última, com a folga: esta parte fica com o que cabe na
        // última, e o resto vai na seguinte.
        let cut = match cut_point(rest, max) {
            whole if whole >= rest.len() => cut_point(rest, last_budget),
            cut => cut,
        };
        parts.push(rest[..cut].trim_end().to_string());
        rest = rest[cut..].trim_start();
    }
    (parts, whole)
}

/// Onde dividir `text` para a parte caber em `budget` unidades (spec 140 T14): o fim em bytes da
/// parte. Da preferência maior para a menor, no último terço da janela: a quebra de parágrafo, a de
/// linha, o fim de frase e o espaço. Sem nada disso, o último espaço da janela; sem espaço nenhum (uma
/// palavra maior que a mensagem), o corte no limite, que recua para não partir a marca `***`.
fn cut_point(text: &str, budget: usize) -> usize {
    let mut end = 0;
    let mut used = 0;
    for (i, c) in text.char_indices() {
        if used + c.len_utf16() > budget {
            break;
        }
        used += c.len_utf16();
        end = i + c.len_utf8();
    }
    if end >= text.len() {
        return text.len();
    }
    let head = &text[..end];
    let floor = head.len() / 3;
    let at_least = |p: usize| p >= floor && p > 0;
    if let Some(p) = head.rfind("\n\n").filter(|p| at_least(*p)) {
        return p;
    }
    if let Some(p) = head.rfind('\n').filter(|p| at_least(*p)) {
        return p;
    }
    let sentence = head
        .char_indices()
        .filter(|(i, c)| matches!(c, '.' | '!' | '?' | '…') && text[i + c.len_utf8()..].chars().next().is_some_and(char::is_whitespace))
        .map(|(i, c)| i + c.len_utf8())
        .filter(|p| at_least(*p))
        .last();
    if let Some(p) = sentence {
        return p;
    }
    if let Some(p) = head.rfind(char::is_whitespace).filter(|p| *p > 0) {
        return p;
    }
    // Uma palavra só, maior que a mensagem: o corte no limite, fora da marca do segredo.
    let mut cut = end;
    while cut > 0 && text[..cut].ends_with('*') && text[cut..].starts_with('*') {
        cut -= 1;
    }
    if cut == 0 {
        end
    } else {
        cut
    }
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
    fn the_tail_survives_the_cut() {
        let long = format!("{}\nConfirme.", "palavra ".repeat(100).trim());
        let cut = fit_keeping(&long, Some("Confirme."), 100);
        assert!(units(&cut) <= 100 && cut.ends_with("…\nConfirme."), "{cut}");
        assert_eq!(fit_keeping("curto\nConfirme.", Some("Confirme."), 100), "curto\nConfirme.");
        assert_eq!(fit_keeping(&long, Some("outro fecho"), 100), fit(&long, 100), "sem o fecho no fim, o corte comum");
        assert_eq!(fit_keeping(&long, None, 100), fit(&long, 100));
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

    /// Os pedaços de texto de cada parte, juntos de novo.
    fn words_of(parts: &[String]) -> Vec<String> {
        parts.iter().flat_map(|p| p.split_whitespace().map(str::to_string).collect::<Vec<_>>()).collect()
    }

    /// O fim de um texto, para a mensagem do teste.
    fn tail_of(text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        chars[chars.len().saturating_sub(60)..].iter().collect()
    }

    /// Spec 140 T14 (C-27): o que cabe numa mensagem volta inteiro; o longo vira partes que cabem, e
    /// nenhuma palavra partida.
    #[test]
    fn a_long_text_splits_into_parts_that_fit() {
        assert_eq!(split("curto\nfim.", MAX_MESSAGE_CHARS, LAST_PART_ROOM, MAX_PARTS), ["curto\nfim."]);
        let paragraph = |n: usize| format!("Parágrafo {n}: {}", "a arquitetura segue estável e o plano continua. ".repeat(20).trim_end());
        let body: Vec<String> = (1..=6).map(paragraph).collect();
        let text = body.join("\n\n");
        assert!(units(&text) > MAX_MESSAGE_CHARS);
        let parts = split(&text, MAX_MESSAGE_CHARS, LAST_PART_ROOM, MAX_PARTS);
        assert!((2..=MAX_PARTS).contains(&parts.len()), "{}", parts.len());
        assert!(parts.iter().all(|p| units(p) <= MAX_MESSAGE_CHARS));
        let last = parts.last().map(String::as_str).unwrap_or_default();
        assert!(units(last) + LAST_PART_ROOM <= MAX_MESSAGE_CHARS, "{last}");
        // Coube tudo: nada falta, e a marca do corte não aparece.
        assert_eq!(words_of(&parts), text.split_whitespace().map(str::to_string).collect::<Vec<_>>());
        assert!(!parts.iter().any(|p| p.contains(CUT_MARK)));
        // A divisão cai na quebra de parágrafo: cada parte começa por um parágrafo.
        assert!(parts.iter().all(|p| p.starts_with("Parágrafo ")), "{parts:?}");
    }

    /// Revisão 1 (F-01): perto do limite, com teclado (folga), a mensagem única vira duas, e a última
    /// cabe com a folga; sem teclado, fica uma só.
    #[test]
    fn near_the_limit_the_keyboard_room_splits_the_single_message() {
        let text = "Uma frase curta e completa. ".repeat(142).trim_end().to_string();
        assert!(units(&text) > MAX_MESSAGE_CHARS - LAST_PART_ROOM && units(&text) <= MAX_MESSAGE_CHARS);
        assert_eq!(split(&text, MAX_MESSAGE_CHARS, 0, MAX_PARTS), [text.clone()]);
        let parts = split(&text, MAX_MESSAGE_CHARS, LAST_PART_ROOM, MAX_PARTS);
        assert_eq!(parts.len(), 2);
        assert!(parts[1].ends_with("completa.") && units(&parts[1]) + LAST_PART_ROOM <= MAX_MESSAGE_CHARS);
        assert!(parts[0].ends_with("completa.") && units(&parts[0]) + LAST_PART_ROOM <= MAX_MESSAGE_CHARS);
        assert_eq!(words_of(&parts), words_of(&[text]));
    }

    /// Acima de três mensagens, a última termina com a marca do corte, sem frase fixa (spec 141 T10).
    #[test]
    fn above_the_ceiling_the_last_part_ends_with_the_cut_mark() {
        let sentence = "O módulo de filas depende do armazenamento e da rede local. ";
        let text = sentence.repeat(400).trim_end().to_string();
        assert!(units(&text) > MAX_PARTS * MAX_MESSAGE_CHARS);
        let parts = split(&text, MAX_MESSAGE_CHARS, LAST_PART_ROOM, MAX_PARTS);
        assert_eq!(parts.len(), MAX_PARTS);
        assert!(parts.iter().all(|p| units(p) <= MAX_MESSAGE_CHARS));
        let last = &parts[MAX_PARTS - 1];
        assert!(last.ends_with(&format!("local.{CUT_MARK}")), "{}", tail_of(last));
        // Sem frase partida: cada parte termina num fim de frase.
        for p in &parts[..MAX_PARTS - 1] {
            assert!(p.ends_with("local."), "{}", tail_of(p));
        }
    }

    /// A divisão nunca parte uma palavra nem o segredo mascarado, mesmo sem quebra nenhuma, com
    /// caracteres de duas unidades, e só corta no limite quando a palavra é maior que a mensagem.
    #[test]
    fn the_split_never_breaks_a_word_nor_a_masked_secret() {
        let secret = format!("{}{}", "sk-", "Q".repeat(30));
        let secrets = SecretSet::new([secret.clone()]);
        let raw = format!("{} fim", format!("palavra {secret} \u{10400}x ").repeat(900));
        let text = clean_whole(&secrets, &raw);
        assert!(!text.contains(&secret) && text.contains("***"));
        let parts = split(&text, MAX_MESSAGE_CHARS, LAST_PART_ROOM, MAX_PARTS);
        assert!(parts.len() == MAX_PARTS && parts.iter().all(|p| units(p) <= MAX_MESSAGE_CHARS));
        let allowed = ["palavra", "***", "\u{10400}x"];
        for word in words_of(&parts).iter().map(|w| w.trim_end_matches(CUT_MARK)).filter(|w| *w != "fim") {
            assert!(allowed.contains(&word), "palavra partida: {word:?}");
        }
        // Uma palavra só, maior que a mensagem: corta no limite, fora da marca.
        let glued = format!("{}{}", "x".repeat(MAX_MESSAGE_CHARS - 2), "*".repeat(10));
        assert_eq!(cut_point(&glued, MAX_MESSAGE_CHARS), MAX_MESSAGE_CHARS - 2);
    }

    /// Um parágrafo de `n` caracteres, em palavras, terminado em ponto.
    fn paragraph(n: usize, seed: usize) -> String {
        let word = ["fila", "disco", "projeto", "teste", "rede", "build", "plano"];
        let mut out = String::new();
        let mut i = seed;
        while out.chars().count() + 8 < n {
            out.push_str(word[i % word.len()]);
            out.push(' ');
            i = i.wrapping_mul(31).wrapping_add(7);
        }
        format!("{}.", out.trim_end())
    }

    /// O texto saiu inteiro nas partes: nenhuma termina com a marca do corte, e as palavras são as
    /// do texto, na ordem.
    fn whole_in(parts: &[String], text: &str) -> bool {
        let words = |t: &str| t.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        !parts.iter().any(|p| p.ends_with(CUT_MARK)) && words(&parts.join("\n")) == words(text)
    }

    /// Spec 141 T08 (revisão 2, F-02): o `fits_parts` diz o mesmo que a divisão real, em textos com
    /// parágrafos de tamanhos variados, com e sem a folga do teclado.
    #[test]
    fn fits_parts_agrees_with_the_real_split() {
        let mut seed: usize = 12345;
        let mut next = |m: usize| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345) % (1 << 31);
            seed % m
        };
        let (mut fit, mut cut) = (0, 0);
        for case in 0..300 {
            let count = 1 + next(14);
            let paragraphs: Vec<String> = (0..count).map(|i| paragraph(80 + next(2400), case * 17 + i)).collect();
            let sep = if case % 3 == 0 { "\n" } else { "\n\n" };
            let body = paragraphs.join(sep);
            for room in [0, LAST_PART_ROOM] {
                let parts = split(&body, MAX_MESSAGE_CHARS, room, MAX_PARTS);
                let expected = whole_in(&parts, &body);
                assert_eq!(fits_parts(&body, room, MAX_PARTS), expected, "caso {case}, {} unidades", units(&body));
                if expected { fit += 1 } else { cut += 1 }
            }
        }
        assert!(fit > 50 && cut > 50, "os dois lados aparecem: {fit} e {cut}");
        // O caso do F-02: abaixo das três partes cheias, mas os parágrafos de 1500 levam a quatro.
        let text = (0..7).map(|i| paragraph(1500, i)).collect::<Vec<_>>().join("\n\n");
        assert!(units(&text) < MAX_PARTS * MAX_MESSAGE_CHARS - LAST_PART_ROOM, "{}", units(&text));
        assert!(!fits_parts(&text, LAST_PART_ROOM, MAX_PARTS));
        assert!(split(&text, MAX_MESSAGE_CHARS, LAST_PART_ROOM, MAX_PARTS).last().is_some_and(|p| p.ends_with(CUT_MARK)));
    }
}
