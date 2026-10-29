//! Acesso à web do Arquiteto (spec 076): `fetch_url` e `package_versions`.
//!
//! Proteção de rede: o DNS é resolvido aqui, QUALQUER endereço não público recusa a URL e a conexão
//! é feita nos IPs validados (`resolve_to_addrs`), sem nova consulta DNS (anti rebinding).
//! Redirecionamentos são seguidos manualmente (máx. 5) revalidando cada destino. Sem proxy.
//! Em testes, `allow_loopback` (só `cfg(test)`) libera 127.0.0.0/8 para servidores locais.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::Url;

/// Teto de redirecionamentos seguidos.
pub const MAX_REDIRECTS: usize = 5;
/// Tempo máximo total de uma busca.
pub const TIMEOUT: Duration = Duration::from_secs(15);
/// Teto do corpo de `fetch_url`.
pub const MAX_BODY: usize = 2 * 1024 * 1024;
/// Teto do corpo de consultas a registros (índices grandes, como o do npm).
const MAX_REGISTRY_BODY: usize = 12 * 1024 * 1024;
/// Teto da saída devolvida ao modelo.
pub const MAX_OUTPUT: usize = 24 * 1024;
/// Versões recentes listadas por `package_versions`.
const RECENT: usize = 15;
const USER_AGENT: &str = concat!("nants/", env!("CARGO_PKG_VERSION"), " (+https://nants.tmdevlab.com)");

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("URL inválida: {0}")]
    InvalidUrl(String),
    #[error("somente http e https são permitidos")]
    Scheme,
    #[error("URL com credenciais não é permitida")]
    Credentials,
    #[error("não foi possível resolver o host: {0}")]
    Dns(String),
    #[error("destino bloqueado (endereço não público): {0}")]
    Blocked(IpAddr),
    #[error("redirecionamentos demais (máx. {MAX_REDIRECTS})")]
    TooManyRedirects,
    #[error("falha na requisição: {0}")]
    Request(String),
    #[error("resposta HTTP {0}")]
    Status(u16),
    #[error("conteúdo binário ou de tipo não suportado ({0})")]
    Binary(String),
    #[error("tempo esgotado (15 s)")]
    Timeout,
    #[error("ecossistema desconhecido: {0} (use npm, pypi, crates, dockerhub ou mcr)")]
    Ecosystem(String),
    #[error("nome de pacote inválido para {0}")]
    InvalidName(String),
    #[error("resposta do registro não pôde ser interpretada: {0}")]
    Parse(String),
}

/// Cliente web com as proteções acima.
#[derive(Debug, Clone, Default)]
pub struct WebClient {
    #[cfg(test)]
    allow_loopback: bool,
    #[cfg(test)]
    bases: std::collections::HashMap<&'static str, String>,
}

struct Fetched {
    content_type: String,
    body: Vec<u8>,
    /// Endereço final, depois dos redirecionamentos.
    url: Url,
    /// O corpo chegou ao teto e foi cortado.
    truncated: bool,
}

/// Página lida inteira (até [`MAX_BODY`]) para a base de conhecimento (spec 134/T02).
#[derive(Debug, Clone)]
pub struct Page {
    /// Endereço final, depois dos redirecionamentos (base dos links relativos).
    pub url: String,
    /// `Content-Type` da resposta, em minúsculas (vazio se ausente).
    pub content_type: String,
    pub body: Vec<u8>,
    /// O corpo chegou a [`MAX_BODY`] e foi cortado.
    pub truncated: bool,
}

