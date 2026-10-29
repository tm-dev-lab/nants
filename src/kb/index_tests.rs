//! Testes do índice (spec 134/T03): fatiamento, ranking, acentos, frase, pesos, invalidação,
//! PDF por página e concorrência. A medição de desempenho é um teste ignorado
//! (`kb_index_bench`), fora da suíte padrão.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::kb::{self, test_pdf, Origin, Patch};

const NOW: &str = "2026-10-28T12:00:00Z";

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nants-kbi-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let root = dir.join("repo");
    fs::create_dir_all(&root).expect("mkdir");
    root
}

fn none() -> SecretSet {
    SecretSet::new(Vec::<String>::new())
}

fn entry(id: &str, title: &str, kind: Kind) -> Entry {
    Entry {
        id: id.to_string(),
        title: title.to_string(),
        kind,
        source: Origin::File,
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
        role: Role::Reference,
        description: String::new(),
        applies_to: Vec::new(),
        links_out: Vec::new(),
        link_targets: Vec::new(),
    }
}

fn md(id: &str, title: &str, body: &str) -> (Entry, String) {
    (entry(id, title, Kind::Markdown), body.to_string())
}

fn ids(hits: &[Hit]) -> Vec<String> {
    hits.iter().map(|h| format!("{}#{}", h.doc_id, h.section)).collect()
}

const FILLER: &str = "O sistema registra as decisões em documentos curtos, revisados pela equipe a cada ciclo de entrega. ";

#[test]
fn tokens_fold_case_and_accents_without_stemming() {
    let terms: Vec<String> = tokens("Configuração, SEGURANÇA e Ação-rápida_2 über Ñandú").into_iter().map(|(_, t)| t).collect();
    assert_eq!(terms, ["configuracao", "seguranca", "e", "acao", "rapida", "2", "uber", "nandu"]);
    // Texto decomposto (letra + acento combinante) vira o mesmo termo.
    assert_eq!(terms_of("Seguranc\u{327}a"), ["seguranca"]);
    // Sem stemming: plural e singular são termos diferentes.
    assert_ne!(terms_of("serviços"), terms_of("serviço"));
    let (range, _) = &tokens("  olá mundo")[0];
    assert_eq!(*range, 2..6);
    assert!(tokens(&"a".repeat(65)).is_empty(), "termo enorme fica de fora");
}

#[test]
fn sections_keep_the_trail_and_big_ones_split_by_paragraph() {
    let big: String = (0..30).map(|i| format!("Parágrafo {i}. {FILLER}{FILLER}\n\n")).collect();
    let body = format!("Texto de abertura.\n\n# Guia\n\nintro\n\n## Erros\n\nerros\n\n### Códigos\n\n{big}## Logs\n\nlogs\n");
    let index = Index::build(vec![md("guia-1", "Guia", &body)]);
    let list = index.sections("guia-1", &none()).expect("seções");
    let shape: Vec<(&str, usize, &str)> = list.iter().map(|s| (s.anchor.as_str(), s.level, s.trail.as_str())).collect();
    assert_eq!(
        shape,
        [
            ("top", 0, "Guia"),
            ("guia", 1, "Guia"),
            ("erros", 2, "Guia > Erros"),
            ("codigos", 3, "Guia > Erros > Códigos"),
            ("logs", 2, "Guia > Logs"),
        ]
    );
    // A seção grande virou vários trechos, cada um até o teto e cortado entre parágrafos.
    let doc = &index.docs[0];
    let codes: Vec<&Chunk> = index.chunks.iter().filter(|c| doc.sections[c.section as usize].anchor == "codigos").collect();
    assert!(codes.len() > 1, "{}", codes.len());
    for c in &codes {
        let text = index.chunk_text(c);
        assert!(text.len() <= CHUNK_MAX_BYTES, "{}", text.len());
        assert!(text.trim_start().starts_with("Parágrafo") || text.starts_with("### Códigos"), "{text:.40}");
    }
    // A leitura devolve a seção com as subseções; a lista dá o tamanho inteiro.
    let erros = index.section("guia-1", "erros", &none()).expect("erros");
    assert!(erros.text.starts_with("## Erros") && erros.text.contains("### Códigos") && !erros.text.contains("## Logs"));
    assert_eq!(list[2].bytes, erros.text.len());
    assert_eq!(index.section("guia-1", "CÓDIGOS", &none()).expect("pelo título").anchor, "codigos");
    assert!(index.section("guia-1", "nao-existe", &none()).is_none() && index.section("outro", "erros", &none()).is_none());
    // Títulos repetidos ganham âncoras distintas.
    let rep = Index::build(vec![md("r-1", "R", "## Notas\na\n## Notas\nb\n")]);
    let anchors: Vec<String> = rep.sections("r-1", &none()).expect("seções").into_iter().map(|s| s.anchor).collect();
    assert_eq!(anchors, ["notas", "notas-2"]);
}

