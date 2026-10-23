//! Testes do diálogo persistido (`dialog.jsonl`), do evento `agent_message` e do GET paginado.
//! Incluídos por `dialog.rs`.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{env, process};

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::agents::runner::{EventSink, RunnerEvent};
use crate::config::{Config, Project};
use crate::server::events::{BusSink, EventBus, EventType, SecretSet};
use crate::server::projects::ensure_local_excludes;
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";
const SPEC: &str = "001-demo";
const PROJECT: &str = "proj-dialog";
const SECRET: &str = "sk-SEGREDO-DIALOGO-98765";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-dialog-{}-{}", process::id(), tag));
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

fn dialog_file(root: &Path) -> PathBuf {
    root.join(".sdd/specs").join(SPEC).join("dialog.jsonl")
}

/// Espera a gravação em fila deste projeto terminar (a publicação já ocorreu).
fn settle(root: &Path) {
    flush_project(root, Duration::from_secs(5));
}

fn lines(root: &Path) -> Vec<Value> {
    settle(root);
    let text = fs::read_to_string(dialog_file(root)).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("seq").is_some())
        .collect()
}

fn agent_msg(text: &str) -> RunnerEvent {
    RunnerEvent::AgentMessage {
        agent: "developer".into(),
        text: text.into(),
        task_id: Some("T01".into()),
        attempt: Some(2),
    }
}

fn sink(bus: &EventBus, root: &Path, secrets: SecretSet) -> BusSink {
    BusSink::new(bus.clone(), PROJECT, Arc::new(secrets)).with_dialog(DialogTarget::new(root, SPEC))
}

/// Próximo evento do barramento (espera assíncrona, pois a publicação é ordenada pelo escritor).
async fn next_event(rx: &mut tokio::sync::broadcast::Receiver<ApiEvent>) -> ApiEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("tempo esgotado")
        .expect("evento")
}

fn config_for(root: &Path) -> Config {
    let mut cfg = Config::default();
    cfg.projects.push(Project {
        id: PROJECT.into(),
        name: "Diálogo".into(),
        path: root.to_str().expect("utf8").into(),
        created_at: "2026-10-14T00:00:00Z".into(),
    });
    cfg
}

fn state_for(root: &Path) -> AppState {
    AppState::new(root.join("config.toml"), config_for(root), 3000)
}

async fn get(state: &AppState, uri: &str, host: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::HOST, host)
        .body(Body::empty())
        .expect("requisição");
    let resp = router(state.clone()).oneshot(req).await.expect("resposta");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.expect("corpo");
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn uri(project: &str, spec: &str, query: &str) -> String {
    format!("/api/projects/{project}/specs/{spec}/dialog{query}")
}

