//! Índice da base de conhecimento (spec 134/T03): fatiamento por títulos e por página e busca
//! BM25 própria, em memória, sem índice em disco.
//!
//! - **Fatiamento:** markdown, texto e links pelos títulos `#` a `######`
//!   ([`crate::sdd::spec_sections::parse_levels`], o mesmo fatiador da spec, a partir do nível 1);
//!   o PDF pelas seções `# p. N` da nota, uma por página. O texto antes do primeiro título vira a
//!   seção [`TOP_ANCHOR`]. Cada seção guarda a trilha "Documento > H2 > H3"; uma seção acima de
//!   [`CHUNK_MAX_BYTES`] se divide em trechos por parágrafo (e, num parágrafo enorme, por linha ou
//!   por palavra).
//! - **Tokenização:** letras e dígitos Unicode, em minúsculas e com os acentos dobrados
//!   ([`super::fold_accent`]), sem stemming e sem lista de palavras vazias.
//! - **Pontuação:** BM25 (k1 = [`K1`], b = [`B`]) sobre a frequência ponderada de cada trecho: o
//!   corpo pesa 1, a trilha (título do documento e títulos das seções) [`W_TRAIL`], as `tags` do
//!   frontmatter [`W_TAGS`] e a descrição de uma linha [`W_DESCRIPTION`].
//! - **Consulta:** termos soltos em OR ranqueado; frase entre aspas é obrigatória e precisa
//!   aparecer em sequência (no corpo, na trilha, nas tags ou na descrição). O melhor trecho de cada
//!   seção representa a seção, com um recorte de [`SNIPPET_MIN_CHARS`] a [`SNIPPET_MAX_CHARS`]
//!   caracteres em volta dos termos.
//! - **Montagem preguiçosa:** o índice de um projeto nasce na primeira consulta ([`index_for`]) e
//!   vale enquanto o SHA-256 do manifesto não mudar (toda mudança de nota passa pelo manifesto).
//!   Consultas simultâneas do mesmo projeto esperam uma única montagem; projetos diferentes não
//!   se bloqueiam. Só documentos `ready` entram.
//!
//! Os textos daqui vão ao modelo (resultado de ferramenta), não à pessoa; a saída passa de novo
//! pela redação de segredos.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use super::{
    extract, fold_accent, note_body, read_note_file, sha256_hex, slugify, Entry, KbError, Kind, Manifest, Role, Status,
    MANIFEST_REL, MAX_MANIFEST_BYTES,
};
use crate::server::events::SecretSet;
use crate::server::projects::scrub_text;
use crate::server::safe_fs::{self, SafeFsError};
use crate::sdd::spec_sections;

/// Teto de um trecho do índice: uma seção maior se divide por parágrafo.
pub const CHUNK_MAX_BYTES: usize = 4 * 1024;
/// Recorte devolvido em volta dos termos: alvo, mínimo e máximo, em caracteres.
pub const SNIPPET_TARGET_CHARS: usize = 480;
pub const SNIPPET_MIN_CHARS: usize = 300;
pub const SNIPPET_MAX_CHARS: usize = 600;
/// Teto de resultados por consulta.
pub const MAX_RESULTS: usize = 50;
/// Âncora da seção com o texto antes do primeiro título.
pub const TOP_ANCHOR: &str = "top";
/// Pesos dos campos na frequência de cada trecho (o corpo pesa 1).
pub const W_TRAIL: f32 = 3.0;
pub const W_TAGS: f32 = 3.0;
pub const W_DESCRIPTION: f32 = 2.0;
/// Parâmetros do BM25.
pub const K1: f32 = 1.2;
pub const B: f32 = 0.75;
/// Termos mais longos que isto (blocos base64, hashes) não entram no índice.
const MAX_TOKEN_CHARS: usize = 64;
/// Teto de tags lidas do frontmatter de um documento.
const MAX_TAGS: usize = 32;
/// Separador da trilha.
const TRAIL_SEP: &str = " > ";

// --- tokenização ---