#[test]
fn split_ranges_cut_on_paragraph_line_word_or_char() {
    let text = format!("{}\n\n{}", "a ".repeat(1500), "b ".repeat(1500));
    let parts = split_ranges(&text, 4096);
    assert!(parts.len() == 2 && text[parts[0].clone()].ends_with("\n\n"), "{parts:?}");
    let word = "ç".repeat(5000);
    let parts = split_ranges(&word, 4096);
    assert!(parts.iter().all(|r| word.is_char_boundary(r.start) && word.is_char_boundary(r.end) && r.len() <= 4096));
    assert_eq!(parts.iter().map(|r| r.len()).sum::<usize>(), word.len());
}

#[test]
fn ranking_prefers_frequency_rarity_and_the_title() {
    let docs = vec![
        md("a-1", "Operação", &format!("## Rotina\n{FILLER} cache {FILLER}\n")),
        md("b-1", "Desempenho", &format!("## Cache\ncache de leitura, cache de escrita e invalidação do cache.\n{FILLER}\n")),
        md("c-1", "Outros", &format!("## Geral\n{FILLER}\n")),
    ];
    let index = Index::build(docs);
    let hits = index.search("cache", 10, &none());
    assert_eq!(ids(&hits), ["b-1#cache", "a-1#rotina"]);
    assert!(hits[0].score > hits[1].score);
    assert_eq!(hits[0].citation(), "kb:b-1#cache");
    assert_eq!(hits[0].trail, "Desempenho > Cache");
    // OR ranqueado: quem tem os dois termos vem antes de quem tem um.
    let hits = index.search("invalidação rotina", 10, &none());
    assert_eq!(hits.len(), 2);
    let both = Index::build(vec![
        md("x-1", "X", &format!("## S\ncache {FILLER}\n")),
        md("y-1", "Y", &format!("## S\ncache e fila {FILLER}\n")),
    ]);
    assert_eq!(ids(&both.search("fila cache", 10, &none())), ["y-1#s", "x-1#s"]);
    assert_eq!(both.search("fila cache", 1, &none()).len(), 1, "o máximo vale");
    assert!(index.search("", 10, &none()).is_empty() && index.search("!!! ??", 10, &none()).is_empty());
    assert!(index.search("inexistente", 10, &none()).is_empty());
}

#[test]
fn accents_and_case_do_not_matter_in_either_direction() {
    let index = Index::build(vec![
        md("p-1", "Política", "## Segurança da Informação\nA configuração exige revisão.\n"),
        md("q-1", "Plain", "## Security\nseguranca sem acento aqui.\n"),
    ]);
    let hits = index.search("SEGURANCA", 10, &none());
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].doc_id, "p-1", "o título pesa mais");
    assert_eq!(index.search("configuracao", 10, &none())[0].doc_id, "p-1");
    assert_eq!(index.search("Segurança", 10, &none()).len(), 2);
    let read = index.section("p-1", "seguranca da informacao", &none()).expect("pelo título sem acento");
    assert_eq!(read.anchor, "seguranca-da-informacao");
}

