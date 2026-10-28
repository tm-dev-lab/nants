//! Testes de `GET /api/projects/:id/specs/:spec_id/result` em repositórios Git reais.
//! Incluídos por `spec_result.rs`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::{env, process};

use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::{Config, Connection, WireApi};
use crate::server::{router, AppState};

const PORT: u16 = 3000;
const HOST: &str = "127.0.0.1:3000";
const SPEC: &str = "001-demo";
const SECRET: &str = "sk-resultado-segredo-123456";

const SPEC_MD: &str = r#"---
id: "001-demo"
title: "Demo do resultado"
status: "COMPLETED"
base_branch: "main"
created_at: "2026-10-14T10:00:00Z"
---

## Contexto
Corpo da spec.
"#;

fn task_md(id: &str, status: &str) -> String {
    format!(
        r#"---
id: "{id}"
spec_id: "001-demo"
title: "Tarefa {id}"
status: "{status}"
depends_on: []
branch: ""
attempts: 1
max_attempts: 3
last_review_verdict: "APPROVED"
allowed_scope:
  - "src/**"
---

## Objetivo
Objetivo de {id}.
"#
    )
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-spec-result-{}-{}", process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Escritas do próprio teste. A manutenção automática fica desligada: depois de `commit`, o Git
/// roda `git maintenance run --auto`, que cria `.git/objects/maintenance.lock` mesmo sem nada a
/// fazer. Do git 2.47 em diante esse processo roda destacado (`--detach`) e pode seguir vivo
/// depois que o `commit` termina; o lock aparecia ou sumia entre dois `snapshot` e o teste de
/// ausência de efeitos falhava sem que a leitura do produto tivesse escrito nada.
fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("pai")).expect("mkdir");
    fs::write(path, content).expect("escrever");
}

/// Commit com `files` (conteúdo `Some`) e remoções (`None`). `git add -A` cobre o resto.
fn commit(root: &Path, files: &[(&str, Option<&str>)], msg: &str) {
    for (rel, content) in files {
        match content {
            Some(text) => write(&root.join(rel), text),
            None => fs::remove_file(root.join(rel)).expect("remover"),
        }
    }
    git(root, &["add", "-A", "-f", "."]);
    git(root, &["commit", "-q", "--allow-empty", "-m", msg]);
}

/// Árvore completa (conteúdo, diretórios e alvos de links), para provar ausência de efeitos.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(dir).expect("listar") {
            let path = entry.expect("entrada").path();
            let rel = path.strip_prefix(base).expect("relativo").to_path_buf();
            let meta = fs::symlink_metadata(&path).expect("metadados");
            if meta.file_type().is_symlink() {
                out.insert(rel, format!("link:{:?}", fs::read_link(&path).expect("link")));
            } else if meta.is_dir() {
                out.insert(rel, "dir".into());
                walk(base, &path, out);
            } else {
                out.insert(rel, format!("file:{:?}", fs::read(&path).expect("ler")));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Caminhos que diferem entre dois `snapshot` (só os nomes, nunca o conteúdo).
fn changed_paths(a: &BTreeMap<PathBuf, String>, b: &BTreeMap<PathBuf, String>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> =
        a.keys().chain(b.keys()).filter(|p| a.get(*p) != b.get(*p)).cloned().collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Saída somente leitura do Git que descreve commits, refs e worktrees.
fn git_state(root: &Path) -> String {
    let run = |args: &[&str]| {
        let out = Command::new("git").args(args).current_dir(root).output().expect("git");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    format!(
        "{}\n{}\n{}\n{}",
        run(&["log", "--all", "--format=%H %s"]),
        run(&["for-each-ref"]),
        run(&["worktree", "list", "--porcelain"]),
        run(&["ls-files", "-s"]),
    )
}

fn request(method: Method, uri: &str, host: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, host)
        .body(Body::empty())
        .expect("requisição")
}

fn get(uri: &str) -> Request<Body> {
    request(Method::GET, uri, HOST)
}

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, String) {
    let resp: Response<Body> = router(state.clone()).oneshot(req).await.expect("resposta");
    let status = resp.status();
    let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("corpo");
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).expect("json")
}

