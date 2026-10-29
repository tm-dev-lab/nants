//! Testes do catálogo e do bloco da base nos briefings (spec 134 T05).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::kb::{add_file, update, write_overview, Patch};

const NOW: &str = "2026-10-28T12:00:00Z";

fn root(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("nants-kbcat-{}-{n}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn none() -> SecretSet {
    SecretSet::new(Vec::<String>::new())
}

fn md(root: &Path, name: &str, text: &str) -> String {
    add_file(root, name, text.as_bytes(), &none(), NOW).expect("add").id
}

fn standard(root: &Path, id: &str, globs: &[&str], description: &str) {
    let patch = Patch {
        role: Some(Role::Standard),
        description: Some(description.to_string()),
        applies_to: Some(globs.iter().map(|g| g.to_string()).collect()),
    };
    update(root, id, &patch).expect("padrão");
}

#[test]
fn empty_base_gives_nothing() {
    let root = root("empty");
    assert_eq!(render(&root, &none()), "");
    assert_eq!(architect_message(""), "");
    assert_eq!(plan_review_message(""), "");
    let input = BriefInput { task_text: "kb:x-0123abcd#a", spec_text: "", scope: &[], standards: &[], reviewer: true, max: 1000 };
    assert_eq!(briefing_block(&root, &input, &none()), BriefBlock::default());
}

#[test]
fn catalog_lists_standards_first_with_every_field_and_overview_on_top() {
    let root = root("lines");
    let reference = md(&root, "guia.md", "# Guia\nTexto do guia.\n\n# Glossário\nTermos.\n");
    let rules = md(&root, "regras.md", "Intro.\n\n## Filas\nUse filas.\n\n## Logs\nLogs em JSON.\n");
    standard(&root, &rules, &["src/**", "tests/**"], "Regras de arquitetura do time");
    write_overview(&root, "Base da empresa X: padrões de backend.").expect("visão geral");
    let text = render(&root, &none());
    assert!(text.starts_with("Visão geral da base"), "{text}");
    assert!(text.contains("Base da empresa X"), "{text}");
    let at_rules = text.find(&format!("- kb:{rules} |")).expect("padrão");
    let at_ref = text.find(&format!("- kb:{reference} |")).expect("referência");
    assert!(at_rules < at_ref, "padrões primeiro: {text}");
    let line = text.lines().find(|l| l.starts_with(&format!("- kb:{rules} |"))).expect("linha");
    for part in ["[padrão]", "| markdown |", " B |", "títulos: Filas; Logs", "Regras de arquitetura do time", "aplica-se a: src/**, tests/**"] {
        assert!(line.contains(part), "{part}: {line}");
    }
    let line = text.lines().find(|l| l.starts_with(&format!("- kb:{reference} |"))).expect("linha");
    assert!(line.contains("títulos: Guia; Glossário") && !line.contains("[padrão]") && !line.contains("aplica-se"), "{line}");
    // Mesmo estado, mesmo texto (prefixo estável).
    assert_eq!(render(&root, &none()), text);
    let msg = architect_message(&text);
    assert!(msg.starts_with(ARCHITECT_RULES) && msg.contains(CATALOG_LABEL) && msg.ends_with(&text));
    assert!(CATALOG_LABEL.contains("DADO NÃO CONFIÁVEL"));
}

#[test]
fn long_catalog_keeps_only_titles_within_the_ceiling() {
    let root = root("ceiling");
    let long = "d".repeat(190);
    for i in 0..60 {
        let id = md(&root, &format!("doc{i}.md"), &format!("# Seção {i}\nTexto {i} único {}.\n", "x".repeat(i)));
        update(&root, &id, &Patch { role: None, description: Some(long.clone()), applies_to: None }).expect("descrição");
    }
    write_overview(&root, &"visão ".repeat(2000)).expect("visão geral");
    let text = render(&root, &none());
    assert!(text.len() <= MAX_CATALOG_BYTES, "{}", text.len());
    assert!(text.contains("Catálogo longo") && text.contains("use kb_search"), "{text}");
    assert!(text.contains("[visão geral cortada]"), "visão geral no mesmo teto");
    assert!(!text.contains(&long), "sem as descrições");
    // Muitos documentos: o fim diz quantos ficaram de fora.
    for i in 60..200 {
        md(&root, &format!("extra{i}.md"), &format!("# Extra {i}\nConteúdo {i} {}.\n", "y".repeat(i)));
    }
    let text = render(&root, &none());
    assert!(text.len() <= MAX_CATALOG_BYTES, "{}", text.len());
    assert!(text.contains("(e mais ") && text.ends_with("documentos: use kb_search)"), "{text}");
}

#[test]
fn globs_overlap_is_conservative() {
    assert!(globs_overlap("src/**", "src/agents/**"));
    assert!(globs_overlap("src/agents/**", "src/**"));
    assert!(globs_overlap("src/agents/x.rs", "src/**"));
    assert!(globs_overlap("src/**", "src/agents/x.rs"));
    assert!(globs_overlap("src/agents", "src/agents/**"));
    assert!(globs_overlap("**/*.rs", "web/**"), "curinga no começo cruza tudo");
    assert!(!globs_overlap("src/**", "web/**"));
    assert!(!globs_overlap("src/a.rs", "src/b.rs"));
    assert!(!globs_overlap("docs/**", "src/kb/mod.rs"));
    assert!(!globs_overlap("", "src/**"));
}

fn std_ref(id: &str, globs: &[&str]) -> StandardRef {
    StandardRef { id: id.into(), title: format!("Padrão {id}"), description: String::new(), applies_to: globs.iter().map(|g| g.to_string()).collect() }
}

#[test]
fn plan_warnings_cover_no_citation_and_uncited_scope() {
    let standards = [std_ref("api-1a2b3c4d", &["src/api/**"]), std_ref("web-2b3c4d5e", &["web/**"])];
    let scope_api = vec!["src/api/handlers.rs".to_string()];
    let scope_docs = vec!["docs/**".to_string()];
    let tasks = [PlanTask { id: "T01", scope: &scope_api, text: "## Objetivo\nAPI" }, PlanTask { id: "T02", scope: &scope_docs, text: "docs" }];
    let w = plan_warnings(&standards, "spec sem citação", &tasks);
    assert!(w.no_standard_cited);
    assert_eq!(w.uncited.iter().map(|(t, s)| (t.as_str(), s.id.as_str())).collect::<Vec<_>>(), [("T01", "api-1a2b3c4d")]);
    // A spec cita um padrão (o aviso C-17 sai) e a tarefa cita o dela (o C-21 sai).
    let tasks = [PlanTask { id: "T01", scope: &scope_api, text: "Siga kb:api-1a2b3c4d#rotas" }];
    let w = plan_warnings(&standards, "Premissa: kb:web-2b3c4d5e#layout", &tasks);
    assert!(w.is_empty(), "{w:?}");
    // Citar só na spec não vale para a tarefa.
    let tasks = [PlanTask { id: "T01", scope: &scope_api, text: "sem citação" }];
    let w = plan_warnings(&standards, "kb:api-1a2b3c4d#rotas", &tasks);
    assert!(!w.no_standard_cited && w.uncited.len() == 1, "{w:?}");
    // Sem padrão, nada.
    assert!(plan_warnings(&[], "x", &tasks).is_empty());
}

#[test]
fn briefing_block_brings_cited_sections_within_the_ceiling_and_scope_standards() {
    let root = root("brief");
    let rules = md(&root, "regras.md", "# Filas\nUse filas para tarefas lentas.\n\n# Logs\nLogs em JSON.\n");
    let web = md(&root, "web.md", "# Layout\nGrade de 12 colunas.\n");
    let other = md(&root, "outro.md", "# Outro\nNada.\n");
    standard(&root, &rules, &["src/**"], "Regras do backend");
    standard(&root, &web, &["web/**"], "Regras da web");
    let manifest = crate::kb::load_manifest(&root).expect("manifesto");
    let stds = standards(&manifest);
    let scope = vec!["src/server/**".to_string()];
    let task = format!("## Critérios\n1. Filas (kb:{rules}#filas)");
    let spec = format!("## Premissas\n- Logs (kb:{rules}#logs) e kb:{other}");
    let input = BriefInput { task_text: &task, spec_text: &spec, scope: &scope, standards: &stds, reviewer: true, max: 4096 };
    let block = briefing_block(&root, &input, &none());
    let text = &block.text;
    assert!(text.contains("DADO NÃO CONFIÁVEL") && text.contains(REVIEWER_RULE), "{text}");
    assert!(text.contains(&format!("#### kb:{rules}#filas")) && text.contains("Use filas para tarefas lentas."), "{text}");
    assert!(text.contains(&format!("#### kb:{rules}#logs")) && text.contains("Logs em JSON."), "{text}");
    assert!(text.find("#filas").unwrap_or(usize::MAX) < text.find("#logs").unwrap_or(0), "tarefa primeiro");
    assert!(text.contains(&format!("kb:{other} (o documento inteiro")), "{text}");
    // Só o padrão do escopo, só a linha (sem o conteúdo de web.md).
    assert!(text.contains(&format!("- kb:{rules} | ")) && text.contains("aplica-se a: src/**"), "{text}");
    assert!(!text.contains(&format!("- kb:{web} |")) && !text.contains("Grade de 12 colunas"), "{text}");
    assert!(block.used > 0 && block.used <= 4096);

    // Teto pequeno: a primeira cabe, a segunda vira índice para o kb_read.
    let filas = crate::kb::index::read_section(&root, &rules, "filas", &none()).expect("seção");
    let first_len = format!("#### kb:{rules}#filas | {}\n{}\n", filas.trail, filas.text.trim_end()).len();
    let input = BriefInput { max: first_len, reviewer: false, ..input };
    let block = briefing_block(&root, &input, &none());
    assert_eq!(block.used, first_len, "{}", block.text);
    assert!(block.text.contains(DEV_RULE) && !block.text.contains(REVIEWER_RULE));
    assert!(block.text.contains(&format!("Citadas fora deste briefing (leia com kb_read): kb:{rules}#logs (")), "{}", block.text);
    assert!(!block.text.contains("Logs em JSON."));
}