/// Escreve linhas válidas `seq` de `first` a `last` diretamente no arquivo.
fn write_lines(root: &Path, first: u64, last: u64, text: &str) {
    let mut out = String::new();
    for seq in first..=last {
        let line = json!({
            "seq": seq, "ts": 1, "agent": "developer", "task_id": "T01",
            "attempt": 1, "text": text,
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    fs::create_dir_all(dialog_file(root).parent().expect("pai")).expect("dir");
    fs::write(dialog_file(root), out).expect("escrita");
}

#[tokio::test]
async fn appends_lines_with_increasing_seq_and_event_carries_the_same_seq() {
    let root = setup("seq");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let s = sink(&bus, &root, SecretSet::default());
    s.emit(agent_msg("primeira"));
    s.emit(agent_msg("segunda"));
    let e1 = next_event(&mut rx).await;
    let e2 = next_event(&mut rx).await;
    assert!(e1.payload["seq"].as_u64().is_some());
    assert!(e2.payload["seq"].as_u64() > e1.payload["seq"].as_u64(), "seq crescente");
    assert_eq!(e1.payload["task_id"], "T01");
    assert_eq!(e1.payload["attempt"], 2);
    // Evento com destino traz a spec do destino; é a mesma spec da linha gravada.
    assert_eq!(e1.payload["spec_id"], SPEC);
    let file = lines(&root);
    assert_eq!(file.len(), 2);
    assert_eq!(file[1]["seq"], e2.payload["seq"]);
    assert_eq!(file[1]["text"], "segunda");
    assert_eq!(file[1]["agent"], "developer");
    assert_eq!(file[1]["task_id"], "T01");
    assert_eq!(file[1]["attempt"], 2);
    assert!(file[1]["ts"].as_u64().is_some());
}

#[tokio::test]
async fn seq_continues_after_restart_from_last_line_of_file() {
    let root = setup("restart");
    write_lines(&root, 7, 8, "antes do reinício");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("depois"));
    let ev = next_event(&mut rx).await;
    // Sem ler o arquivo: a base é o relógio (µs), então o seq novo passa o do arquivo.
    assert!(ev.payload["seq"].as_u64().expect("seq") > 8);
    assert_eq!(lines(&root).last().expect("linha")["seq"], ev.payload["seq"]);
}

#[tokio::test]
async fn seq_after_restart_is_greater_than_file_seq_and_keeps_growing() {
    let root = setup("restart-monotonic");
    write_lines(&root, 5, 5, "antes");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let s = sink(&bus, &root, SecretSet::default());
    s.emit(agent_msg("a"));
    s.emit(agent_msg("b"));
    let a = next_event(&mut rx).await.payload["seq"].as_u64().expect("seq");
    let b = next_event(&mut rx).await.payload["seq"].as_u64().expect("seq");
    assert!(a > 5 && b > a, "{a} {b}");
}

#[tokio::test]
async fn text_is_redacted_before_writing_and_controls_become_spaces() {
    let root = setup("redact");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let secrets = SecretSet::new([SECRET.to_string()]);
    sink(&bus, &root, secrets).emit(agent_msg(&format!("chave {SECRET} e\u{7}bip")));
    let ev = next_event(&mut rx).await;
    assert_eq!(ev.payload["text"], "chave *** e bip");
    settle(&root);
    let raw = fs::read_to_string(dialog_file(&root)).expect("arquivo");
    assert!(!raw.contains(SECRET), "{raw}");
    assert_eq!(lines(&root)[0]["text"], "chave *** e bip");
}

/// Próximo evento ao vivo e a página do GET devolvem o mesmo texto da mesma mensagem.
async fn live_and_get_text(root: &Path, text: &str, secrets: SecretSet) -> (String, String) {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let state = AppState::new(root.join("config.toml"), config_for(root), 3000);
    sink(&bus, root, secrets).emit(agent_msg(text));
    let live = next_event(&mut rx).await.payload["text"]
        .as_str()
        .expect("texto")
        .to_string();
    settle(root);
    let (_, body) = get(&state, &uri(PROJECT, SPEC, ""), HOST).await;
    let stored = body["messages"][0]["text"].as_str().expect("texto").to_string();
    (live, stored)
}

#[tokio::test]
async fn newlines_tabs_and_crlf_survive_round_trip_and_match_live() {
    let root = setup("newlines");
    let text = "linha 1\r\nlinha 2\n\tindentado\r\n```rust\nfn x() {}\n```";
    let expected = "linha 1\nlinha 2\n\tindentado\n```rust\nfn x() {}\n```";
    let (live, stored) = live_and_get_text(&root, text, SecretSet::default()).await;
    assert_eq!(live, expected);
    assert_eq!(stored, expected);
    let raw = fs::read_to_string(dialog_file(&root)).expect("arquivo");
    assert_eq!(raw.lines().count(), 1, "uma linha JSONL por mensagem: {raw}");
    assert_eq!(lines(&root)[0]["text"], expected);
}

#[tokio::test]
async fn lone_cr_becomes_newline() {
    let root = setup("lone-cr");
    let (live, stored) = live_and_get_text(&root, "a\rb\r\rc", SecretSet::default()).await;
    assert_eq!(live, "a\nb\n\nc");
    assert_eq!(stored, "a\nb\n\nc");
}

#[tokio::test]
async fn bidi_formatting_and_other_controls_become_spaces_live_and_stored() {
    let root = setup("bidi");
    let text = "a\u{202E}b\u{2066}c\u{200B}d\u{FEFF}e\u{7}f\u{85}g\u{7f}h\u{1b}i";
    let (live, stored) = live_and_get_text(&root, text, SecretSet::default()).await;
    assert_eq!(live, "a b c d e f g h i");
    assert_eq!(stored, live);
    assert_eq!(lines(&root)[0]["text"], live);
}

#[tokio::test]
async fn more_than_400_lines_is_cut_with_ellipsis() {
    let root = setup("many-lines");
    let text: String = (0..450).map(|i| format!("l{i}\n")).collect();
    let (live, stored) = live_and_get_text(&root, &text, SecretSet::default()).await;
    assert_eq!(live, stored);
    let kept: Vec<&str> = live.split('\n').collect();
    assert_eq!(kept.len(), 401, "400 linhas e a marca de excedente");
    assert_eq!(kept[0], "l0");
    assert_eq!(kept[399], "l399");
    assert_eq!(kept[400], "…");
}

#[tokio::test]
async fn persisted_line_stays_within_8_kib_even_with_escapes() {
    let root = setup("line-size");
    let text = "\"".repeat(8000);
    let (live, stored) = live_and_get_text(&root, &text, SecretSet::default()).await;
    assert_eq!(live, stored);
    let raw = fs::read_to_string(dialog_file(&root)).expect("arquivo");
    assert_eq!(raw.lines().count(), 1);
    assert!(raw.len() <= 8 * 1024, "{}", raw.len());
    assert!(serde_json::from_str::<Value>(raw.trim_end()).is_ok());
}

#[tokio::test]
async fn exceeding_two_mib_keeps_most_recent_lines_and_seq_continues() {
    let root = setup("rotate");
    let big = "x".repeat(4000);
    let total: u64 = 600;
    write_lines(&root, 1, total, &big);
    assert!(fs::metadata(dialog_file(&root)).expect("meta").len() > DIALOG_MAX_BYTES);
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("mais recente"));
    let ev = next_event(&mut rx).await;
    assert!(ev.payload["seq"].as_u64().expect("seq") > total);
    settle(&root);
    let len = fs::metadata(dialog_file(&root)).expect("meta").len();
    assert!(len <= DIALOG_MAX_BYTES, "{len}");
    let file = lines(&root);
    assert!(file[0]["seq"].as_u64().expect("seq") > 1, "início não foi truncado");
    let last = file.last().expect("última");
    assert_eq!(last["seq"], ev.payload["seq"]);
    assert_eq!(last["text"], "mais recente");
    // Sequência estritamente crescente no que restou.
    let seqs: Vec<u64> = file.iter().map(|l| l["seq"].as_u64().expect("seq")).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]));
}

