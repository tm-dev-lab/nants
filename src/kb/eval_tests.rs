//! Medição de recuperação da base (spec 134 T07): perguntas-gabarito contra bases reais, fora da
//! suíte padrão (`#[ignore]`).
//!
//! As bases ficam fora do repositório (materiais de terceiros): `NANTS_KB_EVAL_DIR` aponta para
//! uma pasta com `<base>/.sdd/kb` montada pela API de verdade (`node scripts/e2e-kb.mjs bases`).
//! As perguntas e as descrições escritas à mão ficam em `tests/fixtures/kb-eval/perguntas.json`.
//!
//! Uso:
//!
//! ```sh
//! NANTS_KB_EVAL_DIR=<pasta> scripts/test.sh cargo test --lib kb::eval_tests -- --ignored --nocapture --test-threads=1
//! ```
//!
//! - `kb_eval_sections`: lista as seções de cada documento (para escrever o gabarito);
//! - `kb_eval_recall`: hit@1, hit@5 e MRR da busca (a mesma de `kb_search`), por base e na base
//!   com todos os documentos juntos; a escolha do documento (doc@1) com três descrições (vazia,
//!   a extraída na entrada e a escrita à mão, C-19); e a escolha do documento só pelo catálogo
//!   (título contra título e descrição), que é o que o modelo vê no prefixo.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Deserialize;

use super::index::{Hit, Index};
use super::{list, read_note, Entry, Status};
use crate::server::events::SecretSet;

#[derive(Debug, Deserialize)]
struct Question {
    q: String,
    /// Prefixo do id do documento certo (o sufixo é um hash).
    doc: String,
    /// Âncoras aceitas (qualquer uma vale).
    sections: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BaseSpec {
    /// Pasta da base em `NANTS_KB_EVAL_DIR` (padrão: o nome da base). A base com outra pasta fica
    /// fora do total "todas".
    #[serde(default)]
    dir: Option<String>,
    /// Descrição de uma linha escrita à mão, por prefixo de id.
    #[serde(default)]
    descriptions: HashMap<String, String>,
    questions: Vec<Question>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    bases: std::collections::BTreeMap<String, BaseSpec>,
}

fn eval_dir() -> Option<PathBuf> {
    std::env::var_os("NANTS_KB_EVAL_DIR").map(PathBuf::from)
}

fn fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kb-eval/perguntas.json");
    let text = std::fs::read_to_string(path).expect("perguntas.json");
    serde_json::from_str(&text).expect("perguntas.json válido")
}

