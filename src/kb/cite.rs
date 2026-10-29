//! Citações da base no plano (spec 134 T05): `kb:<id>` (o documento inteiro) e
//! `kb:<id>#<âncora>` (uma seção).
//!
//! - **Validação:** [`check`] recusa, na gravação da spec e das tarefas, a citação que não resolve
//!   no manifesto (documento fora da base ou ainda não pronto) ou no índice (seção inexistente),
//!   com o motivo, para o modelo corrigir.
//! - **Congelamento:** na aprovação da spec, [`freeze`] grava em `.sdd/kb/frozen/<spec>.json`
//!   (fora do Git, dentro de `.sdd/kb/`) o SHA-256 do texto de cada seção citada, o catálogo e os
//!   padrões daquele momento. Durante a execução, o catálogo e o índice de padrões vêm dali (não
//!   mudam a cada turno) e [`changed`] diz quais fontes citadas mudaram desde a aprovação.
//!
//! A base é sempre dado: o que obriga é a spec aprovada (decisão C-14 da spec).

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::catalog::{self, StandardRef};
use super::index;
use super::{is_doc_id, load_manifest, sha256_hex, write_atomic, KbError, Manifest, Status, KB_REL};
use crate::server::events::SecretSet;
use crate::server::safe_fs::{self, SafeFsError};

/// Pasta das versões congeladas, relativa à raiz do projeto (dentro de `.sdd/kb/`, fora do Git).
pub const FROZEN_REL: &str = ".sdd/kb/frozen";
/// Versão do arquivo congelado.
pub const FROZEN_VERSION: u32 = 1;
/// Teto de leitura do arquivo congelado.
const MAX_FROZEN_BYTES: u64 = 1024 * 1024;
/// Motivos listados numa recusa (o resto vira "e mais N").
const MAX_ERRORS: usize = 8;
/// Âncoras listadas quando a seção citada não existe.
const MAX_ANCHORS_LISTED: usize = 20;
/// Tamanho máximo do texto citado ecoado num motivo.
const MAX_ECHO_CHARS: usize = 80;
/// Teto do id de spec no nome do arquivo congelado.
const MAX_SPEC_ID_CHARS: usize = 128;

/// Uma citação da base.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Citation {
    pub doc: String,
    /// Âncora da seção; `None` cita o documento inteiro.
    pub section: Option<String>,
}

