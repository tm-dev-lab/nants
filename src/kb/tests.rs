//! Testes do armazenamento da base de conhecimento (spec 134/T01), sem servidor nem rede.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;

const NOW: &str = "2026-10-28T12:00:00Z";
// Montado em pedaços: a árvore pública recusa literal com cara de segredo.
const SECRET: &str = concat!("sk", "-segredo-de-teste-0123456789");

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nants-kb-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let root = dir.join("repo");
    fs::create_dir_all(&root).expect("mkdir");
    root
}

fn none() -> SecretSet {
    SecretSet::new(Vec::<String>::new())
}

fn secrets() -> SecretSet {
    SecretSet::new(vec![SECRET.to_string()])
}

fn add_md(root: &Path, name: &str, text: &str) -> Entry {
    add_file(root, name, text.as_bytes(), &none(), NOW).expect("add")
}

#[test]
fn ids_are_path_safe_and_stable() {
    let root = root("ids");
    let a = add_md(&root, "Padrões de Código.md", "# Padrões\n");
    assert!(a.id.starts_with("padroes-de-codigo-"), "{}", a.id);
    assert!(valid_id(&a.id));
    remove(&root, &a.id).expect("remove");
    let again = add_md(&root, "Padrões de Código.md", "# Padrões\n");
    assert_eq!(again.id, a.id, "mesmo conteúdo, mesmo id");

    let odd = add_md(&root, "../../etc/..passwd!!.txt", "texto");
    assert!(valid_id(&odd.id), "{}", odd.id);
    assert!(root.join(NOTES_REL).join(format!("{}.md", odd.id)).is_file());
    let symbols = add_md(&root, "###.md", "outro");
    assert!(symbols.id.starts_with("doc-"));

    for bad in ["", "-a", "A", "a/b", "a.b", "..", "a_b", &"a".repeat(65)] {
        assert!(!valid_id(bad), "{bad}");
    }
    assert!(matches!(read_note(&root, "../x", &none()), Err(KbError::NotFound)));
    assert!(matches!(remove(&root, "a/b"), Err(KbError::NotFound)));
}

#[test]
fn markdown_is_ready_with_frontmatter_and_raw_body() {
    let root = root("md");
    let text = "\u{feff}# Arquitetura\n\nUse camadas.\n";
    let entry = add_md(&root, "arquitetura.md", text);
    assert_eq!(entry.kind, Kind::Markdown);
    assert_eq!(entry.status, Status::Ready);
    assert_eq!(entry.role, Role::Reference);
    assert_eq!(entry.extractor.as_deref(), Some("raw"));
    assert_eq!(entry.size, text.len() as u64);
    assert_eq!(entry.sha256.len(), 64);
    assert!(entry.links_out.is_empty() && entry.applies_to.is_empty());
    assert_eq!(entry.description, "Use camadas.", "primeira frase (C-19)");
    assert_eq!(entry.title, "arquitetura");

    let note = fs::read_to_string(root.join(NOTES_REL).join(format!("{}.md", entry.id))).expect("nota");
    for key in ["title", "kind", "source", "source_url", "added_at", "fetched_at", "sha256", "pages", "extractor", "status", "role"] {
        assert!(note.contains(&format!("\n{key}: ")), "{key} no frontmatter:\n{note}");
    }
    assert!(note.contains("status: \"ready\"") && note.contains("role: \"reference\""));
    assert_eq!(note_body(&note), "# Arquitetura\n\nUse camadas.\n");
    assert_eq!(fs::read(root.join(SOURCES_REL).join(format!("{}.md", entry.id))).expect("original"), text.as_bytes());

    let (read, body) = read_note(&root, &entry.id, &none()).expect("ler");
    assert_eq!(read, entry);
    assert_eq!(body, "# Arquitetura\n\nUse camadas.\n");

    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join(MANIFEST_REL)).expect("manifesto")).expect("json");
    let doc = &manifest["docs"][0];
    for key in ["id", "title", "kind", "source", "size", "sha256", "added_at", "status", "role", "description", "applies_to", "links_out"] {
        assert!(!doc[key].is_null(), "{key} no manifesto");
    }
}