/// Entradas prontas e corpos das notas de uma base.
fn load(root: &Path) -> Vec<(Entry, String)> {
    let none = SecretSet::default();
    list(root)
        .expect("manifesto")
        .into_iter()
        .filter(|e| e.status == Status::Ready)
        .map(|e| {
            let (entry, body) = read_note(root, &e.id, &none).expect("nota");
            (entry, body)
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Desc {
    Empty,
    Extracted,
    Written,
}

impl Desc {
    fn label(self) -> &'static str {
        match self {
            Self::Empty => "vazia",
            Self::Extracted => "extraída",
            Self::Written => "escrita",
        }
    }
}

fn with_descriptions(docs: &[(Entry, String)], desc: Desc, written: &HashMap<String, String>) -> Vec<(Entry, String)> {
    docs.iter()
        .map(|(e, body)| {
            let mut e = e.clone();
            match desc {
                Desc::Empty => e.description.clear(),
                Desc::Extracted => {}
                Desc::Written => {
                    if let Some((_, d)) = written.iter().find(|(prefix, _)| e.id.starts_with(prefix.as_str())) {
                        e.description = d.clone();
                    }
                }
            }
            (e, body.clone())
        })
        .collect()
}

#[derive(Default, Debug, Clone, Copy)]
struct Score {
    n: usize,
    hit1: usize,
    hit5: usize,
    rr: f64,
    doc1: usize,
    doc_rr: f64,
}

impl Score {
    fn pct(a: usize, n: usize) -> String {
        if n == 0 {
            return "-".into();
        }
        format!("{:.0}%", 100.0 * a as f64 / n as f64)
    }

    fn row(&self) -> String {
        let n = self.n.max(1) as f64;
        format!(
            "{:>3} | {:>5} | {:>5} | {:.2} | {:>5} | {:.2}",
            self.n,
            Self::pct(self.hit1, self.n),
            Self::pct(self.hit5, self.n),
            self.rr / n,
            Self::pct(self.doc1, self.n),
            self.doc_rr / n
        )
    }
}

fn is_hit(h: &Hit, q: &Question) -> bool {
    h.doc_id.starts_with(&q.doc) && q.sections.iter().any(|s| *s == h.section)
}

fn score(index: &Index, questions: &[&Question], verbose: bool) -> Score {
    let none = SecretSet::default();
    let mut s = Score::default();
    for q in questions {
        let hits = index.search(&q.q, 10, &none);
        s.n += 1;
        let pos = hits.iter().position(|h| is_hit(h, q));
        if pos == Some(0) {
            s.hit1 += 1;
        }
        if pos.is_some_and(|p| p < 5) {
            s.hit5 += 1;
        }
        if let Some(p) = pos {
            s.rr += 1.0 / (p as f64 + 1.0);
        }
        // Posição do primeiro documento distinto certo, na ordem dos documentos dos resultados.
        let mut docs: Vec<&str> = Vec::new();
        for h in &hits {
            if !docs.contains(&h.doc_id.as_str()) {
                docs.push(&h.doc_id);
            }
        }
        let dpos = docs.iter().position(|d| d.starts_with(&q.doc));
        if dpos == Some(0) {
            s.doc1 += 1;
        }
        if let Some(p) = dpos {
            s.doc_rr += 1.0 / (p as f64 + 1.0);
        }
        if verbose && pos != Some(0) {
            let top: Vec<String> = hits.iter().take(3).map(|h| format!("{}#{}", short(&h.doc_id), h.section)).collect();
            println!("    erro@1 [{}] esperado {}#{:?}; pos {:?}; top3 {:?}", q.q, q.doc, q.sections, pos.map(|p| p + 1), top);
        }
    }
    s
}

fn short(id: &str) -> &str {
    id.rsplit_once('-').map_or(id, |(a, _)| a)
}

/// Escolha do documento só pelo catálogo (título, com ou sem a descrição): o que o modelo vê no
/// prefixo antes de buscar. Mede doc@1 de um BM25 sobre as linhas do catálogo.
fn catalog_choice(docs: &[(Entry, String)], questions: &[&Question], with_description: bool) -> (usize, usize) {
    let none = SecretSet::default();
    let lines: Vec<(Entry, String)> = docs
        .iter()
        .map(|(e, _)| {
            let mut e = e.clone();
            let body = if with_description { format!("{}\n\n{}\n", e.title, e.description) } else { format!("{}\n", e.title) };
            e.description.clear();
            (e, body)
        })
        .collect();
    let index = Index::build(lines);
    let mut ok = 0;
    for q in questions {
        let hits = index.search(&q.q, 1, &none);
        if hits.first().is_some_and(|h| h.doc_id.starts_with(&q.doc)) {
            ok += 1;
        }
    }
    (ok, questions.len())
}

#[test]
#[ignore = "medição com bases reais: NANTS_KB_EVAL_DIR (spec 134 T07)"]
fn kb_eval_sections() {
    let Some(dir) = eval_dir() else {
        println!("NANTS_KB_EVAL_DIR ausente");
        return;
    };
    let only = std::env::var("NANTS_KB_EVAL_DOC").unwrap_or_default();
    let none = SecretSet::default();
    let root = dir.join("all");
    for e in list(&root).expect("manifesto") {
        if !only.is_empty() && !e.id.starts_with(&only) {
            continue;
        }
        println!("== {} ({}) desc: {}", e.id, e.title, e.description);
        for s in super::index::list_sections(&root, &e.id, &none).unwrap_or_default() {
            println!("  {:<48} {}", s.anchor, s.trail);
        }
    }
}

#[test]
#[ignore = "medição com bases reais: NANTS_KB_EVAL_DIR (spec 134 T07)"]
fn kb_eval_recall() {
    let Some(dir) = eval_dir() else {
        println!("NANTS_KB_EVAL_DIR ausente");
        return;
    };
    let verbose = std::env::var_os("NANTS_KB_EVAL_VERBOSE").is_some();
    let fx = fixture();
    let all_docs = load(&dir.join("all"));
    let all_written: HashMap<String, String> = fx.bases.values().flat_map(|b| b.descriptions.clone()).collect();
    let mut total = 0usize;
    println!("\nbusca (kb_search): base | descrição | n | hit@1 | hit@5 | MRR | doc@1 | MRR doc");
    let mut rows: Vec<(String, Desc, Score)> = Vec::new();
    for (name, base) in &fx.bases {
        let root = dir.join(base.dir.as_deref().unwrap_or(name));
        let docs = load(&root);
        assert!(!docs.is_empty(), "base {name} vazia");
        let qs: Vec<&Question> = base.questions.iter().collect();
        total += qs.len();
        let written = if base.dir.is_some() { &all_written } else { &base.descriptions };
        for desc in [Desc::Empty, Desc::Extracted, Desc::Written] {
            let t = Instant::now();
            let index = Index::build(with_descriptions(&docs, desc, written));
            let build_ms = t.elapsed().as_secs_f64() * 1000.0;
            if verbose {
                println!("  [{name} / {}] montagem {build_ms:.1} ms", desc.label());
            }
            let s = score(&index, &qs, verbose && desc == Desc::Extracted);
            println!("{name:<5} | {:<8} | {}", desc.label(), s.row());
            rows.push((name.clone(), desc, s));
        }
    }
    // Todas as bases juntas: os outros domínios viram distratores.
    let qs_all: Vec<&Question> = fx.bases.values().filter(|b| b.dir.is_none()).flat_map(|b| b.questions.iter()).collect();
    for desc in [Desc::Empty, Desc::Extracted, Desc::Written] {
        let index = Index::build(with_descriptions(&all_docs, desc, &all_written));
        let s = score(&index, &qs_all, false);
        println!("{:<5} | {:<8} | {}", "todas", desc.label(), s.row());
    }
    // Montagem e consulta na base com tudo.
    let t = Instant::now();
    let index = Index::build(all_docs.clone());
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    let stats = index.stats();
    let none = SecretSet::default();
    let t = Instant::now();
    for q in &qs_all {
        let _ = index.search(&q.q, 5, &none);
    }
    let per_query_us = t.elapsed().as_secs_f64() * 1e6 / qs_all.len().max(1) as f64;
    println!(
        "\níndice (todas): {} docs, {} seções, {} trechos, {} termos, ~{} KiB; montagem {build_ms:.1} ms; consulta {per_query_us:.0} us",
        stats.docs,
        stats.sections,
        stats.chunks,
        stats.terms,
        stats.approx_bytes / 1024
    );

    // Escolha do documento só pelo catálogo (C-19).
    println!("\ncatálogo (doc@1 só pela linha do catálogo): base | só título | título + extraída | título + escrita");
    for (name, base) in &fx.bases {
        let docs = load(&dir.join(base.dir.as_deref().unwrap_or(name)));
        let qs: Vec<&Question> = base.questions.iter().collect();
        let written = if base.dir.is_some() { &all_written } else { &base.descriptions };
        let (t_ok, n) = catalog_choice(&docs, &qs, false);
        let (e_ok, _) = catalog_choice(&docs, &qs, true);
        let (w_ok, _) = catalog_choice(&with_descriptions(&docs, Desc::Written, written), &qs, true);
        println!("{name:<5} | {} | {} | {}", Score::pct(t_ok, n), Score::pct(e_ok, n), Score::pct(w_ok, n));
    }
    {
        let (t_ok, n) = catalog_choice(&all_docs, &qs_all, false);
        let (e_ok, _) = catalog_choice(&all_docs, &qs_all, true);
        let (w_ok, _) = catalog_choice(&with_descriptions(&all_docs, Desc::Written, &all_written), &qs_all, true);
        println!("{:<5} | {} | {} | {}", "todas", Score::pct(t_ok, n), Score::pct(e_ok, n), Score::pct(w_ok, n));
    }
    assert!(total >= 10, "poucas perguntas: {total}");
    assert!(rows.iter().all(|(_, _, s)| s.n > 0));
}