impl WebClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Só testes: libera loopback e redireciona registros para servidores locais.
    #[cfg(test)]
    pub(crate) fn for_test(bases: &[(&'static str, String)]) -> Self {
        Self { allow_loopback: true, bases: bases.iter().cloned().collect() }
    }

    fn blocked(&self, ip: IpAddr) -> bool {
        #[cfg(test)]
        if self.allow_loopback && ip.is_loopback() {
            return false;
        }
        ip_blocked(ip)
    }

    fn base(&self, eco: &'static str, default: &str) -> String {
        #[cfg(test)]
        if let Some(b) = self.bases.get(eco) {
            return b.clone();
        }
        let _ = eco;
        default.to_string()
    }

    /// Lê uma página pública e devolve texto legível (≤ 24 KiB).
    pub async fn fetch_url(&self, url: &str) -> Result<String, WebError> {
        let fetched = self.get(url, None, MAX_BODY).await?;
        let ct = fetched.content_type.to_ascii_lowercase();
        let text = String::from_utf8_lossy(&fetched.body).into_owned();
        let out = if ct.starts_with("text/html") || ct.starts_with("application/xhtml") {
            html_to_text(&text)
        } else if textual(&ct) || (ct.is_empty() && !fetched.body.contains(&0)) {
            text
        } else {
            return Err(WebError::Binary(if ct.is_empty() { "sem content-type".into() } else { ct }));
        };
        Ok(cut(out, MAX_OUTPUT))
    }

    /// Lê uma página pública inteira, com as mesmas proteções de [`Self::fetch_url`], sem
    /// converter nem cortar a saída (o corpo fica até [`MAX_BODY`]). Usado pela base de
    /// conhecimento, que lê o link uma vez (spec 134/T02).
    pub async fn fetch_page(&self, url: &str) -> Result<Page, WebError> {
        let fetched = self.get(url, None, MAX_BODY).await?;
        Ok(Page {
            url: fetched.url.to_string(),
            content_type: fetched.content_type.to_ascii_lowercase(),
            body: fetched.body,
            truncated: fetched.truncated,
        })
    }

    /// Versões publicadas de um pacote/imagem em registro oficial.
    pub async fn package_versions(&self, ecosystem: &str, name: &str) -> Result<String, WebError> {
        let name = name.trim();
        let (url, versions, stable): (String, Vec<String>, Option<String>) = match ecosystem.trim() {
            "npm" => {
                if !valid_npm(name) {
                    return Err(WebError::InvalidName("npm".into()));
                }
                let url = format!("{}/{}", self.base("npm", "https://registry.npmjs.org"), name.replace('/', "%2f"));
                let body = self.get(&url, Some("application/vnd.npm.install-v1+json"), MAX_REGISTRY_BODY).await?;
                let (v, s) = parse_npm(&body.body)?;
                (url, v, s)
            }
            "pypi" => {
                if !valid_pypi(name) {
                    return Err(WebError::InvalidName("pypi".into()));
                }
                let url = format!("{}/pypi/{}/json", self.base("pypi", "https://pypi.org"), name);
                let body = self.get(&url, Some("application/json"), MAX_REGISTRY_BODY).await?;
                let (v, s) = parse_pypi(&body.body)?;
                (url, v, s)
            }
            "crates" => {
                if !valid_crate(name) {
                    return Err(WebError::InvalidName("crates".into()));
                }
                let url = format!("{}/api/v1/crates/{}", self.base("crates", "https://crates.io"), name);
                let body = self.get(&url, Some("application/json"), MAX_REGISTRY_BODY).await?;
                let (v, s) = parse_crates(&body.body)?;
                (url, v, s)
            }
            "dockerhub" => {
                let repo = docker_repo(name).ok_or_else(|| WebError::InvalidName("dockerhub".into()))?;
                let url = format!(
                    "{}/v2/repositories/{}/tags?page_size=100&ordering=last_updated",
                    self.base("dockerhub", "https://hub.docker.com"),
                    repo
                );
                let body = self.get(&url, Some("application/json"), MAX_REGISTRY_BODY).await?;
                (url, parse_dockerhub(&body.body)?, None)
            }
            "mcr" => {
                if !valid_mcr(name) {
                    return Err(WebError::InvalidName("mcr".into()));
                }
                let url = format!("{}/v2/{}/tags/list", self.base("mcr", "https://mcr.microsoft.com"), name);
                let body = self.get(&url, Some("application/json"), MAX_REGISTRY_BODY).await?;
                (url, parse_mcr(&body.body)?, None)
            }
            other => return Err(WebError::Ecosystem(other.to_string())),
        };
        let image = matches!(ecosystem.trim(), "dockerhub" | "mcr");
        Ok(cut(summarize(ecosystem.trim(), name, &url, versions, stable, image), MAX_OUTPUT))
    }

    /// GET com SSRF guard, redirecionamentos manuais e teto de corpo.
    async fn get(&self, start: &str, accept: Option<&str>, cap: usize) -> Result<Fetched, WebError> {
        tokio::time::timeout(TIMEOUT, self.get_inner(start, accept, cap))
            .await
            .map_err(|_| WebError::Timeout)?
    }

    async fn get_inner(&self, start: &str, accept: Option<&str>, cap: usize) -> Result<Fetched, WebError> {
        let mut url = Url::parse(start.trim()).map_err(|e| WebError::InvalidUrl(e.to_string()))?;
        for hop in 0..=MAX_REDIRECTS {
            if !matches!(url.scheme(), "http" | "https") {
                return Err(WebError::Scheme);
            }
            if !url.username().is_empty() || url.password().is_some() {
                return Err(WebError::Credentials);
            }
            let port = url.port_or_known_default().ok_or_else(|| WebError::InvalidUrl("sem porta".into()))?;
            let mut builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .timeout(TIMEOUT)
                .user_agent(USER_AGENT);
            let host = url.host_str().ok_or_else(|| WebError::InvalidUrl("sem host".into()))?.to_string();
            let literal = host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().ok();
            match literal {
                Some(ip) => self.check(&[ip])?,
                None => {
                    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
                        .await
                        .map_err(|e| WebError::Dns(e.to_string()))?
                        .collect();
                    if addrs.is_empty() {
                        return Err(WebError::Dns(host));
                    }
                    let ips: Vec<IpAddr> = addrs.iter().map(|a| a.ip()).collect();
                    self.check(&ips)?;
                    builder = builder.resolve_to_addrs(&host, &addrs);
                }
            }
            let client = builder.build().map_err(|e| WebError::Request(e.to_string()))?;
            let mut req = client.get(url.clone());
            if let Some(a) = accept {
                req = req.header("accept", a);
            }
            let mut resp = req.send().await.map_err(|e| WebError::Request(e.without_url().to_string()))?;
            let status = resp.status();
            if status.is_redirection() {
                if hop == MAX_REDIRECTS {
                    return Err(WebError::TooManyRedirects);
                }
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(WebError::Status(status.as_u16()))?;
                url = url.join(loc).map_err(|e| WebError::InvalidUrl(e.to_string()))?;
                continue;
            }
            if !status.is_success() {
                return Err(WebError::Status(status.as_u16()));
            }
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let mut body = Vec::new();
            let mut truncated = false;
            while let Some(chunk) = resp.chunk().await.map_err(|e| WebError::Request(e.without_url().to_string()))? {
                let room = cap - body.len();
                if chunk.len() >= room {
                    body.extend_from_slice(&chunk[..room]);
                    truncated = true;
                    break;
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(Fetched { content_type, body, url, truncated });
        }
        Err(WebError::TooManyRedirects)
    }

    fn check(&self, ips: &[IpAddr]) -> Result<(), WebError> {
        match ips.iter().find(|ip| self.blocked(**ip)) {
            Some(ip) => Err(WebError::Blocked(*ip)),
            None => Ok(()),
        }
    }
}

pub(crate) fn textual(ct: &str) -> bool {
    let main = ct.split(';').next().unwrap_or("").trim();
    main.starts_with("text/")
        || main == "application/json"
        || main == "application/xml"
        || main == "application/javascript"
        || main == "application/x-yaml"
        || main.ends_with("+json")
        || main.ends_with("+xml")
}

/// `true` para endereços que nunca devem ser alvo de `fetch_url`.
pub fn ip_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => v6_blocked(v6),
    }
}

fn v4_blocked(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // inclui 169.254.169.254 (metadados)
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0 // 0.0.0.0/8
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmark 198.18/15
        || o[0] >= 240 // reservado
}

fn v6_blocked(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    if (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xffc0) == 0xfec0 {
        return true; // ULA fc00::/7, link-local fe80::/10, site-local
    }
    if s[0] == 0x2001 && s[1] == 0x0db8 {
        return true; // documentação
    }
    let v4 = |hi: u16, lo: u16| v4_blocked(Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8));
    // IPv4 embutido: mapeado ::ffff:a.b.c.d, compatível ::a.b.c.d, NAT64 64:ff9b::/96, 6to4 2002::/16.
    if s[..5] == [0; 5] && (s[5] == 0xffff || s[5] == 0) {
        return v4(s[6], s[7]);
    }
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0; 4] {
        return v4(s[6], s[7]);
    }
    if s[0] == 0x2002 {
        return v4(s[1], s[2]);
    }
    false
}

fn cut(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n[saída truncada em 24 KiB]");
    text
}

const BLOCK_TAGS: &[&str] = &[
    "p", "div", "br", "li", "ul", "ol", "tr", "td", "th", "table", "section", "article", "header", "footer",
    "nav", "main", "h1", "h2", "h3", "h4", "h5", "h6", "pre", "blockquote", "hr", "dt", "dd", "form",
];

/// Tags sem fechamento.
const VOID_TAGS: [&str; 14] =
    ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr", "param"];

