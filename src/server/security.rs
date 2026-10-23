//! Middlewares de segurança: validação de `Host`/`Origin` (DNS rebinding e CSRF),
//! `Content-Type` em métodos mutantes e cabeçalhos de resposta.

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use serde_json::{json, Map};

use super::error::{params, ApiError};
use super::state::AppState;

/// Hosts aceitos para a porta dada. Porta 80 também aceita a forma sem porta.
pub fn allowed_hosts(port: u16) -> Vec<String> {
    let mut hosts = vec![
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    if port == 80 {
        hosts.extend(["127.0.0.1", "localhost", "[::1]"].map(String::from));
    }
    hosts
}

/// Política de `Host` do servidor, decidida em um único ponto (`host_allowed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostPolicy {
    /// Apenas nomes de loopback.
    #[default]
    Loopback,
    /// Modo `--lan`: loopback e IP literal (v4 ou v6 entre colchetes).
    Lan,
}

/// Decide se o valor de `Host` é aceito para a porta e a política dadas.
pub fn host_allowed(host: &str, port: u16, policy: HostPolicy) -> bool {
    if allowed_hosts(port)
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(host))
    {
        return true;
    }
    if policy != HostPolicy::Lan {
        return false;
    }
    let (ip_ok, rest) = if let Some(inner) = host.strip_prefix('[') {
        match inner.split_once(']') {
            Some((ip, rest)) => (ip.parse::<std::net::Ipv6Addr>().is_ok(), rest),
            None => return false,
        }
    } else {
        match host.find(':') {
            Some(i) => (host[..i].parse::<std::net::Ipv4Addr>().is_ok(), &host[i..]),
            None => (host.parse::<std::net::Ipv4Addr>().is_ok(), ""),
        }
    };
    ip_ok && (rest == format!(":{port}") || (rest.is_empty() && port == 80))
}

/// Código (spec 102) do 403 de `Host` ausente, repetido ou não permitido.
pub const CODE_HOST_FORBIDDEN: &str = "host_forbidden";
/// Código (spec 102) do 403 de `Origin` repetida, inválida ou diferente do `Host`.
pub const CODE_ORIGIN_FORBIDDEN: &str = "origin_forbidden";
/// Código (spec 102) do 415 de `Content-Type` que não é JSON.
pub const CODE_CONTENT_TYPE: &str = "content_type_json";
/// Código (spec 102) do 413 do limite de corpo, devolvido pelo `finalize`.
pub const CODE_TOO_LARGE: &str = "request_too_large";
/// Código (spec 102) dos demais erros sem corpo JSON que o `finalize` completa (`params.status`).
pub const CODE_HTTP: &str = "http_error";

/// Exige `Host` permitido e, quando `Origin` estiver presente, que seja
/// `http://<mesmo host>`. Qualquer falha resulta em 403.
pub fn check_host_and_origin(headers: &HeaderMap, port: u16) -> Result<(), ApiError> {
    check_host_and_origin_with(headers, port, HostPolicy::Loopback)
}

/// Como `check_host_and_origin`, com a política de `Host` explícita.
pub fn check_host_and_origin_with(
    headers: &HeaderMap,
    port: u16,
    policy: HostPolicy,
) -> Result<(), ApiError> {
    let forbidden = || ApiError::coded(StatusCode::FORBIDDEN, CODE_HOST_FORBIDDEN, Map::new());
    let bad_origin = || ApiError::coded(StatusCode::FORBIDDEN, CODE_ORIGIN_FORBIDDEN, Map::new());
    // Exige exatamente um Host: valores repetidos podem enganar proxies/validadores.
    let mut hosts = headers.get_all(header::HOST).iter();
    let host = match (hosts.next(), hosts.next()) {
        (Some(v), None) => v
            .to_str()
            .map_err(|_| forbidden())?,
        _ => return Err(forbidden()),
    };
    if !host_allowed(host, port, policy) {
        return Err(forbidden());
    }
    // Origin é opcional, mas no máximo um valor, idêntico ao Host.
    let mut origins = headers.get_all(header::ORIGIN).iter();
    if let Some(origin) = origins.next() {
        if origins.next().is_some() {
            return Err(bad_origin());
        }
        let origin = origin
            .to_str()
            .map_err(|_| bad_origin())?;
        if !origin.eq_ignore_ascii_case(&format!("http://{host}")) {
            return Err(bad_origin());
        }
    }
    Ok(())
}

