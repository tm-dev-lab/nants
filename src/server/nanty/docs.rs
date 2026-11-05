//! A documentação do Nants para o Nanty (spec 141 T09, G7, C-09): `nants_docs(query)`.
//!
//! - **Fonte:** o README, o ARCHITECTURE e o SECURITY do próprio Nants, embutidos no binário por
//!   `include_str!`. São da versão que está rodando, escritos pelo time do Nants: fonte confiável, que
//!   não contamina a procedência do turno (nada aqui vem de projeto, de terceiro ou da internet).
//! - **Divisão:** cada documento é dividido pelos cabeçalhos Markdown (fora de bloco de código), e as
//!   seções longas viram trechos de até [`MAX_CHUNK_CHARS`], cortados entre parágrafos. Cada trecho
//!   leva o caminho dos títulos (`Nanty > Voice`).
//! - **Busca:** por termos, sem acento e sem caixa. O termo no título pesa mais que no corpo, e quem
//!   casa mais termos diferentes vem primeiro. Sem termo útil ou sem resultado, volta o índice das
//!   seções, para o modelo refazer a busca com as palavras dos títulos.
//! - **Teto:** até [`MAX_RESULTS`] trechos e [`MAX_TOTAL_CHARS`] caracteres no total (cerca de 1500
//!   tokens). Os três documentos têm perto de 150 KB (uns 37 mil tokens): mandar tudo em cada turno foi
//!   a sobra apontada no C-13. Um trecho de 1500 caracteres cabe um subtítulo inteiro do README na
//!   maioria dos casos, e quatro trechos respondem a uma pergunta sem pesar no orçamento do dia.
//! - **Nunca entram:** endereço (URL) e IP. Os documentos são públicos e não têm segredo nem caminho
//!   do usuário (o teste confere); mesmo assim, a saída passa pelo redator único na ferramenta, e os
//!   endereços saem aqui, porque o Nanty nunca escreve endereço à pessoa.

use std::sync::OnceLock;

use serde_json::{json, Value};

/// O nome da ferramenta.
pub const NANTS_DOCS: &str = "nants_docs";
/// Trechos devolvidos por busca, no máximo.
pub const MAX_RESULTS: usize = 4;
/// Tamanho de um trecho, em caracteres.
pub const MAX_CHUNK_CHARS: usize = 1_500;
/// Teto de texto devolvido por busca, em caracteres.
pub const MAX_TOTAL_CHARS: usize = 6_000;
/// Termos da busca considerados, no máximo.
const MAX_TERMS: usize = 12;
/// Linhas do índice quando nada casa.
const MAX_INDEX: usize = 120;
/// O que fica no lugar de um endereço e de um IP.
const URL_MARK: &str = "[link]";
const IP_MARK: &str = "[ip]";

/// Os documentos embutidos: o nome e o texto.
pub const SOURCES: [(&str, &str); 3] = [
    ("README.md", include_str!("../../../README.md")),
    ("ARCHITECTURE.md", include_str!("../../../ARCHITECTURE.md")),
    ("SECURITY.md", include_str!("../../../SECURITY.md")),
];

/// Palavras sem peso na busca, nos dois idiomas da conversa.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "how", "what", "does", "with", "that", "this", "from", "are", "can", "not", "when", "who", "why",
    "como", "que", "para", "por", "uma", "com", "não", "nao", "dos", "das", "ele", "ela", "isso", "esse", "essa", "qual",
    "quais", "onde", "quando", "nants",
];

/// Um trecho de documento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub doc: &'static str,
    /// O caminho dos títulos, como `Nanty > Voice`.
    pub section: String,
    /// A parte e o total da seção (`1/1` numa seção curta).
    pub part: (usize, usize),
    pub text: String,
    /// Título e texto sem acento e sem caixa, para a busca.
    title_key: String,
    text_key: String,
}

