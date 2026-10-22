//! Testes de `GET /api/attention`. Incluídos por `attention.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{env, process};

use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::agents::runner::{OPTIONS_CLOSE, OPTIONS_OPEN, RECOVERY_PREFIX};
use crate::config::{Config, Project};
use crate::harness::tools::{ApprovalRequest, Approver, Role};
use crate::server::approvals::ProjectApprover;
use crate::server::events::SecretSet;
use crate::server::state::ArchitectTurn;
use crate::server::{router, AppState};

const HOST: &str = "127.0.0.1:3000";
const SECRET_COMMAND: &str = "make deploy-prod --token TEXTO-DO-AGENTE";
const OPTION_LABEL: &str = "RÓTULO-DO-MODELO";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("nants-attention-{}-{}", process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn spec_md(status: &str) -> String {
    format!(
        "---\nid: \"001-demo\"\ntitle: \"Demo\"\nstatus: \"{status}\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-19T10:00:00Z\"\n---\n\n## Contexto\nx\n"
    )
}

fn task_md(id: &str, status: &str, body: &str) -> String {
    format!(
        "---\nid: \"{id}\"\nspec_id: \"001-demo\"\ntitle: \"Tarefa {id}\"\nstatus: \"{status}\"\ndepends_on: []\nbranch: \"\"\nattempts: 1\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope:\n  - \"src/**\"\n---\n\n## Objetivo\nx\n{body}"
    )
}

/// Projeto SDD com a spec `001-demo` no status dado e as tarefas `(id, status, corpo)`.
fn sdd_project(root: &Path, spec_status: &str, tasks: &[(&str, &str, &str)]) {
    let spec_dir = root.join(".sdd/specs/001-demo");
    fs::create_dir_all(spec_dir.join("tasks")).expect("mkdir");
    fs::write(root.join(".sdd/settings.yaml"), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\n").expect("settings");
    fs::write(spec_dir.join("spec.md"), spec_md(spec_status)).expect("spec");
    for (id, status, body) in tasks {
        fs::write(spec_dir.join(format!("tasks/{id}.md")), task_md(id, status, body)).expect("task");
    }
}

/// Grava o histórico do chat com as linhas `(papel, texto)`.
fn chat(root: &Path, turns: &[(&str, &str)]) {
    fs::create_dir_all(root.join(".sdd/specs")).expect("mkdir");
    let lines: String = turns
        .iter()
        .map(|(role, text)| format!("{}\n", json!({"role": role, "text": text, "ts": 1})))
        .collect();
    fs::write(root.join(".sdd/specs/.chat.jsonl"), lines).expect("chat");
}

fn choices_text() -> String {
    let q = json!({"questions": [{"id": "q0", "text": "Qual banco?", "options": ["Postgres", "SQLite"]}]});
    format!("Preciso decidir.\n```nants-choices\n{q}\n```\n")
}

fn state_with(dir: &Path, ids: &[&str]) -> AppState {
    AppState::new(dir.join("config.toml"), config_with(dir, ids), 3000).with_data_dir(dir.join("data"))
}

/// Configuração com os projetos `ids` em `<dir>/<id>` (pastas criadas).
fn config_with(dir: &Path, ids: &[&str]) -> Config {
    let mut cfg = Config::default();
    for id in ids {
        let path = dir.join(id);
        fs::create_dir_all(&path).expect("mkdir projeto");
        cfg.projects.push(Project {
            id: (*id).into(),
            name: format!("Projeto {id}"),
            path: path.to_str().expect("utf-8").into(),
            created_at: "2026-10-19T00:00:00Z".into(),
        });
    }
    cfg
}

async fn attention(state: &AppState) -> (Value, String) {
    let r = Request::builder()
        .method(Method::GET)
        .uri("/api/attention")
        .header(header::HOST, HOST)
        .body(Body::empty())
        .expect("requisição");
    let resp: Response<Body> = router(state.clone()).oneshot(r).await.expect("resposta");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
    let text = String::from_utf8_lossy(&bytes).to_string();
    (serde_json::from_str(&text).expect("json"), text)
}

fn approver(state: &AppState, dir: &Path, id: &str) -> ProjectApprover {
    ProjectApprover::new(
        id.to_string(),
        dir.join(id),
        state.approvals().clone(),
        state.events().clone(),
        Arc::new(SecretSet::default()),
    )
}

async fn wait_pending(state: &AppState, id: &str, n: usize) {
    for _ in 0..500 {
        if state.approvals().list_pending(id).len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("pedido pendente não apareceu");
}

fn project<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["projects"].as_array().expect("projects").iter().find(|p| p["project_id"] == id)
}

#[tokio::test]
async fn lists_only_projects_with_items_and_each_kind_of_item() {
    let dir = temp_dir("kinds");
    let state = state_with(&dir, &["a", "b", "plan", "choices", "answered", "paused", "stalled", "done"]);
    // a: nada (sem .sdd). done: spec concluída, nada pendente.
    sdd_project(&dir.join("done"), "COMPLETED", &[("T01", "DONE", "")]);
    // plan: plano aguardando aprovação.
    sdd_project(&dir.join("plan"), "AWAITING_APPROVAL", &[("T01", "TODO", "")]);
    // choices: última resposta do Arquiteto com perguntas válidas.
    chat(&dir.join("choices"), &[("user", "quero um app"), ("architect", &choices_text())]);
    // answered: o usuário respondeu depois.
    chat(&dir.join("answered"), &[("architect", &choices_text()), ("user", "Qual banco? Postgres")]);
    // paused: tarefa BLOCKED com opções de escalada.
    let options = json!([
        {"id": "opt-a", "label": OPTION_LABEL, "kind": "guidance", "text": "faça x"},
        {"id": "opt-b", "label": OPTION_LABEL, "kind": "revise_task", "objective": "y"}
    ]);
    let analysis = format!("\n## {RECOVERY_PREFIX}2\n- Causa raiz: z\n{OPTIONS_OPEN}\n{options}\n{OPTIONS_CLOSE}\n");
    sdd_project(&dir.join("paused"), "IN_PROGRESS", &[("T01", "DONE", ""), ("T02", "BLOCKED", &analysis)]);
    // stalled: em execução sem runner, sem bloqueio e com tarefa por fazer.
    sdd_project(&dir.join("stalled"), "IN_PROGRESS", &[("T01", "DONE", ""), ("T02", "TODO", "")]);
    // b: permissão pendente (outro projeto que não o "ativo" da UI).
    let ap = approver(&state, &dir, "b");
    let waiting = tokio::spawn(async move {
        let req = ApprovalRequest {
            in_container: false,
            role: Role::Developer,
            task_id: Some("T01".into()),
            command: SECRET_COMMAND.into(),
        };
        ap.approve(&req).await
    });
    wait_pending(&state, "b", 1).await;

    let (body, text) = attention(&state).await;
    // Sem conexão nem agentes: a prontidão (spec 093 T02) marca o setup como incompleto.
    assert_eq!(body["setup_incomplete"], true);
    let ids: Vec<&str> = body["projects"].as_array().expect("projects").iter().filter_map(|p| p["project_id"].as_str()).collect();
    assert_eq!(ids, vec!["b", "plan", "choices", "paused", "stalled"], "{text}");

    let b = project(&body, "b").expect("b");
    assert_eq!(b["name"], "Projeto b");
    let perms = b["pending_permissions"].as_array().expect("lista");
    assert_eq!(perms.len(), 1);
    assert_eq!(perms[0]["agent"], "developer");
    assert_eq!(perms[0]["kind"], "command");
    assert_eq!(perms[0]["task_id"], "T01");
    assert_eq!(perms[0]["request_id"].as_str().map(str::len), Some(32));
    assert_eq!(b["pending_choices"], false);
    assert_eq!(b["plan_awaiting_approval"], false);
    assert_eq!(b["paused_or_stalled"], Value::Null);

    let plan = project(&body, "plan").expect("plan");
    assert_eq!(plan["plan_awaiting_approval"], true);
    assert_eq!(plan["pending_permissions"], json!([]));

    assert_eq!(project(&body, "choices").expect("choices")["pending_choices"], true);

    let paused = project(&body, "paused").expect("paused");
    assert_eq!(
        paused["paused_or_stalled"],
        json!({
            "kind": "paused", "spec_id": "001-demo", "task_id": "T02", "analysis": 2,
            "escalation_options": [{"id": "opt-a", "kind": "guidance"}, {"id": "opt-b", "kind": "revise_task"}]
        })
    );
    assert_eq!(
        project(&body, "stalled").expect("stalled")["paused_or_stalled"],
        json!({"kind": "stalled", "spec_id": "001-demo"})
    );

    // Só ids e rótulos fixos: nada do comando, do rótulo da opção nem do histórico.
    for remote in ["TEXTO-DO-AGENTE", "deploy-prod", OPTION_LABEL, "faça x", "Qual banco", "Preciso decidir"] {
        assert!(!text.contains(remote), "{remote} vazou: {text}");
    }

    state.approvals().clear_project("b", state.events());
    let _ = tokio::time::timeout(Duration::from_secs(5), waiting).await;
}

#[tokio::test]
async fn final_fix_confirmation_counts_once_as_a_permission() {
    let dir = temp_dir("final-fix");
    let state = state_with(&dir, &["p"]);
    sdd_project(&dir.join("p"), "IN_PROGRESS", &[("T01", "DONE", "")]);
    let ap = approver(&state, &dir, "p");
    let waiting = tokio::spawn(async move { ap.approve_final_fix("critério G2 não atendido: MOTIVO-REMOTO").await });
    wait_pending(&state, "p", 1).await;

    let (body, text) = attention(&state).await;
    let p = project(&body, "p").expect("p");
    let perms = p["pending_permissions"].as_array().expect("lista");
    assert_eq!(perms.len(), 1, "{text}");
    assert_eq!(perms[0]["kind"], "final_fix");
    assert_eq!(perms[0]["agent"], "architect");
    // Todas as tarefas DONE: nada de "parada" contado à parte.
    assert_eq!(p["paused_or_stalled"], Value::Null);
    assert!(!text.contains("MOTIVO-REMOTO"), "{text}");

    state.approvals().clear_project("p", state.events());
    let _ = tokio::time::timeout(Duration::from_secs(5), waiting).await;
}

#[tokio::test]
async fn running_project_has_no_plan_or_stall_and_open_turn_hides_choices() {
    let dir = temp_dir("running");
    let state = state_with(&dir, &["run", "turn"]);
    sdd_project(&dir.join("run"), "IN_PROGRESS", &[("T01", "TODO", "")]);
    let _guard = state.runs().try_start("run", "001-demo").expect("reserva");
    chat(&dir.join("turn"), &[("architect", &choices_text())]);
    let turn = ArchitectTurn { turn_id: "ab".into(), user_text: String::new(), started_at: 1 };
    let _scope = state.begin_architect_turn("turn", turn, false);

    let (body, text) = attention(&state).await;
    assert_eq!(body["projects"], json!([]), "{text}");
}

#[tokio::test]
async fn no_projects_gives_empty_list() {
    let dir = temp_dir("none");
    let state = state_with(&dir, &[]);
    let (body, _) = attention(&state).await;
    assert_eq!(body, json!({"setup_incomplete": true, "projects": []}));
}

#[test]
fn choices_pending_reads_the_visible_history() {
    let dir = temp_dir("visible");
    let root = dir.join("p");
    fs::create_dir_all(&root).expect("mkdir");
    assert!(!super::choices_pending(&root), "sem histórico");
    chat(&root, &[("architect", &choices_text())]);
    assert!(super::choices_pending(&root));
    chat(&root, &[("architect", "Só texto, sem perguntas.")]);
    assert!(!super::choices_pending(&root));
    chat(&root, &[("architect", "Oi\n```nants-choices\n{nope\n```")]);
    assert!(!super::choices_pending(&root), "bloco inválido");
}

/// Estado com o papel Revisor configurado (a revisão do plano passa a ter `pending`/`stale`).
fn state_with_reviewer(dir: &Path, ids: &[&str]) -> AppState {
    use crate::config::{AgentConfig, Connection, Effort, WireApi};
    let mut cfg = config_with(dir, ids);
    cfg.connections.push(Connection {
        id: "c".into(),
        name: "Conexão".into(),
        base_url: "https://api.example.com/v1".into(),
        api_key: "sk-teste-atencao-0123456789".into(),
        wire_api: WireApi::default(),
    });
    cfg.agents.reviewer = AgentConfig { connection_id: "c".into(), model: "rev".into(), effort: Effort::Medium, max_steps: None };
    AppState::new(dir.join("config.toml"), cfg, 3000).with_data_dir(dir.join("data"))
}

/// Recua o mtime do plano para fora da janela de espera da revisão.
fn age_plan(root: &Path) {
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    let spec_dir = root.join(".sdd/specs/001-demo");
    for path in [spec_dir.join("spec.md"), spec_dir.join("tasks/T01.md")] {
        fs::File::options().write(true).open(&path).expect("abrir").set_modified(old).expect("mtime");
    }
}

#[tokio::test]
async fn plan_counts_only_when_it_can_be_approved() {
    let dir = temp_dir("plan-approvable");
    let state = state_with_reviewer(&dir, &["pending", "stale", "reviewed", "turn"]);
    // Revisão `pending`: plano recém-alterado, sem resultado; o Aprovar fica desabilitado.
    sdd_project(&dir.join("pending"), "AWAITING_APPROVAL", &[("T01", "TODO", "")]);
    // `stale`: a espera passou; o cliente libera a aprovação com aviso.
    sdd_project(&dir.join("stale"), "AWAITING_APPROVAL", &[("T01", "TODO", "")]);
    age_plan(&dir.join("stale"));
    // `ok`: resultado gravado da versão atual (mesmo com o plano recém-alterado).
    let reviewed = dir.join("reviewed");
    sdd_project(&reviewed, "AWAITING_APPROVAL", &[("T01", "TODO", "")]);
    let signature = crate::server::plan_review::plan_signature(&reviewed, "001-demo").expect("assinatura");
    let record = json!({"signature": signature, "at": "2026-10-19T10:00:00Z", "status": "ok", "issues": []});
    fs::write(reviewed.join(".sdd/specs/001-demo/plan-review.json"), record.to_string()).expect("registro");
    // Turno do Arquiteto em andamento: o plano ainda pode mudar.
    sdd_project(&dir.join("turn"), "AWAITING_APPROVAL", &[("T01", "TODO", "")]);
    age_plan(&dir.join("turn"));
    let turn = ArchitectTurn { turn_id: "cd".into(), user_text: String::new(), started_at: 1 };
    let _scope = state.begin_architect_turn("turn", turn, false);

    let (body, text) = attention(&state).await;
    let ids: Vec<&str> = body["projects"].as_array().expect("projects").iter().filter_map(|p| p["project_id"].as_str()).collect();
    assert_eq!(ids, vec!["stale", "reviewed"], "{text}");
    for id in ["stale", "reviewed"] {
        assert_eq!(project(&body, id).expect("projeto")["plan_awaiting_approval"], true, "{id}");
    }
}

#[tokio::test]
async fn without_reviewer_fresh_plan_counts() {
    let dir = temp_dir("plan-no-reviewer");
    let state = state_with(&dir, &["p"]);
    sdd_project(&dir.join("p"), "AWAITING_APPROVAL", &[("T01", "TODO", "")]);
    let (body, text) = attention(&state).await;
    assert_eq!(project(&body, "p").expect("p")["plan_awaiting_approval"], true, "{text}");
}

#[test]
fn final_fix_kind_requires_an_environment_request() {
    use crate::server::approvals::{PendingView, FINAL_FIX_COMMAND};
    let view = |kind: Option<&'static str>| PendingView {
        request_id: "a".repeat(32),
        agent: "architect",
        command: FINAL_FIX_COMMAND.to_string(),
        task_id: None,
        always_allowed: false,
        always_denied_reason: None,
        kind,
        reason: None,
        diff: None,
    };
    assert_eq!(super::permission_item(view(Some("environment"))).kind, "final_fix");
    assert_eq!(super::permission_item(view(None)).kind, "command", "comando com o mesmo texto não é a correção final");
}

#[tokio::test]
async fn unparsable_settings_still_follow_state() {
    // `build_state_at` lista as specs mesmo com `settings.yaml` inválido; o sino segue o `/state`.
    let dir = temp_dir("bad-settings");
    let state = state_with(&dir, &["p"]);
    sdd_project(&dir.join("p"), "AWAITING_APPROVAL", &[("T01", "TODO", "")]);
    fs::write(dir.join("p/.sdd/settings.yaml"), ": : [não é yaml").expect("settings");
    let r = Request::builder()
        .method(Method::GET)
        .uri("/api/projects/p/state")
        .header(header::HOST, HOST)
        .body(Body::empty())
        .expect("requisição");
    let resp: Response<Body> = router(state.clone()).oneshot(r).await.expect("resposta");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
    let st: Value = serde_json::from_slice(&bytes).expect("json");
    let awaiting = st["specs"]
        .as_array()
        .is_some_and(|specs| specs.iter().any(|s| s["status"] == "AWAITING_APPROVAL"));
    assert!(awaiting, "o /state lista o plano: {st}");
    let (body, text) = attention(&state).await;
    assert_eq!(project(&body, "p").expect("p")["plan_awaiting_approval"], true, "{text}");
}

// --- Paridade da etapa do plano com o cliente (spec 094 T03, decisão 3) ---

/// Fixture compartilhada com o vitest de `planStage` (`web/src/phase.parity.test.ts`).
const PARITY_FIXTURE: &str = include_str!("testdata/plan_stage_parity.json");

/// Revisor fake que só responde quando liberado: mantém a revisão do plano `running`.
struct GatedReviewer {
    gate: Arc<tokio::sync::Notify>,
    calls: std::sync::atomic::AtomicUsize,
}

impl crate::agents::chat::ChatBackend for GatedReviewer {
    fn complete<'a>(
        &'a self,
        _req: &'a crate::agents::llm_client::ChatRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::agents::architect::testing::Reply> + Send + 'a>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            self.gate.notified().await;
            crate::agents::architect::testing::text("{\"issues\": []}")
        })
    }
}

