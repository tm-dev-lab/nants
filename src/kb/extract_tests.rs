//! Testes da extração (spec 134/T02): cada formato com fixtures pequenos gerados aqui.

use super::*;
use crate::kb::test_pdf;

fn page(content_type: &str, body: &str) -> Page {
    Page {
        url: "https://docs.exemplo.com/guia/inicio".to_string(),
        content_type: content_type.to_string(),
        body: body.as_bytes().to_vec(),
        truncated: false,
    }
}

const PARAGRAPH: &str = "A arquitetura do sistema separa o domínio da infraestrutura em camadas bem definidas. \
    Cada serviço expõe uma API REST documentada e versionada.";

#[test]
fn markdown_newlines_frontmatter_and_description() {
    let raw = "---\r\ntitle: Padrões de API\r\nsummary: Regras para APIs REST da empresa.\r\ntags: [api]\r\n---\r\n# API\r\n\r\nUse REST.\rFim.\r\n";
    let ex = text(raw, true);
    assert_eq!(ex.status, Status::Ready);
    assert!(!ex.body.contains('\r'), "{:?}", ex.body);
    // O frontmatter de origem fica no corpo, intocado (só o final de linha muda).
    assert!(ex.body.starts_with("---\ntitle: Padrões de API\nsummary: Regras para APIs REST da empresa.\ntags: [api]\n---\n# API\n"));
    assert_eq!(ex.title.as_deref(), Some("Padrões de API"));
    assert_eq!(ex.description.as_deref(), Some("Regras para APIs REST da empresa."));
    assert_eq!(ex.extractor, RAW_EXTRACTOR);

    // `description` vence `summary`; sem frontmatter, a primeira frase.
    let ex = text("---\ndescription: Do frontmatter.\nsummary: Outro.\n---\nTexto.\n", true);
    assert_eq!(ex.description.as_deref(), Some("Do frontmatter."));
    let ex = text(&format!("# Título\n\n```\ncodigo()\n```\n\n{PARAGRAPH}\n"), true);
    assert_eq!(
        ex.description.as_deref(),
        Some("A arquitetura do sistema separa o domínio da infraestrutura em camadas bem definidas.")
    );
    assert_eq!(ex.title, None);
    // Texto puro: o frontmatter não é interpretado.
    let ex = text("---\ndescription: x\n---\nLinha um do texto.\n", false);
    assert_eq!(ex.description.as_deref(), Some("description: x"));
    assert!(ex.link_targets.is_empty());
    // BOM sai.
    assert_eq!(text("\u{feff}ok\n", false).body, "ok\n");
}

#[test]
fn every_reason_has_text_in_both_languages_on_server_and_web() {
    use crate::i18n::{lookup, Lang};
    let web_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/i18n");
    let web: Vec<String> = ["ptBR.ts", "en.ts"]
        .iter()
        .map(|f| std::fs::read_to_string(web_dir.join(f)).expect("dicionário da web"))
        .collect();
    for code in reason::ALL {
        let key = format!("kb.reason.{code}");
        for lang in [Lang::PtBr, Lang::En] {
            assert!(lookup(lang, &key).is_some_and(|t| !t.is_empty()), "{key} sem texto no servidor ({lang:?})");
        }
        assert_ne!(lookup(Lang::PtBr, &key), lookup(Lang::En, &key), "{key}: o en não pode repetir o pt-BR");
        for dict in &web {
            assert!(dict.contains(&format!("  \"{key}\": \"")), "{key} sem texto na web");
        }
    }
}

#[test]
fn first_sentence_skips_structure_and_fits_one_line() {
    assert_eq!(first_sentence("# T\n\n| a | b |\n![img](x.png)\n\n- [Guia](guia.md) para **novos** times. Resto.").as_deref(), Some("Guia para novos times."));
    assert_eq!(first_sentence("Sem ponto final"), Some("Sem ponto final".to_string()));
    assert_eq!(first_sentence("# Só título\n\n```\nx\n```\n"), None);
    let long = "palavra ".repeat(60);
    let cut = first_sentence(&long).expect("frase");
    assert!(cut.chars().count() <= crate::kb::MAX_DESCRIPTION_CHARS && !cut.ends_with(' '), "{cut}");
    assert_eq!(first_sentence("v1.2 é a versão. Depois."), Some("v1.2 é a versão.".to_string()));
}