impl Citation {
    /// Forma escrita: `kb:<id>` ou `kb:<id>#<âncora>`.
    pub fn text(&self) -> String {
        match &self.section {
            Some(s) => index::citation(&self.doc, s),
            None => format!("kb:{}", self.doc),
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Citações de `text`, sem repetição, na ordem em que aparecem. Só conta `kb:` seguido de um id
/// com a forma real ([`is_doc_id`], `<slug>-<hex de 8, 16 ou 32>`): `kb:session`, `kb:user:1`, o
/// modelo `kb:<id>#<seção>` e `kb:` dentro de outra palavra (`abckb:`) não são citação.
/// O ponto e o hífen no fim da âncora são pontuação da frase e saem.
pub fn find(text: &str) -> Vec<Citation> {
    let mut out: Vec<Citation> = Vec::new();
    for (at, _) in text.match_indices("kb:") {
        if text[..at].chars().next_back().is_some_and(is_word) {
            continue;
        }
        let rest = &text[at + 3..];
        let id_len = rest.find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
        let id = rest[..id_len].trim_end_matches('-');
        if !is_doc_id(id) {
            continue;
        }
        let after = &rest[id_len..];
        let section = after.strip_prefix('#').and_then(|a| {
            let len = a.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')).unwrap_or(a.len());
            let anchor = a[..len].trim_end_matches(['.', '-']);
            (!anchor.is_empty()).then(|| anchor.to_string())
        });
        let c = Citation { doc: id.to_string(), section };
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// Ids dos documentos citados em `text`, sem repetição.
pub fn cited_docs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in find(text) {
        if !out.contains(&c.doc) {
            out.push(c.doc);
        }
    }
    out
}

/// Texto que a citação aponta hoje: a seção (com as subseções) ou o documento inteiro. `Err` com o
/// motivo, para o modelo (nunca com caminho).
pub fn resolve(root: &Path, manifest: &Manifest, c: &Citation, secrets: &SecretSet) -> Result<String, String> {
    let cited = c.text();
    if !manifest.docs.iter().any(|d| d.id == c.doc && d.status == Status::Ready) {
        return Err(format!(
            "`{cited}`: o documento `{}` não está pronto na base de conhecimento do projeto (use kb_search para achar o id)",
            c.doc
        ));
    }
    match &c.section {
        None => index::doc_text(root, &c.doc, secrets).map_err(|_| format!("`{cited}`: documento ilegível no índice da base")),
        Some(anchor) => match index::read_section(root, &c.doc, anchor, secrets) {
            Ok(s) => Ok(s.text),
            Err(KbError::NotFound) => {
                let anchors: Vec<String> = index::list_sections(root, &c.doc, secrets)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|s| s.anchor)
                    .collect();
                let more = anchors.len().saturating_sub(MAX_ANCHORS_LISTED);
                let mut shown = anchors.into_iter().take(MAX_ANCHORS_LISTED).collect::<Vec<_>>().join(", ");
                if more > 0 {
                    shown.push_str(&format!(" e mais {more}"));
                }
                let echo: String = anchor.chars().take(MAX_ECHO_CHARS).collect();
                Err(format!(
                    "`kb:{}#{echo}`: a seção `{echo}` não existe nesse documento; âncoras válidas: {shown} (kb_read com section \"list\" mostra o índice)",
                    c.doc
                ))
            }
            Err(_) => Err(format!("`{cited}`: base de conhecimento indisponível agora")),
        },
    }
}

/// Confere todas as citações de `text` (spec ou tarefa): `Ok` sem citação, com a base sem nenhum
/// documento ou com todas resolvidas; `Err` com os motivos, um por linha. Sem citação, a base nem
/// é lida.
pub fn check(root: &Path, text: &str, secrets: &SecretSet) -> Result<(), String> {
    check_counted(root, text, secrets).map_err(|r| r.message)
}

/// Recusa de [`check`] com a contagem (spec 134 T07, C-24): quantas citações inválidas o texto
/// trazia, inclusive as que a mensagem resume em "e mais N". A contagem é o indicador de nota
/// inventada; a mensagem vai ao modelo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub count: usize,
    pub message: String,
}

/// Como [`check`], com a contagem das citações recusadas.
pub fn check_counted(root: &Path, text: &str, secrets: &SecretSet) -> Result<(), Refusal> {
    let found = find(text);
    if found.is_empty() {
        return Ok(());
    }
    let manifest = load_manifest(root).unwrap_or_default();
    // Sem nenhum documento na base, nada é validado: o projeto sem base grava como antes (G2).
    if manifest.docs.is_empty() {
        return Ok(());
    }
    let errors: Vec<String> = found.iter().filter_map(|c| resolve(root, &manifest, c, secrets).err()).collect();
    if errors.is_empty() {
        return Ok(());
    }
    let count = errors.len();
    let more = count.saturating_sub(MAX_ERRORS);
    let mut lines: Vec<String> = errors.into_iter().take(MAX_ERRORS).map(|e| format!("- {e}")).collect();
    if more > 0 {
        lines.push(format!("- e mais {more} citações inválidas"));
    }
    Err(Refusal {
        count,
        message: format!(
            "citação da base de conhecimento que não existe; corrija ou remova a citação e grave de novo (nada foi gravado):\n{}",
            lines.join("\n")
        ),
    })
}

/// Uma fonte citada e congelada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenSource {
    pub citation: String,
    /// SHA-256 do texto da seção (ou do documento) na aprovação; vazio se já não resolvia.
    pub sha256: String,
}

/// O que a aprovação congela de uma spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frozen {
    pub version: u32,
    pub spec: String,
    pub frozen_at: String,
    #[serde(default)]
    pub sources: Vec<FrozenSource>,
    /// Catálogo do prefixo do Arquiteto na aprovação ([`catalog::render`]).
    #[serde(default)]
    pub catalog: String,
    /// Padrões prontos na aprovação, para o índice por escopo do briefing.
    #[serde(default)]
    pub standards: Vec<StandardRef>,
}

