//! `GET /api/projects/:id/specs/:spec/captures/:name` (spec 082/T04): serve uma captura de tela
//! copiada pelo Revisor para `.sdd/specs/<spec>/captures/` (ver `sdd::screenshots::copy_viewed`).
//!
//! Só aceita nomes `[a-z0-9._-]` (até [`MAX_COPY_NAME_CHARS`]) com extensão png, jpg ou jpeg, e
//! spec com id válido. Nome ou spec inválidos, `..`, barras, symlink em qualquer componente,
//! arquivo especial ou ausente: 404 estático, sem eco de caminho. A leitura usa
//! `safe_fs::read_bounded` (`symlink_metadata` em cada segmento e `O_NOFOLLOW`).

use std::path::PathBuf;

use axum::extract::{Path as AxPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use super::error::ApiError;
use super::events::validate_project_id;
use super::projects::find_project;
use super::safe_fs;
use super::spec_result::validate_spec_id;
use super::state::AppState;
use crate::sdd::screenshots::{media_type_of_name, CAPTURES_DIR, MAX_COPY_NAME_CHARS, MAX_FILE_BYTES};

const NOT_FOUND: &str = "captura não encontrada";
const INTERNAL_MSG: &str = "falha interna";
/// Cache privado: o nome da cópia inclui tarefa e tentativa, então o conteúdo raramente muda.
const CACHE_CONTROL: &str = "private, max-age=3600";

fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, NOT_FOUND)
}

/// Nome de captura aceito: `[a-z0-9._-]`, até [`MAX_COPY_NAME_CHARS`], sem começar por `.` ou `-`,
/// sem `..` e com extensão png, jpg ou jpeg. Devolve o tipo MIME.
pub(super) fn capture_mime(name: &str) -> Option<&'static str> {
    let ok = !name.is_empty()
        && name.len() <= MAX_COPY_NAME_CHARS
        && !name.starts_with(['.', '-'])
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        media_type_of_name(name)
    } else {
        None
    }
}