fn state_with(dir: &Path, cfg: Config) -> AppState {
    AppState::new(dir.join("config.toml"), crate::server::readiness::chat_ready(cfg), PORT)
}

/// Cadastra `path` pelo fluxo de produção (`POST /api/projects`) e devolve o id.
async fn create(state: &AppState, path: &Path) -> String {
    let body = json!({ "name": "Projeto Resultado", "path": path.to_str().expect("utf-8") }).to_string();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/projects")
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("requisição");
    let (status, text) = call(state, req).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    json_of(&text)["id"].as_str().expect("id").to_string()
}

/// Repositório `main` com commit vazio inicial, projeto cadastrado e um commit "sdd: base"
/// contendo a spec COMPLETED, três tarefas DONE e um README. Devolve (estado, id, raiz).
async fn project(tag: &str, cfg: Config) -> (AppState, String, PathBuf) {
    let dir = temp_dir(tag);
    let root = dir.join("repo");
    fs::create_dir_all(&root).expect("repo");
    git(&root, &["init", "-q"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let state = state_with(&dir, cfg);
    let id = create(&state, &root).await;
    write(&root.join(".sdd/settings.yaml"), &settings_yaml("node --test"));
    write(&root.join(".sdd/specs/001-demo/spec.md"), SPEC_MD);
    for t in ["T01", "T02", "T03"] {
        write(
            &root.join(format!(".sdd/specs/001-demo/tasks/{t}.md")),
            &task_md(t, "DONE"),
        );
    }
    write(&root.join("README.md"), "Projeto base\n");
    commit(&root, &[], "sdd: base");
    (state, id, root)
}

fn settings_yaml(test_command: &str) -> String {
    format!(
        "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"{test_command}\"\nallowed_commands: []\ndenied_commands: []\n"
    )
}

fn result_uri(id: &str) -> String {
    format!("/api/projects/{id}/specs/{SPEC}/result")
}

/// Três commits de integração `feat(001-demo): Txx - ...` com arquivos variados, mais ruído:
/// commit vazio, commit não relacionado, outra spec e assunto que só parece com o padrão.
fn integrate_three(root: &Path) {
    commit(root, &[], "chore: commit vazio");
    commit(
        root,
        &[("src/app.js", Some("export const a = 1;\n")), ("index.html", Some("<h1>x</h1>\n"))],
        "feat(001-demo): T01 - Estrutura inicial",
    );
    commit(root, &[("docs/notas.txt", Some("nota\n"))], "docs: nota não relacionada");
    commit(root, &[("src/other.js", Some("x\n"))], "feat(002-outra): T01 - outra spec");
    commit(root, &[("src/app.js", Some("export const a = 2;\n"))], "feat(001-demo): T02 - Lógica");
    commit(root, &[("src/app.js", Some("export const a = 3;\n"))], "feat(001-demo): Thing sem id");
    commit(root, &[("test/app.test.js", Some("test\n"))], "feat(001-demo): T03 - Testes");
}

#[tokio::test]
async fn lists_only_integration_commits_in_chronological_order() {
    let (state, id, root) = project("commits", Config::default()).await;
    integrate_three(&root);
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["spec_id"], SPEC);
    assert_eq!(v["title"], "Demo do resultado");
    assert_eq!(v["status"], "COMPLETED");
    let commits = v["commits"].as_array().expect("commits");
    let subjects: Vec<&str> = commits.iter().map(|c| c["subject"].as_str().expect("s")).collect();
    assert_eq!(
        subjects,
        [
            "feat(001-demo): T01 - Estrutura inicial",
            "feat(001-demo): T02 - Lógica",
            "feat(001-demo): T03 - Testes",
        ]
    );
    let task_ids: Vec<&str> = commits.iter().map(|c| c["task_id"].as_str().expect("t")).collect();
    assert_eq!(task_ids, ["T01", "T02", "T03"]);
    for c in commits {
        let hash = c["hash"].as_str().expect("hash");
        assert_eq!(hash.len(), 7, "{hash}");
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()), "{hash}");
        let date = c["date"].as_str().expect("date");
        assert_eq!(date.len(), 20, "{date}");
        assert!(date.ends_with('Z') && date.as_bytes()[10] == b'T', "{date}");
    }
}

