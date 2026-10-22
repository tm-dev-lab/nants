//! Testes da revisão do plano (spec 086 T08), com fake do modelo.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;
use tokio::sync::Notify;

use super::*;
use crate::agents::architect::testing::{text, Reply, Scripted};
use crate::agents::llm_client::{ChatRequest, Usage};
use crate::agents::prompts::{plan_review_system, EXAMPLE_SPEC_MD, EXAMPLE_TASK_MD};
use crate::config::{AgentConfig, Config, Connection, Effort, Project, WireApi};

const PROJECT: &str = "proj";
const SPEC: &str = "001-exemplo";
const SECRET: &str = "sk-SEGREDO-PLANO-0123456789";

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("nants-plan-review-{}-{n}-{tag}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".sdd/specs")).expect("mkdir");
    dir
}

fn write_plan(root: &Path, status: &str, task: &str) {
    let dir = root.join(format!(".sdd/specs/{SPEC}"));
    fs::create_dir_all(dir.join("tasks")).expect("tasks");
    fs::write(dir.join("spec.md"), EXAMPLE_SPEC_MD.replace("AWAITING_APPROVAL", status)).expect("spec");
    fs::write(dir.join("tasks/T01.md"), task).expect("task");
}

fn config_for(root: &Path, reviewer: bool) -> Config {
    let mut cfg = Config::default();
    cfg.connections.push(Connection {
        id: "c".into(),
        name: "Conexão".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: SECRET.into(),
        wire_api: WireApi::default(),
    });
    if reviewer {
        cfg.agents.reviewer = AgentConfig { connection_id: "c".into(), model: "rev".into(), effort: Effort::Medium, max_steps: None };
    }
    cfg.projects.push(Project {
        id: PROJECT.into(),
        name: "Projeto".into(),
        path: root.display().to_string(),
        created_at: "2026-10-18T00:00:00Z".into(),
    });
    cfg
}

fn state(root: &Path, reviewer: bool, backend: Arc<dyn ChatBackend>) -> AppState {
    AppState::new(root.join("config.toml"), config_for(root, reviewer), 3000).with_report_backend(backend)
}

async fn run(state: &AppState, root: &Path) -> usize {
    let handles = after_chat_turn(state, PROJECT, root.to_path_buf()).await;
    let n = handles.len();
    for h in handles {
        h.await.expect("task");
    }
    n
}

fn record(root: &Path) -> PlanReviewRecord {
    read_record(root, SPEC).expect("plan-review.json")
}

#[tokio::test]
async fn reviews_only_awaiting_approval_and_records_issues() {
    let root = tmp("awaiting");
    write_plan(&root, "DRAFT", EXAMPLE_TASK_MD);
    let reply = json!({"issues": [{"task": "T01", "kind": "untestable", "text": "Critério vago."}]}).to_string();
    let scripted = Scripted::new(vec![text(&reply)]);
    let st = state(&root, true, scripted.clone());
    assert_eq!(run(&st, &root).await, 0, "DRAFT não dispara");
    assert!(scripted.requests().is_empty());
    assert!(read_record(&root, SPEC).is_none());

    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    fs::create_dir_all(root.join(".git/info")).expect("git");
    let mut rx = st.events().subscribe();
    assert_eq!(run(&st, &root).await, 1);
    let reqs = scripted.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].messages[0].content, plan_review_system());
    assert!(reqs[0].messages[1].content.contains("### tasks/T01.md"), "spec e tarefas vão como dado");
    let rec = record(&root);
    assert_eq!(rec.status, STATUS_OK);
    assert_eq!(rec.signature, plan_signature(&root, SPEC).expect("sig"));
    assert_eq!(rec.issues, vec![PlanIssue { task: Some("T01".into()), kind: "untestable".into(), text: "Critério vago.".into() }]);
    let ev = rx.try_recv().expect("evento");
    let mut seen = vec![ev];
    while let Ok(e) = rx.try_recv() {
        seen.push(e);
    }
    assert!(seen.iter().any(|e| e.event_type() == "sdd_draft_updated"), "publica ao concluir");
    let stage = seen.iter().find(|e| e.event_type() == "agent_activity").expect("atividade ao vivo");
    assert_eq!(stage.payload["stage"], PLAN_REVIEW_STAGE);
    assert_eq!(stage.payload["agent"], "reviewer");
    let exclude = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
    assert!(exclude.lines().any(|l| l == PLAN_REVIEW_EXCLUDE_LINE), "resultado local: {exclude}");
}

