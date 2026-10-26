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
        kind: Default::default(),
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
    let expected = PlanIssue { id: Some("P1".into()), ..PlanIssue::new(Some("T01".into()), "untestable", "Critério vago.") };
    assert_eq!(rec.issues, vec![expected]);
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
    let cycle = crate::server::plan_consensus::Cycle::default();
    let failed = review_once(&st, PROJECT, &root, SPEC, &setup, 0, None, &cycle).await;
    assert!(matches!(failed, Err(Stop::Failed)));
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
        rounds: Vec::new(),
        outcome: None,
        points: Vec::new(),
        next_point: 0,
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


/// Spec 124 T04: o plano enviado ao Revisor traz as capacidades medidas do ambiente e a regra de
/// apontar como untestable o critério que depende de capacidade ausente.
#[test]
fn plan_carries_environment_capabilities_with_the_plan_rule() {
    use crate::sdd::capabilities::{self, Capabilities, Probe};
    let root = tmp("caps");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    fs::write(root.join(".sdd/settings.yaml"), "base_branch: main\nmerge_strategy: squash\ntest_command: \"true\"\nenvironment: docker\n").expect("settings");
    let plan = build_plan(&root, SPEC, &SecretSet::default()).expect("plano");
    assert!(!plan.contains(capabilities::SECTION_TITLE), "Docker sem medida: sem seção");
    let mut caps = Capabilities::default();
    caps.apply_probe("docker", "h", Probe { gpu: Some(false), display: Some(true), cpus: Some(8), memory_mb: Some(16000) }, "t".into());
    capabilities::write(&root, &caps).expect("estado");
    let plan = build_plan(&root, SPEC, &SecretSet::default()).expect("plano");
    assert!(plan.contains("GPU ou dispositivo de vídeo: ausente") && plan.contains("Tela ou servidor gráfico: presente"), "{plan}");
    assert!(plan.contains("aponte-o como untestable"), "{plan}");
    // No host sem nada gravado, a medida é feita na hora.
    fs::write(root.join(".sdd/settings.yaml"), "base_branch: main\nmerge_strategy: squash\ntest_command: \"true\"\n").expect("settings");
    fs::remove_file(root.join(capabilities::CAPABILITIES_REL)).expect("rm");
    let plan = build_plan(&root, SPEC, &SecretSet::default()).expect("plano");
    assert!(plan.contains("Onde os testes rodam: host"), "{plan}");
    assert!(plan_review_system().contains("capacidades do ambiente"));
    let _ = fs::remove_dir_all(&root);
}

/// Tarefa com `n` critérios de aceitação.
fn big_task(n: usize) -> String {
    let criteria: Vec<String> = (1..=n).map(|i| format!("{i}. critério {i}")).collect();
    let head = EXAMPLE_TASK_MD.split("## Critérios de Aceitação").next().unwrap_or_default();
    format!("{head}## Critérios de Aceitação\n{}\n", criteria.join("\n"))
}

/// Spec 124 T06: o modelo aprende o tipo `too_large` com a sugestão de divisão, e o plano leva o
/// tamanho contado de cada tarefa.
#[test]
fn too_large_is_a_known_kind_and_the_plan_carries_task_sizes() {
    let p = plan_review_system();
    assert!(p.contains("too_large") && p.contains("servidor, cliente e testes ponta a ponta") && p.contains("dividir"), "{p}");
    assert!(!p.contains("divisão das tarefas) não é problema"), "dividir uma tarefa grande deixou de ser preferência");
    let raw = json!({"issues": [{"task": "T01", "kind": "too_large", "text": "Mistura servidor e cliente; divida em duas."}]}).to_string();
    let issues = parse_issues(&raw, &SecretSet::default()).expect("válido");
    assert_eq!(issues[0].kind, TOO_LARGE);
    let root = tmp("sizes");
    write_plan(&root, "AWAITING_APPROVAL", &big_task(7));
    let plan = build_plan(&root, SPEC, &SecretSet::default()).expect("plano");
    assert!(plan.contains("### Tamanho das tarefas (contado pelo servidor)\n- T01: 7 critérios de aceitação; allowed_scope: src/**"), "{plan}");
    assert_eq!(task_sizes(&root, SPEC).iter().map(|s| (s.id.as_str(), s.criteria)).collect::<Vec<_>>(), vec![("T01", 7)]);
    let _ = fs::remove_dir_all(&root);
}

/// Spec 124 T06: a tarefa com muitos critérios vira apontamento com sugestão de divisão, no idioma
/// do projeto, sem duplicar o que o modelo já apontou e sem perder vaga para os do modelo.
#[test]
fn many_criteria_become_a_too_large_issue_with_a_split_suggestion() {
    use crate::agents::recovery_plan::MANY_CRITERIA;
    use crate::i18n::Lang;
    let size = |id: &str, criteria: usize| TaskSize { id: id.into(), criteria, weight: criteria, scope: Vec::new() };
    let sizes = [size("T01", MANY_CRITERIA - 1), size("T02", MANY_CRITERIA), size("T03", 9)];
    let model = PlanIssue::new(Some("T03".into()), TOO_LARGE, "Mistura servidor e e2e.");
    let out = with_size_issues(vec![model.clone()], &sizes, Lang::PtBr);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out[0], model, "o do modelo fica, sem duplicar a T03");
    assert_eq!(out[1].task.as_deref(), Some("T02"));
    assert_eq!(out[1].kind, TOO_LARGE);
    assert!(out[1].text.starts_with("6 critérios de aceitação: grande demais para uma revisão. Divida em tarefas menores"), "{}", out[1].text);
    let en = with_size_issues(Vec::new(), &sizes, Lang::En);
    assert!(en.iter().all(|i| i.text.contains("Split it into smaller tasks")), "{en:?}");
    assert_eq!(en.iter().map(|i| i.task.as_deref()).collect::<Vec<_>>(), vec![Some("T02"), Some("T03")]);
    // Lista cheia do modelo: o do servidor entra e um do modelo cede a vaga.
    let full: Vec<PlanIssue> = (0..MAX_ISSUES).map(|i| PlanIssue::new(None, "scope", &format!("p{i}"))).collect();
    let out = with_size_issues(full, &[size("T02", 8)], Lang::PtBr);
    assert_eq!(out.len(), MAX_ISSUES);
    assert_eq!(out.last().map(|i| i.kind.as_str()), Some(TOO_LARGE));
}

