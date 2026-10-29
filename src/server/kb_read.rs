//! Leitura da base para a interface (spec 136 T03): a seção de um documento, a busca com os
//! destaques, os links de um documento e as fontes da base citadas num plano. As rotas ficam em
//! [`super::kb`]; aqui, a montagem das respostas, sem `async`, para rodar em `spawn_blocking`.
//!
//! - **Seção (C-09):** a seção pela âncora pública (a do índice ou a neutra `s.<n>`), com as
//!   subseções, redigida e com teto ([`MAX_SECTION_BYTES`], cortada numa linha e marcada).
//! - **Busca (C-10):** o `q` tem teto ([`MAX_QUERY_CHARS`]) e o `limit` também
//!   ([`MAX_SEARCH_LIMIT`]). Cada resultado traz o trecho já redigido e os destaques em posições de
//!   caractere desse trecho ([`index::highlights`], a mesma tokenização do índice, com a dobra de
//!   acentos). A interface pinta os intervalos como texto, sem HTML do servidor e sem tokenizador
//!   próprio.
//! - **Links ([`links_view`]):** "Links para" (os documentos da base, pelo `links_out` do
//!   manifesto, e os alvos que ficam fora da base) e "Citado por" (com a linha do link no
//!   documento que cita, redigida e com teto). Um mapa do alvo para os documentos é montado uma
//!   vez: o custo é linear no número de documentos e de links, nunca quadrático.
//! - **Uso nos planos ([`used_in_plans`]):** as specs que citam o documento, pelo rascunho atual
//!   (`spec.md` e tarefas) e pelos arquivos congelados na aprovação (`.sdd/kb/frozen/*.json`), com
//!   a âncora e o lugar de cada citação.
//! - **Fontes do plano ([`plan_sources`]):** as citações da spec e das tarefas, sem repetir, com
//!   o título resolvido, a seção citada, o lugar de cada uso, `binding` e `removed`. Fonte única:
//!   [`kb::cite::find`] (com o `is_doc_id`), pelo [`catalog::placed_citations`].
//!
//! Rota própria para as fontes do plano (`GET /specs/:spec_id/kb-sources`), e não um campo no
//! `GET /state`: o estado é lido a cada evento e cobre todas as specs; as fontes precisam do
//! manifesto e do índice da base, que só a janela do plano aberta pede.
//!
//! **Concorrência (critério 5, medida em `tests::search_during_a_batch_upload_is_measured`):** a
//! busca usa o índice em memória ([`index::index_for`]), remontado só quando o manifesto muda;
//! durante um envio em lote, a primeira busca depois de cada documento novo remonta o índice
//! inteiro. Números de 2026-10-29, 50 documentos de ~16 KiB (perfil de teste, sem otimização):
//! - a busca que remonta cresce em linha reta com a base, de 6 ms (1 documento) a 178 ms (50);
//! - com o índice pronto, a busca leva 2,6 ms;
//! - o envio leva 12 ms em média sozinho e 9,7 ms com uma busca seguida em paralelo: a busca não
//!   segura o envio (travas diferentes), e buscas simultâneas do mesmo projeto esperam uma única
//!   montagem;
//! - no perfil de release, a medição da 134/T03 dá 51 ms para montar 50 documentos de ~30 KB.
//!
//! Decisão: a busca da interface não pula a remontagem durante o envio. O custo é linear e
//! limitado (dezenas de milissegundos no release, com a busca da interface já espaçada pela
//! digitação), não atrasa o envio, e um índice velho esconderia justo o documento que a pessoa
//! acabou de enviar.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use super::events::SecretSet;
use super::projects::{id_view, read_spec, read_task, real_children, scrub_text, text_field};
use super::safe_fs;
use crate::kb::catalog::{self, PlacedCitation};
use crate::kb::cite::{self, Citation};
use crate::kb::index::{self, Index};
use crate::kb::{self, Entry, KbError, Status};

