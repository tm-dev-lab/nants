//! Testes da API da base de conhecimento (spec 134/T01): ida e volta de cada rota, exclude local,
//! recusas com código, redação, symlink, duplicata, o PATCH antigo (spec 136 T02) e envios
//! concorrentes. Sem rede.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, process};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::Config;
use crate::kb::{KB_EXCLUDE_LINE, KB_REL, MANIFEST_REL, MAX_UPLOAD_BYTES};
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";
const OCTET: &str = "application/octet-stream";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-kbapi-{}-{}", process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn git(root: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Value) {
    let resp = router(state.clone()).oneshot(req).await.expect("resposta");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024).await.expect("corpo");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn req(method: Method, uri: &str, content_type: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri).header(header::HOST, HOST);
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder.body(Body::from(body)).expect("requisição")
}

fn get(uri: &str) -> Request<Body> {
    req(Method::GET, uri, None, Vec::new())
}

fn json_req(method: Method, uri: &str, body: Value) -> Request<Body> {
    req(method, uri, Some("application/json"), body.to_string().into_bytes())
}

fn upload_req(id: &str, name: &str, body: &[u8]) -> Request<Body> {
    req(Method::POST, &format!("/api/projects/{id}/kb/files?name={name}"), Some(OCTET), body.to_vec())
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
    let body = json!({ "name": "Base", "path": root.to_str().expect("utf-8") });
    let (status, v) = call(&state, json_req(Method::POST, "/api/projects", body)).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let id = v["id"].as_str().expect("id").to_string();
    (state, id, root)
}

fn assert_code(status: StatusCode, v: &Value, want_status: StatusCode, code: &str) {
    assert_eq!(status, want_status, "{v}");
    assert_eq!(v["code"], code, "{v}");
    assert!(v["error"].as_str().is_some_and(|e| !e.is_empty()), "texto de reserva: {v}");
}