#[tokio::test]
async fn write_failure_never_blocks_or_drops_the_event() {
    let root = setup("fail");
    let outside = temp_dir("fail-out");
    // Diretório da spec é symlink: a escrita é recusada sem seguir o link.
    fs::remove_dir_all(root.join(".sdd/specs").join(SPEC)).expect("rm");
    symlink(&outside, root.join(".sdd/specs").join(SPEC)).expect("ln");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("sem gravação"));
    let ev = next_event(&mut rx).await;
    assert_eq!(ev.event_type(), "agent_message");
    assert_eq!(ev.payload["text"], "sem gravação");
    settle(&root);
    assert!(!outside.join("dialog.jsonl").exists());
}

#[tokio::test]
async fn events_without_dialog_target_are_published_unchanged_and_not_persisted() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let s = BusSink::new(bus.clone(), PROJECT, Arc::new(SecretSet::default()));
    s.emit(agent_msg("só ao vivo"));
    let got = rx.try_recv().expect("publicado de imediato");
    assert_eq!(got.payload["task_id"], "T01");
    assert!(got.payload["seq"].is_null());
    // Sem destino a chave existe e é nula (não ausente): o cliente não deduz a spec.
    assert_eq!(got.payload.get("spec_id"), Some(&Value::Null));
}

#[tokio::test]
async fn system_messages_are_persisted_with_null_task_and_system_agent() {
    let root = setup("system");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let target = DialogTarget::new(&root, SPEC);
    let ev = ApiEvent::new(
        EventType::AgentMessage,
        Some(PROJECT.into()),
        json!({"agent": "system", "text": "falha do provedor", "task_id": null, "attempt": null, "seq": null}),
    );
    deliver(&bus, Some(&target), ev);
    let got = next_event(&mut rx).await;
    assert!(got.payload["seq"].as_u64().is_some());
    let line = &lines(&root)[0];
    assert_eq!(line["seq"], got.payload["seq"]);
    assert_eq!(line["agent"], "system");
    assert!(line["task_id"].is_null());
    assert!(line["attempt"].is_null());
    assert_eq!(line["text"], "falha do provedor");
}

#[tokio::test]
async fn get_pages_with_after_and_limit_in_ascending_order() {
    let root = setup("page");
    write_lines(&root, 1, 5, "msg");
    let state = state_for(&root);
    let (st, body) = get(&state, &uri(PROJECT, SPEC, "?after=0&limit=2"), HOST).await;
    assert_eq!(st, StatusCode::OK);
    let seqs: Vec<u64> = body["messages"].as_array().expect("lista").iter().map(|m| m["seq"].as_u64().expect("seq")).collect();
    assert_eq!(seqs, vec![1, 2]);
    assert_eq!(body["last_seq"], 5);
    assert_eq!(body["truncated_before"], false);
    let (_, body) = get(&state, &uri(PROJECT, SPEC, "?after=2&limit=500"), HOST).await;
    let seqs: Vec<u64> = body["messages"].as_array().expect("lista").iter().map(|m| m["seq"].as_u64().expect("seq")).collect();
    assert_eq!(seqs, vec![3, 4, 5]);
    let first = &body["messages"][0];
    assert_eq!(first["agent"], "developer");
    assert_eq!(first["task_id"], "T01");
    assert_eq!(first["attempt"], 1);
    assert!(first["ts"].as_u64().is_some());
}

#[tokio::test]
async fn get_defaults_and_missing_file_is_empty() {
    let root = setup("missing");
    let state = state_for(&root);
    let (st, body) = get(&state, &uri(PROJECT, SPEC, ""), HOST).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body, json!({"messages": [], "last_seq": 0, "truncated_before": false}));
}