/// Termos de `text` com o intervalo em bytes de cada um: letras e dígitos Unicode, em minúsculas
/// e com os acentos dobrados. Marcas combinantes (texto decomposto) ficam dentro do termo e somem.
pub fn tokens(text: &str) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut start: Option<usize> = None;
    let mut chars = 0usize;
    let mut flush = |cur: &mut String, start: &mut Option<usize>, chars: &mut usize, end: usize| {
        if let Some(s) = start.take() {
            if !cur.is_empty() && *chars <= MAX_TOKEN_CHARS {
                out.push((s..end, std::mem::take(cur)));
            }
        }
        cur.clear();
        *chars = 0;
    };
    for (at, c) in text.char_indices() {
        if c.is_alphanumeric() {
            if start.is_none() {
                start = Some(at);
            }
            for low in c.to_lowercase() {
                cur.push(fold_accent(low));
            }
            chars += 1;
        } else if start.is_some() && ('\u{300}'..='\u{36f}').contains(&c) {
            // Acento combinante: continua o termo, sem entrar nele.
        } else {
            flush(&mut cur, &mut start, &mut chars, at);
        }
    }
    flush(&mut cur, &mut start, &mut chars, text.len());
    out
}

/// Só os termos, sem os intervalos.
fn terms_of(text: &str) -> Vec<String> {
    tokens(text).into_iter().map(|(_, t)| t).collect()
}

/// Comparação de títulos e âncoras sem caixa nem acento.
fn folded(text: &str) -> String {
    terms_of(text).join(" ")
}

// --- estruturas ---

/// Uma seção de um documento do índice.
#[derive(Debug, Clone)]
struct SectionData {
    anchor: String,
    title: String,
    level: usize,
    trail: String,
    page: Option<u32>,
    /// Cabeçalho e texto próprio (sem as subseções).
    own: String,
    /// Índice (exclusivo) da última descendente.
    end: usize,
    /// Linha do cabeçalho no corpo da nota (0 para o texto antes do primeiro título).
    line: usize,
}

#[derive(Debug, Clone)]
struct DocData {
    id: String,
    title: String,
    role: Role,
    kind: Kind,
    sections: Vec<SectionData>,
    /// SHA-256 dos termos do texto (sem formatação, caixa nem acento): o mesmo conteúdo vindo por
    /// caminhos diferentes (um link e um arquivo) dá o mesmo valor (indicador de duplicata, C-24).
    text_hash: String,
}

/// Um trecho pontuável: um pedaço do texto próprio de uma seção.
#[derive(Debug, Clone)]
struct Chunk {
    doc: u32,
    section: u32,
    range: Range<usize>,
}

/// O índice de um projeto.
#[derive(Debug, Default)]
pub struct Index {
    docs: Vec<DocData>,
    /// Termos extras (tags e descrição) de cada documento, para a frase.
    doc_meta: Vec<(Vec<String>, Vec<String>)>,
    chunks: Vec<Chunk>,
    terms: HashMap<String, u32>,
    /// Por termo: (trecho, frequência ponderada), em ordem de trecho.
    postings: Vec<Vec<(u32, f32)>>,
    /// Comprimento ponderado de cada trecho.
    lens: Vec<f32>,
    avg_len: f32,
}

/// Um resultado da busca: a melhor passagem de uma seção.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub doc_id: String,
    pub doc_title: String,
    pub role: Role,
    /// Âncora da seção, estável no documento: a citação é `kb:<doc_id>#<section>`.
    pub section: String,
    pub title: String,
    pub trail: String,
    /// Página, nos PDFs.
    pub page: Option<u32>,
    pub snippet: String,
    pub score: f32,
}

impl Hit {
    /// Citação `kb:<id>#<seção>`.
    pub fn citation(&self) -> String {
        citation(&self.doc_id, &self.section)
    }
}

/// Citação `kb:<id>#<seção>`.
pub fn citation(doc_id: &str, anchor: &str) -> String {
    format!("kb:{doc_id}#{anchor}")
}

/// Uma seção na lista de um documento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionInfo {
    pub anchor: String,
    pub title: String,
    /// 0 para [`TOP_ANCHOR`], 1 a 6 para os títulos (o PDF usa 1 por página).
    pub level: usize,
    pub trail: String,
    pub page: Option<u32>,
    /// Linha do cabeçalho no corpo da nota (o `content` do `GET /kb/docs/:doc`), a partir de 0.
    pub line: usize,
    /// Tamanho da seção inteira (com as subseções), em bytes.
    pub bytes: usize,
}

/// Uma seção lida inteira, com as subseções.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionText {
    pub doc_id: String,
    pub doc_title: String,
    pub role: Role,
    pub anchor: String,
    pub title: String,
    pub trail: String,
    pub page: Option<u32>,
    pub text: String,
}