#[test]
fn pdf_and_link_start_pending() {
    let root = root("pending");
    let pdf = add_file(&root, "manual.PDF", b"%PDF-1.7\n...", &none(), NOW).expect("pdf");
    assert_eq!((pdf.kind, pdf.status), (Kind::Pdf, Status::Pending));
    assert!(root.join(SOURCES_REL).join(format!("{}.pdf", pdf.id)).is_file());
    let (_, body) = read_note(&root, &pdf.id, &none()).expect("nota");
    assert!(body.is_empty());

    let link = add_link(&root, " https://exemplo.com/padroes/api#secao ", &none(), NOW).expect("link");
    assert_eq!((link.kind, link.status, link.source), (Kind::Link, Status::Pending, Origin::Url));
    assert_eq!(link.source_url.as_deref(), Some("https://exemplo.com/padroes/api"));
    assert!(link.id.starts_with("exemplo-com-padroes-api-"), "{}", link.id);
    assert!(matches!(add_link(&root, "https://exemplo.com/padroes/api", &none(), NOW), Err(KbError::Duplicate(id)) if id == link.id));
    assert_eq!(list(&root).expect("lista").len(), 2);
}

#[test]
fn bad_input_is_refused() {
    let root = root("bad");
    let s = none();
    assert_eq!(add_file(&root, "a.docx", b"x", &s, NOW), Err(KbError::Unsupported));
    assert_eq!(add_file(&root, "a.pdf", b"nao e pdf", &s, NOW), Err(KbError::Unsupported));
    assert_eq!(add_file(&root, "a.txt", b"\xff\xfe", &s, NOW), Err(KbError::NotText));
    assert_eq!(add_file(&root, "a.md", b"a\0b", &s, NOW), Err(KbError::NotText));
    assert_eq!(add_file(&root, "a.md", b"", &s, NOW), Err(KbError::Empty));
    assert_eq!(add_file(&root, "", b"x", &s, NOW), Err(KbError::BadName));
    assert_eq!(add_file(&root, "a\nb.md", b"x", &s, NOW), Err(KbError::BadName));
    for url in ["ftp://x.com/a", "file:///etc/passwd", "nao e link", "http://", ""] {
        assert_eq!(add_link(&root, url, &s, NOW), Err(KbError::BadUrl), "{url}");
    }
    assert_eq!(add_link(&root, "https://u:p@x.com/", &s, NOW), Err(KbError::UrlCredentials));
    assert!(!root.join(MANIFEST_REL).exists(), "nada gravado");
}

#[test]
fn secret_names_and_contents_are_refused() {
    let root = root("secret");
    let s = none();
    for name in [
        ".env", ".env.local", "prod.env", "id_rsa", "id_ed25519", "chave.pem", "server.key", "cert.p12", "a.pfx",
        "credentials", ".npmrc", ".netrc", "secrets.md", "passwords.txt", "api_key.txt", "dir/.env.production",
        "C:\\x\\secrets.md",
    ] {
        assert!(is_secret_name(name), "{name}");
        assert_eq!(add_file(&root, name, b"conteudo", &s, NOW), Err(KbError::SecretName), "{name}");
    }
    for name in ["padroes.md", "token.md", "environment.md", "id_rsa.pub", "secret-management-standard.md", "tokens-de-design.md"] {
        assert!(!is_secret_name(name), "{name}");
    }
    let pem = concat!("texto\n-----", "BEGIN OPENSSH PRIVATE KEY-----\nabc\n").as_bytes();
    assert_eq!(add_file(&root, "notas.md", pem, &s, NOW), Err(KbError::SecretContent));
    let mut pdf = b"%PDF-1.4\n".to_vec();
    pdf.extend_from_slice(concat!("-----", "BEGIN RSA PRIVATE KEY-----\n").as_bytes());
    assert_eq!(add_file(&root, "doc.pdf", &pdf, &s, NOW), Err(KbError::SecretContent));
    assert!(!has_private_key(b"-----BEGIN CERTIFICATE-----\nabc"));
    let with_key = format!("a chave e {SECRET}");
    assert_eq!(add_file(&root, "notas.md", with_key.as_bytes(), &secrets(), NOW), Err(KbError::SecretContent));
    assert_eq!(add_link(&root, &format!("https://x.com/?k={SECRET}"), &secrets(), NOW), Err(KbError::SecretContent));
    assert!(list(&root).expect("lista").is_empty());
}