#[tokio::test]
async fn get_reports_truncated_before_only_after_rotation_marker() {
    // F-08: `truncated_before` vem do estado da janela (marcador de rotação), não dos números.
    let root = setup("trunc-get");
    let body_text: String = (10..=12).map(|s| line_of_len(s, 120)).collect();
    write_lines_raw(&root, &format!("{ROTATED_MARKER}\n{body_text}"));
    let state = state_for(&root);
    let (_, body) = get(&state, &uri(PROJECT, SPEC, "?after=0"), HOST).await;
    assert_eq!(body["truncated_before"], true);
    assert_eq!(body["messages"].as_array().expect("lista").len(), 3);
    let (_, body) = get(&state, &uri(PROJECT, SPEC, "?after=10"), HOST).await;
    assert_eq!(body["truncated_before"], false, "after dentro da janela");
}

#[tokio::test]
async fn three_real_seq_messages_read_from_zero_are_not_truncated() {
    // F-08: seq em µs não faz `after=0` parecer truncado.
    let root = setup("no-trunc");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let s = sink(&bus, &root, SecretSet::default());
    for i in 0..3 {
        s.emit(agent_msg(&format!("m{i}")));
    }
    for _ in 0..3 {
        next_event(&mut rx).await;
    }
    settle(&root);
    let state = state_for(&root);
    let (_, body) = get(&state, &uri(PROJECT, SPEC, "?after=0"), HOST).await;
    assert_eq!(body["messages"].as_array().expect("lista").len(), 3);
    assert_eq!(body["truncated_before"], false);
}

#[tokio::test]
async fn rotated_file_reports_truncation_only_for_cursors_before_the_window() {
    // F-08: após rotação, `after` anterior à janela é truncado; dentro dela, não.
    let root = setup("rotated-cursor");
    let body_text: String = (20..=22).map(|s| line_of_len(s, 120)).collect();
    write_lines_raw(&root, &format!("{ROTATED_MARKER}\n{body_text}"));
    let state = state_for(&root);
    let (_, body) = get(&state, &uri(PROJECT, SPEC, "?after=5"), HOST).await;
    assert_eq!(body["truncated_before"], true);
    let (_, body) = get(&state, &uri(PROJECT, SPEC, "?after=20"), HOST).await;
    assert_eq!(body["truncated_before"], false);
}

#[tokio::test]
async fn rotation_writes_marker_and_keeps_it_single() {
    // F-08: a rotação grava o marcador no topo; rotações seguintes não o duplicam.
    let root = setup("marker");
    write_lines(&root, 1, 600, &"x".repeat(4000));
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("gatilho"));
    next_event(&mut rx).await;
    settle(&root);
    let raw = fs::read_to_string(dialog_file(&root)).expect("arquivo");
    assert!(raw.starts_with(ROTATED_MARKER), "marcador no topo");
    assert_eq!(raw.matches(ROTATED_MARKER).count(), 1);
}

#[tokio::test]
async fn seed_seq_from_file_keeps_new_seq_above_recorded_ones() {
    // F-09: após reinício (registro vazio), o seq novo passa o maior gravado.
    let root = setup("seed");
    let far = now_micros() + 1_000_000_000_000;
    write_lines_raw(&root, &format!("{}\n", line_of_len(far, 200)));
    seed_seq(&root, SPEC);
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("depois do reinício"));
    let ev = next_event(&mut rx).await;
    assert!(ev.payload["seq"].as_u64().expect("seq") > far);
}

#[tokio::test]
async fn forget_project_releases_writer_and_seqs_and_thread_exits() {
    // F-10: forget_project libera fila, seqs e encerra a thread de gravação.
    let root = setup("forget");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("antes"));
    next_event(&mut rx).await;
    settle(&root);
    let handle = forget_project(&root).expect("thread do projeto");
    assert!(!registry_has(&root), "fila e seqs liberados");
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = handle.join();
        let _ = done_tx.send(());
    });
    assert!(done_rx.recv_timeout(Duration::from_secs(5)).is_ok(), "thread encerrou");
}

#[tokio::test]
async fn reimport_after_forget_writes_again() {
    // F-10: remover e reimportar o mesmo caminho volta a gravar.
    let root = setup("reimport");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("um"));
    next_event(&mut rx).await;
    let _ = forget_project(&root);
    // Reimportação: create_project readmite o caminho antes de qualquer gravação.
    revive_project(&root);
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("dois"));
    next_event(&mut rx).await;
    let file = lines(&root);
    assert!(file.iter().any(|l| l["text"] == "dois"), "{file:?}");
}

#[tokio::test]
async fn late_delivery_after_forget_creates_nothing_and_publishes_without_seq() {
    // F-11: entrega tardia de um projeto removido não recria fila, thread nem seq.
    let root = setup("tombstone");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("antes"));
    next_event(&mut rx).await;
    settle(&root);
    let _ = forget_project(&root);
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("tardia"));
    let ev = next_event(&mut rx).await;
    assert_eq!(ev.payload["text"], "tardia", "publicação mantida");
    assert!(ev.payload["seq"].is_null(), "sem seq após remoção");
    assert!(!registry_has(&root), "nenhuma fila nem seq reinseridos");
    settle(&root);
    assert_eq!(lines(&root).len(), 1, "nada gravado no projeto removido");
}