/// Números do índice, para a medição e a instrumentação.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub docs: usize,
    pub sections: usize,
    pub chunks: usize,
    pub terms: usize,
    pub postings: usize,
    /// Texto guardado (corpo das seções), em bytes.
    pub text_bytes: usize,
    /// Estimativa da memória das estruturas, em bytes.
    pub approx_bytes: usize,
}

// --- fatiamento ---

/// Página de uma linha `# p. N` da nota de um PDF.
fn page_heading(line: &str) -> Option<u32> {
    let n = line.strip_prefix("# p. ")?.trim_end();
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then(|| n.parse().ok()).flatten()
}

/// Seções de um PDF: uma por página.
fn pdf_sections(body: &str) -> Vec<(usize, String, String, Option<u32>)> {
    let mut out: Vec<(usize, String, String, Option<u32>)> = Vec::new();
    for line in body.lines() {
        if let Some(page) = page_heading(line) {
            out.push((1, format!("p. {page}"), line.to_string(), Some(page)));
        } else if let Some(last) = out.last_mut() {
            last.2.push('\n');
            last.2.push_str(line);
        }
    }
    for s in &mut out {
        let trimmed = s.2.trim_end().len();
        s.2.truncate(trimmed);
    }
    out
}

/// Tags do frontmatter de origem (`tags` ou `keywords`, lista ou texto separado por vírgula).
fn frontmatter_tags(body: &str) -> Vec<String> {
    let (Some(block), _) = extract::split_frontmatter(body) else {
        return Vec::new();
    };
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(block) else {
        return Vec::new();
    };
    let Some(map) = value.as_mapping() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for key in ["tags", "keywords"] {
        match map.get(serde_yaml::Value::String(key.to_string())) {
            Some(serde_yaml::Value::Sequence(items)) => {
                out.extend(items.iter().filter_map(|i| i.as_str()).map(str::to_string));
            }
            Some(serde_yaml::Value::String(text)) => out.extend(text.split(',').map(str::to_string)),
            _ => {}
        }
    }
    out.into_iter()
        .map(|t| t.trim().trim_start_matches('#').trim().chars().take(MAX_TOKEN_CHARS).collect::<String>())
        .filter(|t| !t.is_empty())
        .take(MAX_TAGS)
        .collect()
}

/// Âncora única no documento: o slug do título; repetida ganha `-2`, `-3`...
fn unique_anchor(base: &str, used: &mut HashSet<String>) -> String {
    let mut anchor = base.to_string();
    let mut n = 2;
    while !used.insert(anchor.clone()) {
        anchor = format!("{base}-{n}");
        n += 1;
    }
    anchor
}

/// Seções de um documento, com âncora, trilha e página.
fn doc_sections(entry: &Entry, body: &str) -> Vec<SectionData> {
    // (nível, título, texto próprio, página, fim, linha)
    let mut raw: Vec<(usize, String, String, Option<u32>, usize, usize)> = Vec::new();
    if entry.kind == Kind::Pdf {
        let page_lines: Vec<usize> = body.lines().enumerate().filter(|(_, l)| page_heading(l).is_some()).map(|(i, _)| i).collect();
        for (i, (level, title, own, page)) in pdf_sections(body).into_iter().enumerate() {
            raw.push((level, title, own, page, i + 1, page_lines.get(i).copied().unwrap_or(0)));
        }
    } else {
        let (preamble, sections) = spec_sections::parse_levels(body, 1);
        let shift = usize::from(!preamble.trim().is_empty());
        if shift == 1 {
            raw.push((0, entry.title.clone(), preamble, None, 1, 0));
        }
        for s in sections {
            let end = s.end() + shift;
            raw.push((s.level, s.title, s.own, None, end, s.line));
        }
    }
    let mut used: HashSet<String> = HashSet::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut out = Vec::with_capacity(raw.len());
    for (level, title, own, page, end, line) in raw {
        let anchor = if level == 0 { unique_anchor(TOP_ANCHOR, &mut used) } else { unique_anchor(&slugify(&title), &mut used) };
        let mut trail: Vec<&str> = vec![entry.title.as_str()];
        if level > 0 {
            while stack.last().is_some_and(|(l, _)| *l >= level) {
                stack.pop();
            }
            trail.extend(stack.iter().map(|(_, t)| t.as_str()));
            trail.push(title.as_str());
            if trail.len() > 1 && folded(trail[0]) == folded(trail[1]) {
                trail.remove(0);
            }
        }
        let trail = trail.join(TRAIL_SEP);
        if level > 0 {
            stack.push((level, title.clone()));
        }
        out.push(SectionData { anchor, title, level, trail, page, own, end, line });
    }
    out
}

