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
//!   pela linha real do catálogo, com e sem o sumário do nível útil e com a descrição escrita
//!   (spec 136 T08), que é o que o modelo vê no prefixo; e os bytes do catálogo em bases
//!   simuladas de 10, 25 e 50 documentos.

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

/// Linha do catálogo usada na escolha do documento (spec 136 T08, C-08).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Line {
    /// Só o título (a linha de base da 134/T07, mantida como histórico).
    Title,
    /// A linha real do catálogo (`catalog_line` sem o sumário), com a descrição extraída.
    Real,
    /// A linha real com o sumário do nível útil.
    Summary,
    /// A linha real com a descrição escrita à mão.
    Written,
    /// A linha real com o sumário e a descrição escrita.
    SummaryWritten,
}

const LINES: [Line; 5] = [Line::Title, Line::Real, Line::Summary, Line::Written, Line::SummaryWritten];

/// Escolha do documento só pelo catálogo: o que o modelo vê no prefixo antes de buscar. Mede
/// doc@1 de um BM25 sobre as linhas do catálogo (uma linha por documento) e devolve os acertos.
fn catalog_choice(root: &Path, docs: &[(Entry, String)], questions: &[&Question], line: Line, written: &HashMap<String, String>) -> usize {
    let none = SecretSet::default();
    let docs = match line {
        Line::Written | Line::SummaryWritten => with_descriptions(docs, Desc::Written, written),
        _ => docs.to_vec(),
    };
    let lines: Vec<(Entry, String)> = docs
        .iter()
        .map(|(e, _)| {
            let body = match line {
                Line::Title => format!("{}\n", e.title),
                Line::Real | Line::Written => format!("{}\n", super::catalog::catalog_line(root, e, &none, false)),
                Line::Summary | Line::SummaryWritten => format!("{}\n", super::catalog::catalog_line(root, e, &none, true)),
            };
            let mut e = e.clone();
            e.description.clear();
            (e, body)
        })
        .collect();
    let index = Index::build(lines);
    questions
        .iter()
        .filter(|q| index.search(&q.q, 1, &none).first().is_some_and(|h| h.doc_id.starts_with(&q.doc)))
        .count()
}