/// Entidades nomeadas comuns além das cinco do XML e do `nbsp`: aspas e pontuação tipográficas e
/// as letras acentuadas do português.
const NAMED_ENTITIES: &[(&str, char)] = &[
    ("ldquo", '\u{201C}'), ("rdquo", '\u{201D}'), ("lsquo", '\u{2018}'), ("rsquo", '\u{2019}'),
    ("laquo", '\u{AB}'), ("raquo", '\u{BB}'), ("hellip", '\u{2026}'), ("mdash", '\u{2014}'),
    ("ndash", '\u{2013}'), ("bull", '\u{2022}'), ("middot", '\u{B7}'), ("copy", '\u{A9}'),
    ("reg", '\u{AE}'), ("trade", '\u{2122}'), ("deg", '\u{B0}'), ("times", '\u{D7}'),
    ("rarr", '\u{2192}'), ("larr", '\u{2190}'), ("para", '\u{B6}'), ("sect", '\u{A7}'),
    ("ordm", '\u{BA}'), ("ordf", '\u{AA}'), ("euro", '\u{20AC}'), ("zwj", '\u{200D}'),
    ("aacute", 'á'), ("eacute", 'é'), ("iacute", 'í'), ("oacute", 'ó'), ("uacute", 'ú'),
    ("Aacute", 'Á'), ("Eacute", 'É'), ("Iacute", 'Í'), ("Oacute", 'Ó'), ("Uacute", 'Ú'),
    ("agrave", 'à'), ("Agrave", 'À'), ("acirc", 'â'), ("ecirc", 'ê'), ("ocirc", 'ô'),
    ("Acirc", 'Â'), ("Ecirc", 'Ê'), ("Ocirc", 'Ô'), ("atilde", 'ã'), ("otilde", 'õ'),
    ("Atilde", 'Ã'), ("Otilde", 'Õ'), ("ccedil", 'ç'), ("Ccedil", 'Ç'), ("uuml", 'ü'),
    ("Uuml", 'Ü'), ("ntilde", 'ñ'), ("Ntilde", 'Ñ'),
];

/// Marcadores internos (área de uso privado do Unicode), removidos da entrada antes da conversão:
/// início e fim de um bloco `pre` e um nível de recuo de lista.
const PRE_OPEN: char = '\u{E000}';
const PRE_CLOSE: char = '\u{E001}';
const INDENT: char = '\u{E002}';

