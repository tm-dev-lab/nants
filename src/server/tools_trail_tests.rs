//! Testes da leitura da trilha de ações (`tools.jsonl`), da contagem por tarefa e do GET paginado.
//! Incluídos por `tools_trail.rs`.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::{env, process};

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::{Config, Project};
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";
const SPEC: &str = "001-demo";
const PROJECT: &str = "proj-tools";
const SECRET: &str = "sk-SEGREDO-TRILHA-24680";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-tools-{}-{}", process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Projeto com `.sdd/specs/<SPEC>/spec.md` (o GET exige a spec existente).
fn setup(tag: &str) -> PathBuf {
    let root = temp_dir(tag);
    let spec_dir = root.join(".sdd/specs").join(SPEC);
    fs::create_dir_all(&spec_dir).expect("spec dir");
    fs::write(spec_dir.join("spec.md"), "---\nid: \"001-demo\"\n---\n").expect("spec");
    root
}

fn trail_file(root: &Path) -> PathBuf {
    root.join(".sdd/specs").join(SPEC).join("tools.jsonl")
}

fn line(task: &str, text: &str) -> String {
    json!({"ts": 1_700_000_000u64, "agent": "developer", "task_id": task, "kind": "tool", "text": text})
        .to_string()
}

fn write_trail(root: &Path, lines: &[String]) {
    let mut out = lines.join("\n");
    out.push('\n');
    fs::write(trail_file(root), out).expect("trilha");
}

fn config_for(root: &Path) -> Config {
    let mut cfg = Config::default();
    cfg.projects.push(Project {
        id: PROJECT.into(),
        name: "Trilha".into(),
        path: root.to_str().expect("utf8").into(),
        created_at: "2026-10-18T00:00:00Z".into(),
    });
    cfg
}

async fn get(state: &AppState, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::HOST, HOST)
        .body(Body::empty())
        .expect("requisição");
    let resp = router(state.clone()).oneshot(req).await.expect("resposta");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.expect("corpo");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn uri(project: &str, spec: &str, query: &str) -> String {
    format!("/api/projects/{project}/specs/{spec}/tools{query}")
}

fn seqs(page: &TrailPage) -> Vec<u64> {
    page.items.iter().map(|i| i.seq).collect()
}

#[test]
fn pages_backwards_with_before_and_limit() {
    let root = setup("paginate");
    let lines: Vec<String> = (1..=7).map(|n| line("T01", &format!("ação {n}"))).collect();
    write_trail(&root, &lines);

    let last = read_page(&root, SPEC, None, None, 3);
    assert_eq!(seqs(&last), vec![5, 6, 7], "janela final em ordem cronológica");
    assert_eq!(last.total, 7);
    assert!(last.has_more);

    let prev = read_page(&root, SPEC, None, Some(5), 3);
    assert_eq!(seqs(&prev), vec![2, 3, 4]);
    assert!(prev.has_more);

    let first = read_page(&root, SPEC, None, Some(2), 3);
    assert_eq!(seqs(&first), vec![1]);
    assert!(!first.has_more);
    assert_eq!(first.items[0].text, "ação 1");
}

#[test]
fn filters_by_task_and_final() {
    let root = setup("filter");
    write_trail(
        &root,
        &[
            line("T01", "a"),
            line("T02", "b"),
            line("FINAL", "c"),
            line("T01", "d"),
            line("FINAL", "e"),
        ],
    );
    let t01 = read_page(&root, SPEC, Some("T01"), None, 100);
    assert_eq!(seqs(&t01), vec![1, 4]);
    assert_eq!(t01.total, 2);
    assert!(!t01.has_more);

    let fin = read_page(&root, SPEC, Some("FINAL"), None, 1);
    assert_eq!(seqs(&fin), vec![5]);
    assert_eq!(fin.total, 2);
    assert!(fin.has_more);
    let fin_prev = read_page(&root, SPEC, Some("FINAL"), Some(5), 1);
    assert_eq!(seqs(&fin_prev), vec![3]);
    assert!(!fin_prev.has_more);
}

