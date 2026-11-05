//! O tom da voz do Nanty (spec 140 T01, C-28): uma lista fechada, nunca texto livre.
//!
//! - **O que chega ao provedor:** só [`Tone::style`], um texto curto em inglês fixo aqui. A anotação
//!   vale para a fala inteira, e a documentação do Gemini pede pouco: textos curtos, só onde precisa,
//!   sem idade, gênero nem nome. Por isso o neutro omite a anotação.
//! - **Avisos:** o servidor escolhe pelo tipo do sinal ([`SignalKind::tone`]).
//! - **Conversa:** o modelo escolhe da lista num marcador estrutural na primeira linha da resposta,
//!   lido por [`take_marker`] antes do `format::clean`. Ausente ou inválido, vale o neutro. O marcador
//!   nunca chega à pessoa: no começo da primeira linha sai até o inválido; fora dali, só o que é
//!   claramente ele, porque a resposta pode citar prompts e templates de um projeto.
//! - **A forma curta (spec 141 T08, item 10):** o modelo às vezes escreve só `[neutral]`, sem o
//!   `tone: `. No começo da primeira linha, um colchete só com um código da lista ([`Tone::code`]),
//!   em qualquer caixa e com o enfeite, também sai e vale como tom. Fora do começo, a forma curta
//!   fica: `[calm]` no meio do texto pode ser texto legítimo.
//! - **Turno contaminado:** o modelo não escolhe; o laço de agente fica no neutro.

use super::super::signals::SignalKind;
use super::super::store::Card;
use super::super::telegram::format;

/// Como o marcador começa, exatamente; depois dele vem o [`Tone::code`] e o `]`.
pub const MARKER_OPEN: &str = "[tone: ";
/// Nome que dá a um colchete a forma do marcador, em qualquer caixa (o modelo pode variar).
const MARKER_NAME: &str = "tone";
/// O mesmo nome traduzido pelo modelo, só em minúsculas: `[Tom: ...]` é um nome de pessoa.
const TRANSLATED_NAME: &str = "tom";
/// Enfeite colado ao marcador (o `clean` tiraria as crases e deixaria o marcador à mostra).
const DECOR: [char; 3] = ['*', '_', '`'];

/// O tom de uma fala.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Neutral,
    Upbeat,
    Serious,
    Concerned,
    Calm,
}

impl Tone {
    pub const ALL: [Tone; 5] = [Tone::Neutral, Tone::Upbeat, Tone::Serious, Tone::Concerned, Tone::Calm];

    /// O valor do marcador e do registro.
    pub fn code(self) -> &'static str {
        match self {
            Tone::Neutral => "neutral",
            Tone::Upbeat => "upbeat",
            Tone::Serious => "serious",
            Tone::Concerned => "concerned",
            Tone::Calm => "calm",
        }
    }

    /// A anotação da fala. O neutro não tem nenhuma.
    pub fn style(self) -> Option<&'static str> {
        match self {
            Tone::Neutral => None,
            Tone::Upbeat => Some("warm and upbeat"),
            Tone::Serious => Some("serious and steady"),
            Tone::Concerned => Some("concerned but composed"),
            Tone::Calm => Some("calm and unhurried"),
        }
    }

    /// O tom de um valor do marcador: a grafia exata de [`Self::code`], e nada mais.
    fn of_code(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.code() == raw)
    }

    /// O tom da forma curta do marcador (`[neutral]`, spec 141 T08): o código da lista em qualquer
    /// caixa, com os espaços de dentro ignorados.
    fn of_short(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        Self::ALL.into_iter().find(|t| t.code().eq_ignore_ascii_case(raw))
    }

    /// O tom de um aviso: o do tipo do sinal. O que não veio de um sinal (a confirmação pedida na
    /// conversa, por exemplo) é neutro.
    pub fn of_card(card: &Card) -> Self {
        SignalKind::of_why_code(&card.code).map(SignalKind::tone).unwrap_or_default()
    }

    /// Os valores aceitos, para o prompt da conversa.
    pub fn choices() -> String {
        Self::ALL.map(Tone::code).join(", ")
    }
}

