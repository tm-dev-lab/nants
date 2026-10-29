//! Testes dos documentos escritos no Nants (spec 136 T10): criar, editar, conflito, recusas,
//! symlink, links, índice, indicador de duplicata e fonte citada editada. Sem servidor nem rede.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::*;

const NOW: &str = "2026-10-29T12:00:00Z";

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nants-kbw-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let root = dir.join("repo");
    fs::create_dir_all(&root).expect("mkdir");
    root
}

fn none() -> SecretSet {
    SecretSet::new(Vec::<String>::new())
}

/// Montado em tempo de execução: a árvore pública recusa literal com cara de segredo.
fn configured_secret() -> String {
    ["sk", "-escrito-", "0123456789abcdef"].concat()
}

fn private_key_block() -> String {
    format!("{}BEGIN PRIVATE KEY{}\nabc\n", "-".repeat(5), "-".repeat(5))
}

fn write(root: &Path, title: &str, content: &str) -> Entry {
    add_written(root, title, content, &none(), NOW).expect("escrever")
}

fn note_on_disk(root: &Path, id: &str) -> String {
    fs::read_to_string(root.join(NOTES_REL).join(format!("{id}.md"))).expect("nota")
}

fn hits(root: &Path, query: &str) -> Vec<String> {
    index::search(root, query, 10, &none()).expect("busca").into_iter().map(|h| h.doc_id).collect()
}

#[test]
fn written_document_is_markdown_ready_and_indexed_at_once() {
    let root = root("create");
    let doc = write(&root, "Padrões de Código", "# Nomes\n\nUse camelCase nos serviços de faturamento.\n");
    assert!(doc.id.starts_with("padroes-de-codigo-") && is_doc_id(&doc.id), "{}", doc.id);
    assert_eq!((doc.kind, doc.source, doc.status), (Kind::Markdown, Origin::Written, Status::Ready));
    assert_eq!(doc.source.as_str(), "written");
    assert_eq!(doc.title, "Padrões de Código");
    assert_eq!(doc.file_name.as_deref(), Some("padroes-de-codigo.md"));
    assert_eq!(doc.sha256, sha256_hex(b"# Nomes\n\nUse camelCase nos servi\xc3\xa7os de faturamento.\n"));
    assert!(root.join(SOURCES_REL).join(format!("{}.md", doc.id)).is_file(), "o original é o próprio texto");
    let note = note_on_disk(&root, &doc.id);
    assert!(note.contains("title: \"Padrões de Código\"") && note.contains("source: \"written\""), "{note}");
    let manifest = fs::read_to_string(root.join(MANIFEST_REL)).expect("manifesto");
    assert!(manifest.contains("\"source\": \"written\""), "{manifest}");
    assert_eq!(hits(&root, "faturamento"), [doc.id.clone()], "indexado na hora");

    // O id vem do título: o mesmo título dá o mesmo id, também com outro texto.
    remove(&root, &doc.id).expect("remover");
    let again = write(&root, "Padrões de Código", "outro texto");
    assert_eq!(again.id, doc.id);
    // Um frontmatter no texto não troca o título da pessoa.
    let fm = write(&root, "Guia", "---\ntitle: Outro\n---\n# Guia\n");
    assert_eq!(fm.title, "Guia");
}

#[test]
fn title_empty_long_or_repeated_is_refused_with_its_reason() {
    let root = root("title");
    assert_eq!(add_written(&root, "  \n ", "x", &none(), NOW), Err(KbError::TitleEmpty));
    assert_eq!(add_written(&root, &"a".repeat(MAX_TITLE_CHARS + 1), "x", &none(), NOW), Err(KbError::TitleTooLong));
    let first = write(&root, "Padrões da API", "texto");
    // Caixa, acento e espaços não fazem outro título.
    assert_eq!(add_written(&root, "padroes  da  api", "x", &none(), NOW), Err(KbError::TitleTaken(first.id.clone())));
    // O título de um documento enviado também conta.
    let uploaded = add_file(&root, "Guia de deploy.md", b"# Passos\n", &none(), NOW).expect("envio");
    assert_eq!(add_written(&root, "Guia de deploy", "x", &none(), NOW), Err(KbError::TitleTaken(uploaded.id)));
    assert_eq!(list(&root).expect("lista").len(), 2);
}