/// Os trechos de todos os documentos (montados uma vez).
pub fn chunks() -> &'static [Chunk] {
    static CHUNKS: OnceLock<Vec<Chunk>> = OnceLock::new();
    CHUNKS.get_or_init(|| SOURCES.iter().flat_map(|(doc, text)| split(doc, text)).collect())
}

/// A linha é um cabeçalho Markdown: o nível e o título.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    let rest = line.get(level..)?;
    ((1..=6).contains(&level) && rest.starts_with(' ')).then(|| (level, rest.trim()))
}

/// Divide um documento em trechos, pelos cabeçalhos fora de bloco de código.
fn split(doc: &'static str, text: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut titles: Vec<(usize, String)> = Vec::new();
    let mut body: Vec<&str> = Vec::new();
    let mut fence = false;
    let flush = |titles: &[(usize, String)], body: &mut Vec<&str>, out: &mut Vec<Chunk>| {
        let section = titles.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > ");
        let parts = pack(&body.join("\n"));
        let total = parts.len();
        for (i, part) in parts.into_iter().enumerate() {
            let text = clean(&part);
            out.push(Chunk { doc, title_key: fold(&section), text_key: fold(&text), section: section.clone(), part: (i + 1, total), text });
        }
        body.clear();
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
        }
        match heading(line).filter(|_| !fence) {
            Some((level, title)) => {
                flush(&titles, &mut body, &mut out);
                titles.retain(|(l, _)| *l < level);
                titles.push((level, title.to_string()));
            }
            None => body.push(line),
        }
    }
    flush(&titles, &mut body, &mut out);
    out
}

