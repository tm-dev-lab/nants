//! Testes do explorador de arquivos (`file_explorer.rs`). Incluídos por ele.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, process};

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::*;
use crate::config::{Config, Connection, Project, WireApi};
use crate::server::{router, AppState};

const PROJECT: &str = "proj-files";
const HOST: &str = "127.0.0.1:3000";
const SECRET: &str = "sk-SEGREDO-ARQUIVOS-777";

fn temp_root(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-files-{}-{tag}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("diretório temporário");
    dir
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=Teste", "-c", "user.email=teste@example.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// Repositório com `src/main.rs`, `.gitignore` (`target/`, `.env`) e `.sdd/specs/x/spec.md`,
/// commitado; depois cria o que o git ignora.
fn repo(tag: &str) -> PathBuf {
    let root = temp_root(tag);
    fs::create_dir_all(root.join("src")).expect("src");
    fs::create_dir_all(root.join(".sdd/specs/x")).expect("sdd");
    fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("main");
    fs::write(root.join(".gitignore"), "target/\n.env\n").expect("gitignore");
    fs::write(root.join(".sdd/specs/x/spec.md"), "spec\n").expect("spec");
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    fs::create_dir_all(root.join("target/debug")).expect("target");
    fs::write(root.join("target/out.txt"), "saida\n").expect("out");
    fs::create_dir_all(root.join(".sdd/worktrees/T01")).expect("worktrees");
    fs::write(root.join(".sdd/worktrees/T01/segredo.txt"), "wt\n").expect("wt");
    root
}

fn state(root: &Path) -> AppState {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "literal".into(),
        name: "Conexão de teste".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
    cfg.projects.push(Project {
        id: PROJECT.into(),
        name: "Arquivos".into(),
        path: root.display().to_string(),
        created_at: "2026-10-16T00:00:00Z".into(),
    });
    AppState::new(root.join("config.toml"), cfg, 3000)
}

async fn request(state: &AppState, uri: &str, inm: Option<&str>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut builder = Request::builder().method(Method::GET).uri(uri).header(header::HOST, HOST);
    if let Some(tag) = inm {
        builder = builder.header(header::IF_NONE_MATCH, tag);
    }
    let req = builder.body(Body::empty()).expect("requisição");
    let resp = router(state.clone()).oneshot(req).await.expect("resposta");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 8 * 1024 * 1024).await.expect("corpo");
    (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn get(state: &AppState, uri: &str) -> (StatusCode, Value) {
    let (s, _, v) = request(state, uri, None).await;
    (s, v)
}

fn files(project: &str, query: &str) -> String {
    format!("/api/projects/{project}/files?{query}")
}

fn file(query: &str) -> String {
    format!("/api/projects/{PROJECT}/file?{query}")
}

fn kind_of(v: &Value, path: &str) -> Option<String> {
    v["entries"]
        .as_array()?
        .iter()
        .find(|e| e["path"] == path)
        .map(|e| e["kind"].as_str().unwrap_or("").to_string())
}

#[tokio::test]
async fn lists_tracked_and_untracked_flat_with_kinds() {
    let root = repo("list");
    fs::write(root.join("novo.txt"), "x").expect("novo");
    symlink("src/main.rs", root.join("link.rs")).expect("symlink");
    let st = state(&root);
    let (status, v) = get(&st, &files(PROJECT, "source=base")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["source"], "base");
    assert_eq!(v["truncated"], false);
    assert_eq!(kind_of(&v, "src/main.rs").as_deref(), Some("file"));
    assert_eq!(kind_of(&v, "novo.txt").as_deref(), Some("file"));
    assert_eq!(kind_of(&v, ".sdd/specs/x/spec.md").as_deref(), Some("file"));
    assert_eq!(kind_of(&v, "link.rs").as_deref(), Some("symlink"));
    assert!(v["entries"][0].get("target").is_none());
}

#[tokio::test]
async fn git_dir_and_sdd_worktrees_are_never_listed() {
    let root = repo("hidden");
    let st = state(&root);
    let (_, v) = get(&st, &files(PROJECT, "source=base")).await;
    for e in v["entries"].as_array().expect("entries") {
        let p = e["path"].as_str().expect("path");
        assert!(!p.split('/').any(|c| c == ".git"), "{p}");
        assert!(!p.starts_with(".sdd/worktrees"), "{p}");
    }
    assert!(kind_of(&v, ".git").is_none());
    assert!(kind_of(&v, ".sdd/worktrees").is_none());
    for q in ["dir=.git", "dir=.sdd/worktrees", "dir=.sdd/worktrees/T01"] {
        let (s, _) = get(&st, &files(PROJECT, &format!("source=base&{q}"))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{q}");
    }
    for q in ["path=.git/config", "path=.sdd/worktrees/T01/segredo.txt"] {
        let (s, _) = get(&st, &file(&format!("source=base&{q}"))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{q}");
    }
}

#[tokio::test]
async fn ignored_dir_is_listed_collapsed_and_expands_via_dir() {
    let root = repo("ignored");
    fs::write(root.join("target/debug/a.bin"), "a").expect("a");
    let st = state(&root);
    let (_, v) = get(&st, &files(PROJECT, "source=base")).await;
    assert_eq!(kind_of(&v, "target").as_deref(), Some("ignored_dir"));
    assert!(kind_of(&v, "target/out.txt").is_none(), "sem varredura recursiva");
    let (s, d) = get(&st, &files(PROJECT, "source=base&dir=target")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(d["dir"], "target");
    assert_eq!(kind_of(&d, "target/out.txt").as_deref(), Some("file"));
    assert_eq!(kind_of(&d, "target/debug").as_deref(), Some("dir"));
    assert_eq!(d["truncated"], false);
}

#[tokio::test]
async fn dir_listing_is_capped_at_500_and_marks_truncated() {
    let root = repo("dircap");
    for i in 0..510 {
        fs::write(root.join(format!("target/f{i:04}")), "x").expect("f");
    }
    let st = state(&root);
    let (s, d) = get(&st, &files(PROJECT, "source=base&dir=target")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(d["entries"].as_array().expect("entries").len(), 500);
    assert_eq!(d["truncated"], true);
}

#[tokio::test]
async fn flat_list_is_capped_at_20000_and_marks_truncated() {
    let root = repo("listcap");
    fs::create_dir_all(root.join("muitos")).expect("dir");
    for i in 0..20_001 {
        fs::write(root.join(format!("muitos/f{i:05}")), "").expect("f");
    }
    let st = state(&root);
    let (s, v) = get(&st, &files(PROJECT, "source=base")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["entries"].as_array().expect("entries").len(), MAX_LIST_PATHS);
    assert_eq!(v["truncated"], true);
}

#[tokio::test]
async fn confinement_rejects_dotdot_absolute_and_symlinks() {
    let root = repo("confine");
    let outside = temp_root("confine-out");
    fs::write(outside.join("fora.txt"), "fora").expect("fora");
    symlink(&outside, root.join("ponte")).expect("dir link");
    symlink(outside.join("fora.txt"), root.join("final.txt")).expect("file link");
    let st = state(&root);
    for q in [
        "path=../x",
        "path=src/../../x",
        "path=/etc/passwd",
        "path=src//main.rs",
        "path=ponte/fora.txt",
        "path=final.txt",
        "path=src",
    ] {
        let (s, v) = get(&st, &file(&format!("source=base&{q}"))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(v["code"], "path_invalid", "{q}");
    }
    for q in ["dir=..", "dir=/etc", "dir=ponte", "dir=src/main.rs"] {
        let (s, _) = get(&st, &files(PROJECT, &format!("source=base&{q}"))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{q}");
    }
    let (_, v) = get(&st, &files(PROJECT, "source=base")).await;
    assert_eq!(kind_of(&v, "ponte").as_deref(), Some("symlink"));
    assert_eq!(kind_of(&v, "final.txt").as_deref(), Some("symlink"));
}

#[tokio::test]
async fn unknown_project_missing_file_and_bad_source() {
    let root = repo("errors");
    let st = state(&root);
    let (s, _) = get(&st, &files("nao-existe", "source=base")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = get(&st, &file("source=base&path=src/nada.rs")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["code"], "not_found");
    let (s, _) = get(&st, &file("source=outra&path=src/main.rs")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&st, &files(PROJECT, "source=outra")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&st, &file("source=base")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn no_write_routes_exist() {
    let root = repo("nowrite");
    let st = state(&root);
    for method in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
        for uri in [files(PROJECT, "source=base"), file("source=base&path=src/main.rs")] {
            let req = Request::builder()
                .method(method.clone())
                .uri(&uri)
                .header(header::HOST, HOST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("requisição");
            let resp = router(st.clone()).oneshot(req).await.expect("resposta");
            assert!(
                resp.status() == StatusCode::METHOD_NOT_ALLOWED || resp.status() == StatusCode::NOT_FOUND,
                "{method} {uri}: {}",
                resp.status()
            );
        }
    }
}

#[tokio::test]
async fn text_binary_and_too_large() {
    let root = repo("kinds");
    fs::write(root.join("bin.dat"), [1u8, 0, 2, 3]).expect("bin");
    fs::write(root.join("latin.txt"), [0x66u8, 0xE9, 0x67]).expect("latin");
    let big = vec![b'a'; (MAX_FILE_BYTES as usize) + 1];
    fs::write(root.join("grande.txt"), &big).expect("grande");
    fs::write(root.join("limite.txt"), &big[..MAX_FILE_BYTES as usize]).expect("limite");
    let st = state(&root);
    let (s, v) = get(&st, &file("source=base&path=src/main.rs")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["kind"], "text");
    assert_eq!(v["content"], "fn main() {}\n");
    assert_eq!(v["size"], 13);
    assert_eq!(v["masked"], false);
    let (_, v) = get(&st, &file("source=base&path=bin.dat")).await;
    assert_eq!(v["kind"], "binary");
    assert_eq!(v["size"], 4);
    assert!(v.get("content").is_none());
    let (_, v) = get(&st, &file("source=base&path=latin.txt")).await;
    assert_eq!(v["kind"], "binary");
    let (_, v) = get(&st, &file("source=base&path=grande.txt")).await;
    assert_eq!(v["kind"], "too_large");
    assert_eq!(v["size"], MAX_FILE_BYTES + 1);
    let (_, v) = get(&st, &file("source=base&path=limite.txt")).await;
    assert_eq!(v["kind"], "text");
}

#[tokio::test]
async fn ignored_file_values_are_masked_unless_reveal() {
    let root = repo("mask");
    fs::write(
        root.join(".env"),
        "# comentario=1\nAPI_KEY=abc123\nexport TOKEN: xyz\r\nVAZIO=\nlinha livre\n",
    )
    .expect("env");
    fs::write(root.join("config.txt"), "API_KEY=publico\n").expect("config");
    let st = state(&root);
    let (s, v) = get(&st, &file("source=base&path=.env")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["masked"], true);
    assert_eq!(
        v["content"],
        "# comentario=1\nAPI_KEY=•••\nexport TOKEN: •••\r\nVAZIO=\nlinha livre\n"
    );
    let (_, v) = get(&st, &file("source=base&path=.env&reveal=1")).await;
    assert_eq!(v["masked"], false);
    assert!(v["content"].as_str().expect("content").contains("API_KEY=abc123"));
    // Arquivo não ignorado pelo git não é mascarado.
    let (_, v) = get(&st, &file("source=base&path=config.txt")).await;
    assert_eq!(v["masked"], false);
    assert_eq!(v["content"], "API_KEY=publico\n");
}

#[tokio::test]
async fn known_secret_is_scrubbed_even_with_reveal() {
    let root = repo("scrub");
    fs::write(root.join("notas.txt"), format!("chave {SECRET} e url https://u:senha@host/x\n")).expect("notas");
    let st = state(&root);
    for q in ["source=base&path=notas.txt", "source=base&path=notas.txt&reveal=1"] {
        let (_, v) = get(&st, &file(q)).await;
        let content = v["content"].as_str().expect("content");
        assert!(!content.contains(SECRET), "{content}");
        assert!(!content.contains("senha"), "{content}");
    }
}

#[tokio::test]
async fn etag_gives_304_and_changes_with_file() {
    let root = repo("etag");
    let st = state(&root);
    let uri = file("source=base&path=src/main.rs");
    let (s, headers, _) = request(&st, &uri, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    let etag = headers[header::ETAG].to_str().expect("etag").to_string();
    let (s, headers, _) = request(&st, &uri, Some(&etag)).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
    assert_eq!(headers[header::ETAG].to_str().expect("etag"), etag);
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    fs::write(root.join("src/main.rs"), "fn main() { println!(); }\n").expect("rewrite");
    let (s, headers, v) = request(&st, &uri, Some(&etag)).await;
    assert_eq!(s, StatusCode::OK);
    assert_ne!(headers[header::ETAG].to_str().expect("etag"), etag);
    assert!(v["content"].as_str().expect("content").contains("println"));
}

#[test]
fn mask_line_rules() {
    assert_eq!(mask_line("A=1").as_deref(), Some("A=•••"));
    assert_eq!(mask_line("  a.b-c: valor").as_deref(), Some("  a.b-c: •••"));
    assert_eq!(mask_line("export K = v").as_deref(), Some("export K = •••"));
    assert_eq!(mask_line("A="), None);
    assert_eq!(mask_line("A=   "), None);
    assert_eq!(mask_line("=x"), None);
    assert_eq!(mask_line("1A=x"), None);
    assert_eq!(mask_line("só texto"), None);
}

#[tokio::test]
async fn ignored_root_file_is_listed_and_masked() {
    let root = repo("ignfile");
    fs::write(root.join(".env"), "API_KEY=abc\n").expect("env");
    let st = state(&root);
    let (_, v) = get(&st, &files(PROJECT, "source=base")).await;
    assert_eq!(kind_of(&v, ".env").as_deref(), Some("file"));
    let (_, f) = get(&st, &file("source=base&path=.env")).await;
    assert_eq!(f["masked"], true);
    assert_eq!(f["content"], "API_KEY=•••\n");
}

#[test]
fn listing_cache_expires_with_ttl() {
    let root = repo("ttl");
    let first = cached_listing(Source::Base, &root, Duration::from_secs(3600)).expect("lista");
    fs::write(root.join("novo.txt"), "x").expect("novo");
    let cached = cached_listing(Source::Base, &root, Duration::from_secs(3600)).expect("lista");
    assert!(!cached.entries.iter().any(|e| e.path == "novo.txt"));
    assert_eq!(first.entries.len(), cached.entries.len());
    let fresh = cached_listing(Source::Base, &root, Duration::ZERO).expect("lista");
    assert!(fresh.entries.iter().any(|e| e.path == "novo.txt"));
}

#[tokio::test]
async fn etag_changes_when_file_becomes_ignored() {
    let root = repo("etagign");
    fs::write(root.join("a.cfg"), "K=v\n").expect("a");
    let st = state(&root);
    let uri = file("source=base&path=a.cfg");
    let (_, h1, v1) = request(&st, &uri, None).await;
    assert_eq!(v1["masked"], false);
    let old = h1[header::ETAG].to_str().expect("etag").to_string();
    fs::write(root.join(".gitignore"), "target/\n.env\na.cfg\n").expect("ignore");
    let (s, h2, v2) = request(&st, &uri, Some(&old)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v2["masked"], true);
    assert_ne!(h2[header::ETAG].to_str().expect("etag"), old);
}

// ---- T02: fonte `task` (worktree da tarefa ativa), alterados e diff ----

fn task_md(status: &str) -> String {
    format!(
        "---\nid: \"T01\"\nspec_id: \"x\"\ntitle: \"t\"\nstatus: \"{status}\"\ndepends_on: []\n\
         branch: \"sdd/x/T01\"\nattempts: 0\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\n\
         allowed_scope:\n  - \"src/**\"\n---\n\n## Objetivo\nx\n"
    )
}

/// Repositório base + tarefa T01 da spec `x` com worktree real, uma alteração commitada
/// (`src/main.rs`), uma nova commitada (`src/novo.rs`) e uma não commitada (`wip.txt`).
fn task_repo(tag: &str, status: &str) -> PathBuf {
    let root = repo(tag);
    fs::create_dir_all(root.join(".sdd/specs/x/tasks")).expect("tasks");
    fs::write(root.join(".sdd/specs/x/tasks/T01.md"), task_md(status)).expect("task");
    git(&root, &["worktree", "add", "-q", "-b", "sdd/x/T01", ".sdd/worktrees/x/T01"]);
    let wt = root.join(".sdd/worktrees/x/T01");
    fs::write(wt.join("src/main.rs"), "fn main() { println!(\"tarefa\"); }\n").expect("main");
    fs::write(wt.join("src/novo.rs"), "pub fn novo() {}\n").expect("novo");
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-q", "-m", "wip"]);
    fs::write(wt.join("wip.txt"), "nao commitado\n").expect("wip");
    root
}

fn changed(v: &Value) -> Vec<String> {
    v["changed"]
        .as_array()
        .map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn task_source_without_active_task_is_409_static() {
    let root = repo("t2-none");
    let st = state(&root);
    for uri in [files(PROJECT, "source=task"), file("source=task&path=src/main.rs")] {
        let (s, v) = get(&st, &uri).await;
        assert_eq!(s, StatusCode::CONFLICT, "{uri}");
        assert_eq!(v, serde_json::json!({ "reason": "no_active_task" }));
    }
    // Tarefa ativa no papel, mas sem worktree: continua 409.
    fs::create_dir_all(root.join(".sdd/specs/x/tasks")).expect("tasks");
    fs::write(root.join(".sdd/specs/x/tasks/T01.md"), task_md("IN_PROGRESS")).expect("task");
    let (s, v) = get(&st, &files(PROJECT, "source=task")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["reason"], "no_active_task");
}

#[tokio::test]
async fn task_source_with_finished_task_is_409() {
    let root = task_repo("t2-done", "DONE");
    let st = state(&root);
    let (s, v) = get(&st, &files(PROJECT, "source=task")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["reason"], "no_active_task");
}

#[tokio::test]
async fn task_source_lists_worktree_with_task_id_and_changed() {
    for status in ["IN_PROGRESS", "IN_REVIEW", "CHANGES_REQUESTED", "BLOCKED"] {
        let root = task_repo(&format!("t2-list-{status}"), status);
        let st = state(&root);
        let (s, v) = get(&st, &files(PROJECT, "source=task")).await;
        assert_eq!(s, StatusCode::OK, "{status}");
        assert_eq!(v["source"], "task");
        assert_eq!(v["task_id"], "T01");
        assert_eq!(kind_of(&v, "src/novo.rs").as_deref(), Some("file"));
        assert_eq!(kind_of(&v, "wip.txt").as_deref(), Some("file"));
        assert_eq!(kind_of(&v, ".sdd/worktrees"), None);
        let mut ch = changed(&v);
        ch.sort();
        assert_eq!(ch, vec!["src/main.rs", "src/novo.rs", "wip.txt"], "{status}");
        // A base não enxerga a worktree e não tem `changed`; o cache é por fonte e raiz.
        let (_, b) = get(&st, &files(PROJECT, "source=base")).await;
        assert_eq!(kind_of(&b, "src/novo.rs"), None);
        assert!(b.get("changed").is_none() && b.get("task_id").is_none());
        let (_, again) = get(&st, &files(PROJECT, "source=task")).await;
        assert_eq!(kind_of(&again, "src/novo.rs").as_deref(), Some("file"));
    }
}

#[tokio::test]
async fn task_source_reads_file_from_worktree_and_dir_listing() {
    let root = task_repo("t2-read", "IN_PROGRESS");
    let st = state(&root);
    let (s, v) = get(&st, &file("source=task&path=src/main.rs")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["content"].as_str().unwrap_or("").contains("tarefa"));
    let (_, base) = get(&st, &file("source=base&path=src/main.rs")).await;
    assert_eq!(base["content"], "fn main() {}\n");
    fs::create_dir_all(root.join(".sdd/worktrees/x/T01/target")).expect("target");
    fs::write(root.join(".sdd/worktrees/x/T01/target/o.txt"), "o").expect("o");
    let (s, v) = get(&st, &files(PROJECT, "source=task&dir=target")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(kind_of(&v, "target/o.txt").as_deref(), Some("file"));
}

#[tokio::test]
async fn task_diff_returns_unified_diff_scrubbed_and_base_is_rejected() {
    let root = task_repo("t2-diff", "IN_REVIEW");
    let wt = root.join(".sdd/worktrees/x/T01");
    fs::write(wt.join("src/segredo.rs"), format!("const K: &str = \"{SECRET}\";\n")).expect("seg");
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-q", "-m", "seg"]);
    let st = state(&root);
    let (s, v) = get(&st, &file("source=task&path=src/main.rs&diff=1")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["kind"], "diff");
    let body = v["content"].as_str().unwrap_or("");
    assert!(body.contains("-fn main() {}") && body.contains("+fn main() { println!"), "{body}");
    let (_, v) = get(&st, &file("source=task&path=src/segredo.rs&diff=1")).await;
    let body = v["content"].as_str().unwrap_or("");
    assert!(!body.contains(SECRET) && body.contains("const K"), "{body}");
    let (s, v) = get(&st, &file("source=base&path=src/main.rs&diff=1")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["code"], "source_invalid");
    // Sem tarefa ativa, o diff também é 409.
    let (s, _) = get(&state(&repo("t2-diff-none")), &file("source=task&path=a&diff=1")).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn task_diff_above_limit_is_too_large() {
    let root = task_repo("t2-big", "IN_PROGRESS");
    let wt = root.join(".sdd/worktrees/x/T01");
    let big: String = (0..40_000).map(|i| format!("linha numero {i}\n")).collect();
    fs::write(wt.join("grande.txt"), big).expect("grande");
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-q", "-m", "grande"]);
    let st = state(&root);
    let (s, v) = get(&st, &file("source=task&path=grande.txt&diff=1")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["kind"], "too_large");
    assert!(v["size"].as_u64().unwrap_or(0) > MAX_FILE_BYTES);
}

#[tokio::test]
async fn task_worktree_symlinks_are_refused() {
    let root = task_repo("t2-sym", "IN_PROGRESS");
    let wt = root.join(".sdd/worktrees/x/T01");
    let outside = temp_root("t2-sym-out");
    fs::write(outside.join("fora.txt"), "fora").expect("fora");
    symlink(&outside, wt.join("ponte")).expect("dir link");
    symlink(outside.join("fora.txt"), wt.join("final.txt")).expect("file link");
    let st = state(&root);
    for q in ["path=ponte/fora.txt", "path=final.txt", "path=../x", "path=.git"] {
        for extra in ["", "&diff=1"] {
            let (s, _) = get(&st, &file(&format!("source=task&{q}{extra}"))).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{q}{extra}");
        }
    }
    let (s, _) = get(&st, &files(PROJECT, "source=task&dir=ponte")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, v) = get(&st, &files(PROJECT, "source=task")).await;
    assert_eq!(kind_of(&v, "ponte").as_deref(), Some("symlink"));
    assert_eq!(kind_of(&v, "final.txt").as_deref(), Some("symlink"));
}

#[tokio::test]
async fn task_source_ignores_symlinked_worktree_dir() {
    let root = task_repo("t2-wtlink", "IN_PROGRESS");
    let real = root.join(".sdd/worktrees/x/T01");
    let moved = temp_root("t2-wtlink-moved").join("T01");
    fs::rename(&real, &moved).expect("mover");
    symlink(&moved, &real).expect("link");
    let (s, v) = get(&state(&root), &files(PROJECT, "source=task")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["reason"], "no_active_task");
}

#[tokio::test]
async fn task_diff_includes_uncommitted_untracked_and_binary() {
    let root = task_repo("t2-wip", "IN_PROGRESS");
    let wt = root.join(".sdd/worktrees/x/T01");
    // Modificado e NÃO commitado sobre arquivo da base.
    fs::write(wt.join("src/main.rs"), "fn main() { ainda_nao_commitado(); }\n").expect("mod");
    // Novo não rastreado, com segredo; binário não rastreado; ignorado.
    fs::write(wt.join("wip.txt"), format!("linha1\n{SECRET}\n")).expect("wip");
    fs::write(wt.join("bin.dat"), [0u8, 1, 2, 3]).expect("bin");
    fs::write(wt.join(".env"), "K=v\n").expect("env");
    let st = state(&root);
    let (s, v) = get(&st, &file("source=task&path=src/main.rs&diff=1")).await;
    assert_eq!(s, StatusCode::OK);
    let body = v["content"].as_str().unwrap_or("");
    assert!(body.contains("-fn main() {}") && body.contains("+fn main() { ainda_nao_commitado"), "{body}");
    let (_, v) = get(&st, &file("source=task&path=wip.txt&diff=1")).await;
    assert_eq!(v["kind"], "diff");
    let body = v["content"].as_str().unwrap_or("");
    assert!(body.contains("@@ -0,0 +1,2 @@") && body.contains("+linha1"), "{body}");
    assert!(!body.contains(SECRET), "{body}");
    let (_, v) = get(&st, &file("source=task&path=bin.dat&diff=1")).await;
    assert_eq!(v["kind"], "binary");
    let (_, v) = get(&st, &file("source=task&path=.env&diff=1")).await;
    assert_eq!(v["content"], "");
}

#[tokio::test]
async fn task_diff_magic_pathspecs_never_expand_to_other_files() {
    let root = task_repo("t2-magic", "IN_PROGRESS");
    let wt = root.join(".sdd/worktrees/x/T01");
    fs::write(wt.join("src/main.rs"), "fn main() { alterado(); }\n").expect("mod");
    let st = state(&root);
    for q in ["*", "**", ":(glob)**", ":(top)src/main.rs", "src/*", "src/main.r%2A"] {
        let (s, v) = get(&st, &file(&format!("source=task&path={q}&diff=1"))).await;
        let body = v["content"].as_str().unwrap_or("");
        assert!(
            s == StatusCode::BAD_REQUEST || (s == StatusCode::OK && body.is_empty()),
            "{q}: {s} {v}"
        );
        assert!(!body.contains("alterado") && !body.contains("println"), "{q}: {body}");
    }
    // O caminho literal continua funcionando.
    let (_, v) = get(&st, &file("source=task&path=src/main.rs&diff=1")).await;
    assert!(v["content"].as_str().unwrap_or("").contains("+fn main() { alterado"));
}

#[tokio::test]
async fn task_source_prefers_in_progress_over_blocked_and_resolves_blocked_alone() {
    // BLOCKED sozinha (com worktree) é resolvida.
    let root = task_repo("t4-blocked", "BLOCKED");
    let (s, v) = get(&state(&root), &files(PROJECT, "source=task")).await;
    assert_eq!((s, v["task_id"].as_str()), (StatusCode::OK, Some("T01")));
    // T01 BLOCKED + T02 IN_PROGRESS (ambas com worktree): T02 vence mesmo vindo depois na ordem.
    let t2 = task_md("IN_PROGRESS").replace("\"T01\"", "\"T02\"").replace("sdd/x/T01", "sdd/x/T02");
    fs::write(root.join(".sdd/specs/x/tasks/T02.md"), t2).expect("t2");
    git(&root, &["worktree", "add", "-q", "-b", "sdd/x/T02", ".sdd/worktrees/x/T02"]);
    let (s, v) = get(&state(&root), &files(PROJECT, "source=task")).await;
    assert_eq!((s, v["task_id"].as_str()), (StatusCode::OK, Some("T02")));
}
