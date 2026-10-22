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
            while let Some(chunk) = resp.chunk().await.map_err(|e| WebError::Request(e.without_url().to_string()))? {
                let room = cap - body.len();
                if chunk.len() >= room {
                    body.extend_from_slice(&chunk[..room]);
                    break;
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(Fetched { content_type, body });
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

fn textual(ct: &str) -> bool {
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

/// Converte HTML em texto legível: remove script/style/comentários/tags e colapsa espaços.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        if rest.starts_with('<') {
            if lower[i..].starts_with("<!--") {
                i = lower[i + 4..].find("-->").map(|p| i + 4 + p + 3).unwrap_or(html.len());
                continue;
            }
            let next = rest[1..].chars().next();
            if !matches!(next, Some(c) if c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?') {
                out.push('<');
                i += 1;
                continue;
            }
            let name: String = lower[i + 1..]
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let closing = lower[i + 1..].starts_with('/');
            if !closing && matches!(name.as_str(), "script" | "style" | "noscript" | "template" | "svg") {
                let close = format!("</{name}");
                let after = lower[i..].find(&close).map(|p| i + p);
                i = match after {
                    Some(p) => lower[p..].find('>').map(|q| p + q + 1).unwrap_or(html.len()),
                    None => html.len(),
                };
                continue;
            }
            if BLOCK_TAGS.contains(&name.as_str()) {
                out.push('\n');
            }
            i = match rest.find('>') {
                Some(p) => i + p + 1,
                None => html.len(),
            };
        } else if let Some(c) = rest.chars().next() {
            out.push(c);
            i += c.len_utf8();
        } else {
            break;
        }
    }
    let decoded = decode_entities(&out);
    let mut lines: Vec<String> = Vec::new();
    for line in decoded.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() && lines.last().map_or(true, String::is_empty) {
            continue;
        }
        lines.push(collapsed);
    }
    lines.join("\n").trim().to_string()
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
        assert_eq!(t, "T\nOlá mundo\n\num & dois <b>\n\nab");
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
