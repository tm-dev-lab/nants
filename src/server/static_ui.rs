//! Serve a SPA embutida no binário (`web/dist`, via rust-embed).
//!
//! Regras: `/` e rotas de cliente (caminhos sem extensão) recebem `index.html`;
//! caminhos com extensão inexistentes dão 404; `/api/*` nunca cai no index;
//! caminhos com segmentos ocultos, vazios, `..`, barra invertida ou NUL são rejeitados.

use std::borrow::Cow;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::{EmbeddedFile, RustEmbed};

use serde_json::Map;
use super::error::ApiError;

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

/// Arquivo estático servido, com o SHA-256 do conteúdo usado para o ETag.
#[derive(Debug, Clone)]
pub struct Asset {
    pub data: Cow<'static, [u8]>,
    pub sha256: [u8; 32],
}

impl From<EmbeddedFile> for Asset {
    fn from(file: EmbeddedFile) -> Self {
        Self {
            data: file.data,
            sha256: file.metadata.sha256_hash(),
        }
    }
}

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
font-src 'self'; manifest-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; \
frame-ancestors 'none'; form-action 'self'";

/// CSP das respostas HTML da UI (spec 093 T01): a base mais `frame-src http://<host>:*`, para a
/// janela App embutir o app do projeto (portas publicadas no mesmo host da página). O `<host>` vem
/// de [`super::app::host_name`] sobre o `Host` já validado pelo `security::guard`; um valor com
/// caractere fora de nome/IP (só possível sem o guard) cai no loopback e nunca quebra a diretiva.
pub fn csp_for_host(host: &str) -> String {
    let safe = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'));
    let host = if safe { host } else { super::app::FALLBACK_HOST };
    CSP.replacen("frame-ancestors", &format!("frame-src http://{host}:*; frame-ancestors"), 1)
}

/// Host da CSP a partir dos cabeçalhos: o `Host` (sem porta) ou o loopback.
fn frame_host(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map_or_else(|| super::app::FALLBACK_HOST.to_string(), super::app::host_name)
}

const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";
const CACHE_REVALIDATE: &str = "no-cache";

/// Handler de fallback do roteador: serve a SPA e os arquivos de `web/dist`.
pub async fn static_handler(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok());
    let host = frame_host(&headers);
    serve_asset(|path| Assets::get(path).map(Asset::from), &method, uri.path(), if_none_match, &host)
}

/// Lógica de roteamento estático com lookup injetável (testável sem o embed). `frame_host` é o host
/// da diretiva `frame-src` da CSP das respostas HTML.
pub fn serve_asset<F>(get: F, method: &Method, raw_path: &str, if_none_match: Option<&str>, frame_host: &str) -> Response
where
    F: Fn(&str) -> Option<Asset>,
{
    if *method != Method::GET && *method != Method::HEAD {
        return ApiError::coded(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed", Map::new()).into_response();
    }
    let not_found = || ApiError::coded(StatusCode::NOT_FOUND, "not_found", Map::new()).into_response();

    // `%2f` no caminho cru é rejeitado antes de decodificar: barra codificada não vira separador.
    if has_encoded_slash(raw_path) {
        return not_found();
    }
    let Some(decoded) = percent_decode(raw_path) else {
        return not_found();
    };
    let rel = decoded.strip_prefix('/').unwrap_or(&decoded);
    if rel == "api" || rel.starts_with("api/") {
        return not_found();
    }

    let name: &str = if rel.is_empty() {
        "index.html"
    } else {
        if !is_safe_path(rel) {
            return not_found();
        }
        rel
    };

    if let Some(asset) = get(name) {
        return respond(asset, name, method, if_none_match, frame_host);
    }
    // Último segmento com ponto = arquivo: não existe, então 404 (sem fallback).
    let last = name.rsplit('/').next().unwrap_or(name);
    if last.contains('.') {
        return not_found();
    }
    match get("index.html") {
        Some(index) => respond(index, "index.html", method, if_none_match, frame_host),
        None => not_found(),
    }
}

/// Monta a resposta de um arquivo: cabeçalhos, ETag/304 e corpo (vazio em HEAD). A CSP vai em todo
/// HTML, inclusive no 304.
fn respond(asset: Asset, name: &str, method: &Method, if_none_match: Option<&str>, frame_host: &str) -> Response {
    let is_html = name.ends_with(".html");
    let etag = etag_of(&asset.sha256);
    let cache = if name.starts_with("assets/") {
        CACHE_IMMUTABLE
    } else {
        CACHE_REVALIDATE
    };

    let mut resp = if etag_matches(if_none_match, &etag) {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::NOT_MODIFIED;
        r
    } else {
        let len = asset.data.len();
        let body = if *method == Method::HEAD {
            Body::empty()
        } else {
            Body::from(asset.data.into_owned())
        };
        let mut r = Response::new(body);
        let h = r.headers_mut();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(name)));
        if let Ok(v) = HeaderValue::from_str(&len.to_string()) {
            h.insert(header::CONTENT_LENGTH, v);
        }
        r
    };

    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    if is_html {
        let csp = HeaderValue::from_str(&csp_for_host(frame_host)).unwrap_or_else(|_| HeaderValue::from_static(CSP));
        h.insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    resp
}