#[test]
fn invalid_lines_are_skipped_but_keep_positions() {
    let root = setup("invalid");
    write_trail(
        &root,
        &[
            line("T01", "ok 1"),
            "isto não é json".to_string(),
            json!({"agent": "developer"}).to_string(),
            line("T01", "ok 2"),
        ],
    );
    let page = read_page(&root, SPEC, None, None, 100);
    assert_eq!(seqs(&page), vec![1, 4]);
    assert_eq!(page.total, 2);
}

#[test]
fn missing_file_is_empty() {
    let root = setup("missing");
    let page = read_page(&root, SPEC, None, None, 100);
    assert!(page.items.is_empty());
    assert_eq!(page.total, 0);
    assert!(!page.has_more);
    assert!(counts(&root, SPEC, &SecretSet::default()).is_empty());
}

#[test]
fn symlink_is_refused() {
    let root = setup("symlink");
    let outside = temp_dir("symlink-out");
    let target = outside.join("fora.jsonl");
    fs::write(&target, format!("{}\n", line("T01", "segredo de fora"))).expect("fora");
    symlink(&target, trail_file(&root)).expect("ln");
    let page = read_page(&root, SPEC, None, None, 100);
    assert!(page.items.is_empty(), "symlink não pode ser seguido");
    assert!(counts(&root, SPEC, &SecretSet::default()).is_empty());

    // Symlink num diretório intermediário também é recusado.
    let root2 = temp_dir("symlink-dir");
    fs::create_dir_all(outside.join("specs").join(SPEC)).expect("specs fora");
    fs::write(outside.join("specs").join(SPEC).join("tools.jsonl"), format!("{}\n", line("T01", "x"))).expect("w");
    fs::create_dir_all(root2.join(".sdd")).expect(".sdd");
    symlink(outside.join("specs"), root2.join(".sdd/specs")).expect("ln dir");
    assert!(read_page(&root2, SPEC, None, None, 100).items.is_empty());
}

#[test]
fn counts_per_task_follow_file_changes() {
    let root = setup("counts");
    write_trail(&root, &[line("T01", "a"), line("T01", "b"), line("FINAL", "c")]);
    let first = counts(&root, SPEC, &SecretSet::default());
    assert_eq!(first.get("T01"), Some(&2));
    assert_eq!(first.get("FINAL"), Some(&1));
    assert_eq!(first.get("T02"), None);

    // Arquivo cresceu (tamanho muda): a contagem é recalculada.
    write_trail(&root, &[line("T01", "a"), line("T01", "b"), line("FINAL", "c"), line("T02", "d")]);
    let second = counts(&root, SPEC, &SecretSet::default());
    assert_eq!(second.get("T02"), Some(&1));
}

#[tokio::test]
async fn get_returns_page_shape_and_redacts_again() {
    let root = setup("get");
    let mut stage_line: Value = serde_json::from_str(&line("FINAL", "npm test")).expect("json");
    stage_line["kind"] = json!("command");
    stage_line["stage"] = json!("final_verification");
    write_trail(
        &root,
        &[line("T01", &format!("vazou {SECRET}")), stage_line.to_string(), line("T01", "b")],
    );
    let mut cfg = config_for(&root);
    cfg.connections.push(crate::config::Connection {
        id: "c".into(),
        name: "c".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: crate::config::WireApi::default(),
    });
    let state = AppState::new(root.join("config.toml"), cfg, 3000);

    let (st, body) = get(&state, &uri(PROJECT, SPEC, "?task=T01")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!body.to_string().contains(SECRET), "{body}");
    assert_eq!(body["total"], 2);
    assert_eq!(body["has_more"], false);
    assert_eq!(body["items"][0]["seq"], 1);
    assert_eq!(body["items"][0]["text"], "vazou ***");
    assert_eq!(body["items"][1]["seq"], 3);
    assert!(body["items"][0].get("stage").is_none());

    let (_, fin) = get(&state, &uri(PROJECT, SPEC, "?task=FINAL")).await;
    assert_eq!(fin["items"][0]["kind"], "command");
    assert_eq!(fin["items"][0]["stage"], "final_verification");
    assert_eq!(fin["items"][0]["task_id"], "FINAL");

    let (_, paged) = get(&state, &uri(PROJECT, SPEC, "?before=3&limit=1")).await;
    assert_eq!(paged["items"][0]["seq"], 2);
    assert_eq!(paged["has_more"], true);
}