/// Intervalos de até `max` bytes de `text`: corta no fim de um parágrafo; num parágrafo enorme,
/// numa linha; numa linha enorme, num espaço; em último caso, numa fronteira de caractere.
fn split_ranges(text: &str, max: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    while text.len() - start > max {
        let limit = text.floor_char_boundary(start + max);
        let window = &text[start..limit];
        let min = max / 4;
        let cut = window
            .rfind("\n\n")
            .filter(|p| *p >= min)
            .map(|p| p + 2)
            .or_else(|| window.rfind('\n').filter(|p| *p >= min).map(|p| p + 1))
            .or_else(|| window.rfind(' ').filter(|p| *p >= min).map(|p| p + 1))
            .unwrap_or(window.len())
            .max(1);
        let cut = text.ceil_char_boundary(start + cut) - start;
        out.push(start..start + cut);
        start += cut;
    }
    if start < text.len() {
        out.push(start..text.len());
    }
    out.retain(|r| !text[r.clone()].trim().is_empty());
    out
}

// --- montagem ---

/// Conta os termos de `text` com o peso `w` em `tf`; devolve o comprimento ponderado.
fn add_terms(tf: &mut HashMap<String, f32>, terms: &[String], w: f32) -> f32 {
    for t in terms {
        *tf.entry(t.clone()).or_insert(0.0) += w;
    }
    terms.len() as f32 * w
}

impl Index {
    /// Monta o índice com as entradas e o corpo das notas (sem o frontmatter da nota).
    pub fn build(docs: Vec<(Entry, String)>) -> Self {
        let mut index = Index::default();
        let mut total_len = 0f64;
        for (entry, body) in docs {
            let doc_no = u32::try_from(index.docs.len()).unwrap_or(u32::MAX);
            let tags: Vec<String> = frontmatter_tags(&body).iter().flat_map(|t| terms_of(t)).collect();
            let description = terms_of(&entry.description);
            let sections = doc_sections(&entry, &body);
            for (s_no, section) in sections.iter().enumerate() {
                let trail = terms_of(&section.trail);
                // Página ou seção só com o título não tem trecho: fica na lista, fora da busca.
                let has_body = section.own.lines().skip(1).any(|l| !l.trim().is_empty()) || section.level == 0;
                if !has_body {
                    continue;
                }
                for range in split_ranges(&section.own, CHUNK_MAX_BYTES) {
                    let mut tf: HashMap<String, f32> = HashMap::new();
                    let mut len = add_terms(&mut tf, &terms_of(&section.own[range.clone()]), 1.0);
                    len += add_terms(&mut tf, &trail, W_TRAIL);
                    len += add_terms(&mut tf, &tags, W_TAGS);
                    len += add_terms(&mut tf, &description, W_DESCRIPTION);
                    let chunk_no = u32::try_from(index.chunks.len()).unwrap_or(u32::MAX);
                    for (term, f) in tf {
                        let next = u32::try_from(index.terms.len()).unwrap_or(u32::MAX);
                        let id = *index.terms.entry(term).or_insert(next) as usize;
                        if id == index.postings.len() {
                            index.postings.push(Vec::new());
                        }
                        index.postings[id].push((chunk_no, f));
                    }
                    index.lens.push(len);
                    total_len += f64::from(len);
                    index.chunks.push(Chunk { doc: doc_no, section: s_no as u32, range });
                }
            }
            index.doc_meta.push((tags, description));
            let text_hash = sha256_hex(terms_of(&body).join(" ").as_bytes());
            index.docs.push(DocData { id: entry.id, title: entry.title, role: entry.role, kind: entry.kind, sections, text_hash });
        }
        index.avg_len = if index.chunks.is_empty() { 1.0 } else { (total_len / index.chunks.len() as f64).max(1.0) as f32 };
        for list in &mut index.postings {
            list.shrink_to_fit();
        }
        index
    }

