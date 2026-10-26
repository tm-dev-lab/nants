//! Testes de `chat_attachments` (spec 082/T05): upload, tipo conferido pela assinatura, limite de
//! corpo próprio, cota com limpeza, GET seguro e symlinks.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};
use std::{env, process};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{
    attachment_exists, attachment_mime, is_upload_path, load_attachment, make_room, store, StoreError, ATTACHMENTS_EXCLUDE_LINE,
    ATTACHMENTS_REL, MAX_ATTACHMENT_BYTES,
};
use crate::agents::architect::{ChatTurn, TurnRole};
use crate::config::Config;
use crate::server::chat::append_turns;
use crate::server::{router, AppState, MAX_BODY_BYTES};

const HOST: &str = "127.0.0.1:3000";
pub(crate) const PNG: &[u8] = b"\x89PNG\r\n\x1a\nresto-da-imagem";
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0jpeg";
const WEBP: &[u8] = b"RIFF\x10\0\0\0WEBPVP8 ";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-attach-{}-{}", process::id(), tag));
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

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let resp = router(state.clone()).oneshot(req).await.expect("resposta");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024).await.expect("corpo");
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

fn upload_req(id: &str, content_type: &str, body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(format!("/api/projects/{id}/chat/attachments"))
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .expect("requisição")
}