/// Spec 128 T03 (C-12): critérios longos valem mais de um. Com os critérios reais da T12 do Test 6
/// (3 parágrafos de 84 a 94 palavras), a tarefa vale 6 e vira `too_large` com o texto da densidade,
/// nos dois idiomas; o plano enviado ao Revisor mostra o peso. Não barra nada: só o apontamento.
#[test]
fn dense_criteria_become_a_too_large_issue() {
    use crate::agents::recovery_plan::tests::TEST6_T12_ACCEPTANCE;
    use crate::i18n::Lang;
    let root = tmp("dense");
    let head = EXAMPLE_TASK_MD.split("## Critérios de Aceitação").next().unwrap_or_default();
    write_plan(&root, "AWAITING_APPROVAL", &format!("{head}## Critérios de Aceitação\n{TEST6_T12_ACCEPTANCE}"));
    let sizes = task_sizes(&root, SPEC);
    assert_eq!(sizes.iter().map(|s| (s.criteria, s.weight)).collect::<Vec<_>>(), vec![(3, 6)]);
    let plan = build_plan(&root, SPEC, &SecretSet::default()).expect("plano");
    assert!(plan.contains("- T01: 3 critérios de aceitação (valem 6 pela densidade: critério com mais de 60 palavras conta como mais de um); allowed_scope"), "{plan}");
    let pt = with_size_issues(Vec::new(), &sizes, Lang::PtBr);
    assert_eq!(pt.len(), 1, "{pt:?}");
    assert_eq!((pt[0].task.as_deref(), pt[0].kind.as_str()), (Some("T01"), TOO_LARGE));
    assert!(pt[0].text.starts_with("3 critérios de aceitação, que valem por 6 pelo tamanho do texto (critério com mais de 60 palavras"), "{}", pt[0].text);
    assert!(pt[0].text.contains("um critério por comportamento observável, numa frase curta"), "{}", pt[0].text);
    let en = with_size_issues(Vec::new(), &sizes, Lang::En);
    assert!(en[0].text.starts_with("3 acceptance criteria, which count as 6 because of their length"), "{}", en[0].text);
    // Um critério só, longo: singular.
    let one = TaskSize { id: "T02".into(), criteria: 1, weight: 6, scope: Vec::new() };
    let single = with_size_issues(Vec::new(), &[one], Lang::PtBr);
    assert!(single[0].text.starts_with("1 critério de aceitação, que vale por 6"), "{}", single[0].text);
    // Abaixo do limite, mesmo com densidade: nada.
    let light = TaskSize { id: "T03".into(), criteria: 3, weight: 5, scope: Vec::new() };
    assert!(with_size_issues(Vec::new(), &[light], Lang::PtBr).is_empty());
    let _ = fs::remove_dir_all(&root);
}

/// Spec 124 T06: critérios detalhados em subitens contam só pelo primeiro nível. Três critérios com
/// quatro subitens cada não viram `too_large`, nem no plano enviado nem no resultado gravado.
#[tokio::test]
async fn detailed_sub_items_do_not_make_a_task_too_large() {
    let root = tmp("sub-items");
    let head = EXAMPLE_TASK_MD.split("## Critérios de Aceitação").next().unwrap_or_default();
    let criteria: Vec<String> = (1..=3)
        .map(|i| format!("{i}. critério {i}:\n   - detalhe a\n   - detalhe b\n   - detalhe c\n   - detalhe d"))
        .collect();
    write_plan(&root, "AWAITING_APPROVAL", &format!("{head}## Critérios de Aceitação\n{}\n", criteria.join("\n")));
    assert_eq!(task_sizes(&root, SPEC).iter().map(|s| s.criteria).collect::<Vec<_>>(), vec![3]);
    let plan = build_plan(&root, SPEC, &SecretSet::default()).expect("plano");
    assert!(plan.contains("- T01: 3 critérios de aceitação;"), "{plan}");
    fs::create_dir_all(root.join(".git/info")).expect("git");
    let scripted = Scripted::new(vec![text("{\"issues\": []}")]);
    let st = state(&root, true, scripted.clone());
    assert_eq!(run(&st, &root).await, 1);
    let rec = record(&root);
    assert_eq!(rec.status, STATUS_OK);
    assert!(rec.issues.is_empty(), "{:?}", rec.issues);
    let _ = fs::remove_dir_all(&root);
}

/// Spec 124 T06: no fluxo inteiro, o resultado gravado traz o apontamento `too_large` do servidor.
#[tokio::test]
async fn review_records_the_too_large_issue_for_a_big_task() {
    let root = tmp("too-large");
    write_plan(&root, "AWAITING_APPROVAL", &big_task(8));
    fs::create_dir_all(root.join(".git/info")).expect("git");
    let scripted = Scripted::new(vec![text("{\"issues\": []}")]);
    let st = state(&root, true, scripted.clone());
    assert_eq!(run(&st, &root).await, 1);
    let rec = record(&root);
    assert_eq!(rec.status, STATUS_OK);
    assert_eq!(rec.issues.len(), 1, "{:?}", rec.issues);
    assert_eq!((rec.issues[0].task.as_deref(), rec.issues[0].kind.as_str()), (Some("T01"), TOO_LARGE));
    assert!(rec.issues[0].text.starts_with("8 critérios"), "{}", rec.issues[0].text);
    let shown = view(&root, SPEC, &SecretSet::default()).expect("vista");
    assert_eq!(shown.issues, rec.issues);
    let _ = fs::remove_dir_all(&root);
}

// ---- Spec 128 T02: ciclo de consenso do plano (Revisor e Arquiteto falsos) ----

use crate::agents::prompts::PLAN_CONSENSUS_INTRO;
use crate::server::plan_consensus::{
    DECISION_ACCEPT, DECISION_REJECT, MAX_CONSENSUS_ROUNDS, OUTCOME_AWAITING_USER, OUTCOME_CONSENSUS, OUTCOME_DISPUTED,
    OUTCOME_OPEN, OUTCOME_UNAVAILABLE,
};
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

/// Texto da mensagem do usuário nos testes (o Arquiteto falso a reconhece).
const USER_MESSAGE: &str = "MENSAGEM DO USUÁRIO: siga com o plano.";