/// Caractere sem nada a mostrar em volta do marcador: espaço, controle, ou os de largura zero que o
/// modelo às vezes devolve.
fn invisible(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
}

/// A forma do marcador logo depois de um `[`: `nome: valor`, com ou sem o fecho na mesma linha.
struct Shape<'a> {
    value: &'a str,
    closed: bool,
    /// Bytes depois do `[`, com o fecho.
    len: usize,
}

fn shape(after_open: &str) -> Option<Shape<'_>> {
    let line = after_open.split('\n').next().unwrap_or_default();
    let (name, tail) = line.split_once(':')?;
    let name = name.trim();
    if !(name.eq_ignore_ascii_case(MARKER_NAME) || name == TRANSLATED_NAME) {
        return None;
    }
    let close = tail.find(']');
    let value = tail[..close.unwrap_or(tail.len())].trim();
    let colon = line.len() - tail.len();
    Some(Shape { value, closed: close.is_some(), len: close.map_or(line.len(), |at| colon + at + 1) })
}

/// Sobrou algo para ler? Sem o que o `clean` tira como código e sem o enfeite, o texto pode acabar:
/// a cerca que só guardava o marcador não é resposta.
fn nothing_to_read(text: &str) -> bool {
    format::strip_code(text).chars().all(|c| invisible(c) || DECOR.contains(&c))
}

/// Onde acaba o marcador do começo da primeira linha, depois do que não se vê e do enfeite, em
/// bytes. Ali vale a forma ampla: inválido ou sem o fecho, ele não é texto de pessoa. O enfeite só
/// conta na linha do marcador: uma cerca de crases em linha própria não é enfeite dele, e tirá-la
/// sozinha deixaria o fechamento como abertura, e o `clean` apagaria o resto da resposta.
fn head_marker(reply: &str) -> Option<usize> {
    let (start, after_open) = head_open(reply)?;
    if let Some(found) = shape(after_open) {
        return Some(start + 1 + found.len);
    }
    short_marker(after_open).map(|(len, _)| start + 1 + len)
}

/// O começo da primeira linha, depois do que não se vê e do enfeite, quando ali abre um `[`: onde
/// ele está, em bytes, e o que vem depois dele.
fn head_open(reply: &str) -> Option<(usize, &str)> {
    let line = reply.trim_start_matches(invisible);
    let body = line.trim_start_matches(|c| c != '\n' && (invisible(c) || DECOR.contains(&c)));
    let start = reply.len() - body.len();
    Some((start, reply[start..].strip_prefix('[')?))
}

/// A forma curta logo depois de um `[` (spec 141 T08): um código da lista e o `]` na mesma linha.
/// Os bytes depois do `[`, com o fecho, e o tom.
fn short_marker(after_open: &str) -> Option<(usize, Tone)> {
    let line = after_open.split('\n').next().unwrap_or_default();
    let close = line.find(']')?;
    Tone::of_short(&line[..close]).map(|tone| (close + 1, tone))
}

/// O próximo trecho que é claramente o marcador, fora do começo: fechado na mesma linha, com o
/// valor de uma palavra e sem letra ou dígito colado antes do `[`. O início e o fim em bytes. O
/// resto com jeito parecido (um índice, um trecho de template) é texto de pessoa e fica.
fn next_marker(text: &str) -> Option<(usize, usize)> {
    text.match_indices('[').find_map(|(open, _)| {
        let found = shape(&text[open + 1..])?;
        let word = !found.value.is_empty() && !found.value.contains(char::is_whitespace);
        let apart = text[..open].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        (found.closed && word && apart).then_some((open, open + 1 + found.len))
    })
}