#[tokio::test]
async fn files_are_added_or_modified_without_sdd_and_created_then_removed_is_absent() {
    let (state, id, root) = project("files", Config::default()).await;
    integrate_three(&root);
    commit(
        &root,
        &[
            ("tmp.txt", Some("temporário\n")),
            ("README.md", Some("Projeto base\nlinha nova\n")),
        ],
        "feat(001-demo): T04 - Arquivos",
    );
    commit(&root, &[("tmp.txt", None)], "feat(001-demo): T05 - Remoção");
    commit(
        &root,
        &[(".sdd/specs/001-demo/tasks/T03.md", Some(&task_md("T03", "TODO")))],
        "feat(001-demo): T06 - Ajuste de sdd",
    );
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(
        v["files"],
        json!([
            {"path": "README.md", "change": "M"},
            {"path": "index.html", "change": "A"},
            {"path": "src/app.js", "change": "A"},
            {"path": "test/app.test.js", "change": "A"},
        ])
    );
    assert_eq!(v["files_truncated"], false);
    assert!(!body.contains("tmp.txt"), "{body}");
    assert!(!body.contains(".sdd/"), "{body}");
}

#[tokio::test]
async fn files_are_capped_at_200_with_truncated_flag() {
    let (state, id, root) = project("files-cap", Config::default()).await;
    let many: Vec<(String, Option<String>)> = (0..201)
        .map(|i| (format!("gen/f{i:03}.txt"), Some(format!("{i}\n"))))
        .collect();
    let refs: Vec<(&str, Option<&str>)> = many
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_deref()))
        .collect();
    commit(&root, &refs, "feat(001-demo): T01 - Muitos arquivos");
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["files"].as_array().expect("files").len(), 200);
    assert_eq!(v["files_truncated"], true);
    assert_eq!(v["files"][0]["path"], "gen/f000.txt");
}

#[tokio::test]
async fn long_readme_is_truncated_and_redacted_as_plain_text() {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "k".into(),
        name: "K".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
    let (state, id, root) = project("readme-long", cfg).await;
    let mut text = format!("# Projeto\nchave: {SECRET}\nrepo: https://user:pw123@host.example/x\n");
    for i in 0..120 {
        text.push_str(&format!("linha {i:03} com conteúdo de teste para ocupar espaço\n"));
    }
    commit(&root, &[("README.md", Some(&text))], "feat(001-demo): T01 - Readme longo");
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    let readme = &v["readme"];
    let out = readme["text"].as_str().expect("texto");
    assert_eq!(readme["truncated"], true);
    assert!(out.lines().count() <= 60, "{}", out.lines().count());
    assert!(out.len() <= 8 * 1024, "{}", out.len());
    assert!(out.starts_with("# Projeto\n"), "{out}");
    assert!(!body.contains(SECRET), "segredo vazou: {body}");
    assert!(!body.contains("pw123"), "credencial de URL vazou: {body}");
    assert!(!out.contains('\r'), "{out:?}");
}

#[tokio::test]
async fn short_readme_is_returned_whole_and_not_truncated() {
    let (state, id, _root) = project("readme-short", Config::default()).await;
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["readme"]["text"], "Projeto base\n".trim_end());
    assert_eq!(v["readme"]["truncated"], false);
}