/// Arquiteto falso por turno: cada pedido de rodada do consenso carrega o roteiro seguinte; a
/// mensagem do usuário carrega o roteiro do usuário; o que sobra (a continuação dos avisos do
/// plano) responde "Plano conferido.". Com `gate`, a primeira chamada de uma rodada espera a
/// liberação (avisa em `reached`).
struct Architect {
    rounds: StdMutex<VecDeque<Vec<Reply>>>,
    user: StdMutex<VecDeque<Vec<Reply>>>,
    current: StdMutex<VecDeque<Reply>>,
    requests: StdMutex<Vec<ChatRequest>>,
    gate: Option<(Arc<Notify>, Arc<Notify>)>,
}

impl Architect {
    fn new(rounds: Vec<Vec<Reply>>) -> Arc<Self> {
        Self::build(rounds, Vec::new(), None)
    }

    fn build(rounds: Vec<Vec<Reply>>, user: Vec<Vec<Reply>>, gate: Option<(Arc<Notify>, Arc<Notify>)>) -> Arc<Self> {
        Arc::new(Self {
            rounds: StdMutex::new(rounds.into()),
            user: StdMutex::new(user.into()),
            current: StdMutex::new(VecDeque::new()),
            requests: StdMutex::new(Vec::new()),
            gate,
        })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl ChatBackend for Architect {
    fn complete<'a>(&'a self, req: &'a ChatRequest) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        Box::pin(async move {
            self.requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(req.clone());
            let last = req.messages.last().map(|m| (m.role, m.content.clone()));
            let mut wait = false;
            if let Some((crate::agents::llm_client::Role::User, content)) = &last {
                let next = if content.starts_with(PLAN_CONSENSUS_INTRO) {
                    wait = self.gate.is_some();
                    self.rounds.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front()
                } else if content.contains(USER_MESSAGE) {
                    self.user.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front()
                } else {
                    None
                };
                if let Some(script) = next {
                    *self.current.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = script.into();
                }
            }
            if let (true, Some((reached, release))) = (wait, &self.gate) {
                reached.notify_one();
                release.notified().await;
            }
            let reply = self.current.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front();
            reply.unwrap_or_else(|| text("Plano conferido."))
        })
    }
}

/// Estado com o Revisor e o Arquiteto falsos.
fn consensus_state(root: &Path, reviewer: Arc<Scripted>, architect: Arc<Architect>) -> AppState {
    let mut cfg = config_for(root, true);
    cfg.agents.architect = AgentConfig { connection_id: "c".into(), model: "arq".into(), effort: Effort::Low, max_steps: None };
    AppState::new(root.join("config.toml"), cfg, 3000).with_llm_backend(architect).with_report_backend(reviewer)
}

fn plan_root(tag: &str) -> PathBuf {
    let root = tmp(tag);
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    fs::create_dir_all(root.join(".git/info")).expect("git");
    root
}

/// Resposta da revisão: `(tarefa, tipo, texto, ref, new_argument)`.
fn review(items: &[(&str, &str, &str, Option<&str>, bool)]) -> Reply {
    let list: Vec<serde_json::Value> = items
        .iter()
        .map(|(task, kind, text, reference, new_argument)| {
            json!({"task": task, "kind": kind, "text": text, "ref": reference, "new_argument": new_argument})
        })
        .collect();
    text(&json!({ "issues": list }).to_string())
}

fn with_usage(reply: Reply) -> Reply {
    reply.map(|mut r| {
        r.usage = Some(Usage::new(100, 10));
        r
    })
}

/// Resposta final do Arquiteto com o bloco das decisões: `(id, decisão, reason_kind, motivo)`.
fn decided(list: &[(&str, &str, &str, &str)]) -> Reply {
    let items: Vec<serde_json::Value> = list
        .iter()
        .map(|(id, decision, kind, reason)| json!({"id": id, "decision": decision, "reason_kind": kind, "reason": reason}))
        .collect();
    text(&format!("Respondi aos apontamentos.\n```nants-plan-decisions\n{}\n```\n", json!({ "decisions": items })))
}

fn write_task_reply(call: &str, id: &str, content: &str) -> Reply {
    let args = json!({"path": format!(".sdd/specs/{SPEC}/tasks/{id}.md"), "content": content});
    crate::agents::architect::testing::calls(&[(call, "write_sdd_artifacts", &args.to_string())])
}

fn chat_lines(root: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(root.join(crate::agents::architect::CHAT_FILE_REL))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect()
}

/// Pedidos internos do servidor ao Arquiteto (as rodadas), gravados na conversa.
fn internal_turns(root: &Path) -> Vec<String> {
    chat_lines(root)
        .into_iter()
        .filter(|v| v["internal"] == true && v["role"] == "user")
        .filter_map(|v| v["text"].as_str().map(str::to_string))
        .collect()
}

/// Linhas do sistema com `code` (a linha de cada rodada).
fn round_lines(root: &Path) -> Vec<serde_json::Value> {
    chat_lines(root).into_iter().filter(|v| v["role"] == "system" && v["notice"]["code"].is_string()).collect()
}

fn consensus(root: &Path) -> ConsensusView {
    view(root, SPEC, &SecretSet::default()).and_then(|v| v.consensus).expect("vista do consenso")
}

