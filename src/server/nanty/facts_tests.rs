//! Testes da fonte única do Nanty (spec 139 T02, G2 e C-05): a precedência da fase, os fatos limpos
//! (sem zero nem padrão) e a mesma leitura do sino, do observador e de `project_facts` para um
//! projeto com execução bloqueada.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;
use crate::config::Project;
use crate::server::attention::SpecProgress;
use crate::server::nanty::clock::{at, fixed_clock};
use crate::server::nanty::signals::group_key;

/// 2026-10-30 12:00 UTC.
const NOON_MS: u64 = 1_793_361_600_000;
const SPEC: &str = "001-demo";

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-facts-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

/// Projeto com a spec no estado dado e as tarefas `(id, status, attempts, next_action_code)`.
fn write_project(root: &Path, spec_status: &str, tasks: &[(&str, &str, u32, Option<&str>)]) {
    let spec = root.join(".sdd/specs").join(SPEC);
    std::fs::create_dir_all(spec.join("tasks")).expect("mkdir");
    std::fs::write(root.join(".sdd/settings.yaml"), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\n")
        .expect("settings");
    std::fs::write(
        spec.join("spec.md"),
        format!("---\nid: \"{SPEC}\"\ntitle: \"Demo\"\nstatus: \"{spec_status}\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-30T10:00:00Z\"\n---\n\n## Contexto\nx\n"),
    )
    .expect("spec");
    for (id, status, attempts, code) in tasks {
        let code = code.map(|c| format!("next_action_code: \"{c}\"\n")).unwrap_or_default();
        std::fs::write(
            spec.join(format!("tasks/{id}.md")),
            format!("---\nid: \"{id}\"\nspec_id: \"{SPEC}\"\ntitle: \"Tarefa\"\nstatus: \"{status}\"\ndepends_on: []\nbranch: \"\"\nattempts: {attempts}\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\n{code}allowed_scope:\n  - \"src/**\"\n---\n\n## Objetivo\nx\n"),
        )
        .expect("task");
    }
}

struct World {
    dir: PathBuf,
    state: AppState,
}

impl World {
    fn new(tag: &str) -> Self {
        let dir = temp(tag);
        let mut cfg = Config::default();
        cfg.projects.push(Project {
            id: "p1".into(),
            name: "Projeto".into(),
            path: dir.join("p1").to_str().expect("utf-8").into(),
            created_at: "2026-10-30T00:00:00Z".into(),
        });
        cfg.nanty.filter.group_wait_secs = 0;
        let state = AppState::new(dir.join("config.toml"), cfg, 3000)
            .with_data_dir(dir.join("data"))
            .with_nanty_clock(fixed_clock(at((2026, 10, 30), 12, 0, NOON_MS)));
        Self { dir, state }
    }

    fn root(&self) -> PathBuf {
        self.dir.join("p1")
    }

    async fn cfg(&self) -> Config {
        self.state.config().read().await.clone()
    }

    async fn facts(&self) -> ProjectFacts {
        project_facts(&self.state, &self.cfg().await, "p1").await.expect("fatos")
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn base() -> ProjectFacts {
    ProjectFacts { project_id: "p1".into(), ..ProjectFacts::default() }
}

fn blocked(attempts: u32) -> Option<BlockedTask> {
    Some(BlockedTask { task_id: "T01".into(), code: None, attempts, max_attempts: 3 })
}

#[test]
fn the_phase_follows_the_documented_precedence() {
    let all = ProjectFacts {
        blocked: blocked(1),
        stop_code: Some("run.billing".into()),
        plan_waiting: true,
        stalled: true,
        running_spec_id: Some(SPEC.into()),
        ..base()
    };
    assert_eq!(all.phase(), Phase::Paused, "pausada vence tudo");
    let no_task = ProjectFacts { blocked: None, ..all.clone() };
    assert_eq!(no_task.phase(), Phase::ProviderStopped);
    let no_stop = ProjectFacts { stop_code: None, ..no_task.clone() };
    assert_eq!(no_stop.phase(), Phase::PlanWaiting);
    let no_plan = ProjectFacts { plan_waiting: false, ..no_stop.clone() };
    assert_eq!(no_plan.phase(), Phase::Stalled);
    let no_stall = ProjectFacts { stalled: false, ..no_plan };
    assert_eq!(no_stall.phase(), Phase::Running);
    let idle = ProjectFacts { running_spec_id: None, ..no_stall };
    assert_eq!(idle.phase(), Phase::Stopped);
    assert_eq!(base().phase(), Phase::Stopped, "o padrão é parada sem pendência");
    let codes: Vec<&str> = [Phase::Paused, Phase::ProviderStopped, Phase::PlanWaiting, Phase::Stalled, Phase::Running, Phase::Stopped]
        .map(Phase::as_str)
        .to_vec();
    assert_eq!(codes, ["paused", "provider_stopped", "plan_waiting", "stalled", "running", "stopped"]);
}

#[test]
fn the_phase_kind_is_the_signal_the_observer_emits_for_it() {
    let paused = ProjectFacts {
        blocked: Some(BlockedTask { task_id: "T01".into(), code: Some(BUDGET_CODE.into()), attempts: 1, max_attempts: 3 }),
        stop_code: Some("run.billing".into()),
        ..base()
    };
    assert_eq!(paused.phase_kind(), Some(SignalKind::Budget));
    assert!(project_signals(&paused).iter().any(|s| s.kind == SignalKind::Budget));
    let credit = ProjectFacts { stop_code: Some("run.billing".into()), ..base() };
    assert_eq!(credit.phase_kind(), Some(SignalKind::NoCredit));
    let provider = ProjectFacts { stop_code: Some("run.provider_failed".into()), ..base() };
    assert_eq!(provider.phase_kind(), Some(SignalKind::ProviderStopped));
    assert_eq!(ProjectFacts { plan_waiting: true, ..base() }.phase_kind(), Some(SignalKind::PlanWaiting));
    assert_eq!(ProjectFacts { running_spec_id: Some(SPEC.into()), ..base() }.phase_kind(), None);
    assert_eq!(base().phase_kind(), None);
}

#[test]
fn zero_attempts_and_defaults_are_not_facts() {
    let zero = ProjectFacts { blocked: blocked(0), run_spec_id: Some(SPEC.into()), ..base() };
    let signals = project_signals(&zero);
    assert_eq!(signals.len(), 1);
    assert!(!signals[0].facts.contains_key("attempts"), "{:?}", signals[0].facts);
    assert!(!zero.status_facts().contains_key("attempts"));
    let two = ProjectFacts { blocked: blocked(2), ..zero.clone() };
    assert_eq!(project_signals(&two)[0].facts["attempts"], 2, "tentativa de verdade continua fato");
    assert_eq!(two.status_facts()["attempts"], 2);

    // Nada aberto: só a fase.
    let idle = ProjectFacts { cost_usd: Some(0.0), progress: None, ..base() };
    assert_eq!(serde_json::Value::Object(idle.status_facts()), json!({"phase": "stopped"}));

    // Spec sem tarefas: o andamento 0 de 0 não é fato.
    let empty = ProjectFacts {
        progress: Some(SpecProgress { spec_id: SPEC.into(), done: 0, total: 0, current_task: None }),
        ..base()
    };
    assert_eq!(serde_json::Value::Object(empty.status_facts()), json!({"phase": "stopped", "spec_id": SPEC}));
}

#[test]
fn status_facts_carry_spec_task_progress_waiting_and_cost() {
    let running = ProjectFacts {
        running_spec_id: Some(SPEC.into()),
        progress: Some(SpecProgress { spec_id: SPEC.into(), done: 1, total: 3, current_task: Some("T02".into()) }),
        waiting: Waiting { permissions: 2, choices: true, plan: false, recovery: false },
        cost_usd: Some(0.123_456),
        cost_alerts: 1,
        ..base()
    };
    assert_eq!(
        serde_json::Value::Object(running.status_facts()),
        json!({
            "phase": "running", "spec_id": SPEC, "task_id": "T02", "done": 1, "total": 3,
            "waiting": ["permission", "choice"], "permissions": 2, "cost_usd": 0.1235, "cost_alerts": 1,
        })
    );
    // Andamento de outra spec não é o andamento da fase.
    let other = ProjectFacts { running_spec_id: Some("002-outra".into()), ..running };
    let facts = other.status_facts();
    assert_eq!(facts["spec_id"], "002-outra");
    assert!(!facts.contains_key("done") && !facts.contains_key("task_id"), "{facts:?}");
}

#[tokio::test]
async fn a_blocked_run_reads_the_same_in_the_bell_the_signal_and_project_facts() {
    let w = World::new("blocked");
    write_project(&w.root(), "IN_PROGRESS", &[("T01", "DONE", 1, None), ("T02", "BLOCKED", 0, Some("tests_failed"))]);
    let cfg = w.cfg().await;

    // O sino.
    let view = crate::server::attention::build_attention(&w.state).await;
    let bell = view.projects.iter().find(|p| p.project_id == "p1").expect("p1 no sino");
    let run = bell.paused_or_stalled.as_ref().expect("execução pausada");
    assert_eq!((run.kind, run.spec_id.as_str(), run.task_id.as_deref()), ("paused", SPEC, Some("T02")));

    // O observador.
    let signals = observe_projects(&w.state, &cfg).await.expect("observou");
    assert_eq!(signals.len(), 1, "{signals:?}");
    let signal = &signals[0];
    assert_eq!(signal.kind, SignalKind::RunBlocked);
    assert_eq!((&signal.facts["spec_id"], &signal.facts["task_id"]), (&json!(SPEC), &json!("T02")));
    assert!(!signal.facts.contains_key("attempts"), "zero tentativas não vai ao aviso");

    // A fonte única, antes e depois do observador abrir a condição.
    let facts = w.facts().await;
    assert_eq!(facts.phase(), Phase::Paused);
    assert_eq!((facts.phase_spec_id(), facts.phase_task_id()), (Some(SPEC), Some("T02")));
    assert_eq!(project_signals(&facts), signals, "o observador emite o que os fatos dizem");
    assert_eq!(facts.since, None, "sem condição aberta ainda");
    w.state.nanty().observe(&cfg.nanty, &signals, &SignalKind::PROJECT_CONDITIONS);
    let alert = w.state.nanty().snapshot().alerts.get(&group_key(Some("p1"), SignalKind::RunBlocked)).cloned().expect("aviso");
    let facts = w.facts().await;
    assert_eq!(facts.since.as_deref(), Some(alert.since.as_str()), "o desde quando é o do aviso");
    let status = facts.status_facts();
    assert_eq!(
        serde_json::Value::Object(status),
        json!({"phase": "paused", "since": alert.since, "spec_id": SPEC, "task_id": "T02", "done": 1, "total": 2, "reason": "tests_failed"})
    );
}

#[tokio::test]
async fn running_and_plan_waiting_come_from_the_same_reading() {
    let w = World::new("phases");
    write_project(&w.root(), "IN_PROGRESS", &[("T01", "DONE", 1, None), ("T02", "IN_PROGRESS", 1, None), ("T03", "TODO", 0, None)]);
    {
        let _run = w.state.runs().try_start("p1", SPEC).expect("reserva");
        let facts = w.facts().await;
        assert_eq!(facts.phase(), Phase::Running);
        assert_eq!(
            serde_json::Value::Object(facts.status_facts()),
            json!({"phase": "running", "spec_id": SPEC, "task_id": "T02", "done": 1, "total": 3})
        );
    }
    // Sem execução e com tarefa por fazer, sem tarefa BLOCKED nem parada do provedor: o sino lista a
    // execução parada com a opção de retomar, e os fatos dizem o mesmo (fase própria, com a spec da
    // execução). O observador não avisa nada, como antes.
    let bell = crate::server::attention::build_attention(&w.state).await;
    let item = bell.projects.iter().find(|p| p.project_id == "p1").expect("p1 no sino");
    let run = item.paused_or_stalled.as_ref().expect("execução parada");
    assert_eq!((run.kind, run.spec_id.as_str(), run.stop_code.as_deref()), ("stalled", SPEC, None));
    let facts = w.facts().await;
    assert_eq!(facts.phase(), Phase::Stalled);
    assert_eq!(
        serde_json::Value::Object(facts.status_facts()),
        json!({"phase": "stalled", "spec_id": run.spec_id, "done": 1, "total": 3})
    );
    assert!(project_signals(&facts).is_empty());

    // Tudo concluído: nada no sino e parada sem pendência.
    write_project(&w.root(), "IN_PROGRESS", &[("T01", "DONE", 1, None), ("T02", "DONE", 1, None), ("T03", "DONE", 1, None)]);
    let bell = crate::server::attention::build_attention(&w.state).await;
    assert!(bell.projects.iter().all(|p| p.project_id != "p1"), "{bell:?}");
    assert_eq!(w.facts().await.phase(), Phase::Stopped);

    // Sem Revisor, o plano não espera a revisão do plano e já pode ser aprovado.
    w.state.config().write().await.agents.reviewer.model.clear();
    write_project(&w.root(), "AWAITING_APPROVAL", &[("T01", "TODO", 0, None)]);
    let facts = w.facts().await;
    assert_eq!(facts.phase(), Phase::PlanWaiting);
    let status = facts.status_facts();
    assert_eq!((&status["spec_id"], &status["waiting"]), (&json!(SPEC), &json!(["plan_approval"])));
    let bell = crate::server::attention::build_attention(&w.state).await;
    assert!(bell.projects.iter().any(|p| p.project_id == "p1" && p.plan_awaiting_approval));
}

#[test]
fn an_old_alert_with_zero_attempts_does_not_reach_the_model() {
    let mut facts = serde_json::Map::new();
    facts.insert("task_id".into(), json!("T01"));
    facts.insert("attempts".into(), json!(0));
    let seen = crate::server::nanty::turn::structured(&facts);
    assert_eq!(serde_json::Value::Object(seen), json!({"task_id": "T01"}));
    facts.insert("attempts".into(), json!(2));
    assert_eq!(crate::server::nanty::turn::structured(&facts)["attempts"], 2);
}

/// Um aviso de execução bloqueada gravado por uma versão anterior: já avisado, com `attempts: 0`
/// nos fatos e no que foi avisado.
fn legacy_state(cfg: &crate::config::NantyConfig) -> crate::server::nanty::store::NantyState {
    let old = Signal::new(SignalKind::RunBlocked, Some("p1")).fact("spec_id", SPEC).fact("task_id", "T01").fact("attempts", 0);
    let mut st = crate::server::nanty::store::NantyState::default();
    let first = crate::server::nanty::filter::apply(&mut st, &[old], &SignalKind::PROJECT_CONDITIONS, cfg, at((2026, 10, 30), 12, 0, NOON_MS));
    assert!(first.journal.iter().any(|e| e.code == "notify"), "avisado");
    let alert = &st.alerts[&group_key(Some("p1"), SignalKind::RunBlocked)];
    assert_eq!(alert.notified.as_ref().map(|n| n["attempts"].clone()), Some(json!(0)));
    st
}

#[test]
fn an_alert_saved_with_zero_attempts_is_not_resent_after_the_update() {
    let mut cfg = crate::config::NantyConfig::default();
    cfg.filter.group_wait_secs = 0;
    cfg.filter.group_interval_secs = 60;
    cfg.filter.repeat_interval_secs = 4 * 3600;
    let new = Signal::new(SignalKind::RunBlocked, Some("p1")).fact("spec_id", SPEC).fact("task_id", "T01");
    // Passado o intervalo do grupo (o caso normal depois de reiniciar), antes da repetição.
    let later = at((2026, 10, 30), 12, 10, NOON_MS + 10 * 60 * 1000);
    let group = group_key(Some("p1"), SignalKind::RunBlocked);

    // Sem normalizar, o mesmo aviso voltaria como atualização (o defeito que o carregamento evita).
    let mut raw = legacy_state(&cfg);
    let out = crate::server::nanty::filter::apply(&mut raw, std::slice::from_ref(&new), &SignalKind::PROJECT_CONDITIONS, &cfg, later);
    assert!(out.journal.iter().any(|e| e.code == "update"), "{:?}", out.journal);

    // Gravado e lido de novo: o carregamento normaliza e nada sai.
    let dir = temp("legacy");
    let store = crate::server::nanty::store::Store::new(dir.join("nanty"));
    store.save(&legacy_state(&cfg)).expect("gravou");
    let mut st = store.load();
    let alert = &st.alerts[&group];
    assert!(!alert.facts.contains_key("attempts"));
    assert!(alert.notified.as_ref().is_some_and(|n| !n.contains_key("attempts")));
    let (cards, digest, sent) = (st.cards.clone(), st.digest.clone(), st.last_sent.clone());
    let out = crate::server::nanty::filter::apply(&mut st, &[new], &SignalKind::PROJECT_CONDITIONS, &cfg, later);
    assert!(out.journal.is_empty(), "nenhum sinal novo nem envio: {:?}", out.journal);
    assert!(!out.changed);
    assert_eq!((st.cards, st.digest, st.last_sent), (cards, digest, sent), "nada reenviado");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_unknown_project_has_no_facts() {
    let w = World::new("unknown");
    assert!(project_facts(&w.state, &w.cfg().await, "nope").await.is_none());
}