#[test]
fn note_is_redacted_on_write_and_on_read() {
    let root = root("redact");
    // Segredo que só passa a ser conhecido depois do envio: a leitura redige.
    let entry = add_md(&root, "notas.md", &format!("token: {SECRET}\n"));
    let (_, body) = read_note(&root, &entry.id, &secrets()).expect("ler");
    assert!(!body.contains(SECRET), "{body}");
    assert!(body.contains("***"));
    // URL com usuário e senha no texto também sai redigida.
    let other = add_md(&root, "urls.md", "veja https://user:senha123@example.com/x\n");
    let (_, body) = read_note(&root, &other.id, &none()).expect("ler");
    assert!(!body.contains("senha123"), "{body}");
    write_overview(&root, &format!("chave {SECRET}")).expect("visao geral");
    assert!(!read_overview(&root, &secrets()).expect("ler").contains(SECRET));
}

#[test]
fn duplicate_by_sha256_is_refused() {
    let root = root("dup");
    let first = add_md(&root, "a.md", "mesmo texto");
    assert_eq!(add_file(&root, "b.txt", b"mesmo texto", &none(), NOW), Err(KbError::Duplicate(first.id.clone())));
    assert_eq!(list(&root).expect("lista").len(), 1);
    // Mesmo slug, conteúdo diferente: entra com outro id.
    let second = add_md(&root, "a.md", "outro texto");
    assert_ne!(second.id, first.id);
}

#[test]
fn update_role_description_and_applies_to() {
    let root = root("update");
    let entry = add_md(&root, "api.md", "# API\n");
    let globs = vec!["src/api/**".to_string(), " ".to_string(), "src/api/**".to_string()];
    let refused = update(&root, &entry.id, &Patch { applies_to: Some(globs.clone()), ..Patch::default() });
    assert_eq!(refused, Err(KbError::AppliesToStandardOnly));

    let patch = Patch { role: Some(Role::Standard), description: Some("  Padrão de APIs REST  ".into()), applies_to: Some(globs) };
    let updated = update(&root, &entry.id, &patch).expect("update");
    assert_eq!(updated.role, Role::Standard);
    assert_eq!(updated.description, "Padrão de APIs REST");
    assert_eq!(updated.applies_to, vec!["src/api/**".to_string()]);
    let note = fs::read_to_string(root.join(NOTES_REL).join(format!("{}.md", entry.id))).expect("nota");
    assert!(note.contains("role: \"standard\""), "{note}");
    assert_eq!(note_body(&note), "# API\n", "corpo preservado");
    assert_eq!(list(&root).expect("lista")[0], updated);

    let back = update(&root, &entry.id, &Patch { role: Some(Role::Reference), ..Patch::default() }).expect("referência");
    assert!(back.applies_to.is_empty(), "referência não guarda globs");

    let long = "a".repeat(MAX_DESCRIPTION_CHARS + 1);
    assert_eq!(update(&root, &entry.id, &Patch { description: Some(long), ..Patch::default() }), Err(KbError::DescriptionTooLong));
    assert_eq!(
        update(&root, &entry.id, &Patch { description: Some("a\nb".into()), ..Patch::default() }),
        Err(KbError::BadDescription)
    );
    for bad in ["/etc/**", "../x", "a/../b", "a\\b"] {
        let patch = Patch { role: Some(Role::Standard), applies_to: Some(vec![bad.into()]), ..Patch::default() };
        assert_eq!(update(&root, &entry.id, &patch), Err(KbError::BadAppliesTo), "{bad}");
    }
    let many: Vec<String> = (0..=MAX_APPLIES_TO).map(|i| format!("src/{i}/**")).collect();
    let patch = Patch { role: Some(Role::Standard), applies_to: Some(many), ..Patch::default() };
    assert_eq!(update(&root, &entry.id, &patch), Err(KbError::BadAppliesTo));
    assert_eq!(update(&root, "nao-existe-1", &Patch::default()), Err(KbError::NotFound));
}