/// Consenso na 1ª rodada: o Arquiteto aceita e ajusta, o Revisor revisa a versão nova com a memória
/// e não aponta mais nada. A resposta do Arquiteto fica fora da Conversa; a linha da rodada entra.
#[tokio::test]
async fn consensus_in_the_first_round() {
    let root = plan_root("consensus-1");
    let reviewer = Scripted::new(vec![
        with_usage(review(&[("T01", "untestable", "Critério vago.", None, false)])),
        with_usage(review(&[])),
    ]);
    let adjusted = EXAMPLE_TASK_MD.replace("Critério verificável.", "GET /health responde 200.");
    let architect = Architect::new(vec![vec![
        write_task_reply("a1", "T01", &adjusted),
        with_usage(decided(&[("P1", DECISION_ACCEPT, "agree", "Critério reescrito.")])),
    ]]);
    let st = consensus_state(&root, reviewer.clone(), architect.clone());
    assert_eq!(run(&st, &root).await, 1);

    let turns = internal_turns(&root);
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert!(turns[0].starts_with(PLAN_CONSENSUS_INTRO), "{}", turns[0]);
    let asked = architect.requests()[0].messages.last().map(|m| m.content.clone()).unwrap_or_default();
    assert_eq!(asked, turns[0], "o pedido gravado é o que o Arquiteto recebe");
    assert!(turns[0].contains("Rodada 1 de 3") && turns[0].contains("- P1 (T01, untestable): Critério vago."), "{}", turns[0]);
    assert!(turns[0].contains("nants-plan-decisions") && turns[0].contains("unverifiable_in_sandbox"), "o formato vai no pedido");
    assert!(chat_lines(&root).iter().all(|l| !l["text"].as_str().unwrap_or_default().contains("Fix these points")));

    // A resposta da rodada fica fora da Conversa e sem o bloco.
    let replies: Vec<_> = chat_lines(&root).into_iter().filter(|l| l["role"] == "architect").collect();
    assert!(replies.iter().all(|l| l["internal"] == true), "{replies:?}");
    assert!(replies.iter().all(|l| !l["text"].as_str().unwrap_or_default().contains("nants-plan-decisions")));
    assert!(crate::server::chat::read_visible_history(&root).iter().all(|t| t.role != crate::agents::architect::TurnRole::Architect));
    let lines = round_lines(&root);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["notice"]["code"], ROUND_CODE);
    assert_eq!(lines[0]["notice"]["params"], json!({"round": 1, "issues": 1, "accepted": 1, "kept": 0}));
    assert_eq!(lines[0]["text"], "Revisão do plano, rodada 1: o Revisor apontou 1; o Arquiteto ajustou 1 e manteve 0.");

    // O Revisor revisou a versão nova com a memória da rodada anterior.
    let reqs = reviewer.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].messages.len(), 3, "rodada 0 sem memória");
    let memory = &reqs[1].messages[2].content;
    assert!(memory.contains("rodada 1 de 3") && memory.contains("P1 (T01, untestable): Critério vago. | Arquiteto: accept [agree] Critério reescrito."), "{memory}");
    assert!(memory.contains("O QUE MUDOU") && memory.contains("Tarefas novas: nenhuma"), "{memory}");

    let rec = record(&root);
    assert_eq!(rec.outcome.as_deref(), Some(OUTCOME_CONSENSUS));
    assert!(rec.issues.is_empty());
    assert_eq!(rec.rounds.len(), 2);
    assert_eq!(rec.rounds[0].issues[0].id.as_deref(), Some("P1"));
    assert_eq!(rec.rounds[1].decisions[0].decision, DECISION_ACCEPT);
    assert_eq!(rec.rounds[1].accept_rate, Some(100));
    assert_eq!((rec.rounds[1].tasks, rec.rounds[1].criteria), (1, 1));
    let view = consensus(&root);
    assert_eq!((view.outcome.as_deref(), view.adjusted, view.kept, view.round), (Some(OUTCOME_CONSENSUS), 1, 0, 1));

    // Custo: as chamadas do ciclo levam a etapa própria e a rodada.
    let usage = fs::read_to_string(root.join(".sdd/specs/.usage.jsonl")).expect("uso");
    let lines: Vec<serde_json::Value> = usage.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let staged = |agent: &str, round: u64| lines.iter().any(|l| l["agent"] == agent && l["stage"] == "plan_consensus" && l["round"] == round);
    assert!(staged("reviewer", 0) && staged("reviewer", 1) && staged("architect", 1), "{usage}");
    let _ = fs::remove_dir_all(&root);
}

/// Recusa respeitada: o Arquiteto mantém o plano com motivo; o Revisor revisa de novo (mesmo sem
/// mudança no plano), vê a recusa e não repete o ponto.
#[tokio::test]
async fn rejection_is_respected() {
    let root = plan_root("consensus-reject");
    let reviewer = Scripted::new(vec![review(&[("T01", "scope", "Precisa de arquivos em docs/.", None, false)]), review(&[])]);
    let architect = Architect::new(vec![vec![decided(&[("P1", DECISION_REJECT, "not_worth_cost", "A tarefa não escreve em docs/.")])]]);
    let st = consensus_state(&root, reviewer.clone(), architect);
    assert_eq!(run(&st, &root).await, 1);
    let reqs = reviewer.requests();
    assert_eq!(reqs.len(), 2, "revisa de novo depois da recusa");
    assert!(reqs[1].messages[2].content.contains("Arquiteto: reject [not_worth_cost] A tarefa não escreve em docs/."));
    // F-01: o turno não alterou o plano; nada antigo volta como mudança.
    assert!(reqs[1].messages[2].content.contains("O QUE MUDOU NO PLANO DESDE A RODADA ANTERIOR (DADO):\n- (nada registrado)"), "{}", reqs[1].messages[2].content);
    let rec = record(&root);
    assert_eq!(rec.outcome.as_deref(), Some(OUTCOME_CONSENSUS));
    let view = consensus(&root);
    assert_eq!((view.adjusted, view.kept), (0, 1));
    assert_eq!(view.kept_points.len(), 1);
    assert_eq!(view.kept_points[0].reason.as_deref(), Some("A tarefa não escreve em docs/."));
    assert_eq!(view.kept_points[0].reason_kind.as_deref(), Some("not_worth_cost"));
    assert_eq!(round_lines(&root)[0]["notice"]["params"]["kept"], 1);
    let _ = fs::remove_dir_all(&root);
}

/// Um recusado que volta sem argumento novo vira disputa e não volta ao Arquiteto.
#[tokio::test]
async fn repeated_rejection_becomes_a_dispute() {
    let root = plan_root("consensus-dispute");
    let issue = ("T01", "untestable", "O teste confere strings literais do README.", None, false);
    let reviewer = Scripted::new(vec![
        review(&[issue]),
        review(&[("T01", "untestable", "Continua conferindo texto literal do README.", Some("P1"), false)]),
    ]);
    let architect = Architect::new(vec![vec![decided(&[("P1", DECISION_REJECT, "other", "O texto do README é o entregável.")])]]);
    let st = consensus_state(&root, reviewer.clone(), architect);
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(internal_turns(&root).len(), 1, "a disputa não volta ao Arquiteto");
    let rec = record(&root);
    assert_eq!(rec.outcome.as_deref(), Some(OUTCOME_DISPUTED));
    assert!(rec.issues[0].disputed && rec.issues[0].id.as_deref() == Some("P1"));
    let view = consensus(&root);
    assert_eq!(view.disputed.len(), 1);
    assert_eq!(view.disputed[0].text, "Continua conferindo texto literal do README.");
    assert_eq!(view.disputed[0].reason.as_deref(), Some("O texto do README é o entregável."));
    assert!(view.open.is_empty());
    // F-01: a disputa conta só como em aberto.
    assert_eq!((view.total, view.adjusted, view.kept), (1, 0, 0));
    assert_sums(&view);
    let _ = fs::remove_dir_all(&root);
}