#[tokio::test]
async fn readme_symlink_fifo_missing_and_large_are_null() {
    // Symlink para arquivo real dentro do projeto.
    let (state, id, root) = project("readme-link", Config::default()).await;
    fs::remove_file(root.join("README.md")).expect("remover");
    write(&root.join("real.md"), "conteúdo\n");
    std::os::unix::fs::symlink(root.join("real.md"), root.join("README.md")).expect("link");
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(json_of(&body)["readme"].is_null(), "{body}");

    // README ausente.
    let (state, id, root) = project("readme-missing", Config::default()).await;
    fs::remove_file(root.join("README.md")).expect("remover");
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(json_of(&body)["readme"].is_null(), "{body}");

    // FIFO no lugar do README: não pode bloquear.
    let (state, id, root) = project("readme-fifo", Config::default()).await;
    fs::remove_file(root.join("README.md")).expect("remover");
    let fifo = Command::new("mkfifo").arg(root.join("README.md")).status().expect("mkfifo");
    assert!(fifo.success());
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(json_of(&body)["readme"].is_null(), "{body}");

    // README acima de 64 KiB: ignorado.
    let (state, id, root) = project("readme-big", Config::default()).await;
    write(&root.join("README.md"), &"x".repeat(70 * 1024));
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(json_of(&body)["readme"].is_null(), "{body}");
}

#[tokio::test]
async fn spec_without_integration_commits_has_empty_lists() {
    let (state, id, _root) = project("no-commits", Config::default()).await;
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["commits"], json!([]));
    assert_eq!(v["files"], json!([]));
    assert_eq!(v["files_truncated"], false);
    assert_eq!(v["tasks"].as_array().expect("tasks").len(), 3);
}

