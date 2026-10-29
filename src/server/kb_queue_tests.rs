//! Testes da fila de extração (spec 134/T02): estados e eventos, PDF, link por um servidor local,
//! o arquivo patológico (tempo máximo) e a ordem da fila. Nenhum acesso à rede de fora.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::broadcast;

use super::*;
use crate::harness::web::tests::{ok, serve};
use crate::kb::{test_pdf, Status, NOTES_REL, SOURCES_REL};

const NOW: &str = "2026-10-28T12:00:00Z";
const TEXT: &str = "A política de segurança exige revisão de código, testes automatizados e registro das decisões \
    de arquitetura em cada mudança relevante do sistema.";

fn root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nants-kbq-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let root = dir.join("repo");
    fs::create_dir_all(&root).expect("mkdir");
    root
}

fn secrets() -> Arc<SecretSet> {
    Arc::new(SecretSet::new(Vec::<String>::new()))
}

fn queue(bus: &EventBus, timeout: Duration) -> KbQueue {
    KbQueue::new(bus.clone(), WebClient::for_test(&[]), timeout)
}

/// Espera o estado final de `id` e devolve os estados vistos, na ordem.
async fn statuses(rx: &mut broadcast::Receiver<ApiEvent>, id: &str) -> (Vec<String>, Value) {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = tokio::time::timeout(left, rx.recv()).await.expect("evento a tempo").expect("canal");
        if event.event_type() != "kb_updated" || event.payload["id"] != id {
            continue;
        }
        assert_eq!(event.project_id.as_deref(), Some("p1"));
        let status = event.payload["status"].as_str().expect("status").to_string();
        seen.push(status.clone());
        if !matches!(status.as_str(), "pending" | "extracting") {
            return (seen, event.payload.clone());
        }
    }
}

fn note(root: &Path, id: &str) -> String {
    fs::read_to_string(root.join(NOTES_REL).join(format!("{id}.md"))).expect("nota")
}

#[tokio::test]
async fn pdf_goes_through_extracting_to_ready() {
    let root = root("pdf");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let q = queue(&bus, EXTRACT_TIMEOUT);
    let bytes = test_pdf::pdf(&[TEXT, "Segunda página com o glossário de termos usados nos padrões da empresa."]);
    let entry = kb::add_file(&root, "padroes.pdf", &bytes, &secrets(), NOW).expect("add");
    assert_eq!(entry.status, Status::Pending);
    assert!(q.enqueue("p1", root.clone(), &entry.id, secrets()));
    assert!(!q.enqueue("p1", root.clone(), &entry.id, secrets()), "repetido não entra");
    let (seen, payload) = statuses(&mut rx, &entry.id).await;
    assert_eq!(seen, ["extracting", "ready"]);
    assert_eq!(payload["doc"]["pages"], 2);
    assert_eq!(payload["doc"]["extractor"], extract::PDF_EXTRACTOR);
    assert!(payload["doc"]["description"].as_str().is_some_and(|d| d.starts_with("A política")), "{payload}");
    let text = note(&root, &entry.id);
    assert!(text.contains("status: \"ready\"") && text.contains("pages: 2"), "{text}");
    assert!(text.contains("# p. 1\n") && text.contains("# p. 2\n"), "{text}");
}

#[tokio::test]
async fn scanned_and_broken_pdfs_get_coded_states() {
    let root = root("states");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let q = queue(&bus, EXTRACT_TIMEOUT);
    let scanned = kb::add_file(&root, "scan.pdf", &test_pdf::pdf(&["", ""]), &secrets(), NOW).expect("scan");
    let broken = kb::add_file(&root, "quebrado.pdf", b"%PDF-1.7\nsem nada", &secrets(), NOW).expect("broken");
    q.enqueue("p1", root.clone(), &scanned.id, secrets());
    q.enqueue("p1", root.clone(), &broken.id, secrets());
    let (_, payload) = statuses(&mut rx, &scanned.id).await;
    assert_eq!((payload["status"].as_str(), payload["status_reason"].as_str()), (Some("no_text"), Some("kb_pdf_no_text")));
    let (_, payload) = statuses(&mut rx, &broken.id).await;
    assert_eq!((payload["status"].as_str(), payload["status_reason"].as_str()), (Some("error"), Some("kb_pdf_unreadable")));
    let docs = kb::list(&root).expect("lista");
    assert_eq!(docs[1].status_reason.as_deref(), Some("kb_pdf_unreadable"));
}

#[tokio::test]
async fn pathological_file_times_out_and_the_queue_moves_on() {
    let root = root("timeout");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    // Arquivo patológico real: uma árvore de páginas plana faz a leitura crescer mais que
    // linearmente (medido no unpdf: 300 páginas 0,8 s, 1000 páginas 8,8 s em debug; 5000 páginas
    // 30 s em release). Prazo curto: o grande passa dele; o pequeno, que vem depois, cabe.
    let q = queue(&bus, Duration::from_secs(1));
    let pages: Vec<String> = (0..600).map(|i| format!("{TEXT} Página {i}.")).collect();
    let refs: Vec<&str> = pages.iter().map(String::as_str).collect();
    let big = kb::add_file(&root, "enorme.pdf", &test_pdf::pdf(&refs), &secrets(), NOW).expect("big");
    let small = kb::add_file(&root, "pequeno.pdf", &test_pdf::pdf(&[TEXT]), &secrets(), NOW).expect("small");
    q.enqueue("p1", root.clone(), &big.id, secrets());
    q.enqueue("p1", root.clone(), &small.id, secrets());
    let (seen, payload) = statuses(&mut rx, &big.id).await;
    assert_eq!(seen, ["extracting", "error"]);
    assert_eq!(payload["status_reason"], "kb_extract_timeout");
    let (seen, _) = statuses(&mut rx, &small.id).await;
    assert_eq!(seen, ["extracting", "ready"], "a fila segue depois do tempo esgotado");
    assert!(note(&root, &big.id).contains("status: \"error\""));
    assert_eq!(q.peak_workers(), 1, "o trabalho abandonado termina antes do próximo começar");
}