#[test]
fn quoted_phrase_must_appear_in_sequence() {
    let index = Index::build(vec![
        md("a-1", "A", "## Regra\nToda mudança passa por revisão de código antes do merge.\n"),
        md("b-1", "B", "## Outra\nO código da revisão fica no histórico, de forma separada.\n"),
    ]);
    assert_eq!(index.search("revisão código", 10, &none()).len(), 2);
    assert_eq!(ids(&index.search("\"revisão de código\"", 10, &none())), ["a-1#regra"]);
    assert_eq!(ids(&index.search("merge \"Revisao de Codigo\"", 10, &none())), ["a-1#regra"]);
    assert!(index.search("\"código de revisão\"", 10, &none()).is_empty());
    assert!(index.search("\"revisão de inexistente\"", 10, &none()).is_empty());
    // A frase também vale na trilha.
    assert_eq!(ids(&index.search("\"a regra\"", 10, &none())), ["a-1#regra"]);
    let (terms, phrases) = parse_query("x \"y z\" \"sem fim");
    assert_eq!(terms, ["x", "y", "z", "sem", "fim"]);
    assert_eq!(phrases, vec![vec!["y".to_string(), "z".to_string()]]);
}

#[test]
fn description_tags_and_title_weigh_more_than_the_body() {
    let body_only = md("corpo-1", "Notas", &format!("## Geral\n{FILLER} kubernetes {FILLER}\n"));
    let mut described = md("desc-1", "Notas", &format!("## Geral\n{FILLER}\n"));
    described.0.description = "Guia de kubernetes da empresa.".to_string();
    let tagged = md("tags-1", "Notas", &format!("---\ntitle: Notas\ntags: [kubernetes, deploy]\n---\n## Geral\n{FILLER}\n"));
    let titled = md("titulo-1", "Kubernetes", &format!("## Geral\n{FILLER}\n"));
    let index = Index::build(vec![body_only, described, tagged, titled]);
    let order = index.search("kubernetes", 10, &none());
    let docs: Vec<&str> = order.iter().map(|h| h.doc_id.as_str()).collect();
    assert_eq!(docs.len(), 4);
    assert_eq!(docs.last(), Some(&"corpo-1"), "{docs:?}");
    assert!(docs[..3].contains(&"desc-1") && docs[..3].contains(&"tags-1") && docs[..3].contains(&"titulo-1"));
    // Tag em texto separado por vírgula também vale; a tag não vira seção nem trecho próprio.
    let csv = Index::build(vec![md("c-1", "C", "---\nkeywords: \"Observabilidade, logs\"\n---\n## S\ntexto\n")]);
    assert_eq!(ids(&csv.search("observabilidade", 10, &none())), ["c-1#s"]);
    assert_eq!(ids(&csv.search("\"observabilidade\"", 10, &none())), ["c-1#s"]);
}

#[test]
fn snippet_is_around_the_terms_and_within_size() {
    let long: String = (0..40).map(|i| format!("Frase número {i} sobre assuntos gerais da equipe. ")).collect();
    let text = format!("{long} O ponto central é o rollback automático do deploy. {long}");
    let s = snippet(&text, &terms_of("rollback deploy"));
    let n = s.chars().count();
    assert!((SNIPPET_MIN_CHARS..=SNIPPET_MAX_CHARS + 6).contains(&n), "{n}: {s}");
    assert!(s.contains("rollback automático do deploy") && s.starts_with("...") && s.ends_with("..."), "{s}");
    assert_eq!(snippet("curto\n\ntexto", &[]), "curto texto");
    let index = Index::build(vec![md("s-1", "S", &format!("## S\n{text}\n"))]);
    let hit = &index.search("rollback", 1, &none())[0];
    assert!(hit.snippet.contains("rollback"), "{}", hit.snippet);
}