/// Repositório git com o projeto cadastrado. Devolve (estado, id, raiz).
async fn project(tag: &str) -> (AppState, String, PathBuf) {
    let dir = temp_dir(tag);
    let root = dir.join("repo");
    fs::create_dir_all(&root).expect("repo");
    git(&root, &["init", "-q"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let state = AppState::new(dir.join("config.toml"), crate::server::readiness::chat_ready(Config::default()), 3000);
    let body = json!({ "name": "Anexos", "path": root.to_str().expect("utf-8") }).to_string();
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
    (state, id, root)
}

/// Sobe uma imagem; devolve (status, JSON).
pub(crate) async fn upload(state: &AppState, id: &str, content_type: &str, body: Vec<u8>) -> (StatusCode, Value) {
    let (status, _, bytes) = call(state, upload_req(id, content_type, body)).await;
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn dir(root: &Path) -> PathBuf {
    root.join(ATTACHMENTS_REL)
}

fn uri(id: &str, name: &str) -> String {
    format!("/api/projects/{id}/chat/attachments/{name}")
}

#[test]
fn accepts_only_generated_names() {
    let id = "0123456789abcdef0123456789abcdef";
    assert_eq!(attachment_mime(&format!("{id}.png")), Some("image/png"));
    assert_eq!(attachment_mime(&format!("{id}.jpg")), Some("image/jpeg"));
    assert_eq!(attachment_mime(&format!("{id}.webp")), Some("image/webp"));
    for bad in [
        String::new(),
        format!("{id}.gif"),
        format!("{id}.PNG"),
        format!("{id}.png.png"),
        format!("{}.png", id.to_uppercase()),
        format!("{}.png", &id[1..]),
        format!("{id}0.png"),
        format!("../{id}.png"),
        format!("{id}"),
        "x".repeat(36),
    ] {
        assert_eq!(attachment_mime(&bad), None, "{bad}");
    }
    assert!(is_upload_path("/api/projects/p1/chat/attachments"));
    assert!(!is_upload_path("/api/projects/p1/chat"));
    assert!(!is_upload_path("/api/projects/a/b/chat/attachments"));
    assert!(!is_upload_path("/api/projects//chat/attachments"));
}

#[tokio::test]
async fn valid_upload_is_stored_out_of_git_and_served() {
    let (state, id, root) = project("valid").await;
    let (status, body) = upload(&state, &id, "image/png", PNG.to_vec()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let name = body["name"].as_str().expect("name").to_string();
    let aid = body["id"].as_str().expect("id");
    assert_eq!(name, format!("{aid}.png"));
    assert_eq!(aid.len(), 32);
    assert!(attachment_mime(&name).is_some());
    assert_eq!(fs::read(dir(&root).join(&name)).expect("arquivo"), PNG);
    assert!(attachment_exists(&root, &name));
    assert_eq!(load_attachment(&root, &name).map(|i| i.media_type), Some("image/png".to_string()));

    // Fora do Git: linha no exclude local e nada em `git status`.
    let exclude = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
    assert_eq!(exclude.lines().filter(|l| *l == ATTACHMENTS_EXCLUDE_LINE).count(), 1, "{exclude}");
    let out = Command::new("git").args(["status", "--porcelain", "--untracked-files=all"]).current_dir(&root).output().expect("git");
    let status_out = String::from_utf8_lossy(&out.stdout);
    assert!(!status_out.contains("chat-attachments"), "{status_out}");

    let (status, headers, bytes) = call(&state, get(&uri(&id, &name))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, PNG);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    let cache = headers[header::CACHE_CONTROL].to_str().expect("cache");
    assert!(cache.starts_with("private"), "{cache}");

    // JPEG e WebP também; cada upload ganha um id novo.
    let (status, jpg) = upload(&state, &id, "image/jpeg", JPEG.to_vec()).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(jpg["name"].as_str().expect("n").ends_with(".jpg"));
    let (status, webp) = upload(&state, &id, "image/webp; charset=binary", WEBP.to_vec()).await;
    assert_eq!(status, StatusCode::CREATED);
    let webp_name = webp["name"].as_str().expect("n");
    assert!(webp_name.ends_with(".webp"));
    let (_, headers, _) = call(&state, get(&uri(&id, webp_name))).await;
    assert_eq!(headers[header::CONTENT_TYPE], "image/webp");
}

#[tokio::test]
async fn hidden_attachments_dir_is_not_a_spec() {
    let (state, id, root) = project("hidden").await;
    let (status, _) = upload(&state, &id, "image/png", PNG.to_vec()).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(dir(&root).is_dir());
    let (status, _, body) = call(&state, get(&format!("/api/projects/{id}/state"))).await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).expect("json");
    let text = v.to_string();
    assert!(!text.contains("chat-attachments"), "{text}");
    let specs = v["specs"].as_array().expect("specs");
    assert!(specs.is_empty(), "{text}");
}

#[tokio::test]
async fn fake_or_unsupported_type_is_415() {
    let (state, id, root) = project("fake").await;
    for (ct, body) in [
        ("image/png", JPEG.to_vec()),
        ("image/jpeg", PNG.to_vec()),
        ("image/webp", PNG.to_vec()),
        ("image/png", b"<svg onload=alert(1)>".to_vec()),
        ("image/png", Vec::new()),
        ("image/gif", b"GIF89a".to_vec()),
        ("image/svg+xml", b"<svg/>".to_vec()),
        ("text/plain", PNG.to_vec()),
    ] {
        let (status, body) = upload(&state, &id, ct, body).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct}: {body}");
    }
    assert!(!dir(&root).exists() || fs::read_dir(dir(&root)).expect("ls").count() == 0);
}

#[tokio::test]
async fn over_the_limit_is_413_and_route_limit_is_own() {
    let (state, id, root) = project("limit").await;
    let mut big = PNG.to_vec();
    big.resize(MAX_ATTACHMENT_BYTES + 1, 0);
    let (status, _) = upload(&state, &id, "image/png", big).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    // Bem acima do limite global (256 KiB) e dentro do da rota: aceito.
    let mut ok = PNG.to_vec();
    ok.resize(MAX_ATTACHMENT_BYTES, 0);
    assert!(ok.len() > MAX_BODY_BYTES);
    let (status, _) = upload(&state, &id, "image/png", ok).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(fs::read_dir(dir(&root)).expect("ls").count(), 1);
    // O limite global segue valendo nas outras rotas (ex.: o chat).
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/projects/{id}/chat"))
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"message": "a".repeat(MAX_BODY_BYTES + 1)}).to_string()))
        .expect("requisição");
    let (status, _, _) = call(&state, req).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn invalid_or_missing_names_are_404() {
    let (state, id, root) = project("names").await;
    fs::create_dir_all(dir(&root)).expect("dir");
    fs::write(root.join(".sdd/specs/segredo.png"), PNG).expect("fora");
    fs::write(dir(&root).join("nota.txt"), b"texto").expect("txt");
    for name in [
        "..%2Fsegredo.png",
        "%2E%2E%2Fsegredo.png",
        "nota.txt",
        "segredo.png",
        "0123456789ABCDEF0123456789ABCDEF.png",
        "0123456789abcdef0123456789abcdef.gif",
        "0123456789abcdef0123456789abcdef.png",
    ] {
        let (status, _, body) = call(&state, get(&uri(&id, name))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{name}");
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("/tmp"), "{text}");
    }
    let (status, _, _) = call(&state, get(&uri("nao-existe", "0123456789abcdef0123456789abcdef.png"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn symlinks_are_never_followed() {
    let (state, id, root) = project("symlink").await;
    let outside = root.parent().expect("pai").join("fora");
    fs::create_dir_all(&outside).expect("fora");

    // Arquivo symlink com nome válido: GET 404 e não "existe".
    fs::create_dir_all(dir(&root)).expect("dir");
    let name = "0123456789abcdef0123456789abcdef.png";
    fs::write(outside.join("alvo.png"), PNG).expect("alvo");
    std::os::unix::fs::symlink(outside.join("alvo.png"), dir(&root).join(name)).expect("link");
    let (status, _, _) = call(&state, get(&uri(&id, name))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!attachment_exists(&root, name));
    assert!(load_attachment(&root, name).is_none());

    // Pasta de anexos symlink para fora: upload recusado e nada gravado fora.
    fs::remove_dir_all(dir(&root)).expect("rm");
    std::os::unix::fs::symlink(&outside, dir(&root)).expect("link pasta");
    let (status, body) = upload(&state, &id, "image/png", PNG.to_vec()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(fs::read_dir(&outside).expect("ls").count(), 1, "nada criado fora");
    assert!(!body.to_string().contains("/tmp"));
}

fn set_age(path: &Path, secs_ago: u64) {
    let file = fs::File::options().write(true).open(path).expect("abrir");
    file.set_modified(SystemTime::now() - Duration::from_secs(secs_ago)).expect("mtime");
}

fn user_turn(attachments: &[&str]) -> ChatTurn {
    ChatTurn {
        role: TurnRole::User,
        text: "veja".into(),
        ts: 1,
        turn_id: None,
        internal: false,
        attachments: attachments.iter().map(|s| s.to_string()).collect(),
        notice: None,
    }
}

#[test]
fn quota_deletes_oldest_unreferenced_then_refuses() {
    let root = temp_dir("quota").join("repo");
    fs::create_dir_all(&root).expect("repo");
    fs::create_dir_all(dir(&root)).expect("dir");
    let names: Vec<String> = (0..4).map(|i| format!("{i:032x}.png")).collect();
    for (i, name) in names.iter().enumerate() {
        let path = dir(&root).join(name);
        fs::write(&path, vec![0u8; 100]).expect("arquivo");
        set_age(&path, 1000 - i as u64 * 100); // names[0] é o mais antigo
    }
    // O mais antigo (names[0]) é citado num turno recente: fica.
    assert!(append_turns(&root, &[user_turn(&[&names[0]])]));
    let keep: HashSet<String> = super::super::chat::recent_attachments(&root, super::KEEP_TURNS);
    assert!(keep.contains(&names[0]));

    // Cota 400, entram 150: apaga names[1] e names[2] (os mais antigos não citados).
    make_room(&root, &dir(&root), 150, 400, &keep).expect("cabe");
    assert!(dir(&root).join(&names[0]).exists());
    assert!(!dir(&root).join(&names[1]).exists());
    assert!(!dir(&root).join(&names[2]).exists());
    assert!(dir(&root).join(&names[3]).exists());

    // Pela gravação: cabe depois de apagar names[3]; o novo arquivo fica.
    let stored = store(&root, PNG, "png", 100 + PNG.len() as u64).expect("gravou");
    assert!(dir(&root).join(&stored).exists());
    assert!(!dir(&root).join(&names[3]).exists());

    // Tudo citado: não cabe → Full, nada apagado.
    assert!(append_turns(&root, &[user_turn(&[&stored])]));
    assert_eq!(store(&root, PNG, "png", 100 + PNG.len() as u64), Err(StoreError::Full));
    assert!(dir(&root).join(&names[0]).exists());
    assert!(dir(&root).join(&stored).exists());
}

#[test]
fn quota_keeps_only_the_last_turns() {
    let root = temp_dir("quota-turns").join("repo");
    fs::create_dir_all(dir(&root)).expect("dir");
    let old = format!("{:032x}.png", 7);
    fs::write(dir(&root).join(&old), vec![0u8; 100]).expect("arquivo");
    assert!(append_turns(&root, &[user_turn(&[&old])]));
    let filler: Vec<ChatTurn> = (0..super::KEEP_TURNS).map(|_| user_turn(&[])).collect();
    assert!(append_turns(&root, &filler));
    let keep = super::super::chat::recent_attachments(&root, super::KEEP_TURNS);
    assert!(!keep.contains(&old));
    make_room(&root, &dir(&root), 50, 100, &keep).expect("cabe");
    assert!(!dir(&root).join(&old).exists());
}