/// Teto do texto da seção devolvida, em bytes.
pub const MAX_SECTION_BYTES: usize = 64 * 1024;
/// Teto da consulta da busca, em caracteres.
pub const MAX_QUERY_CHARS: usize = 200;
/// Resultados da busca sem `limit`.
pub const DEFAULT_SEARCH_LIMIT: usize = 10;
/// Teto do `limit` da busca.
pub const MAX_SEARCH_LIMIT: usize = 20;
/// Teto da linha em volta do link no "Citado por", em caracteres.
pub const MAX_CONTEXT_CHARS: usize = 240;
/// Teto de itens de cada lista de links e do uso nos planos.
pub const MAX_LINK_ITEMS: usize = 100;

const SPECS_REL: &str = ".sdd/specs";

// --- seção ---

/// Texto cortado em até `max` bytes, numa linha quando der; `true` se cortou.
fn cut(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let end = text.floor_char_boundary(max);
    let end = text[..end].rfind('\n').filter(|nl| *nl > end / 2).unwrap_or(end);
    (text[..end].trim_end().to_string(), true)
}

/// `GET /kb/docs/:doc?section=<âncora>`: a seção com as subseções, redigida e com teto. Documento
/// fora do índice ou seção inexistente → [`KbError::NotFound`].
pub fn section_view(root: &Path, doc: &str, anchor: &str, secrets: &SecretSet) -> Result<Value, KbError> {
    let s = index::read_section(root, doc, anchor, secrets)?;
    let (text, truncated) = cut(&s.text, MAX_SECTION_BYTES);
    Ok(json!({
        "anchor": s.anchor,
        "title": s.title,
        "trail": s.trail,
        "page": s.page,
        "text": text,
        "truncated": truncated,
        "max_bytes": MAX_SECTION_BYTES,
    }))
}

// --- busca ---

/// `limit` da busca: sem valor ou inválido, [`DEFAULT_SEARCH_LIMIT`]; de 1 a [`MAX_SEARCH_LIMIT`].
pub fn search_limit(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(DEFAULT_SEARCH_LIMIT).clamp(1, MAX_SEARCH_LIMIT)
}

/// `true` se a consulta passa do teto.
pub fn query_too_long(q: &str) -> bool {
    q.chars().count() > MAX_QUERY_CHARS
}

/// Resultados da busca com os destaques. Consulta sem termo → lista vazia.
pub fn search_view(index: &Index, q: &str, limit: usize, secrets: &SecretSet) -> Value {
    let (terms, _) = index::parse_query(q);
    let results: Vec<Value> = index
        .search(q, limit.clamp(1, MAX_SEARCH_LIMIT), secrets)
        .into_iter()
        .map(|hit| {
            // O trecho já sai redigido do índice: os destaques valem para o texto mostrado.
            let marks: Vec<[usize; 2]> = index::highlights(&hit.snippet, &terms).into_iter().map(|(a, b)| [a, b]).collect();
            json!({
                "doc": hit.doc_id,
                "doc_title": hit.doc_title,
                "anchor": hit.section,
                "title": hit.title,
                "trail": hit.trail,
                "page": hit.page,
                "snippet": hit.snippet,
                "highlights": marks,
            })
        })
        .collect();
    json!({ "results": results })
}

// --- links ---

/// Texto de uma linha, sem espaços nas pontas, redigido antes do corte, até `max` caracteres.
fn one_line_capped(secrets: &SecretSet, line: &str, max: usize) -> String {
    let clean = scrub_text(secrets, line.trim());
    if clean.chars().count() <= max {
        return clean;
    }
    let mut out: String = clean.chars().take(max).collect();
    out.push_str("...");
    out
}

/// Alvos de link que apontam para cada documento (`url:<link>` e `file:<nome em minúsculas>`),
/// pela regra de [`kb::link_points_to`]. Uma passada pelo manifesto.
fn targets_map(docs: &[Entry]) -> HashMap<String, Vec<usize>> {
    let mut map: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, d) in docs.iter().enumerate() {
        if let Some(url) = &d.source_url {
            map.entry(format!("url:{url}")).or_default().push(i);
        }
        if let Some(file) = &d.file_name {
            map.entry(format!("file:{}", file.to_lowercase())).or_default().push(i);
        }
    }
    map
}

