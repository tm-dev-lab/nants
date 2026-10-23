//! Testes de `DELETE /api/projects/:id` (remoção só do Nants). Incluídos por `projects.rs`.

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use std::{env, process};

use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::harness::tools::{ApprovalDecision, ApprovalRequest, Approver, Role};
use crate::server::approvals::ProjectApprover;
use crate::server::events::SecretSet;
use crate::server::{router, AppState};

pub(super) const PORT: u16 = 3000;
pub(super) const HOST: &str = "127.0.0.1:3000";
pub(super) const RUN_MSG: &str = "já existe uma execução em andamento neste projeto";
pub(super) const CHAT_MSG: &str = "já existe um turno de chat em andamento para este projeto";
pub(super) const SAVE_MSG: &str = "falha ao salvar a configuração";
pub(super) const NOT_FOUND_MSG: &str = "projeto não encontrado";

pub(super) fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-remove-{}-{}", process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

pub(super) fn request(method: Method, uri: &str, host: &str, body: &str) -> Request<Body> {
    // DELETE exige Content-Type JSON (guarda global), mesmo sem corpo; o corpo é ignorado.
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("requisição")
}

pub(super) fn delete(id: &str) -> Request<Body> {
    request(Method::DELETE, &format!("/api/projects/{id}"), HOST, "")
}

pub(super) fn get(uri: &str) -> Request<Body> {
    request(Method::GET, uri, HOST, "")
}

pub(super) async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, String) {
    let resp: Response<Body> = router(state.clone()).oneshot(req).await.expect("resposta");
    let status = resp.status();
    let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("corpo");
    (status, String::from_utf8_lossy(&bytes).to_string())
}

pub(super) fn json_of(text: &str) -> Value {
    serde_json::from_str(text).expect("json")
}

pub(super) fn state_at(config_path: PathBuf) -> AppState {
    AppState::new(config_path, crate::server::readiness::execute_ready(Config::default()), PORT)
}

/// Cadastra `path` pelo fluxo de produção e devolve o id.
pub(super) async fn create(state: &AppState, name: &str, path: &Path) -> String {
    fs::create_dir_all(path).expect("mkdir do projeto");
    let body = json!({ "name": name, "path": path.to_str().expect("utf-8") }).to_string();
    let req = request(Method::POST, "/api/projects", HOST, &body);
    let (status, text) = call(state, req).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    json_of(&text)["id"].as_str().expect("id").to_string()
}

pub(super) fn ids_in_file(config_path: &Path) -> Vec<String> {
    Config::load_from(config_path)
        .expect("config gravada")
        .projects
        .into_iter()
        .map(|p| p.id)
        .collect()
}

