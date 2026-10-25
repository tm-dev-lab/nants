//! Spec 125: trava dos títulos estruturais do `ARCHITECTURE.md`.
//!
//! Os sete títulos das seções (Visão geral, Stack, Estrutura, Comandos, Padrões e convenções,
//! Decisões, Restrições) saem do dicionário (`architecture.section.*`, em `src/i18n/*.rs`), no
//! idioma do projeto, e a leitura aceita os dois idiomas pelas mesmas chaves. Nenhum literal de
//! produção pode trazer um desses títulos como `## <título>` fixo num idioma: foi assim que o
//! ARCHITECTURE.md de um projeto en saiu com os títulos em pt (Test 3).
//!
//! Lê os literais de produção de `src/**/*.rs` (sem comentários, sem `#[cfg(test)]`/`mod tests`
//! e sem os `*_tests.rs`), inclusive os prompts. Exceção só com motivo, e exceção sem uso falha.

#[path = "support/rust_tokens.rs"]
mod rust_tokens;

use std::collections::BTreeSet;

use nants::i18n::{lookup, Lang};
use rust_tokens::{production_sources, production_tokens};

/// Chaves dos títulos das seções no dicionário.
const SECTION_KEYS: [&str; 7] = [
    "architecture.section.overview",
    "architecture.section.stack",
    "architecture.section.structure",
    "architecture.section.commands",
    "architecture.section.conventions",
    "architecture.section.decisions",
    "architecture.section.constraints",
];

/// Arquivos fora da varredura, com o motivo.
const SKIPPED_FILES: &[(&str, &str)] = &[
    ("i18n/pt_br.rs", "dicionário pt-BR: é a fonte dos títulos em português"),
    ("i18n/en.rs", "dicionário en: é a fonte dos títulos em inglês"),
];

/// Literais permitidos (arquivo, título achado, motivo). Hoje vazia: a leitura dos dois idiomas
/// também usa o dicionário (`arch_memory::parse_heading`).
const EXCEPTIONS: &[(&str, &str, &str)] = &[];

/// Títulos `## <título>` fixos achados no literal. O título conta quando termina o literal ou é
/// seguido de quebra de linha, pontuação de lista ou do complemento entre parênteses; um título
/// mais longo que só começa igual (ex.: `## Decisões do Arquiteto sobre ...`) não é o da seção.
fn fixed_headings(literal: &str) -> Vec<String> {
    let mut out = Vec::new();
    for key in SECTION_KEYS {
        // Um título igual nos dois idiomas ("Stack") conta uma vez.
        let titles: BTreeSet<&str> = [Lang::PtBr, Lang::En].into_iter().filter_map(|lang| lookup(lang, key)).collect();
        for title in titles {
            let heading = format!("## {title}");
            for (at, _) in literal.match_indices(&heading) {
                let rest = &literal[at + heading.len()..];
                let ends = rest.is_empty() || ["\n", "\r", ",", ";", ".", "`", "\"", "(", " ("].iter().any(|p| rest.starts_with(p));
                if ends {
                    out.push(heading.clone());
                }
            }
        }
    }
    out
}

#[test]
fn every_section_key_exists_in_both_dictionaries() {
    for key in SECTION_KEYS {
        assert!(lookup(Lang::PtBr, key).is_some(), "{key} sem pt-BR");
        assert!(lookup(Lang::En, key).is_some(), "{key} sem en");
    }
}

#[test]
fn detector_catches_the_old_fixed_headings() {
    assert_eq!(fixed_headings("## Visão geral, ## Stack, ## Estrutura"), ["## Visão geral", "## Stack", "## Estrutura"]);
    assert_eq!(fixed_headings("x\n## Comandos (instalar, testar, executar)\n"), ["## Comandos"]);
    assert_eq!(fixed_headings("## Decisions\n## Constraints"), ["## Decisions", "## Constraints"]);
    assert!(fixed_headings("## Decisões do Arquiteto sobre findings contestados").is_empty());
    assert!(fixed_headings("## Stacks e ## Overviewing").is_empty());
}

#[test]
fn no_fixed_architecture_heading_in_production_code() {
    let skipped: BTreeSet<&str> = SKIPPED_FILES.iter().map(|(f, _)| *f).collect();
    let mut found: Vec<(String, usize, String)> = Vec::new();
    let mut used: BTreeSet<(&str, &str)> = BTreeSet::new();
    for (rel, text) in production_sources() {
        if skipped.contains(rel.as_str()) {
            continue;
        }
        for tok in production_tokens(&text) {
            let Some(literal) = tok.str() else { continue };
            for heading in fixed_headings(literal) {
                match EXCEPTIONS.iter().find(|(f, h, _)| *f == rel && *h == heading) {
                    Some((f, h, _)) => {
                        used.insert((*f, *h));
                    }
                    None => found.push((rel.clone(), tok.line, heading)),
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "título do ARCHITECTURE.md fixo num idioma (use as chaves architecture.section.* e arch_memory): {found:?}"
    );
    for (file, heading, why) in EXCEPTIONS {
        assert!(why.len() > 10, "{file} {heading}: motivo curto");
        assert!(used.contains(&(*file, *heading)), "exceção sem uso: {file} {heading}");
    }
    for (file, why) in SKIPPED_FILES {
        assert!(why.len() > 10, "{file}: motivo curto");
    }
}
