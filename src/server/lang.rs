//! Idioma da interface a cada turno (spec 102, decisão 3).
//!
//! Contrato: o cliente manda o idioma ativo da interface no cabeçalho `X-Nants-Lang` (`pt-BR` ou
//! `en`) em todas as requisições. Nas que mudam algo (`POST`, `PUT`, `DELETE`), como o turno do
//! Arquiteto (`POST /api/projects/:id/chat`) e as da execução (aprovar, retomar, decidir), o
//! servidor guarda o idioma como o ativo do projeto do caminho (`/api/projects/:id/...`) e como o
//! último informado. Os agentes leem o idioma do projeto a cada mensagem, então a troca vale para as
//! próximas. Valor ausente ou desconhecido não muda nada; sem nenhum idioma informado, pt-BR.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::Response;

use super::events::validate_project_id;
use super::state::AppState;
use crate::i18n::Lang;

/// Cabeçalho com o idioma ativo da interface.
pub const LANG_HEADER: &str = "x-nants-lang";

/// Idioma do cabeçalho (`pt-BR` ou `en`); exige um único valor válido.
pub fn header_lang(headers: &HeaderMap) -> Option<Lang> {
    let mut values = headers.get_all(LANG_HEADER).iter();
    match (values.next(), values.next()) {
        (Some(v), None) => v.to_str().ok().and_then(Lang::from_tag),
        _ => None,
    }
}

/// Projeto do caminho `/api/projects/:id/...` (id validado), ou `None`.
pub fn path_project(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/projects/")?;
    let id = rest.split('/').next()?;
    validate_project_id(id).ok().map(|()| id)
}

/// Middleware: guarda o idioma informado nas requisições que mudam algo.
pub async fn capture(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let mutating = matches!(*req.method(), Method::POST | Method::PUT | Method::DELETE);
    if mutating {
        if let Some(lang) = header_lang(req.headers()) {
            state.set_lang(path_project(req.uri().path()), lang);
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::path::PathBuf;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(LANG_HEADER, HeaderValue::from_str(v).expect("valor"));
        }
        h
    }

    #[test]
    fn header_accepts_only_one_known_tag() {
        assert_eq!(header_lang(&headers(&["en"])), Some(Lang::En));
        assert_eq!(header_lang(&headers(&["pt-BR"])), Some(Lang::PtBr));
        assert_eq!(header_lang(&headers(&[])), None);
        assert_eq!(header_lang(&headers(&["fr"])), None);
        assert_eq!(header_lang(&headers(&["en", "en"])), None);
    }

    #[test]
    fn project_comes_from_the_path() {
        assert_eq!(path_project("/api/projects/abc/chat"), Some("abc"));
        assert_eq!(path_project("/api/projects/abc"), Some("abc"));
        assert_eq!(path_project("/api/projects"), None);
        assert_eq!(path_project("/api/config"), None);
        assert_eq!(path_project("/api/projects/../x"), None);
    }

    #[test]
    fn state_keeps_the_language_per_project_and_shares_the_handle() {
        let state = AppState::new(PathBuf::from("/tmp/nants-lang.toml"), crate::config::Config::default(), 0);
        let handle = state.project_lang("p1");
        assert_eq!(handle.get(), Lang::PtBr, "sem idioma informado: pt-BR");
        state.set_lang(Some("p1"), Lang::En);
        assert_eq!(handle.get(), Lang::En, "a troca vale para quem já tem o handle");
        assert_eq!(state.project_lang("p2").get(), Lang::En, "projeto novo herda o último informado");
        state.set_lang(None, Lang::PtBr);
        assert_eq!(state.project_lang("p1").get(), Lang::En, "global não muda o do projeto");
        assert_eq!(state.project_lang("p3").get(), Lang::PtBr);
    }

    #[tokio::test]
    async fn middleware_records_the_language_only_on_mutating_requests() {
        use axum::routing::{get, post};
        use tower::ServiceExt;
        let state = AppState::new(PathBuf::from("/tmp/nants-lang2.toml"), crate::config::Config::default(), 0);
        let app = axum::Router::new()
            .route("/api/projects/:id/chat", post(|| async { "ok" }).get(|| async { "ok" }))
            .route("/api/projects/:id/state", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(state.clone(), capture))
            .with_state(state.clone());
        let send = |method: &str, path: &str, lang: &str| {
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header(LANG_HEADER, lang)
                .body(axum::body::Body::empty())
                .expect("req")
        };
        app.clone().oneshot(send("GET", "/api/projects/p1/state", "en")).await.expect("get");
        assert_eq!(state.project_lang("p1").get(), Lang::PtBr, "GET não muda o idioma");
        app.clone().oneshot(send("POST", "/api/projects/p1/chat", "en")).await.expect("post");
        assert_eq!(state.project_lang("p1").get(), Lang::En);
        app.oneshot(send("POST", "/api/projects/p1/chat", "xx")).await.expect("post");
        assert_eq!(state.project_lang("p1").get(), Lang::En, "valor desconhecido não muda nada");
    }
}