/// Teto: no máximo 3 turnos e 4 revisões; o que resta chega ao usuário em aberto.
#[tokio::test]
async fn cycle_stops_at_three_rounds() {
    let root = plan_root("consensus-cap");
    let reviewer = Scripted::new(vec![
        review(&[("T01", "scope", "Ponto alfa sobre login.", None, false)]),
        review(&[("T01", "contradiction", "Ponto beta sobre cache.", None, false)]),
        review(&[("T01", "untestable", "Ponto gama sobre fila.", None, false)]),
        review(&[("T01", "missing_dependency", "Ponto delta sobre banco.", None, false)]),
    ]);
    let architect = Architect::new(vec![
        vec![decided(&[("P1", DECISION_ACCEPT, "agree", "ok")])],
        vec![decided(&[("P2", DECISION_ACCEPT, "agree", "ok")])],
        vec![decided(&[("P3", DECISION_ACCEPT, "agree", "ok")])],
        vec![decided(&[("P4", DECISION_ACCEPT, "agree", "ok")])],
    ]);
    let st = consensus_state(&root, reviewer.clone(), architect);
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(internal_turns(&root).len(), MAX_CONSENSUS_ROUNDS as usize);
    assert_eq!(reviewer.requests().len(), 4);
    let rec = record(&root);
    assert_eq!(rec.outcome.as_deref(), Some(OUTCOME_OPEN));
    assert_eq!(rec.rounds.len(), 4);
    let view = consensus(&root);
    assert_eq!((view.round, view.open.len()), (3, 1));
    assert_eq!(view.open[0].id, "P4");
    assert_eq!((view.total, view.adjusted, view.kept), (4, 3, 0));
    assert_sums(&view);
    assert!(view.reopened.is_empty());
    assert_eq!(round_lines(&root).len(), 3);
    let _ = fs::remove_dir_all(&root);
}

/// Spec 128 T05 (critério 11): um recusado que o Revisor reabre com argumento novo na última
/// revisão, com o ciclo parado no teto, vem em `reopened` com o texto novo e o motivo da recusa;
/// o desfecho é `open` com a lista `open` vazia, e o ponto não aparece como mantido.
#[tokio::test]
async fn point_reopened_at_the_cap_is_exposed_with_both_positions() {
    let root = plan_root("consensus-reopened");
    let reviewer = Scripted::new(vec![
        review(&[("T01", "scope", "Ponto alfa sobre login.", None, false)]),
        review(&[("T01", "contradiction", "Ponto beta sobre cache.", None, false)]),
        review(&[("T01", "untestable", "Ponto gama sobre fila.", None, false)]),
        review(&[("T01", "scope", "O login novo exige o teste de sessão: argumento novo.", Some("P1"), true)]),
    ]);
    let architect = Architect::new(vec![
        vec![decided(&[("P1", DECISION_REJECT, "not_worth_cost", "O login já é coberto.")])],
        vec![decided(&[("P2", DECISION_ACCEPT, "agree", "ok")])],
        vec![decided(&[("P3", DECISION_ACCEPT, "agree", "ok")])],
    ]);
    let st = consensus_state(&root, reviewer, architect);
    assert_eq!(run(&st, &root).await, 1);
    let rec = record(&root);
    assert_eq!(rec.outcome.as_deref(), Some(OUTCOME_OPEN));
    let view = consensus(&root);
    assert!(view.open.is_empty() && view.disputed.is_empty(), "{view:?}");
    assert!(view.kept_points.is_empty(), "{view:?}");
    assert_eq!(view.reopened.len(), 1, "{view:?}");
    let point = &view.reopened[0];
    assert_eq!(point.id, "P1");
    assert_eq!(point.text, "O login novo exige o teste de sessão: argumento novo.");
    assert_eq!((point.reason_kind.as_deref(), point.reason.as_deref()), (Some("not_worth_cost"), Some("O login já é coberto.")));
    // F-01: o reaberto conta só como em aberto, nunca também como mantido.
    assert_eq!((view.total, view.adjusted, view.kept), (3, 2, 0));
    assert_sums(&view);
    let _ = fs::remove_dir_all(&root);
}

/// O resumo fecha a conta: total = ajustados + mantidos + em aberto, reabertos e em disputa.
fn assert_sums(view: &ConsensusView) {
    let waiting = view.open.len() + view.reopened.len() + view.disputed.len();
    assert_eq!(view.total as usize, view.adjusted as usize + view.kept as usize + waiting, "{view:?}");
}

/// F-01: um ponto recusado duas vezes (o Revisor o reabre com argumento novo e o Arquiteto recusa
/// de novo) conta uma vez como mantido; o aceito conta uma vez como ajustado.
#[tokio::test]
async fn summary_counts_each_point_once_by_its_final_decision() {
    let root = plan_root("consensus-twice");
    let reviewer = Scripted::new(vec![
        review(&[("T01", "scope", "Ponto alfa sobre login.", None, false), ("T01", "contradiction", "Ponto beta sobre cache.", None, false)]),
        review(&[("T01", "scope", "O login novo exige o teste de sessão: argumento novo.", Some("P1"), true)]),
        review(&[]),
    ]);
    let architect = Architect::new(vec![
        vec![decided(&[("P1", DECISION_REJECT, "not_worth_cost", "O login já é coberto."), ("P2", DECISION_ACCEPT, "agree", "ok")])],
        vec![decided(&[("P1", DECISION_REJECT, "duplicate", "O teste de sessão já está na T01.")])],
    ]);
    let st = consensus_state(&root, reviewer, architect);
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(record(&root).outcome.as_deref(), Some(OUTCOME_CONSENSUS));
    let view = consensus(&root);
    assert_eq!((view.total, view.adjusted, view.kept), (2, 1, 1), "{view:?}");
    assert_eq!(view.kept_points.len(), 1);
    assert_eq!(view.kept_points[0].reason_kind.as_deref(), Some("duplicate"));
    assert_sums(&view);
    let _ = fs::remove_dir_all(&root);
}

