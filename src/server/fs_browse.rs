//! Navegação de filesystem para escolher projetos: `GET /api/fs/browse`.
//!
//! Lista apenas subdiretórios (nunca arquivos) do caminho canônico. Ocultos só com
//! `hidden=true`. Symlinks são ignorados (não são seguidos nem listados). Erros do SO
//! viram mensagens estáticas; diretórios de sistema são recusados.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::events::SecretSet;
use super::projects::{
    is_git_repo, is_system_dir, resolve_dir, scrub_text, text_field, validate_path_shape,
    DirProblem,
};
use super::state::AppState;

/// Máximo de entradas devolvidas por listagem.
pub const MAX_ENTRIES: usize = 500;

#[derive(Deserialize)]
pub struct BrowseQuery {
    path: Option<String>,
    #[serde(default)]
    hidden: bool,
}

#[derive(Serialize)]
pub struct BrowseView {
    path: String,
    parent: Option<String>,
    entries: Vec<EntryView>,
    truncated: bool,
}

#[derive(Serialize)]
pub struct EntryView {
    name: String,
    path: String,
    is_git_repo: bool,
}

/// Diretório inicial: `$HOME` quando definido e absoluto; senão `/`.
fn home_path() -> String {
    match env::var_os("HOME") {
        Some(home) if !home.is_empty() && Path::new(&home).is_absolute() => {
            home.to_string_lossy().into_owned()
        }
        _ => "/".to_string(),
    }
}

fn map_browse_problem(problem: DirProblem) -> ApiError {
    match problem {
        DirProblem::Missing => ApiError::new(StatusCode::NOT_FOUND, "diretório não encontrado"),
        DirProblem::Denied => {
            ApiError::new(StatusCode::FORBIDDEN, "sem permissão para listar o diretório")
        }
        DirProblem::NotDir => ApiError::new(StatusCode::BAD_REQUEST, "caminho não é um diretório"),
        DirProblem::Invalid => ApiError::new(StatusCode::BAD_REQUEST, "caminho inválido"),
    }
}

fn map_read_error(e: io::Error) -> ApiError {
    match e.kind() {
        io::ErrorKind::NotFound => ApiError::new(StatusCode::NOT_FOUND, "diretório não encontrado"),
        io::ErrorKind::PermissionDenied => {
            ApiError::new(StatusCode::FORBIDDEN, "sem permissão para listar o diretório")
        }
        _ => ApiError::new(StatusCode::BAD_REQUEST, "não foi possível listar o diretório"),
    }
}

/// Lista os subdiretórios de `raw` (canonicalizado), ordenados por nome.
fn list_dir(raw: &Path, hidden: bool, secrets: &SecretSet) -> Result<BrowseView, ApiError> {
    let canonical = resolve_dir(raw).map_err(map_browse_problem)?;
    if is_system_dir(&canonical) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "diretório de sistema não é permitido"));
    }
    let read = fs::read_dir(&canonical).map_err(map_read_error)?;

    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for entry in read.flatten() {
        // `file_type` não segue symlinks: symlinks de diretório não são is_dir().
        let is_real_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if !is_real_dir {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !hidden && name.starts_with('.') {
            continue;
        }
        let child = canonical.join(name);
        if is_system_dir(&child) {
            continue;
        }
        found.push((name.to_string(), child));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let truncated = found.len() > MAX_ENTRIES;
    found.truncate(MAX_ENTRIES);

    let entries = found
        .iter()
        .filter_map(|(name, child)| {
            let path = child.to_str()?;
            Some(EntryView {
                name: text_field(secrets, "name", name),
                path: scrub_text(secrets, path),
                is_git_repo: is_git_repo(child),
            })
        })
        .collect();

    let path = canonical
        .to_str()
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "caminho inválido"))?;
    let parent = canonical
        .parent()
        .and_then(Path::to_str)
        .map(|p| scrub_text(secrets, p));
    Ok(BrowseView {
        path: scrub_text(secrets, path),
        parent,
        entries,
        truncated,
    })
}

/// `GET /api/fs/browse?path=<abs>&hidden=<bool>`
pub async fn browse(
    State(state): State<AppState>,
    Query(query): Query<BrowseQuery>,
) -> Result<Response, ApiError> {
    let raw = match query.path {
        Some(path) => {
            validate_path_shape(&path)?;
            path
        }
        None => home_path(),
    };
    let hidden = query.hidden;
    let secrets = state.secret_set().await;
    let view = tokio::task::spawn_blocking(move || list_dir(Path::new(&raw), hidden, &secrets))
        .await
        .map_err(|_| ApiError::internal("falha interna"))??;
    Ok((StatusCode::OK, Json(view)).into_response())
}

#[cfg(test)]
mod tests {
    use std::process;