/// `true` se a linha leva ao documento `target`: um link de markdown para ele ou, nos textos sem
/// markdown (página lida, PDF), o endereço ou o nome do arquivo dele escrito na linha.
fn line_links_to(line: &str, target: &Entry) -> bool {
    if (line.contains("](") || line.contains("]:")) && kb::extract::markdown_links(line).iter().any(|t| kb::link_points_to(target, t)) {
        return true;
    }
    if let Some(url) = target.source_url.as_deref().filter(|u| !u.is_empty()) {
        if line.contains(url) {
            return true;
        }
    }
    target.file_name.as_deref().filter(|f| !f.is_empty()).is_some_and(|f| line.to_lowercase().contains(&f.to_lowercase()))
}

/// "Links para" e "Citado por" de `entry`, para o `GET /kb/docs/:doc`.
pub fn links_view(entry: &Entry, docs: &[Entry], index: Option<&Index>, secrets: &SecretSet) -> (Value, Value) {
    let by_id: HashMap<&str, &Entry> = docs.iter().map(|d| (d.id.as_str(), d)).collect();
    let doc_item = |d: &Entry| json!({ "id": d.id, "title": scrub_text(secrets, &d.title), "status": d.status.as_str() });
    let linked: Vec<Value> =
        entry.links_out.iter().filter_map(|id| by_id.get(id.as_str())).take(MAX_LINK_ITEMS).map(|d| doc_item(d)).collect();
    let map = targets_map(docs);
    let mut outside: Vec<Value> = Vec::new();
    for target in &entry.link_targets {
        if outside.len() >= MAX_LINK_ITEMS {
            break;
        }
        // Fora da base: o alvo não leva a nenhum documento; o link para si mesmo não conta.
        if map.contains_key(target) {
            continue;
        }
        let item = match (target.strip_prefix("url:"), target.strip_prefix("file:")) {
            // Só página http(s) válida vira link (a régua do envio de link, `kb::clean_url`): um
            // `file:`, `data:` ou `javascript:` gravado no manifesto não chega à interface.
            (Some(url), _) => match kb::clean_url(url) {
                Ok(clean) => json!({ "kind": "url", "target": scrub_text(secrets, &clean) }),
                Err(_) => continue,
            },
            (_, Some(name)) => json!({ "kind": "file", "target": scrub_text(secrets, name) }),
            _ => continue,
        };
        if !outside.contains(&item) {
            outside.push(item);
        }
    }
    let cited_by: Vec<Value> = docs
        .iter()
        .filter(|d| d.id != entry.id && d.links_out.contains(&entry.id))
        .take(MAX_LINK_ITEMS)
        .map(|d| {
            let context = index
                .and_then(|ix| ix.find_line(&d.id, |line| line_links_to(line, entry)))
                .map(|line| one_line_capped(secrets, &line, MAX_CONTEXT_CHARS))
                .filter(|l| !l.is_empty());
            let mut item = doc_item(d);
            item["context"] = json!(context);
            item
        })
        .collect();
    (json!({ "docs": linked, "outside": outside }), Value::Array(cited_by))
}

// --- lugares das citações nos planos ---

/// Onde fica um texto do plano.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    Spec,
    Task(String),
}

/// Um uso de uma citação: o texto e a seção.
fn use_view(origin: &Origin, placed: &PlacedCitation, secrets: &SecretSet) -> Value {
    let section = placed.section.as_deref();
    let mut item = json!({
        "in": match origin { Origin::Spec => "spec", Origin::Task(_) => "task" },
        "section": section.map(|s| scrub_text(secrets, s)),
        // Título estrutural: o mesmo em qualquer idioma, a interface o traduz.
        "structural": section.is_some_and(crate::sdd::section_titles::is_structural),
        "binding": placed.binding,
    });
    if let Origin::Task(id) = origin {
        item["task"] = json!(id_view(secrets, id));
    }
    item
}