#[tokio::test]
async fn every_route_round_trips() {
    let (state, id, root) = project("roundtrip").await;
    let base = format!("/api/projects/{id}/kb");

    let (status, v) = call(&state, get(&base)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({ "docs": [], "has_overview": false }));

    let (status, v) = call(&state, upload_req(&id, "Padr%C3%B5es%20API.md", b"# API\n\nUse REST.\n")).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let doc = v["doc"]["id"].as_str().expect("id").to_string();
    assert!(doc.starts_with("padroes-api-"), "{doc}");
    assert_eq!(v["doc"]["title"], "Padrões API");
    assert_eq!(v["doc"]["status"], "ready");
    assert!(v["doc"].get("role").is_none() && v["doc"].get("applies_to").is_none(), "sem marcação: {v}");
    assert_eq!(v["doc"]["source"], "file");
    assert_eq!(v["doc"]["links_out"], json!([]));

    let (status, v) = call(&state, json_req(Method::POST, &format!("{base}/links"), json!({ "url": "https://exemplo.com/guia" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["doc"]["status"], "pending");
    assert_eq!(v["doc"]["kind"], "link");
    let link = v["doc"]["id"].as_str().expect("id").to_string();

    let (status, v) = call(&state, get(&base)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["docs"].as_array().map(Vec::len), Some(2));

    let (status, v) = call(&state, get(&format!("{base}/docs/{doc}"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["content"], "# API\n\nUse REST.\n");
    assert_eq!(v["doc"]["id"], doc.as_str());

    // Spec 136 T02 (C-06): o PATCH da 134 é aceito; `role` e `applies_to` são ignorados.
    let patch = json!({ "role": "standard", "description": "Padrão de APIs", "applies_to": ["src/api/**"] });
    let (status, v) = call(&state, json_req(Method::PATCH, &format!("{base}/docs/{doc}"), patch)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["doc"]["description"], "Padrão de APIs");
    assert!(v["doc"].get("role").is_none() && v["doc"].get("applies_to").is_none(), "{v}");
    let manifest = fs::read_to_string(root.join(MANIFEST_REL)).expect("manifesto");
    assert!(!manifest.contains("\"role\"") && !manifest.contains("applies_to"), "{manifest}");

    let (status, v) = call(&state, json_req(Method::PUT, &format!("{base}/overview"), json!({ "text": "# Visão\nPadrões da empresa." }))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = call(&state, get(&format!("{base}/overview"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["text"], "# Visão\nPadrões da empresa.");
    let (_, v) = call(&state, get(&base)).await;
    assert_eq!(v["has_overview"], true);

    let (status, v) = call(&state, req(Method::DELETE, &format!("{base}/docs/{link}"), Some("application/json"), Vec::new())).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, v) = call(&state, get(&format!("{base}/docs/{link}"))).await;
    assert_code(status, &v, StatusCode::NOT_FOUND, "kb_doc_not_found");
    let (status, v) = call(&state, get(&format!("{base}/docs/..%2Fetc"))).await;
    assert_code(status, &v, StatusCode::NOT_FOUND, "kb_doc_not_found");
    let (_, v) = call(&state, get(&base)).await;
    assert_eq!(v["docs"].as_array().map(Vec::len), Some(1));
    assert!(root.join(MANIFEST_REL).is_file());
}

#[tokio::test]
async fn the_base_stays_out_of_git_through_the_local_exclude() {
    let (state, id, root) = project("exclude").await;
    let exclude = root.join(".git/info/exclude");
    // Projeto antigo, sem a linha: o primeiro envio a acrescenta.
    let text = fs::read_to_string(&exclude).expect("exclude");
    fs::write(&exclude, text.replace(&format!("{KB_EXCLUDE_LINE}\n"), "")).expect("w");
    assert!(!fs::read_to_string(&exclude).expect("r").lines().any(|l| l == KB_EXCLUDE_LINE));

    let (status, v) = call(&state, upload_req(&id, "a.md", b"texto")).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let (status, _) = call(&state, json_req(Method::PUT, &format!("/api/projects/{id}/kb/overview"), json!({ "text": "x" }))).await;
    assert_eq!(status, StatusCode::OK);
    let lines: Vec<String> = fs::read_to_string(&exclude).expect("r").lines().map(str::to_string).collect();
    assert_eq!(lines.iter().filter(|l| *l == KB_EXCLUDE_LINE).count(), 1);
    assert!(root.join(KB_REL).is_dir());
    let porcelain = git(&root, &["status", "--porcelain", "--untracked-files=all"]);
    assert!(!porcelain.contains(".sdd/kb"), "{porcelain}");
    assert!(!root.join(".gitignore").exists(), "o .gitignore não é tocado");
}

#[tokio::test]
async fn refusals_carry_codes() {
    let (state, id, root) = project("refusals").await;
    let base = format!("/api/projects/{id}/kb");

    let (status, v) = call(&state, upload_req(&id, ".env", b"A=1")).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_secret_name");
    let (status, v) = call(&state, upload_req(&id, "deploy.pem", b"x")).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_secret_name");
    let (status, v) = call(&state, upload_req(&id, "a.md", concat!("-----", "BEGIN PRIVATE KEY-----\nabc\n").as_bytes())).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_secret_content");
    let (status, v) = call(&state, upload_req(&id, "a.docx", b"x")).await;
    assert_code(status, &v, StatusCode::UNSUPPORTED_MEDIA_TYPE, "kb_type_unsupported");
    let (status, v) = call(&state, upload_req(&id, "a.txt", b"\xff\xfe")).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_text_invalid");
    let (status, v) = call(&state, upload_req(&id, "a.md", b"")).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_file_empty");
    let (status, v) = call(&state, req(Method::POST, &format!("{base}/files"), Some(OCTET), b"x".to_vec())).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_name_invalid");
    // Corpo binário só na rota de envio e só com `application/octet-stream`.
    let (status, v) = call(&state, req(Method::POST, &format!("{base}/files?name=a.md"), Some("text/plain"), b"x".to_vec())).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{v}");
    let (status, v) = call(&state, req(Method::POST, &format!("{base}/links"), Some(OCTET), b"{}".to_vec())).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{v}");

    let (status, v) = call(&state, json_req(Method::POST, &format!("{base}/links"), json!({ "url": "file:///etc/passwd" }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_url_invalid");
    let (status, v) = call(&state, json_req(Method::POST, &format!("{base}/links"), json!({ "url": "https://u:p@x.com/" }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_url_credentials");

    let (status, v) = call(&state, upload_req(&id, "a.md", b"mesmo")).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let doc = v["doc"]["id"].as_str().expect("id").to_string();
    let (status, v) = call(&state, upload_req(&id, "b.md", b"mesmo")).await;
    assert_code(status, &v, StatusCode::CONFLICT, "kb_duplicate");
    assert_eq!(v["params"]["id"], doc.as_str());

    // O PATCH antigo, com valores que a 134 recusava, passa e não muda nada (spec 136 T02).
    for old in [json!({ "applies_to": ["src/**"] }), json!({ "role": "standard", "applies_to": ["../fora"] }), json!({ "role": "dono" })] {
        let (status, v) = call(&state, json_req(Method::PATCH, &format!("{base}/docs/{doc}"), old.clone())).await;
        assert_eq!(status, StatusCode::OK, "{old}: {v}");
    }
    let (status, v) = call(&state, json_req(Method::PATCH, &format!("{base}/docs/{doc}"), json!({ "description": "a".repeat(201) }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_description_too_long");
    assert_eq!(v["params"]["max"], 200);
    let (status, v) = call(&state, json_req(Method::PATCH, &format!("{base}/docs/{doc}"), json!({ "description": "a\nb" }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_description_invalid");
    let (status, v) = call(&state, json_req(Method::PATCH, &format!("{base}/docs/{doc}"), json!({ "dono": "x" }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields");

    let big = "a".repeat(crate::kb::MAX_OVERVIEW_BYTES + 1);
    let (status, v) = call(&state, json_req(Method::PUT, &format!("{base}/overview"), json!({ "text": big }))).await;
    assert_code(status, &v, StatusCode::PAYLOAD_TOO_LARGE, "kb_overview_too_large");

    let (status, v) = call(&state, upload_req(&id, "grande.md", &vec![b'a'; MAX_UPLOAD_BYTES + 1])).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert!(v["code"].as_str().is_some(), "{v}");

    fs::write(root.join(MANIFEST_REL), "{quebrado").expect("w");
    let (status, v) = call(&state, get(&base)).await;
    assert_code(status, &v, StatusCode::CONFLICT, "kb_manifest_invalid");
}

#[tokio::test]
async fn note_comes_out_redacted() {
    let (state, id, _root) = project("redact").await;
    let (status, v) = call(&state, upload_req(&id, "a.md", b"acesso em https://admin:senha-forte@db.local/x\n")).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let doc = v["doc"]["id"].as_str().expect("id").to_string();
    let (_, v) = call(&state, get(&format!("/api/projects/{id}/kb/docs/{doc}"))).await;
    let content = v["content"].as_str().expect("conteúdo");
    assert!(!content.contains("senha-forte"), "{content}");
}

#[tokio::test]
async fn symlinked_base_is_refused() {
    let (state, id, root) = project("symlink").await;
    let outside = root.parent().expect("pai").join("fora");
    fs::create_dir_all(&outside).expect("fora");
    std::os::unix::fs::symlink(&outside, root.join(KB_REL)).expect("symlink");
    let (status, v) = call(&state, upload_req(&id, "a.md", b"x")).await;
    assert_code(status, &v, StatusCode::CONFLICT, "project_dir_symlink");
    let (status, v) = call(&state, get(&format!("/api/projects/{id}/kb"))).await;
    assert_code(status, &v, StatusCode::CONFLICT, "project_dir_symlink");
    assert_eq!(fs::read_dir(&outside).expect("fora").count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_uploads_keep_every_document() {
    let (state, id, _root) = project("concurrent").await;
    let handles: Vec<_> = (0..12)
        .map(|i| {
            let state = state.clone();
            let id = id.clone();
            tokio::spawn(async move { call(&state, upload_req(&id, &format!("doc{i}.md"), format!("texto {i}").as_bytes())).await })
        })
        .collect();
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.expect("tarefa"));
    }
    assert!(results.iter().all(|(status, _)| *status == StatusCode::CREATED), "{results:?}");
    let (_, v) = call(&state, get(&format!("/api/projects/{id}/kb"))).await;
    assert_eq!(v["docs"].as_array().map(Vec::len), Some(12));
}

#[test]
fn upload_path_and_content_type_detection() {
    assert!(super::is_upload_path("/api/projects/abc/kb/files"));
    assert!(!super::is_upload_path("/api/projects/abc/kb/links"));
    assert!(!super::is_upload_path("/api/projects/a/b/kb/files"));
    assert!(!super::is_upload_path("/api/projects//kb/files"));
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "Application/Octet-Stream; x=1".parse().expect("valor"));
    assert!(super::is_upload_content_type(&headers));
    headers.insert(header::CONTENT_TYPE, "text/plain".parse().expect("valor"));
    assert!(!super::is_upload_content_type(&headers));
}

// --- spec 134/T02: observações da T01 e a fila pelas rotas ---

fn live_queue(state: &AppState) -> crate::server::kb_queue::KbQueue {
    crate::server::kb_queue::KbQueue::new(
        state.events().clone(),
        crate::harness::web::WebClient::for_test(&[]),
        crate::server::kb_queue::EXTRACT_TIMEOUT,
    )
}

#[tokio::test]
async fn invalid_overview_does_not_break_the_listing() {
    let (state, id, root) = project("overview-invalid").await;
    let (status, _) = call(&state, upload_req(&id, "a.md", b"texto")).await;
    assert_eq!(status, StatusCode::CREATED);
    let overview = root.join(crate::kb::OVERVIEW_REL);
    for bad in [vec![b'a'; crate::kb::MAX_OVERVIEW_BYTES + 1], vec![0xff, 0xfe, b'x']] {
        fs::write(&overview, bad).expect("visão geral editada no disco");
        let (status, v) = call(&state, get(&format!("/api/projects/{id}/kb"))).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["docs"].as_array().map(Vec::len), Some(1));
        assert_eq!((v["has_overview"].clone(), v["overview_invalid"].clone()), (json!(false), json!(true)));
    }
}

#[tokio::test]
async fn upload_route_has_its_own_ceiling_and_links_keep_the_global_one() {
    let (state, id, _root) = project("ceilings").await;
    // Acima do teto global (256 KiB) e abaixo do teto do envio: aceito.
    let big = "linha de texto comum\n".repeat(crate::server::MAX_BODY_BYTES / 20);
    assert!(big.len() > crate::server::MAX_BODY_BYTES);
    let (status, v) = call(&state, upload_req(&id, "grande.md", big.as_bytes())).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    // JSON acima do teto global em `/kb/links`: 413.
    let url = format!("https://exemplo.com/{}", "a".repeat(crate::server::MAX_BODY_BYTES));
    let (status, _) = call(&state, json_req(Method::POST, &format!("/api/projects/{id}/kb/links"), json!({ "url": url }))).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn note_is_redacted_on_disk() {
    let (state, id, root) = project("redact-disk").await;
    let (status, v) = call(&state, upload_req(&id, "a.md", b"acesso em https://admin:senha-forte@db.local/x\n")).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let doc = v["doc"]["id"].as_str().expect("id");
    let note = fs::read_to_string(root.join(crate::kb::NOTES_REL).join(format!("{doc}.md"))).expect("nota no disco");
    assert!(!note.contains("senha-forte"), "{note}");
}

#[tokio::test]
async fn io_failure_has_its_own_code() {
    use std::os::unix::fs::PermissionsExt;
    let (state, id, root) = project("io").await;
    let (status, _) = call(&state, upload_req(&id, "a.md", b"primeiro")).await;
    assert_eq!(status, StatusCode::CREATED);
    let sources = root.join(crate::kb::SOURCES_REL);
    fs::set_permissions(&sources, fs::Permissions::from_mode(0o500)).expect("só leitura");
    let (status, v) = call(&state, upload_req(&id, "b.md", b"segundo")).await;
    fs::set_permissions(&sources, fs::Permissions::from_mode(0o700)).expect("devolve");
    assert_code(status, &v, StatusCode::INTERNAL_SERVER_ERROR, "kb_io_failed");
}

#[tokio::test]
async fn uploaded_pdf_is_extracted_in_the_background_with_events() {
    let (state, id, root) = project("queue").await;
    let state = state.clone().with_kb_queue(live_queue(&state));
    let mut rx = state.events().subscribe();
    let text = "Os serviços publicam eventos de domínio com esquema versionado e documentado no catálogo da empresa.";
    let pdf = crate::kb::test_pdf::pdf(&[text]);
    let (status, v) = call(&state, upload_req(&id, "eventos.pdf", &pdf)).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["doc"]["status"], "pending");
    let doc = v["doc"]["id"].as_str().expect("id").to_string();
    let mut seen: Vec<String> = Vec::new();
    while seen.last().map(String::as_str) != Some("ready") {
        let event = tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv()).await.expect("evento").expect("canal");
        if event.event_type() == "kb_updated" && event.payload["id"] == doc.as_str() {
            assert_eq!(event.project_id.as_deref(), Some(id.as_str()));
            seen.push(event.payload["status"].as_str().expect("status").to_string());
        }
    }
    assert_eq!(seen, ["pending", "extracting", "ready"]);
    let (_, v) = call(&state, get(&format!("/api/projects/{id}/kb/docs/{doc}"))).await;
    assert_eq!(v["doc"]["pages"], 1);
    assert!(v["content"].as_str().is_some_and(|c| c.starts_with("# p. 1\n") && c.contains("eventos de domínio")), "{v}");
    assert!(root.join(crate::kb::NOTES_REL).join(format!("{doc}.md")).is_file());

    let delete = req(Method::DELETE, &format!("/api/projects/{id}/kb/docs/{doc}"), Some("application/json"), Vec::new());
    let (status, _) = call(&state, delete).await;
    assert_eq!(status, StatusCode::OK);
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await.expect("evento").expect("canal");
        if event.event_type() == "kb_updated" && event.payload["removed"] == true {
            assert_eq!(event.payload["id"], doc.as_str());
            break;
        }
    }
}

#[tokio::test]
async fn listing_requeues_documents_left_pending() {
    let (state, id, root) = project("requeue").await;
    // Sem fila (como depois de um reinício no meio): o PDF fica `pending`.
    let pdf = crate::kb::test_pdf::pdf(&["Padrões de nomenclatura das tabelas e colunas do banco de dados relacional da empresa."]);
    let (status, v) = call(&state, upload_req(&id, "nomes.pdf", &pdf)).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let doc = v["doc"]["id"].as_str().expect("id").to_string();
    let state = state.clone().with_kb_queue(live_queue(&state));
    let (_, v) = call(&state, get(&format!("/api/projects/{id}/kb"))).await;
    assert_eq!(v["docs"][0]["status"], "pending");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let entry = crate::kb::list(&root).expect("lista").into_iter().find(|d| d.id == doc).expect("doc");
        if entry.status == crate::kb::Status::Ready {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "não saiu da fila: {entry:?}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn listing_carries_coded_indicators_and_the_note_its_sections() {
    let (state, id, _root) = project("indicators").await;
    let (status, v) = call(&state, upload_req(&id, "padroes.md", b"# Padroes\n\n## Versionamento\n\nPrefixo /v2. Veja [banco](banco.md).\n")).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let padroes = v["doc"]["id"].as_str().expect("id").to_string();
    let (status, v) = call(&state, upload_req(&id, "notas.txt", b"sem titulo nenhum\n")).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let notas = v["doc"]["id"].as_str().expect("id").to_string();

    let (status, v) = call(&state, get(&format!("/api/projects/{id}/kb"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let docs = v["docs"].as_array().expect("docs");
    let find = |doc: &str| docs.iter().find(|d| d["id"] == doc).cloned().expect("documento");
    assert_eq!(find(&padroes)["indicators"], json!([{ "code": "broken_link", "params": { "target": "banco.md" } }]));
    assert_eq!(find(&notas)["indicators"], json!([{ "code": "no_headings", "params": {} }]));

    let (status, v) = call(&state, get(&format!("/api/projects/{id}/kb/docs/{padroes}"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let sections = v["sections"].as_array().expect("sections");
    assert!(
        sections.iter().any(|s| s["anchor"] == "versionamento" && s["title"] == "Versionamento" && s["level"] == 2),
        "{v}"
    );
}

#[tokio::test]
async fn section_lines_point_into_the_returned_content() {
    let (state, id, _root) = project("lines").await;
    let md = "---\ntags: [api]\n---\n# Guia\n\nIntro.\n\n## Exemplo\n\nPrimeiro.\n\n```md\n## Exemplo\n```\n\n## Exemplo\n\nSegundo.\n";
    let (status, v) = call(&state, upload_req(&id, "guia.md", md.as_bytes())).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let doc = v["doc"]["id"].as_str().expect("id").to_string();
    let (status, v) = call(&state, get(&format!("/api/projects/{id}/kb/docs/{doc}"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let content = v["content"].as_str().expect("conteúdo");
    let lines: Vec<&str> = content.lines().collect();
    let sections = v["sections"].as_array().expect("sections");
    let line_of = |anchor: &str| -> usize {
        let s = sections.iter().find(|s| s["anchor"] == anchor).unwrap_or_else(|| panic!("{anchor}: {v}"));
        usize::try_from(s["line"].as_u64().expect("line")).expect("usize")
    };
    // O título dentro da cerca de código não é seção: só há "exemplo" e "exemplo-2".
    assert!(!sections.iter().any(|s| s["anchor"] == "exemplo-3"), "{v}");
    assert_eq!(lines[line_of("guia")], "# Guia");
    let first = line_of("exemplo");
    let second = line_of("exemplo-2");
    assert_eq!(lines[first], "## Exemplo");
    assert_eq!(lines[second], "## Exemplo");
    assert_eq!(lines.get(second + 2), Some(&"Segundo."), "{content}");
    assert!(lines[first..second].contains(&"```md"), "a segunda é a de depois da cerca");
}

// --- spec 136 T03: seção, busca, links e fontes do plano pelas rotas ---

/// Configura uma conexão com a chave `secret` (o segredo entra depois do envio do documento).
async fn configure_secret(state: &AppState, secret: &str) {
    use crate::config::{Connection, WireApi};
    state.config().write().await.connections.push(Connection {
        id: "c".into(),
        name: "Conexão".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: secret.into(),
        wire_api: WireApi::default(),
        kind: Default::default(),
        base_url_override: None,
    });
}

#[tokio::test]
async fn section_route_returns_only_the_section_or_a_coded_404() {
    let (state, id, _root) = project("section").await;
    let md = "# Guia\n\nIntro.\n\n## Rotas\n\nUse /v2.\n\n### Erros\n\nLista.\n\n## Outra\n\nFim.\n";
    let (_, v) = call(&state, upload_req(&id, "guia.md", md.as_bytes())).await;
    let doc = v["doc"]["id"].as_str().expect("id").to_string();
    let base = format!("/api/projects/{id}/kb/docs/{doc}");

    let (status, v) = call(&state, get(&format!("{base}?section=rotas"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("content").is_none(), "só a seção: {v}");
    assert_eq!(v["doc"]["id"], doc.as_str());
    assert_eq!(v["section"]["anchor"], "rotas");
    assert_eq!(v["section"]["text"], "## Rotas\n\nUse /v2.\n\n### Erros\n\nLista.");
    assert_eq!(v["section"]["truncated"], false);

    let (status, v) = call(&state, get(&format!("{base}?section=sumida"))).await;
    assert_code(status, &v, StatusCode::NOT_FOUND, "kb_section_not_found");
    let (status, v) = call(&state, get(&format!("/api/projects/{id}/kb/docs/outro-0123abcd?section=rotas"))).await;
    assert_code(status, &v, StatusCode::NOT_FOUND, "kb_doc_not_found");

    // Âncora neutra: o título passou a trazer um segredo configurado.
    configure_secret(&state, "Rotas").await;
    let (_, v) = call(&state, get(&base)).await;
    let neutral = v["sections"]
        .as_array()
        .expect("seções")
        .iter()
        .find(|s| s["anchor"].as_str().is_some_and(|a| a.starts_with("s.")))
        .cloned()
        .expect("âncora neutra");
    let neutral = neutral["anchor"].as_str().expect("âncora").to_string();
    let (status, v) = call(&state, get(&format!("{base}?section={neutral}"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["section"]["anchor"], neutral.as_str());
    assert!(!v["section"].to_string().contains("Rotas"), "{v}");
}

#[tokio::test]
async fn search_route_has_ceilings_redaction_and_highlights() {
    let (state, id, _root) = project("search").await;
    let secret = concat!("sk", "-segredo-busca-0123456789");
    let (status, _) = call(&state, upload_req(&id, "a.md", format!("# Guia\n\nA ação {secret} e a ação.\n").as_bytes())).await;
    assert_eq!(status, StatusCode::CREATED);
    configure_secret(&state, secret).await;
    let base = format!("/api/projects/{id}/kb/search");

    let (status, v) = call(&state, get(&format!("{base}?q=acao&limit=999"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let hit = &v["results"][0];
    assert_eq!(hit["anchor"], "guia");
    let snippet = hit["snippet"].as_str().expect("trecho");
    assert!(!v.to_string().contains(secret), "{v}");
    let chars: Vec<char> = snippet.chars().collect();
    for m in hit["highlights"].as_array().expect("destaques") {
        let (a, b) = (m[0].as_u64().expect("a") as usize, m[1].as_u64().expect("b") as usize);
        assert_eq!(chars[a..b].iter().collect::<String>(), "ação");
    }
    assert_eq!(hit["highlights"].as_array().map(Vec::len), Some(2));

    let long = "a".repeat(super::kb_read::MAX_QUERY_CHARS + 1);
    let (status, v) = call(&state, get(&format!("{base}?q={long}"))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_query_too_long");
    let (status, v) = call(&state, get(&base)).await;
    assert_eq!((status, v), (StatusCode::OK, json!({ "results": [] })));
}

#[tokio::test]
async fn doc_route_lists_links_citations_and_plans() {
    let (state, id, root) = project("links").await;
    let (_, v) = call(&state, upload_req(&id, "banco.md", b"# Banco\n\nPostgres.\n")).await;
    let banco = v["doc"]["id"].as_str().expect("id").to_string();
    let (_, v) = call(&state, upload_req(&id, "guia.md", b"# Guia\n\nVeja o [banco](banco.md) e [fora](https://exemplo.com/x).\n")).await;
    let guia = v["doc"]["id"].as_str().expect("id").to_string();
    let spec_dir = root.join(".sdd/specs/001-api");
    fs::create_dir_all(&spec_dir).expect("spec");
    let spec = format!(
        "---\nid: \"001-api\"\ntitle: \"API\"\nstatus: \"DRAFT\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-29T00:00:00Z\"\n---\n\
## Premissas\n- kb:{banco}#banco\n"
    );
    fs::write(spec_dir.join("spec.md"), spec).expect("w");

    let (status, v) = call(&state, get(&format!("/api/projects/{id}/kb/docs/{guia}"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["links_to"]["docs"][0]["id"], banco.as_str());
    assert_eq!(v["links_to"]["outside"], json!([{ "kind": "url", "target": "https://exemplo.com/x" }]));
    assert_eq!(v["cited_by"], json!([]));
    assert_eq!(v["used_in_plans"], json!([]));

    let (status, v) = call(&state, get(&format!("/api/projects/{id}/kb/docs/{banco}"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["cited_by"][0]["id"], guia.as_str());
    assert!(v["cited_by"][0]["context"].as_str().is_some_and(|c| c.contains("[banco](banco.md)")), "{v}");
    let plan = &v["used_in_plans"][0];
    assert_eq!((plan["spec"].clone(), plan["title"].clone()), (json!("001-api"), json!("API")));
    assert_eq!(plan["uses"][0]["section"], "Premissas");
    assert_eq!(plan["uses"][0]["binding"], true);

    let (status, v) = call(&state, get(&format!("/api/projects/{id}/specs/001-api/kb-sources"))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["sources"][0]["doc_title"], "banco");
    assert_eq!(v["sources"][0]["section_title"], "Banco");
    assert_eq!(v["sources"][0]["removed"], false);
    let (status, v) = call(&state, get(&format!("/api/projects/{id}/specs/999-nada/kb-sources"))).await;
    assert_code(status, &v, StatusCode::NOT_FOUND, "spec_not_found");
    let (status, v) = call(&state, get(&format!("/api/projects/{id}/specs/a.b/kb-sources"))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "spec_id_invalid");
}

// --- spec 136 T10: documentos escritos no Nants ---

/// Próximo `kb_updated` de `doc` no barramento.
async fn next_kb_updated(rx: &mut tokio::sync::broadcast::Receiver<crate::server::events::ApiEvent>, doc: &str) -> Value {
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await.expect("evento").expect("canal");
        if event.event_type() == "kb_updated" && event.payload["id"] == doc {
            return event.payload.clone();
        }
    }
}

#[tokio::test]
async fn written_document_is_created_edited_and_searchable_with_events() {
    let (state, id, root) = project("written").await;
    let base = format!("/api/projects/{id}/kb");
    let mut rx = state.events().subscribe();

    let body = json!({ "title": "Padrões de filas", "content": "# Filas\n\nUse filas para relatórios lentos.\n" });
    let (status, v) = call(&state, json_req(Method::POST, &format!("{base}/written"), body)).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let doc = v["doc"]["id"].as_str().expect("id").to_string();
    assert!(doc.starts_with("padroes-de-filas-"), "{doc}");
    let shape = (v["doc"]["source"].clone(), v["doc"]["kind"].clone(), v["doc"]["status"].clone());
    assert_eq!(shape, (json!("written"), json!("markdown"), json!("ready")));
    assert_eq!(v["doc"]["title"], "Padrões de filas");
    let sha = v["doc"]["sha256"].as_str().expect("sha").to_string();
    let event = next_kb_updated(&mut rx, &doc).await;
    assert_eq!(event["status"], "ready");
    assert_eq!(event["doc"]["source"], "written");
    let (_, v) = call(&state, get(&format!("{base}/search?q=relatorios"))).await;
    assert_eq!(v["results"][0]["doc"], doc.as_str(), "{v}");
    let porcelain = git(&root, &["status", "--porcelain", "--untracked-files=all"]);
    assert!(!porcelain.contains(".sdd/kb"), "fora do Git: {porcelain}");

    // Edição: o texto novo entra no índice, com evento.
    let edit = json!({ "content": "# Filas\n\nUse eventos com idempotência.\n", "base_sha256": sha });
    let (status, v) = call(&state, json_req(Method::PUT, &format!("{base}/docs/{doc}/content"), edit)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let new_sha = v["doc"]["sha256"].as_str().expect("sha").to_string();
    assert_ne!(new_sha, sha);
    assert_eq!(v["doc"]["title"], "Padrões de filas");
    let event = next_kb_updated(&mut rx, &doc).await;
    assert_eq!(event["doc"]["sha256"], new_sha.as_str());
    let (_, v) = call(&state, get(&format!("{base}/search?q=idempotencia"))).await;
    assert_eq!(v["results"][0]["doc"], doc.as_str(), "{v}");
    let (_, v) = call(&state, get(&format!("{base}/search?q=relatorios"))).await;
    assert_eq!(v["results"], json!([]), "{v}");
    let (_, v) = call(&state, get(&format!("{base}/docs/{doc}"))).await;
    assert_eq!(v["content"], "# Filas\n\nUse eventos com idempotência.\n");

    // Conflito: a versão antiga dá 409 com o atual no corpo.
    let stale = json!({ "content": "# Outra\n", "base_sha256": sha });
    let (status, v) = call(&state, json_req(Method::PUT, &format!("{base}/docs/{doc}/content"), stale)).await;
    assert_code(status, &v, StatusCode::CONFLICT, "kb_edit_conflict");
    assert_eq!(v["doc"]["sha256"], new_sha.as_str());
    assert_eq!(v["content"], "# Filas\n\nUse eventos com idempotência.\n");

    // O mesmo texto de outro documento: aceito, com o indicador.
    let twin = json!({ "title": "Cópia", "content": "# Filas\n\nUse eventos com idempotência.\n" });
    let (status, v) = call(&state, json_req(Method::POST, &format!("{base}/written"), twin)).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let copy = v["doc"]["id"].as_str().expect("id").to_string();
    let (_, v) = call(&state, get(&base)).await;
    let listed = v["docs"].as_array().expect("docs").iter().find(|d| d["id"] == copy.as_str()).cloned().expect("cópia");
    assert_eq!(listed["indicators"], json!([{ "code": "duplicate", "params": { "doc": doc } }]));
}

#[tokio::test]
async fn written_routes_refuse_with_codes() {
    let (state, id, _root) = project("written-refusals").await;
    let base = format!("/api/projects/{id}/kb");
    let post = |body: Value| json_req(Method::POST, &format!("{base}/written"), body);

    let (status, v) = call(&state, post(json!({ "title": "  ", "content": "x" }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_title_empty");
    let (status, v) = call(&state, post(json!({ "title": "a".repeat(201) }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_title_too_long");
    assert_eq!(v["params"]["max"], 200);
    let (status, v) = call(&state, post(json!({ "title": "Guia" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    let guia = v["doc"]["id"].as_str().expect("id").to_string();
    let guia_sha = v["doc"]["sha256"].as_str().expect("sha").to_string();
    let (status, v) = call(&state, post(json!({ "title": "guia" }))).await;
    assert_code(status, &v, StatusCode::CONFLICT, "kb_title_taken");
    assert_eq!(v["params"]["id"], guia.as_str());
    let key = format!("{}BEGIN PRIVATE KEY{}\nabc\n", "-".repeat(5), "-".repeat(5));
    let (status, v) = call(&state, post(json!({ "title": "Chave", "content": key }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_content_secret");
    let (status, v) = call(&state, post(json!({ "title": "Nulo", "content": "a\u{0}b" }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_content_invalid");
    let big = "a".repeat(crate::kb::MAX_WRITTEN_BYTES + 1);
    let (status, v) = call(&state, post(json!({ "title": "Grande", "content": big }))).await;
    assert_code(status, &v, StatusCode::PAYLOAD_TOO_LARGE, "kb_written_too_large");
    let (status, v) = call(&state, post(json!({ "title": "X", "dono": "y" }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields");
    // Corpo que não é JSON: recusado pela regra de sempre.
    let (status, v) = call(&state, req(Method::POST, &format!("{base}/written"), Some("text/plain"), b"{}".to_vec())).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{v}");
    let (status, v) = call(&state, req(Method::PUT, &format!("{base}/docs/{guia}/content"), Some(OCTET), b"{}".to_vec())).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{v}");

    // Segredo configurado no texto da edição (montado em tempo de execução).
    let secret = ["sk", "-escrito-rota-", "0123456789"].concat();
    configure_secret(&state, &secret).await;
    let put = |doc: &str, body: Value| json_req(Method::PUT, &format!("{base}/docs/{doc}/content"), body);
    let (status, v) = call(&state, put(&guia, json!({ "content": format!("token {secret}"), "base_sha256": guia_sha }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_content_secret");
    assert!(!v.to_string().contains(&secret), "{v}");
    // O título não muda por esta rota.
    let (status, v) = call(&state, put(&guia, json!({ "content": "x", "base_sha256": guia_sha, "title": "Outro" }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields");

    // PDF e link: só leitura.
    let pdf = crate::kb::test_pdf::pdf(&["Texto qualquer do PDF de teste para a recusa da edição."]);
    let (_, v) = call(&state, upload_req(&id, "a.pdf", &pdf)).await;
    let pdf_id = v["doc"]["id"].as_str().expect("id").to_string();
    let pdf_sha = v["doc"]["sha256"].as_str().expect("sha").to_string();
    let (status, v) = call(&state, put(&pdf_id, json!({ "content": "x", "base_sha256": pdf_sha }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_not_editable");
    let (_, v) = call(&state, json_req(Method::POST, &format!("{base}/links"), json!({ "url": "https://exemplo.com/guia" }))).await;
    let link = v["doc"]["id"].as_str().expect("id").to_string();
    let (status, v) = call(&state, put(&link, json!({ "content": "x", "base_sha256": "" }))).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_not_editable");
    let (status, v) = call(&state, put("nada-0123abcd", json!({ "content": "x", "base_sha256": "" }))).await;
    assert_code(status, &v, StatusCode::NOT_FOUND, "kb_doc_not_found");

    // O enviado em markdown é editável, e os links são recalculados.
    let (_, v) = call(&state, upload_req(&id, "banco.md", b"# Banco\n\nPostgres.\n")).await;
    let banco = v["doc"]["id"].as_str().expect("id").to_string();
    let (_, v) = call(&state, upload_req(&id, "deploy.md", b"# Deploy\n\nSem links.\n")).await;
    let deploy = v["doc"]["id"].as_str().expect("id").to_string();
    let deploy_sha = v["doc"]["sha256"].as_str().expect("sha").to_string();
    let linked = json!({ "content": "# Deploy\n\nVeja o [banco](banco.md).\n", "base_sha256": deploy_sha });
    let (status, v) = call(&state, put(&deploy, linked)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["doc"]["links_out"], json!([banco]));
    assert_eq!(v["doc"]["source"], "file");
}

#[tokio::test]
async fn editor_reads_the_original_text_with_its_version() {
    let (state, id, _root) = project("written-source").await;
    let base = format!("/api/projects/{id}/kb");
    let source = |doc: &str| get(&format!("{base}/docs/{doc}/content"));

    // O enviado: o original inteiro, com o frontmatter que a nota não mostra, e a versão do PUT.
    let original = "---\nautor: equipe\n---\n# Banco\n\nPostgres.   \n";
    let (_, v) = call(&state, upload_req(&id, "banco.md", original.as_bytes())).await;
    let banco = v["doc"]["id"].as_str().expect("id").to_string();
    let sha = v["doc"]["sha256"].as_str().expect("sha").to_string();
    let (status, v) = call(&state, source(&banco)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({ "content": original, "sha256": sha }));
    let edit = json!({ "content": "# Banco\n\nPostgres 16.\n", "base_sha256": v["sha256"] });
    let (status, v) = call(&state, json_req(Method::PUT, &format!("{base}/docs/{banco}/content"), edit)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, v) = call(&state, source(&banco)).await;
    assert_eq!(v["content"], "# Banco\n\nPostgres 16.\n");
    assert_eq!(v["sha256"], crate::kb::sha256_hex(b"# Banco\n\nPostgres 16.\n").as_str());

    // O escrito: o texto como foi escrito.
    let (_, v) = call(&state, json_req(Method::POST, &format!("{base}/written"), json!({ "title": "Filas", "content": "# Filas\n" }))).await;
    let filas = v["doc"]["id"].as_str().expect("id").to_string();
    let (_, v) = call(&state, source(&filas)).await;
    assert_eq!(v["content"], "# Filas\n");

    // PDF, link e inexistente.
    let pdf = crate::kb::test_pdf::pdf(&["Texto qualquer do PDF de teste para a leitura do original."]);
    let (_, v) = call(&state, upload_req(&id, "a.pdf", &pdf)).await;
    let pdf_id = v["doc"]["id"].as_str().expect("id").to_string();
    let (status, v) = call(&state, source(&pdf_id)).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_not_editable");
    let (_, v) = call(&state, json_req(Method::POST, &format!("{base}/links"), json!({ "url": "https://exemplo.com/fonte" }))).await;
    let link = v["doc"]["id"].as_str().expect("id").to_string();
    let (status, v) = call(&state, source(&link)).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_not_editable");
    let (status, v) = call(&state, source("nada-0123abcd")).await;
    assert_code(status, &v, StatusCode::NOT_FOUND, "kb_doc_not_found");

    // Acima do teto da escrita: a gravação recusaria, então a leitura também.
    let big = format!("# Grande\n\n{}\n", "a".repeat(crate::kb::MAX_WRITTEN_BYTES));
    let (_, v) = call(&state, upload_req(&id, "grande.txt", big.as_bytes())).await;
    let grande = v["doc"]["id"].as_str().expect("id").to_string();
    let (status, v) = call(&state, source(&grande)).await;
    assert_code(status, &v, StatusCode::PAYLOAD_TOO_LARGE, "kb_written_too_large");

    // Segredo configurado depois do envio: recusa com código, nunca o texto redigido.
    let secret = ["sk", "-fonte-depois-", "0123456789"].concat();
    let (_, v) = call(&state, upload_req(&id, "chave.md", format!("# Chave\n\ntoken {secret}\n").as_bytes())).await;
    let chave = v["doc"]["id"].as_str().expect("id").to_string();
    configure_secret(&state, &secret).await;
    let (status, v) = call(&state, source(&chave)).await;
    assert_code(status, &v, StatusCode::UNPROCESSABLE_ENTITY, "kb_source_redacted");
    assert!(!v.to_string().contains(&secret), "{v}");
}

#[tokio::test]
async fn written_routes_never_follow_symlinks() {
    let (state, id, root) = project("written-symlink").await;
    let outside = root.parent().expect("pai").join("fora");
    fs::create_dir_all(&outside).expect("fora");
    std::os::unix::fs::symlink(&outside, root.join(KB_REL)).expect("symlink");
    let create = json!({ "title": "Doc", "content": "x" });
    let (status, v) = call(&state, json_req(Method::POST, &format!("/api/projects/{id}/kb/written"), create)).await;
    assert_code(status, &v, StatusCode::CONFLICT, "project_dir_symlink");
    let edit = json!({ "content": "x", "base_sha256": "" });
    let (status, v) = call(&state, json_req(Method::PUT, &format!("/api/projects/{id}/kb/docs/doc-0123abcd/content"), edit)).await;
    assert_code(status, &v, StatusCode::CONFLICT, "project_dir_symlink");
    assert_eq!(fs::read_dir(&outside).expect("fora").count(), 0);
}