#[tokio::test]
async fn revive_after_removal_writes_again_with_seq() {
    // F-11: reimportar o mesmo caminho limpa a marca e volta a gravar com seq.
    let root = setup("revive");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("um"));
    next_event(&mut rx).await;
    let _ = forget_project(&root);
    revive_project(&root);
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("volta"));
    let ev = next_event(&mut rx).await;
    assert!(ev.payload["seq"].as_u64().is_some());
    let file = lines(&root);
    assert!(file.iter().any(|l| l["text"] == "volta" && l["seq"] == ev.payload["seq"]));
}

#[tokio::test]
async fn seed_does_not_reinsert_removed_project() {
    // F-11: a semeadura do início de um run não reinsere seqs de projeto removido.
    let root = setup("seed-removed");
    let _ = forget_project(&root);
    seed_seq(&root, SPEC);
    assert!(!registry_has(&root));
    revive_project(&root);
}

#[test]
fn removed_set_is_capped_at_1024_oldest_dropped() {
    // F-11: o conjunto de removidos tem teto (1024) e descarta o mais antigo.
    for i in 0..1100 {
        let _ = forget_project(Path::new(&format!("/nants-tombstone-cap/{i}")));
    }
    assert!(removed_count() <= 1024, "{}", removed_count());
    assert!(removed_count() >= 1000, "{}", removed_count());
}

#[tokio::test]
async fn get_skips_invalid_lines() {
    let root = setup("invalid");
    write_lines(&root, 1, 2, "ok");
    let mut text = fs::read_to_string(dialog_file(&root)).expect("leitura");
    text.push_str("isto não é json\n{\"seq\":\"x\"}\n");
    write_lines_raw(&root, &text);
    let state = state_for(&root);
    let (_, body) = get(&state, &uri(PROJECT, SPEC, ""), HOST).await;
    assert_eq!(body["messages"].as_array().expect("lista").len(), 2);
    assert_eq!(body["last_seq"], 2);
}

fn write_lines_raw(root: &Path, text: &str) {
    fs::write(dialog_file(root), text).expect("escrita");
}

#[tokio::test]
async fn get_symlink_and_fifo_are_empty_without_error() {
    let root = setup("unsafe");
    let outside = temp_dir("unsafe-out");
    write_lines(&outside, 1, 3, "fora");
    symlink(outside.join("dialog.jsonl"), dialog_file(&root)).expect("ln");
    let state = state_for(&root);
    let (st, body) = get(&state, &uri(PROJECT, SPEC, ""), HOST).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["messages"], json!([]));
    fs::remove_file(dialog_file(&root)).expect("rm");
    let made = Command::new("mkfifo").arg(dialog_file(&root)).status();
    if made.map(|s| s.success()).unwrap_or(false) {
        let (st, body) = get(&state, &uri(PROJECT, SPEC, ""), HOST).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["messages"], json!([]));
    }
}

#[tokio::test]
async fn get_redacts_secret_written_in_clear_after_the_fact() {
    let root = setup("get-redact");
    write_lines_raw(
        &root,
        &format!("{}\n", json!({"seq":1,"ts":1,"agent":"reviewer","task_id":null,"attempt":null,"text":format!("vazou {SECRET}")})),
    );
    let mut cfg = config_for(&root);
    cfg.connections.push(crate::config::Connection {
        id: "c".into(),
        name: "c".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: crate::config::WireApi::default(),
        kind: Default::default(),
    });
    let state = AppState::new(root.join("config.toml"), cfg, 3000);
    let (_, body) = get(&state, &uri(PROJECT, SPEC, ""), HOST).await;
    let text = body.to_string();
    assert!(!text.contains(SECRET), "{text}");
    assert_eq!(body["messages"][0]["text"], "vazou ***");
}

#[tokio::test]
async fn get_errors_are_static_and_never_touch_files() {
    let root = setup("errors");
    let state = state_for(&root);
    let (st, _) = get(&state, &uri("inexistente", SPEC, ""), HOST).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = get(&state, &uri(PROJECT, "spec-que-nao-existe", ""), HOST).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = get(&state, &uri("bad.id", SPEC, ""), HOST).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _) = get(&state, &uri(PROJECT, "spec.x", ""), HOST).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    for q in ["?after=-1", "?after=abc", "?limit=0", "?limit=501", "?limit=x"] {
        let (st, _) = get(&state, &uri(PROJECT, SPEC, q), HOST).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{q}");
    }
    let (st, _) = get(&state, &uri(PROJECT, SPEC, ""), "evil.example.com").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(!dialog_file(&root).exists());
}