#[test]
fn secret_null_byte_and_size_are_refused_and_nothing_is_written() {
    let root = root("refuse");
    let key = private_key_block();
    assert_eq!(add_written(&root, "Chave", &key, &none(), NOW), Err(KbError::SecretContent));
    let secret = configured_secret();
    let set = SecretSet::new(vec![secret.clone()]);
    assert_eq!(add_written(&root, "Token", &format!("usar {secret}"), &set, NOW), Err(KbError::SecretContent));
    assert_eq!(add_written(&root, &format!("Título {secret}"), "texto", &set, NOW), Err(KbError::SecretContent));
    assert_eq!(add_written(&root, "Nulo", "a\0b", &none(), NOW), Err(KbError::NotText));
    assert_eq!(add_written(&root, "Grande", &"a".repeat(MAX_WRITTEN_BYTES + 1), &none(), NOW), Err(KbError::WrittenTooLarge));
    assert!(list(&root).expect("lista").is_empty());

    let doc = write(&root, "Doc", "texto");
    assert_eq!(edit_content(&root, &doc.id, &key, &doc.sha256, &none()), Err(KbError::SecretContent));
    assert_eq!(edit_content(&root, &doc.id, &format!("x {secret}"), &doc.sha256, &set), Err(KbError::SecretContent));
    assert_eq!(edit_content(&root, &doc.id, &"a".repeat(MAX_WRITTEN_BYTES + 1), &doc.sha256, &none()), Err(KbError::WrittenTooLarge));
    assert_eq!(read_entry(&root, &doc.id).expect("entrada").sha256, doc.sha256, "nada mudou");
}

#[test]
fn edit_rewrites_original_and_note_and_reindexes() {
    let root = root("edit");
    let doc = write(&root, "Filas", "# Filas\n\nUse filas para relatórios lentos.\n");
    let text = "# Filas\n\nUse eventos assíncronos com idempotência.\n";
    let edited = edit_content(&root, &doc.id, text, &doc.sha256, &none()).expect("editar");
    assert_eq!(edited.sha256, sha256_hex(text.as_bytes()));
    assert_eq!(edited.size, text.len() as u64);
    assert_eq!(edited.title, "Filas", "o título fica");
    assert_eq!(edited.added_at, doc.added_at);
    let source = fs::read_to_string(root.join(SOURCES_REL).join(format!("{}.md", doc.id))).expect("original");
    assert_eq!(source, text);
    let (_, body) = read_note(&root, &doc.id, &none()).expect("nota");
    assert_eq!(body, text);
    assert!(note_on_disk(&root, &doc.id).contains(&format!("sha256: \"{}\"", edited.sha256)));
    assert_eq!(hits(&root, "idempotencia"), [doc.id.clone()], "a busca acha o texto novo");
    assert!(hits(&root, "relatorios").is_empty(), "e não o antigo");
    assert_eq!(edited.description, "Use eventos assíncronos com idempotência.", "o rascunho acompanha o texto");
    // A descrição escrita pela pessoa fica.
    update(&root, &doc.id, &Patch { description: Some("Minha descrição".into()), ..Patch::default() }).expect("descrição");
    let kept = edit_content(&root, &doc.id, "# Filas\n\nOutro texto.\n", &edited.sha256, &none()).expect("editar");
    assert_eq!(kept.description, "Minha descrição");
    let edited = kept;

    // Conflito: a versão aberta não é mais a atual; nada é gravado.
    assert_eq!(edit_content(&root, &doc.id, "# Outra\n", &doc.sha256, &none()), Err(KbError::EditConflict));
    assert_eq!(read_entry(&root, &doc.id).expect("entrada").sha256, edited.sha256);
    assert_eq!(edit_content(&root, "nada-0123abcd", "x", "", &none()), Err(KbError::NotFound));
    assert_eq!(edit_content(&root, "../x", "x", "", &none()), Err(KbError::NotFound));
}

#[test]
fn uploaded_markdown_and_text_are_editable_pdf_and_link_are_not() {
    let root = root("editable");
    let md = add_file(&root, "guia.md", b"# Guia\n\nPasso antigo.\n", &none(), NOW).expect("md");
    let txt = add_file(&root, "notas.txt", b"linha antiga\n", &none(), NOW).expect("txt");
    let pdf = add_file(&root, "a.pdf", b"%PDF-1.4\n", &none(), NOW).expect("pdf");
    let link = add_link(&root, "https://exemplo.com/guia", &none(), NOW).expect("link");

    let md2 = edit_content(&root, &md.id, "# Guia\n\nPasso novo.\n", &md.sha256, &none()).expect("md");
    assert_eq!(md2.title, md.title, "o título fica");
    assert_eq!(md2.file_name.as_deref(), Some("guia.md"));
    let source = fs::read_to_string(root.join(SOURCES_REL).join(format!("{}.md", md.id))).expect("original");
    assert_eq!(source, "# Guia\n\nPasso novo.\n");
    let txt2 = edit_content(&root, &txt.id, "linha nova\n", &txt.sha256, &none()).expect("txt");
    assert_eq!(txt2.kind, Kind::Text);
    let source = fs::read_to_string(root.join(SOURCES_REL).join(format!("{}.txt", txt.id))).expect("original");
    assert_eq!(source, "linha nova\n");

    assert_eq!(edit_content(&root, &pdf.id, "x", &pdf.sha256, &none()), Err(KbError::NotEditable));
    assert_eq!(edit_content(&root, &link.id, "x", &link.sha256, &none()), Err(KbError::NotEditable));
    assert!(!is_editable(&pdf) && !is_editable(&link) && is_editable(&md) && is_editable(&txt));
}