    /// Números do índice.
    pub fn stats(&self) -> Stats {
        let sections: usize = self.docs.iter().map(|d| d.sections.len()).sum();
        let text_bytes: usize = self.docs.iter().flat_map(|d| &d.sections).map(|s| s.own.len()).sum();
        let meta_bytes: usize = self
            .docs
            .iter()
            .flat_map(|d| &d.sections)
            .map(|s| s.anchor.len() + s.title.len() + s.trail.len() + std::mem::size_of::<SectionData>())
            .sum();
        let postings: usize = self.postings.iter().map(Vec::len).sum();
        let term_bytes: usize = self.terms.keys().map(|k| k.len() + 24 + 4 + 8).sum();
        let approx_bytes = text_bytes
            + meta_bytes
            + term_bytes
            + postings * std::mem::size_of::<(u32, f32)>()
            + self.postings.len() * 24
            + self.chunks.len() * (std::mem::size_of::<Chunk>() + 4);
        Stats { docs: self.docs.len(), sections, chunks: self.chunks.len(), terms: self.terms.len(), postings, text_bytes, approx_bytes }
    }

    /// Texto de um trecho.
    fn chunk_text(&self, c: &Chunk) -> &str {
        let own = &self.docs[c.doc as usize].sections[c.section as usize].own;
        own.get(c.range.clone()).unwrap_or("")
    }

    /// `true` se a frase aparece em sequência no corpo do trecho, na trilha, nas tags ou na
    /// descrição do documento.
    fn has_phrase(&self, c: &Chunk, phrase: &[String]) -> bool {
        let contains = |terms: &[String]| terms.windows(phrase.len()).any(|w| w == phrase);
        let section = &self.docs[c.doc as usize].sections[c.section as usize];
        let (tags, description) = &self.doc_meta[c.doc as usize];
        contains(&terms_of(self.chunk_text(c))) || contains(&terms_of(&section.trail)) || contains(tags) || contains(description)
    }

    /// Busca: termos em OR ranqueado, frases entre aspas obrigatórias; até `max` seções (no
    /// máximo [`MAX_RESULTS`]), da mais relevante para a menos.
    pub fn search(&self, query: &str, max: usize, secrets: &SecretSet) -> Vec<Hit> {
        self.search_role(query, max, None, secrets)
    }

    /// Como [`Self::search`], só nos documentos com a marcação `role` quando dada (spec 134 T04:
    /// `only_standards`). O filtro vem antes do teto e da conferência das frases: um padrão abaixo
    /// de muitas seções de referência não some.
    pub fn search_role(&self, query: &str, max: usize, role: Option<Role>, secrets: &SecretSet) -> Vec<Hit> {
        let (terms, phrases) = parse_query(query);
        if terms.is_empty() || self.chunks.is_empty() {
            return Vec::new();
        }
        let n = self.chunks.len() as f32;
        let mut scores = vec![0f32; self.chunks.len()];
        // Quantos termos distintos das frases cada trecho tem: só quem tem todos é conferido.
        let phrase_terms: HashSet<&String> = phrases.iter().flatten().collect();
        let mut present = vec![0u16; if phrase_terms.is_empty() { 0 } else { self.chunks.len() }];
        let mut touched: Vec<u32> = Vec::new();
        for term in &terms {
            let Some(&id) = self.terms.get(term) else {
                if phrase_terms.contains(term) {
                    return Vec::new(); // termo de frase ausente: nenhuma frase casa
                }
                continue;
            };
            let list = &self.postings[id as usize];
            let df = list.len() as f32;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            let in_phrase = phrase_terms.contains(term);
            for &(chunk, f) in list {
                let len = self.lens[chunk as usize];
                let norm = K1 * (1.0 - B + B * len / self.avg_len);
                let slot = &mut scores[chunk as usize];
                if *slot == 0.0 {
                    touched.push(chunk);
                }
                *slot += idf * f * (K1 + 1.0) / (f + norm);
                if in_phrase {
                    present[chunk as usize] = present[chunk as usize].saturating_add(1);
                }
            }
        }
        let needed = u16::try_from(phrase_terms.len()).unwrap_or(u16::MAX);
        let mut ranked: Vec<(u32, f32)> = touched
            .into_iter()
            .filter(|c| needed == 0 || present[*c as usize] >= needed)
            .filter(|c| role.is_none_or(|r| self.docs[self.chunks[*c as usize].doc as usize].role == r))
            .map(|c| (c, scores[c as usize]))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let max = max.clamp(1, MAX_RESULTS);
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        let mut hits = Vec::new();
        for (c, score) in ranked {
            let chunk = &self.chunks[c as usize];
            // A frase é conferida na ordem do ranking, só até completar o máximo.
            if seen.contains(&(chunk.doc, chunk.section)) || !phrases.iter().all(|p| self.has_phrase(chunk, p)) {
                continue;
            }
            seen.insert((chunk.doc, chunk.section));
            let doc = &self.docs[chunk.doc as usize];
            let section = &doc.sections[chunk.section as usize];
            hits.push(Hit {
                doc_id: doc.id.clone(),
                doc_title: scrub_text(secrets, &doc.title),
                role: doc.role,
                section: public_anchor(&doc.sections, chunk.section as usize, secrets),
                title: scrub_text(secrets, &section.title),
                trail: scrub_text(secrets, &section.trail),
                page: section.page,
                // Redige antes de recortar: um segredo na borda do recorte sairia pela metade.
                snippet: snippet(&scrub_text(secrets, self.chunk_text(chunk)), &terms),
                score,
            });
            if hits.len() >= max {
                break;
            }
        }
        hits
    }