/// Peça do HTML vista pelo [`walk_html`].
enum HtmlPiece<'a> {
    /// Texto entre tags (entidades ainda não decodificadas).
    Text(&'a str),
    /// Tag de abertura: nome em minúsculas e a tag inteira (`<a href=...>`).
    Open(&'a str, &'a str),
    /// Tag de fechamento: nome em minúsculas.
    Close(&'a str),
}

/// Percorre o HTML em peças, sem comentários e sem o conteúdo de script/style/noscript/template/svg.
/// `lower` é `html` em minúsculas ASCII (mesmos índices).
fn walk_html<'a>(html: &'a str, lower: &'a str, mut on: impl FnMut(HtmlPiece<'a>)) {
    let mut i = 0;
    let mut text_start = 0;
    while i < html.len() {
        let rest = &html[i..];
        if !rest.starts_with('<') {
            i += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let next = rest[1..].chars().next();
        if !matches!(next, Some(c) if c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?') {
            i += 1;
            continue;
        }
        if text_start < i {
            on(HtmlPiece::Text(&html[text_start..i]));
        }
        if lower[i..].starts_with("<!--") {
            i = lower[i + 4..].find("-->").map(|p| i + 4 + p + 3).unwrap_or(html.len());
            text_start = i;
            continue;
        }
        let closing = lower[i + 1..].starts_with('/');
        let name_start = if closing { i + 2 } else { i + 1 };
        let name_len = lower[name_start..].bytes().take_while(u8::is_ascii_alphanumeric).count();
        let name = &lower[name_start..name_start + name_len];
        if !closing && matches!(name, "script" | "style" | "noscript" | "template" | "svg") {
            let close = format!("</{name}");
            let after = lower[i..].find(&close).map(|p| i + p);
            i = match after {
                Some(p) => lower[p..].find('>').map(|q| p + q + 1).unwrap_or(html.len()),
                None => html.len(),
            };
            text_start = i;
            continue;
        }
        let end = rest.find('>').map_or(html.len(), |p| i + p + 1);
        if !name.is_empty() {
            if closing {
                on(HtmlPiece::Close(name));
            } else {
                on(HtmlPiece::Open(name, &html[i..end]));
            }
        }
        i = end;
        text_start = i;
    }
    if text_start < html.len() {
        on(HtmlPiece::Text(&html[text_start..]));
    }
}

/// Estado da conversão de [`html_to_text`].
struct Render {
    focus: Option<&'static str>,
    focus_depth: usize,
    skip_depth: usize,
    pre_depth: usize,
    heading: bool,
    /// Listas abertas: (ordenada, número do próximo item).
    lists: Vec<(bool, u32)>,
    /// A saída acabou de receber o marcador de um item e ainda não o texto dele.
    at_marker: bool,
    /// Elemento com `role="navigation"` aberto: nome da tag e quantas do mesmo nome estão abertas.
    role_skip: Option<(String, usize)>,
    out: String,
}

impl Render {
    /// Fora das áreas de navegação e, se a página marca o conteúdo principal, dentro dele.
    fn visible(&self) -> bool {
        self.skip_depth == 0 && (self.focus.is_none() || self.focus_depth > 0)
    }

    /// Área de navegação descartada (C-23). Sem `<main>`/`<article>`: `nav`, `aside` e `footer`.
    /// Dentro do conteúdo principal, só `nav` (menus e abas, como os da Wikipédia): `aside` e
    /// `footer` de um artigo costumam ser conteúdo (quadros laterais, notas).
    fn is_chrome(&self, name: &str) -> bool {
        match self.focus {
            Some(_) => name == "nav",
            None => matches!(name, "nav" | "aside" | "footer"),
        }
    }

    fn open(&mut self, name: &str, tag: &str) {
        if Some(name) == self.focus {
            self.focus_depth += 1;
        }
        if self.is_chrome(name) {
            self.skip_depth += 1;
        }
        // Elemento com `role="navigation"` (menus de documentação em `div`): fora, até fechar.
        match self.role_skip.as_mut() {
            Some((skip_name, nested)) => {
                if skip_name == name {
                    *nested += 1;
                }
            }
            None => {
                let navigation = !VOID_TAGS.contains(&name)
                    && tag_attr(tag, "role").is_some_and(|r| r.trim().eq_ignore_ascii_case("navigation"));
                if navigation {
                    self.role_skip = Some((name.to_string(), 1));
                    self.skip_depth += 1;
                }
            }
        }
        if matches!(name, "ul" | "ol") {
            self.lists.push((name == "ol", 1));
        }
        let show = self.visible();
        match name {
            "li" => {
                let marker = match self.lists.last_mut() {
                    Some((true, n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "- ".to_string(),
                };
                if show {
                    self.line_start();
                    self.out.extend(std::iter::repeat_n(INDENT, self.lists.len().saturating_sub(1)));
                    self.out.push_str(&marker);
                    self.at_marker = true;
                }
            }
            "ul" | "ol" => {
                if show {
                    // A lista de fora abre com uma linha em branco; a de dentro, na linha seguinte.
                    if self.lists.len() == 1 {
                        self.out.push('\n');
                    }
                    self.line_start();
                }
            }
            "pre" => {
                self.pre_depth += 1;
                if show && self.pre_depth == 1 {
                    self.out.push('\n');
                    self.out.push(PRE_OPEN);
                    self.out.push('\n');
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if show && self.pre_depth == 0 {
                    let level = usize::from(name.as_bytes()[1] - b'0');
                    self.out.push_str("\n\n");
                    self.out.push_str(&"#".repeat(level));
                    self.out.push(' ');
                    self.heading = true;
                }
            }
            _ => {
                if show && BLOCK_TAGS.contains(&name) {
                    self.block_break();
                }
            }
        }
    }

    /// Começa uma linha nova, se a saída ainda não está no começo de uma.
    fn line_start(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.at_marker = false;
    }

    /// Quebra de bloco, exceto logo depois do marcador de um item (`<li><p>texto</p></li>`).
    fn block_break(&mut self) {
        if !self.at_marker {
            self.out.push('\n');
        }
    }

    fn close(&mut self, name: &str) {
        let show = self.visible();
        match name {
            "pre" => {
                if show && self.pre_depth == 1 {
                    self.out.push('\n');
                    self.out.push(PRE_CLOSE);
                    self.out.push('\n');
                }
                self.pre_depth = self.pre_depth.saturating_sub(1);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if self.heading {
                    self.out.push_str("\n\n");
                }
                self.heading = false;
            }
            "ul" | "ol" => {
                self.lists.pop();
                if show {
                    self.line_start();
                    if self.lists.is_empty() {
                        self.out.push('\n');
                    }
                }
            }
            // O próximo item (ou o fim da lista) já começa uma linha.
            "li" => {}
            _ => {
                if show && BLOCK_TAGS.contains(&name) {
                    self.block_break();
                }
            }
        }
        if Some(name) == self.focus {
            self.focus_depth = self.focus_depth.saturating_sub(1);
        }
        if self.is_chrome(name) {
            self.skip_depth = self.skip_depth.saturating_sub(1);
        }
        if let Some((skip_name, nested)) = self.role_skip.as_mut() {
            if skip_name == name {
                *nested -= 1;
                if *nested == 0 {
                    self.role_skip = None;
                    self.skip_depth = self.skip_depth.saturating_sub(1);
                }
            }
        }
    }

    fn text(&mut self, text: &str) {
        if !self.visible() {
            return;
        }
        if !text.trim().is_empty() {
            self.at_marker = false;
        }
        if self.pre_depth > 0 {
            self.out.push_str(text);
        } else {
            // Fora do `pre`, a quebra de linha do fonte é espaço, como no navegador.
            self.out.extend(text.chars().map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c }));
        }
    }
}

/// Junta as linhas: colapsa espaços, uma linha em branco no máximo entre blocos, recuo das listas
/// e os blocos `pre` entre crases, sem colapsar.
fn finish_lines(decoded: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut code: Option<Vec<String>> = None;
    let fence = |lines: &mut Vec<String>, block: Vec<String>| {
        let first = block.iter().position(|l| !l.trim().is_empty()).unwrap_or(block.len());
        let last = block.iter().rposition(|l| !l.trim().is_empty()).map_or(first, |p| p + 1);
        if lines.last().is_some_and(|l| !l.is_empty()) {
            lines.push(String::new());
        }
        lines.push("```".to_string());
        lines.extend(block.into_iter().take(last).skip(first));
        lines.push("```".to_string());
    };
    for line in decoded.lines() {
        let marker = line.trim();
        if let Some(mut block) = code.take() {
            if marker.starts_with(PRE_CLOSE) && marker.chars().count() == 1 {
                fence(&mut lines, block);
            } else {
                block.push(line.trim_end().to_string());
                code = Some(block);
            }
            continue;
        }
        if marker.starts_with(PRE_OPEN) && marker.chars().count() == 1 {
            code = Some(Vec::new());
            continue;
        }
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ").replace(INDENT, "  ");
        if collapsed.is_empty() && lines.last().is_none_or(String::is_empty) {
            continue;
        }
        lines.push(collapsed);
    }
    if let Some(block) = code {
        fence(&mut lines, block);
    }
    lines.join("\n").replace([PRE_OPEN, PRE_CLOSE, INDENT], "").trim().to_string()
}

/// Converte HTML em texto legível, em markdown simples (spec 134/T02):
/// - remove script/style/comentários/tags, decodifica entidades e colapsa espaços;
/// - preserva os títulos (`#` a `######`), as listas (`-` e `1.`, com recuo por nível) e o `pre`
///   (bloco entre crases, sem colapsar espaços);
/// - foca no conteúdo principal (C-23): com `<main>`, extrai só ele; sem `<main>` e com
///   `<article>`, só os artigos; sem nenhum dos dois, descarta `nav`, `aside` e `footer`.
pub fn html_to_text(html: &str) -> String {
    let cleaned: std::borrow::Cow<'_, str> = if html.contains([PRE_OPEN, PRE_CLOSE, INDENT]) {
        std::borrow::Cow::Owned(html.replace([PRE_OPEN, PRE_CLOSE, INDENT], ""))
    } else {
        std::borrow::Cow::Borrowed(html)
    };
    let html = cleaned.as_ref();
    let lower = html.to_ascii_lowercase();
    let mut has_main = false;
    let mut has_article = false;
    walk_html(html, &lower, |piece| {
        if let HtmlPiece::Open(name, _) = piece {
            has_main |= name == "main";
            has_article |= name == "article";
        }
    });
    let focus = if has_main {
        Some("main")
    } else if has_article {
        Some("article")
    } else {
        None
    };
    let mut render = Render {
        focus,
        focus_depth: 0,
        skip_depth: 0,
        pre_depth: 0,
        heading: false,
        lists: Vec::new(),
        at_marker: false,
        role_skip: None,
        out: String::with_capacity(html.len() / 2),
    };
    walk_html(html, &lower, |piece| match piece {
        HtmlPiece::Open(name, tag) => render.open(name, tag),
        HtmlPiece::Close(name) => render.close(name),
        HtmlPiece::Text(text) => render.text(text),
    });
    finish_lines(&decode_entities(&render.out))
}

/// Metadados de uma página para a base de conhecimento (spec 134/T02).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HtmlMeta {
    /// Texto do `<title>`, decodificado e numa linha.
    pub title: Option<String>,
    /// `<meta name="description" content="...">`, numa linha.
    pub description: Option<String>,
    /// `href` de cada `<a>`, decodificado, na ordem da página e sem repetição, até
    /// [`crate::kb::extract::MAX_LINK_TARGETS`]. Âncoras (`#x`) e esquemas que nunca são alvo
    /// (`mailto:`, `javascript:`, `tel:`, `data:`) ficam de fora e não gastam o teto.
    pub links: Vec<String>,
}