#[test]
fn pdf_is_indexed_page_by_page() {
    let root = root("pdf");
    let pages = [
        "Introdução geral aos padrões de arquitetura adotados pela empresa em todos os sistemas.",
        "Página sobre filas: toda mensagem precisa de idempotência e de reprocessamento seguro.",
        "Página final com o glossário de termos e a lista de responsáveis por cada padrão.",
    ];
    let pdf = kb::add_file(&root, "Padrões.pdf", &test_pdf::pdf(&pages), &none(), NOW).expect("add");
    let begun = kb::begin_extraction(&root, &pdf.id).expect("begin").expect("na fila");
    let bytes = kb::source_bytes(&root, &begun).expect("original");
    let outcome = extract::pdf(&bytes).map(|ex| (ex, None));
    kb::finish_extraction(&root, &pdf.id, &begun.added_at, outcome, &none(), NOW).expect("finish").expect("entrada");
    let hits = search(&root, "idempotência", 5, &none()).expect("busca");
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].section.as_str(), hits[0].page), ("p-2", Some(2)));
    assert_eq!(hits[0].trail, "Padrões > p. 2");
    assert_eq!(hits[0].citation(), format!("kb:{}#p-2", pdf.id));
    let list = list_sections(&root, &pdf.id, &none()).expect("seções");
    assert_eq!(list.iter().map(|s| s.page).collect::<Vec<_>>(), [Some(1), Some(2), Some(3)]);
    let page = read_section(&root, &pdf.id, "p. 3", &none()).expect("página");
    assert!(page.text.starts_with("# p. 3") && page.text.contains("glossário") && !page.text.contains("idempotência"));
    assert_eq!(read_section(&root, &pdf.id, "p-9", &none()), Err(KbError::NotFound));
    // Uma linha "# p. N" falsa no meio do texto não existe: a nota só tem as do extrator.
    let fake = Index::build(vec![(entry("f-1", "F", Kind::Pdf), "# p. 1\n\nUm\n## não é título\n\n# p. 2\n\n".to_string())]);
    let list = fake.sections("f-1", &none()).expect("seções");
    assert_eq!(list.len(), 2);
    assert_eq!(fake.chunks.len(), 1, "página vazia fica fora da busca");
}

#[test]
fn lazy_build_is_invalidated_by_the_manifest() {
    let root = root("invalidate");
    assert!(search(&root, "qualquer", 5, &none()).expect("base vazia").is_empty());
    let a = kb::add_file(&root, "a.md", b"# Alfa\n\nA fila de mensagens usa retentativas.\n", &none(), NOW).expect("a");
    let first = index_for(&root).expect("índice");
    let builds = builds(&root);
    let again = index_for(&root).expect("índice");
    assert!(Arc::ptr_eq(&first, &again), "sem mudança, o mesmo índice");
    assert_eq!(super::builds(&root), builds);
    assert_eq!(search(&root, "retentativas", 5, &none()).expect("busca").len(), 1);

    let b = kb::add_file(&root, "b.md", b"# Beta\n\nO cache guarda as retentativas.\n", &none(), NOW).expect("b");
    assert_eq!(search(&root, "retentativas", 5, &none()).expect("busca").len(), 2, "documento novo entra");
    assert_eq!(super::builds(&root), builds + 1);

    kb::update(&root, &b.id, &Patch { description: Some("Guia de cache".into()), ..Patch::default() }).expect("update");
    assert_eq!(search(&root, "guia", 5, &none()).expect("busca")[0].doc_id, b.id, "a descrição nova vale");

    kb::remove(&root, &a.id).expect("remove");
    let hits = search(&root, "retentativas", 5, &none()).expect("busca");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].doc_id, b.id);
    assert_eq!(list_sections(&root, &a.id, &none()), Err(KbError::NotFound));

    // Documento ainda na fila não entra; manifesto quebrado é relatado.
    let pending = kb::add_file(&root, "c.pdf", &test_pdf::pdf(&["texto"]), &none(), NOW).expect("pdf");
    assert_eq!(list_sections(&root, &pending.id, &none()), Err(KbError::NotFound));
    fs::write(root.join(MANIFEST_REL), "{ quebrado").expect("corromper");
    assert_eq!(search(&root, "cache", 5, &none()).map(|h| h.len()), Err(KbError::Corrupt));
    forget(&root);
}