#[test]
fn remove_deletes_entry_source_and_note() {
    let root = root("remove");
    let entry = add_md(&root, "a.md", "texto");
    remove(&root, &entry.id).expect("remove");
    assert!(list(&root).expect("lista").is_empty());
    assert!(!root.join(SOURCES_REL).join(format!("{}.md", entry.id)).exists());
    assert!(!root.join(NOTES_REL).join(format!("{}.md", entry.id)).exists());
    assert_eq!(remove(&root, &entry.id), Err(KbError::NotFound));
}

#[test]
fn symlinks_are_refused_and_targets_untouched() {
    // A pasta da base é symlink.
    let root_a = root("link-dir");
    let outside = root_a.parent().expect("pai").join("fora");
    fs::create_dir_all(&outside).expect("fora");
    fs::create_dir_all(root_a.join(".sdd")).expect("sdd");
    symlink(&outside, root_a.join(KB_REL)).expect("symlink");
    assert_eq!(add_file(&root_a, "a.md", b"x", &none(), NOW), Err(KbError::Fs(SafeFsError::Unsafe)));
    assert_eq!(write_overview(&root_a, "x"), Err(KbError::Fs(SafeFsError::Unsafe)));
    assert_eq!(fs::read_dir(&outside).expect("fora").count(), 0, "nada gravado fora");

    // O manifesto é symlink: não é lido nem substituído.
    let root_b = root("link-manifest");
    fs::create_dir_all(root_b.join(KB_REL)).expect("kb");
    let target = root_b.parent().expect("pai").join("alvo.json");
    fs::write(&target, "{\"version\":1,\"docs\":[]}").expect("alvo");
    symlink(&target, root_b.join(MANIFEST_REL)).expect("symlink");
    assert_eq!(add_file(&root_b, "a.md", b"x", &none(), NOW), Err(KbError::Fs(SafeFsError::Unsafe)));
    assert_eq!(list(&root_b), Err(KbError::Fs(SafeFsError::Unsafe)));
    assert_eq!(fs::read_to_string(&target).expect("alvo"), "{\"version\":1,\"docs\":[]}");
    assert!(fs::symlink_metadata(root_b.join(MANIFEST_REL)).expect("link").file_type().is_symlink());

    // A nota de destino já existe como symlink: o envio é recusado e nada sobra.
    let root_c = root("link-note");
    let probe = add_md(&root_c, "a.md", "conteudo");
    remove(&root_c, &probe.id).expect("remove");
    let victim = root_c.parent().expect("pai").join("vitima.md");
    fs::write(&victim, "original").expect("vítima");
    symlink(&victim, root_c.join(NOTES_REL).join(format!("{}.md", probe.id))).expect("symlink");
    assert_eq!(add_file(&root_c, "a.md", b"conteudo", &none(), NOW), Err(KbError::Fs(SafeFsError::Unsafe)));
    assert_eq!(fs::read_to_string(&victim).expect("vítima"), "original");
    assert!(list(&root_c).expect("lista").is_empty());
    assert!(!root_c.join(SOURCES_REL).join(format!("{}.md", probe.id)).exists(), "original desfeito");

    // `sources/` é symlink.
    let root_d = root("link-sources");
    fs::create_dir_all(root_d.join(KB_REL)).expect("kb");
    let out_d = root_d.parent().expect("pai").join("fontes");
    fs::create_dir_all(&out_d).expect("fontes");
    symlink(&out_d, root_d.join(SOURCES_REL)).expect("symlink");
    assert_eq!(add_file(&root_d, "a.md", b"x", &none(), NOW), Err(KbError::Fs(SafeFsError::Unsafe)));
    assert_eq!(fs::read_dir(&out_d).expect("fontes").count(), 0);
}