/// Valor do atributo `name` numa tag (`<a href="x">`), com as entidades decodificadas.
fn tag_attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(p) = lower[from..].find(name).map(|p| p + from) {
        from = p + name.len();
        let before_ok = p > 0 && (bytes[p - 1].is_ascii_whitespace() || bytes[p - 1] == b'/');
        let mut k = from;
        while bytes.get(k).is_some_and(u8::is_ascii_whitespace) {
            k += 1;
        }
        if !before_ok || bytes.get(k) != Some(&b'=') {
            continue;
        }
        k += 1;
        while bytes.get(k).is_some_and(u8::is_ascii_whitespace) {
            k += 1;
        }
        let value = match bytes.get(k) {
            Some(&q) if q == b'"' || q == b'\'' => {
                let end = lower[k + 1..].find(char::from(q)).map_or(tag.len(), |e| k + 1 + e);
                &tag[k + 1..end]
            }
            Some(_) => {
                let end = lower[k..].find(|c: char| c.is_ascii_whitespace() || c == '>').map_or(tag.len(), |e| k + e);
                &tag[k..end]
            }
            None => return None,
        };
        return Some(decode_entities(value));
    }
    None
}

/// Título, descrição e links de uma página.
pub fn html_meta(html: &str) -> HtmlMeta {
    let lower = html.to_ascii_lowercase();
    let mut meta = HtmlMeta::default();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut in_title = false;
    let mut title = String::new();
    walk_html(html, &lower, |piece| match piece {
        HtmlPiece::Open("title", _) => in_title = meta.title.is_none(),
        HtmlPiece::Close("title") => {
            if in_title {
                let text = decode_entities(&title).split_whitespace().collect::<Vec<_>>().join(" ");
                meta.title = (!text.is_empty()).then_some(text);
            }
            in_title = false;
        }
        HtmlPiece::Text(text) if in_title => title.push_str(text),
        HtmlPiece::Open("meta", tag) => {
            let is_description = tag_attr(tag, "name").is_some_and(|n| n.trim().eq_ignore_ascii_case("description"));
            if is_description && meta.description.is_none() {
                meta.description = tag_attr(tag, "content")
                    .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|c| !c.is_empty());
            }
        }
        HtmlPiece::Open("a", tag) => {
            if meta.links.len() >= crate::kb::extract::MAX_LINK_TARGETS {
                return;
            }
            if let Some(href) = tag_attr(tag, "href").map(|h| h.trim().to_string()) {
                if !href.is_empty() && !never_a_target(&href) && seen.insert(href.clone()) {
                    meta.links.push(href);
                }
            }
        }
        _ => {}
    });
    meta
}

/// `href` que nunca vira alvo de link entre documentos: âncora da própria página ou esquema que
/// não é endereço de página.
fn never_a_target(href: &str) -> bool {
    let lower = href.get(..href.len().min(16)).unwrap_or(href).to_ascii_lowercase();
    href.starts_with('#') || ["mailto:", "javascript:", "tel:", "data:"].iter().any(|p| lower.starts_with(p))
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let decoded = rest.find(';').filter(|&e| e <= 10).and_then(|e| {
            let ent = &rest[1..e];
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ if !ent.starts_with('#') => NAMED_ENTITIES.iter().find(|(name, _)| *name == ent).map(|(_, c)| *c),
                _ => ent
                    .strip_prefix('#')
                    .and_then(|n| match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => n.parse().ok(),
                    })
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------- validação de nomes ----------

fn ok_chars(s: &str, extra: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

fn valid_npm(name: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty() && p.len() <= 214 && !p.starts_with(['.', '_']) && ok_chars(p, "._-~") && p == p.to_ascii_lowercase()
    };
    match name.strip_prefix('@') {
        Some(rest) => matches!(rest.split_once('/'), Some((s, n)) if part(s) && part(n)),
        None => part(name),
    }
}

fn valid_pypi(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && ok_chars(name, "._-")
}

fn valid_crate(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.starts_with(|c: char| c.is_ascii_alphabetic()) && ok_chars(name, "_-")
}

fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && ok_chars(s, "._-")
        && s == s.to_ascii_lowercase()
}

/// `node` → `library/node`; `ns/repo` mantido.
fn docker_repo(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('/').collect();
    match parts.as_slice() {
        [r] if valid_segment(r) => Some(format!("library/{r}")),
        [n, r] if valid_segment(n) && valid_segment(r) => Some(format!("{n}/{r}")),
        _ => None,
    }
}

fn valid_mcr(name: &str) -> bool {
    let parts: Vec<&str> = name.split('/').collect();
    (1..=4).contains(&parts.len()) && parts.iter().all(|p| valid_segment(p))
}

// ---------- versões ----------

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
struct Ver {
    nums: Vec<u64>,
    stable: bool,
    rest: String,
}

fn parse_ver(raw: &str) -> Option<Ver> {
    let s = raw.strip_prefix(['v', 'V']).unwrap_or(raw);
    let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, rest) = s.split_at(end);
    let nums: Vec<u64> = num.trim_end_matches('.').split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
    if nums.is_empty() || nums.len() > 6 {
        return None;
    }
    // Estável: nada depois dos números, ou só metadado de build (+...). Qualquer `-x`, `a1`, `rc1` é pré-release.
    let stable = rest.is_empty() || rest.starts_with('+');
    Some(Ver { nums, stable, rest: rest.to_string() })
}

/// Versões ordenadas da mais nova para a mais antiga; `strict` (imagens) só aceita `N(.N)*`.
fn order(versions: &[String], strict: bool) -> Vec<(Ver, String)> {
    let mut v: Vec<(Ver, String)> = versions
        .iter()
        .filter_map(|s| parse_ver(s).map(|p| (p, s.clone())))
        .filter(|(p, _)| !strict || (p.stable && p.rest.is_empty()))
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.dedup_by(|a, b| a.1 == b.1);
    v
}

fn summarize(
    eco: &str,
    name: &str,
    url: &str,
    versions: Vec<String>,
    declared_stable: Option<String>,
    image: bool,
) -> String {
    let ordered = order(&versions, image);
    let stable: Vec<&(Ver, String)> = ordered.iter().filter(|(p, _)| p.stable).collect();
    let pick: Vec<String> = if !stable.is_empty() {
        stable.iter().map(|(_, s)| s.clone()).collect()
    } else if !ordered.is_empty() {
        ordered.iter().map(|(_, s)| s.clone()).collect()
    } else {
        versions.clone()
    };
    // O "latest" declarado pelo registro só vale se for estável.
    let latest = declared_stable
        .filter(|d| versions.contains(d) && parse_ver(d).is_some_and(|p| p.stable))
        .or_else(|| pick.first().cloned())
        .unwrap_or_else(|| "(nenhuma)".into());
    let recent: Vec<String> = pick.into_iter().take(RECENT).collect();
    format!(
        "{eco}: {name}\nÚltima versão estável: {latest}\nRecentes: {}\nFonte: {url}",
        if recent.is_empty() { "(nenhuma)".into() } else { recent.join(", ") }
    )
}

