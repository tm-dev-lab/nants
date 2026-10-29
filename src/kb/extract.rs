//! Extração da base de conhecimento (spec 134/T02): funções puras, sem I/O, que transformam o
//! original (markdown, texto, PDF ou a página de um link) no corpo da nota, na descrição de uma
//! linha (C-19) e nos alvos dos links entre documentos (C-22).
//!
//! - **Markdown e texto:** final de linha normalizado (`\r\n` e `\r` viram `\n`); o frontmatter
//!   de origem fica no corpo, intocado, e dele saem `title` e `description`/`summary`.
//! - **PDF:** biblioteca `unpdf` (Rust puro, MIT), escolhida por medição em 20 PDFs reais (números
//!   na tarefa T02), em modo sequencial (sem usar todos os núcleos). Cada página vira a seção
//!   `# p. N`. Página ilegível (pouco texto, ou mais símbolo que letra, típico de PDF escaneado ou
//!   de fonte sem mapa de caracteres) fica sem texto; com menos da metade das páginas legíveis, o
//!   documento fica `no_text`.
//! - **Link:** HTML pelo [`crate::harness::web::html_to_text`] (títulos, listas, `pre` e só o
//!   conteúdo principal, C-23); texto e markdown como estão; PDF pelo mesmo extrator. Página sem
//!   texto (que depende de JavaScript) fica `no_text`.
//!
//! A descrição nunca é gerada por modelo: frontmatter, `<meta name="description">` ou a primeira
//! frase do texto, nessa ordem.

use super::{clean_url, one_line, Status, MAX_DESCRIPTION_CHARS, MAX_TITLE_CHARS, MAX_UPLOAD_BYTES};
use crate::harness::web::{html_meta, html_to_text, textual, Page};

/// Mínimo de letras de uma página (do PDF ou do link) para contar como texto.
pub const MIN_PAGE_LETTERS: usize = 40;
/// Teto de alvos de link guardados por documento.
pub const MAX_LINK_TARGETS: usize = 256;
/// Nome e versão do extrator de PDF, gravados no `extractor` da nota.
pub const PDF_EXTRACTOR: &str = "unpdf-0.51";
/// Extrator do HTML dos links.
pub const HTML_EXTRACTOR: &str = "html";
/// Extrator de markdown e texto.
pub const RAW_EXTRACTOR: &str = "raw";

/// Motivos codificados de `no_text` e `error` (a interface traduz pelo código).
pub mod reason {
    /// PDF com pouco texto por página: provavelmente escaneado (ou com fontes sem mapa de texto).
    pub const PDF_NO_TEXT: &str = "kb_pdf_no_text";
    /// PDF protegido por senha.
    pub const PDF_ENCRYPTED: &str = "kb_pdf_encrypted";
    /// PDF que a biblioteca não conseguiu ler (corrompido ou fora do padrão).
    pub const PDF_UNREADABLE: &str = "kb_pdf_unreadable";
    /// Página do link sem texto (provavelmente depende de JavaScript).
    pub const PAGE_NO_TEXT: &str = "kb_link_no_text";
    /// Link para conteúdo que não é HTML, texto nem PDF.
    pub const LINK_TYPE: &str = "kb_link_type_unsupported";
    /// PDF do link acima do teto de leitura de páginas.
    pub const LINK_TOO_LARGE: &str = "kb_link_too_large";
    /// Link para a rede local ou privada (bloqueado pelo cliente web).
    pub const LINK_BLOCKED: &str = "kb_link_blocked";
    /// Link que respondeu com erro HTTP.
    pub const LINK_HTTP: &str = "kb_link_http_error";
    /// Link que não respondeu (DNS, conexão, tempo, redirecionamentos demais).
    pub const LINK_UNREACHABLE: &str = "kb_link_unreachable";
    /// O texto extraído traz uma chave privada ou um segredo configurado.
    pub const SECRET: &str = "kb_secret_content";
    /// A extração passou do tempo máximo por arquivo.
    pub const TIMEOUT: &str = "kb_extract_timeout";
    /// Falha interna da extração (pânico da biblioteca, original ausente ou ilegível).
    pub const FAILED: &str = "kb_extract_failed";