async fn get_json(state: &AppState, uri: &str) -> Value {
    let r = Request::builder().method(Method::GET).uri(uri).header(header::HOST, HOST).body(Body::empty()).expect("requisição");
    let resp: Response<Body> = router(state.clone()).oneshot(r).await.expect("resposta");
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    let bytes: Bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.expect("corpo");
    serde_json::from_slice(&bytes).expect("json")
}

/// Grava o resultado da revisão (`ok`/`unavailable`) com a assinatura do plano atual.
fn write_review(root: &Path, status: &str) {
    let signature = crate::server::plan_review::plan_signature(root, "001-demo").expect("assinatura");
    let record = json!({"signature": signature, "at": "2026-10-19T10:00:00Z", "status": status, "issues": []});
    fs::write(root.join(".sdd/specs/001-demo/plan-review.json"), record.to_string()).expect("registro");
}

/// Cada linha da matriz (turno × `plan_review.status` × execução) é montada de verdade (disco,
/// turno, execução e revisão em curso) e o `/api/attention` conta o plano se e somente se a linha
/// diz `awaiting` (o que o `planStage` do cliente devolve para a mesma linha).
#[tokio::test]
async fn plan_stage_parity_fixture_matches_the_server_rule() {
    let fixture: Value = serde_json::from_str(PARITY_FIXTURE).expect("fixture");
    assert_eq!(fixture["spec_status"], "AWAITING_APPROVAL");
    let cases = fixture["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 24, "turno (2) × revisão (6) × execução (2)");
    for (i, case) in cases.iter().enumerate() {
        let running = case["running"].as_bool().expect("running");
        let turn_open = case["turn_open"].as_bool().expect("turn_open");
        let review = case["plan_review"].as_str();
        let expected = case["plan_awaiting_approval"].as_bool().expect("plan_awaiting_approval");
        assert_eq!(expected, case["stage"] == "awaiting", "linha {i}: awaiting ⇔ plan_awaiting_approval");

        let dir = temp_dir(&format!("parity-{i}"));
        let gated = Arc::new(GatedReviewer { gate: Arc::new(tokio::sync::Notify::new()), calls: Default::default() });
        // Sem revisão (`null`) = sem Revisor configurado; as demais, com Revisor.
        let base = if review.is_some() { state_with_reviewer(&dir, &["p"]) } else { state_with(&dir, &["p"]) };
        let state = base.with_report_backend(gated.clone());
        let root = dir.join("p");
        sdd_project(&root, "AWAITING_APPROVAL", &[("T01", "TODO", "")]);
        let mut reviews = Vec::new();
        match review {
            Some(status @ ("ok" | "unavailable")) => write_review(&root, status),
            Some("stale") => age_plan(&root),
            Some("running") => {
                reviews = crate::server::plan_review::after_chat_turn(&state, "p", root.clone()).await;
                assert_eq!(reviews.len(), 1, "linha {i}: revisão disparada");
                for _ in 0..400 {
                    if gated.calls.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
            _ => {}
        }
        let _run = running.then(|| state.runs().try_start("p", "001-demo").expect("reserva"));
        let _turn = turn_open.then(|| {
            let turn = ArchitectTurn { turn_id: format!("t{i:02}"), user_text: String::new(), started_at: 1 };
            state.begin_architect_turn("p", turn, false)
        });

        // O estado do projeto expõe exatamente a revisão da linha (a mesma que o cliente lê).
        let st = get_json(&state, "/api/projects/p/state").await;
        let spec = st["specs"].as_array().and_then(|s| s.first()).expect("spec");
        assert_eq!(spec["status"], "AWAITING_APPROVAL", "linha {i}");
        assert_eq!(spec["plan_review"]["status"].as_str(), review, "linha {i}: {spec}");

        let (body, text) = attention(&state).await;
        let counted = project(&body, "p").is_some_and(|p| p["plan_awaiting_approval"] == true);
        assert_eq!(counted, expected, "linha {i} ({case}): {text}");

        gated.gate.notify_one();
        for handle in reviews {
            handle.await.expect("revisão");
        }
    }
}