/// Métodos mutantes exigem `Content-Type: application/json` (parâmetros são ignorados).
pub fn check_content_type(method: &Method, headers: &HeaderMap) -> Result<(), ApiError> {
    let mutating = matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    if !mutating || is_json(headers) {
        return Ok(());
    }
    Err(ApiError::coded(StatusCode::UNSUPPORTED_MEDIA_TYPE, CODE_CONTENT_TYPE, Map::new()))
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
        })
}

/// Middleware de validação aplicado a todas as rotas.
pub async fn guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    // Upload de imagem do chat (spec 082 T05): o corpo é a imagem, com `Content-Type` de imagem
    // (também não é tipo "simples" de CORS, então segue exigindo preflight).
    let image_upload = req.method() == Method::POST
        && super::chat_attachments::is_upload_path(req.uri().path())
        && super::chat_attachments::is_upload_content_type(req.headers());
    let checked = check_host_and_origin_with(req.headers(), state.bound_port(), state.host_policy())
        .and_then(|()| if image_upload { Ok(()) } else { check_content_type(req.method(), req.headers()) });
    match checked {
        Ok(()) => next.run(req).await,
        Err(err) => err.into_response(),
    }
}

/// Middleware externo: cabeçalhos de segurança em toda resposta e erros
/// sempre em JSON (inclui rejeições do axum, como 413 do limite de corpo).
pub async fn finalize(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let status = resp.status();
    if (status.is_client_error() || status.is_server_error()) && !is_json_response(&resp) {
        resp = if status == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::coded(status, CODE_TOO_LARGE, Map::new())
        } else {
            ApiError::coded(status, CODE_HTTP, params([("status", json!(status.as_u16()))]))
        }
        .into_response();
    }
    let headers = resp.headers_mut();
    // Respostas estáticas (static_ui) já definem Cache-Control; API segue no-store.
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp
}

