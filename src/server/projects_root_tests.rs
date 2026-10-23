//! Testes de rota da pasta de projetos gerenciada (`projects_root.rs`): `POST /api/projects
//! {name}`, o contrato `{name, path}` e `PUT /api/config/projects-root`. Diretórios temporários
//! próprios; nunca a raiz padrão do usuário.

use std::fs;
use std::path::{Path, PathBuf};
use std::{env, process};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::{Config, Project};
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-projroot-{}-{tag}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn state_with_root(dir: &Path, root: &str) -> AppState {
    let mut cfg = crate::server::readiness::chat_ready(Config::default());
    cfg.projects_root = Some(root.to_string());
    AppState::new(dir.join("config.toml"), cfg, 3000)
}

fn req(method: Method, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("requisição")
}

async fn call(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
    let resp = router(state.clone()).oneshot(request).await.expect("resposta");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("corpo");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn create(state: &AppState, body: Value) -> (StatusCode, Value) {
    call(state, req(Method::POST, "/api/projects", &body.to_string())).await
}

fn canonical(p: &Path) -> String {
    fs::canonicalize(p).expect("canônico").display().to_string()
}

#[tokio::test]
async fn by_name_creates_root_and_slug_folder_with_git_keeping_the_display_name() {
    let dir = temp_dir("byname");
    let root = dir.join("NantsProjects");
    assert!(!root.exists());
    let state = state_with_root(&dir, root.to_str().expect("utf8"));
    let (status, body) = create(&state, json!({"name": "Órbita Kids!"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let folder = root.join("orbita-kids");
    assert!(folder.is_dir(), "raiz criada e pasta pelo slug");
    assert_eq!(body["name"], "Órbita Kids!", "nome de exibição é o digitado");
    assert_eq!(body["path"], canonical(&folder));
    assert_eq!(body["is_git_repo"], true);
    assert_eq!(body["git_initialized"], true);
    assert_eq!(body["initialized"], true);
    assert!(folder.join(".git").is_dir() && folder.join(".sdd/settings.yaml").is_file());
    let cfg = state.config().read().await.clone();
    assert_eq!(cfg.projects.len(), 1);
    assert_eq!(cfg.projects[0].name, "Órbita Kids!");
    // O id segue a regra de sempre (`project_id`); o slug da pasta é outro, com acentos dobrados.
    assert!(cfg.projects[0].id.contains("kids-"), "{}", cfg.projects[0].id);
    // Persistido no config.toml.
    let saved = Config::load_from(&dir.join("config.toml")).expect("config");
    assert_eq!(saved.projects.len(), 1);

    // Remover do Nants não apaga a pasta.
    let id = cfg.projects[0].id.clone();
    let (status, body) = call(&state, req(Method::DELETE, &format!("/api/projects/{id}"), "")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(folder.join(".git").is_dir(), "pasta intacta");
}

#[tokio::test]
async fn existing_folder_is_409_project_dir_exists_with_the_path() {
    let dir = temp_dir("exists");
    let root = dir.join("raiz");
    let state = state_with_root(&dir, root.to_str().expect("utf8"));
    fs::create_dir_all(root.join("meu-app")).expect("mkdir");
    fs::write(root.join("meu-app/arquivo.txt"), "dados").expect("arquivo");
    let (status, body) = create(&state, json!({"name": "Meu App"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body,
        json!({"error": "a pasta do projeto já existe", "code": "project_dir_exists", "path": canonical(&root.join("meu-app"))})
    );
    assert!(state.config().read().await.projects.is_empty());
    assert!(!root.join("meu-app/.git").exists(), "nada inicializado");
    // Segunda criação do mesmo nome depois de uma criação bem-sucedida também é 409.
    let (status, _) = create(&state, json!({"name": "Outro"})).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = create(&state, json!({"name": "outro"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "project_dir_exists");
}

#[tokio::test]
async fn empty_or_symbol_only_name_is_400_without_effects() {
    let dir = temp_dir("badname");
    let root = dir.join("raiz");
    let state = state_with_root(&dir, root.to_str().expect("utf8"));
    for name in ["", "   ", "!!!", "../..", "/", "日本"] {
        let (status, body) = create(&state, json!({"name": name})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name:?}: {body}");
    }
    assert!(!root.exists(), "nada criado");
    assert!(state.config().read().await.projects.is_empty());
}

#[tokio::test]
async fn invalid_root_is_422_and_nothing_is_created() {
    let dir = temp_dir("badroot");
    // Relativo, de sistema e dentro de projeto registrado.
    for root in ["relativo/x", "/etc/nants-projetos", "/proc/nants"] {
        let state = state_with_root(&dir, root);
        let (status, body) = create(&state, json!({"name": "App"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{root}: {body}");
        assert!(body["error"].as_str().is_some_and(|e| e.starts_with("pasta de projetos inválida")), "{body}");
    }
    let registered = dir.join("projeto");
    fs::create_dir_all(&registered).expect("mkdir");
    let mut state = state_with_root(&dir, registered.join("sub").to_str().expect("utf8"));
    state.config().write().await.projects.push(Project {
        id: "p1".into(),
        name: "P".into(),
        path: registered.display().to_string(),
        created_at: "2026-10-19T00:00:00Z".into(),
    });
    let (status, _) = create(&state, json!({"name": "App"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!registered.join("sub").exists());
    // Raiz que é symlink para diretório de sistema: a forma canônica também é validada.
    let link = dir.join("link-etc");
    std::os::unix::fs::symlink("/etc", &link).expect("symlink");
    state = state_with_root(&dir, link.to_str().expect("utf8"));
    let (status, _) = create(&state, json!({"name": "App"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!Path::new("/etc/app").exists());
}

#[tokio::test]
async fn symlink_in_place_of_the_folder_is_never_followed() {
    let dir = temp_dir("symlink");
    let root = dir.join("raiz");
    let outside = dir.join("fora");
    fs::create_dir_all(&root).expect("raiz");
    fs::create_dir_all(&outside).expect("fora");
    std::os::unix::fs::symlink(&outside, root.join("meu-app")).expect("symlink");
    // Link quebrado também conta como "já existe".
    std::os::unix::fs::symlink(dir.join("nao-existe"), root.join("quebrado")).expect("symlink");
    let state = state_with_root(&dir, root.to_str().expect("utf8"));
    let (status, body) = create(&state, json!({"name": "Meu App"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "project_dir_exists");
    let (status, _) = create(&state, json!({"name": "Quebrado"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(fs::read_dir(&outside).expect("ler").next().is_none(), "destino do link intocado");
    assert!(!dir.join("nao-existe").exists());
}

#[tokio::test]
async fn name_never_escapes_the_root() {
    let dir = temp_dir("escape");
    let root = dir.join("raiz");
    let state = state_with_root(&dir, root.to_str().expect("utf8"));
    let (status, body) = create(&state, json!({"name": "../../fuga"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["path"], canonical(&root.join("fuga")));
    assert!(!dir.join("fuga").exists() && !dir.parent().expect("pai").join("fuga").exists());
    assert_eq!(body["name"], "../../fuga", "nome de exibição preservado");
}

#[tokio::test]
async fn name_and_path_contract_is_unchanged() {
    let dir = temp_dir("contract");
    let existing = dir.join("pasta existente");
    fs::create_dir_all(&existing).expect("mkdir");
    let root = dir.join("raiz-nao-usada");
    let state = state_with_root(&dir, root.to_str().expect("utf8"));
    let (status, body) = create(&state, json!({"name": "Antigo", "path": existing.to_str().expect("utf8")})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["path"], canonical(&existing));
    assert_eq!(body["name"], "Antigo");
    assert!(!root.exists(), "{{name, path}} não usa a raiz gerenciada");
    // Mesmo caminho de novo: 200 com o existente; caminho inexistente continua 400.
    let (status, _) = create(&state, json!({"name": "Antigo", "path": existing.to_str().expect("utf8")})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = create(&state, json!({"name": "X", "path": dir.join("nao-existe").to_str().expect("utf8")})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // `name` vazio com `path` mantém o 422 original.
    let (status, _) = create(&state, json!({"name": " ", "path": existing.to_str().expect("utf8")})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn projects_root_is_editable_through_the_config_api() {
    let dir = temp_dir("cfgapi");
    let state = AppState::new(dir.join("config.toml"), Config::default(), 3000);
    let (_, body) = call(&state, req(Method::GET, "/api/config", "")).await;
    assert_eq!(body["projects_root"], Value::Null, "padrão");
    assert!(body["projects_root_effective"].as_str().is_some_and(|p| p.ends_with("/NantsProjects")), "{body}");

    let wanted = dir.join("meus projetos");
    let (status, body) = call(
        &state,
        req(Method::PUT, "/api/config/projects-root", &json!({"projects_root": wanted.to_str()}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["projects_root"], wanted.to_str().expect("utf8"));
    assert_eq!(state.config().read().await.projects_root.as_deref(), wanted.to_str());
    assert_eq!(Config::load_from(&dir.join("config.toml")).expect("config").projects_root.as_deref(), wanted.to_str());
    let (_, body) = call(&state, req(Method::GET, "/api/config", "")).await;
    assert_eq!(body["projects_root_effective"], wanted.to_str().expect("utf8"));

    for bad in ["relativo", "/etc/x", "/", "~/../x", ""] {
        let (status, _) =
            call(&state, req(Method::PUT, "/api/config/projects-root", &json!({"projects_root": bad}).to_string())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}");
    }
    assert_eq!(state.config().read().await.projects_root.as_deref(), wanted.to_str(), "recusa não grava");
    let (status, _) =
        call(&state, req(Method::PUT, "/api/config/projects-root", r#"{"projects_root":"/tmp/x","extra":1}"#)).await;
    assert!(status.is_client_error());
    // `null` volta ao padrão.
    let (status, body) = call(&state, req(Method::PUT, "/api/config/projects-root", r#"{"projects_root":null}"#)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["projects_root"], Value::Null);
    assert!(state.config().read().await.projects_root.is_none());
}