/// Id de spec seguro para o nome do arquivo.
fn safe_spec_id(spec_id: &str) -> bool {
    !spec_id.is_empty()
        && spec_id.chars().count() <= MAX_SPEC_ID_CHARS
        && !spec_id.starts_with('.')
        && spec_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Caminho do arquivo congelado da spec, relativo à raiz.
pub fn frozen_rel(spec_id: &str) -> String {
    format!("{FROZEN_REL}/{spec_id}.json")
}

/// Congela as citações de `texts` (a spec e as tarefas) e o catálogo atual. Sem documento pronto
/// e sem citação, nada é gravado (`Ok(None)`). Sobrescreve uma versão anterior da mesma spec.
pub fn freeze(root: &Path, spec_id: &str, texts: &[&str], secrets: &SecretSet, now: &str) -> Result<Option<Frozen>, KbError> {
    if !safe_spec_id(spec_id) {
        return Err(KbError::NotFound);
    }
    let manifest = load_manifest(root)?;
    let mut cited: Vec<Citation> = Vec::new();
    for text in texts {
        for c in find(text) {
            if !cited.contains(&c) {
                cited.push(c);
            }
        }
    }
    let ready = manifest.docs.iter().any(|d| d.status == Status::Ready);
    if !ready && cited.is_empty() {
        return Ok(None);
    }
    let sources = cited
        .iter()
        .map(|c| FrozenSource {
            citation: c.text(),
            sha256: resolve(root, &manifest, c, secrets).map(|t| sha256_hex(t.as_bytes())).unwrap_or_default(),
        })
        .collect();
    let frozen = Frozen {
        version: FROZEN_VERSION,
        spec: spec_id.to_string(),
        frozen_at: now.to_string(),
        sources,
        catalog: catalog::render(root, secrets),
        standards: catalog::standards(&manifest),
    };
    let mut text = serde_json::to_string_pretty(&frozen).map_err(|_| KbError::Fs(SafeFsError::Io))?;
    text.push('\n');
    safe_fs::ensure_dirs(root, KB_REL)?;
    safe_fs::ensure_dirs(root, FROZEN_REL)?;
    write_atomic(root, &frozen_rel(spec_id), text.as_bytes())?;
    Ok(Some(frozen))
}

/// Versão congelada da spec; ausente, ilegível ou de outra spec → `None`.
pub fn load_frozen(root: &Path, spec_id: &str) -> Option<Frozen> {
    if !safe_spec_id(spec_id) {
        return None;
    }
    let text = safe_fs::read_regular(root, &frozen_rel(spec_id), MAX_FROZEN_BYTES).ok()?;
    serde_json::from_str::<Frozen>(&text).ok().filter(|f| f.spec == spec_id)
}

/// Fontes citadas que mudaram desde o congelamento: o texto de hoje tem outro SHA-256 ou a
/// citação já não resolve (documento removido, seção renomeada).
pub fn changed(root: &Path, frozen: &Frozen, secrets: &SecretSet) -> Vec<String> {
    if frozen.sources.is_empty() {
        return Vec::new();
    }
    let manifest = load_manifest(root).unwrap_or_default();
    frozen
        .sources
        .iter()
        .filter(|s| {
            let Some(c) = find(&s.citation).into_iter().next() else {
                return true;
            };
            match resolve(root, &manifest, &c, secrets) {
                Ok(text) => sha256_hex(text.as_bytes()) != s.sha256,
                Err(_) => !s.sha256.is_empty(),
            }
        })
        .map(|s| s.citation.clone())
        .collect()
}

#[cfg(test)]
#[path = "cite_tests.rs"]
mod tests;