#[tokio::test]
async fn unknown_project_or_spec_is_static_404() {
    let (state, id, _root) = project("404", Config::default()).await;
    let (status, body) = call(&state, get("/api/projects/nao-existe/specs/001-demo/result")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(json_of(&body)["error"], "projeto não encontrado");
    let (status, body) = call(&state, get(&format!("/api/projects/{id}/specs/999-nada/result"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(json_of(&body)["error"], "spec não encontrada");
}

#[tokio::test]
async fn invalid_ids_are_422_without_echo() {
    let (state, id, _root) = project("422", Config::default()).await;
    let long = "a".repeat(65);
    for uri in [
        "/api/projects/bad.id/specs/001-demo/result".to_string(),
        format!("/api/projects/{id}/specs/bad.id/result"),
        format!("/api/projects/{id}/specs/{long}/result"),
        format!("/api/projects/{id}/specs/a%20b/result"),
    ] {
        let (status, body) = call(&state, get(&uri)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{uri}: {body}");
        assert!(!body.contains("bad.id") && !body.contains("a b"), "{body}");
    }
}

#[tokio::test]
async fn foreign_host_is_403() {
    let (state, id, _root) = project("host", Config::default()).await;
    let req = request(Method::GET, &result_uri(&id), "evil.example:3000");
    let (status, body) = call(&state, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn git_timeout_is_static_and_has_no_effects() {
    let (state, id, root) = project("timeout", Config::default()).await;
    integrate_three(&root);
    // A raiz do projeto é a canônica gravada no cadastro.
    let canonical = root.canonicalize().expect("canonica");
    TEST_TIMEOUTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((canonical.clone(), Duration::from_millis(50)));
    TEST_HOOKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((canonical, Box::new(|| std::thread::sleep(Duration::from_millis(400)))));
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    let v = json_of(&body);
    assert!(v["error"].is_string(), "{body}");
    assert!(!body.contains(root.to_str().expect("utf-8")), "caminho vazou: {body}");
}

#[tokio::test]
async fn reading_result_has_no_side_effects_and_reflects_disk() {
    let (state, id, root) = project("effects", Config::default()).await;
    integrate_three(&root);
    let tree_before = snapshot(&root);
    let git_before = git_state(&root);

    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["project_path"], root.canonicalize().expect("canon").to_str().expect("utf-8"));
    assert_eq!(v["test_command"], "node --test");
    let tasks = v["tasks"].as_array().expect("tasks");
    assert_eq!(tasks[0]["status"], "DONE");

    // Disco muda: a leitura segue o disco, sem gravar nada.
    write(&root.join(".sdd/specs/001-demo/spec.md"), &SPEC_MD.replace("COMPLETED", "IN_PROGRESS"));
    write(&root.join(".sdd/specs/001-demo/tasks/T01.md"), &task_md("T01", "TODO"));
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["status"], "IN_PROGRESS");
    assert_eq!(v["tasks"][0]["status"], "TODO");

    // Árvore e refs idênticas: a leitura não cria worktree, não escreve e não muda status.
    write(&root.join(".sdd/specs/001-demo/spec.md"), SPEC_MD);
    write(&root.join(".sdd/specs/001-demo/tasks/T01.md"), &task_md("T01", "DONE"));
    let tree_mid = snapshot(&root);
    let git_mid = git_state(&root);
    let changed = changed_paths(&tree_before, &tree_mid);
    assert!(changed.is_empty(), "árvore mudou durante as leituras: {changed:?}");
    assert_eq!(git_before, git_mid, "refs mudaram durante as leituras");
    assert!(!root.join(".sdd/worktrees").exists());
}

#[test]
fn task_id_needs_number_and_separator() {
    let prefix = "feat(001-demo): T";
    assert_eq!(task_id_of("feat(001-demo): T01 - Título", prefix).as_deref(), Some("T01"));
    assert_eq!(task_id_of("feat(001-demo): Thing sem id", prefix), None);
    assert_eq!(task_id_of("feat(001-demo): T1x - y", prefix), None);
    assert_eq!(task_id_of("feat(001-demo): T01-y", prefix), None);
    assert_eq!(task_id_of("feat(002-outra): T01 - y", prefix), None);
}

#[test]
fn created_then_removed_vanishes_and_other_sequences_resolve() {
    use Change::{Added, Deleted, Modified};
    assert_eq!(final_change(Added, Deleted), None);
    assert_eq!(final_change(Added, Modified), Some(Added));
    assert_eq!(final_change(Modified, Deleted), Some(Deleted));
    assert_eq!(final_change(Deleted, Added), Some(Modified));
    assert_eq!(final_change(Modified, Modified), Some(Modified));
}

#[tokio::test]
async fn symlinked_git_dir_is_not_followed() {
    let (state, id, root) = project("git-symlink", Config::default()).await;
    integrate_three(&root);
    let store = root.parent().expect("pai").join("store.git");
    fs::rename(root.join(".git"), &store).expect("mover");
    std::os::unix::fs::symlink(&store, root.join(".git")).expect("link");
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["commits"], json!([]), "{body}");
    assert_eq!(v["files"], json!([]), "{body}");
    assert_eq!(v["files_truncated"], false);
}

#[tokio::test]
async fn gitfile_is_not_followed() {
    let (state, id, root) = project("git-file", Config::default()).await;
    integrate_three(&root);
    let store = root.parent().expect("pai").join("store.git");
    fs::rename(root.join(".git"), &store).expect("mover");
    write(&root.join(".git"), &format!("gitdir: {}\n", store.display()));
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["commits"], json!([]), "{body}");
    assert_eq!(v["files"], json!([]), "{body}");
}

#[tokio::test]
async fn subject_keeps_long_text_and_redacts_instead_of_marking_invalid() {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "k".into(),
        name: "K".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
    let (state, id, root) = project("subject", cfg).await;
    let long = "x".repeat(110);
    commit(&root, &[("src/a.js", Some("a\n"))], &format!("feat(001-demo): T01 - {long}"));
    commit(
        &root,
        &[("src/b.js", Some("b\n"))],
        &format!("feat(001-demo): T02 - a\tb {SECRET}"),
    );
    commit(
        &root,
        &[("src/c.js", Some("c\n"))],
        &format!("feat(001-demo): T03 - {}", "y".repeat(250)),
    );
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    let subjects: Vec<&str> = v["commits"]
        .as_array()
        .expect("commits")
        .iter()
        .map(|c| c["subject"].as_str().expect("s"))
        .collect();
    assert_eq!(subjects.len(), 3);
    assert_eq!(subjects[0], format!("feat(001-demo): T01 - {long}"));
    assert!(subjects[1].contains("a b"), "{}", subjects[1]);
    assert!(!subjects[1].contains(SECRET), "{}", subjects[1]);
    assert!(!body.contains("[inválido]"), "{body}");
    assert!(subjects[2].ends_with('…'), "{}", subjects[2]);
    assert_eq!(subjects[2].chars().count(), 201);
}

#[tokio::test]
async fn malformed_task_is_flagged_not_silent() {
    let (state, id, root) = project("bad-task", Config::default()).await;
    write(&root.join(".sdd/specs/001-demo/tasks/T02.md"), "sem frontmatter\n");
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    let tasks = v["tasks"].as_array().expect("tasks");
    assert_eq!(tasks[1]["id"], "T02");
    assert_eq!(tasks[1]["status"], Value::Null);
    assert_eq!(tasks[1]["error"], "arquivo inválido");
    assert!(tasks[0].get("error").is_none(), "{body}");
}

#[tokio::test]
async fn hostile_file_names_and_subjects_do_not_break_the_endpoint() {
    let (state, id, root) = project("hostile", Config::default()).await;
    write(&root.join("a\nb\"c.txt"), "x\n");
    fs::write(root.join(OsStr::from_bytes(b"bad\xff.txt")), "y\n").expect("escrever");
    git(&root, &["add", "-A", "-f", "."]);
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0", "commit", "-q", "-m"])
        .arg(OsStr::from_bytes(b"feat(001-demo): T01 - byte \xff invalido"))
        .current_dir(&root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .status()
        .expect("git");
    assert!(out.success());
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["commits"].as_array().expect("commits").len(), 1, "{body}");
    let paths: Vec<&str> = v["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| f["path"].as_str().expect("path"))
        .collect();
    assert!(paths.contains(&"a\nb\"c.txt"), "{paths:?}");
    assert!(paths.iter().any(|p| p.starts_with("bad") && p.ends_with(".txt")), "{paths:?}");
}

#[tokio::test]
async fn secret_in_test_command_is_redacted() {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "k".into(),
        name: "K".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
    let (state, id, root) = project("cmd-secret", cfg).await;
    write(&root.join(".sdd/settings.yaml"), &settings_yaml(&format!("node --test {SECRET}")));
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains(SECRET), "segredo vazou: {body}");
    assert!(json_of(&body)["test_command"].is_string(), "{body}");
}

#[tokio::test]
async fn invalid_settings_yield_null_test_command() {
    let (state, id, root) = project("cmd-invalid", Config::default()).await;
    write(&root.join(".sdd/settings.yaml"), &settings_yaml(""));
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(json_of(&body)["test_command"].is_null(), "{body}");
}

fn spec_with_run(extra: &str) -> String {
    SPEC_MD.replacen("base_branch: \"main\"\n", &format!("base_branch: \"main\"\n{extra}"), 1)
}

#[tokio::test]
async fn run_fields_present_when_declared() {
    let (state, id, root) = project("run-ok", Config::default()).await;
    write(
        &root.join(".sdd/specs/001-demo/spec.md"),
        &spec_with_run("run_command: \"npm start\"\nrun_url: \"http://localhost:3000/app\"\n"),
    );
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert_eq!(v["run_command"], "npm start");
    assert_eq!(v["run_url"], "http://localhost:3000/app");
}

#[tokio::test]
async fn run_fields_null_when_absent_or_invalid() {
    let (state, id, root) = project("run-none", Config::default()).await;
    let (_, body) = call(&state, get(&result_uri(&id))).await;
    let v = json_of(&body);
    assert!(v["run_command"].is_null() && v["run_url"].is_null(), "{body}");
    write(
        &root.join(".sdd/specs/001-demo/spec.md"),
        &spec_with_run("run_command: \"a\\nb\"\nrun_url: \"https://example.com:3000\"\n"),
    );
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = json_of(&body);
    assert!(v["run_command"].is_null() && v["run_url"].is_null(), "{body}");
}

#[tokio::test]
async fn secret_in_run_command_is_redacted() {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "k".into(),
        name: "K".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
    let (state, id, root) = project("run-secret", cfg).await;
    write(
        &root.join(".sdd/specs/001-demo/spec.md"),
        &spec_with_run(&format!("run_command: \"npm start {SECRET}\"\n")),
    );
    let (status, body) = call(&state, get(&result_uri(&id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains(SECRET), "segredo vazou: {body}");
    assert!(json_of(&body)["run_command"].is_string(), "{body}");
}