fn json(body: &[u8]) -> Result<serde_json::Value, WebError> {
    serde_json::from_slice(body).map_err(|e| WebError::Parse(e.to_string()))
}

type Parsed = (Vec<String>, Option<String>);

fn parse_npm(body: &[u8]) -> Result<Parsed, WebError> {
    let v = json(body)?;
    let versions = v
        .get("versions")
        .and_then(|x| x.as_object())
        .ok_or_else(|| WebError::Parse("campo `versions` ausente".into()))?
        .keys()
        .cloned()
        .collect();
    let latest = v.pointer("/dist-tags/latest").and_then(|x| x.as_str()).map(String::from);
    Ok((versions, latest))
}

fn parse_pypi(body: &[u8]) -> Result<Parsed, WebError> {
    let v = json(body)?;
    let versions = v
        .get("releases")
        .and_then(|x| x.as_object())
        .ok_or_else(|| WebError::Parse("campo `releases` ausente".into()))?
        .iter()
        .filter(|(_, files)| files.as_array().map_or(true, |f| !f.is_empty()))
        .map(|(k, _)| k.clone())
        .collect();
    let latest = v.pointer("/info/version").and_then(|x| x.as_str()).map(String::from);
    Ok((versions, latest))
}

fn parse_crates(body: &[u8]) -> Result<Parsed, WebError> {
    let v = json(body)?;
    let versions = v
        .get("versions")
        .and_then(|x| x.as_array())
        .ok_or_else(|| WebError::Parse("campo `versions` ausente".into()))?
        .iter()
        .filter(|e| !e.get("yanked").and_then(|y| y.as_bool()).unwrap_or(false))
        .filter_map(|e| e.get("num").and_then(|n| n.as_str()).map(String::from))
        .collect();
    let latest = v.pointer("/crate/max_stable_version").and_then(|x| x.as_str()).map(String::from);
    Ok((versions, latest))
}

fn parse_dockerhub(body: &[u8]) -> Result<Vec<String>, WebError> {
    let v = json(body)?;
    Ok(v.get("results")
        .and_then(|x| x.as_array())
        .ok_or_else(|| WebError::Parse("campo `results` ausente".into()))?
        .iter()
        .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect())
}

