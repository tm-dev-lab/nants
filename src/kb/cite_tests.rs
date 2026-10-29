//! Testes das citações da base (spec 134 T05).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::kb::add_file;

const NOW: &str = "2026-10-28T12:00:00Z";

fn root(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("nants-kbcite-{}-{n}-{tag}", std::process::id()));
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

const RULES: &str = "# Filas\nUse filas para tarefas lentas.\n\n# Logs\nLogs em JSON.\n";

#[test]
fn find_reads_citations_and_ignores_templates_and_words() {
    let text = "Siga kb:regras-ab12cd34#filas. Veja também (kb:regras-ab12cd34#logs) e kb:outro-ff00ff00.\n\
                Modelo: kb:<id>#<seção>; palavra: abckb:regras-ab12cd34#filas; repetida: kb:regras-ab12cd34#filas";
    let found = find(text);
    let shown: Vec<String> = found.iter().map(Citation::text).collect();
    assert_eq!(shown, ["kb:regras-ab12cd34#filas", "kb:regras-ab12cd34#logs", "kb:outro-ff00ff00"]);
    assert_eq!(found[2].section, None);
    assert_eq!(cited_docs(text), ["regras-ab12cd34", "outro-ff00ff00"]);
    assert!(find("sem citação nenhuma").is_empty());
    // A âncora neutra (`s.<n>`) é aceita; o ponto final da frase sai.
    assert_eq!(find("kb:doc-0123abcd#s.3.")[0].section.as_deref(), Some("s.3"));
}

#[test]
fn check_accepts_valid_and_refuses_unknown_doc_or_section_with_the_reason() {
    let root = root("check");
    let id = md(&root, "regras.md", RULES);
    assert_eq!(check(&root, "nada citado", &none()), Ok(()));
    assert_eq!(check(&root, &format!("Use kb:{id}#filas e kb:{id}."), &none()), Ok(()));
    let err = check(&root, &format!("kb:{id}#cache e kb:inexistente-00000000#x"), &none()).expect_err("recusa");
    assert!(err.contains("nada foi gravado"), "{err}");
    assert!(err.contains(&format!("`kb:{id}#cache`")) && err.contains("filas, logs"), "{err}");
    assert!(err.contains("`inexistente-00000000` não está pronto"), "{err}");
    // Base sem nenhum documento: nada é validado (G2).
    let empty = self::root("check-empty");
    assert_eq!(check(&empty, "kb:regras-ab12cd34#filas", &none()), Ok(()));
    assert_eq!(check(&empty, "sem citação", &none()), Ok(()));
    // Documento citado que saiu da base continua recusado enquanto a base existe.
    let other = md(&root, "outro.md", "# Outro\nTexto.\n");
    crate::kb::remove(&root, &other).expect("remover");
    assert!(check(&root, &format!("kb:{other}#outro"), &none()).is_err());
}

/// Revisão 1, F-01: só a forma real do id (`<slug>-<hex de 8, 16 ou 32>`) é citação.
#[test]
fn only_real_ids_are_citations() {
    for not_cited in ["kb:session", "\"kb:user:1\"", "https://x/kb:faq", "kb:sessao-abc", "kb:doc-0123abc", "kb:doc-0123abcde", "kb:-0123abcd", "kb:doc-0123ABCD"] {
        assert!(find(not_cited).is_empty(), "{not_cited}");
    }
    for id in ["doc-0123abcd", "doc-0123456789abcdef", "a-b-0123456789abcdef0123456789abcdef"] {
        assert!(crate::kb::is_doc_id(id), "{id}");
        assert_eq!(find(&format!("kb:{id}#s")).len(), 1, "{id}");
    }
}

