//! Testes dos indicadores da base (spec 134/T06, C-24).

use std::fs;
use std::path::PathBuf;

use super::*;
use crate::kb::{self, Kind, Origin};
use crate::server::events::SecretSet;

const NOW: &str = "2026-10-29T12:00:00Z";

fn entry(id: &str, kind: Kind, source: Origin) -> Entry {
    Entry {
        id: id.to_string(),
        title: id.to_string(),
        kind,
        source,
        file_name: None,
        source_url: None,
        size: 0,
        sha256: String::new(),
        added_at: NOW.to_string(),
        fetched_at: None,
        status: Status::Ready,
        status_reason: None,
        pages: None,
        extractor: None,
        description: String::new(),
        links_out: Vec::new(),
        link_targets: Vec::new(),
    }
}

fn codes(map: &HashMap<String, Vec<Indicator>>, id: &str) -> Vec<&'static str> {
    map.get(id).map(|l| l.iter().map(Indicator::code).collect()).unwrap_or_default()
}

#[test]
fn same_text_by_another_path_is_a_duplicate() {
    let file = entry("api-0000aaaa", Kind::Markdown, Origin::File);
    let mut link = entry("api-link-0000bbbb", Kind::Link, Origin::Url);
    link.source_url = Some("https://wiki.exemplo.com/api".into());
    let other = entry("outro-0000cccc", Kind::Markdown, Origin::File);
    let docs = vec![file.clone(), link.clone(), other.clone()];
    let index = Index::build(vec![
        (file, "# Padrões de API\n\nUse **versionamento** no caminho.\n".into()),
        // O mesmo texto vindo de uma página: formatação, caixa e acento diferentes.
        (link, "# padroes de api\n\nUse versionamento no caminho\n".into()),
        (other, "# Outro\n\nNada a ver.\n".into()),
    ]);
    let map = compute(&docs, &index);
    assert_eq!(map.get("api-0000aaaa"), Some(&vec![Indicator::Duplicate { doc: "api-link-0000bbbb".into() }]));
    assert_eq!(map.get("api-link-0000bbbb"), Some(&vec![Indicator::Duplicate { doc: "api-0000aaaa".into() }]));
    assert!(!map.contains_key("outro-0000cccc"));
}

#[test]
fn same_original_hash_is_a_duplicate_even_without_text() {
    let mut a = entry("a-0000aaaa", Kind::Pdf, Origin::File);
    let mut b = entry("b-0000bbbb", Kind::Pdf, Origin::Url);
    a.sha256 = "ab".repeat(32);
    b.sha256 = "ab".repeat(32);
    let index = Index::build(Vec::new());
    let map = compute(&[a, b], &index);
    assert_eq!(codes(&map, "a-0000aaaa"), vec!["duplicate"]);
}

#[test]
fn text_without_headings_is_flagged_and_pdf_never_is() {
    let plain = entry("plain-0000aaaa", Kind::Text, Origin::File);
    let titled = entry("titled-0000bbbb", Kind::Markdown, Origin::File);
    let pdf = entry("pdf-0000cccc", Kind::Pdf, Origin::File);
    let docs = vec![plain.clone(), titled.clone(), pdf.clone()];
    let index = Index::build(vec![
        (plain, "Mensagens no imperativo.\nUma branch por tarefa.\n".into()),
        (titled, "# Título\n\nTexto.\n".into()),
        (pdf, "# p. 1\nPrimeira página.\n".into()),
    ]);
    let map = compute(&docs, &index);
    assert_eq!(codes(&map, "plain-0000aaaa"), vec!["no_headings"]);
    assert!(!map.contains_key("titled-0000bbbb"));
    assert!(!map.contains_key("pdf-0000cccc"));
}

#[test]
fn internal_link_without_document_is_broken_and_only_ready_docs_count() {
    let mut a = entry("a-0000aaaa", Kind::Markdown, Origin::File);
    a.link_targets = vec!["file:guia.md".into(), "file:banco.md".into(), "url:https://x.com/".into()];
    let mut guia = entry("guia-0000bbbb", Kind::Markdown, Origin::File);
    guia.file_name = Some("Guia.md".into());
    let mut queued = entry("fila-0000cccc", Kind::Pdf, Origin::File);
    queued.status = Status::Pending;
    queued.link_targets = vec!["file:nada.md".into()];
    let docs = vec![a.clone(), guia.clone(), queued];
    let index = Index::build(vec![(a, "# A\n\nVer guia.\n".into()), (guia, "# Guia\n\nTexto.\n".into())]);
    let map = compute(&docs, &index);
    assert_eq!(map.get("a-0000aaaa"), Some(&vec![Indicator::BrokenLink { target: "banco.md".into() }]));
    assert!(!map.contains_key("fila-0000cccc"), "documento fora de ready não tem indicador");
}

#[test]
fn for_project_reads_the_base_on_disk() {
    let dir = std::env::temp_dir().join(format!("nants-kbind-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let root: PathBuf = dir.join("repo");
    fs::create_dir_all(&root).expect("mkdir");
    let none = SecretSet::new(Vec::<String>::new());
    let a = kb::add_file(&root, "padroes.md", b"# Padroes\n\nVeja [o guia](guia.md).\n", &none, NOW).expect("add");
    let b = kb::add_file(&root, "notas.txt", b"sem titulo nenhum aqui\n", &none, NOW).expect("add");
    let map = for_project(&root).expect("indicadores");
    assert_eq!(map.get(&a.id), Some(&vec![Indicator::BrokenLink { target: "guia.md".into() }]));
    assert_eq!(codes(&map, &b.id), vec!["no_headings"]);
    let guia = kb::add_file(&root, "guia.md", b"# Guia\n\nTexto do guia.\n", &none, NOW).expect("add");
    let map = for_project(&root).expect("indicadores");
    assert!(!map.contains_key(&a.id), "o link passou a resolver");
    assert!(!map.contains_key(&guia.id));
    let _ = fs::remove_dir_all(&dir);
}