fn is_json_response(resp: &Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.append(
                header::HeaderName::from_bytes(k.as_bytes()).expect("nome"),
                HeaderValue::from_str(v).expect("valor"),
            );
        }
        map
    }

    #[test]
    fn host_accepts_loopback_names_with_port() {
        for host in ["127.0.0.1:3000", "localhost:3000", "[::1]:3000"] {
            let h = headers(&[("host", host)]);
            assert!(check_host_and_origin(&h, 3000).is_ok(), "{host}");
        }
    }

    #[test]
    fn host_rejects_foreign_or_portless_names() {
        for host in ["evil.com:3000", "127.0.0.1", "localhost:3001", "192.168.0.10:3000"] {
            let h = headers(&[("host", host)]);
            let err = check_host_and_origin(&h, 3000).expect_err(host);
            assert_eq!(err.status(), StatusCode::FORBIDDEN, "{host}");
        }
    }

    #[test]
    fn lan_policy_matrix() {
        let lan = |h: &str, o: Option<&str>| {
            let mut pairs = vec![("host", h)];
            if let Some(o) = o {
                pairs.push(("origin", o));
            }
            check_host_and_origin_with(&headers(&pairs), 3000, HostPolicy::Lan).is_ok()
        };
        assert!(lan("192.168.1.50:3000", None));
        assert!(lan("[fe80::1]:3000", None));
        assert!(lan("localhost:3000", None));
        assert!(lan("192.168.1.50:3000", Some("http://192.168.1.50:3000")));
        assert!(!lan("192.168.1.50:3000", Some("http://192.168.1.51:3000")));
        assert!(!lan("192.168.1.50:3000", Some("http://evil.example")));
        assert!(!lan("evil.example:3000", None));
        assert!(!lan("192.168.1.50:3001", None));
        assert!(!lan("192.168.1.50", None));
        assert!(!lan("[fe80::1]", None));
        assert!(!lan("[nope]:3000", None));
        assert!(!lan("192.168.1.50.evil.com:3000", None));
        let mut dup = HeaderMap::new();
        dup.append(header::HOST, HeaderValue::from_static("192.168.1.50:3000"));
        dup.append(header::HOST, HeaderValue::from_static("192.168.1.50:3000"));
        assert!(check_host_and_origin_with(&dup, 3000, HostPolicy::Lan).is_err());
        assert!(host_allowed("10.0.0.1", 80, HostPolicy::Lan));
        assert!(!host_allowed("192.168.1.50:3000", 3000, HostPolicy::Loopback));
    }

    #[test]
    fn forbidden_host_and_origin_carry_stable_codes() {
        let bad_host = check_host_and_origin(&headers(&[("host", "evil.com:3000")]), 3000).expect_err("host");
        assert_eq!(bad_host.code(), Some(CODE_HOST_FORBIDDEN));
        assert_eq!(check_host_and_origin(&HeaderMap::new(), 3000).expect_err("sem host").code(), Some(CODE_HOST_FORBIDDEN));
        let cross = headers(&[("host", "127.0.0.1:3000"), ("origin", "http://evil.com")]);
        let err = check_host_and_origin(&cross, 3000).expect_err("origin");
        assert_eq!((err.status(), err.code()), (StatusCode::FORBIDDEN, Some(CODE_ORIGIN_FORBIDDEN)));
        assert_eq!(err.message(), "Origin não permitida", "o texto pt-BR continua como reserva");
        let ct = check_content_type(&Method::POST, &HeaderMap::new()).expect_err("415");
        assert_eq!(ct.code(), Some(CODE_CONTENT_TYPE));
    }

    async fn finalized(status: StatusCode) -> serde_json::Value {
        use axum::routing::post;
        use tower::ServiceExt;
        let app = axum::Router::new()
            .route("/x", post(move || async move { status }))
            .layer(axum::middleware::from_fn(finalize));
        let req = axum::http::Request::post("/x").body(axum::body::Body::empty()).expect("req");
        let resp = app.oneshot(req).await.expect("resposta");
        assert_eq!(resp.status(), status);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16).await.expect("corpo");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn finalize_turns_bare_errors_into_json_with_code() {
        assert_eq!(
            finalized(StatusCode::PAYLOAD_TOO_LARGE).await,
            json!({"error": "pedido grande demais", "code": CODE_TOO_LARGE})
        );
        assert_eq!(
            finalized(StatusCode::METHOD_NOT_ALLOWED).await,
            json!({"error": "erro HTTP 405", "code": CODE_HTTP, "params": {"status": 405}})
        );
    }

    #[test]
    fn host_is_required() {
        let err = check_host_and_origin(&HeaderMap::new(), 3000).expect_err("sem host");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn port_80_also_accepts_hosts_without_port() {
        let h = headers(&[("host", "localhost")]);
        assert!(check_host_and_origin(&h, 80).is_ok());
        let h = headers(&[("host", "localhost")]);
        assert!(check_host_and_origin(&h, 3000).is_err());
    }

    #[test]
    fn origin_must_match_host() {
        let same = headers(&[("host", "127.0.0.1:3000"), ("origin", "http://127.0.0.1:3000")]);
        assert!(check_host_and_origin(&same, 3000).is_ok());
        let cross = headers(&[("host", "127.0.0.1:3000"), ("origin", "http://evil.com")]);
        assert!(check_host_and_origin(&cross, 3000).is_err());
        let https = headers(&[("host", "127.0.0.1:3000"), ("origin", "https://127.0.0.1:3000")]);
        assert!(check_host_and_origin(&https, 3000).is_err());
        let null = headers(&[("host", "127.0.0.1:3000"), ("origin", "null")]);
        assert!(check_host_and_origin(&null, 3000).is_err());
    }

    #[test]
    fn mutating_methods_require_json_content_type() {
        let json = headers(&[("content-type", "application/json; charset=utf-8")]);
        assert!(check_content_type(&Method::PUT, &json).is_ok());
        let text = headers(&[("content-type", "text/plain")]);
        let err = check_content_type(&Method::PUT, &text).expect_err("text");
        assert_eq!(err.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(check_content_type(&Method::POST, &HeaderMap::new()).is_err());
        assert!(check_content_type(&Method::DELETE, &HeaderMap::new()).is_err());
        assert!(check_content_type(&Method::GET, &HeaderMap::new()).is_ok());
    }

    #[test]
    fn duplicate_host_is_forbidden_even_when_first_is_valid() {
        let mut h = HeaderMap::new();
        h.append(header::HOST, HeaderValue::from_static("127.0.0.1:3000"));
        h.append(header::HOST, HeaderValue::from_static("evil.com:3000"));
        assert!(check_host_and_origin(&h, 3000).is_err());
        let mut same = HeaderMap::new();
        same.append(header::HOST, HeaderValue::from_static("127.0.0.1:3000"));
        same.append(header::HOST, HeaderValue::from_static("127.0.0.1:3000"));
        assert!(check_host_and_origin(&same, 3000).is_err());
    }

    #[test]
    fn duplicate_origin_is_forbidden() {
        let mut h = HeaderMap::new();
        h.append(header::HOST, HeaderValue::from_static("127.0.0.1:3000"));
        h.append(header::ORIGIN, HeaderValue::from_static("http://127.0.0.1:3000"));
        h.append(header::ORIGIN, HeaderValue::from_static("http://127.0.0.1:3000"));
        assert!(check_host_and_origin(&h, 3000).is_err());
    }
}