/// Textos do plano de uma spec (`name` = pasta em `.sdd/specs`): o título da spec e, na ordem, o
/// corpo da spec e o das tarefas. `None` sem `spec.md` legível.
fn plan_texts(root: &Path, name: &str, secrets: &SecretSet) -> Option<(String, Vec<(Origin, String)>)> {
    let spec_rel = format!("{SPECS_REL}/{name}");
    let spec = read_spec(root, &format!("{spec_rel}/spec.md")).ok()?;
    let title = text_field(secrets, "title", &spec.frontmatter.title);
    let mut texts = vec![(Origin::Spec, spec.body)];
    let tasks_rel = format!("{spec_rel}/tasks");
    if safe_fs::check_chain(root, &tasks_rel).is_ok() {
        for file in real_children(&root.join(&tasks_rel), false) {
            let Some(stem) = file.strip_suffix(".md") else { continue };
            if let Ok(task) = read_task(root, &format!("{tasks_rel}/{file}")) {
                let id = if task.frontmatter.id.is_empty() { stem.to_string() } else { task.frontmatter.id };
                texts.push((Origin::Task(id), task.body));
            }
        }
    }
    Some((title, texts))
}

/// Pastas de spec do projeto (sem as ocultas), em ordem.
fn spec_names(root: &Path) -> Vec<String> {
    if safe_fs::check_chain(root, SPECS_REL).is_err() {
        return Vec::new();
    }
    real_children(&root.join(SPECS_REL), true).into_iter().filter(|n| !n.starts_with('.')).collect()
}

/// Âncora de saída de uma citação (`null` para o documento inteiro), redigida.
fn anchor_view(c: &Citation, secrets: &SecretSet) -> Value {
    json!(c.section.as_deref().map(|s| scrub_text(secrets, s)))
}

/// `used_in_plans` de `doc`: as specs que o citam no rascunho atual ou na versão congelada na
/// aprovação, com a âncora e o lugar de cada citação. Os textos sem `kb:<doc>` nem são fatiados.
pub fn used_in_plans(root: &Path, doc: &str, secrets: &SecretSet) -> Vec<Value> {
    let needle = format!("kb:{doc}");
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for name in spec_names(root) {
        if out.len() >= MAX_LINK_ITEMS {
            break;
        }
        let Some((title, texts)) = plan_texts(root, &name, secrets) else { continue };
        let mut uses: Vec<Value> = Vec::new();
        for (origin, text) in &texts {
            if !text.contains(&needle) {
                continue;
            }
            for placed in catalog::placed_citations(text).iter().filter(|p| p.citation.doc == doc) {
                let mut item = use_view(origin, placed, secrets);
                item["anchor"] = anchor_view(&placed.citation, secrets);
                uses.push(item);
            }
        }
        let frozen = frozen_anchors(root, &name, doc);
        if uses.is_empty() && frozen.is_none() {
            continue;
        }
        // O que só a versão aprovada cita (o rascunho mudou depois): o lugar não é mais conhecido.
        for c in frozen.iter().flatten() {
            let anchor = anchor_view(c, secrets);
            if !uses.iter().any(|u| u["anchor"] == anchor) {
                uses.push(json!({ "in": "frozen", "section": null, "structural": false, "binding": false, "anchor": anchor }));
            }
        }
        seen.push(name.clone());
        out.push(json!({ "spec": id_view(secrets, &name), "title": title, "frozen": frozen.is_some(), "uses": uses }));
    }
    // Specs congeladas cuja pasta não existe mais.
    for name in frozen_names(root) {
        if out.len() >= MAX_LINK_ITEMS || seen.contains(&name) {
            continue;
        }
        let Some(frozen) = frozen_anchors(root, &name, doc) else { continue };
        let uses: Vec<Value> = frozen
            .iter()
            .map(|c| json!({ "in": "frozen", "section": null, "structural": false, "binding": false, "anchor": anchor_view(c, secrets) }))
            .collect();
        out.push(json!({ "spec": id_view(secrets, &name), "title": null, "frozen": true, "uses": uses }));
    }
    out
}

