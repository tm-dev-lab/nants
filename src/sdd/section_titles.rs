//! Spec 133 T08 (C-15): a lista explícita dos títulos de seção ESTRUTURAIS, que o código lê e que
//! ficam exatamente como estão em qualquer idioma, e dos títulos LIVRES, que seguem o idioma do
//! projeto.
//!
//! - Estruturais escritos pelos agentes: o Arquiteto os grava no spec.md, nas tarefas e nos perfis;
//!   o prompt os cita um a um e proíbe traduzi-los ([`STRUCTURAL`]).
//! - Estruturais escritos pelo servidor: o Nants os grava e os relê nas tarefas e no spec.md; nenhum
//!   agente os escreve ([`SERVER_WRITTEN`] e, numerados, [`SERVER_WRITTEN_PREFIXES`]).
//! - Livres: todo outro título (os do modelo de spec, as decisões, as fontes consultadas e qualquer
//!   seção nova). Nenhum leitor do código depende deles; saem do dicionário no idioma do projeto
//!   ([`FREE_KEYS`], [`free_title`]).
//!
//! Os títulos do ARCHITECTURE.md (`agents::arch_memory`) e do relatório final
//! (`agents::prompts::report_titles`) já seguem o idioma do projeto e são lidos nos dois idiomas; os
//! marcadores da resposta do Desenvolvedor e da mensagem do Revisor ficam em `agents::markers`.

use crate::agents::{finding_reply, finding_track, runner};
use crate::i18n::{self, Lang};

use super::{images, role_guidance, spec_sections};

/// Arquivo em que o título estrutural aparece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doc {
    /// `.sdd/specs/<id>/spec.md`.
    Spec,
    /// `.sdd/specs/<id>/tasks/Txx.md`.
    Task,
    /// `.sdd/skills/<papel>.md`.
    Profile,
}

/// Um título estrutural escrito por um agente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Structural {
    /// Nível do cabeçalho (2 = `##`).
    pub level: usize,
    pub title: &'static str,
    pub doc: Doc,
}

const fn s(level: usize, title: &'static str, doc: Doc) -> Structural {
    Structural { level, title, doc }
}

/// Títulos estruturais que o Arquiteto escreve, com quem os lê.
pub const STRUCTURAL: &[Structural] = &[
    // `runner` (verificação final), `sdd::coverage` (`covers`) e o plano na tela.
    s(2, runner::GLOBAL_CRITERIA, Doc::Spec),
    // `sdd::spec_sections` (ordem do briefing), `harness::tools` (validação) e `server::projects`.
    s(2, spec_sections::PREMISES, Doc::Spec),
    // `sdd::role_guidance`, `harness::tools` (validação) e `server::projects`.
    s(2, role_guidance::ROLE_GUIDANCE_SECTION, Doc::Spec),
    s(3, role_guidance::ROLE_DEVELOPER, Doc::Spec),
    s(3, role_guidance::ROLE_REVIEWER, Doc::Spec),
    // `sdd::images` (o guia vai junto de cada pedido de `generate_image`).
    s(2, images::GUIDE_SECTION, Doc::Spec),
    // `runner`, `sdd::scope_warnings`, `sdd::plan_diff`, `harness::tools` (validação) e o relatório.
    s(2, runner::OBJECTIVE, Doc::Task),
    s(2, runner::ACCEPTANCE, Doc::Task),
    // `sdd::skills` (`USER_NOTES_SECTION` e `LESSONS_SECTION`, preservadas literalmente).
    s(2, "Notas do usuário", Doc::Profile),
    s(2, "Lições do projeto", Doc::Profile),
];