    /// Todos os motivos: cada um tem o texto `kb.reason.<código>` nos dicionários do servidor e
    /// da web, nos dois idiomas.
    pub const ALL: [&str; 12] = [
        PDF_NO_TEXT, PDF_ENCRYPTED, PDF_UNREADABLE, PAGE_NO_TEXT, LINK_TYPE, LINK_TOO_LARGE, LINK_BLOCKED, LINK_HTTP,
        LINK_UNREACHABLE, SECRET, TIMEOUT, FAILED,
    ];
}

/// Resultado da extração de um documento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// `Ready` ou `NoText`.
    pub status: Status,
    /// Motivo, quando `NoText`.
    pub reason: Option<&'static str>,
    /// Corpo da nota (markdown).
    pub body: String,
    /// Título achado no conteúdo (frontmatter ou `<title>`).
    pub title: Option<String>,
    /// Descrição de uma linha extraída (C-19).
    pub description: Option<String>,
    /// Páginas, para PDF.
    pub pages: Option<u32>,
    pub extractor: &'static str,
    /// Alvos de link (`url:<endereço>` ou `file:<nome em minúsculas>`), C-22.
    pub link_targets: Vec<String>,
}

/// O original de um link, para gravar em `sources/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSource {
    pub ext: &'static str,
    pub bytes: Vec<u8>,
}

// --- texto ---

/// Final de linha normalizado: `\r\n` e `\r` viram `\n`.
pub fn normalize_newlines(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

/// Frontmatter YAML no início (`---` ... `---`): o bloco sem os delimitadores e o resto.
pub fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, text);
    };
    if let Some(end) = rest.find("\n---\n") {
        return (Some(&rest[..end]), &rest[end + 5..]);
    }
    match rest.strip_suffix("\n---") {
        Some(block) => (Some(block), ""),
        None => (None, text),
    }
}

/// Campo de texto do frontmatter (o primeiro de `keys` que existir e não for vazio).
fn frontmatter_field(block: &str, keys: &[&str]) -> Option<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(block).ok()?;
    let map = value.as_mapping()?;
    keys.iter().find_map(|key| {
        let text = map.get(serde_yaml::Value::String((*key).to_string()))?.as_str()?;
        let line = one_line(text, MAX_DESCRIPTION_CHARS.max(MAX_TITLE_CHARS));
        (!line.is_empty()).then_some(line)
    })
}