#[test]
fn exclude_is_idempotent_and_dialog_stays_out_of_git_status() {
    let root = temp_dir("exclude");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(&root)
            .env_remove("GIT_DIR")
            .output()
            .expect("git");
        assert!(out.status.success(), "{args:?}");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    fs::write(root.join("README.md"), "base\n").expect("readme");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "base"]);
    ensure_local_excludes(&root).expect("exclude");
    ensure_local_excludes(&root).expect("exclude again");
    let exclude = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
    assert_eq!(exclude.lines().filter(|l| l.trim() == DIALOG_EXCLUDE_LINE).count(), 1);
    fs::create_dir_all(root.join(".sdd/specs").join(SPEC)).expect("dir");
    fs::write(root.join(".sdd/specs").join(SPEC).join("spec.md"), "---\nid: \"001-demo\"\n---\n").expect("spec");
    let bus = EventBus::new();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("local"));
    // Aguarda a escrita ordenada terminar antes de checar o Git.
    for _ in 0..500 {
        if dialog_file(&root).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(dialog_file(&root).exists());
    // A spec de teste fica sem commit: o que importa é que o diálogo não aparece no status.
    let status = git(&["status", "--porcelain", "--untracked-files=all"]);
    assert!(!status.contains("dialog"), "diálogo aparece no status: {status}");
    assert_eq!(git(&["check-ignore", ".sdd/specs/001-demo/.dialog.jsonl.x.tmp"]).trim(), ".sdd/specs/001-demo/.dialog.jsonl.x.tmp");
    assert_eq!(git(&["check-ignore", ".sdd/specs/001-demo/dialog.jsonl"]).trim(), ".sdd/specs/001-demo/dialog.jsonl");
}

/// Linha JSON de exatamente `total` bytes (incluindo o `\n` final), com `seq` dado.
fn line_of_len(seq: u64, total: usize) -> String {
    let base = json!({"seq": seq, "ts": 1, "agent": "developer", "task_id": null, "attempt": null, "text": ""}).to_string();
    let pad = total - 1 - base.len();
    let text = "x".repeat(pad);
    format!("{}\n", json!({"seq": seq, "ts": 1, "agent": "developer", "task_id": null, "attempt": null, "text": text}))
}

#[tokio::test]
async fn missing_spec_dir_is_never_created_and_event_has_null_seq() {
    // F-01: a gravação não cria diretório de spec (quebraria o parser do projeto).
    let root = temp_dir("no-spec");
    fs::create_dir_all(root.join(".sdd/specs")).expect("specs");
    let before: Vec<_> = fs::read_dir(root.join(".sdd/specs")).expect("ls").collect();
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("sem spec"));
    let _ev = next_event(&mut rx).await;
    settle(&root);
    let after: Vec<_> = fs::read_dir(root.join(".sdd/specs")).expect("ls").collect();
    assert_eq!(before.len(), after.len(), "nenhum diretório novo em .sdd/specs");
    assert!(!root.join(".sdd/specs").join(SPEC).exists());
}

#[tokio::test]
async fn spec_removed_during_run_creates_no_directory() {
    // F-01: spec apagada depois do primeiro evento não reaparece por uma gravação posterior.
    let root = setup("removed");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let s = sink(&bus, &root, SecretSet::default());
    s.emit(agent_msg("antes"));
    assert!(next_event(&mut rx).await.payload["seq"].as_u64().is_some());
    settle(&root);
    fs::remove_dir_all(root.join(".sdd/specs").join(SPEC)).expect("rm");
    s.emit(agent_msg("depois"));
    next_event(&mut rx).await;
    settle(&root);
    assert!(!root.join(".sdd/specs").join(SPEC).exists());
}

#[tokio::test]
async fn long_or_formatted_agent_and_task_id_match_live_line_and_get() {
    // F-02: agent/task_id seguem a mesma normalização nos três caminhos.
    let root = setup("ids");
    let long_task = format!("T\u{7}\u{202E}{}", "9".repeat(100));
    let msg = RunnerEvent::AgentMessage {
        agent: "dev\u{7}eloper".into(),
        text: "oi".into(),
        task_id: Some(long_task),
        attempt: Some(1),
    };
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let state = AppState::new(root.join("config.toml"), config_for(&root), 3000);
    sink(&bus, &root, SecretSet::default()).emit(msg);
    let live = next_event(&mut rx).await.payload.clone();
    let stored = lines(&root)[0].clone();
    let (_, body) = get(&state, &uri(PROJECT, SPEC, ""), HOST).await;
    let got = body["messages"][0].clone();
    assert_eq!(live["task_id"], stored["task_id"]);
    assert_eq!(live["task_id"], got["task_id"]);
    assert_eq!(live["agent"], stored["agent"]);
    assert_eq!(live["agent"], got["agent"]);
    let task = live["task_id"].as_str().expect("task");
    assert!(task.len() <= 64, "{}", task.len());
    assert!(!task.contains('\u{7}') && !task.contains('\u{202E}'), "{task:?}");
    assert_eq!(live["agent"], "dev eloper");
}