#[tokio::test]
async fn same_signature_is_not_reviewed_twice_and_new_version_is() {
    let root = tmp("same-sig");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let scripted = Scripted::new(vec![text("{\"issues\": []}"), text("{\"issues\": []}")]);
    let st = state(&root, true, scripted.clone());
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(run(&st, &root).await, 0, "mesma assinatura: não revisa de novo");
    assert_eq!(scripted.requests().len(), 1);
    write_plan(&root, "AWAITING_APPROVAL", &EXAMPLE_TASK_MD.replace("O que implementar.", "Outra coisa."));
    assert_eq!(run(&st, &root).await, 1, "versão nova é revisada");
    assert_eq!(scripted.requests().len(), 2);
}

#[tokio::test]
async fn invalid_json_and_provider_failure_are_unavailable() {
    let root = tmp("invalid");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let st = state(&root, true, Scripted::new(vec![text("não é JSON")]));
    run(&st, &root).await;
    let rec = record(&root);
    assert_eq!(rec.status, STATUS_UNAVAILABLE);
    assert!(rec.issues.is_empty());
    assert_eq!(rec.detail.as_deref(), Some(DETAIL_INVALID_JSON));

    let root = tmp("provider");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let st = state(&root, true, Scripted::new(Vec::new()));
    run(&st, &root).await;
    let rec = record(&root);
    assert_eq!(rec.status, STATUS_UNAVAILABLE);
    assert!(rec.detail.as_deref().is_some_and(|d| d.starts_with("provider_")), "{:?}", rec.detail);
    let view = view(&root, SPEC, &SecretSet::default()).expect("view");
    assert_eq!(view.status, STATUS_UNAVAILABLE);
}

#[test]
fn parse_limits_issues_and_redacts_before_cutting() {
    let secrets = SecretSet::new([SECRET.to_string()]);
    let long = format!("{} chave {SECRET} no fim", "palavra ".repeat(60));
    let mut items: Vec<serde_json::Value> = (0..12)
        .map(|i| json!({"task": format!("T{i:02}"), "kind": "scope", "text": format!("Item {i}")}))
        .collect();
    items.insert(0, json!({"task": null, "kind": "contradiction", "text": long}));
    items.insert(1, json!({"task": "../x", "kind": "preferencia", "text": "ignorado"}));
    let raw = format!("```json\n{}\n```", json!({"issues": items}));
    let issues = parse_issues(&raw, &secrets).expect("ok");
    assert_eq!(issues.len(), MAX_ISSUES);
    assert_eq!(issues[0].task, None);
    assert!(issues[0].text.len() <= MAX_ISSUE_TEXT_BYTES, "{}", issues[0].text.len());
    assert!(!issues.iter().any(|i| i.text.contains("SEGREDO")));
    assert!(issues.iter().all(|i| i.kind != "preferencia"));
    // Segredo na fronteira do corte: redigido antes, nenhum prefixo vaza.
    let edge = format!("{}{SECRET}", "a".repeat(MAX_ISSUE_TEXT_BYTES - 8));
    let cut = parse_issues(&json!({"issues": [{"task": null, "kind": "scope", "text": edge}]}).to_string(), &secrets).expect("ok");
    assert!(!cut[0].text.contains("sk-SEG"), "{}", cut[0].text);
    assert_eq!(parse_issues("{\"issues\": []}", &secrets), Some(Vec::new()));
    assert_eq!(parse_issues("{\"outra\": 1}", &secrets), None);
    assert_eq!(parse_issues("{\"issues\": [{\"kind\": \"x\", \"text\": \"y\"}]}", &secrets), None, "só itens inválidos");
}