    fn doc(&self, doc_id: &str) -> Option<&DocData> {
        self.docs.iter().find(|d| d.id == doc_id)
    }

    /// Seções de um documento do índice, na ordem do texto. `None` se ele não está no índice.
    pub fn sections(&self, doc_id: &str, secrets: &SecretSet) -> Option<Vec<SectionInfo>> {
        let doc = self.doc(doc_id)?;
        Some(
            doc.sections
                .iter()
                .enumerate()
                .map(|(i, s)| SectionInfo {
                    anchor: public_anchor(&doc.sections, i, secrets),
                    title: scrub_text(secrets, &s.title),
                    level: s.level,
                    trail: scrub_text(secrets, &s.trail),
                    page: s.page,
                    line: s.line,
                    bytes: full_len(&doc.sections, i),
                })
                .collect(),
        )
    }

    /// Seção inteira (com as subseções) pela âncora ou, sem ela, pelo título (sem caixa nem
    /// acento; o primeiro que casar). `None` se o documento ou a seção não existem.
    pub fn section(&self, doc_id: &str, wanted: &str, secrets: &SecretSet) -> Option<SectionText> {
        let doc = self.doc(doc_id)?;
        let wanted = wanted.trim().trim_start_matches('#').trim();
        let key = folded(wanted);
        let i = (0..doc.sections.len())
            .find(|&i| public_anchor(&doc.sections, i, secrets) == wanted)
            .or_else(|| (!key.is_empty()).then(|| doc.sections.iter().position(|s| folded(&s.title) == key)).flatten())?;
        let s = &doc.sections[i];
        let text: Vec<&str> = doc.sections[i..s.end.max(i + 1)].iter().map(|x| x.own.as_str()).collect();
        Some(SectionText {
            doc_id: doc.id.clone(),
            doc_title: scrub_text(secrets, &doc.title),
            role: doc.role,
            anchor: public_anchor(&doc.sections, i, secrets),
            title: scrub_text(secrets, &s.title),
            trail: scrub_text(secrets, &s.trail),
            page: s.page,
            text: scrub_text(secrets, &text.join("\n\n")),
        })
    }

    /// Texto inteiro de um documento do índice (todas as seções, redigido). `None` se ele não
    /// está no índice. É o que a citação do documento inteiro (`kb:<id>`) congela (spec 134 T05).
    pub fn doc_text(&self, doc_id: &str, secrets: &SecretSet) -> Option<String> {
        let doc = self.doc(doc_id)?;
        let parts: Vec<&str> = doc.sections.iter().map(|s| s.own.as_str()).collect();
        Some(scrub_text(secrets, &parts.join("\n\n")))
    }

    /// Para cada documento do índice, em uma passada: o id, a impressão do texto (`None` sem
    /// texto) e se tem títulos citáveis (a impressão é `DocData::text_hash`; o PDF, por página, sempre tem títulos).
    pub fn doc_facts(&self) -> impl Iterator<Item = (&str, Option<&str>, bool)> {
        self.docs.iter().map(|doc| {
            let has_text = doc.sections.iter().any(|s| !s.own.trim().is_empty());
            let headings = doc.kind == Kind::Pdf || doc.sections.iter().any(|s| s.level > 0);
            (doc.id.as_str(), has_text.then_some(doc.text_hash.as_str()), headings)
        })
    }

    /// Tipo do documento no índice.
    pub fn kind(&self, doc_id: &str) -> Option<Kind> {
        self.doc(doc_id).map(|d| d.kind)
    }
}

/// Prefixo da âncora neutra: o ponto nunca sai do `slugify` (só `[a-z0-9-]`), então a âncora
/// neutra não coincide com o slug de um título como "S 0" (spec 134 T04).
pub const NEUTRAL_ANCHOR_PREFIX: &str = "s.";