/// Spec 102: `code`/`params` do rótulo e `code` da etapa saem no GET (saneados); `code` fora do
/// formato fica de fora, e a linha antiga, sem `code`, continua legível.
#[tokio::test]
async fn get_keeps_codes_and_reads_old_lines() {
    let root = setup("get-codes");
    let mut coded: Value = serde_json::from_str(&line("T01", "gerando imagem web/a.png")).expect("json");
    coded["code"] = json!("tool.generate_image");
    coded["params"] = json!({"path": format!("web/a.png {SECRET}"), "bad": {"x": 1}});
    coded["stage"] = json!("Escrevendo relatório final");
    coded["stage_code"] = json!("report");
    let mut bad: Value = serde_json::from_str(&line("T01", "lendo x")).expect("json");
    bad["code"] = json!("Tool!");
    bad["params"] = json!({"path": "x"});
    write_trail(&root, &[coded.to_string(), bad.to_string(), line("T01", "lendo y")]);
    let mut cfg = config_for(&root);
    cfg.connections.push(crate::config::Connection {
        id: "c".into(),
        name: "c".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: crate::config::WireApi::default(),
    });
    let state = AppState::new(root.join("config.toml"), cfg, 3000);
    let (st, body) = get(&state, &uri(PROJECT, SPEC, "?task=T01")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!body.to_string().contains(SECRET), "{body}");
    assert_eq!(body["items"][0]["code"], "tool.generate_image");
    assert_eq!(body["items"][0]["params"], json!({"path": "web/a.png ***"}));
    assert_eq!(body["items"][0]["stage_code"], "report");
    assert!(body["items"][1].get("code").is_none() && body["items"][1].get("params").is_none());
    assert_eq!(body["items"][2]["text"], "lendo y");
    assert!(body["items"][2].get("code").is_none());
}

#[tokio::test]
async fn get_missing_file_is_empty_list() {
    let root = setup("get-missing");
    let state = AppState::new(root.join("config.toml"), config_for(&root), 3000);
    let (st, body) = get(&state, &uri(PROJECT, SPEC, "")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body, json!({"items": [], "total": 0, "has_more": false}));
}

#[tokio::test]
async fn get_errors_match_sibling_routes() {
    let root = setup("errors");
    let state = AppState::new(root.join("config.toml"), config_for(&root), 3000);
    let (st, _) = get(&state, &uri("inexistente", SPEC, "")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = get(&state, &uri(PROJECT, "spec-que-nao-existe", "")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = get(&state, &uri("bad.id", SPEC, "")).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _) = get(&state, &uri(PROJECT, "spec.x", "")).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    for bad in ["?limit=0", "?limit=501", "?limit=x", "?before=0", "?before=-1", "?task=a.b", "?task=a%2Fb"] {
        let (st, body) = get(&state, &uri(PROJECT, SPEC, bad)).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {body}");
    }
    let (st, _) = get(&state, &uri(PROJECT, SPEC, "?limit=500")).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn project_state_exposes_tool_counts_per_task() {
    let root = setup("state");
    fs::write(root.join(".sdd/settings.yaml"), "base_branch: main\ntest_command: \"true\"\n").expect("settings");
    write_trail(&root, &[line("T01", "a"), line("T01", "b"), line("FINAL", "c")]);
    let state = AppState::new(root.join("config.toml"), config_for(&root), 3000);
    let (st, body) = get(&state, &format!("/api/projects/{PROJECT}/state")).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["specs"][0]["tool_counts"], json!({"T01": 2, "FINAL": 1}), "{body}");
}

#[test]
fn default_limit_is_100() {
    let root = setup("default-limit");
    let lines: Vec<String> = (1..=130).map(|n| line("T01", &n.to_string())).collect();
    write_trail(&root, &lines);
    let page = read_page(&root, SPEC, None, None, DEFAULT_LIMIT);
    assert_eq!(page.items.len(), 100);
    assert_eq!(page.items.first().map(|i| i.seq), Some(31));
    assert!(page.has_more);
}