/// `[texto](alvo)` → `texto`; marcas de ênfase e de código saem.
fn strip_inline_markdown(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| after[mid + 2..].find(')').map(|close| (mid, mid + 2 + close))) {
            Some((mid, close)) => {
                out.push_str(&after[..mid]);
                rest = &after[close + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.chars().filter(|c| !matches!(c, '*' | '`')).collect()
}

/// Corta a descrição em até [`MAX_DESCRIPTION_CHARS`], numa fronteira de palavra.
fn fit_description(text: &str) -> String {
    let line = one_line(text, usize::MAX);
    if line.chars().count() <= MAX_DESCRIPTION_CHARS {
        return line;
    }
    let cut: String = line.chars().take(MAX_DESCRIPTION_CHARS).collect();
    match cut.rfind(' ') {
        Some(p) if p > MAX_DESCRIPTION_CHARS / 2 => cut[..p].trim_end_matches([',', ';', ':']).to_string(),
        _ => cut,
    }
}

/// Primeira frase do texto (C-19): o primeiro parágrafo que não é título, código, tabela nem
/// imagem, até o primeiro `.`, `!` ou `?` seguido de espaço.
pub fn first_sentence(text: &str) -> Option<String> {
    let mut in_fence = false;
    let mut paragraph: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if in_fence {
            continue;
        }
        let skip = line.is_empty()
            || line.starts_with('#')
            || line.starts_with('|')
            || line.starts_with("![")
            || line.starts_with("<!--")
            || line.starts_with("---");
        if skip {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        let item = line.trim_start_matches(['>', '-', '*', '+']).trim_start();
        let item = item.split_once(". ").filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).map_or(item, |(_, t)| t);
        let clean = strip_inline_markdown(item);
        if clean.chars().any(char::is_alphabetic) {
            paragraph.push(clean);
        } else if !paragraph.is_empty() {
            break;
        }
    }
    let joined = one_line(&paragraph.join(" "), usize::MAX);
    if joined.is_empty() {
        return None;
    }
    let bytes = joined.as_bytes();
    let end = bytes
        .windows(2)
        .position(|w| matches!(w[0], b'.' | b'!' | b'?') && w[1] == b' ')
        .map_or(joined.len(), |p| p + 1);
    Some(fit_description(&joined[..end]))
}

/// Remove caracteres de controle (menos `\n`), o caractere de substituição, a área de uso privado
/// e desfaz as ligaduras latinas (`ﬁ` vira `fi`), que atrapalham a busca.
pub fn clean_extracted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in normalize_newlines(text).chars() {
        match c {
            '\n' => out.push('\n'),
            '\t' => out.push(' '),
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            '\u{FFFD}' | '\u{E000}'..='\u{F8FF}' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    let mut lines: Vec<&str> = Vec::new();
    for line in out.lines().map(str::trim_end) {
        if line.is_empty() && lines.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n").trim().to_string()
}

/// `true` se o texto bruto de uma página conta como texto: ao menos [`MIN_PAGE_LETTERS`] letras
/// e letras com dígitos em ao menos metade dos caracteres visíveis (controle, substituição e
/// símbolos soltos contam contra).
pub fn readable(raw: &str) -> bool {
    let mut letters = 0usize;
    let mut alnum = 0usize;
    let mut visible = 0usize;
    for c in raw.chars() {
        if c == '\n' || c == ' ' || c == '\r' {
            continue;
        }
        visible += 1;
        if c.is_alphabetic() {
            letters += 1;
            alnum += 1;
        } else if c.is_numeric() {
            alnum += 1;
        }
    }
    letters >= MIN_PAGE_LETTERS && alnum * 2 >= visible
}

// --- links (C-22) ---

/// Decodifica `%XX` (UTF-8 com perda); sequência inválida fica como está.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(hi), Some(lo)) => {
                out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Alvo de um link de markdown: endereço http(s) (`url:`) ou nome de arquivo da base (`file:`).
fn markdown_target(raw: &str) -> Option<String> {
    let target = raw.trim().trim_start_matches('<').trim_end_matches('>');
    let target = target.split_whitespace().next()?;
    if target.starts_with('#') {
        return None;
    }
    let lower = target.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return clean_url(target).ok().map(|u| format!("url:{u}"));
    }
    if lower.contains(':') {
        return None; // mailto:, file:, javascript: e afins
    }
    let path = target.split(['#', '?']).next()?;
    let name = percent_decode(path.rsplit(['/', '\\']).next()?).trim().to_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e)?;
    matches!(ext, "md" | "markdown" | "txt" | "text" | "pdf").then(|| format!("file:{name}"))
}

/// Alvos dos links de um markdown: `[texto](alvo)` e definições `[ref]: alvo`.
pub fn markdown_links(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |target: Option<String>| {
        if let Some(t) = target {
            if out.len() < MAX_LINK_TARGETS && !out.contains(&t) {
                out.push(t);
            }
        }
    };
    let mut rest = text;
    while let Some(p) = rest.find("](") {
        let after = &rest[p + 2..];
        let end = after.find([')', '\n']).unwrap_or(after.len());
        push(markdown_target(&after[..end]));
        rest = &after[end..];
    }
    for line in text.lines() {
        let line = line.trim_start();
        if let Some((label, target)) = line.strip_prefix('[').and_then(|l| l.split_once("]:")) {
            if !label.is_empty() && !label.contains(']') {
                push(markdown_target(target));
            }
        }
    }
    out
}

/// Alvos dos links de uma página: `href` resolvido contra o endereço final, só http(s), sem o
/// fragmento e sem a própria página.
fn page_links(base: &str, hrefs: &[String]) -> Vec<String> {
    let Ok(base_url) = reqwest::Url::parse(base) else {
        return Vec::new();
    };
    let own = clean_url(base).ok();
    let mut out: Vec<String> = Vec::new();
    for href in hrefs {
        let Ok(joined) = base_url.join(href) else { continue };
        let Ok(url) = clean_url(joined.as_str()) else { continue };
        if own.as_deref() == Some(url.as_str()) {
            continue;
        }
        let target = format!("url:{url}");
        if !out.contains(&target) {
            out.push(target);
        }
        if out.len() >= MAX_LINK_TARGETS {
            break;
        }
    }
    out
}

// --- formatos ---

/// Markdown ou texto já validado como UTF-8: corpo normalizado, título e descrição do
/// frontmatter (ou a primeira frase) e os links.
pub fn text(raw: &str, markdown: bool) -> Extracted {
    let body = normalize_newlines(raw.strip_prefix('\u{feff}').unwrap_or(raw));
    let (front, content) = if markdown { split_frontmatter(&body) } else { (None, body.as_str()) };
    let title = front.and_then(|f| frontmatter_field(f, &["title"])).map(|t| one_line(&t, MAX_TITLE_CHARS));
    let description = front
        .and_then(|f| frontmatter_field(f, &["description", "summary"]))
        .map(|d| fit_description(&d))
        .or_else(|| first_sentence(content));
    let link_targets = if markdown { markdown_links(content) } else { Vec::new() };
    Extracted {
        status: Status::Ready,
        reason: None,
        body,
        title,
        description,
        pages: None,
        extractor: RAW_EXTRACTOR,
        link_targets,
    }
}

/// PDF com texto: uma seção `# p. N` por página. `Err` traz o motivo codificado.
pub fn pdf(bytes: &[u8]) -> Result<Extracted, &'static str> {
    let options = unpdf::ParseOptions::new().sequential().with_resources(false);
    let doc = unpdf::parse_bytes_with_options(bytes, options).map_err(|e| match e.kind() {
        unpdf::ErrorKind::Encrypted | unpdf::ErrorKind::InvalidPassword => reason::PDF_ENCRYPTED,
        _ => reason::PDF_UNREADABLE,
    })?;
    let total = doc.pages.len();
    if total == 0 {
        // Sem nenhuma página reconhecida (arquivo cortado, árvore de páginas quebrada).
        return Err(reason::PDF_UNREADABLE);
    }
    let mut body = String::new();
    let mut readable_pages = 0usize;
    let mut first_text: Option<String> = None;
    for (i, page) in doc.pages.iter().enumerate() {
        let number = if page.number > 0 { page.number as usize } else { i + 1 };
        let raw = page.plain_text();
        body.push_str(&format!("# p. {number}\n\n"));
        if readable(&raw) {
            readable_pages += 1;
            let text = clean_extracted(&raw);
            if first_text.is_none() {
                first_text = Some(text.clone());
            }
            body.push_str(&text);
            body.push_str("\n\n");
        }
        if body.len() > MAX_UPLOAD_BYTES {
            break;
        }
    }
    let mut body = body.trim_end().to_string();
    body.push('\n');
    let mut end = body.len().min(MAX_UPLOAD_BYTES);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    body.truncate(end);
    let no_text = readable_pages * 2 < total;
    Ok(Extracted {
        status: if no_text { Status::NoText } else { Status::Ready },
        reason: no_text.then_some(reason::PDF_NO_TEXT),
        description: if no_text { None } else { first_text.as_deref().and_then(first_sentence) },
        body,
        title: None,
        pages: u32::try_from(total).ok(),
        extractor: PDF_EXTRACTOR,
        link_targets: Vec::new(),
    })
}