#[test]
fn concurrent_searches_share_one_build_and_survive_writes() {
    let root = root("concurrency");
    for i in 0..10 {
        let text = format!("# Doc {i}\n\n## Seção\n\nTexto {i} sobre filas e mensagens.\n");
        kb::add_file(&root, &format!("d{i}.md"), text.as_bytes(), &none(), NOW).expect("add");
    }
    let before = builds(&root);
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || index_for(&root).expect("índice"))
        })
        .collect();
    let indexes: Vec<Arc<Index>> = handles.into_iter().map(|h| h.join().expect("thread")).collect();
    assert!(indexes.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])));
    assert_eq!(builds(&root), before + 1, "uma montagem só");

    // Buscas enquanto outra thread grava: nenhuma falha e, no fim, tudo está no índice.
    let writer = {
        let root = root.clone();
        std::thread::spawn(move || {
            for i in 10..20 {
                let text = format!("# Doc {i}\n\nTexto {i} sobre filas.\n");
                kb::add_file(&root, &format!("d{i}.md"), text.as_bytes(), &none(), NOW).expect("add");
            }
        })
    };
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || {
                for _ in 0..20 {
                    let hits = search(&root, "filas", MAX_RESULTS, &none()).expect("busca");
                    assert!(hits.len() >= 10);
                }
            })
        })
        .collect();
    writer.join().expect("writer");
    for r in readers {
        r.join().expect("reader");
    }
    assert_eq!(search(&root, "filas", MAX_RESULTS, &none()).expect("busca").len(), 20);
}

#[test]
fn output_is_redacted() {
    let secret = concat!("tok", "-redigir-0123456789");
    let index = Index::build(vec![md("s-1", "S", &format!("## Chave {secret}\nusar {secret} na fila\n"))]);
    let secrets = SecretSet::new(vec![secret.to_string()]);
    let hit = &index.search("fila", 1, &secrets)[0];
    assert!(!hit.snippet.contains(secret) && !hit.title.contains(secret) && !hit.trail.contains(secret), "{hit:?}");
    // A âncora (slug do título) também levaria o segredo: sai a forma neutra, aceita na leitura.
    assert!(!hit.section.contains("redigir") && !hit.citation().contains("redigir"), "{hit:?}");
    assert_eq!(hit.section, "s.0");
    let read = index.section("s-1", &hit.section, &secrets).expect("seção");
    assert!(!read.text.contains(secret) && read.anchor == "s.0");
    let list = index.sections("s-1", &secrets).expect("seções");
    assert!(list.iter().all(|s| !s.anchor.contains("redigir") && !s.title.contains(secret)), "{list:?}");
    // Sem o segredo configurado, a âncora é a do título.
    assert_eq!(index.search("fila", 1, &none())[0].section, "chave-tok-redigir-0123456789");
}

#[test]
fn a_long_secret_on_the_snippet_border_never_leaks_by_half() {
    let secret = format!("{}{}", concat!("tok", "-"), "a1b2c3d4e5".repeat(7));
    let secrets = SecretSet::new(vec![secret.clone()]);
    // O segredo aparece a cada poucas palavras: alguma ocorrência cai na borda do recorte.
    let mut body = String::from("## S\n");
    for i in 0..60 {
        body.push_str(&format!("palavra comum {i} rollback {secret} "));
    }
    let index = Index::build(vec![md("b-1", "B", &body)]);
    let hit = &index.search("rollback", 1, &secrets)[0];
    let chars: Vec<char> = secret.chars().collect();
    for w in chars.windows(12) {
        let piece: String = w.iter().collect();
        assert!(!hit.snippet.contains(&piece), "pedaço {piece} em {}", hit.snippet);
    }
    assert!(hit.snippet.contains("rollback"));
}