/// Nomes das specs com versão congelada (`.sdd/kb/frozen/<spec>.json`).
fn frozen_names(root: &Path) -> Vec<String> {
    if safe_fs::check_chain(root, cite::FROZEN_REL).is_err() {
        return Vec::new();
    }
    real_children(&root.join(cite::FROZEN_REL), false)
        .into_iter()
        .filter_map(|f| f.strip_suffix(".json").map(str::to_string))
        .collect()
}

/// Citações de `doc` na versão congelada da spec `name`; `None` se ela não o cita (ou não há versão
/// congelada).
fn frozen_anchors(root: &Path, name: &str, doc: &str) -> Option<Vec<Citation>> {
    let frozen = cite::load_frozen(root, name)?;
    let cited: Vec<Citation> = frozen.sources.iter().flat_map(|s| cite::find(&s.citation)).filter(|c| c.doc == doc).collect();
    (!cited.is_empty()).then_some(cited)
}

// --- fontes do plano ---

/// Uma fonte citada no plano e os lugares em que aparece.
struct Source {
    citation: Citation,
    binding: bool,
    uses: Vec<Value>,
}

/// `GET /specs/:spec_id/kb-sources`: as citações da spec e das tarefas, sem repetir. `None` se a
/// spec não existe (sem `spec.md` legível).
pub fn plan_sources(root: &Path, spec: &str, secrets: &SecretSet) -> Result<Option<Value>, KbError> {
    let Some((_, texts)) = plan_texts(root, spec, secrets) else {
        return Ok(None);
    };
    let mut sources: Vec<Source> = Vec::new();
    for (origin, text) in &texts {
        // A mesma regra da T02 (C-04): regra é o que está em critério, Premissas ou Orientações.
        let binding = catalog::binding_citations(text);
        for placed in catalog::placed_citations(text) {
            let item = use_view(origin, &placed, secrets);
            let is_binding = binding.contains(&placed.citation);
            match sources.iter_mut().find(|s| s.citation == placed.citation) {
                Some(s) => {
                    s.binding |= is_binding;
                    s.uses.push(item);
                }
                None => sources.push(Source { citation: placed.citation, binding: is_binding, uses: vec![item] }),
            }
        }
    }
    if sources.is_empty() {
        return Ok(Some(json!({ "sources": [] })));
    }
    let docs = kb::load_manifest(root)?.docs;
    let index = index::index_for(root)?;
    let views: Vec<Value> = sources
        .into_iter()
        .map(|s| {
            let c = &s.citation;
            let entry = docs.iter().find(|d| d.id == c.doc);
            let ready = entry.is_some_and(|e| e.status == Status::Ready);
            // A mesma régua do portão (`cite::resolve` → `Index::section`): a âncora pública ou o
            // título sem caixa nem acento. Sai a âncora pública que resolveu e o título redigido.
            let resolved = c.section.as_deref().and_then(|anchor| index.resolve_section(&c.doc, anchor, secrets));
            // Removida: o documento saiu da base, a nota sumiu do índice ou a seção não existe mais.
            let removed = match entry {
                None => true,
                Some(_) if !ready => false,
                Some(_) => !index.has_doc(&c.doc) || (c.section.is_some() && resolved.is_none()),
            };
            let (public, section_title) = match resolved {
                Some((public, title)) => (Some(public), Some(title)),
                None => (None, None),
            };
            json!({
                "citation": scrub_text(secrets, &c.text()),
                "doc": c.doc,
                "doc_title": entry.map(|e| scrub_text(secrets, &e.title)),
                "anchor": anchor_view(c, secrets),
                "resolved_anchor": public,
                "section_title": section_title,
                "binding": s.binding,
                "removed": removed,
                "uses": s.uses,
            })
        })
        .collect();
    Ok(Some(json!({ "sources": views })))
}

#[cfg(test)]
#[path = "kb_read_tests.rs"]
mod tests;