/// `charset` do `Content-Type` ou do `<meta charset>` no início do HTML, em minúsculas.
fn charset(content_type: &str, body: &[u8]) -> Option<String> {
    let from_header = content_type
        .split(';')
        .filter_map(|p| p.trim().strip_prefix("charset="))
        .map(|c| c.trim_matches(['"', '\'']).to_ascii_lowercase())
        .next();
    from_header.or_else(|| {
        let head = String::from_utf8_lossy(&body[..body.len().min(2048)]).to_ascii_lowercase();
        let at = head.find("charset=")?;
        let value: String = head[at + 8..]
            .trim_start_matches(['"', '\''])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        (!value.is_empty()).then_some(value)
    })
}

/// Bytes como texto: Latin-1 e Windows-1252 byte a byte; o resto como UTF-8 (com perda).
fn decode(content_type: &str, body: &[u8]) -> String {
    match charset(content_type, body).as_deref() {
        Some("iso-8859-1" | "latin1" | "latin-1" | "windows-1252" | "cp1252" | "us-ascii") => {
            body.iter().map(|&b| char::from(b)).collect()
        }
        _ => String::from_utf8_lossy(body).into_owned(),
    }
}

/// Página lida de um link: o resultado da extração e o original para `sources/`.
pub fn page(page: &Page) -> Result<(Extracted, LinkSource), &'static str> {
    let media = page.content_type.split(';').next().unwrap_or("").trim().to_string();
    let looks_pdf = media == "application/pdf" || (media.is_empty() && page.body.starts_with(b"%PDF-"));
    if looks_pdf {
        if page.truncated {
            return Err(reason::LINK_TOO_LARGE);
        }
        let extracted = pdf(&page.body)?;
        return Ok((extracted, LinkSource { ext: "pdf", bytes: page.body.clone() }));
    }
    let head = String::from_utf8_lossy(&page.body[..page.body.len().min(512)]).to_ascii_lowercase();
    let is_html = media == "text/html"
        || media == "application/xhtml+xml"
        || (media.is_empty() && (head.contains("<html") || head.contains("<!doctype html")));
    let is_text = !is_html && (textual(&media) || (media.is_empty() && !page.body.contains(&0)));
    if !is_html && !is_text {
        return Err(reason::LINK_TYPE);
    }
    let raw = decode(&page.content_type, &page.body);
    if is_html {
        let meta = html_meta(&raw);
        let body = clean_extracted(&html_to_text(&raw));
        let no_text = !readable(&body);
        let description = meta.description.as_deref().map(fit_description).or_else(|| first_sentence(&body));
        let extracted = Extracted {
            status: if no_text { Status::NoText } else { Status::Ready },
            reason: no_text.then_some(reason::PAGE_NO_TEXT),
            description: if no_text { None } else { description },
            title: meta.title.map(|t| one_line(&t, MAX_TITLE_CHARS)).filter(|t| !t.is_empty()),
            body: if body.is_empty() { String::new() } else { format!("{body}\n") },
            pages: None,
            extractor: HTML_EXTRACTOR,
            link_targets: page_links(&page.url, &meta.links),
        };
        return Ok((extracted, LinkSource { ext: "html", bytes: page.body.clone() }));
    }
    let markdown = media == "text/markdown" || media == "text/x-markdown";
    let mut extracted = text(&raw, markdown);
    if !readable(&extracted.body) {
        extracted.status = Status::NoText;
        extracted.reason = Some(reason::PAGE_NO_TEXT);
        extracted.description = None;
    }
    let ext = if markdown { "md" } else { "txt" };
    Ok((extracted, LinkSource { ext, bytes: page.body.clone() }))
}

#[cfg(test)]
#[path = "extract_tests.rs"]
mod tests;