/// Junta os parágrafos em partes de até [`MAX_CHUNK_CHARS`]; um parágrafo maior é cortado por linha,
/// e uma linha maior, por caractere. Parte vazia não existe.
fn pack(body: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let push = |piece: &str, current: &mut String, parts: &mut Vec<String>| {
        let sep = if current.is_empty() { 0 } else { 2 };
        if !current.is_empty() && current.chars().count() + sep + piece.chars().count() > MAX_CHUNK_CHARS {
            parts.push(std::mem::take(current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(piece);
    };
    for paragraph in body.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if paragraph.chars().count() <= MAX_CHUNK_CHARS {
            push(paragraph, &mut current, &mut parts);
            continue;
        }
        for line in paragraph.lines() {
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(MAX_CHUNK_CHARS) {
                let piece: String = piece.iter().collect();
                if current.chars().count() + 1 + piece.chars().count() > MAX_CHUNK_CHARS && !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
                if !current.is_empty() {
                    current.push('\n');
                }
                current.push_str(&piece);
            }
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

/// Tira endereços e IPs do texto. No link Markdown `[texto](endereço)` fica só o texto.
pub fn clean(text: &str) -> String {
    strip_ips(&strip_urls(text))
}

fn strip_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = ["http://", "https://"].iter().filter_map(|p| rest.find(p)).min() {
        let (before, from) = rest.split_at(at);
        let end = from.find(|c: char| c.is_whitespace() || matches!(c, ')' | '>' | '"' | '\'' | '`' | ']')).unwrap_or(from.len());
        // `[texto](url)`: some o `(url)` inteiro.
        if before.ends_with("](") && from[end..].starts_with(')') {
            out.push_str(&before[..before.len() - 1]);
            rest = &from[end + 1..];
        } else {
            out.push_str(before);
            out.push_str(URL_MARK);
            rest = &from[end..];
        }
    }
    out.push_str(rest);
    out
}

/// Troca os IPv4 (quatro grupos de 1 a 3 dígitos até 255, fora de um número maior) pela marca.
fn strip_ips(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut copied = 0;
    while i < bytes.len() {
        let starts = bytes[i].is_ascii_digit() && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'.'));
        if starts {
            if let Some(len) = ipv4_at(&bytes[i..]) {
                out.push_str(&text[copied..i]);
                out.push_str(IP_MARK);
                i += len;
                copied = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// O tamanho do IPv4 que começa em `b`, se houver.
fn ipv4_at(b: &[u8]) -> Option<usize> {
    let mut i = 0;
    for group in 0..4 {
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() && i - start < 4 {
            i += 1;
        }
        let digits = i - start;
        if !(1..=3).contains(&digits) {
            return None;
        }
        let value: u32 = std::str::from_utf8(&b[start..i]).ok()?.parse().ok()?;
        if value > 255 {
            return None;
        }
        if group < 3 {
            if b.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
    }
    let after = b.get(i);
    (after.is_none_or(|c| !(c.is_ascii_alphanumeric()) && !(*c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)))).then_some(i)
}

/// Sem acento e sem caixa (as letras do português), para a busca.
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

/// Os termos da busca: sem acento, sem caixa, com 3 letras ou mais, fora das palavras sem peso.
pub fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for term in fold(query).split(|c: char| !c.is_alphanumeric()).filter(|t| t.chars().count() >= 3) {
        if !STOPWORDS.contains(&term) && !out.iter().any(|t| t == term) {
            out.push(term.to_string());
        }
        if out.len() >= MAX_TERMS {
            break;
        }
    }
    out
}

/// A nota de um trecho: termo no título vale 5, cada ocorrência no corpo vale 1 (até 10 por termo), e
/// cada termo diferente que casa soma 3. Zero quando nada casa.
fn score(chunk: &Chunk, terms: &[String]) -> usize {
    let mut total = 0;
    let mut matched = 0;
    for term in terms {
        let in_title = chunk.title_key.matches(term.as_str()).count();
        let in_text = chunk.text_key.matches(term.as_str()).count().min(10);
        if in_title + in_text > 0 {
            matched += 1;
            total += in_title * 5 + in_text;
        }
    }
    if matched == 0 {
        0
    } else {
        total + matched * 3
    }
}

/// O índice das seções (sem repetir), para refazer a busca.
fn index() -> Vec<Value> {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for chunk in chunks() {
        if !chunk.section.is_empty() && !seen.contains(&(chunk.doc, chunk.section.as_str())) {
            seen.push((chunk.doc, chunk.section.as_str()));
        }
    }
    seen.into_iter().take(MAX_INDEX).map(|(doc, section)| json!({ "doc": doc, "section": section })).collect()
}

/// `nants_docs(query)`: os trechos que mais casam com a busca, com o teto; sem resultado, o índice.
pub fn search(query: &str) -> Value {
    let terms = terms(query);
    let mut ranked: Vec<(usize, usize)> =
        chunks().iter().enumerate().map(|(i, c)| (score(c, &terms), i)).filter(|(s, _)| *s > 0).collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut results = Vec::new();
    let mut left = MAX_TOTAL_CHARS;
    for (_, i) in ranked.iter().take(MAX_RESULTS) {
        if left == 0 {
            break;
        }
        let chunk = &chunks()[*i];
        let text: String = chunk.text.chars().take(left).collect();
        left = left.saturating_sub(text.chars().count());
        let cut = text.chars().count() < chunk.text.chars().count();
        let mut item = json!({
            "doc": chunk.doc,
            "section": chunk.section,
            "part": format!("{}/{}", chunk.part.0, chunk.part.1),
            "text": text,
        });
        if cut {
            item["truncated"] = json!(true);
        }
        results.push(item);
    }
    let mut out = json!({ "source": "nants_docs", "version": env!("CARGO_PKG_VERSION"), "results": results });
    if ranked.is_empty() {
        out["sections"] = Value::Array(index());
    } else if ranked.len() > MAX_RESULTS {
        out["more_matches"] = json!(ranked.len() - MAX_RESULTS);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Os documentos embutidos não têm segredo, caminho do usuário nem endereço na saída.
    #[test]
    fn embedded_docs_have_no_secret_nor_user_path_and_the_output_has_no_address() {
        let pem = format!("{}{}", "-----BEGIN", " ");
        let key_prefix = format!("{}{}", "sk", "-");
        for (doc, text) in SOURCES {
            assert!(!text.is_empty(), "{doc} vazio");
            assert!(!text.contains("/home/") && !text.contains("/Users/") && !text.contains("C:\\Users"), "{doc} com caminho de usuário");
            assert!(!text.contains(&pem), "{doc} com chave PEM");
            for (i, _) in text.match_indices(&key_prefix) {
                let tail: String = text[i + key_prefix.len()..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                assert!(tail.len() < 16, "{doc} com cara de chave: {tail}");
            }
            assert_eq!(crate::server::nanty::redact::patterns(text), text, "{doc}: o redator mudaria o texto");
        }
        for chunk in chunks() {
            assert!(!chunk.text.contains("http://") && !chunk.text.contains("https://"), "{}: {}", chunk.section, chunk.text);
            assert!(!chunk.text.contains("127.0.0.1") && !chunk.text.contains("0.0.0.0"), "{}: {}", chunk.section, chunk.text);
            assert!(chunk.text.chars().count() <= MAX_CHUNK_CHARS, "{} grande demais", chunk.section);
            assert!(!chunk.text.trim().is_empty());
        }
    }

    #[test]
    fn sections_follow_the_markdown_headings_outside_code_blocks() {
        let text = "# Top\nintro\n## A\nalfa\n```\n# not a heading\n```\n### A1\nfundo\n## B\nbeta\n";
        let chunks = split("X.md", text);
        let sections: Vec<&str> = chunks.iter().map(|c| c.section.as_str()).collect();
        assert_eq!(sections, ["Top", "Top > A", "Top > A > A1", "Top > B"]);
        assert!(chunks[1].text.contains("# not a heading"));
        // Uma seção longa vira várias partes, nenhuma acima do teto.
        let long = format!("## L\n{}", (0..400).map(|i| format!("Paragraph {i} about recovery.\n")).collect::<Vec<_>>().join("\n"));
        let parts = split("X.md", &long);
        assert!(parts.len() > 1 && parts.iter().all(|c| c.text.chars().count() <= MAX_CHUNK_CHARS && c.part.1 == parts.len()));
    }

    #[test]
    fn addresses_and_ips_leave_the_text_and_link_text_stays() {
        assert_eq!(clean("see [the docs](https://example.org/a) now"), "see [the docs] now");
        assert_eq!(clean("open http://127.0.0.1:3000/x or 192.168.0.68."), "open [link] or [ip].");
        assert_eq!(clean("version 0.1.0 and 1.2.3.4.5 stay"), "version 0.1.0 and 1.2.3.4.5 stay");
        assert_eq!(clean("host 10.0.0.300 is no ip"), "host 10.0.0.300 is no ip");
    }

    #[test]
    fn search_finds_the_section_by_terms_without_accents_and_caps_the_size() {
        let out = search("Recuperação recovery escalate");
        let results = out["results"].as_array().expect("lista");
        assert!(!results.is_empty() && results.len() <= MAX_RESULTS, "{out}");
        let total: usize = results.iter().map(|r| r["text"].as_str().map_or(0, |t| t.chars().count())).sum();
        assert!(total <= MAX_TOTAL_CHARS, "{total}");
        let voice = search("Telegram voice note");
        assert!(voice["results"][0]["section"].as_str().is_some_and(|s| s.contains("Voice") || s.contains("Telegram")), "{voice}");
        // Nada casa: o índice das seções, para buscar de novo.
        let none = search("zzqqxx");
        assert!(none["results"].as_array().is_some_and(Vec::is_empty));
        assert!(none["sections"].as_array().is_some_and(|s| s.iter().any(|v| v["section"] == "Nants architecture > Nanty")), "{none}");
        assert_eq!(terms("Como o Nants faz a recuperação?"), ["faz", "recuperacao"]);
    }
}