/// O turno que termina numa pergunta ao usuário para o ciclo e a resposta aparece na Conversa.
#[tokio::test]
async fn question_to_the_user_stops_the_cycle() {
    let root = plan_root("consensus-ask");
    let reviewer = Scripted::new(vec![review(&[(
        "T01",
        "uncovered_criterion",
        "Falta decidir quem cria o grupo.",
        None,
        false,
    )])]);
    let choices = json!({"questions": [{"id": "q1", "text": "Quem cria o grupo?", "options": ["Qualquer membro", "Só o dono"]}]});
    let ask = text(&format!("Preciso da sua decisão.\n```nants-choices\n{choices}\n```\n"));
    let architect = Architect::new(vec![vec![ask]]);
    let st = consensus_state(&root, reviewer.clone(), architect);
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(reviewer.requests().len(), 1, "espera o usuário");
    assert_eq!(record(&root).outcome.as_deref(), Some(OUTCOME_AWAITING_USER));
    let reply = chat_lines(&root).into_iter().find(|l| l["role"] == "architect").expect("resposta");
    assert!(reply.get("internal").is_none_or(|v| v != true), "a pergunta aparece: {reply}");
    assert!(reply["text"].as_str().unwrap_or_default().contains("nants-choices"));
    assert_eq!(round_lines(&root)[0]["notice"]["code"], ROUND_OPEN_CODE, "sem decisão, o ponto fica em aberto");
    let _ = fs::remove_dir_all(&root);
}

/// F-02: o turno que recusa P1 com motivo e pergunta sobre P2 expõe a decisão e o motivo de P1
/// (mantido, nunca "sem resposta"); P2 segue em aberto, sem decisão.
#[tokio::test]
async fn rejected_point_keeps_its_position_when_the_cycle_waits_for_the_user() {
    let root = plan_root("consensus-ask-kept");
    let reviewer = Scripted::new(vec![review(&[
        ("T01", "scope", "Precisa de arquivos em docs/.", None, false),
        ("T01", "uncovered_criterion", "Falta decidir quem cria o grupo.", None, false),
    ])]);
    let choices = json!({"questions": [{"id": "q1", "text": "Quem cria o grupo?", "options": ["Qualquer membro", "Só o dono"]}]});
    let block = json!({"decisions": [{"id": "P1", "decision": "reject", "reason_kind": "not_worth_cost", "reason": "A tarefa não escreve em docs/."}]});
    let ask = text(&format!("Preciso da sua decisão.\n```nants-choices\n{choices}\n```\n```nants-plan-decisions\n{block}\n```\n"));
    let st = consensus_state(&root, reviewer, Architect::new(vec![vec![ask]]));
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(record(&root).outcome.as_deref(), Some(OUTCOME_AWAITING_USER));
    let view = consensus(&root);
    assert_eq!(view.kept_points.len(), 1, "{view:?}");
    let kept = &view.kept_points[0];
    assert_eq!((kept.id.as_str(), kept.decision.as_deref()), ("P1", Some(DECISION_REJECT)));
    assert_eq!((kept.reason_kind.as_deref(), kept.reason.as_deref()), (Some("not_worth_cost"), Some("A tarefa não escreve em docs/.")));
    assert_eq!(view.open.len(), 1);
    assert_eq!((view.open[0].id.as_str(), view.open[0].decision.as_deref()), ("P2", None));
    let _ = fs::remove_dir_all(&root);
}

/// Falha do provedor no turno do Arquiteto: o ciclo termina como indisponível, sem laço.
#[tokio::test]
async fn provider_failure_ends_the_cycle_as_unavailable() {
    let root = plan_root("consensus-fail");
    let reviewer = Scripted::new(vec![review(&[("T01", "scope", "Fora do escopo.", None, false)])]);
    let failure = Err(crate::agents::LlmError::Http { status: 400, body_snippet: "x".into(), code: None });
    let architect = Architect::new(vec![vec![failure]]);
    let st = consensus_state(&root, reviewer.clone(), architect);
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(reviewer.requests().len(), 1);
    assert_eq!(internal_turns(&root).len(), 1);
    let rec = record(&root);
    assert_eq!(rec.outcome.as_deref(), Some(OUTCOME_UNAVAILABLE));
    assert_eq!(rec.status, STATUS_OK, "a revisão em si valeu; os pontos chegam ao usuário");
    assert_eq!(consensus(&root).open.len(), 1);
    let _ = fs::remove_dir_all(&root);
}

/// O contador só zera com uma mensagem do usuário: depois do teto, outra versão do plano (sem
/// mensagem do usuário) é revisada sem abrir rodada; com a mensagem, o ciclo recomeça.
#[tokio::test]
async fn round_counter_resets_only_with_a_user_message() {
    let root = plan_root("consensus-reset");
    let texts = [
        "Login exige arquivos em auth/.",
        "Cache grava em tmp/ fora do escopo.",
        "Fila de mensagens usa worker/.",
        "Migração do banco mexe em db/.",
        "Busca textual precisa de search/.",
        "Paginação altera web/list.",
    ];
    let point = |n: usize| review(&[("T01", "scope", texts[n], None, false)]);
    let reviewer = Scripted::new(vec![point(0), point(1), point(2), point(3), point(4), point(5), review(&[])]);
    let accept = |id: &str| vec![decided(&[(id, DECISION_ACCEPT, "agree", "ok")])];
    let architect = Architect::new(vec![accept("P1"), accept("P2"), accept("P3"), accept("P6")]);
    let st = consensus_state(&root, reviewer.clone(), architect);
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(internal_turns(&root).len(), 3);
    assert_eq!(record(&root).outcome.as_deref(), Some(OUTCOME_OPEN));

    // Outra versão sem mensagem do usuário (ex.: "continuar"): revisa, mas não abre rodada.
    write_plan(&root, "AWAITING_APPROVAL", &EXAMPLE_TASK_MD.replace("O que implementar.", "Versão dois."));
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(internal_turns(&root).len(), 3, "o contador não zerou");
    assert_eq!(record(&root).outcome.as_deref(), Some(OUTCOME_OPEN));

    // Mensagem do usuário que altera o plano: ciclo novo.
    drop(st.plan_consensus().begin_user_turn(PROJECT));
    write_plan(&root, "AWAITING_APPROVAL", &EXAMPLE_TASK_MD.replace("O que implementar.", "Versão três."));
    assert_eq!(run(&st, &root).await, 1);
    assert_eq!(internal_turns(&root).len(), 4, "o contador zerou");
    let rec = record(&root);
    assert_eq!(rec.outcome.as_deref(), Some(OUTCOME_CONSENSUS));
    assert_eq!(rec.rounds.len(), 2, "rodadas do ciclo novo");
    assert_eq!(rec.rounds[0].issues[0].id.as_deref(), Some("P6"), "ids estáveis na spec");
    let _ = fs::remove_dir_all(&root);
}