#[tokio::test]
async fn run_limited_reports_timeout_and_panic() {
    let soon = Instant::now() + Duration::from_millis(30);
    let (slow, leftover) = run_limited(soon, || {
        std::thread::sleep(Duration::from_millis(400));
        Ok(1)
    })
    .await;
    assert_eq!(slow, Err(extract::reason::TIMEOUT));
    // O trabalho abandonado continua e termina: quem chama pode esperar por ele.
    let rest = leftover.expect("trabalho em andamento");
    assert_eq!(rest.await.expect("termina"), Ok(1));
    let later = Instant::now() + Duration::from_secs(5);
    let (boom, leftover): (Result<u8, _>, _) = run_limited(later, || panic!("biblioteca quebrou")).await;
    assert_eq!(boom, Err(extract::reason::FAILED));
    assert!(leftover.is_none());
    let (done, leftover) = run_limited(later, || Ok(7)).await;
    assert_eq!(done, Ok(7));
    assert!(leftover.is_none());
}

#[tokio::test]
async fn link_is_read_once_through_the_web_client() {
    let root = root("link");
    let html = format!(
        "<html><head><title>Guia de APIs</title><meta name=\"description\" content=\"Como desenhar APIs.\"></head>\
         <body><nav>Menu principal</nav><article><h2>Versões</h2><p>{TEXT}</p><a href=\"/outro\">outro</a></article></body></html>"
    );
    let base = serve(vec![
        ok("/guia", "text/html; charset=utf-8", &html),
        ok("/outro", "text/plain", TEXT),
        ok("/spa", "text/html", "<html><body><div id=\"app\"></div><script>boot()</script></body></html>"),
        ("/sumiu", 404, "text/plain", String::new(), Vec::new()),
        ("/interna", 302, "text/plain", "http://10.0.0.1/admin".to_string(), Vec::new()),
    ])
    .await;
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let q = queue(&bus, EXTRACT_TIMEOUT);
    let other = kb::add_link(&root, &format!("{base}/outro"), &secrets(), NOW).expect("outro");
    let guide = kb::add_link(&root, &format!("{base}/guia"), &secrets(), NOW).expect("guia");
    let spa = kb::add_link(&root, &format!("{base}/spa"), &secrets(), NOW).expect("spa");
    let gone = kb::add_link(&root, &format!("{base}/sumiu"), &secrets(), NOW).expect("sumiu");
    let private = kb::add_link(&root, &format!("{base}/interna"), &secrets(), NOW).expect("interna");
    for doc in [&other, &guide, &spa, &gone, &private] {
        q.enqueue("p1", root.clone(), &doc.id, secrets());
    }
    let (_, payload) = statuses(&mut rx, &other.id).await;
    assert_eq!(payload["status"], "ready");
    let (seen, payload) = statuses(&mut rx, &guide.id).await;
    assert_eq!(seen, ["extracting", "ready"]);
    let doc = &payload["doc"];
    assert_eq!(doc["title"], "Guia de APIs");
    assert_eq!(doc["description"], "Como desenhar APIs.");
    assert_eq!(doc["source_url"], format!("{base}/guia"));
    assert!(doc["fetched_at"].as_str().is_some_and(|t| !t.is_empty()));
    assert_eq!(doc["links_out"], serde_json::json!([other.id]), "link entre documentos da base (C-22)");
    assert_eq!(doc["size"], html.len());
    let text = note(&root, &guide.id);
    assert!(text.contains("## Versões") && !text.contains("Menu principal"), "{text}");
    assert!(text.contains(&format!("source_url: \"{base}/guia\"")) && !text.contains("fetched_at: null"), "{text}");
    assert_eq!(fs::read(root.join(SOURCES_REL).join(format!("{}.html", guide.id))).expect("original"), html.as_bytes());

    let (_, payload) = statuses(&mut rx, &spa.id).await;
    assert_eq!((payload["status"].as_str(), payload["status_reason"].as_str()), (Some("no_text"), Some("kb_link_no_text")));
    let (_, payload) = statuses(&mut rx, &gone.id).await;
    assert_eq!((payload["status"].as_str(), payload["status_reason"].as_str()), (Some("error"), Some("kb_link_http_error")));
    let (_, payload) = statuses(&mut rx, &private.id).await;
    assert_eq!((payload["status"].as_str(), payload["status_reason"].as_str()), (Some("error"), Some("kb_link_blocked")));
}

#[tokio::test]
async fn removed_during_extraction_stays_removed_and_idle_queue_does_nothing() {
    let root = root("removed");
    let entry = kb::add_file(&root, "a.pdf", &test_pdf::pdf(&[TEXT]), &secrets(), NOW).expect("add");
    let begun = kb::begin_extraction(&root, &entry.id).expect("begin").expect("na fila");
    assert_eq!(begun.status, Status::Extracting);
    kb::remove(&root, &entry.id).expect("remove");
    let outcome = Ok((extract::text(TEXT, false), None));
    let done = kb::finish_extraction(&root, &entry.id, &begun.added_at, outcome, &secrets(), NOW).expect("finish");
    assert!(done.is_none());
    assert!(kb::list(&root).expect("lista").is_empty());
    assert!(!root.join(NOTES_REL).join(format!("{}.md", entry.id)).exists());

    assert!(!KbQueue::idle().enqueue("p1", root.clone(), "x", secrets()));
}