#[tokio::test]
async fn flush_waits_for_queued_messages() {
    // F-03: após flush, todas as mensagens enfileiradas estão no arquivo.
    let root = setup("flush");
    let bus = EventBus::new();
    let s = sink(&bus, &root, SecretSet::default());
    for i in 0..25 {
        s.emit(agent_msg(&format!("m{i}")));
    }
    flush(Duration::from_secs(5));
    assert_eq!(lines(&root).len(), 25);
}

#[test]
fn flush_returns_without_hanging_when_worker_is_gone_or_stuck() {
    // F-03: thread morta (receptor fechado) ou sem confirmação: retorna no prazo.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(4);
    drop(rx);
    assert!(!flush_on(&tx, Duration::from_secs(2)));
    let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(4);
    let started = std::time::Instant::now();
    assert!(!flush_on(&tx, Duration::from_millis(200)));
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(rx);
}

/// Mensagem de agente com `text` (sem tarefa), para testes de ordem e de fila.
fn text_event(text: &str) -> ApiEvent {
    ApiEvent::new(
        EventType::AgentMessage,
        Some(PROJECT.into()),
        json!({"agent": "developer", "text": text, "task_id": null, "attempt": null, "seq": null}),
    )
}

/// Drena o assinante durante o envio em lotes (o barramento tem capacidade limitada).
fn drain_into(rx: &mut tokio::sync::broadcast::Receiver<ApiEvent>, out: &mut Vec<ApiEvent>) {
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
}

#[tokio::test]
async fn publication_is_not_delayed_by_a_stalled_writer() {
    // F-06: a escrita travada não atrasa o evento ao vivo, que já sai com seq.
    let root = setup("stalled");
    STALLED.lock().expect("gancho").push(root.clone());
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("com disco travado"));
    let ev = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("publicado sem esperar a gravação")
        .expect("evento");
    assert!(ev.payload["seq"].as_u64().is_some());
    STALLED.lock().expect("gancho").retain(|p| p != &root);
    settle(&root);
    assert_eq!(lines(&root).len(), 1);
}

#[tokio::test]
async fn stalled_project_does_not_delay_another_project() {
    // F-06: isolamento entre projetos (uma thread de gravação por projeto).
    let slow = setup("iso-slow");
    let fast = setup("iso-fast");
    STALLED.lock().expect("gancho").push(slow.clone());
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &slow, SecretSet::default()).emit(agent_msg("A"));
    sink(&bus, &fast, SecretSet::default()).emit(agent_msg("B"));
    let mut got = Vec::new();
    for _ in 0..2 {
        got.push(
            tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("B não pode esperar A")
                .expect("evento"),
        );
    }
    assert_eq!(got[1].payload["text"], "B");
    settle(&fast);
    assert_eq!(lines(&fast).len(), 1);
    STALLED.lock().expect("gancho").retain(|p| p != &slow);
    settle(&slow);
}

#[tokio::test]
async fn full_write_queue_never_blocks_and_publication_keeps_order() {
    // F-05: fila de gravação limitada; excedente só perde a gravação (lacuna), nunca a ordem.
    let root = setup("queue-full");
    STALLED.lock().expect("gancho").push(root.clone());
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let target = DialogTarget::new(&root, SPEC);
    let total = HARD_LIMIT + 50;
    let mut got = Vec::new();
    let started = std::time::Instant::now();
    for i in 0..total {
        deliver(&bus, Some(&target), text_event(&format!("m{i}")));
        if i % 500 == 0 {
            drain_into(&mut rx, &mut got);
        }
    }
    drain_into(&mut rx, &mut got);
    assert!(started.elapsed() < Duration::from_secs(10), "envios bloquearam");
    assert_eq!(got.len(), total, "todos publicados");
    let seqs: Vec<u64> = got.iter().map(|e| e.payload["seq"].as_u64().expect("seq")).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "ordem de emissão");
    STALLED.lock().expect("gancho").retain(|p| p != &root);
    settle(&root);
    // A thread pode ter pego 1 item antes de travar; o resto do excedente não entra no arquivo.
    assert!(lines(&root).len() <= HARD_LIMIT + 1, "{}", lines(&root).len());
    assert!(lines(&root).len() < total);
}

#[tokio::test]
async fn seq_at_u64_max_in_file_does_not_panic() {
    // Nota (a): arquivo com seq no limite não causa pânico; a publicação segue.
    let root = setup("seq-max");
    write_lines_raw(&root, &format!("{}\n", line_of_len(u64::MAX, 200)));
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("além do limite"));
    let ev = next_event(&mut rx).await;
    assert_eq!(ev.payload["text"], "além do limite");
}