/// Âncora de saída da seção `i`: a âncora do título ou, se o título (ou a âncora) traz um segredo
/// conhecido, a forma neutra e estável `s.<posição>` ([`NEUTRAL_ANCHOR_PREFIX`]), aceita também
/// na leitura.
fn public_anchor(sections: &[SectionData], i: usize, secrets: &SecretSet) -> String {
    let s = &sections[i];
    if scrub_text(secrets, &s.title) != s.title || scrub_text(secrets, &s.anchor) != s.anchor {
        format!("{NEUTRAL_ANCHOR_PREFIX}{i}")
    } else {
        s.anchor.clone()
    }
}

/// Tamanho da seção `i` com as subseções (os textos unidos por uma linha em branco).
fn full_len(sections: &[SectionData], i: usize) -> usize {
    let range = i..sections[i].end.max(i + 1);
    let parts = range.len();
    sections[range].iter().map(|s| s.own.len()).sum::<usize>() + 2 * parts.saturating_sub(1)
}

/// Termos (sem repetição, na ordem) e frases da consulta. Uma aspa sem par vale como texto solto.
pub fn parse_query(query: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let mut terms: Vec<String> = Vec::new();
    let mut phrases: Vec<Vec<String>> = Vec::new();
    let parts: Vec<&str> = query.split('"').collect();
    let closed = parts.len() % 2 == 1;
    for (i, part) in parts.iter().enumerate() {
        let quoted = i % 2 == 1 && (closed || i + 1 < parts.len());
        let words = terms_of(part);
        if quoted && !words.is_empty() && !phrases.contains(&words) {
            phrases.push(words.clone());
        }
        for w in words {
            if !terms.contains(&w) {
                terms.push(w);
            }
        }
    }
    (terms, phrases)
}

/// Recorte de [`SNIPPET_MIN_CHARS`] a [`SNIPPET_MAX_CHARS`] caracteres em volta da região com
/// mais termos distintos da consulta, numa linha, com `...` onde o texto continua.
pub fn snippet(text: &str, terms: &[String]) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let total = flat.chars().count();
    if total <= SNIPPET_MAX_CHARS {
        return flat;
    }
    let hits: Vec<(usize, usize)> = tokens(&flat)
        .into_iter()
        .filter_map(|(r, t)| terms.iter().position(|q| *q == t).map(|k| (r.start, k)))
        .collect();
    // Byte de início da janela com mais termos distintos (o primeiro, no empate).
    let window_bytes = SNIPPET_TARGET_CHARS;
    let mut best = (0usize, 0usize);
    for (i, &(start, _)) in hits.iter().enumerate() {
        let distinct: HashSet<usize> = hits[i..].iter().take_while(|(s, _)| *s < start + window_bytes).map(|(_, k)| *k).collect();
        if distinct.len() > best.1 {
            best = (start, distinct.len());
        }
    }
    let chars: Vec<(usize, char)> = flat.char_indices().collect();
    let at = chars.partition_point(|(b, _)| *b < best.0);
    // Um pouco de contexto antes do primeiro termo.
    let mut from = at.saturating_sub(SNIPPET_TARGET_CHARS / 5);
    let mut to = (from + SNIPPET_TARGET_CHARS).min(total);
    if to - from < SNIPPET_TARGET_CHARS {
        from = to.saturating_sub(SNIPPET_TARGET_CHARS);
    }
    // Ajusta para fronteiras de palavra sem sair da faixa de tamanho.
    if from > 0 {
        if let Some(sp) = (from..at.min(from + 40)).find(|&i| chars[i].1 == ' ') {
            from = sp + 1;
        }
    }
    if to < total {
        if let Some(sp) = (to.saturating_sub(40).max(from + SNIPPET_MIN_CHARS)..to).rev().find(|&i| chars[i].1 == ' ') {
            to = sp;
        }
    }
    let begin = chars[from].0;
    let end = chars.get(to).map_or(flat.len(), |(b, _)| *b);
    let mut out = String::new();
    if from > 0 {
        out.push_str("...");
    }
    out.push_str(flat[begin..end].trim());
    if to < total {
        out.push_str("...");
    }
    out
}

// --- montagem preguiçosa por projeto ---

/// Índice montado de um projeto e a impressão do manifesto que o gerou.
#[derive(Default)]
struct Slot {
    current: Option<(String, Arc<Index>)>,
    builds: u64,
}