pub async fn get_capture(
    State(state): State<AppState>,
    AxPath((id, spec, name)): AxPath<(String, String, String)>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    validate_spec_id(&spec).map_err(|_| not_found())?;
    let mime = capture_mime(&name).ok_or_else(not_found)?;
    let project = find_project(&state, &id).await?;
    let root = PathBuf::from(&project.path);
    let rel = format!(".sdd/specs/{spec}/{CAPTURES_DIR}/{name}");
    let read = tokio::task::spawn_blocking(move || safe_fs::read_bounded(&root, &rel, MAX_FILE_BYTES))
        .await
        .map_err(|_| ApiError::internal(INTERNAL_MSG))?;
    let data = match read {
        Ok(safe_fs::BoundedRead { data: Some(data), .. }) => data,
        _ => return Err(not_found()),
    };
    let mut resp = data.into_response();
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::{env, process};

    use axum::body::Body;
    use axum::http::{header, Method, Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::capture_mime;
    use crate::config::Config;
    use crate::server::{router, AppState};

    const HOST: &str = "127.0.0.1:3000";
    const SPEC: &str = "001-demo";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nresto";

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("nants-captures-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let resp = router(state.clone()).oneshot(req).await.expect("resposta");
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024).await.expect("corpo");
        (status, headers, bytes.to_vec())
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(header::HOST, HOST)
            .body(Body::empty())
            .expect("requisição")
    }

    /// Repositório com o projeto cadastrado e a pasta de capturas da spec. Devolve (estado, id, raiz).
    async fn project(tag: &str) -> (AppState, String, PathBuf) {
        let dir = temp_dir(tag);
        let root = dir.join("repo");
        fs::create_dir_all(&root).expect("repo");
        git(&root, &["init", "-q"]);
        git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let state = AppState::new(dir.join("config.toml"), crate::server::readiness::chat_ready(Config::default()), 3000);
        let body = json!({ "name": "Capturas", "path": root.to_str().expect("utf-8") }).to_string();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/projects")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("requisição");
        let (status, _, body) = call(&state, req).await;
        assert_eq!(status, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
        let v: Value = serde_json::from_slice(&body).expect("json");
        let id = v["id"].as_str().expect("id").to_string();
        fs::create_dir_all(captures(&root)).expect("captures");
        (state, id, root)
    }

    fn captures(root: &Path) -> PathBuf {
        root.join(".sdd/specs").join(SPEC).join("captures")
    }

    fn uri(id: &str, spec: &str, name: &str) -> String {
        format!("/api/projects/{id}/specs/{spec}/captures/{name}")
    }

    #[test]
    fn accepts_only_safe_image_names() {
        assert_eq!(capture_mime("t01-1-shots-home.png"), Some("image/png"));
        assert_eq!(capture_mime("t01-1-a.jpg"), Some("image/jpeg"));
        assert_eq!(capture_mime("t01-1-a.jpeg"), Some("image/jpeg"));
        for bad in [
            "", "..png", "a..png", ".a.png", "-a.png", "a/b.png", "a\\b.png", "A.png", "a.PNG",
            "a.gif", "a.svg", "a b.png", "a.png%00", "a",
        ] {
            assert_eq!(capture_mime(bad), None, "{bad}");
        }
        let long = format!("{}.png", "a".repeat(117));
        assert_eq!(long.len(), 121);
        assert_eq!(capture_mime(&long), None);
        assert!(capture_mime(&format!("{}.png", "a".repeat(116))).is_some());
        // Spec 084 T01: os nomes novos (só o arquivo, com sufixo de colisão e corte no fim) passam.
        let batch: Vec<String> = ["d/aparência.png", "m/aparência.png", &format!("x/{}.jpg", "z".repeat(300))]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for name in crate::sdd::screenshots::copy_names("FINAL", 2, &batch).into_iter().flatten() {
            assert!(capture_mime(&name).is_some(), "{name}");
        }
    }

    #[tokio::test]
    async fn serves_a_capture_with_safe_headers() {
        let (state, id, root) = project("serve").await;
        fs::write(captures(&root).join("t01-1-shots-home.png"), PNG).expect("png");
        fs::write(captures(&root).join("t01-1-menu.jpg"), b"\xFF\xD8\xFFjpeg").expect("jpg");
        let (status, headers, body) = call(&state, get(&uri(&id, SPEC, "t01-1-shots-home.png"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, PNG);
        assert_eq!(headers[header::CONTENT_TYPE], "image/png");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let cache = headers[header::CACHE_CONTROL].to_str().expect("cache");
        assert!(cache.starts_with("private, max-age="), "{cache}");
        let (status, headers, _) = call(&state, get(&uri(&id, SPEC, "t01-1-menu.jpg"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    }

    #[tokio::test]
    async fn invalid_names_and_specs_are_404() {
        let (state, id, root) = project("invalid").await;
        fs::write(captures(&root).join("ok.png"), PNG).expect("png");
        fs::write(root.join(".sdd/specs").join(SPEC).join("spec.png"), PNG).expect("png");
        fs::write(captures(&root).join("nota.txt"), b"texto").expect("txt");
        for name in ["..%2Fspec.png", "%2E%2E%2Fspec.png", "a%2Fok.png", "nota.txt", "OK.png", "..png", "a%5Cok.png"] {
            let (status, _, _) = call(&state, get(&uri(&id, SPEC, name))).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{name}");
        }
        for spec in ["..", "%2E%2E", "a%2Fb", "x.y"] {
            let (status, _, _) = call(&state, get(&uri(&id, spec, "ok.png"))).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{spec}");
        }
        let (status, _, _) = call(&state, get(&uri("nao-existe", SPEC, "ok.png"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn missing_capture_is_404() {
        let (state, id, _root) = project("missing").await;
        let (status, _, body) = call(&state, get(&uri(&id, SPEC, "t01-1-nada.png"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("/tmp") && !text.contains("nants-captures"), "{text}");
        let (status, _, _) = call(&state, get(&uri(&id, "002-outra", "t01-1-nada.png"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn symlinks_are_404() {
        let (state, id, root) = project("symlink").await;
        let outside = root.parent().expect("pai").join("fora.png");
        fs::write(&outside, PNG).expect("fora");
        std::os::unix::fs::symlink(&outside, captures(&root).join("link.png")).expect("link arquivo");
        let (status, _, _) = call(&state, get(&uri(&id, SPEC, "link.png"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Pasta `captures` de outra spec é um symlink para fora do projeto.
        let other = root.join(".sdd/specs/002-outra");
        fs::create_dir_all(&other).expect("spec");
        let ext_dir = root.parent().expect("pai").join("ext");
        fs::create_dir_all(&ext_dir).expect("ext");
        fs::write(ext_dir.join("x.png"), PNG).expect("x");
        std::os::unix::fs::symlink(&ext_dir, other.join("captures")).expect("link pasta");
        let (status, _, _) = call(&state, get(&uri(&id, "002-outra", "x.png"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