/// Base simulada com `n` documentos, repetindo os de `pool` (a base com tudo) com outro id, para
/// medir os bytes do catálogo. Devolve a pasta (em `TMPDIR`, que o `scripts/test.sh` apaga).
fn simulated_base(pool_root: &Path, n: usize) -> PathBuf {
    let root = std::env::temp_dir().join(format!("nants-kbeval-sim-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".sdd/kb/notes")).expect("pasta");
    let text = std::fs::read_to_string(pool_root.join(super::MANIFEST_REL)).expect("manifesto");
    let manifest: serde_json::Value = serde_json::from_str(&text).expect("json");
    let pool: Vec<serde_json::Value> =
        manifest["docs"].as_array().expect("docs").iter().filter(|d| d["status"] == "ready").cloned().collect();
    let mut docs = Vec::new();
    for i in 0..n {
        let mut d = pool[i % pool.len()].clone();
        let old = d["id"].as_str().expect("id").to_string();
        let id = format!("{}-{:08x}", short(&old), i);
        std::fs::copy(pool_root.join(format!(".sdd/kb/notes/{old}.md")), root.join(format!(".sdd/kb/notes/{id}.md"))).expect("nota");
        d["id"] = serde_json::json!(id);
        docs.push(d);
    }
    let out = serde_json::json!({ "version": manifest["version"], "docs": docs });
    std::fs::write(root.join(super::MANIFEST_REL), serde_json::to_string_pretty(&out).expect("json")).expect("manifesto");
    root
}

/// Modo do catálogo: inteiro com o sumário, inteiro sem ele ou curto.
fn catalog_mode(text: &str) -> &'static str {
    if text.contains("Catálogo longo") {
        "curto"
    } else if text.contains("(seções: ") {
        "inteiro com sumário"
    } else {
        "inteiro sem sumário"
    }
}

/// Bytes e modo do catálogo em bases simuladas de 10, 25 e 50 documentos com os degraus `steps`,
/// e o maior número de documentos com o catálogo inteiro (com ou sem o sumário).
fn catalog_sizes(pool_root: &Path, steps: &[bool]) -> (usize, Vec<(usize, usize, &'static str)>) {
    let none = SecretSet::default();
    let mut samples = Vec::new();
    for n in [10usize, 25, 50] {
        let root = simulated_base(pool_root, n);
        let text = super::catalog::render_steps(&root, &none, steps);
        assert_eq!(super::catalog::render_steps(&root, &none, steps), text, "mesma base, mesmo catálogo");
        samples.push((n, text.len(), catalog_mode(&text)));
        let _ = std::fs::remove_dir_all(&root);
    }
    let mut last = 0;
    for n in 1..=60usize {
        let root = simulated_base(pool_root, n);
        let text = super::catalog::render_steps(&root, &none, steps);
        let _ = std::fs::remove_dir_all(&root);
        if catalog_mode(&text) == "curto" {
            break;
        }
        last = n;
    }
    (last, samples)
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

    // Escolha do documento só pelo catálogo (C-19; spec 136 T08, C-08).
    //
    // LIMIAR, escrito antes de rodar (spec 136 T08): o sumário do nível útil entra no catálogo se
    // a escolha do documento certo pela linha com o sumário (`Summary`) não piorar em nenhuma base
    // contra a linha real (`Real`), contando os acertos absolutos, e melhorar na soma das bases
    // (pelo menos um acerto a mais), e se a base simulada de 25 documentos não cair no modo curto
    // por causa dele. Senão, o catálogo fica como está e os números ficam registrados aqui.
    //
    // Resultado de 2026-10-29 (acertos/n; (a) linha real, (b) com o sumário, (c) com a escrita):
    // cruzado 0/5, 0/5, 3/5; cruzado-traduzido 2/5, 2/5, 1/5; html 9/15, 11/15, 10/15;
    // md 4/14, 5/14, 6/14; pdf 0/15, 0/15, 0/15 (soma 15/54 contra 18/54); todas 13/44, 15/44,
    // 13/44. Bytes: só com o sumário, a base de 25 documentos cairia no modo curto (inteiro até
    // 16); em degraus (`CATALOG_STEPS`), 10 documentos com o sumário, 25 inteiros sem ele e 50
    // no modo curto, como antes: adotado em degraus.
    println!("\ncatálogo (doc@1 só pela linha do catálogo, acertos/n): base | só título | (a) linha real | (b) real + sumário | (c) real + escrita | (b)+(c)");
    let mut not_worse = true;
    let (mut real_sum, mut summary_sum) = (0usize, 0usize);
    let mut table = |name: &str, root: &Path, docs: &[(Entry, String)], qs: &[&Question], written: &HashMap<String, String>, counts: bool| {
        let n = qs.len();
        let got: Vec<usize> = LINES.iter().map(|l| catalog_choice(root, docs, qs, *l, written)).collect();
        let cells: Vec<String> = got.iter().map(|ok| format!("{ok}/{n} ({})", Score::pct(*ok, n))).collect();
        println!("{name:<5} | {}", cells.join(" | "));
        let (real, summary) = (got[1], got[2]);
        not_worse &= summary >= real;
        if counts {
            real_sum += real;
            summary_sum += summary;
        }
    };
    for (name, base) in &fx.bases {
        let root = dir.join(base.dir.as_deref().unwrap_or(name));
        let docs = load(&root);
        let qs: Vec<&Question> = base.questions.iter().collect();
        let written = if base.dir.is_some() { &all_written } else { &base.descriptions };
        table(name, &root, &docs, &qs, written, true);
    }
    // A linha "todas" junta as perguntas de pdf, html e md na base com tudo: conta para o "não
    // piorar", fora da soma (as perguntas já estão nas linhas das bases).
    table("todas", &dir.join("all"), &all_docs, &qs_all, &all_written, false);
    // Bytes: com quantos documentos o catálogo cai no modo curto, antes (sem o sumário), só com
    // o sumário e em degraus (o catálogo de verdade).
    let all_root = dir.join("all");
    let (fit_before, before) = catalog_sizes(&all_root, &[false]);
    let (fit_summary, only) = catalog_sizes(&all_root, &[true]);
    let (fit_steps, steps) = catalog_sizes(&all_root, &super::catalog::CATALOG_STEPS);
    println!("\ncatálogo (bytes, teto {} B): n | sem sumário | só com sumário | em degraus", super::catalog::MAX_CATALOG_BYTES);
    for (((n, b, bm), (_, o, om)), (_, d, dm)) in before.iter().zip(only.iter()).zip(steps.iter()) {
        println!("{n:>3} | {b:>5} {bm} | {o:>5} {om} | {d:>5} {dm}");
    }
    println!("inteiro até: {fit_before} documentos sem o sumário; {fit_summary} só com o sumário; {fit_steps} em degraus");
    let medium_ok = fit_steps >= fit_before;
    let adopt = not_worse && summary_sum > real_sum && medium_ok;
    println!(
        "limiar: não piora em nenhuma base = {not_worse}; soma (a) {real_sum} contra (b) {summary_sum}; o sumário não leva base nenhuma ao modo curto = {medium_ok}; adotar em degraus = {adopt}"
    );
    assert!(total >= 10, "poucas perguntas: {total}");
    assert!(rows.iter().all(|(_, _, s)| s.n > 0));
}