#[test]
fn markdown_links_keep_base_files_and_urls() {
    let md = "Veja [API](./padroes/API%20REST.md#erros), [site](https://exemplo.com/a#b), \
        [âncora](#topo), [mail](mailto:x@y.z), [img](foto.png) e [ref].\n\n[ref]: ../manual.pdf\n";
    assert_eq!(
        markdown_links(md),
        vec![
            "file:api rest.md".to_string(),
            "url:https://exemplo.com/a".to_string(),
            "file:manual.pdf".to_string()
        ]
    );
}

#[test]
fn clean_and_readable() {
    assert_eq!(clean_extracted("di\u{FB03}cil\u{0}\u{1}\r\n\n\n\nfim\u{FFFD}\t."), "difficil\n\nfim .");
    assert!(readable(PARAGRAPH));
    assert!(!readable("curto"));
    assert!(!readable(&"%&#()*+ ab ".repeat(20)), "mais símbolo que letra");
    assert!(readable(&format!("{PARAGRAPH} 2024 2025 2026 | 10 | 20 |")));
}

#[test]
fn pdf_pages_become_sections_with_accents() {
    let bytes = test_pdf::pdf(&[
        &format!("Política de Segurança\n{PARAGRAPH}"),
        "Segunda página: atenção à configuração dos serviços e à revisão das dependências do projeto.",
    ]);
    let ex = pdf(&bytes).expect("pdf");
    assert_eq!(ex.status, Status::Ready, "{ex:?}");
    assert_eq!(ex.pages, Some(2));
    assert_eq!(ex.extractor, PDF_EXTRACTOR);
    let p1 = ex.body.find("# p. 1\n").expect("p. 1");
    let p2 = ex.body.find("# p. 2\n").expect("p. 2");
    assert!(p1 < p2);
    assert!(ex.body[p1..p2].contains("Política de Segurança"), "{}", ex.body);
    assert!(ex.body[p2..].contains("atenção à configuração"), "{}", ex.body);
    assert!(ex.description.as_deref().is_some_and(|d| d.contains("Política de Segurança")), "{:?}", ex.description);
}

#[test]
fn pdf_without_text_is_no_text_and_garbage_is_refused() {
    // Escaneado: páginas sem camada de texto.
    let ex = pdf(&test_pdf::pdf(&["", "", ""])).expect("pdf");
    assert_eq!((ex.status, ex.reason), (Status::NoText, Some(reason::PDF_NO_TEXT)));
    assert_eq!(ex.pages, Some(3));
    assert!(ex.body.contains("# p. 3") && ex.description.is_none());
    // Menos da metade das páginas com texto também.
    let ex = pdf(&test_pdf::pdf(&[PARAGRAPH, "", "", ""])).expect("pdf");
    assert_eq!(ex.status, Status::NoText);
    // Metade ou mais: pronto, e a página vazia fica só com o título.
    let ex = pdf(&test_pdf::pdf(&[PARAGRAPH, ""])).expect("pdf");
    assert_eq!(ex.status, Status::Ready);
    assert!(ex.body.ends_with("# p. 2\n"), "{:?}", ex.body);

    assert_eq!(pdf(b"%PDF-1.7\nlixo sem estrutura"), Err(reason::PDF_UNREADABLE));
    // Protegido por senha: motivo próprio, não "ilegível".
    assert_eq!(pdf(&test_pdf::encrypted(&[PARAGRAPH])), Err(reason::PDF_ENCRYPTED));
    let whole = test_pdf::pdf(&[PARAGRAPH]);
    assert_eq!(pdf(&whole[..whole.len() / 3]), Err(reason::PDF_UNREADABLE));
}