/// ETag fraco de 16 hex (8 bytes do SHA-256 do conteúdo), entre aspas.
fn etag_of(sha256: &[u8; 32]) -> String {
    let hex: String = sha256[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

fn etag_matches(if_none_match: Option<&str>, etag: &str) -> bool {
    let Some(value) = if_none_match else {
        return false;
    };
    value.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

/// Decodifica percent-encoding; `None` se houver escape inválido ou UTF-8 inválido.
fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes.get(i + 1).copied().and_then(hex_val)?;
            let lo = bytes.get(i + 2).copied().and_then(hex_val)?;
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `true` se o caminho cru contém `%2f` ou `%2F` (case-insensitive).
fn has_encoded_slash(raw: &str) -> bool {
    raw.as_bytes().windows(3).any(|w| {
        w[0] == b'%' && w[1] == b'2' && (w[2] == b'f' || w[2] == b'F')
    })
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Segmentos não vazios, sem ocultos (`.`), sem `..`, sem `\` nem NUL.
fn is_safe_path(rel: &str) -> bool {
    !rel.contains(['\\', '\0'])
        && rel
            .split('/')
            .all(|seg| !seg.is_empty() && !seg.starts_with('.'))
}

/// Content-Type pela extensão; texto recebe `charset=utf-8`.
fn content_type(name: &str) -> &'static str {
    let ext = name
        .rsplit('.')
        .next()
        .filter(|e| *e != name)
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    const INDEX: &[u8] = b"<!doctype html><html><body><div id=\"root\"></div></body></html>";
    const JS: &[u8] = b"console.log('ok');";

    fn asset(data: &'static [u8], tag: u8) -> Asset {
        Asset {
            data: Cow::Borrowed(data),
            sha256: [tag; 32],
        }
    }

    /// Mapa em memória: index, um asset com hash e um arquivo na raiz.
    fn lookup(name: &str) -> Option<Asset> {
        match name {
            "index.html" => Some(asset(INDEX, 1)),
            "assets/app.js" => Some(asset(JS, 2)),
            "robots.txt" => Some(asset(b"User-agent: *", 3)),
            _ => None,
        }
    }

    fn get(path: &str) -> Response {
        serve_asset(lookup, &Method::GET, path, None, "127.0.0.1")
    }

    async fn text(resp: Response) -> String {
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
        String::from_utf8_lossy(&bytes).to_string()
    }

    fn header_str(resp: &Response, name: header::HeaderName) -> String {
        resp.headers()[name].to_str().expect("ascii").to_string()
    }

    #[tokio::test]
    async fn root_serves_html_with_csp() {
        let resp = get("/");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(header_str(&resp, header::CONTENT_TYPE), "text/html; charset=utf-8");
        assert_eq!(header_str(&resp, header::CACHE_CONTROL), "no-cache");
        assert_eq!(header_str(&resp, header::REFERRER_POLICY), "no-referrer");
        let csp = header_str(&resp, header::CONTENT_SECURITY_POLICY);
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(!csp.contains("unsafe-inline"));
        assert_eq!(text(resp).await.as_bytes(), INDEX);
    }

    #[tokio::test]
    async fn asset_served_with_js_type_and_immutable_cache_without_csp() {
        let resp = get("/assets/app.js");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            header_str(&resp, header::CONTENT_TYPE),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            header_str(&resp, header::CACHE_CONTROL),
            "public, max-age=31536000, immutable"
        );
        assert!(resp.headers().get(header::CONTENT_SECURITY_POLICY).is_none());
        assert_eq!(text(resp).await.as_bytes(), JS);
    }

    #[tokio::test]
    async fn etag_is_quoted_16_hex_and_304_on_match() {
        let resp = get("/assets/app.js");
        let etag = header_str(&resp, header::ETAG);
        assert_eq!(etag, "\"0202020202020202\"");

        let hit = serve_asset(lookup, &Method::GET, "/assets/app.js", Some(&etag), "127.0.0.1");
        assert_eq!(hit.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(text(hit).await, "");

        let weak = format!("W/{etag}, \"outro\"");
        let hit = serve_asset(lookup, &Method::GET, "/assets/app.js", Some(&weak), "127.0.0.1");
        assert_eq!(hit.status(), StatusCode::NOT_MODIFIED);

        let miss = serve_asset(lookup, &Method::GET, "/assets/app.js", Some("\"outro\""), "127.0.0.1");
        assert_eq!(miss.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn space_url_serves_index_with_200() {
        // O URL da interface só tem o espaço ativo (`/?space=<id>`); a consulta não é caminho.
        let uri: Uri = "/?space=p1".parse().expect("uri");
        let resp = get(uri.path());
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(header_str(&resp, header::CONTENT_TYPE), "text/html; charset=utf-8");
        assert!(resp.headers().get(header::CONTENT_SECURITY_POLICY).is_some());
        assert_eq!(text(resp).await.as_bytes(), INDEX);
    }

    #[tokio::test]
    async fn unknown_path_falls_back_to_index_with_200() {
        // Qualquer outro caminho cai no index: o cliente o normaliza para `/` com `replaceState`.
        let resp = get("/um/caminho/qualquer");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(header_str(&resp, header::CONTENT_TYPE), "text/html; charset=utf-8");
        assert_eq!(text(resp).await.as_bytes(), INDEX);
    }

    #[tokio::test]
    async fn api_paths_never_fall_back_to_index() {
        for path in ["/api", "/api/nope", "/api/", "/%61pi/nope"] {
            let resp = get(path);
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(
                header_str(&resp, header::CONTENT_TYPE),
                "application/json",
                "{path}"
            );
            assert_eq!(text(resp).await, r#"{"code":"not_found","error":"não encontrado"}"#, "{path}");
        }
    }

    #[tokio::test]
    async fn missing_file_with_extension_is_404_not_index() {
        let resp = get("/missing.js");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(header_str(&resp, header::CONTENT_TYPE), "application/json");
    }

    #[tokio::test]
    async fn traversal_hidden_and_empty_segments_are_404() {
        for path in [
            "/../Cargo.toml",
            "/%2e%2e/etc",
            "/%2E%2E/etc",
            "/..%2fetc",
            "/assets%2fapp.js",
            "/assets%2Fapp.js",
            "/a%2fb",
            "/a%2Fb",
            "/a/b%2f",
            "/a//b",
            "/assets/",
            "/.git/config",
            "/assets/.hidden",
            "/assets/../index.html",
            "/assets\\..\\x",
            "/a%5cb",
            "/a%00b",
            "/%FF",
            "/%zz",
        ] {
            let resp = get(path);
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn post_is_405_json() {
        let resp = serve_asset(lookup, &Method::POST, "/", None, "127.0.0.1");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(header_str(&resp, header::CONTENT_TYPE), "application/json");
        let resp = serve_asset(lookup, &Method::OPTIONS, "/", None, "127.0.0.1");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn head_has_headers_without_body() {
        let resp = serve_asset(lookup, &Method::HEAD, "/", None, "127.0.0.1");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(header_str(&resp, header::CONTENT_TYPE), "text/html; charset=utf-8");
        assert_eq!(header_str(&resp, header::CONTENT_LENGTH), INDEX.len().to_string());
        assert_eq!(text(resp).await, "");
    }

    #[tokio::test]
    async fn static_handler_serves_embedded_index() {
        let resp = static_handler(Method::GET, "/".parse().expect("uri"), HeaderMap::new()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = text(resp).await;
        assert!(body.contains("<title>Nants</title>"));
        assert!(body.contains("<div id=\"root\">"));
        // Só script externo com src (sem inline), compatível com a CSP `script-src 'self'`.
        assert!(!body.contains("<script>"));
        assert!(!body.contains("<script type=\"module\">"));
        for tag in body.match_indices("<script") {
            assert!(body[tag.0..].split('>').next().is_some_and(|t| t.contains("src=\"/assets/")));
        }
    }

    #[tokio::test]
    async fn embedded_brand_assets_are_served_with_correct_types() {
        for (path, ctype) in [
            ("/brand/favicon.svg", "image/svg+xml; charset=utf-8"),
            ("/brand/favicon.ico", "image/x-icon"),
            ("/brand/icon-192.png", "image/png"),
            ("/brand/apple-touch-icon.png", "image/png"),
            ("/brand/site.webmanifest", "application/manifest+json"),
        ] {
            let resp = static_handler(Method::GET, path.parse().expect("uri"), HeaderMap::new()).await;
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            assert_eq!(header_str(&resp, header::CONTENT_TYPE), ctype, "{path}");
        }
    }

    #[test]
    fn csp_allows_same_origin_images_fonts_and_manifest() {
        for d in ["img-src 'self'", "font-src 'self'", "manifest-src 'self'"] {
            assert!(CSP.contains(d), "{d}");
        }
    }

    /// Cabeçalhos com o `Host` dado.
    fn host_headers(host: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(host).expect("host"));
        h
    }

    #[tokio::test]
    async fn html_csp_has_frame_src_from_host_ipv4_localhost_and_port() {
        for (host, expected) in [
            ("192.168.0.68:3000", "frame-src http://192.168.0.68:*;"),
            ("localhost:3000", "frame-src http://localhost:*;"),
            ("127.0.0.1", "frame-src http://127.0.0.1:*;"),
            ("[::1]:3000", "frame-src http://[::1]:*;"),
        ] {
            let resp = static_handler(Method::GET, "/".parse().expect("uri"), host_headers(host)).await;
            assert_eq!(resp.status(), StatusCode::OK, "{host}");
            let csp = header_str(&resp, header::CONTENT_SECURITY_POLICY);
            assert!(csp.contains(expected), "{host}: {csp}");
            // O restante da CSP não muda.
            assert_eq!(csp.replace(&format!("{expected} "), ""), CSP, "{host}");
            assert!(csp.contains("frame-ancestors 'none'"));
        }
        // Sem `Host`: loopback.
        let resp = static_handler(Method::GET, "/".parse().expect("uri"), HeaderMap::new()).await;
        assert!(header_str(&resp, header::CONTENT_SECURITY_POLICY).contains("frame-src http://127.0.0.1:*;"));
    }

    #[tokio::test]
    async fn not_modified_index_keeps_csp_with_frame_src() {
        let first = serve_asset(lookup, &Method::GET, "/", None, "192.168.0.68");
        let etag = header_str(&first, header::ETAG);
        let hit = serve_asset(lookup, &Method::GET, "/", Some(&etag), "192.168.0.68");
        assert_eq!(hit.status(), StatusCode::NOT_MODIFIED);
        let csp = header_str(&hit, header::CONTENT_SECURITY_POLICY);
        assert!(csp.contains("frame-src http://192.168.0.68:*;"), "{csp}");
        assert!(csp.contains("frame-ancestors 'none'"));
        // Rota de cliente com 304 também (cai no index).
        let hit = serve_asset(lookup, &Method::GET, "/qualquer-caminho", Some(&etag), "localhost");
        assert_eq!(hit.status(), StatusCode::NOT_MODIFIED);
        assert!(header_str(&hit, header::CONTENT_SECURITY_POLICY).contains("frame-src http://localhost:*;"));
        // Asset não-HTML continua sem CSP.
        let js = serve_asset(lookup, &Method::GET, "/assets/app.js", None, "localhost");
        assert!(js.headers().get(header::CONTENT_SECURITY_POLICY).is_none());
    }

    #[test]
    fn csp_host_with_unexpected_chars_falls_back_to_loopback() {
        for bad in ["evil.com; script-src *", "a b", "", "x'y"] {
            let csp = csp_for_host(bad);
            assert!(csp.contains("frame-src http://127.0.0.1:*;"), "{bad}: {csp}");
            assert!(!csp.contains("evil"), "{bad}");
        }
    }

    #[test]
    fn content_type_table() {
        assert_eq!(content_type("a.png"), "image/png");
        assert_eq!(content_type("a.JS"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("a.svg"), "image/svg+xml; charset=utf-8");
        assert_eq!(content_type("a.ico"), "image/x-icon");
        assert_eq!(content_type("a.woff2"), "font/woff2");
        assert_eq!(content_type("brand/site.webmanifest"), "application/manifest+json");
        assert_eq!(content_type("a.bin"), "application/octet-stream");
        assert_eq!(content_type("noext"), "application/octet-stream");
    }
}