#[test]
fn corrupt_manifest_is_reported() {
    let root = root("corrupt");
    fs::create_dir_all(root.join(KB_REL)).expect("kb");
    fs::write(root.join(MANIFEST_REL), "{nao e json").expect("w");
    assert_eq!(list(&root), Err(KbError::Corrupt));
    assert_eq!(add_file(&root, "a.md", b"x", &none(), NOW), Err(KbError::Corrupt));
    assert_eq!(fs::read_to_string(root.join(MANIFEST_REL)).expect("r"), "{nao e json", "não sobrescreve");
}

#[test]
fn concurrent_uploads_all_land_in_the_manifest() {
    let root = Arc::new(root("concurrent"));
    let handles: Vec<_> = (0..16)
        .map(|i| {
            let root = Arc::clone(&root);
            std::thread::spawn(move || add_file(&root, &format!("doc-{i}.md"), format!("conteudo {i}").as_bytes(), &none(), NOW))
        })
        .collect();
    let mut ids: Vec<String> = handles.into_iter().map(|h| h.join().expect("thread").expect("add").id).collect();
    ids.sort();
    let mut listed: Vec<String> = list(&root).expect("lista").into_iter().map(|d| d.id).collect();
    listed.sort();
    assert_eq!(listed, ids);
    // Nenhum temporário sobrou.
    let leftovers = fs::read_dir(root.join(KB_REL)).expect("kb").flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
    assert_eq!(leftovers, 0);
}

#[test]
fn overview_round_trip_and_cap() {
    let root = root("overview");
    assert_eq!(read_overview(&root, &none()).expect("vazia"), "");
    write_overview(&root, "# Visão geral\nPadrões da empresa.\n").expect("gravar");
    assert_eq!(read_overview(&root, &none()).expect("ler"), "# Visão geral\nPadrões da empresa.\n");
    write_overview(&root, "outra").expect("regravar");
    assert_eq!(read_overview(&root, &none()).expect("ler"), "outra");
    assert_eq!(write_overview(&root, &"a".repeat(MAX_OVERVIEW_BYTES + 1)), Err(KbError::OverviewTooLarge));
    write_overview(&root, "  ").expect("apagar");
    assert!(!root.join(OVERVIEW_REL).exists());
}

#[test]
fn note_body_handles_frontmatter_shapes() {
    assert_eq!(note_body("---\na: 1\n---\ncorpo"), "corpo");
    assert_eq!(note_body("---\na: 1\n---\n"), "");
    assert_eq!(note_body("sem frontmatter"), "sem frontmatter");
    assert_eq!(note_body("---\na: 1\n---"), "");
}

#[test]
fn accents_fold_in_slugs() {
    assert_eq!(slugify("Ação São João Ñandu"), "acao-sao-joao-nandu");
    assert_eq!(slugify("***"), "doc");
    assert!(slugify(&"palavra ".repeat(20)).len() <= SLUG_MAX_CHARS);
}

// --- spec 134/T02 ---