#[test]
fn freeze_records_hashes_and_catalog_and_detects_changes() {
    let root = root("freeze");
    let id = md(&root, "regras.md", RULES);
    let spec = format!("## Premissas\n- Filas (kb:{id}#filas)\n");
    let task = format!("## Critérios\n1. Logs (kb:{id}#logs)\n");
    let frozen = freeze(&root, "134-x", &[&spec, &task], &none(), NOW).expect("freeze").expect("gravado");
    assert_eq!(frozen.sources.len(), 2);
    assert!(frozen.sources.iter().all(|s| s.sha256.len() == 64), "{frozen:?}");
    assert!(frozen.catalog.contains(&format!("kb:{id}")), "{}", frozen.catalog);
    assert!(root.join(frozen_rel("134-x")).is_file());
    assert!(frozen_rel("134-x").starts_with(".sdd/kb/"), "fora do Git, dentro da base");
    assert_eq!(load_frozen(&root, "134-x"), Some(frozen.clone()));
    assert_eq!(load_frozen(&root, "outra"), None);
    assert_eq!(load_frozen(&root, "../x"), None);
    assert!(changed(&root, &frozen, &none()).is_empty(), "nada mudou");

    // A nota muda: só a seção alterada aparece.
    let note = root.join(crate::kb::NOTES_REL).join(format!("{id}.md"));
    let text = fs::read_to_string(&note).expect("nota").replace("Logs em JSON.", "Logs em texto.");
    fs::write(&note, text).expect("editar");
    // O índice vale pelo manifesto: força a remontagem.
    crate::kb::index::forget(&root);
    assert_eq!(changed(&root, &frozen, &none()), [format!("kb:{id}#logs")]);
}

/// Spec 136 T02 (C-06): o arquivo congelado da 134, com `standards` e o catálogo com o selo
/// `[padrão]`, continua legível; o catálogo vem como está (a recuperação o usa assim).
#[test]
fn frozen_from_134_with_standards_stays_readable() {
    let root = root("frozen-134");
    let id = md(&root, "regras.md", RULES);
    let old = serde_json::json!({
        "version": 1,
        "spec": "134-x",
        "frozen_at": NOW,
        "sources": [{"citation": format!("kb:{id}#filas"), "sha256": ""}],
        "catalog": format!("Documentos prontos (1; os padrões primeiro).\n- kb:{id} | Regras [padrão] | markdown"),
        "standards": [{"id": id, "title": "Regras", "description": "", "applies_to": ["src/**"]}],
    });
    fs::create_dir_all(root.join(FROZEN_REL)).expect("mkdir");
    fs::write(root.join(frozen_rel("134-x")), serde_json::to_string_pretty(&old).expect("json")).expect("gravar");
    let frozen = load_frozen(&root, "134-x").expect("legível");
    assert_eq!(frozen.sources.len(), 1);
    assert!(frozen.catalog.contains("[padrão]"), "o catálogo congelado fica como está");
    assert!(frozen.overview.is_empty(), "a Visão geral da 134 está dentro do catálogo");
}

#[test]
fn freeze_without_ready_docs_or_citations_writes_nothing() {
    let root = root("freeze-empty");
    assert_eq!(freeze(&root, "134-x", &["sem citação"], &none(), NOW), Ok(None));
    assert!(!root.join(FROZEN_REL).exists());
    assert!(freeze(&root, "../fuga", &["x"], &none(), NOW).is_err(), "id inseguro");
}

#[test]
fn check_counted_counts_listed_and_remaining() {
    let root = root("refused");
    md(&root, "regras.md", RULES);
    let ten: String = (0..10).map(|i| format!("kb:falta{i}-ab12cd3{i}#x ")).collect();
    let r = check_counted(&root, &ten, &none()).expect_err("recusa");
    assert_eq!(r.count, 10, "{}", r.message);
    assert!(r.message.contains("e mais"), "a mensagem resume o resto: {}", r.message);
    assert_eq!(check(&root, &ten, &none()).expect_err("recusa"), r.message, "check devolve a mesma mensagem");
    let one = check_counted(&root, "kb:falta-ab12cd34#x", &none()).expect_err("recusa");
    assert_eq!(one.count, 1, "{}", one.message);
    assert_eq!(check_counted(&root, "texto sem citação", &none()), Ok(()));
}

/// Paridade com a leitura da web (spec 136 T04, C-14): os mesmos casos de `cite_cases.json` passam
/// pelo `splitCitations` de `web/src/apps/knowledge/knowledgeModel.ts`.
#[test]
fn find_follows_the_shared_cases() {
    let cases: serde_json::Value = serde_json::from_str(include_str!("cite_cases.json")).expect("json dos casos");
    let list = cases["cases"].as_array().expect("lista de casos");
    assert!(list.len() >= 10);
    for case in list {
        let text = case["text"].as_str().expect("texto");
        let expected: Vec<Citation> = case["cites"]
            .as_array()
            .expect("citações")
            .iter()
            .map(|c| Citation {
                doc: c["doc"].as_str().expect("doc").to_string(),
                section: c["section"].as_str().map(str::to_string),
            })
            .collect();
        assert_eq!(find(text), expected, "{text}");
    }
}