#[tokio::test]
async fn stored_text_is_redacted() {
    let root = tmp("redact");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let reply = json!({"issues": [{"task": "T01", "kind": "scope", "text": format!("usa {SECRET}")}]}).to_string();
    let st = state(&root, true, Scripted::new(vec![text(&reply)]));
    run(&st, &root).await;
    let raw = fs::read_to_string(root.join(format!(".sdd/specs/{SPEC}/{PLAN_REVIEW_FILE}"))).expect("arquivo");
    assert!(!raw.contains(SECRET), "{raw}");
}

#[tokio::test]
async fn plan_review_disappears_when_plan_changes() {
    let root = tmp("changes");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let reply = json!({"issues": [{"task": null, "kind": "uncovered_criterion", "text": "G1 sem tarefa."}]}).to_string();
    let st = state(&root, true, Scripted::new(vec![text(&reply)]));
    run(&st, &root).await;
    let secrets = SecretSet::default();
    let v = view(&root, SPEC, &secrets).expect("atual");
    assert_eq!(v.status, STATUS_OK);
    assert_eq!(v.issues.len(), 1);
    write_plan(&root, "AWAITING_APPROVAL", &EXAMPLE_TASK_MD.replace("Critério verificável.", "Outro critério."));
    assert!(view(&root, SPEC, &secrets).is_none(), "resultado de versão antiga não aparece");
}

#[tokio::test]
async fn usage_is_recorded_as_reviewer_in_planning_file() {
    let root = tmp("usage");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let mut reply = text("{\"issues\": []}").expect("resposta");
    reply.usage = Some(Usage::new(7, 3));
    let st = state(&root, true, Scripted::new(vec![Ok(reply)]));
    run(&st, &root).await;
    let usage = fs::read_to_string(root.join(".sdd/specs/.usage.jsonl")).expect("uso");
    let line: serde_json::Value = serde_json::from_str(usage.lines().next().expect("linha")).expect("json");
    assert_eq!(line["agent"], "reviewer");
    assert_eq!(line["model"], "rev");
}

#[tokio::test]
async fn without_reviewer_nothing_happens() {
    let root = tmp("no-reviewer");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let scripted = Scripted::new(vec![text("{\"issues\": []}")]);
    let st = state(&root, false, scripted.clone());
    assert_eq!(run(&st, &root).await, 0);
    assert!(scripted.requests().is_empty());
    assert!(view(&root, SPEC, &SecretSet::default()).is_none());
}

/// Fake que espera a liberação antes de responder.
struct Gated {
    gate: Arc<Notify>,
    calls: AtomicUsize,
}

impl ChatBackend for Gated {
    fn complete<'a>(&'a self, _req: &'a ChatRequest) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.gate.notified().await;
            text("{\"issues\": []}")
        })
    }
}