#[tokio::test]
async fn final_message_of_active_run_is_written_before_serve_returns() {
    // F-07: mensagem final de um run ativo entra no arquivo antes de o servidor terminar.
    let root = setup("serve-final");
    let state = state_for(&root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(crate::server::serve_with_shutdown(
        listener,
        state.clone(),
        async move {
            let _ = stop_rx.await;
        },
    ));
    // Run lento: segura a marca de run ativo e emite a mensagem final só depois do shutdown.
    let active = crate::server::runs::ActiveRun::enter();
    let bus = state.events().clone();
    let target = DialogTarget::new(&root, SPEC);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ev = ApiEvent::new(
            EventType::AgentMessage,
            Some(PROJECT.into()),
            json!({"agent": "system", "text": "fim do run", "task_id": null, "attempt": null, "seq": null}),
        );
        deliver(&bus, Some(&target), ev);
        drop(active);
    });
    stop_tx.send(()).expect("parar");
    server.await.expect("join").expect("serve");
    assert!(lines(&root).iter().any(|l| l["text"] == "fim do run"));
}

#[tokio::test]
async fn rotation_without_newline_in_tail_still_shrinks_the_file() {
    // Nota (b): arquivo sem `\n` na metade final não cresce sem limite.
    let root = setup("no-newline");
    write_lines_raw(&root, &"y".repeat(DIALOG_MAX_BYTES as usize + 100_000));
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("depois"));
    next_event(&mut rx).await;
    settle(&root);
    let len = fs::metadata(dialog_file(&root)).expect("meta").len();
    assert!(len <= DIALOG_MAX_BYTES, "{len}");
}

#[tokio::test]
async fn rotation_removes_old_orphan_tmp_files() {
    // Nota (c): tmp órfão antigo do replace_atomic é removido ao rotacionar.
    let root = setup("tmp-orphan");
    let dir = root.join(".sdd/specs").join(SPEC);
    let orphan = dir.join(".dialog.jsonl.dead.tmp");
    fs::write(&orphan, "lixo").expect("tmp");
    fs::File::options()
        .write(true)
        .open(&orphan)
        .expect("abrir")
        .set_modified(SystemTime::UNIX_EPOCH)
        .expect("mtime");
    write_lines(&root, 1, 600, &"x".repeat(4000));
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    sink(&bus, &root, SecretSet::default()).emit(agent_msg("gatilho"));
    next_event(&mut rx).await;
    settle(&root);
    assert!(!orphan.exists(), "tmp órfão deveria ter sido removido");
}

#[tokio::test]
async fn window_starting_exactly_on_line_boundary_keeps_first_full_line() {
    // Nota (d): início da janela em fronteira de linha não descarta a primeira linha completa.
    let root = setup("boundary");
    let prefix = line_of_len(1, 500);
    let body = line_of_len(2, DIALOG_MAX_BYTES as usize);
    write_lines_raw(&root, &format!("{prefix}{body}"));
    let page = read_page(&root, SPEC, 0, 10);
    let seqs: Vec<u64> = page.messages.iter().map(|m| m.seq).collect();
    assert_eq!(seqs, vec![2]);
    assert!(page.truncated_before);
}

#[test]
fn exclude_includes_tmp_pattern_of_dialog_rotation() {
    // Nota (c): os tmp do diálogo ficam fora do Git.
    assert_eq!(DIALOG_TMP_EXCLUDE_LINE, ".sdd/specs/*/.dialog.jsonl.*.tmp");
}

/// Spec 102: mensagem do sistema com `code`/`params` é gravada com eles e o GET os devolve; as
/// demais seguem sem as chaves.
#[tokio::test]
async fn system_notice_code_and_params_are_persisted_and_served() {
    let root = setup("coded");
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let secrets = SecretSet::new(vec![SECRET.to_string()]);
    let mut params = serde_json::Map::new();
    params.insert("path".into(), json!(format!("img/{SECRET}.webp")));
    params.insert("count".into(), json!(2));
    let s = sink(&bus, &root, secrets.clone());
    s.emit(RunnerEvent::SystemNotice { text: "texto pt-BR".into(), code: "image.failed".into(), params, task_id: None });
    s.emit(agent_msg("comum"));
    let live = next_event(&mut rx).await;
    assert_eq!(live.payload["code"], "image.failed");
    next_event(&mut rx).await;
    let saved = lines(&root);
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0]["code"], "image.failed");
    assert_eq!(saved[0]["params"]["count"], 2);
    assert!(!saved[0]["params"]["path"].as_str().unwrap_or_default().contains(SECRET));
    assert!(saved[1].get("code").is_none() && saved[1].get("params").is_none(), "{}", saved[1]);
    let view = page_view(&secrets, read_page(&root, SPEC, 0, 10));
    let messages = view["messages"].as_array().cloned().unwrap_or_default();
    assert_eq!(messages[0]["code"], "image.failed");
    assert_eq!(messages[0]["text"], "texto pt-BR");
    assert_eq!(messages[0]["params"]["count"], 2);
    assert!(messages[1].get("code").is_none());
}