#[test]
fn markdown_keeps_source_frontmatter_and_normalizes_newlines() {
    let root = root("t02-md");
    let text = "---\r\ntitle: Guia de Testes\r\ndescription: Como testar os serviços.\r\n---\r\n# Testes\r\n\r\nUse mocks.\r\n";
    let entry = add_md(&root, "testes.md", text);
    assert_eq!(entry.title, "Guia de Testes", "título do frontmatter de origem");
    assert_eq!(entry.description, "Como testar os serviços.");
    let note = fs::read_to_string(root.join(NOTES_REL).join(format!("{}.md", entry.id))).expect("nota");
    assert!(!note.contains('\r'));
    assert_eq!(
        note_body(&note),
        "---\ntitle: Guia de Testes\ndescription: Como testar os serviços.\n---\n# Testes\n\nUse mocks.\n"
    );
    // O original fica intocado.
    assert_eq!(fs::read(root.join(SOURCES_REL).join(format!("{}.md", entry.id))).expect("original"), text.as_bytes());
}

#[test]
fn redaction_on_write_is_on_disk() {
    let root = root("t02-redact");
    let entry = add_md(&root, "acesso.md", "banco em https://admin:senha-forte@db.local/x\n");
    let note = fs::read_to_string(root.join(NOTES_REL).join(format!("{}.md", entry.id))).expect("nota");
    assert!(!note.contains("senha-forte"), "{note}");
}

#[test]
fn title_and_file_name_are_redacted_in_manifest_and_note() {
    let root = root("t02-names");
    let entry = add_file(&root, &format!("chave {SECRET}.md"), b"# Texto comum\n", &secrets(), NOW).expect("add");
    assert!(!entry.title.contains(SECRET) && !entry.id.contains("segredo"), "{entry:?}");
    assert!(!entry.file_name.as_deref().unwrap_or_default().contains(SECRET));
    let manifest = fs::read_to_string(root.join(MANIFEST_REL)).expect("manifesto");
    assert!(!manifest.contains(SECRET), "{manifest}");
    let note = fs::read_to_string(root.join(NOTES_REL).join(format!("{}.md", entry.id))).expect("nota");
    assert!(!note.contains(SECRET), "{note}");
}

#[test]
fn links_between_documents_follow_the_base() {
    let root = root("t02-links");
    let a = add_md(&root, "visao.md", "Veja [as regras](./docs/Regras%20API.md#erros) e [o site](https://exemplo.com/guia).\n");
    assert!(a.links_out.is_empty(), "o alvo ainda não está na base");
    let b = add_md(&root, "Regras API.md", "# Regras\n");
    let link = add_link(&root, "https://exemplo.com/guia#topo", &none(), NOW).expect("link");
    let docs = list(&root).expect("lista");
    assert_eq!(docs[0].links_out, vec![b.id.clone(), link.id.clone()]);
    assert!(docs[1].links_out.is_empty());
    remove(&root, &b.id).expect("remove");
    assert_eq!(list(&root).expect("lista")[0].links_out, vec![link.id]);
}

#[test]
fn extraction_states_and_secret_in_extracted_text() {
    let root = root("t02-states");
    let pdf = add_file(&root, "a.pdf", &test_pdf::pdf_with(&[&format!("O manual de implantação traz o token {SECRET} no meio do texto, o que não pode entrar na base.")], true), &secrets(), NOW)
        .expect("pdf: o segredo não aparece nos bytes");
    assert_eq!(queued_ids(&root).expect("fila"), vec![pdf.id.clone()]);
    let begun = begin_extraction(&root, &pdf.id).expect("begin").expect("entrada");
    assert_eq!(begun.status, Status::Extracting);
    assert!(begin_extraction(&root, "nao-existe-1").expect("begin").is_none());
    let bytes = source_bytes(&root, &begun).expect("original");
    let outcome = extract::pdf(&bytes).map(|ex| (ex, None));
    let done = finish_extraction(&root, &pdf.id, &begun.added_at, outcome, &secrets(), NOW).expect("finish").expect("entrada");
    assert_eq!((done.status, done.status_reason.as_deref()), (Status::Error, Some("kb_secret_content")));
    assert!(!root.join(SOURCES_REL).join(format!("{}.pdf", pdf.id)).exists(), "o original sai");
    let note = fs::read_to_string(root.join(NOTES_REL).join(format!("{}.md", pdf.id))).expect("nota");
    assert!(!note.contains(SECRET) && note.contains("status: \"error\""), "{note}");
    assert!(queued_ids(&root).expect("fila").is_empty());
    // Fora de `extracting`, o resultado não é gravado.
    let late = finish_extraction(&root, &pdf.id, &begun.added_at, Err("kb_extract_failed"), &secrets(), NOW).expect("finish");
    assert!(late.is_none());
    // Manifesto antigo com `failed` é lido como `error`.
    let old: Status = serde_json::from_str("\"failed\"").expect("status");
    assert_eq!(old, Status::Error);
    assert_eq!(serde_json::to_string(&Status::NoText).expect("json"), "\"no_text\"");
}