/// Um lugar por raiz de projeto; cada lugar tem a própria trava.
static CACHE: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<Slot>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn slot_for(root: &Path) -> Arc<Mutex<Slot>> {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.entry(root.to_path_buf()).or_default().clone()
}

/// Lê as notas `ready` do manifesto (nota ausente ou ilegível fica de fora) e monta o índice.
fn build_from_disk(root: &Path, manifest: &Manifest) -> Index {
    let mut docs = Vec::new();
    for entry in manifest.docs.iter().filter(|d| d.status == Status::Ready) {
        if let Ok(Some(note)) = read_note_file(root, &entry.id) {
            let body = note_body(&note).to_string();
            docs.push((entry.clone(), body));
        }
    }
    Index::build(docs)
}

/// O índice do projeto em `root`: montado na primeira chamada e remontado quando o manifesto
/// muda (SHA-256 do arquivo). Sem manifesto, índice vazio.
pub fn index_for(root: &Path) -> Result<Arc<Index>, KbError> {
    // O manifesto é lido já sob a trava do lugar: uma montagem mais lenta não troca um índice
    // novo pelo antigo.
    let slot = slot_for(root);
    let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
    let text = match safe_fs::read_regular(root, MANIFEST_REL, MAX_MANIFEST_BYTES) {
        Ok(text) => text,
        Err(SafeFsError::Missing) => String::new(),
        Err(SafeFsError::Invalid) => return Err(KbError::Corrupt),
        Err(e) => return Err(KbError::Fs(e)),
    };
    let fingerprint = sha256_hex(text.as_bytes());
    if let Some((fp, index)) = &slot.current {
        if *fp == fingerprint {
            return Ok(index.clone());
        }
    }
    let manifest: Manifest = if text.is_empty() { Manifest::default() } else { serde_json::from_str(&text).map_err(|_| KbError::Corrupt)? };
    let index = Arc::new(build_from_disk(root, &manifest));
    slot.current = Some((fingerprint, index.clone()));
    slot.builds += 1;
    Ok(index)
}

/// Esquece o índice de um projeto (projeto removido).
pub fn forget(root: &Path) {
    CACHE.lock().unwrap_or_else(PoisonError::into_inner).remove(root);
}

/// `true` se há um lugar no cache para `root` (testes).
#[cfg(test)]
pub(crate) fn is_cached(root: &Path) -> bool {
    CACHE.lock().unwrap_or_else(PoisonError::into_inner).contains_key(root)
}

/// Quantas vezes o índice de `root` foi montado (testes).
#[cfg(test)]
pub(crate) fn builds(root: &Path) -> u64 {
    slot_for(root).lock().unwrap_or_else(PoisonError::into_inner).builds
}

/// Busca na base do projeto (ver [`Index::search`]).
pub fn search(root: &Path, query: &str, max: usize, secrets: &SecretSet) -> Result<Vec<Hit>, KbError> {
    Ok(index_for(root)?.search(query, max, secrets))
}

/// Busca na base do projeto só nos documentos com a marcação `role` (ver [`Index::search_role`]).
pub fn search_role(root: &Path, query: &str, max: usize, role: Option<Role>, secrets: &SecretSet) -> Result<Vec<Hit>, KbError> {
    Ok(index_for(root)?.search_role(query, max, role, secrets))
}

/// Seção inteira de um documento (ver [`Index::section`]). Documento fora do índice ou seção
/// inexistente → [`KbError::NotFound`].
pub fn read_section(root: &Path, doc_id: &str, section: &str, secrets: &SecretSet) -> Result<SectionText, KbError> {
    index_for(root)?.section(doc_id, section, secrets).ok_or(KbError::NotFound)
}

/// Seções de um documento (ver [`Index::sections`]). Documento fora do índice →
/// [`KbError::NotFound`].
pub fn list_sections(root: &Path, doc_id: &str, secrets: &SecretSet) -> Result<Vec<SectionInfo>, KbError> {
    index_for(root)?.sections(doc_id, secrets).ok_or(KbError::NotFound)
}

/// Texto inteiro de um documento (ver [`Index::doc_text`]). Documento fora do índice →
/// [`KbError::NotFound`].
pub fn doc_text(root: &Path, doc_id: &str, secrets: &SecretSet) -> Result<String, KbError> {
    index_for(root)?.doc_text(doc_id, secrets).ok_or(KbError::NotFound)
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