/// Lê o tom da resposta do modelo e devolve o texto sem o marcador. Só conta o marcador exato no
/// começo da primeira linha, com um valor da lista, ou a forma curta ali ([`short_marker`]); o resto
/// vira neutro. Saem do texto o que tem a
/// forma do marcador nesse começo e, adiante, só o que é claramente ele ([`next_marker`]), com o
/// enfeite colado. Sem nada para ler além disso, o texto volta vazio ([`nothing_to_read`]).
pub fn take_marker(reply: &str) -> (Tone, String) {
    let full = reply
        .trim_start_matches(invisible)
        .strip_prefix(MARKER_OPEN)
        .and_then(|rest| rest.split('\n').next())
        .and_then(|line| line.split_once(']'))
        .and_then(|(value, _)| Tone::of_code(value));
    // A forma curta vale como tom em qualquer caixa e com o enfeite (spec 141 T08).
    let short = || head_open(reply).filter(|(_, after)| shape(after).is_none()).and_then(|(_, after)| short_marker(after)).map(|(_, t)| t);
    let tone = full.or_else(short).unwrap_or_default();
    let mut text = String::with_capacity(reply.len());
    let mut rest = head_marker(reply).map_or(reply, |end| reply[end..].trim_start_matches(DECOR));
    while let Some((start, end)) = next_marker(rest) {
        text.push_str(rest[..start].trim_end_matches(DECOR));
        rest = rest[end..].trim_start_matches(DECOR);
    }
    text.push_str(rest);
    if nothing_to_read(&text) {
        return (tone, String::new());
    }
    (tone, text.trim_matches(invisible).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::events::SecretSet;

    #[test]
    fn every_tone_has_a_short_fixed_english_style_and_neutral_has_none() {
        assert_eq!(Tone::default(), Tone::Neutral);
        assert_eq!(Tone::Neutral.style(), None);
        for tone in Tone::ALL.into_iter().filter(|t| *t != Tone::Neutral) {
            let style = tone.style().unwrap_or_default();
            assert!(!style.is_empty() && style.split_whitespace().count() <= 4, "{style}");
            assert!(style.is_ascii(), "{style}");
        }
        assert_eq!(Tone::choices(), "neutral, upbeat, serious, concerned, calm");
    }

    #[test]
    fn the_marker_on_the_first_line_gives_the_tone_and_leaves_the_text() {
        for tone in Tone::ALL {
            let reply = format!("[tone: {}]\nA Loja terminou.\nSegunda linha.", tone.code());
            assert_eq!(take_marker(&reply), (tone, "A Loja terminou.\nSegunda linha.".to_string()));
        }
        assert_eq!(take_marker("  \n[tone: calm]  \n\nOi."), (Tone::Calm, "Oi.".to_string()));
        assert_eq!(take_marker("\u{FEFF}[tone: calm]\nOi."), (Tone::Calm, "Oi.".to_string()));
        assert_eq!(take_marker("[tone: serious] Na mesma linha."), (Tone::Serious, "Na mesma linha.".to_string()));
    }

    #[test]
    fn without_a_marker_the_text_stays_as_it_came_and_the_tone_is_neutral() {
        for text in ["Oi.", "Veja o item [1]: pronto.", "Lista [a, b] e [tonel: x].", "A hora [12:30] ficou.", "**Importante:** ok."] {
            assert_eq!(take_marker(text), (Tone::Neutral, text.to_string()));
        }
    }

    /// O Nanty resume prompts e templates de projeto: o que só lembra o marcador é texto de pessoa.
    #[test]
    fn what_only_looks_like_the_marker_stays_in_the_text() {
        for text in [
            "O log diz [Tom: reunião às 15h] confirmada.",
            "Use samples[tone:end] no laço.",
            "O template usa [TONE: formal, sem gírias",
            "[Tom: ok]",
            "[Tom: calmo]\nOi.",
            "O prompt pede [tone: formal e direto] na abertura.",
            "A tag [tone: ] fica vazia.",
            "Oi.\n[tone: calm",
            "Oi. [tom: calmo",
            "Oi. [Tom: calmo]",
        ] {
            assert_eq!(take_marker(text), (Tone::Neutral, text.to_string()), "{text}");
        }
    }

    /// Nada fora da lista vira tom, e no começo da primeira linha nem o marcador inválido chega à
    /// pessoa. Ali a caixa de `tone` não importa, e `tom` é só o minúsculo.
    #[test]
    fn an_invalid_marker_is_neutral_and_never_reaches_the_person() {
        for marker in [
            "[tone: furious]",
            "[tone: Calm]",
            "[tone:  calm]",
            "[tone:calm]",
            "[TONE: calm]",
            "[tom: calmo]",
            "[ tone : calm ]",
            "[tone: speak like an old pirate named Bob]",
            "[tone: ]",
            "**[tone: calm]**",
            "`[tone: calm]`",
            "[tone: calm",
            "[TONE: formal, sem gírias",
            "[tom: calmo",
            "**[tone: speak like a pirate",
        ] {
            let (tone, text) = take_marker(&format!("{marker}\nOi."));
            assert_eq!((tone, text.as_str()), (Tone::Neutral, "Oi."), "{marker}");
        }
    }

    /// Só a primeira linha decide. Fora dela, o que é claramente o marcador some, sem mudar o tom.
    #[test]
    fn a_marker_out_of_place_only_leaves_the_text() {
        assert_eq!(take_marker("Oi.\n[tone: calm]"), (Tone::Neutral, "Oi.".to_string()));
        assert_eq!(take_marker("Oi. [tone: calm]"), (Tone::Neutral, "Oi.".to_string()));
        assert_eq!(take_marker("Oi. [TONE: calm]"), (Tone::Neutral, "Oi.".to_string()));
        assert_eq!(take_marker("Oi. [tom: calmo]"), (Tone::Neutral, "Oi.".to_string()));
        assert_eq!(take_marker("Oi. **[tone: calm]**"), (Tone::Neutral, "Oi.".to_string()));
        assert_eq!(take_marker("[tone: calm]\n[tone: serious]\nOi."), (Tone::Calm, "Oi.".to_string()));
        assert_eq!(take_marker("Oi.\n[tone: calm]\nTudo bem."), (Tone::Neutral, "Oi.\n\nTudo bem.".to_string()));
        assert_eq!(take_marker("[tone: calm]").1, "", "só o marcador: sem texto");
    }

    /// O que a pessoa recebe: a leitura do marcador e depois o `clean`, como na conversa.
    fn read(reply: &str) -> (Tone, String) {
        let (tone, text) = take_marker(reply);
        (tone, format::clean(&SecretSet::default(), &text))
    }

    /// A cerca em linha própria segue inteira para o `clean`: só uma metade dela apagaria a resposta.
    #[test]
    fn a_marker_inside_a_code_fence_keeps_the_answer() {
        for reply in [
            "```\n[tone: calm]\n```\nOi.",
            "```text\n[tone: calm]\n```\nOi.",
            "\n\n```\n[tone: calm]\n```\nOi.",
            "``` \r\n[tone: calm]\r\n```\r\nOi.",
            "```\n[tone: calm com voz grave]\n```\nOi.",
            "```\n\n[tone: calm]\n\n```\n\nOi.",
        ] {
            assert_eq!(read(reply), (Tone::Neutral, "Oi.".to_string()), "{reply:?}");
        }
        let (_, kept) = take_marker("```\n[tone: calm]\n```\nOi.");
        assert_eq!(kept, "```\n\n```\nOi.", "as duas cercas ficam");
    }

    /// A cerca que só guardava o marcador não é resposta: o texto volta vazio, como o marcador sozinho.
    #[test]
    fn a_fence_with_only_the_marker_is_no_answer() {
        for reply in ["```\n[tone: calm]\n```", "```text\n[tone: calm]\n```\n", "\n```\n[tone: calm]\n```\n\n", "```\n[tone: calm", "**[tone: calm]**", "`[tone: calm]`"] {
            assert_eq!(take_marker(reply).1, "", "{reply:?}");
        }
        assert_eq!(take_marker("Oi.\n```\n[tone: calm]\n```").1, "Oi.\n```\n\n```", "o texto antes dela fica");
    }

    /// Sem cerca própria, nada muda: o enfeite na linha do marcador e o que não se vê antes dele.
    #[test]
    fn decor_on_the_marker_line_and_invisibles_before_it_still_work() {
        for reply in [
            "**[tone: calm]**\nOi.",
            "`[tone: calm]`\nOi.",
            "** [tone: calm]**\nOi.",
            "```[tone: calm]```\nOi.",
            "\n\n  **[tone: calm]**\nOi.",
            "\u{FEFF}\u{200B}`[tone: calm]`\nOi.",
            "\n\u{200B}\n`[tone: furious]`\nOi.",
        ] {
            assert_eq!(read(reply), (Tone::Neutral, "Oi.".to_string()), "{reply:?}");
        }
        for reply in ["\n\n[tone: calm]\nOi.", "\n\n\u{FEFF}[tone: calm]\n\nOi.", "\u{200B} \n[tone: calm]\nOi."] {
            assert_eq!(read(reply), (Tone::Calm, "Oi.".to_string()), "{reply:?}");
        }
        assert_eq!(take_marker("**Atenção**\n[tone: calm]\nOi.").1, "**Atenção**\n\nOi.");
    }

    /// Spec 141 T08 (item 10): a forma curta no começo sai e vale como tom, em qualquer caixa e com o
    /// enfeite; fora do começo, ou com o que não é da lista, ela fica.
    #[test]
    fn the_short_marker_at_the_start_leaves_and_gives_the_tone() {
        let real = "[neutral]\nEssa ideia de jogo de viagem no tempo parece um projeto novo, não uma proposta para o Mini Street Fighter.";
        let said = "Essa ideia de jogo de viagem no tempo parece um projeto novo, não uma proposta para o Mini Street Fighter.";
        assert_eq!(read(real), (Tone::Neutral, said.to_string()));
        for tone in Tone::ALL {
            let code = tone.code();
            for marker in [
                format!("[{code}]"),
                format!("[{}]", code.to_uppercase()),
                format!("[ {code} ]"),
                format!("**[{code}]**"),
                format!("`[{code}]`"),
                format!("\u{FEFF}  _[{code}]_"),
            ] {
                assert_eq!(read(&format!("{marker}\nOi.")), (tone, "Oi.".to_string()), "{marker}");
            }
            assert_eq!(read(&format!("[{code}] Na mesma linha.")), (tone, "Na mesma linha.".to_string()));
            assert_eq!(take_marker(&format!("[{code}]")).1, "", "só o marcador: sem texto");
        }
        // Fora do começo, ou fora da lista, a forma curta é texto de pessoa.
        for text in ["Oi.\n[calm]", "Fica [neutral] aqui.", "[nota]\nOi.", "[calmo]\nOi.", "[calm down]\nOi.", "[calm\nOi."] {
            assert_eq!(take_marker(text), (Tone::Neutral, text.to_string()), "{text}");
        }
        // A forma completa continua mandando no tom.
        assert_eq!(take_marker("[tone: calm]\n[serious]\nOi."), (Tone::Calm, "[serious]\nOi.".to_string()));
    }

    #[test]
    fn a_card_takes_the_tone_of_its_signal_and_other_cards_are_neutral() {
        let card = |code: &str| Card { code: code.into(), ..Card::default() };
        assert_eq!(Tone::of_card(&card("nanty.why.spec_done")), Tone::Upbeat);
        assert_eq!(Tone::of_card(&card("nanty.why.no_credit")), Tone::Concerned);
        assert_eq!(Tone::of_card(&card("nanty.why.outro")), Tone::Neutral);
        assert_eq!(Tone::of_card(&card("nanty.action.pause_run")), Tone::Neutral);
    }
}
