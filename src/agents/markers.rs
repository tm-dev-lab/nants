//! Marcadores estruturais no idioma do projeto (spec 129 T04).
//!
//! São os títulos e rótulos que os prompts pedem ao Desenvolvedor ("Critérios", "Respostas aos
//! apontamentos") e os que o servidor monta na mensagem do Revisor ("Apontamentos:", "Resolvidos:",
//! "Capturas avaliadas:"...). Eles chegam à tela, então saem do dicionário, no idioma do projeto
//! ([`text`]). Quem os lê aceita a forma de TODOS os idiomas do dicionário ([`crate::i18n::forms`]),
//! nunca uma lista avulsa: um registro gravado em pt continua legível num projeto em inglês e
//! vice-versa. A comparação é pela linha inteira e sem diferenciar maiúsculas nem acentos, para uma
//! frase da prosa que cite a palavra não virar marcador.

use crate::i18n::{self, Lang};

/// Título da seção de critérios da resposta final do Desenvolvedor; na mensagem do Revisor, vem
/// seguido de `:` e da matriz.
pub const CRITERIA: &str = "runner.marker.criteria";
/// Situação do critério atendido na linha do Desenvolvedor (`- critério: atendido: evidência`).
pub const MET: &str = "runner.marker.met";
/// Situação do critério não atendido na linha do Desenvolvedor.
pub const NOT_MET: &str = "runner.marker.not_met";
/// Título da seção da resposta final do Desenvolvedor com as respostas aos apontamentos.
pub const DEV_RESPONSES: &str = "runner.marker.dev_responses";
/// Rótulos do veredito na primeira linha da mensagem do Revisor (spec 078).
pub const VERDICT_APPROVED: &str = "runner.marker.verdict_approved";
pub const VERDICT_CHANGES: &str = "runner.marker.verdict_changes";
/// Seções da mensagem do Revisor.
pub const FINDINGS: &str = "runner.marker.findings";
pub const OBSERVATIONS: &str = "runner.marker.observations";
pub const RESOLVED: &str = "runner.marker.resolved";
pub const RESPONSES: &str = "runner.marker.responses";
pub const DECISIONS: &str = "runner.marker.decisions";
pub const CAPTURES: &str = "runner.marker.captures";
/// Prefixo da linha recuada do cenário de falha (spec 087 T05).
pub const SCENARIO: &str = "runner.marker.scenario";
/// Marcas junto da severidade.
pub const LOW_CONFIDENCE: &str = "runner.marker.low_confidence";
pub const UNCHANGED_FILE: &str = "runner.marker.unchanged_file";
pub const STATE_NEW: &str = "runner.marker.state_new";
/// Base do plural `aberto há N revisões` (`.one` e `.other`).
pub const STATE_OPEN: &str = "runner.marker.state_open";
/// Linha que precede a matriz da verificação final (`{met}` e `{total}`).
pub const FINAL_VERIFICATION: &str = "runner.marker.final_verification";
/// Marca acrescentada aos critérios `(manual)` enviados ao Revisor, entre colchetes.
pub const HUMAN_CHECK: &str = "runner.marker.human_check";
/// Evidência do critério que o Revisor esgotado não chegou a verificar (vai à matriz na tela).
pub const UNVERIFIED_STEP_LIMIT: &str = "runner.marker.unverified_step_limit";
/// Primeira linha da resposta do Desenvolvedor que esgotou os passos (`{reason}`).
pub const STEP_LIMIT: &str = "runner.marker.step_limit";

/// Texto do marcador no idioma do projeto.
pub fn text(lang: Lang, key: &str) -> String {
    i18n::text(lang, key)
}

/// Minúsculas e sem acentos do português, para comparar formas ("Critérios" = "criterios").
fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' => 'a',
            'é' | 'ê' => 'e',
            'í' => 'i',
            'ó' | 'ô' | 'õ' => 'o',
            'ú' | 'ü' => 'u',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

/// Linha sem a decoração de título: `#`, negrito/itálico (`*`, `_`) e o `:` final.
fn bare_title(line: &str) -> &str {
    line.trim().trim_start_matches('#').trim().trim_matches(['*', '_']).trim().trim_end_matches(':').trim()
}

/// A linha inteira é o título do marcador `key`, em qualquer idioma do dicionário: aceita `## `,
/// negrito e `:` no fim, sem diferenciar maiúsculas nem acentos. Uma frase que só contenha a
/// palavra não conta.
pub fn is_heading(line: &str, key: &str) -> bool {
    let title = fold(bare_title(line));
    !title.is_empty() && i18n::forms(key).iter().any(|form| fold(bare_title(form)) == title)
}

/// A linha termina com `[<marcador>]` em algum idioma do dicionário (sem diferenciar maiúsculas).
pub fn ends_with_bracketed(line: &str, key: &str) -> bool {
    let end = fold(line.trim_end());
    i18n::forms(key).iter().any(|form| end.ends_with(&format!("[{}]", fold(form))))
}

/// A linha contém o marcador `key` em algum idioma do dicionário (sem diferenciar maiúsculas).
pub fn contains_any(line: &str, key: &str) -> bool {
    let text = fold(line);
    i18n::forms(key).iter().any(|form| text.contains(&fold(form)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_accepts_every_dictionary_form_by_whole_line() {
        for line in ["Critérios", "## Critérios", "**Critérios:**", "criterios", "CRITERIA", "### Criteria:", "Criteria"] {
            assert!(is_heading(line, CRITERIA), "{line}");
        }
        for line in ["Os critérios foram atendidos", "## Acceptance Criteria", "Critérios de Aceitação", "", "##"] {
            assert!(!is_heading(line, CRITERIA), "{line}");
        }
        for line in ["## Respostas aos apontamentos", "Responses to findings:", "**responses to findings**"] {
            assert!(is_heading(line, DEV_RESPONSES), "{line}");
        }
        assert!(!is_heading("I wrote responses to findings below", DEV_RESPONSES));
    }

    #[test]
    fn every_marker_has_a_form_per_language() {
        for key in [CRITERIA, MET, NOT_MET, DEV_RESPONSES, VERDICT_APPROVED, VERDICT_CHANGES, FINDINGS, OBSERVATIONS, RESOLVED]
            .into_iter()
            .chain([RESPONSES, DECISIONS, CAPTURES, SCENARIO, LOW_CONFIDENCE, UNCHANGED_FILE, STATE_NEW, FINAL_VERIFICATION, HUMAN_CHECK, STEP_LIMIT, UNVERIFIED_STEP_LIMIT])
        {
            assert_eq!(i18n::forms(key).len(), Lang::ALL.len(), "{key}");
            assert_ne!(text(Lang::PtBr, key), key, "{key} sem texto");
        }
        assert_eq!(i18n::forms(&format!("{STATE_OPEN}.other")).len(), 2);
    }

    #[test]
    fn bracketed_and_contains_match_both_languages() {
        assert!(ends_with_bracketed("- (manual) abrir [verificação humana, não bloqueia]", HUMAN_CHECK));
        assert!(ends_with_bracketed("- (manual) open it [Human check, does not block]  ", HUMAN_CHECK));
        assert!(!ends_with_bracketed("- abrir [outra coisa]", HUMAN_CHECK));
        assert!(contains_any("x [human check, does not block] y", HUMAN_CHECK));
        assert!(!contains_any("x y", HUMAN_CHECK));
    }
}