/// Registro antigo (spec 126 T03, com `split_requested`) continua lido, sem a vista do consenso.
#[tokio::test]
async fn old_record_is_still_read() {
    let root = plan_root("consensus-old");
    let old = json!({
        "signature": plan_signature(&root, SPEC).expect("sig"),
        "at": "2026-10-25T00:00:00Z",
        "status": "ok",
        "issues": [{"task": "T01", "kind": "too_large", "text": "Grande."}],
        "split_requested": true
    });
    fs::write(root.join(format!(".sdd/specs/{SPEC}/{PLAN_REVIEW_FILE}")), old.to_string()).expect("registro");
    let rec = record(&root);
    assert_eq!((rec.issues.len(), rec.rounds.len(), rec.outcome.clone()), (1, 0, None));
    let shown = view(&root, SPEC, &SecretSet::default()).expect("vista");
    assert_eq!(shown.status, STATUS_OK);
    assert_eq!(shown.issues[0].id, None);
    assert!(shown.consensus.is_none());
    let json = serde_json::to_string(&rec).expect("json");
    assert!(!json.contains("split_requested") && !json.contains("rounds"), "{json}");
    let _ = fs::remove_dir_all(&root);
}

/// C-04: tarefa criada no turno que já nasce grande demais volta ao Arquiteto na mesma rodada, uma
/// única vez e sem contar como rodada.
#[tokio::test]
async fn new_task_born_too_large_goes_back_once_in_the_same_round() {
    let root = plan_root("consensus-size");
    let reviewer = Scripted::new(vec![review(&[("T01", "too_large", "Mistura servidor e cliente.", None, false)]), review(&[])]);
    let architect = Architect::new(vec![
        vec![
            write_task_reply("a1", "T02", &big_task(7).replace("id: \"T01\"", "id: \"T02\"")),
            decided(&[("P1", DECISION_ACCEPT, "agree", "Dividi em T01 e T02.")]),
        ],
        vec![
            write_task_reply("a2", "T02", &big_task(3).replace("id: \"T01\"", "id: \"T02\"")),
            decided(&[("P2", DECISION_ACCEPT, "agree", "T02 enxuta.")]),
        ],
    ]);
    let st = consensus_state(&root, reviewer.clone(), architect);
    assert_eq!(run(&st, &root).await, 1);
    let turns = internal_turns(&root);
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert!(turns[1].contains("Rodada 1 de 3") && turns[1].contains("Conferência de tamanho"), "{}", turns[1]);
    assert!(turns[1].contains("P2 (T02, too_large): 7 critérios"), "{}", turns[1]);
    let rec = record(&root);
    assert_eq!(rec.outcome.as_deref(), Some(OUTCOME_CONSENSUS));
    assert_eq!(rec.rounds.len(), 2, "a volta não conta como rodada");
    assert_eq!(rec.rounds[1].size_issues.len(), 1);
    assert_eq!(rec.rounds[1].decisions.len(), 2);
    assert_eq!(round_lines(&root).len(), 1, "uma linha por rodada");
    let _ = fs::remove_dir_all(&root);
}