#[test]
fn link_targets_with_a_configured_secret_stay_out_of_the_manifest() {
    let root = root("t02-targets");
    let link = add_link(&root, "https://exemplo.com/guia", &secrets(), NOW).expect("link");
    let begun = begin_extraction(&root, &link.id).expect("begin").expect("entrada");
    let mut ex = extract::text("Guia de integração dos serviços internos da empresa com o gateway de APIs.", false);
    ex.link_targets = vec![format!("url:https://exemplo.com/x?token={SECRET}"), "url:https://exemplo.com/ok".to_string()];
    let done = finish_extraction(&root, &link.id, &begun.added_at, Ok((ex, None)), &secrets(), NOW).expect("finish").expect("entrada");
    assert_eq!(done.status, Status::Ready);
    assert_eq!(done.link_targets, vec!["url:https://exemplo.com/ok".to_string()]);
    assert!(!fs::read_to_string(root.join(MANIFEST_REL)).expect("manifesto").contains(SECRET));
}

#[test]
fn link_original_with_a_secret_is_refused_like_an_upload() {
    let root = root("t03-link-original");
    let clean = "Guia de integração dos serviços internos da empresa com o gateway de APIs.";
    // Segredo configurado fora do texto principal (num atributo do HTML) e chave privada PEM.
    let originals = [
        format!("<html><head><meta name=\"x\" content=\"{SECRET}\"></head><body><p>{clean}</p></body></html>"),
        format!("<html><body><p>{clean}</p><!-- -----{}PRIVATE KEY-----\nabc --></body></html>", "BEGIN "),
    ];
    for (n, html) in originals.iter().enumerate() {
        let link = add_link(&root, &format!("https://exemplo.com/guia-{n}"), &secrets(), NOW).expect("link");
        let begun = begin_extraction(&root, &link.id).expect("begin").expect("entrada");
        let source = extract::LinkSource { ext: "html", bytes: html.as_bytes().to_vec() };
        let outcome = Ok((extract::text(clean, false), Some(source)));
        let done = finish_extraction(&root, &link.id, &begun.added_at, outcome, &secrets(), NOW).expect("finish").expect("entrada");
        assert_eq!((done.status, done.status_reason.as_deref()), (Status::Error, Some("kb_secret_content")), "{n}");
        assert!(!root.join(SOURCES_REL).join(format!("{}.html", link.id)).exists(), "o original não é gravado");
        assert_eq!(done.size, 0);
    }
    // O mesmo original sem segredo é gravado.
    let link = add_link(&root, "https://exemplo.com/limpo", &secrets(), NOW).expect("link");
    let begun = begin_extraction(&root, &link.id).expect("begin").expect("entrada");
    let html = format!("<p>{clean}</p>");
    let source = extract::LinkSource { ext: "html", bytes: html.clone().into_bytes() };
    let done = finish_extraction(&root, &link.id, &begun.added_at, Ok((extract::text(clean, false), Some(source))), &secrets(), NOW)
        .expect("finish")
        .expect("entrada");
    assert_eq!(done.status, Status::Ready);
    assert_eq!(fs::read(root.join(SOURCES_REL).join(format!("{}.html", link.id))).expect("original"), html.as_bytes());
}
