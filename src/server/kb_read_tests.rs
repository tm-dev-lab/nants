//! Testes da leitura da base para a interface (spec 136 T03): a seção com teto, os destaques da
//! busca (acento e segredo redigido no meio), as três listas de links, as fontes do plano e a
//! medição da busca durante um envio em lote. Sem rede e sem modelo.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::kb::{add_file, cite, index, remove};

const NOW: &str = "2026-10-29T12:00:00Z";

fn root(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("nants-kbread-{}-{n}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn add(root: &Path, name: &str, text: &str) -> Entry {
    add_file(root, name, text.as_bytes(), &SecretSet::default(), NOW).expect("documento")
}

fn spec_md(id: &str, body: &str) -> String {
    format!("---\nid: \"{id}\"\ntitle: \"Plano {id}\"\nstatus: \"AWAITING_APPROVAL\"\nbase_branch: \"main\"\ncreated_at: \"{NOW}\"\n---\n{body}")
}

fn task_md(spec: &str, id: &str, body: &str) -> String {
    format!(
        "---\nid: \"{id}\"\nspec_id: \"{spec}\"\ntitle: \"Tarefa {id}\"\nstatus: \"TODO\"\ndepends_on: []\nbranch: \"\"\nattempts: 0\n\
max_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope: [\"src/**\"]\n---\n{body}"
    )
}

fn write_plan(root: &Path, spec: &str, body: &str, tasks: &[(&str, &str)]) {
    let dir = root.join(format!(".sdd/specs/{spec}"));
    fs::create_dir_all(dir.join("tasks")).expect("tasks");
    fs::write(dir.join("spec.md"), spec_md(spec, body)).expect("spec");
    for (id, text) in tasks {
        fs::write(dir.join(format!("tasks/{id}.md")), task_md(spec, id, text)).expect("tarefa");
    }
}

// --- seção ---

#[test]
fn section_comes_with_subsections_and_a_ceiling() {
    let root = root("section");
    let big = "linha comprida de texto da seção\n".repeat(MAX_SECTION_BYTES / 20);
    let doc = add(&root, "guia.md", &format!("# Guia\n\n## Rotas\n\nUse /v2.\n\n### Erros\n\nLista.\n\n## Grande\n\n{big}"));
    let v = section_view(&root, &doc.id, "rotas", &SecretSet::default()).expect("seção");
    let text = v["text"].as_str().expect("texto");
    assert!(text.starts_with("## Rotas") && text.contains("### Erros") && !text.contains("## Grande"), "{text}");
    assert_eq!(v["truncated"], false);
    let v = section_view(&root, &doc.id, "grande", &SecretSet::default()).expect("seção grande");
    let text = v["text"].as_str().expect("texto");
    assert_eq!(v["truncated"], true);
    assert!(text.len() <= MAX_SECTION_BYTES && text.ends_with("seção"), "corte numa linha: {}", &text[text.len() - 20..]);
    assert_eq!(section_view(&root, &doc.id, "nao-existe", &SecretSet::default()), Err(KbError::NotFound));
}

#[test]
fn section_takes_the_neutral_anchor_and_comes_redacted() {
    let root = root("neutral");
    let secret = concat!("sk", "-segredo-do-titulo-123456");
    let doc = add(&root, "a.md", &format!("# Chave {secret}\n\nUse a chave {secret} aqui.\n"));
    let secrets = SecretSet::new(vec![secret.to_string()]);
    let v = section_view(&root, &doc.id, "s.0", &secrets).expect("âncora neutra");
    assert_eq!(v["anchor"], "s.0");
    assert!(!v.to_string().contains(secret), "{v}");
}

// --- busca ---

#[test]
fn search_limits_and_query_ceiling() {
    assert_eq!(search_limit(None), DEFAULT_SEARCH_LIMIT);
    assert_eq!(search_limit(Some("0")), 1);
    assert_eq!(search_limit(Some("9999")), MAX_SEARCH_LIMIT);
    assert_eq!(search_limit(Some("abc")), DEFAULT_SEARCH_LIMIT);
    assert!(!query_too_long(&"a".repeat(MAX_QUERY_CHARS)));
    assert!(query_too_long(&"ç".repeat(MAX_QUERY_CHARS + 1)));

    let root = root("limit");
    for i in 0..30 {
        add(&root, &format!("d{i}.md"), &format!("# Documento {i}\n\nfila de mensagens número {i}\n"));
    }
    let ix = index::index_for(&root).expect("índice");
    let v = search_view(&ix, "fila", 9999, &SecretSet::default());
    assert_eq!(v["results"].as_array().map(Vec::len), Some(MAX_SEARCH_LIMIT));
    assert_eq!(search_view(&ix, "  \"\" ", 5, &SecretSet::default()), json!({ "results": [] }));
}

/// Os destaques são posições de caractere do trecho mostrado, com a dobra de acentos do índice.
#[test]
fn highlights_fold_accents_and_count_characters() {
    let terms = index::parse_query("acao").0;
    assert_eq!(index::highlights("A ação e a AÇÃO.", &terms), vec![(2, 6), (11, 15)]);
    // Texto decomposto (a + acento combinante): o termo inclui a marca.
    assert_eq!(index::highlights("ac\u{327}a\u{303}o!", &terms), vec![(0, 6)]);
    assert!(index::highlights("nada aqui", &terms).is_empty());
}

#[test]
fn search_highlights_survive_a_secret_redacted_in_the_middle() {
    let root = root("secret");
    let secret = concat!("sk", "-segredo-no-meio-0123456789");
    let doc = add(&root, "a.md", &format!("# Guia\n\nA ação {secret} da ação final.\n"));
    let secrets = SecretSet::new(vec![secret.to_string()]);
    let ix = index::index_for(&root).expect("índice");
    let v = search_view(&ix, "acao", 5, &secrets);
    let hit = &v["results"][0];
    assert_eq!(hit["doc"], doc.id.as_str());
    assert_eq!(hit["anchor"], "guia");
    let snippet = hit["snippet"].as_str().expect("trecho");
    assert!(!snippet.contains(secret), "{snippet}");
    let chars: Vec<char> = snippet.chars().collect();
    let marks = hit["highlights"].as_array().expect("destaques");
    assert_eq!(marks.len(), 2, "{v}");
    for m in marks {
        let (a, b) = (m[0].as_u64().expect("início") as usize, m[1].as_u64().expect("fim") as usize);
        assert_eq!(chars[a..b].iter().collect::<String>(), "ação", "{snippet} {m}");
    }
}

// --- links ---

#[test]
fn links_lists_cover_outside_targets_removed_docs_and_the_line_around_the_link() {
    let root = root("links");
    let banco = add(&root, "banco.md", "# Banco\n\nPostgres.\n");
    let secret = concat!("sk", "-segredo-da-linha-0123456789");
    let guia = add(
        &root,
        "guia.md",
        "# Guia\n\nIntro.\n\n## Dados\n\nVeja o [banco](banco.md) e o [site](https://exemplo.com/fora) e [velho](velho.md).\n",
    );
    let docs = kb::list(&root).expect("lista");
    let guia = docs.iter().find(|d| d.id == guia.id).cloned().expect("guia");
    let ix = index::index_for(&root).expect("índice");
    let (links_to, _) = links_view(&guia, &docs, Some(&ix), &SecretSet::default());
    assert_eq!(links_to["docs"], json!([{ "id": banco.id, "title": "banco", "status": "ready" }]));
    assert_eq!(
        links_to["outside"],
        json!([{ "kind": "url", "target": "https://exemplo.com/fora" }, { "kind": "file", "target": "velho.md" }])
    );

    let banco_entry = docs.iter().find(|d| d.id == banco.id).cloned().expect("banco");
    // A linha do link é redigida: o segredo configurado depois do envio não aparece.
    let secrets = SecretSet::new(vec!["exemplo.com/fora".to_string(), secret.to_string()]);
    let (_, cited_by) = links_view(&banco_entry, &docs, Some(&ix), &secrets);
    let item = &cited_by[0];
    assert_eq!(item["id"], guia.id.as_str());
    let context = item["context"].as_str().expect("linha");
    assert!(context.starts_with("Veja o [banco](banco.md)") && !context.contains("exemplo.com/fora"), "{context}");

    // Documento removido: sai de "Links para" (o alvo passa a ficar fora da base) e de "Citado por".
    remove(&root, &banco.id).expect("remove");
    let docs = kb::list(&root).expect("lista");
    let guia = docs.iter().find(|d| d.id == guia.id).cloned().expect("guia");
    let (links_to, _) = links_view(&guia, &docs, None, &SecretSet::default());
    assert_eq!(links_to["docs"], json!([]));
    assert!(links_to["outside"].as_array().expect("fora").contains(&json!({ "kind": "file", "target": "banco.md" })));
}

#[test]
fn outside_links_keep_only_valid_http_pages() {
    let root = root("links-esquema");
    let guia = add(&root, "guia.md", "# Guia\n\nIntro.\n");
    let docs = kb::list(&root).expect("lista");
    let mut guia = docs.iter().find(|d| d.id == guia.id).cloned().expect("guia");
    // Um manifesto gravado à mão (ou antigo) com alvos que não são página: nenhum vira link.
    guia.link_targets = [
        "url:javascript:alert(1)",
        "url:file:///etc/passwd",
        "url:data:text/html,<script>alert(1)</script>",
        "url:https://usuario:senha@exemplo.com/x",
        "url:https://exemplo.com/ok#parte",
        "file:velho.md",
    ]
    .iter()
    .map(|t| t.to_string())
    .collect();
    let (links_to, _) = links_view(&guia, &docs, None, &SecretSet::default());
    assert_eq!(
        links_to["outside"],
        json!([{ "kind": "url", "target": "https://exemplo.com/ok" }, { "kind": "file", "target": "velho.md" }])
    );
}

#[test]
fn the_line_cap_applies_after_redaction() {
    let secret = concat!("sk", "-segredo-longo-0123456789");
    let secrets = SecretSet::new(vec![secret.to_string()]);
    // Cortada antes da redação, a linha deixaria escapar o começo do segredo.
    let line = format!("{} {secret} {}", "x".repeat(MAX_CONTEXT_CHARS - 5), "y".repeat(20));
    let out = one_line_capped(&secrets, &line, MAX_CONTEXT_CHARS);
    assert!(!out.contains(&secret[..4]), "{out}");
    assert!(out.ends_with("...") && out.chars().count() == MAX_CONTEXT_CHARS + 3, "{out}");
}

#[test]
fn used_in_plans_reads_drafts_and_frozen_versions_with_the_place() {
    let root = root("plans");
    let doc = add(&root, "regras.md", "# Regras\n\n## Filas\n\nUse filas.\n\n## Logs\n\nJSON.\n");
    let id = doc.id.clone();
    write_plan(
        &root,
        "001-api",
        &format!("## Premissas\n- Filas (kb:{id}#filas)\n\n## Abordagem técnica\nComo em kb:{id}#logs.\n"),
        &[("T01", &format!("## Objetivo\nVer kb:{id}.\n\n## Critérios de Aceitação\n1. Logs (kb:{id}#logs)\n"))],
    );
    write_plan(&root, "002-outra", "## Contexto\nSem base.\n", &[]);
    // Versão congelada de uma spec cuja pasta não existe mais.
    let frozen = cite::Frozen {
        version: cite::FROZEN_VERSION,
        spec: "000-antiga".into(),
        frozen_at: NOW.into(),
        sources: vec![cite::FrozenSource { citation: format!("kb:{id}#filas"), sha256: String::new() }],
        catalog: String::new(),
        overview: String::new(),
    };
    fs::create_dir_all(root.join(cite::FROZEN_REL)).expect("frozen");
    fs::write(root.join(cite::frozen_rel("000-antiga")), serde_json::to_string(&frozen).expect("json")).expect("w");

    let used = used_in_plans(&root, &id, &SecretSet::default());
    assert_eq!(used.len(), 2, "{used:?}");
    let api = used.iter().find(|u| u["spec"] == "001-api").expect("001");
    assert_eq!(api["title"], "Plano 001-api");
    assert_eq!(api["frozen"], false);
    let uses = api["uses"].as_array().expect("usos");
    let find = |anchor: Value, place: &str| uses.iter().find(|u| u["anchor"] == anchor && u["in"] == place).cloned();
    let premissas = find(json!("filas"), "spec").expect("premissas");
    assert_eq!((premissas["section"].clone(), premissas["binding"].clone()), (json!("Premissas"), json!(true)));
    assert_eq!(premissas["structural"], true);
    let descritiva = find(json!("logs"), "spec").expect("abordagem");
    assert_eq!((descritiva["section"].clone(), descritiva["binding"].clone()), (json!("Abordagem técnica"), json!(false)));
    assert_eq!(descritiva["structural"], false);
    let inteira = find(Value::Null, "task").expect("documento inteiro na tarefa");
    assert_eq!((inteira["task"].clone(), inteira["section"].clone(), inteira["binding"].clone()), (json!("T01"), json!("Objetivo"), json!(false)));
    let criterio = find(json!("logs"), "task").expect("critério");
    assert_eq!((criterio["section"].clone(), criterio["binding"].clone()), (json!("Critérios de Aceitação"), json!(true)));
    let antiga = used.iter().find(|u| u["spec"] == "000-antiga").expect("congelada");
    assert_eq!(antiga["frozen"], true);
    assert_eq!(antiga["uses"], json!([{ "in": "frozen", "section": null, "structural": false, "binding": false, "anchor": "filas" }]));
    assert!(used_in_plans(&root, "outro-0123abcd", &SecretSet::default()).is_empty());
}

// --- fontes do plano ---

#[test]
fn plan_sources_do_not_repeat_and_mark_binding_and_removed() {
    let root = root("sources");
    let regras = add(&root, "regras.md", "# Regras\n\n## Filas\n\nUse filas.\n");
    let livro = add(&root, "livro.md", "# Livro\n\n## Controller\n\nExemplo.\n");
    let (r, l) = (regras.id.clone(), livro.id.clone());
    write_plan(
        &root,
        "001-api",
        &format!(
            "Visão: kb:{r}#filas\n\n## Premissas\n- Filas (kb:{r}#filas)\n\n## Abordagem técnica\nComo no livro (kb:{l}#controller). \
De novo kb:{l}#controller. Sumida: kb:{l}#sumida. Fora: kb:apagado-0123abcd.\n\n## Orientações por papel\n### Revisor\n- kb:{l}#controller\n"
        ),
        &[("T01", &format!("## Critérios de Aceitação\n1. Filas (kb:{r}#filas)\n")), ("T02", &format!("## Objetivo\nkb:{l}\n"))],
    );
    let v = plan_sources(&root, "001-api", &SecretSet::default()).expect("ok").expect("spec");
    let sources = v["sources"].as_array().expect("fontes");
    let citations: Vec<&str> = sources.iter().map(|s| s["citation"].as_str().expect("citação")).collect();
    assert_eq!(
        citations,
        [
            format!("kb:{r}#filas"),
            format!("kb:{l}#controller"),
            format!("kb:{l}#sumida"),
            "kb:apagado-0123abcd".to_string(),
            format!("kb:{l}"),
        ],
        "sem repetir, na ordem: {v}"
    );
    let filas = &sources[0];
    assert_eq!((filas["doc_title"].clone(), filas["section_title"].clone()), (json!("regras"), json!("Filas")));
    assert_eq!((filas["binding"].clone(), filas["removed"].clone()), (json!(true), json!(false)));
    let places: Vec<(Value, Value, Value)> =
        filas["uses"].as_array().expect("usos").iter().map(|u| (u["in"].clone(), u["section"].clone(), u["binding"].clone())).collect();
    assert_eq!(
        places,
        [
            (json!("spec"), Value::Null, json!(false)),
            (json!("spec"), json!("Premissas"), json!(true)),
            (json!("task"), json!("Critérios de Aceitação"), json!(true)),
        ]
    );
    // Na Abordagem técnica (duas vezes, um uso só) e nas Orientações: a fonte obriga.
    let controller = &sources[1];
    assert_eq!(controller["binding"], true);
    assert_eq!(controller["uses"].as_array().map(Vec::len), Some(2), "{controller}");
    assert_eq!(controller["uses"][0]["binding"], false);
    assert_eq!(controller["uses"][1]["section"], "Orientações por papel");
    // Seção que não existe mais e documento fora da base.
    assert_eq!((sources[2]["removed"].clone(), sources[2]["section_title"].clone()), (json!(true), Value::Null));
    assert_eq!((sources[3]["removed"].clone(), sources[3]["doc_title"].clone()), (json!(true), Value::Null));
    assert_eq!((sources[4]["removed"].clone(), sources[4]["anchor"].clone(), sources[4]["binding"].clone()), (json!(false), Value::Null, json!(false)));
    assert_eq!(sources[4]["uses"][0]["task"], "T02");

    // A régua do portão (`Index::section`): o título sem caixa nem acento resolve; a seção que
    // não existe de verdade fica marcada.
    write_plan(&root, "002-titulo", &format!("## Premissas\n- kb:{r}#Filas\n- kb:{l}#Controle_velho\n"), &[]);
    let v = plan_sources(&root, "002-titulo", &SecretSet::default()).expect("ok").expect("spec");
    let titled = &v["sources"][0];
    assert_eq!(titled["citation"], format!("kb:{r}#Filas"));
    assert_eq!((titled["removed"].clone(), titled["section_title"].clone()), (json!(false), json!("Filas")), "{v}");
    assert_eq!(titled["resolved_anchor"], "filas");
    let missing = &v["sources"][1];
    assert_eq!((missing["removed"].clone(), missing["section_title"].clone()), (json!(true), Value::Null), "{v}");
    assert!(cite::check(&root, &format!("kb:{r}#Filas"), &SecretSet::default()).is_ok(), "o portão aceita a mesma forma");

    // O documento sai da base: as fontes dele ficam marcadas.
    remove(&root, &l).expect("remove");
    let v = plan_sources(&root, "001-api", &SecretSet::default()).expect("ok").expect("spec");
    assert!(v["sources"].as_array().expect("fontes").iter().filter(|s| s["doc"] == l.as_str()).all(|s| s["removed"] == true), "{v}");
    assert_eq!(plan_sources(&root, "nao-existe", &SecretSet::default()), Ok(None));
}

#[test]
fn plan_sources_with_an_empty_base() {
    let root = root("empty");
    write_plan(&root, "001-a", "## Contexto\nSem fontes.\n", &[]);
    assert_eq!(plan_sources(&root, "001-a", &SecretSet::default()), Ok(Some(json!({ "sources": [] }))));
    write_plan(&root, "002-b", "## Premissas\n- kb:livro-0123abcd#x\n", &[]);
    let v = plan_sources(&root, "002-b", &SecretSet::default()).expect("ok").expect("spec");
    assert_eq!(v["sources"][0]["removed"], true, "{v}");
    assert_eq!(v["sources"][0]["binding"], true, "{v}");
    assert!(!root.join(crate::kb::KB_REL).exists(), "ler não cria a base");
}

// --- medição (critério 5) ---

/// Texto de ~16 KiB com 16 seções.
fn doc_text(i: usize) -> String {
    let mut out = format!("# Documento {i}\n\n");
    for s in 0..16 {
        out.push_str(&format!("## Seção {s} do documento {i}\n\n"));
        for p in 0..10 {
            out.push_str(&format!("A ação número {p} trata de filas, rotas e validação de dados no serviço {i}-{s}.\n"));
        }
        out.push('\n');
    }
    out
}

/// Spec 136 T03 (critério 5): a busca durante um envio em lote de 50 documentos. Cada documento
/// novo muda o manifesto, e a próxima busca remonta o índice inteiro. Mede a busca que remonta
/// (logo depois de cada envio), a busca com o índice pronto e a busca concorrente com o lote.
///
/// Números de 2026-10-29 (perfil de teste, sem otimização) e a decisão: no comentário de
/// [`super`]; a saída `kb_search_batch` aparece com `--nocapture`.
#[test]
fn search_during_a_batch_upload_is_measured() {
    let root = root("measure");
    let secrets = SecretSet::default();
    let mut rebuilds: Vec<Duration> = Vec::new();
    let mut upload: Vec<Duration> = Vec::new();
    for i in 0..50 {
        let t = Instant::now();
        add(&root, &format!("doc{i}.md"), &doc_text(i));
        upload.push(t.elapsed());
        let t = Instant::now();
        let ix = index::index_for(&root).expect("índice");
        let v = search_view(&ix, "acao filas", 10, &secrets);
        rebuilds.push(t.elapsed());
        assert!(!v["results"].as_array().expect("resultados").is_empty());
    }
    let mut cached: Vec<Duration> = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        let ix = index::index_for(&root).expect("índice");
        search_view(&ix, "acao filas", 10, &secrets);
        cached.push(t.elapsed());
    }

    // Concorrente: uma busca seguida, num fio, enquanto outros 50 documentos entram.
    let stop = Arc::new(AtomicBool::new(false));
    let searcher = {
        let (root, stop) = (root.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut times: Vec<Duration> = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                let t = Instant::now();
                let ix = index::index_for(&root).expect("índice");
                search_view(&ix, "acao filas", 10, &SecretSet::default());
                times.push(t.elapsed());
            }
            times
        })
    };
    let mut upload_concurrent: Vec<Duration> = Vec::new();
    for i in 50..100 {
        let t = Instant::now();
        add(&root, &format!("doc{i}.md"), &doc_text(i));
        upload_concurrent.push(t.elapsed());
    }
    stop.store(true, Ordering::SeqCst);
    let concurrent = searcher.join().expect("fio da busca");

    let stats = |d: &[Duration]| {
        let mut ms: Vec<f64> = d.iter().map(|x| x.as_secs_f64() * 1000.0).collect();
        ms.sort_by(f64::total_cmp);
        let avg = ms.iter().sum::<f64>() / ms.len().max(1) as f64;
        format!("n={} média={avg:.1}ms p50={:.1}ms máx={:.1}ms", ms.len(), ms[ms.len() / 2], ms[ms.len() - 1])
    };
    let index_stats = index::index_for(&root).expect("índice").stats();
    eprintln!("kb_search_batch: busca que remonta (1 a 50 docs): {}", stats(&rebuilds));
    eprintln!("kb_search_batch: busca com o índice pronto (50 docs): {}", stats(&cached));
    eprintln!("kb_search_batch: envio sozinho (1 a 50 docs): {}", stats(&upload));
    eprintln!("kb_search_batch: envio com busca concorrente (51 a 100 docs): {}", stats(&upload_concurrent));
    eprintln!("kb_search_batch: busca concorrente com o lote: {}", stats(&concurrent));
    eprintln!("kb_search_batch: índice final {index_stats:?}");
    eprintln!("kb_search_batch: série {:?}", rebuilds.iter().map(|d| d.as_millis()).collect::<Vec<_>>());
    // Trava folgada (perfil de teste): a busca nunca trava a interface por segundos.
    assert!(rebuilds.iter().chain(&concurrent).all(|d| *d < Duration::from_secs(5)), "{}", stats(&rebuilds));
    assert!(!concurrent.is_empty());
}