/// C-11: a mensagem do usuário durante o turno interno espera por ele e roda normalmente; nenhuma
/// rodada nova é aberta e o contador zera (a versão final é revisada num ciclo novo).
#[tokio::test]
async fn user_message_during_the_cycle_runs_and_resets_the_counter() {
    let root = plan_root("consensus-user");
    let reviewer = Scripted::new(vec![
        review(&[("T01", "untestable", "Critério vago.", None, false)]),
        review(&[]),
    ]);
    let (reached, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let adjusted = EXAMPLE_TASK_MD.replace("Critério verificável.", "GET /health responde 200.");
    let architect = Architect::build(
        vec![vec![write_task_reply("a1", "T01", &adjusted), decided(&[("P1", DECISION_ACCEPT, "agree", "ok")])]],
        vec![vec![text("Certo, anotado.")]],
        Some((Arc::clone(&reached), Arc::clone(&release))),
    );
    let st = consensus_state(&root, reviewer.clone(), architect.clone());
    let cycle = {
        let st = st.clone();
        let root = root.clone();
        tokio::spawn(async move { run(&st, &root).await })
    };
    reached.notified().await;
    // Arquiteto falso ocupado no turno interno: a mensagem do usuário espera a vez.
    let user = {
        let st = st.clone();
        let body = json!({ "message": USER_MESSAGE }).to_string();
        tokio::spawn(async move {
            crate::server::chat::post_chat(axum::extract::State(st), axum::extract::Path(PROJECT.to_string()), body.into()).await
        })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!user.is_finished(), "espera o turno interno");
    release.notify_one();
    let answered = user.await.expect("task").expect("turno do usuário");
    assert_eq!(answered.status(), axum::http::StatusCode::OK);
    assert_eq!(cycle.await.expect("ciclo"), 1);
    // A revisão da versão final roda num ciclo novo, disparada pelo fim do turno do usuário.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let rec = loop {
        let rec = read_record(&root, SPEC).filter(|r| r.signature == plan_signature(&root, SPEC).unwrap_or_default() && r.outcome.is_some());
        if let Some(rec) = rec.filter(|_| !is_running(&root, SPEC)) {
            break rec;
        }
        assert!(std::time::Instant::now() < deadline, "a revisão da versão final não rodou");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(internal_turns(&root).len(), 1, "nenhuma rodada nova depois da mensagem do usuário");
    assert_eq!(reviewer.requests().len(), 2);
    assert_eq!(rec.rounds.len(), 1, "o contador zerou: ciclo novo só com a rodada 0");
    assert_eq!(rec.outcome.as_deref(), Some(OUTCOME_CONSENSUS));
    let user_line = chat_lines(&root).iter().position(|l| l["role"] == "user" && l["text"] == USER_MESSAGE).expect("mensagem do usuário");
    let internal_line = chat_lines(&root).iter().position(|l| l["role"] == "user" && l["internal"] == true).expect("pedido interno");
    assert!(internal_line < user_line, "a mensagem do usuário roda depois do turno interno");
    let _ = fs::remove_dir_all(&root);
}
/// Spec 127 T01: a spec em aprovação sem nenhuma tarefa (o turno que escrevia o plano caiu) não é
/// revisada e não fica "demorando": a vista diz `incomplete` logo, com ou sem Revisor e com
/// qualquer relógio. Gravada a primeira tarefa, volta o fluxo de sempre.
#[tokio::test]
async fn plan_without_tasks_is_incomplete_and_never_reviewed() {
    let root = tmp("incomplete");
    write_plan(&root, "AWAITING_APPROVAL", EXAMPLE_TASK_MD);
    fs::remove_file(root.join(format!(".sdd/specs/{SPEC}/tasks/T01.md"))).expect("sem tarefas");
    fs::create_dir_all(root.join(".git/info")).expect("git");
    let scripted = Scripted::new(vec![text(r#"{"issues": []}"#)]);
    let st = state(&root, true, scripted.clone());
    assert_eq!(run(&st, &root).await, 0, "plano sem tarefas não dispara a revisão");
    assert!(scripted.requests().is_empty());
    assert!(read_record(&root, SPEC).is_none());
    let secrets = SecretSet::default();
    let now = SystemTime::now();
    for reviewer in [true, false] {
        for later in [0u64, 181, 3600] {
            let view = view_at(&root, SPEC, &secrets, reviewer, now + Duration::from_secs(later)).expect("incomplete");
            assert_eq!(view.status, STATUS_INCOMPLETE, "revisor {reviewer}, +{later} s");
            assert!(view.issues.is_empty() && view.at.is_none());
        }
    }
    assert_eq!(view(&root, SPEC, &secrets).expect("incomplete").status, STATUS_INCOMPLETE);

    fs::write(root.join(format!(".sdd/specs/{SPEC}/tasks/T01.md")), EXAMPLE_TASK_MD).expect("tarefa");
    assert_eq!(view_at(&root, SPEC, &secrets, true, SystemTime::now()).expect("pending").status, STATUS_PENDING);
    assert_eq!(run(&st, &root).await, 1, "com tarefa, a revisão volta a rodar");
    assert_eq!(record(&root).status, STATUS_OK);
}

/// Spec 128 T04: um ciclo pelo `depends_on` não tem ordem de execução e vira um apontamento
/// `contradiction` do servidor, no idioma do projeto, que nunca perde a vaga para os do modelo.
#[test]
fn a_dependency_cycle_becomes_a_contradiction_issue() {
    use crate::i18n::Lang;
    let root = tmp("cycle");
    write_plan(&root, "AWAITING_APPROVAL", &EXAMPLE_TASK_MD.replace("depends_on: []", "depends_on: [\"T02\"]"));
    let t02 = EXAMPLE_TASK_MD.replace("id: \"T01\"", "id: \"T02\"").replace("depends_on: []", "depends_on: [\"T01\"]");
    fs::write(root.join(format!(".sdd/specs/{SPEC}/tasks/T02.md")), &t02).expect("T02");
    let t03 = EXAMPLE_TASK_MD.replace("id: \"T01\"", "id: \"T03\"").replace("depends_on: []", "depends_on: [\"T02\"]");
    fs::write(root.join(format!(".sdd/specs/{SPEC}/tasks/T03.md")), &t03).expect("T03");
    let cycle = plan_cycle(&root, SPEC);
    assert_eq!(cycle, ["T01", "T02"], "só o ciclo, sem a T03 que depende dele");
    let pt = with_cycle_issue(Vec::new(), &cycle, Lang::PtBr);
    assert_eq!(pt.len(), 1);
    assert_eq!((pt[0].task.as_deref(), pt[0].kind.as_str()), (Some("T01"), "contradiction"));
    assert!(pt[0].text.starts_with("As tarefas T01, T02 dependem umas das outras em ciclo"), "{}", pt[0].text);
    let en = with_cycle_issue(Vec::new(), &cycle, Lang::En);
    assert!(en[0].text.starts_with("Tasks T01, T02 depend on each other in a cycle"), "{}", en[0].text);
    // Lista cheia do modelo: o do servidor entra e um do modelo cede a vaga.
    let full: Vec<PlanIssue> = (0..MAX_ISSUES).map(|i| PlanIssue::new(None, "scope", &format!("p{i}"))).collect();
    let out = with_cycle_issue(full, &cycle, Lang::PtBr);
    assert_eq!(out.len(), MAX_ISSUES);
    assert_eq!(out.last().map(|i| i.kind.as_str()), Some(CYCLE_KIND));
    // Sem ciclo: nada a acrescentar.
    fs::write(root.join(format!(".sdd/specs/{SPEC}/tasks/T01.md")), EXAMPLE_TASK_MD).expect("T01");
    assert!(plan_cycle(&root, SPEC).is_empty());
    assert!(with_cycle_issue(Vec::new(), &[], Lang::PtBr).is_empty());
    let _ = fs::remove_dir_all(&root);
}

/// Spec 128 T04: na revisão, o ciclo chega gravado como apontamento, mesmo sem o modelo apontar.
#[tokio::test]
async fn the_review_records_the_dependency_cycle() {
    let root = tmp("cycle-review");
    fs::create_dir_all(root.join(".git/info")).expect("git");
    write_plan(&root, "AWAITING_APPROVAL", &EXAMPLE_TASK_MD.replace("depends_on: []", "depends_on: [\"T02\"]"));
    let t02 = EXAMPLE_TASK_MD.replace("id: \"T01\"", "id: \"T02\"").replace("depends_on: []", "depends_on: [\"T01\"]");
    fs::write(root.join(format!(".sdd/specs/{SPEC}/tasks/T02.md")), &t02).expect("T02");
    let st = state(&root, true, Scripted::new(vec![text(r#"{"issues": []}"#)]));
    assert_eq!(run(&st, &root).await, 1);
    let rec = record(&root);
    assert!(rec.issues.iter().any(|i| i.kind == CYCLE_KIND && i.text.contains("T01, T02")), "{:?}", rec.issues);
    let _ = fs::remove_dir_all(&root);
}