/// Medição da T03 (ignorada na suíte): corpus sintético de 50, 200 e 500 documentos de cerca de
/// 30 KB. Rodar com
/// `scripts/test.sh cargo test --release --lib kb_index_bench -- --ignored --nocapture`.
#[test]
#[ignore = "medição de desempenho, fora da suíte padrão"]
fn kb_index_bench() {
    use std::time::Instant;
    fn rss_kb() -> usize {
        fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()))
            .unwrap_or(0)
    }
    // Vocabulário sintético: palavras comuns, termos técnicos e alguns raros por documento.
    let common = [
        "sistema", "serviço", "equipe", "decisão", "padrão", "arquitetura", "camada", "domínio", "dados", "registro",
        "revisão", "código", "teste", "integração", "segurança", "configuração", "entrega", "ciclo", "documento", "regra",
    ];
    let tech = [
        "kubernetes", "postgres", "kafka", "redis", "grpc", "rest", "oauth", "jwt", "terraform", "docker", "nginx",
        "graphql", "rabbitmq", "elasticsearch", "prometheus", "grafana", "sentry", "vault", "helm", "argo",
    ];
    let doc_text = |d: usize| -> String {
        let mut out = format!("---\ntitle: Padrão {d}\ntags: [{}, {}]\n---\n# Padrão {d}\n\n", tech[d % 20], tech[(d * 7) % 20]);
        let mut seed = d as u64 * 2_654_435_761 + 1;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut h2 = 0;
        while out.len() < 30 * 1024 {
            h2 += 1;
            out.push_str(&format!("## Seção {h2} de {}\n\n", tech[(next() % 20) as usize]));
            for h3 in 0..3 {
                out.push_str(&format!("### Item {h2}.{h3}\n\n"));
                for _ in 0..2 {
                    let words: Vec<String> = (0..60)
                        .map(|_| match next() % 10 {
                            0 => tech[(next() % 20) as usize].to_string(),
                            1 => format!("termo{}x{}", d, next() % 50),
                            _ => common[(next() % 20) as usize].to_string(),
                        })
                        .collect();
                    out.push_str(&words.join(" "));
                    out.push_str(".\n\n");
                }
            }
        }
        out
    };
    let queries = ["kubernetes", "revisão de código", "\"arquitetura camada\"", "postgres kafka redis", "termo7x3 segurança"];
    for n in [50usize, 200, 500] {
        let root = root(&format!("bench-{n}"));
        let mut bytes = 0usize;
        for d in 0..n {
            let text = doc_text(d);
            bytes += text.len();
            kb::add_file(&root, &format!("padrao-{d}.md"), text.as_bytes(), &none(), NOW).expect("add");
        }
        let rss_before = rss_kb();
        let started = Instant::now();
        let index = index_for(&root).expect("índice");
        let build = started.elapsed();
        let rss_after = rss_kb();
        let stats = index.stats();
        let rounds = 20u32;
        let mut per_query: Vec<String> = Vec::new();
        let mut found = 0;
        let mut total = std::time::Duration::ZERO;
        for q in queries {
            let started = Instant::now();
            for _ in 0..rounds {
                found += index.search(q, 10, &none()).len();
            }
            let took = started.elapsed();
            total += took;
            per_query.push(format!("{q}={:.2}ms", (took / rounds).as_secs_f64() * 1000.0));
        }
        // A consulta pela raiz inclui a conferência do manifesto (leitura e SHA-256).
        let started = Instant::now();
        for _ in 0..rounds {
            found += search(&root, "kubernetes", 10, &none()).expect("busca").len();
        }
        let via_root = started.elapsed() / rounds;
        println!(
            "kb_index_bench docs={n} corpus={:.1}MB build={:.0}ms rss_before={}MB rss_after={}MB rss_delta={}MB \
             estimate={:.1}MB sections={} chunks={} terms={} postings={} query_avg={:.2}ms via_root={:.2}ms found={found}\n  {}",
            bytes as f64 / 1_048_576.0,
            build.as_secs_f64() * 1000.0,
            rss_before / 1024,
            rss_after / 1024,
            rss_after.saturating_sub(rss_before) / 1024,
            stats.approx_bytes as f64 / 1_048_576.0,
            stats.sections,
            stats.chunks,
            stats.terms,
            stats.postings,
            (total / (rounds * queries.len() as u32)).as_secs_f64() * 1000.0,
            via_root.as_secs_f64() * 1000.0,
            per_query.join(" "),
        );
        forget(&root);
        drop(index);
        let _ = fs::remove_dir_all(root.parent().unwrap_or(&root));
    }
}
