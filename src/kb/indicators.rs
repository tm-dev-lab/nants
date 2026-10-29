//! Indicadores dos documentos da base (spec 134 T06, C-24), calculados pelo servidor sem modelo,
//! a partir do manifesto e do índice já montado:
//! - `duplicate`: o mesmo conteúdo que outro documento por outro caminho (um link e um arquivo,
//!   por exemplo): o mesmo SHA-256 do original ou os mesmos termos no texto extraído;
//! - `no_headings`: a nota não tem títulos, então só se cita o documento inteiro (o PDF é citado
//!   por página e nunca entra aqui);
//! - `broken_link`: um link interno (para um arquivo `.md`, `.txt` ou `.pdf`) que não aponta para
//!   nenhum documento da base.
//!
//! O "sem texto" é o próprio estado `no_text` do documento. Cada indicador sai com `code` e
//! `params`; a interface traduz.

use std::collections::HashMap;
use std::path::Path;

use super::index::{index_for, Index};
use super::{load_manifest, Entry, KbError, Status};

/// Indicador de um documento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Indicator {
    /// Mesmo conteúdo que o documento `doc`.
    Duplicate { doc: String },
    NoHeadings,
    /// Link interno para o arquivo `target` (nome em minúsculas) sem documento na base.
    BrokenLink { target: String },
}

impl Indicator {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Duplicate { .. } => "duplicate",
            Self::NoHeadings => "no_headings",
            Self::BrokenLink { .. } => "broken_link",
        }
    }
}

/// Indicadores de cada documento pronto (só os que têm algum), a partir do manifesto e do índice.
/// Custo linear: uma passada no índice e mapas por SHA-256, por impressão do texto e pelos alvos
/// de link; nada de comparar todos os pares (o `GET /kb` roda a cada `kb_updated`).
pub fn compute(docs: &[Entry], index: &Index) -> HashMap<String, Vec<Indicator>> {
    let facts: HashMap<&str, (Option<&str>, bool)> = index.doc_facts().map(|(id, hash, headings)| (id, (hash, headings))).collect();
    let ready: Vec<&Entry> = docs.iter().filter(|d| d.status == Status::Ready).collect();
    let mut by_sha: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut by_text: HashMap<&str, Vec<&str>> = HashMap::new();
    for doc in &ready {
        if !doc.sha256.is_empty() {
            by_sha.entry(doc.sha256.as_str()).or_default().push(doc.id.as_str());
        }
        if let Some((Some(hash), _)) = facts.get(doc.id.as_str()) {
            by_text.entry(hash).or_default().push(doc.id.as_str());
        }
    }
    // Alvos que resolvem: `file:<nome em minúsculas>` e `url:<endereço>` de cada documento da base
    // (a mesma regra de [`super::link_points_to`]), com quem os tem.
    let mut targets: HashMap<String, Vec<&str>> = HashMap::new();
    for doc in docs {
        if let Some(name) = &doc.file_name {
            targets.entry(format!("file:{}", name.to_lowercase())).or_default().push(doc.id.as_str());
        }
        if let Some(url) = &doc.source_url {
            targets.entry(format!("url:{url}")).or_default().push(doc.id.as_str());
        }
    }
    let other_than = |ids: Option<&Vec<&str>>, me: &str| -> Option<String> {
        ids.and_then(|list| list.iter().find(|id| **id != me)).map(|id| (*id).to_string())
    };

    let mut out: HashMap<String, Vec<Indicator>> = HashMap::new();
    for doc in &ready {
        let me = doc.id.as_str();
        let mut list = Vec::new();
        let fact = facts.get(me).copied();
        let twin = (!doc.sha256.is_empty())
            .then(|| other_than(by_sha.get(doc.sha256.as_str()), me))
            .flatten()
            .or_else(|| fact.and_then(|(hash, _)| hash).and_then(|h| other_than(by_text.get(h), me)));
        if let Some(other) = twin {
            list.push(Indicator::Duplicate { doc: other });
        }
        if fact.is_some_and(|(_, headings)| !headings) {
            list.push(Indicator::NoHeadings);
        }
        for target in &doc.link_targets {
            let Some(name) = target.strip_prefix("file:") else {
                continue;
            };
            let resolves = targets.get(target).is_some_and(|ids| ids.iter().any(|id| *id != me));
            let broken = Indicator::BrokenLink { target: name.to_string() };
            if !resolves && !list.contains(&broken) {
                list.push(broken);
            }
        }
        if !list.is_empty() {
            out.insert(doc.id.clone(), list);
        }
    }
    out
}

/// Indicadores da base do projeto em `root` (o índice vem do cache, montado uma vez por versão do
/// manifesto).
pub fn for_project(root: &Path) -> Result<HashMap<String, Vec<Indicator>>, KbError> {
    let docs = load_manifest(root)?.docs;
    let index = index_for(root)?;
    Ok(compute(&docs, &index))
}

#[cfg(test)]
#[path = "indicators_tests.rs"]
mod tests;