#[tokio::test]
async fn running_review_is_exposed_and_not_duplicated() {
    let root = tmp("running");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let gated = Arc::new(Gated { gate: Arc::new(Notify::new()), calls: AtomicUsize::new(0) });
    let st = state(&root, true, gated.clone());
    let first = after_chat_turn(&st, PROJECT, root.clone()).await;
    assert_eq!(first.len(), 1);
    let secrets = SecretSet::default();
    assert_eq!(view(&root, SPEC, &secrets).expect("running").status, STATUS_RUNNING);
    assert!(after_chat_turn(&st, PROJECT, root.clone()).await.is_empty(), "em curso: não duplica");
    for _ in 0..200 {
        if gated.calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    gated.gate.notify_one();
    for h in first {
        h.await.expect("task");
    }
    assert_eq!(gated.calls.load(Ordering::SeqCst), 1);
    assert_eq!(view(&root, SPEC, &secrets).expect("ok").status, STATUS_OK);
}

/// Spec 087 T06: registro inseguro (symlink ou não regular) não dispara revisão nem chama o modelo,
/// e o alvo do symlink nunca é seguido.
#[tokio::test]
async fn unsafe_record_is_never_followed_nor_reviewed() {
    let root = tmp("symlink");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let outside = tmp("symlink-target").join("alvo.json");
    fs::write(&outside, "original").expect("alvo");
    std::os::unix::fs::symlink(&outside, root.join(format!(".sdd/specs/{SPEC}/{PLAN_REVIEW_FILE}"))).expect("link");
    let scripted = Scripted::new(vec![text("{\"issues\": []}")]);
    let st = state(&root, true, scripted.clone());
    assert!(!needs_review(&root, SPEC));
    assert_eq!(run(&st, &root).await, 0, "symlink: sem vaga e sem revisão");
    assert!(scripted.requests().is_empty(), "nenhuma chamada ao modelo");
    assert_eq!(fs::read_to_string(&outside).expect("r"), "original");
    assert!(view(&root, SPEC, &SecretSet::default()).is_none());

    // Não regular (diretório no lugar do arquivo): também não revisa.
    let root = tmp("not-regular");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    fs::create_dir_all(root.join(format!(".sdd/specs/{SPEC}/{PLAN_REVIEW_FILE}"))).expect("dir");
    let scripted = Scripted::new(vec![text("{\"issues\": []}")]);
    let st = state(&root, true, scripted.clone());
    assert_eq!(run(&st, &root).await, 0, "não regular: sem revisão");
    assert!(scripted.requests().is_empty());
}

#[tokio::test]
async fn unreadable_plan_after_claim_still_publishes_and_leaves_running() {
    // Plano que some entre a reserva e a revisão (leitura falha): sem modelo, mas a UI recarrega.
    let root = tmp("vanished");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let gate = Arc::new(Notify::new());
    let gated = Arc::new(Gated { gate: Arc::clone(&gate), calls: AtomicUsize::new(0) });
    let st = state(&root, true, gated.clone());
    let claim = Claim::take(&root, SPEC).expect("vaga");
    fs::remove_file(root.join(format!(".sdd/specs/{SPEC}/spec.md"))).expect("rm");
    let setup = reviewer_setup(&st).await.expect("setup");
    assert_eq!(review_once(&st, PROJECT, &root, SPEC, &setup).await, Err(Stop::Failed));
    drop(claim);
    assert_eq!(gated.calls.load(Ordering::SeqCst), 0);
    assert!(!is_running(&root, SPEC));
}

#[tokio::test]
async fn gitfile_repo_is_refused_before_model_without_claim() {
    // F-01: `.git` gitfile (worktree ligada): o exclude local não alcança; nada é disparado.
    let root = tmp("gitfile");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    fs::write(root.join(".git"), "gitdir: /tmp/outro/.git/worktrees/x\n").expect("gitfile");
    let scripted = Scripted::new(vec![text("{\"issues\": []}")]);
    let st = state(&root, true, scripted.clone());
    assert_eq!(run(&st, &root).await, 0);
    assert!(scripted.requests().is_empty(), "nenhuma chamada ao modelo");
    assert!(!root.join(format!(".sdd/specs/{SPEC}/{PLAN_REVIEW_FILE}")).exists());
    assert!(!is_running(&root, SPEC));
    assert!(view(&root, SPEC, &SecretSet::default()).is_none(), "sem running no estado");
    assert!(
        view_at(&root, SPEC, &SecretSet::default(), true, SystemTime::now()).is_none(),
        "revisão impossível: nada de pending"
    );
}

/// Spec 092 T02: sem resultado nem revisão em curso da versão atual, com Revisor configurado,
/// `pending` até 3 min depois da última alteração do plano e `stale` depois (relógio injetado).
#[test]
fn pending_then_stale_by_injected_clock() {
    let root = tmp("pending");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let secrets = SecretSet::default();
    let changed = plan_modified(&root, SPEC).expect("mtime");
    let at = |secs: u64| view_at(&root, SPEC, &secrets, true, changed + Duration::from_secs(secs));
    let pending = at(0).expect("pending");
    assert_eq!(pending.status, STATUS_PENDING);
    assert!(pending.at.is_none() && pending.issues.is_empty());
    assert_eq!(at(179).expect("ainda pending").status, STATUS_PENDING);
    assert_eq!(at(180).expect("stale").status, STATUS_STALE);
    assert_eq!(at(3600).expect("stale").status, STATUS_STALE);
    // Relógio pouco antes do mtime (ajuste de hora): continua esperando.
    let early = view_at(&root, SPEC, &secrets, true, changed - Duration::from_secs(5)).expect("pending");
    assert_eq!(early.status, STATUS_PENDING);
    let edge = view_at(&root, SPEC, &secrets, true, changed - Duration::from_secs(180)).expect("pending");
    assert_eq!(edge.status, STATUS_PENDING);
    // mtime 1 h no futuro (relógio defasado): a espera não fica presa; vira `stale`.
    let skewed = view_at(&root, SPEC, &secrets, true, changed - Duration::from_secs(3600)).expect("stale");
    assert_eq!(skewed.status, STATUS_STALE);
    let past_window = view_at(&root, SPEC, &secrets, true, changed - Duration::from_secs(181)).expect("stale");
    assert_eq!(past_window.status, STATUS_STALE);

    // Alterar uma tarefa reinicia a espera (vale o mtime mais recente do plano).
    let task = root.join(format!(".sdd/specs/{SPEC}/tasks/T01.md"));
    let later = changed + Duration::from_secs(600);
    fs::File::options().write(true).open(&task).expect("abrir").set_modified(later).expect("mtime");
    assert_eq!(plan_modified(&root, SPEC), Some(later));
    let reset = view_at(&root, SPEC, &secrets, true, later + Duration::from_secs(10)).expect("pending");
    assert_eq!(reset.status, STATUS_PENDING);
    assert_eq!(view_at(&root, SPEC, &secrets, true, later + Duration::from_secs(181)).expect("stale").status, STATUS_STALE);
}

#[test]
fn without_reviewer_pending_is_absent() {
    let root = tmp("pending-no-reviewer");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let secrets = SecretSet::default();
    let now = SystemTime::now();
    assert!(view_at(&root, SPEC, &secrets, false, now).is_none());
    assert!(view_at(&root, SPEC, &secrets, false, now + Duration::from_secs(600)).is_none());
}

#[test]
fn current_result_and_running_win_over_pending() {
    let root = tmp("pending-result");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    let secrets = SecretSet::default();
    let now = SystemTime::now();
    {
        let _claim = Claim::take(&root, SPEC).expect("vaga");
        assert_eq!(view_at(&root, SPEC, &secrets, true, now).expect("running").status, STATUS_RUNNING);
    }
    let record = PlanReviewRecord {
        signature: plan_signature(&root, SPEC).expect("sig"),
        at: "2026-10-19T00:00:00Z".into(),
        status: STATUS_OK.into(),
        issues: Vec::new(),
        detail: None,
    };
    let body = serde_json::to_string(&record).expect("json");
    fs::write(root.join(format!(".sdd/specs/{SPEC}/{PLAN_REVIEW_FILE}")), body).expect("registro");
    let late = now + Duration::from_secs(3600);
    assert_eq!(view_at(&root, SPEC, &secrets, true, late).expect("ok").status, STATUS_OK);
    // Versão nova do plano: o registro antigo não vale; volta a esperar.
    write_plan(&root, "AWAITING_APPROVAL", &EXAMPLE_TASK_MD.replace("Critério verificável.", "Outro critério."));
    let again = view_at(&root, SPEC, &secrets, true, SystemTime::now()).expect("pending");
    assert_eq!(again.status, STATUS_PENDING);
}