#[test]
fn html_page_with_meta_title_links_and_main_content() {
    let html = format!(
        "<html><head><title> Guia de\n Estilo </title><meta name=\"description\" content=\"Regras de estilo do time.\"></head>\
         <body><nav><a href=\"/menu\">Menu</a></nav><main><h1>Estilo</h1><p>{PARAGRAPH}</p>\
         <ul><li>Um</li><li>Dois</li></ul><pre>fn main() {{\n    ok();\n}}</pre>\
         <a href=\"../api#x\">API</a> <a href=\"https://outro.com/p\">fora</a> <a href=\"#topo\">topo</a></main>\
         <footer>Rodapé</footer></body></html>"
    );
    let (ex, source) = super::page(&page("text/html; charset=utf-8", &html)).expect("html");
    assert_eq!(ex.status, Status::Ready);
    assert_eq!(ex.title.as_deref(), Some("Guia de Estilo"));
    assert_eq!(ex.description.as_deref(), Some("Regras de estilo do time."));
    assert_eq!(ex.extractor, HTML_EXTRACTOR);
    assert!(ex.body.starts_with("# Estilo\n"), "{}", ex.body);
    assert!(ex.body.contains("- Um\n- Dois") && ex.body.contains("```\nfn main() {\n    ok();\n}\n```"), "{}", ex.body);
    assert!(!ex.body.contains("Menu") && !ex.body.contains("Rodapé"), "{}", ex.body);
    assert_eq!(
        ex.link_targets,
        vec![
            "url:https://docs.exemplo.com/menu".to_string(),
            "url:https://docs.exemplo.com/api".to_string(),
            "url:https://outro.com/p".to_string()
        ]
    );
    assert_eq!((source.ext, source.bytes.len()), ("html", html.len()));
    // Sem `<meta name="description">`: a primeira frase.
    let (ex, _) = super::page(&page("text/html", &format!("<p>{PARAGRAPH}</p>"))).expect("html");
    assert!(ex.description.as_deref().is_some_and(|d| d.starts_with("A arquitetura")), "{:?}", ex.description);
}

#[test]
fn page_types_and_no_text() {
    let shell = "<html><head><title>App</title><script>render()</script></head><body><div id=\"root\"></div>\
        <noscript>Ative o JavaScript para usar este site.</noscript></body></html>";
    let (ex, _) = super::page(&page("text/html", shell)).expect("html");
    assert_eq!((ex.status, ex.reason), (Status::NoText, Some(reason::PAGE_NO_TEXT)));
    assert!(ex.description.is_none());

    let (ex, source) = super::page(&page("text/markdown", &format!("---\ndescription: Do link.\n---\n{PARAGRAPH}\n"))).expect("md");
    assert_eq!((ex.status, ex.description.as_deref(), source.ext), (Status::Ready, Some("Do link."), "md"));
    let (ex, source) = super::page(&page("text/plain", PARAGRAPH)).expect("txt");
    assert_eq!((ex.status, source.ext), (Status::Ready, "txt"));

    let mut img = page("image/png", "x");
    img.body = vec![0x89, b'P', b'N', b'G', 0];
    assert_eq!(super::page(&img), Err(reason::LINK_TYPE));

    let mut doc = page("application/pdf", "");
    doc.body = test_pdf::pdf(&[PARAGRAPH]);
    let (ex, source) = super::page(&doc).expect("pdf");
    assert_eq!((ex.status, ex.pages, source.ext), (Status::Ready, Some(1), "pdf"));
    doc.truncated = true;
    assert_eq!(super::page(&doc), Err(reason::LINK_TOO_LARGE));

    // Latin-1 declarado no cabeçalho.
    let mut latin = page("text/html; charset=ISO-8859-1", "");
    latin.body = b"<p>Configura\xe7\xe3o do servi\xe7o de autentica\xe7\xe3o e das permiss\xf5es de acesso ao sistema.</p>".to_vec();
    let (ex, _) = super::page(&latin).expect("latin");
    assert!(ex.body.contains("Configuração do serviço"), "{}", ex.body);
}