/// Títulos estruturais que só o servidor grava (e relê): nenhum agente os escreve nem os traduz.
pub const SERVER_WRITTEN: &[&str] = &[
    runner::FINDINGS,
    runner::FINDINGS_LOG,
    runner::OBSERVATIONS,
    runner::FINAL_OBSERVATIONS,
    runner::PENDING,
    runner::PENDING_RESOLVED,
    runner::HISTORY,
    runner::ADVICE_SECTION,
    runner::GUIDANCE_SECTION,
    runner::INSTALL_SECTION,
    runner::INSTALL_FOOTPRINT_SECTION,
    runner::FORMAT_SECTION,
    runner::REUSE_SECTION,
    runner::FLAKY_SECTION,
    finding_reply::RESPONSES,
    finding_reply::ARBITRATION_LOG,
    finding_reply::SPEC_DECISIONS,
    finding_track::REGISTRY,
];

/// Prefixos dos títulos numerados que só o servidor grava (`## <prefixo><n>`), como a análise de
/// recuperação e o diagnóstico de ambiente do Arquiteto.
pub const SERVER_WRITTEN_PREFIXES: &[&str] = &[runner::RECOVERY_PREFIX, runner::DIAGNOSIS_PREFIX];

/// Título livre do modelo de spec: "Contexto e Problema".
pub const FREE_CONTEXT: &str = "sdd.free_title.context";
/// Título livre do modelo de spec: "Abordagem Técnica".
pub const FREE_APPROACH: &str = "sdd.free_title.approach";
/// Seção livre das decisões da spec (o que não roda no sandbox, nada a executar...).
pub const FREE_DECISIONS: &str = "sdd.free_title.decisions";
/// Seção livre das fontes consultadas na internet (spec 076).
pub const FREE_SOURCES: &str = "sdd.free_title.sources";
/// Exemplo de seção livre que o Objetivo de uma tarefa cita pelo título (spec 131 T01).
pub const FREE_CITED_EXAMPLE: &str = "sdd.free_title.cited_example";

/// Chaves do dicionário de todos os títulos livres que os prompts citam.
pub const FREE_KEYS: &[&str] = &[FREE_CONTEXT, FREE_APPROACH, FREE_DECISIONS, FREE_SOURCES, FREE_CITED_EXAMPLE];

/// Título livre no idioma do projeto.
pub fn free_title(lang: Lang, key: &str) -> String {
    i18n::text(lang, key)
}