    use axum::body::{Body, Bytes};
    use axum::http::{header, Method, Request, Response, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;
    use crate::config::Config;
    use crate::server::{router, AppState};

    const PORT: u16 = 3000;
    const HOST: &str = "127.0.0.1:3000";
    const SECRET: &str = "sk-SEGREDO-LITERAL-1234";

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-browse-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn state_in(dir: &Path, cfg: Config) -> AppState {
        AppState::new(dir.join("config.toml"), cfg, PORT)
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(header::HOST, HOST)
            .body(Body::empty())
            .expect("requisição")
    }

    async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, String) {
        let resp: Response<Body> = router(state.clone()).oneshot(req).await.expect("resposta");
        let status = resp.status();
        let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
            .await
            .expect("corpo");
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    fn json_of(text: &str) -> Value {
        serde_json::from_str(text).expect("json")
    }

    /// `path` como segmento de query (percent-encoding mínimo para caminhos de teste).
    fn browse_uri(path: &Path, extra: &str) -> String {
        let encoded: String = path
            .to_string_lossy()
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '/' | '~' => c.to_string(),
                other => other
                    .to_string()
                    .bytes()
                    .map(|b| format!("%{b:02X}"))
                    .collect(),
            })
            .collect();
        format!("/api/fs/browse?path={encoded}{extra}")
    }

    fn names(v: &Value) -> Vec<String> {
        v["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|e| e["name"].as_str().expect("name").to_string())
            .collect()
    }

    #[tokio::test]
    async fn lists_only_directories_sorted_with_canonical_paths() {
        let root = temp_dir("dirs");
        fs::create_dir_all(root.join("b")).expect("b");
        fs::create_dir_all(root.join("a/sub")).expect("a");
        fs::write(root.join("arquivo.txt"), "x").expect("file");
        let state = state_in(&root, Config::default());
        let (status, body) = call(&state, get_req(&browse_uri(&root, ""))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        assert_eq!(names(&v), vec!["a", "b"]);
        let canon = fs::canonicalize(&root).expect("canon");
        assert_eq!(v["path"], canon.to_str().expect("u"));
        assert_eq!(v["parent"], canon.parent().expect("pai").to_str().expect("u"));
        assert_eq!(v["truncated"], false);
        assert_eq!(v["entries"][0]["is_git_repo"], false);
        assert_eq!(
            v["entries"][0]["path"],
            canon.join("a").to_str().expect("u")
        );
    }

    #[tokio::test]
    async fn hidden_directories_only_with_flag() {
        let root = temp_dir("hidden");
        fs::create_dir_all(root.join(".oculto")).expect("h");
        fs::create_dir_all(root.join("visivel")).expect("v");
        let state = state_in(&root, Config::default());
        let (_, body) = call(&state, get_req(&browse_uri(&root, ""))).await;
        assert_eq!(names(&json_of(&body)), vec!["visivel"]);
        let (status, body) = call(&state, get_req(&browse_uri(&root, "&hidden=true"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(names(&json_of(&body)), vec![".oculto", "visivel"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_symlinks_are_ignored() {
        let root = temp_dir("symlink");
        let target = temp_dir("symlink-target");
        fs::create_dir_all(target.join("dentro")).expect("t");
        std::os::unix::fs::symlink(&target, root.join("link")).expect("symlink");
        fs::create_dir_all(root.join("real")).expect("real");
        let state = state_in(&root, Config::default());
        let (status, body) = call(&state, get_req(&browse_uri(&root, "&hidden=true"))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(names(&json_of(&body)), vec!["real"]);
    }

    #[tokio::test]
    async fn listing_is_truncated_at_500_entries() {
        let root = temp_dir("trunc");
        for i in 0..=MAX_ENTRIES {
            fs::create_dir_all(root.join(format!("d{i:04}"))).expect("d");
        }
        let state = state_in(&root, Config::default());
        let (status, body) = call(&state, get_req(&browse_uri(&root, ""))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        assert_eq!(v["entries"].as_array().expect("arr").len(), MAX_ENTRIES);
        assert_eq!(v["truncated"], true);
    }

    #[tokio::test]
    async fn system_dirs_are_forbidden() {
        let state = state_in(&temp_dir("sysdir"), Config::default());
        let (status, body) = call(&state, get_req("/api/fs/browse?path=/proc")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let (status, _) = call(&state, get_req("/api/fs/browse?path=/etc")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn bad_paths_get_static_errors() {
        let dir = temp_dir("badpaths");
        let file = dir.join("arquivo-xyz.txt");
        fs::write(&file, "x").expect("w");
        let state = state_in(&dir, Config::default());

        let (status, body) = call(&state, get_req(&browse_uri(&dir.join("nope-xyz"), ""))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(!body.contains("xyz"), "eco: {body}");

        let (status, body) = call(&state, get_req(&browse_uri(&file, ""))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!body.contains("xyz"), "eco: {body}");

        let (status, _) = call(&state, get_req("/api/fs/browse?path=relativo")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&state, get_req("/api/fs/browse?path=/tmp/../etc")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&state, get_req("/api/fs/browse?path=/tmp/%01x")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&state, get_req("/api/fs/browse?hidden=talvez")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn default_path_is_home_or_root_and_a_directory() {
        let state = state_in(&temp_dir("default"), Config::default());
        let (status, body) = call(&state, get_req("/api/fs/browse")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = json_of(&body);
        assert!(Path::new(v["path"].as_str().expect("path")).is_dir());
    }

    #[tokio::test]
    async fn root_has_no_parent_and_git_entries_are_flagged() {
        let root = temp_dir("gitflag");
        fs::create_dir_all(root.join("repo/.git")).expect("git");
        let state = state_in(&root, Config::default());
        let (_, body) = call(&state, get_req(&browse_uri(Path::new("/"), ""))).await;
        let v = json_of(&body);
        assert!(v["parent"].is_null());
        let (_, body) = call(&state, get_req(&browse_uri(&root, ""))).await;
        let v = json_of(&body);
        assert_eq!(v["entries"][0]["name"], "repo");
        assert_eq!(v["entries"][0]["is_git_repo"], true);
    }

    #[tokio::test]
    async fn secret_in_directory_name_is_scrubbed() {
        let root = temp_dir("secretname");
        fs::create_dir_all(root.join(format!("x{SECRET}y"))).expect("d");
        let mut cfg = Config::default();
        cfg.connections.push(crate::config::Connection {
            id: "literal".into(),
            name: "L".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: SECRET.into(),
            wire_api: crate::config::WireApi::default(),
        });
        let state = state_in(&root, cfg);
        let (status, body) = call(&state, get_req(&browse_uri(&root, ""))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains(SECRET), "{body}");
        assert!(body.contains("x***y"), "{body}");
    }
}