pub(super) async fn ids_in_memory(state: &AppState) -> Vec<String> {
    state.config().read().await.projects.iter().map(|p| p.id.clone()).collect()
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Entry {
    Dir,
    File(Vec<u8>),
    Link(PathBuf),
}

/// Árvore completa (caminhos e conteúdos, inclusive `.git`, `.sdd` e worktrees).
pub(super) fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Entry>) {
        for entry in fs::read_dir(dir).expect("listar") {
            let path = entry.expect("entrada").path();
            let rel = path.strip_prefix(base).expect("relativo").to_path_buf();
            let meta = fs::symlink_metadata(&path).expect("metadados");
            if meta.file_type().is_symlink() {
                out.insert(rel, Entry::Link(fs::read_link(&path).expect("link")));
            } else if meta.is_dir() {
                out.insert(rel, Entry::Dir);
                walk(base, &path, out);
            } else {
                out.insert(rel, Entry::File(fs::read(&path).expect("ler")));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Saída somente leitura de `git` que descreve commits, refs e worktrees.
pub(super) fn git_state(root: &Path) -> String {
    let run = |args: &[&str]| {
        let out = Command::new("git").args(args).current_dir(root).output().expect("git");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    format!(
        "{}\n{}\n{}",
        run(&["log", "--all", "--format=%H %s"]),
        run(&["for-each-ref"]),
        run(&["worktree", "list", "--porcelain"]),
    )
}

/// Escritas do próprio teste, com a manutenção automática desligada: `git maintenance run
/// --auto` (destacado do git 2.47 em diante) cria `.git/objects/maintenance.lock` e pode
/// sobreviver ao comando, mudando o `snapshot` sem que o produto tenha escrito nada.
pub(super) fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .current_dir(root)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?}");
}

/// Repositório real: commit, branch `sdd/t1`, worktree dentro da pasta e uma spec no disco.
pub(super) fn real_repo(root: &Path) {
    fs::create_dir_all(root).expect("mkdir");
    git(root, &["init", "-q"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(root, &["worktree", "add", "-q", "-b", "sdd/t1", "wt"]);
    let spec = root.join(".sdd/specs/001-demo");
    fs::create_dir_all(&spec).expect("mkdir spec");
    fs::write(
        spec.join("spec.md"),
        "---\nid: \"001-demo\"\ntitle: \"Demo\"\nstatus: \"DONE\"\n---\n\n# Demo\n",
    )
    .expect("escrever spec");
}

#[tokio::test]
async fn removal_writes_config_without_project_and_keeps_others_in_order() {
    let dir = temp_dir("order");
    let config_path = dir.join("config.toml");
    let state = state_at(config_path.clone());
    let a = create(&state, "A", &dir.join("a")).await;
    let b = create(&state, "B", &dir.join("b")).await;
    let c = create(&state, "C", &dir.join("c")).await;
    assert_eq!(ids_in_file(&config_path), vec![a.clone(), b.clone(), c.clone()]);

    let (status, text) = call(&state, delete(&b)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json_of(&text), json!({"ok": true, "removed": b}));
    assert_eq!(ids_in_file(&config_path), vec![a.clone(), c.clone()]);
    assert_eq!(ids_in_memory(&state).await, vec![a.clone(), c.clone()]);

    let (status, _) = call(&state, get(&format!("/api/projects/{b}/state"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, text) = call(&state, get(&format!("/api/projects/{a}/state"))).await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

#[tokio::test]
async fn removal_leaves_real_repo_filesystem_byte_identical_and_reimport_recovers_spec() {
    let dir = temp_dir("fs");
    let repo = dir.join("repo");
    let state = state_at(dir.join("config.toml"));
    real_repo(&repo);
    let id = create(&state, "Repo", &repo).await;

    let tree_before = snapshot(&repo);
    let git_before = git_state(&repo);
    assert!(tree_before.contains_key(Path::new(".sdd/specs/001-demo/spec.md")));
    assert!(git_before.contains("sdd/t1"), "{git_before}");

    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");

    assert_eq!(snapshot(&repo), tree_before, "árvore do projeto mudou");
    assert_eq!(git_state(&repo), git_before, "git mudou");

    // Reimportar o mesmo caminho recria o projeto e recupera a spec do disco.
    let again = create(&state, "Repo", &repo).await;
    assert_eq!(again, id);
    let (status, text) = call(&state, get(&format!("/api/projects/{id}/state"))).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let specs = json_of(&text)["specs"].clone();
    assert!(
        specs.as_array().expect("specs").iter().any(|s| s["id"] == "001-demo"),
        "{text}"
    );
}

#[tokio::test]
async fn pending_permissions_are_denied_releasing_agent_and_events_follow() {
    let dir = temp_dir("pending");
    let proj = dir.join("p");
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "P", &proj).await;

    let approver = ProjectApprover::new(
        id.clone(),
        proj.clone(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::new(SecretSet::default()),
    );
    let waiting = tokio::spawn(async move {
        let req = ApprovalRequest {
            in_container: false,
            role: Role::Developer,
            task_id: Some("T01".into()),
            command: "make deploy-prod".into(),
        };
        approver.approve(&req).await
    });
    for _ in 0..500 {
        if !state.approvals().list_pending(&id).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let pending = state.approvals().list_pending(&id);
    assert_eq!(pending.len(), 1);
    let request_id = pending[0].request_id.clone();

    let mut events = state.events().subscribe();
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");

    let decision = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("agente liberado")
        .expect("tarefa");
    assert_eq!(decision, ApprovalDecision::Deny);
    assert!(state.approvals().list_pending(&id).is_empty());

    let mut seen = Vec::new();
    while let Ok(ev) = events.try_recv() {
        seen.push(ev);
    }
    let resolved = seen
        .iter()
        .position(|e| {
            e.event_type() == "permission_resolved"
                && e.payload["request_id"] == request_id.as_str()
                && e.payload["applied"] == "deny"
        })
        .expect("permission_resolved deny");
    let removed = seen
        .iter()
        .position(|e| e.event_type() == "project_removed")
        .expect("project_removed");
    assert!(resolved < removed, "ordem dos eventos");
    assert_eq!(seen[removed].payload["project_id"], id.as_str());
}

#[tokio::test]
async fn active_run_refuses_removal_with_409_and_no_effect() {
    let dir = temp_dir("run");
    let config_path = dir.join("config.toml");
    let state = state_at(config_path.clone());
    let id = create(&state, "P", &dir.join("p")).await;

    let run = state.runs().try_start(&id, "001-demo").expect("reserva");
    let mut events = state.events().subscribe();
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(json_of(&text)["error"], RUN_MSG);
    assert_eq!(ids_in_file(&config_path), vec![id.clone()]);
    assert_eq!(ids_in_memory(&state).await, vec![id.clone()]);
    assert!(events.try_recv().is_err(), "nenhum evento num 409");

    drop(run);
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

#[tokio::test]
async fn chat_turn_refuses_removal_with_409_and_releases_after() {
    let dir = temp_dir("chat");
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "P", &dir.join("p")).await;

    let turn = state.try_begin_chat(&id).expect("reserva");
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(json_of(&text)["error"], CHAT_MSG);
    assert_eq!(ids_in_memory(&state).await, vec![id.clone()]);

    drop(turn);
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

#[tokio::test]
async fn unknown_and_repeated_removal_return_404_without_effect() {
    let dir = temp_dir("404");
    let state = state_at(dir.join("config.toml"));
    let (status, text) = call(&state, delete("nao-existe")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert_eq!(json_of(&text)["error"], NOT_FOUND_MSG);

    let id = create(&state, "P", &dir.join("p")).await;
    let (status, _) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert_eq!(json_of(&text)["error"], NOT_FOUND_MSG);
}

#[tokio::test]
async fn invalid_id_is_422_and_foreign_host_is_403() {
    let dir = temp_dir("guards");
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "P", &dir.join("p")).await;

    let (status, text) = call(&state, delete("bad.id")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");

    let req = request(Method::DELETE, &format!("/api/projects/{id}"), "evil.example", "");
    let (status, text) = call(&state, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(ids_in_memory(&state).await, vec![id]);
}

#[tokio::test]
async fn persistence_failure_keeps_project_in_memory_and_retry_works() {
    let dir = temp_dir("save");
    let cfg_dir = dir.join("cfg");
    fs::create_dir_all(&cfg_dir).expect("mkdir");
    let config_path = cfg_dir.join("config.toml");
    let state = state_at(config_path.clone());
    let id = create(&state, "P", &dir.join("p")).await;

    // Bloqueia a gravação: o diretório da configuração vira um arquivo comum.
    fs::rename(&cfg_dir, dir.join("cfg-moved")).expect("mover");
    fs::write(&cfg_dir, "bloqueio").expect("bloquear");
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
    assert_eq!(json_of(&text)["error"], SAVE_MSG);
    assert_eq!(ids_in_memory(&state).await, vec![id.clone()]);
    let (status, text) = call(&state, get("/api/config")).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json_of(&text)["projects"][0]["id"], id.as_str());
    let (status, text) = call(&state, get(&format!("/api/projects/{id}/state"))).await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // Desfaz o bloqueio: a remoção agora conclui.
    fs::remove_file(&cfg_dir).expect("desbloquear");
    fs::rename(dir.join("cfg-moved"), &cfg_dir).expect("restaurar");
    let (status, text) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(ids_in_file(&config_path).is_empty());
}

#[tokio::test]
async fn after_removal_state_and_chat_are_404() {
    let dir = temp_dir("gone");
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "P", &dir.join("p")).await;
    let (status, _) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, text) = call(&state, get(&format!("/api/projects/{id}/state"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    let chat = request(
        Method::POST,
        &format!("/api/projects/{id}/chat"),
        HOST,
        r#"{"message":"oi"}"#,
    );
    let (status, text) = call(&state, chat).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
}

#[tokio::test]
async fn live_activity_endpoint_masks_secrets_404s_and_is_cleared_on_remove() {
    use crate::agents::activity::{ActivityEvent, ActivityKind, ActivitySink as _};
    use crate::server::events::BusActivitySink;

    let dir = temp_dir("live-activity");
    let state = state_at(dir.join("config.toml"));
    let id = create(&state, "Live", &dir.join("proj")).await;

    let (status, _) = call(&state, get("/api/projects/nao-existe/activity")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, text) = call(&state, get(&format!("/api/projects/{id}/activity"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&text)["activities"], json!([]));

    let secrets = Arc::new(SecretSet::new(["chave-super-secreta".to_string()]));
    let sink = BusActivitySink::new(state.events().clone(), id.clone(), secrets);
    let ev = |kind, text: Option<&str>| ActivityEvent {
        agent: "developer".into(),
        spec_id: Some("026".into()),
        task_id: Some("T02".into()),
        activity_id: "act-1".into(),
        kind,
        text: text.map(String::from),
        code: None,
        received_bytes: None,
    };
    sink.emit(ev(ActivityKind::Start, None));
    sink.emit(ev(ActivityKind::Reasoning, Some("usar chave-super-secreta agora")));
    let (status, text) = call(&state, get(&format!("/api/projects/{id}/activity"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!text.contains("chave-super-secreta"), "{text}");
    let v = json_of(&text);
    let a = &v["activities"][0];
    assert_eq!(a["agent"], "developer");
    assert_eq!(a["task_id"], "T02");
    assert_eq!(a["spec_id"], "026");
    assert_eq!(a["activity_id"], "act-1");
    assert!(a["reasoning"].as_str().expect("reasoning").contains("usar"));
    assert!(a["age_ms"].is_u64());

    let (status, _) = call(&state, delete(&id)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.events().live_activity().snapshot(&id).is_empty());
}