/// O título é estrutural (escrito por agente ou pelo servidor)? Compara o texto exato, sem `#`.
pub fn is_structural(title: &str) -> bool {
    let t = title.trim().trim_start_matches('#').trim();
    STRUCTURAL.iter().any(|x| x.title == t)
        || SERVER_WRITTEN.contains(&t)
        || SERVER_WRITTEN_PREFIXES.iter().any(|p| t.strip_prefix(p).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}

/// Linhas de cabeçalho (`## Título`) dos estruturais de um arquivo, na ordem da lista.
pub fn structural_headings(doc: Doc) -> Vec<String> {
    STRUCTURAL
        .iter()
        .filter(|x| x.doc == doc)
        .map(|x| format!("{} {}", "#".repeat(x.level), x.title))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdd::{coverage, scope_warnings, skills, Task};

    #[test]
    fn profile_titles_are_the_ones_skills_reads() {
        let profile = structural_headings(Doc::Profile);
        assert_eq!(profile, vec![skills::USER_NOTES_SECTION.to_string(), skills::LESSONS_SECTION.to_string()]);
    }

    #[test]
    fn free_titles_have_both_languages_and_are_never_structural() {
        for key in FREE_KEYS {
            let pt = free_title(Lang::PtBr, key);
            let en = free_title(Lang::En, key);
            assert!(!pt.is_empty() && !en.is_empty() && pt != en, "{key}");
            assert!(!is_structural(&pt) && !is_structural(&en), "{key}");
        }
        assert!(is_structural("## Critérios de Aceitação Globais") && is_structural("Pendências conhecidas"));
        assert!(!is_structural("Ambiente Docker"));
    }

    #[test]
    fn server_written_covers_registry_and_numbered_sections() {
        assert!(is_structural(&format!("## {}", finding_track::REGISTRY)));
        for prefix in SERVER_WRITTEN_PREFIXES {
            assert!(is_structural(&format!("## {prefix}1")) && is_structural(&format!("{prefix}12")), "{prefix}");
            assert!(!is_structural(prefix) && !is_structural(&format!("{prefix}x")), "{prefix}");
        }
        assert!(SERVER_WRITTEN_PREFIXES.contains(&runner::RECOVERY_PREFIX));
        assert!(SERVER_WRITTEN_PREFIXES.contains(&runner::DIAGNOSIS_PREFIX));
    }

    /// Spec de um projeto em inglês: seções livres em inglês e os títulos estruturais como estão.
    const EN_SPEC: &str = "---\nid: \"001-ledger\"\ntitle: \"Ledger\"\nstatus: \"AWAITING_APPROVAL\"\nbase_branch: \"main\"\ntest_command: \"npm test\"\n---\n\
## Context and Problem\nA small ledger.\n\n\
## Technical Approach\nNode and SQLite.\n\n\
## Account and security contract\nPasswords are hashed with scrypt.\n\n\
## Docker environment\nOne service.\n\n\
## Premissas\n- Single user.\n\n\
## Orientações por papel\n### Desenvolvedor\nUse node:test.\n### Revisor\nCheck the hashing.\n\n\
## Guia de arte\nFlat pastel style.\n\n\
## Decisions\n- 2026-10-28: no login provider.\n\n\
## Sources consulted\n- nodejs.org\n\n\
## Critérios de Aceitação Globais\n- [ ] Login rejects a wrong password.\n- [ ] The balance is never negative.\n";

    const EN_TASK: &str = "---\nid: \"T02\"\nspec_id: \"001-ledger\"\ntitle: \"Accounts\"\nstatus: \"TODO\"\ndepends_on: []\nbranch: \"\"\nattempts: 0\nmax_attempts: 5\nlast_review_verdict: \"NONE\"\nallowed_scope:\n  - \"src/**\"\ncovers: [\"G1\"]\n---\n\
## Objetivo\nImplement the login following Account and security contract.\n\n\
## Critérios de Aceitação\n- [ ] `src/auth.js` rejects a wrong password.\n- [ ] `docs/auth.md` documents the hashing.\n";

    #[test]
    fn english_spec_with_structural_titles_is_read_by_every_reader() {
        // Briefing da spec: a seção livre citada pelo Objetivo entra inteira, logo no começo.
        let task = Task::parse(EN_TASK).expect("tarefa");
        let objective = runner::section_text(&task.body, runner::OBJECTIVE).expect("objetivo");
        let ctx = spec_sections::briefing_context(EN_SPEC, &objective, 600, &[]);
        let contract = ctx.find("### Account and security contract").expect("seção citada no briefing");
        let premises = ctx.find("### Premissas").expect("premissas no briefing");
        assert!(contract < premises, "{ctx}");
        assert!(spec_sections::read_section(EN_SPEC, "Sources consulted").is_ok_and(|s| s.contains("nodejs.org")));
        // Critérios globais (verificação final e cobertura).
        assert_eq!(coverage::global_criteria(EN_SPEC).len(), 2);
        assert!(coverage::coverage_warnings(EN_SPEC, std::slice::from_ref(&task)).iter().all(|w| !w.contains("G1")));
        // Critérios da tarefa e avisos de escopo.
        let acceptance = runner::section_text(&task.body, runner::ACCEPTANCE).expect("critérios");
        assert!(acceptance.contains("src/auth.js"));
        let warnings = scope_warnings::scope_warnings(std::slice::from_ref(&task));
        assert!(warnings.iter().any(|w| w.contains("docs/auth.md")), "{warnings:?}");
        assert!(!warnings.iter().any(|w| w.contains("src/auth.js")), "{warnings:?}");
        // Orientações por papel, Premissas e Guia de arte.
        assert_eq!(role_guidance::role_guidance_of(EN_SPEC, role_guidance::ROLE_DEVELOPER).trim(), "Use node:test.");
        assert!(role_guidance::validate(EN_SPEC));
        assert_eq!(images::art_guide(EN_SPEC), "Flat pastel style.");
    }
}