fn parse_mcr(body: &[u8]) -> Result<Vec<String>, WebError> {
    let v = json(body)?;
    Ok(v.get("tags")
        .and_then(|x| x.as_array())
        .ok_or_else(|| WebError::Parse("campo `tags` ausente".into()))?
        .iter()
        .filter_map(|n| n.as_str().map(String::from))
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) type Route = (&'static str, u16, &'static str, String, Vec<u8>);

    /// Servidor HTTP local: `routes` = (caminho, status, content-type, location, corpo).
    pub(crate) async fn serve(routes: Vec<Route>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let route = routes.iter().find(|r| path.split('?').next() == Some(r.0));
                    match route {
                        Some((_, st, ct, loc, body)) => {
                            let mut h = format!(
                                "HTTP/1.1 {st} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n",
                                body.len()
                            );
                            if !loc.is_empty() {
                                h.push_str(&format!("Location: {loc}\r\n"));
                            }
                            h.push_str("\r\n");
                            let _ = sock.write_all(h.as_bytes()).await;
                            let _ = sock.write_all(body).await;
                        }
                        None => {
                            let _ = sock
                                .write_all(b"HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                .await;
                        }
                    }
                });
            }
        });
        format!("http://127.0.0.1:{}", addr.port())
    }

    pub(crate) fn ok(path: &'static str, ct: &'static str, body: &str) -> Route {
        (path, 200, ct, String::new(), body.as_bytes().to_vec())
    }

    #[test]
    fn blocks_every_range() {
        for s in [
            "127.0.0.1", "127.8.9.10", "10.0.0.1", "172.16.0.1", "172.31.255.255", "192.168.1.1", "169.254.169.254",
            "169.254.1.1", "100.64.0.1", "100.127.255.255", "224.0.0.1", "239.1.1.1", "0.0.0.0", "0.1.2.3",
            "255.255.255.255", "192.0.2.1", "198.51.100.1", "203.0.113.1", "198.18.0.1", "240.0.0.1",
            "::1", "::", "fc00::1", "fd12:3456::1", "fe80::1", "febf::1", "ff02::1", "2001:db8::1",
            "::ffff:10.0.0.1", "::ffff:127.0.0.1", "::ffff:169.254.169.254", "64:ff9b::a00:1", "2002:c0a8:101::1",
        ] {
            let ip: IpAddr = s.parse().expect(s);
            assert!(ip_blocked(ip), "{s} deveria ser bloqueado");
        }
        for s in ["8.8.8.8", "1.1.1.1", "100.63.255.255", "100.128.0.1", "172.32.0.1", "2606:4700::1111", "::ffff:8.8.8.8"] {
            let ip: IpAddr = s.parse().expect(s);
            assert!(!ip_blocked(ip), "{s} deveria ser permitido");
        }
    }

    #[tokio::test]
    async fn refuses_private_and_bad_urls_in_production_mode() {
        let c = WebClient::new();
        for u in [
            "http://127.0.0.1/", "http://10.0.0.1/x", "http://[::1]/", "http://[::ffff:10.0.0.1]/", "http://localhost/",
            "http://169.254.169.254/latest/meta-data", "http://0.0.0.0:80/",
        ] {
            assert!(matches!(c.fetch_url(u).await, Err(WebError::Blocked(_)) | Err(WebError::Dns(_))), "{u}");
        }
        assert!(matches!(c.fetch_url("ftp://example.com/").await, Err(WebError::Scheme)));
        assert!(matches!(c.fetch_url("file:///etc/passwd").await, Err(WebError::Scheme)));
        assert!(matches!(c.fetch_url("http://u:p@example.com/").await, Err(WebError::Credentials)));
        assert!(matches!(c.fetch_url("não é url").await, Err(WebError::InvalidUrl(_))));
    }

    #[tokio::test]
    async fn redirect_to_private_ip_is_refused() {
        let base = serve(vec![
            ("/r", 302, "text/plain", "http://192.168.1.1/admin".into(), vec![]),
            ("/meta", 301, "text/plain", "http://169.254.169.254/latest".into(), vec![]),
        ])
        .await;
        let c = WebClient::for_test(&[]);
        assert!(matches!(c.fetch_url(&format!("{base}/r")).await, Err(WebError::Blocked(_))));
        assert!(matches!(c.fetch_url(&format!("{base}/meta")).await, Err(WebError::Blocked(_))));
    }

    #[tokio::test]
    async fn follows_redirects_up_to_five() {
        let mut routes = vec![ok("/end", "text/plain", "fim")];
        let names = ["/r0", "/r1", "/r2", "/r3", "/r4", "/r5"];
        for (i, n) in names.iter().enumerate() {
            let to = if i + 1 < names.len() { names[i + 1].to_string() } else { "/end".into() };
            routes.push((n, 302, "text/plain", to, vec![]));
        }
        let base = serve(routes).await;
        let c = WebClient::for_test(&[]);
        assert_eq!(c.fetch_url(&format!("{base}/r1")).await.expect("5 hops"), "fim");
        assert!(matches!(c.fetch_url(&format!("{base}/r0")).await, Err(WebError::TooManyRedirects)));
    }

    #[tokio::test]
    async fn html_text_json_binary_and_size() {
        let html = "<html><head><title>T</title><style>p{color:red}</style><script>alert('x')</script></head>\
            <body><h1>Olá   mundo</h1><!-- c --><p>um &amp; dois &lt;b&gt;</p><p>a<b>b</b></p></body></html>";
        let big = "x".repeat(MAX_BODY + 100_000);
        let base = serve(vec![
            ok("/h", "text/html; charset=utf-8", html),
            ok("/j", "application/json", "{\"a\":1}"),
            ok("/t", "text/plain", "texto"),
            ("/b", 200, "application/octet-stream", String::new(), vec![0, 1, 2]),
            ("/big", 200, "text/plain", String::new(), big.into_bytes()),
        ])
        .await;
        let c = WebClient::for_test(&[]);
        let t = c.fetch_url(&format!("{base}/h")).await.expect("html");
        // Spec 134/T02: o título da página vira `#` (antes, linha solta).
        assert_eq!(t, "T\n\n# Olá mundo\n\num & dois <b>\n\nab");
        assert!(!t.contains("alert") && !t.contains("color"));
        assert_eq!(c.fetch_url(&format!("{base}/j")).await.expect("json"), "{\"a\":1}");
        assert_eq!(c.fetch_url(&format!("{base}/t")).await.expect("txt"), "texto");
        assert!(matches!(c.fetch_url(&format!("{base}/b")).await, Err(WebError::Binary(_))));
        assert!(matches!(c.fetch_url(&format!("{base}/x")).await, Err(WebError::Status(404))));
        let out = c.fetch_url(&format!("{base}/big")).await.expect("big");
        assert!(out.len() <= MAX_OUTPUT + 64 && out.contains("truncada"));
    }

    #[test]
    fn html_to_text_edge_cases() {
        assert_eq!(html_to_text("a < b <3 <script>x</script>fim"), "a < b <3 fim");
        assert_eq!(html_to_text("<SCRIPT>x</SCRIPT>ok<p>&#65;&#x42;&nbsp;&zzz;"), "ok\nAB &zzz;");
        assert_eq!(html_to_text("<div>sem fecho"), "sem fecho");
        assert_eq!(html_to_text("<p>x</p><p></p><p></p><p>y</p>"), "x\n\ny");
    }

    #[test]
    fn html_to_text_keeps_headings_lists_and_pre() {
        let html = "<h2>Instalação</h2><p>Passos:</p><ol><li>Baixe</li><li>Rode\n   o <b>script</b>\n<ul><li>Linux</li><li>Mac</li></ul></li></ol>\
            <h6>Nota</h6><pre>\nfn main() {\n    println!(\"&lt;oi&gt;\");\n}\n</pre><p>fim</p>";
        assert_eq!(
            html_to_text(html),
            "## Instalação\n\nPassos:\n\n1. Baixe\n2. Rode o script\n  - Linux\n  - Mac\n\n###### Nota\n\n```\nfn main() {\n    println!(\"<oi>\");\n}\n```\n\nfim"
        );
        // Marcadores internos na entrada não vazam nem viram estrutura.
        assert_eq!(html_to_text("a\u{E000}b\u{E002}c"), "abc");
        // `pre` sem fecho fecha no fim.
        assert_eq!(html_to_text("<pre>x  y"), "```\nx  y\n```");
    }

    #[test]
    fn html_to_text_focuses_on_main_content() {
        let chrome = "<header>Topo</header><nav><ul><li>Início</li></ul></nav><aside>Anúncio</aside>";
        // Com `<main>`: só ele (nem os artigos de fora).
        let html = format!("{chrome}<article>Fora</article><main><h1>Doc</h1><p>Corpo</p></main><footer>Rodapé</footer>");
        assert_eq!(html_to_text(&html), "# Doc\n\nCorpo");
        // Sem `<main>` e com `<article>`: os artigos.
        let html = format!("{chrome}<article><p>Um</p></article><div>solto</div><article><p>Dois</p></article>");
        assert_eq!(html_to_text(&html), "Um\n\nDois");
        // Sem nenhum dos dois: descarta nav, aside e footer (o header fica).
        let html = format!("{chrome}<div><p>Texto</p></div><footer><p>Rodapé</p></footer>");
        assert_eq!(html_to_text(&html), "Topo\n\nTexto");
        // `<main>` citado dentro de script não conta.
        assert_eq!(html_to_text("<script>var t='<main>';</script><nav>x</nav><p>ok</p>"), "ok");
    }

    #[test]
    fn html_meta_reads_title_description_and_links() {
        let html = "<head><title>Guia &amp; API</title><meta content='Resumo curto.' name=description>\
            <meta name=\"descriptionx\" content=\"não\"></head><body><a href=\"/a\">a</a><A HREF='b.html'>b</A>\
            <a href=\"/a\">de novo</a><a name=\"x\">sem href</a></body>";
        let meta = html_meta(html);
        assert_eq!(meta.title.as_deref(), Some("Guia & API"));
        assert_eq!(meta.description.as_deref(), Some("Resumo curto."));
        assert_eq!(meta.links, vec!["/a".to_string(), "b.html".to_string()]);
        assert_eq!(html_meta("<p>nada</p>"), HtmlMeta::default());
    }

    #[test]
    fn html_meta_links_skip_anchors_and_stop_at_the_cap() {
        let cap = crate::kb::extract::MAX_LINK_TARGETS;
        let mut html = String::from("<a href=\"#topo\">t</a><a href=\"mailto:a@b.c\">m</a><a href=\"JavaScript:x()\">j</a>");
        for i in 0..cap * 4 {
            html.push_str(&format!("<a href=\"/p{}\">x</a>", i % (cap * 2)));
        }
        let meta = html_meta(&html);
        assert_eq!(meta.links.len(), cap);
        assert_eq!(meta.links[0], "/p0");
        assert_eq!(meta.links[cap - 1], format!("/p{}", cap - 1));
    }

    #[tokio::test]
    async fn fetch_page_keeps_body_final_url_and_truncation() {
        let big = "y".repeat(MAX_BODY + 10);
        let base = serve(vec![
            ("/velho", 301, "text/plain", "/novo".into(), vec![]),
            ok("/novo", "Text/HTML; charset=utf-8", "<p>oi</p>"),
            ("/grande", 200, "text/plain", String::new(), big.into_bytes()),
        ])
        .await;
        let c = WebClient::for_test(&[]);
        let page = c.fetch_page(&format!("{base}/velho")).await.expect("página");
        assert_eq!(page.url, format!("{base}/novo"));
        assert_eq!(page.content_type, "text/html; charset=utf-8");
        assert_eq!(page.body, b"<p>oi</p>");
        assert!(!page.truncated);
        let page = c.fetch_page(&format!("{base}/grande")).await.expect("grande");
        assert!(page.truncated && page.body.len() == MAX_BODY);
        assert!(matches!(WebClient::new().fetch_page("http://127.0.0.1/").await, Err(WebError::Blocked(_))));
    }

    #[test]
    fn name_validation() {
        assert!(valid_npm("react") && valid_npm("@types/node") && valid_npm("lodash.get"));
        assert!(!valid_npm("../x") && !valid_npm("@a") && !valid_npm("A") && !valid_npm("a b") && !valid_npm("a/b"));
        assert!(valid_pypi("Django") && valid_pypi("zope.interface") && !valid_pypi("a/b") && !valid_pypi("-x"));
        assert!(valid_crate("serde_json") && !valid_crate("1x") && !valid_crate("a.b"));
        assert_eq!(docker_repo("node").as_deref(), Some("library/node"));
        assert_eq!(docker_repo("bitnami/redis").as_deref(), Some("bitnami/redis"));
        assert!(docker_repo("a/b/c").is_none() && docker_repo("..").is_none() && docker_repo("a?b").is_none());
        assert!(valid_mcr("dotnet/sdk") && valid_mcr("playwright") && !valid_mcr("a/../b") && !valid_mcr("a//b"));
    }

    #[tokio::test]
    async fn package_versions_rejects_bad_input() {
        let c = WebClient::new();
        assert!(matches!(c.package_versions("npm", "../etc").await, Err(WebError::InvalidName(_))));
        assert!(matches!(c.package_versions("maven", "x").await, Err(WebError::Ecosystem(_))));
        assert!(matches!(c.package_versions("crates", "http://evil").await, Err(WebError::InvalidName(_))));
    }

    #[tokio::test]
    async fn registries_parse_fixed_json() {
        let npm = r#"{"dist-tags":{"latest":"18.3.1"},"versions":{"18.2.0":{},"18.3.1":{},"19.0.0-rc.1":{},"17.0.2":{},"18.10.0":{}}}"#;
        let pypi = r#"{"info":{"version":"5.0.2"},"releases":{"4.2.0":[{}],"5.0.2":[{}],"5.1a1":[{}],"3.0":[],"5.0.10":[{}]}}"#;
        let crates = r#"{"crate":{"max_stable_version":"1.0.200"},"versions":[{"num":"1.0.200","yanked":false},{"num":"1.0.201","yanked":true},{"num":"1.1.0-beta.1","yanked":false},{"num":"0.9.0","yanked":false}]}"#;
        let docker = r#"{"results":[{"name":"latest"},{"name":"20-alpine"},{"name":"20.11.1"},{"name":"18.19.0"},{"name":"22.0.0"},{"name":"22.0.0-rc1"}]}"#;
        let mcr = r#"{"name":"dotnet/sdk","tags":["8.0","8.0.100","9.0.100-preview.1","latest","7.0.400"]}"#;
        let base = serve(vec![
            ok("/react", "application/json", npm),
            ok("/@types%2fnode", "application/json", npm),
            ok("/pypi/django/json", "application/json", pypi),
            ok("/api/v1/crates/serde", "application/json", crates),
            ok("/v2/repositories/library/node/tags", "application/json", docker),
            ok("/v2/dotnet/sdk/tags/list", "application/json", mcr),
            ok("/v2/repositories/library/bad/tags", "application/json", "nao json"),
        ])
        .await;
        let c = WebClient::for_test(&[
            ("npm", base.clone()),
            ("pypi", base.clone()),
            ("crates", base.clone()),
            ("dockerhub", base.clone()),
            ("mcr", base.clone()),
        ]);
        let o = c.package_versions("npm", "react").await.expect("npm");
        assert!(o.contains("Última versão estável: 18.3.1"), "{o}");
        assert!(o.contains("Recentes: 18.10.0, 18.3.1, 18.2.0, 17.0.2"), "{o}");
        assert!(!o.contains("rc.1"));
        assert!(c.package_versions("npm", "@types/node").await.is_ok());
        let o = c.package_versions("pypi", "django").await.expect("pypi");
        assert!(o.contains("Última versão estável: 5.0.2"), "{o}");
        assert!(o.contains("Recentes: 5.0.10, 5.0.2, 4.2.0") && !o.contains("5.1a1") && !o.contains("3.0"), "{o}");
        let o = c.package_versions("crates", "serde").await.expect("crates");
        assert!(o.contains("Última versão estável: 1.0.200") && o.contains("Recentes: 1.0.200, 0.9.0"), "{o}");
        assert!(!o.contains("1.0.201") && !o.contains("beta"));
        let o = c.package_versions("dockerhub", "node").await.expect("docker");
        assert!(o.contains("Última versão estável: 22.0.0") && o.contains("Recentes: 22.0.0, 20.11.1, 18.19.0"), "{o}");
        assert!(!o.contains("alpine") && !o.contains("latest"));
        let o = c.package_versions("mcr", "dotnet/sdk").await.expect("mcr");
        assert!(o.contains("Última versão estável: 8.0.100") && o.contains("Recentes: 8.0.100, 8.0, 7.0.400"), "{o}");
        assert!(matches!(c.package_versions("dockerhub", "bad").await, Err(WebError::Parse(_))));
        assert!(matches!(c.package_versions("npm", "inexistente").await, Err(WebError::Status(404))));
    }

    #[test]
    fn only_prereleases_falls_back_and_limits_to_fifteen() {
        let v: Vec<String> = ["2.0.0-beta.2", "2.0.0-beta.1"].iter().map(|s| s.to_string()).collect();
        let o = summarize("npm", "x", "u", v, None, false);
        assert!(o.contains("Última versão estável: 2.0.0-beta.2"), "{o}");
        let many: Vec<String> = (1..=30).map(|i| format!("1.{i}.0")).collect();
        let o = summarize("npm", "x", "u", many, None, false);
        assert!(o.contains("1.30.0, 1.29.0") && !o.contains("1.15.0,") && o.contains("1.16.0"), "{o}");
    }
}