#[test]
fn edit_recalculates_links_both_ways() {
    let root = root("links");
    let banco = add_file(&root, "banco.md", b"# Banco\n\nPostgres.\n", &none(), NOW).expect("banco");
    let guia = write(&root, "Guia", "# Guia\n\nSem links ainda.\n");
    assert!(guia.links_out.is_empty());
    let linked = edit_content(&root, &guia.id, "# Guia\n\nVeja o [banco](banco.md).\n", &guia.sha256, &none()).expect("editar");
    assert_eq!(linked.links_out, [banco.id.clone()]);
    assert_eq!(read_entry(&root, &guia.id).expect("entrada").links_out, [banco.id.clone()]);
    // Um link para o documento escrito pelo nome dele (`guia.md`) chega a ele.
    let outro = add_file(&root, "outro.md", b"# Outro\n\nSiga o [guia](guia.md).\n", &none(), NOW).expect("outro");
    assert_eq!(outro.links_out, [guia.id.clone()]);
    let unlinked = edit_content(&root, &guia.id, "# Guia\n\nSem link.\n", &linked.sha256, &none()).expect("editar");
    assert!(unlinked.links_out.is_empty());
    assert!(read_entry(&root, &guia.id).expect("entrada").link_targets.is_empty());
}

#[test]
fn same_content_as_another_is_an_indicator_not_a_refusal() {
    let root = root("dup");
    let text = "# Deploy\n\nPublique pelo pipeline.\n";
    let uploaded = add_file(&root, "deploy.md", text.as_bytes(), &none(), NOW).expect("envio");
    let written = write(&root, "Deploy escrito", text);
    let map = indicators::for_project(&root).expect("indicadores");
    assert_eq!(map.get(&written.id), Some(&vec![indicators::Indicator::Duplicate { doc: uploaded.id.clone() }]));
    // Dois documentos escritos ainda vazios não são duplicatas um do outro.
    let a = write(&root, "Vazio A", "");
    let b = write(&root, "Vazio B", "");
    let map = indicators::for_project(&root).expect("indicadores");
    for id in [&a.id, &b.id] {
        assert!(!map.get(id).is_some_and(|l| l.iter().any(|i| i.code() == "duplicate")), "{map:?}");
    }
}

#[test]
fn symlinks_are_never_followed() {
    let root = root("symlink");
    let outside = root.parent().expect("pai").join("fora");
    fs::create_dir_all(&outside).expect("fora");
    let doc = write(&root, "Doc", "texto");
    // O original trocado por um symlink: a edição recusa e o alvo fica intocado.
    let victim = outside.join("alvo.md");
    fs::write(&victim, "intocado").expect("alvo");
    let source = root.join(SOURCES_REL).join(format!("{}.md", doc.id));
    fs::remove_file(&source).expect("rm");
    symlink(&victim, &source).expect("symlink");
    assert_eq!(edit_content(&root, &doc.id, "novo", &doc.sha256, &none()), Err(KbError::Fs(SafeFsError::Unsafe)));
    assert_eq!(fs::read_to_string(&victim).expect("alvo"), "intocado");
    assert_eq!(read_entry(&root, &doc.id).expect("entrada").sha256, doc.sha256);

    // A pasta da base como symlink: nada é escrito fora.
    let other = self::root("symlink-kb");
    let out = other.parent().expect("pai").join("fora");
    fs::create_dir_all(&out).expect("fora");
    fs::create_dir_all(other.join(".sdd")).expect("sdd");
    symlink(&out, other.join(KB_REL)).expect("symlink");
    assert!(add_written(&other, "Doc", "texto", &none(), NOW).is_err());
    assert_eq!(fs::read_dir(&out).expect("fora").count(), 0);
}

/// Critério 4: a seção citada num plano aprovado que é editada no Nants aparece em
/// [`cite::changed`] (o aviso `kb.source_changed`), sem esquecer o índice à mão: a edição muda o
/// manifesto, e o índice segue o manifesto.
#[test]
fn editing_a_cited_section_is_seen_as_a_changed_source() {
    let root = root("cited");
    let doc = write(&root, "Regras", "# Filas\n\nUse filas.\n\n# Logs\n\nLogs em JSON.\n");
    let cited = format!("kb:{}#logs", doc.id);
    let frozen = cite::freeze(&root, "136-x", &[cited.as_str()], &none(), NOW).expect("congelar").expect("gravado");
    assert!(cite::changed(&root, &frozen, &none()).is_empty());
    let text = "# Filas\n\nUse filas.\n\n# Logs\n\nLogs em texto.\n";
    edit_content(&root, &doc.id, text, &doc.sha256, &none()).expect("editar");
    assert_eq!(cite::changed(&root, &frozen, &none()), [cited]);
}
