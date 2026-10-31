//! Testes do catálogo de ações do Nanty (spec 138 T03): cada ação em cada nível, a tabela do código
//! de parada, o limite de novas tentativas, o hash do plano, a faixa "só a pessoa", a decisão
//! atômica, o desfazer (com e sem mudança no meio), a limpeza que poupa o projeto ativo (só com o
//! executor falso do Compose) e nenhuma ação fora do catálogo.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::act::{self, Answer, RunStarter, StartFuture, StartMode};
use super::actions::{self, ActionId, ActionKind, StopRule, CATALOG, MAX_RETRIES};
use super::clock::{at, NantyClock};
use super::journal::JournalKind;
use super::signals::{Signal, SignalKind};
use super::store::{Card, DecidedBy};
use super::*;
use crate::config::{ActionLevel, AgentConfig, Config, Connection, Effort, NantyRoute, Project};
use crate::harness::compose::fake::FakeRunner;
use crate::server::envs::ComposeProvider;
use crate::server::error::ApiError;

/// 2026-10-30 12:00 UTC.
const NOON_MS: u64 = 1_793_361_600_000;
const SPEC: &str = "001-demo";

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-t03-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

/// Relógio que a gente adianta (meio-dia mais `offset`).
fn moving_clock() -> (NantyClock, Arc<AtomicU64>) {
    let offset = Arc::new(AtomicU64::new(0));
    let o = Arc::clone(&offset);
    (Arc::new(move || at((2026, 10, 30), 12, 0, NOON_MS + o.load(Ordering::SeqCst))), offset)
}

/// Projeto no disco com uma spec no estado dado.
fn write_spec(root: &Path, status: &str, body: &str) {
    let spec = root.join(".sdd/specs").join(SPEC);
    std::fs::create_dir_all(spec.join("tasks")).expect("mkdir");
    std::fs::write(root.join(".sdd/settings.yaml"), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\n")
        .expect("settings");
    std::fs::write(
        spec.join("spec.md"),
        format!("---\nid: \"{SPEC}\"\ntitle: \"Demo\"\nstatus: \"{status}\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-30T10:00:00Z\"\n---\n\n## Contexto\n{body}\n"),
    )
    .expect("spec");
    std::fs::write(
        spec.join("tasks/T01.md"),
        format!("---\nid: \"T01\"\nspec_id: \"{SPEC}\"\ntitle: \"Tarefa\"\nstatus: \"TODO\"\ndepends_on: []\nbranch: \"\"\nattempts: 0\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope:\n  - \"src/**\"\n---\n\n## Objetivo\nx\n"),
    )
    .expect("task");
}

fn connection(id: &str) -> Connection {
    Connection {
        id: id.into(),
        name: id.into(),
        base_url: "http://127.0.0.1:9".into(),
        api_key: String::new(),
        wire_api: Default::default(),
        kind: Default::default(),
        base_url_override: None,
    }
}

fn agent(conn: &str, model: &str) -> AgentConfig {
    AgentConfig { connection_id: conn.into(), model: model.into(), effort: Effort::Low, max_steps: None }
}

/// Configuração com o projeto `p1` (em `root`), as conexões `main` e `backup` e a reserva do
/// Desenvolvedor em `backup`.
fn config(root: &Path) -> Config {
    let mut cfg = Config::default();
    cfg.projects.push(Project {
        id: "p1".into(),
        name: "Projeto".into(),
        path: root.to_str().expect("utf-8").into(),
        created_at: "2026-10-30T00:00:00Z".into(),
    });
    cfg.connections = vec![connection("main"), connection("backup")];
    cfg.agents.architect = agent("main", "m-main");
    cfg.agents.developer = agent("main", "m-main");
    cfg.agents.reviewer = agent("main", "m-main");
    cfg.nanty.reserve.insert("developer".into(), NantyRoute { connection_id: "backup".into(), model: "m-backup".into() });
    cfg.nanty.filter.group_wait_secs = 0;
    cfg
}

/// Disparo falso: anota cada chamada e responde o que o teste mandar.
#[derive(Default)]
struct FakeStarter {
    calls: Mutex<Vec<(String, String, StartMode)>>,
    fail: Mutex<Option<&'static str>>,
}

impl FakeStarter {
    fn calls(&self) -> Vec<(String, String, StartMode)> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl RunStarter for FakeStarter {
    fn start(&self, _state: AppState, project_id: String, spec_id: String, mode: StartMode) -> StartFuture {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).push((project_id, spec_id, mode));
        let fail = *self.fail.lock().unwrap_or_else(PoisonError::into_inner);
        Box::pin(async move {
            match fail {
                Some(code) => Err(ApiError::coded(StatusCode::CONFLICT, code, serde_json::Map::new())),
                None => Ok(()),
            }
        })
    }
}

struct World {
    dir: PathBuf,
    root: PathBuf,
    state: AppState,
    starter: Arc<FakeStarter>,
    offset: Arc<AtomicU64>,
}

impl World {
    fn new(tag: &str, edit: impl FnOnce(&mut Config)) -> Self {
        let dir = temp(tag);
        let root = dir.join("p1");
        write_spec(&root, "AWAITING_APPROVAL", "x");
        let mut cfg = config(&root);
        edit(&mut cfg);
        let (clock, offset) = moving_clock();
        let state = AppState::new(dir.join("config.toml"), cfg, 3000).with_data_dir(dir.join("data")).with_nanty_clock(clock);
        assert_eq!(state.nanty().acquire(), InstanceStatus::Active, "o registro só grava com a trava");
        let starter = Arc::new(FakeStarter::default());
        state.nanty().set_starter(Arc::clone(&starter) as Arc<dyn RunStarter>);
        Self { dir, root, state, starter, offset }
    }

    fn nanty(&self) -> Arc<Nanty> {
        Arc::clone(self.state.nanty())
    }

    fn advance(&self, ms: u64) {
        self.offset.fetch_add(ms, Ordering::SeqCst);
    }

    /// Uma rodada: o filtro com os sinais e as ações.
    async fn round(&self, signals: &[Signal]) {
        let cfg = self.state.config().read().await.nanty.clone();
        self.nanty().observe(&cfg, signals, &SignalKind::ALL);
        act::tick(&self.state, &self.nanty()).await;
    }

    fn card(&self, group: &str) -> Option<Card> {
        self.nanty().snapshot().cards.into_iter().find(|c| c.group == group)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.state.nanty().release();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn plan() -> Signal {
    Signal::new(SignalKind::PlanWaiting, Some("p1")).fact("spec_id", SPEC)
}

fn blocked() -> Signal {
    Signal::new(SignalKind::RunBlocked, Some("p1")).fact("spec_id", SPEC).fact("task_id", "T01").fact("attempts", 3)
}

fn stopped(code: &str) -> Signal {
    let kind = if crate::server::nanty::observe::CREDIT_STOP_CODES.contains(&code) {
        SignalKind::NoCredit
    } else {
        SignalKind::ProviderStopped
    };
    Signal::new(kind, Some("p1")).fact("stop", code).fact("role", "developer").fact("spec_id", SPEC)
}

fn orphans() -> Signal {
    Signal::new(SignalKind::DockerOrphans, None).fact("count", 2)
}

fn budget() -> Signal {
    Signal::new(SignalKind::Budget, Some("p1")).fact("spec_id", SPEC).fact("task_id", "T01").fact("attempts", 1).fact("reason", "budget")
}

// ---- Catálogo, níveis e tabela ----

#[test]
fn catalog_is_fixed_with_the_initial_levels_and_nothing_outside_it() {
    let cfg = crate::config::NantyConfig::default();
    let level = |id| actions::level(&cfg, id);
    assert_eq!(level(ActionId::ApprovePlan), Some(ActionLevel::Ask));
    assert_eq!(level(ActionId::ResumeRun), Some(ActionLevel::Ask));
    assert_eq!(level(ActionId::RetryProvider), Some(ActionLevel::Auto));
    assert_eq!(level(ActionId::SwapReserve), Some(ActionLevel::Auto));
    assert_eq!(level(ActionId::CleanOrphans), Some(ActionLevel::Auto));
    for link in [ActionId::RemindLater, ActionId::OpenNants, ActionId::RaiseLimit] {
        assert_eq!(level(link), None, "{link:?}: opção do cartão, sem nível");
    }
    for entry in CATALOG {
        assert_eq!(actions::parse(entry.id.as_str()), Some(entry.id));
        for lang in crate::i18n::Lang::ALL {
            assert!(crate::i18n::lookup(lang, &entry.id.label_code()).is_some(), "{:?} sem nome em {lang:?}", entry.id);
        }
    }
    for outside in ["rm_rf", "run_command", "", "approve", "APPROVE_PLAN", "set_telegram_token"] {
        assert_eq!(actions::parse(outside), None, "{outside}");
    }
    // O nível é por id do catálogo: um id desconhecido na configuração não vale para nada.
    let mut custom = crate::config::NantyConfig::default();
    custom.actions.insert("approve_plan".into(), ActionLevel::Auto);
    custom.actions.insert("retry_provider".into(), ActionLevel::Off);
    custom.actions.insert("runs::resume_spec".into(), ActionLevel::Auto);
    assert_eq!(actions::level(&custom, ActionId::ApprovePlan), Some(ActionLevel::Auto));
    assert_eq!(actions::level(&custom, ActionId::RetryProvider), Some(ActionLevel::Off));
    assert_eq!(actions::level(&custom, ActionId::ResumeRun), Some(ActionLevel::Ask));
}

#[test]
fn person_only_band_never_becomes_automatic() {
    let mut cfg = crate::config::NantyConfig::default();
    for id in [ActionId::AllowCommandAlways, ActionId::ChangeChannel] {
        assert_eq!(id.entry().kind, ActionKind::PersonOnly);
        cfg.actions.insert(id.as_str().into(), ActionLevel::Auto);
        assert_eq!(actions::level(&cfg, id), Some(ActionLevel::Ask), "{id:?}");
        cfg.actions.insert(id.as_str().into(), ActionLevel::Off);
        assert_eq!(actions::level(&cfg, id), Some(ActionLevel::Off));
    }
    assert!(CATALOG.iter().filter(|e| e.kind == ActionKind::PersonOnly).all(|e| e.default_level != ActionLevel::Auto));
}

#[test]
fn stop_code_table() {
    for code in ["run.provider_failed", "run.provider_failed_detail"] {
        assert_eq!(actions::stop_rule(code), StopRule::Retry, "{code}");
    }
    for code in ["run.billing", "run.spend_limit"] {
        assert_eq!(actions::stop_rule(code), StopRule::Swap, "{code}");
    }
    for code in ["run.plan_limit", "run.daily_quota", "run.plan_needs_login", "run.plan_not_eligible", "outro"] {
        assert_eq!(actions::stop_rule(code), StopRule::NoticeOnly, "{code}");
    }
}

#[tokio::test]
async fn notice_only_codes_get_no_action() {
    for code in ["run.plan_limit", "run.daily_quota", "run.plan_needs_login"] {
        let w = World::new(&format!("notice-{}", code.replace('.', "-")), |_| {});
        w.round(&[stopped(code)]).await;
        let snap = w.nanty().snapshot();
        assert!(snap.cards.iter().all(|c| c.action.is_none()), "{code}: {snap:?}");
        assert!(snap.planned.is_empty() && w.starter.calls().is_empty(), "{code}");
        assert_eq!(w.state.config().read().await.agents.developer.connection_id, "main", "{code}: sem troca");
    }
}

// ---- Cada ação em cada nível ----

#[tokio::test]
async fn approve_plan_in_each_level() {
    // Pergunta antes (o padrão): cartão com o `spec_id`, o hash do plano e um token.
    let w = World::new("approve-ask", |_| {});
    w.round(&[plan()]).await;
    let card = w.card("p1:plan_waiting").expect("cartão");
    assert_eq!(card.action.as_deref(), Some("approve_plan"));
    assert_eq!(card.spec_id.as_deref(), Some(SPEC));
    let hash = crate::server::plan_review::plan_signature(&w.root, SPEC);
    assert!(hash.is_some() && card.plan_hash == hash);
    assert_eq!(card.token.as_deref().map(str::len), Some(32));
    assert!(w.starter.calls().is_empty(), "pergunta antes não aprova sozinho");
    let suggested = w.nanty().journal_tail(100).into_iter().filter(|e| e.kind == JournalKind::Suggestion).count();
    assert_eq!(suggested, 1);
    // Outra rodada igual: nada muda.
    let before = w.nanty().snapshot();
    w.round(&[plan()]).await;
    assert_eq!(w.nanty().snapshot(), before);
    drop(w);

    // Automática: aprova sozinho uma vez.
    let w = World::new("approve-auto", |c| {
        c.nanty.actions.insert("approve_plan".into(), ActionLevel::Auto);
    });
    w.round(&[plan()]).await;
    w.round(&[plan()]).await;
    assert_eq!(w.starter.calls(), [("p1".to_string(), SPEC.to_string(), StartMode::Approve)]);
    let done = w.nanty().snapshot().done;
    assert_eq!((done[0].result.as_str(), done[0].decided_by), ("ok", Some(DecidedBy::Auto)));
    assert!(w.card("p1:plan_waiting").is_some_and(|c| c.action.is_none()), "sem ação no cartão");
    drop(w);

    // Desligada: só o aviso.
    let w = World::new("approve-off", |c| {
        c.nanty.actions.insert("approve_plan".into(), ActionLevel::Off);
    });
    w.round(&[plan()]).await;
    assert!(w.card("p1:plan_waiting").is_some_and(|c| c.action.is_none()));
    assert!(w.starter.calls().is_empty());
}

#[tokio::test]
async fn resume_run_in_each_level() {
    for (level, expect_card, expect_start) in
        [(None, true, false), (Some(ActionLevel::Auto), false, true), (Some(ActionLevel::Off), false, false)]
    {
        let w = World::new(&format!("resume-{level:?}"), |c| {
            if let Some(l) = level {
                c.nanty.actions.insert("resume_run".into(), l);
            }
        });
        w.round(&[blocked()]).await;
        let card = w.card("p1:run_blocked").expect("aviso");
        assert_eq!(card.action.as_deref() == Some("resume_run"), expect_card, "{level:?}");
        let calls = w.starter.calls();
        assert_eq!(!calls.is_empty(), expect_start, "{level:?}");
        if expect_start {
            assert_eq!(calls, [("p1".to_string(), SPEC.to_string(), StartMode::Resume)]);
        }
    }
}

#[tokio::test]
async fn retry_in_each_level_with_wait_and_at_most_two_per_stop() {
    // Automática (o padrão): prevista com espera, sai quando vence, no máximo 2 vezes.
    let w = World::new("retry-auto", |_| {});
    let stop = stopped("run.provider_failed");
    w.round(&[stop.clone()]).await;
    let planned = w.nanty().snapshot().planned;
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].at_ms, NOON_MS + actions::retry_wait_ms(0));
    assert!(w.starter.calls().is_empty(), "espera antes da nova tentativa");
    w.advance(actions::retry_wait_ms(0));
    w.round(&[stop.clone()]).await;
    assert_eq!(w.starter.calls().len(), 1);
    // A execução voltou e parou de novo: a condição some e reaparece (a mesma parada).
    w.round(&[]).await;
    w.round(&[stop.clone()]).await;
    assert_eq!(w.nanty().snapshot().planned[0].at_ms, NOON_MS + actions::retry_wait_ms(0) + actions::retry_wait_ms(1));
    w.advance(actions::retry_wait_ms(1));
    w.round(&[stop.clone()]).await;
    assert_eq!(w.starter.calls().len(), 2);
    w.round(&[]).await;
    w.round(&[stop.clone()]).await;
    w.advance(actions::retry_wait_ms(2));
    w.round(&[stop.clone()]).await;
    assert_eq!(w.starter.calls().len(), MAX_RETRIES as usize, "no máximo 2 por parada");
    assert!(w.nanty().snapshot().planned.is_empty());
    assert!(w.starter.calls().iter().all(|c| c.2 == StartMode::Resume));
    // Progresso no projeto: a contagem recomeça.
    w.round(&[Signal::new(SignalKind::TaskDelivered, Some("p1")).fact("task_id", "T01")]).await;
    assert!(w.nanty().snapshot().retries.is_empty());
    drop(w);

    // A outra variante do código também, e nada para os outros códigos.
    let w = World::new("retry-detail", |_| {});
    w.round(&[stopped("run.provider_failed_detail")]).await;
    assert_eq!(w.nanty().snapshot().planned.len(), 1);
    drop(w);

    // Pergunta antes: cartão; desligada: nada.
    let w = World::new("retry-ask", |c| {
        c.nanty.actions.insert("retry_provider".into(), ActionLevel::Ask);
    });
    w.round(&[stop.clone()]).await;
    assert_eq!(w.card("p1:provider_stopped").and_then(|c| c.action).as_deref(), Some("retry_provider"));
    assert!(w.nanty().snapshot().planned.is_empty());
    drop(w);
    let w = World::new("retry-off", |c| {
        c.nanty.actions.insert("retry_provider".into(), ActionLevel::Off);
    });
    w.round(&[stop]).await;
    w.advance(10 * actions::retry_wait_ms(2));
    w.round(&[stopped("run.provider_failed")]).await;
    assert!(w.starter.calls().is_empty() && w.nanty().snapshot().planned.is_empty());
}

#[tokio::test]
async fn swap_to_the_reserve_in_each_level_and_only_for_its_codes() {
    // Automática: troca o papel para a reserva escolhida e retoma.
    let w = World::new("swap-auto", |_| {});
    w.round(&[stopped("run.billing")]).await;
    let dev = w.state.config().read().await.agents.developer.clone();
    assert_eq!((dev.connection_id.as_str(), dev.model.as_str()), ("backup", "m-backup"));
    assert_eq!(w.state.config().read().await.agents.reviewer.connection_id, "main", "só o papel parado");
    assert_eq!(w.starter.calls(), [("p1".to_string(), SPEC.to_string(), StartMode::Resume)], "a troca inclui a retomada");
    let saved = Config::load_from(&w.dir.join("config.toml")).expect("gravado");
    assert_eq!(saved.agents.developer.connection_id, "backup");
    let done = &w.nanty().snapshot().done[0];
    assert!(done.undo.is_some(), "a troca tem desfazer");
    drop(w);

    // `run.spend_limit` também; sem reserva para o papel, só aviso.
    let w = World::new("swap-spend", |c| {
        c.nanty.reserve.clear();
    });
    w.round(&[stopped("run.spend_limit")]).await;
    assert_eq!(w.state.config().read().await.agents.developer.connection_id, "main");
    assert!(w.starter.calls().is_empty());
    drop(w);

    // Pergunta antes: cartão; "sim" troca e retoma.
    let w = World::new("swap-ask", |c| {
        c.nanty.actions.insert("swap_reserve".into(), ActionLevel::Ask);
    });
    w.round(&[stopped("run.spend_limit")]).await;
    let card = w.card("p1:no_credit").expect("cartão");
    assert_eq!(card.action.as_deref(), Some("swap_reserve"));
    assert_eq!(w.state.config().read().await.agents.developer.connection_id, "main", "espera a pessoa");
    let token = card.token.clone().expect("token");
    let out = act::decide(&w.state, &card.id, &token, Answer::Yes, DecidedBy::Telegram).await.expect("decide");
    assert_eq!(out.outcome, "done");
    assert!(out.params.contains_key("undo_id"));
    assert_eq!(w.state.config().read().await.agents.developer.connection_id, "backup");
    drop(w);

    // Desligada.
    let w = World::new("swap-off", |c| {
        c.nanty.actions.insert("swap_reserve".into(), ActionLevel::Off);
    });
    w.round(&[stopped("run.billing")]).await;
    assert_eq!(w.state.config().read().await.agents.developer.connection_id, "main");
    assert!(w.card("p1:no_credit").is_some_and(|c| c.action.is_none()));
}

#[tokio::test]
async fn budget_is_only_a_signal_with_the_raise_limit_link() {
    let w = World::new("budget", |_| {});
    w.round(&[budget()]).await;
    let card = w.card("p1:budget").expect("cartão");
    assert_eq!(card.action.as_deref(), Some("raise_limit"));
    let out = act::decide(&w.state, &card.id, card.token.as_deref().expect("token"), Answer::Yes, DecidedBy::Web)
        .await
        .expect("decide");
    assert_eq!(out.outcome, "open_nants");
    assert_eq!(out.params["action"], "raise_limit");
    assert!(w.starter.calls().is_empty(), "o runner já pausou: nada é executado");
    assert!(w.card("p1:budget").is_some(), "o link não fecha o cartão");
}

// ---- Limpeza: só com o executor falso do Compose ----

fn fake_docker(w: &World, runner: &Arc<FakeRunner>) -> AppState {
    let base = w.dir.join("compose-base");
    w.state.clone().with_compose_provider(ComposeProvider::with_runner(Arc::clone(runner) as _, base))
}

/// Recursos no Docker falso: dois containers (um parado, um rodando) e uma rede de um projeto que
/// esta instalação não conhece, e um container e uma rede do `p1` (cadastrado).
fn docker_with_foreign(runner: &FakeRunner) {
    runner.reply("ps", 0, "c1\tghost\texited\nc2\tghost\trunning\nc3\tp1\texited\n");
    runner.reply("network", 0, "n1\tghost\nn2\tp1\n");
}

fn args_of(runner: &FakeRunner) -> Vec<Vec<String>> {
    runner.calls.lock().unwrap_or_else(PoisonError::into_inner).iter().map(|c| c.args.clone()).collect()
}

/// Nada do projeto desconhecido foi tocado: nenhum `rm`, `network rm` ou `compose` cita os ids dele.
fn foreign_untouched(calls: &[Vec<String>]) {
    for call in calls.iter().filter(|a| a[0] == "rm" || a[0] == "compose" || (a[0] == "network" && a[1] == "rm")) {
        for id in ["c1", "c2", "n1"] {
            assert!(!call.contains(&id.to_string()), "recurso de outra instalação tocado: {call:?}");
        }
        assert!(!call.iter().any(|x| x.contains("ghost")), "{call:?}");
    }
}

#[tokio::test]
async fn clean_orphans_in_each_level_cleans_only_registered_idle_projects() {
    for level in [None, Some(ActionLevel::Ask), Some(ActionLevel::Off)] {
        let w = World::new(&format!("clean-{level:?}"), |c| {
            if let Some(l) = level {
                c.nanty.actions.insert("clean_orphans".into(), l);
            }
        });
        let runner = Arc::new(FakeRunner::default());
        docker_with_foreign(&runner);
        let state = fake_docker(&w, &runner);
        assert!(!state.project_busy_strict("p1"), "p1 parado");
        let cfg = state.config().read().await.nanty.clone();
        w.nanty().observe(&cfg, &[orphans()], &SignalKind::ALL);
        act::tick(&state, &w.nanty()).await;
        let calls = args_of(&runner);
        foreign_untouched(&calls);
        match level {
            None => {
                // O p1 não está em modo docker: o `sweep_orphans` cai no `rm -f` pelos ids dele.
                let rm: Vec<&Vec<String>> = calls.iter().filter(|a| a[0] == "rm").collect();
                assert_eq!(rm, [&vec!["rm".to_string(), "-f".into(), "c3".into()]], "{calls:?}");
                let net_rm: Vec<&Vec<String>> =
                    calls.iter().filter(|a| a.len() > 1 && a[0] == "network" && a[1] == "rm").collect();
                assert_eq!(net_rm, [&vec!["network".to_string(), "rm".into(), "n2".into()]]);
                assert!(calls.iter().any(|a| a[0] == "network" && a[1] == "ls" && a.contains(&"label=nants.project".to_string())));
                let done = &w.nanty().snapshot().done[0];
                assert_eq!(
                    (done.result.as_str(), &done.params["projects"], &done.params["networks"], &done.params["foreign"]),
                    ("ok", &json!(1), &json!(1), &json!(2))
                );
                // Outra rodada com os mesmos fatos: não limpa de novo.
                let before = calls.len();
                act::tick(&state, &w.nanty()).await;
                assert_eq!(args_of(&runner).len(), before);
            }
            Some(ActionLevel::Ask) => {
                assert!(calls.is_empty(), "pergunta antes: {calls:?}");
                assert_eq!(w.card("*:docker_orphans").and_then(|c| c.action).as_deref(), Some("clean_orphans"));
            }
            _ => {
                assert!(calls.is_empty(), "desligada: {calls:?}");
                assert!(w.card("*:docker_orphans").is_none());
            }
        }
    }
}

#[tokio::test]
async fn clean_orphans_spares_the_active_registered_project() {
    let w = World::new("clean-busy", |_| {});
    let runner = Arc::new(FakeRunner::default());
    docker_with_foreign(&runner);
    let state = fake_docker(&w, &runner);
    // O p1 está ativo pela verificação estrita (uma reserva interna conta).
    let _app = state.runs().try_start("p1", crate::server::state::APP_RESERVATION).expect("vaga");
    assert!(!state.project_active("p1"), "a verificação antiga não enxerga a reserva interna");
    assert!(state.project_busy_strict("p1"));
    let cfg = state.config().read().await.nanty.clone();
    w.nanty().observe(&cfg, &[orphans()], &SignalKind::ALL);
    act::tick(&state, &w.nanty()).await;
    let calls = args_of(&runner);
    foreign_untouched(&calls);
    assert!(calls.iter().all(|a| a[0] != "rm" && a[0] != "compose"), "{calls:?}");
    assert!(calls.iter().all(|a| !(a.len() > 1 && a[0] == "network" && a[1] == "rm")), "{calls:?}");
}

#[tokio::test]
async fn foreign_resources_are_only_an_informative_notice() {
    // Mesmo com a limpeza automática, o recurso de projeto desconhecido só vira aviso, sem ação.
    let w = World::new("foreign", |_| {});
    let runner = Arc::new(FakeRunner::default());
    docker_with_foreign(&runner);
    let state = fake_docker(&w, &runner);
    let cfg = state.config().read().await.clone();
    let probe = FakeLabels(vec!["ghost".into(), "ghost".into(), "ghost".into()]);
    let scan = observe::observe_machine(&probe, &cfg, true, &[]);
    assert_eq!(scan.signals.iter().map(|s| s.kind).collect::<Vec<_>>(), [SignalKind::DockerForeign]);
    w.nanty().observe(&cfg.nanty, &scan.signals, &scan.covered);
    act::tick(&state, &w.nanty()).await;
    assert!(args_of(&runner).is_empty(), "nada é chamado no Docker");
    let snap = w.nanty().snapshot();
    let notice = snap.digest.iter().find(|d| d.group == "*:docker_foreign").expect("aviso informativo");
    assert_eq!((notice.code.as_str(), &notice.params["count"]), ("nanty.why.docker_foreign", &json!(3)));
    assert!(snap.cards.iter().all(|c| c.action.is_none()), "sem ação: {snap:?}");
    for lang in crate::i18n::Lang::ALL {
        let text = crate::i18n::tr_plural(lang, "nanty.why.docker_foreign", 3, &[("since", "12:00")]);
        // C-10: o porquê diz o fato e aponta a ajuda; nada de comando.
        assert!(!text.to_lowercase().contains("docker rm") && !text.contains("docker ps") && !text.contains("--filter"), "{lang:?}: {text}");
        assert!(text.contains("Docker") && text.contains("3"), "{lang:?}: {text}");
    }
    // Nenhum nível faz o catálogo agir sobre ele.
    let alert = snap.alerts.get("*:docker_foreign").expect("condição");
    assert!(actions::suggest(alert, &cfg, &snap.retries).is_none());
}

/// Sonda só com a lista do Docker.
struct FakeLabels(Vec<String>);

impl observe::NantyProbe for FakeLabels {
    fn free_bytes(&self, _: &Path) -> Option<u64> {
        None
    }

    fn docker_labeled(&self) -> Option<Vec<String>> {
        Some(self.0.clone())
    }
}

#[test]
fn clean_downs_registered_idle_projects_and_skips_busy_and_foreign_ones() {
    let dir = temp("clean-direct");
    let (idle, busy) = (dir.join("idle"), dir.join("busy"));
    crate::server::envs::tests::docker_project(&idle);
    crate::server::envs::tests::docker_project(&busy);
    let runner = Arc::new(FakeRunner::default());
    runner.reply("ps", 0, "a\tidle\texited\nb\tbusy\trunning\nc1\tghost\texited\nc2\tghost\trunning\n");
    runner.reply("network", 0, "n1\tghost\nn3\tidle\nn2\tbusy\n");
    let provider = ComposeProvider::with_runner(Arc::clone(&runner) as _, dir.join("base"));
    let projects = vec![("idle".to_string(), idle), ("busy".to_string(), busy)];
    let report = crate::server::envs::clean_orphans(&provider, &projects, &|id| id == "busy").expect("limpeza");
    assert_eq!(report.cleaned, ["idle"]);
    assert_eq!((report.networks_removed, report.foreign), (1, 2));
    let calls = args_of(&runner);
    foreign_untouched(&calls);
    assert!(calls.iter().filter(|a| a[0] == "compose").all(|a| a.iter().any(|x| x.starts_with("nants-idle-"))), "{calls:?}");
    assert!(calls.iter().all(|a| !a.contains(&"n2".to_string())), "a rede do ativo fica");
    assert!(calls.iter().any(|a| a[0] == "network" && a.contains(&"n3".to_string())), "a rede do parado sai");
    assert!(calls.iter().all(|a| !a.contains(&"-v".to_string())), "nunca volumes");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- Cartão: hash do plano, estado no clique, decisão atômica, "não" ----

#[tokio::test]
async fn a_changed_plan_gets_a_new_card_and_the_old_button_expires() {
    let w = World::new("hash-rotate", |_| {});
    w.round(&[plan()]).await;
    let old = w.card("p1:plan_waiting").expect("cartão");
    let old_token = old.token.clone().expect("token");
    // O plano mudou com o cartão aberto e a rodada seguinte veio antes do clique.
    write_spec(&w.root, "AWAITING_APPROVAL", "plano editado direto no spec.md");
    w.round(&[plan()]).await;
    let new = w.card("p1:plan_waiting").expect("cartão novo");
    assert_eq!(new.plan_hash, crate::server::plan_review::plan_signature(&w.root, SPEC));
    assert_ne!(new.id, old.id, "id novo");
    assert_ne!(new.token.as_deref(), Some(old_token.as_str()), "token novo");
    // O botão já enviado (id e token antigos) expirou e não aprova nada.
    let out = act::decide(&w.state, &old.id, &old_token, Answer::Yes, DecidedBy::Telegram).await.expect("decide");
    assert_eq!(out.outcome, "expired");
    // O token antigo no cartão novo também não decide.
    let err = act::decide(&w.state, &new.id, &old_token, Answer::Yes, DecidedBy::Telegram).await.expect_err("token antigo");
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
    assert!(w.starter.calls().is_empty(), "nenhum start");
    // A renovação do aviso pelo filtro (mesma oferta) mantém o cartão novo.
    w.advance(4 * 60 * 60 * 1000);
    w.round(&[plan()]).await;
    let renewed = w.card("p1:plan_waiting").expect("renovado");
    assert_eq!((renewed.id, renewed.token), (new.id, new.token));
}

/// Um token só decide a oferta que ele acompanhou: a oferta que sai do cartão (ação desligada, ou o
/// hash do plano ilegível por um instante) e volta com outro plano ganha id e token novos.
#[tokio::test]
async fn an_offer_that_leaves_and_comes_back_never_reuses_the_old_token() {
    for path in ["off", "unreadable"] {
        let w = World::new(&format!("hash-detach-{path}"), |_| {});
        w.round(&[plan()]).await;
        let old = w.card("p1:plan_waiting").expect("cartão");
        let old_token = old.token.clone().expect("token");
        let spec_md = w.root.join(".sdd/specs").join(SPEC).join("spec.md");
        match path {
            "off" => {
                w.state.config().write().await.nanty.actions.insert("approve_plan".into(), ActionLevel::Off);
            }
            _ => std::fs::remove_file(&spec_md).expect("spec.md sendo regravado"),
        }
        w.round(&[plan()]).await;
        let detached = w.card("p1:plan_waiting").expect("aviso");
        assert!(detached.action.is_none() && detached.token.is_none(), "{path}: o token morre com a oferta");
        // O plano muda e a oferta volta.
        write_spec(&w.root, "AWAITING_APPROVAL", "plano novo");
        w.state.config().write().await.nanty.actions.clear();
        w.round(&[plan()]).await;
        let back = w.card("p1:plan_waiting").expect("oferta de volta");
        assert_eq!(back.plan_hash, crate::server::plan_review::plan_signature(&w.root, SPEC), "{path}");
        assert_ne!(back.id, old.id, "{path}: id novo");
        assert_ne!(back.token.as_deref(), Some(old_token.as_str()), "{path}: token novo");
        // O botão antigo não aprova o plano novo.
        let out = act::decide(&w.state, &old.id, &old_token, Answer::Yes, DecidedBy::Telegram).await.expect("decide");
        assert_eq!(out.outcome, "expired", "{path}");
        let err = act::decide(&w.state, &back.id, &old_token, Answer::Yes, DecidedBy::Telegram).await.expect_err("token antigo");
        assert_eq!(err.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(w.starter.calls().is_empty(), "{path}: nenhum start");
    }
}

#[tokio::test]
async fn plan_hash_is_checked_right_before_the_start() {
    let w = World::new("hash", |_| {});
    w.round(&[plan()]).await;
    let card = w.card("p1:plan_waiting").expect("cartão");
    let token = card.token.clone().expect("token");
    // O plano mudou depois do cartão: o sim não aprova.
    write_spec(&w.root, "AWAITING_APPROVAL", "outro texto");
    let out = act::decide(&w.state, &card.id, &token, Answer::Yes, DecidedBy::Web).await.expect("decide");
    assert_eq!(out.outcome, "expired");
    assert!(w.starter.calls().is_empty(), "plano mudou: nada de aprovar");
    assert_eq!(w.nanty().snapshot().done[0].params["reason"], "plan_changed");
    // Fato novo (o plano mudou): a oferta volta com o hash novo, e agora o sim aprova.
    w.round(&[plan()]).await;
    let card = w.card("p1:plan_waiting").expect("cartão de novo");
    assert_eq!(card.plan_hash, crate::server::plan_review::plan_signature(&w.root, SPEC));
    let out = act::decide(&w.state, &card.id, card.token.as_deref().expect("token"), Answer::Yes, DecidedBy::Web)
        .await
        .expect("decide");
    assert_eq!(out.outcome, "done");
    assert_eq!(w.starter.calls(), [("p1".to_string(), SPEC.to_string(), StartMode::Approve)]);
}

#[tokio::test]
async fn state_is_checked_on_the_click_and_expired_cards_say_so() {
    let w = World::new("stale", |_| {});
    w.round(&[blocked()]).await;
    let card = w.card("p1:run_blocked").expect("cartão");
    let token = card.token.clone().expect("token");
    // A pessoa desligou a ação depois do cartão: o clique encontra o estado mudado.
    w.state.config().write().await.nanty.actions.insert("resume_run".into(), ActionLevel::Off);
    let out = act::decide(&w.state, &card.id, &token, Answer::Yes, DecidedBy::Web).await.expect("decide");
    assert_eq!(out.outcome, "expired");
    assert!(w.starter.calls().is_empty());
    // Cartão vencido (validade) e cartão que não existe: expirou.
    w.state.config().write().await.nanty.actions.clear();
    w.round(&[blocked()]).await;
    let card = w.card("p1:run_blocked").expect("cartão");
    w.advance(card.expires_at_ms - NOON_MS);
    let out = act::decide(&w.state, &card.id, card.token.as_deref().expect("token"), Answer::Yes, DecidedBy::Web)
        .await
        .expect("decide");
    assert_eq!(out.outcome, "expired");
    let out = act::decide(&w.state, "0123456789abcdef", &token, Answer::Yes, DecidedBy::Web).await.expect("decide");
    assert_eq!(out.outcome, "expired");
    // Token errado não decide nada.
    w.round(&[blocked()]).await;
    let card = w.card("p1:run_blocked").expect("cartão");
    let err = act::decide(&w.state, &card.id, "f".repeat(32).as_str(), Answer::Yes, DecidedBy::Web).await.expect_err("token");
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
    assert!(w.card("p1:run_blocked").is_some());
    assert!(w.starter.calls().is_empty());
}

#[tokio::test]
async fn decision_is_atomic_and_the_second_click_hears_who_decided() {
    let w = World::new("atomic", |_| {});
    w.round(&[blocked()]).await;
    let card = w.card("p1:run_blocked").expect("cartão");
    let token = card.token.clone().expect("token");
    let (a, b) = tokio::join!(
        act::decide(&w.state, &card.id, &token, Answer::Yes, DecidedBy::Telegram),
        act::decide(&w.state, &card.id, &token, Answer::Yes, DecidedBy::Web),
    );
    let outcomes = [a.expect("a"), b.expect("b")];
    let first = outcomes.iter().filter(|o| o.outcome == "done").count();
    assert_eq!(first, 1, "{outcomes:?}");
    let second = outcomes.iter().find(|o| o.outcome == "already_decided").expect("segundo");
    assert!(second.code.starts_with("nanty.outcome.already_decided."), "{second:?}");
    assert_eq!(w.starter.calls().len(), 1, "executou uma vez só");
    // Mais um clique depois: quem decidiu primeiro aparece.
    let again = act::decide(&w.state, &card.id, &token, Answer::No, DecidedBy::Web).await.expect("de novo");
    let decided = w.nanty().snapshot().decided[0].decided_by;
    assert_eq!(again.code, format!("nanty.outcome.already_decided.{}", serde_json::to_value(decided).expect("by").as_str().unwrap_or_default()));
    for lang in crate::i18n::Lang::ALL {
        assert!(!act::outcome_text(lang, &again).is_empty());
    }
    // O registro tem a decisão (com quem decidiu) e o resultado.
    let journal = w.nanty().journal_tail(100);
    let decision = journal.iter().find(|e| e.kind == JournalKind::Decision).expect("decisão");
    assert_eq!((decision.action.as_deref(), decision.decided_by), (Some("resume_run"), Some(decided)));
    assert!(journal.iter().any(|e| e.kind == JournalKind::Result && e.code == "ok"));
}

#[tokio::test]
async fn after_no_the_same_offer_only_returns_with_a_new_fact() {
    let w = World::new("declined", |_| {});
    w.round(&[blocked()]).await;
    let card = w.card("p1:run_blocked").expect("cartão");
    let out = act::decide(&w.state, &card.id, card.token.as_deref().expect("token"), Answer::No, DecidedBy::Web)
        .await
        .expect("não");
    assert_eq!(out.outcome, "declined");
    assert!(w.card("p1:run_blocked").is_none());
    // O registro da recusa leva a ação e o projeto do cartão.
    let journal = w.nanty().journal_tail(100);
    let decision = journal.iter().find(|e| e.kind == JournalKind::Decision && e.code == "no").expect("recusa no registro");
    assert_eq!(
        (decision.action.as_deref(), decision.project_id.as_deref(), decision.decided_by),
        (Some("resume_run"), Some("p1"), Some(DecidedBy::Web))
    );
    // A repetição do aviso volta sem a oferta.
    w.advance(4 * 60 * 60 * 1000);
    w.round(&[blocked()]).await;
    assert!(w.card("p1:run_blocked").is_some_and(|c| c.action.is_none()), "sem fato novo, sem oferta");
    // Fato novo (outra tarefa bloqueada): a oferta volta.
    let new_fact = Signal::new(SignalKind::RunBlocked, Some("p1")).fact("spec_id", SPEC).fact("task_id", "T02").fact("attempts", 3);
    w.advance(10 * 60 * 1000);
    w.round(&[new_fact]).await;
    assert_eq!(w.card("p1:run_blocked").and_then(|c| c.action).as_deref(), Some("resume_run"));
    assert!(w.starter.calls().is_empty());
}

#[tokio::test]
async fn remind_later_silences_and_comes_back() {
    let w = World::new("later", |_| {});
    w.round(&[blocked()]).await;
    let card = w.card("p1:run_blocked").expect("cartão");
    let out = act::decide(&w.state, &card.id, card.token.as_deref().expect("token"), Answer::Later, DecidedBy::Telegram)
        .await
        .expect("depois");
    assert_eq!(out.outcome, "snoozed");
    w.round(&[blocked()]).await;
    assert!(w.card("p1:run_blocked").is_none(), "calado até o fim do silêncio");
    w.advance(act::REMIND_LATER_MS + 1000);
    w.round(&[blocked()]).await;
    assert_eq!(w.card("p1:run_blocked").and_then(|c| c.action).as_deref(), Some("resume_run"), "voltou");
}

#[tokio::test]
async fn person_only_card_is_never_executed_by_nanty() {
    let w = World::new("person-only", |c| {
        c.nanty.actions.insert("allow_command_always".into(), ActionLevel::Auto);
    });
    let token = act::new_token();
    let id = super::new_id();
    w.nanty().update(|st| {
        st.cards.push(Card {
            id: id.clone(),
            project_id: Some("p1".into()),
            group: "p1:permission".into(),
            code: "c".into(),
            action: Some("allow_command_always".into()),
            token: Some(token.clone()),
            created_at_ms: NOON_MS,
            expires_at_ms: NOON_MS + 60_000,
            ..Card::default()
        });
        true
    });
    let out = act::decide(&w.state, &id, &token, Answer::Yes, DecidedBy::Telegram).await.expect("decide");
    assert_eq!(out.outcome, "person_only");
    assert!(w.starter.calls().is_empty());
    // Ação fora do catálogo num cartão: recusada.
    w.nanty().update(|st| {
        if let Some(c) = st.cards.iter_mut().find(|c| c.id == id) {
            c.action = Some("rm_rf".into());
        }
        true
    });
    let err = act::decide(&w.state, &id, &token, Answer::Yes, DecidedBy::Web).await.expect_err("fora do catálogo");
    assert_eq!(err.code(), Some("nanty_action_invalid"));
}

// ---- Desfazer ----

#[tokio::test]
async fn undo_restores_only_if_the_value_is_still_what_nanty_wrote() {
    // Sem mudança no meio: volta à anterior.
    let w = World::new("undo-ok", |_| {});
    w.round(&[stopped("run.billing")]).await;
    let undo = w.nanty().snapshot().done[0].undo.clone().expect("desfazer");
    let out = act::undo(&w.state, &undo.id, &undo.token, DecidedBy::Web).await.expect("undo");
    assert_eq!(out.outcome, "undone");
    let dev = w.state.config().read().await.agents.developer.clone();
    assert_eq!((dev.connection_id.as_str(), dev.model.as_str()), ("main", "m-main"));
    assert!(w.nanty().snapshot().done.iter().all(|d| d.undo.is_none()), "o desfazer some do feito");
    let again = act::undo(&w.state, &undo.id, &undo.token, DecidedBy::Telegram).await.expect("de novo");
    assert_eq!(again.code, "nanty.outcome.already_decided.web");
    drop(w);

    // Com mudança no meio: a configuração mudou, nada é sobrescrito.
    let w = World::new("undo-changed", |_| {});
    w.round(&[stopped("run.billing")]).await;
    let undo = w.nanty().snapshot().done[0].undo.clone().expect("desfazer");
    let swap = crate::server::config_api::set_role_connection(&w.state, "developer", "main", "m-outro", None).await.expect("troca");
    assert!(matches!(swap, crate::server::config_api::RoleSwap::Done { .. }));
    let out = act::undo(&w.state, &undo.id, &undo.token, DecidedBy::Web).await.expect("undo");
    assert_eq!(out.outcome, "config_changed");
    assert_eq!(w.state.config().read().await.agents.developer.model, "m-outro", "a mudança da pessoa fica");
    let journal = w.nanty().journal_tail(100);
    assert!(journal.iter().any(|e| e.kind == JournalKind::Result && e.code == "config_changed"));
    // Token errado não desfaz.
    let w2 = World::new("undo-token", |_| {});
    w2.round(&[stopped("run.billing")]).await;
    let undo = w2.nanty().snapshot().done[0].undo.clone().expect("desfazer");
    let err = act::undo(&w2.state, &undo.id, &"0".repeat(32), DecidedBy::Web).await.expect_err("token");
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn set_role_connection_validates_like_put_agents() {
    let w = World::new("set-role", |_| {});
    let err = crate::server::config_api::set_role_connection(&w.state, "developer", "nao-existe", "m", None)
        .await
        .expect_err("conexão desconhecida");
    assert_eq!(err.code(), Some("connection_missing"));
    let err = crate::server::config_api::set_role_connection(&w.state, "developer", "backup", " ", None).await.expect_err("modelo");
    assert_eq!(err.code(), Some("model_required"));
    let changed = crate::server::config_api::set_role_connection(&w.state, "developer", "backup", "m", Some(("x", "y")))
        .await
        .expect("cas");
    assert!(matches!(changed, crate::server::config_api::RoleSwap::Changed { .. }));
    assert_eq!(w.state.config().read().await.agents.developer.connection_id, "main");
}

// ---- Rotas ----

async fn post(state: &AppState, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:3000")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("req");
    let resp = crate::server::router(state.clone()).oneshot(req).await.expect("resp");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("corpo").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn decide_and_undo_routes() {
    let w = World::new("routes", |_| {});
    w.round(&[blocked()]).await;
    let card = w.card("p1:run_blocked").expect("cartão");
    let token = card.token.clone().expect("token");
    let uri = format!("/api/nanty/cards/{}/decide", card.id);
    let (status, body) = post(&w.state, &uri, json!({"token": token, "answer": "talvez"})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("nanty_answer_invalid")));
    let (status, _) = post(&w.state, &uri, json!({"token": token, "answer": "yes", "extra": 1})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = post(&w.state, "/api/nanty/cards/x/decide", json!({"token": token, "answer": "yes"})).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("nanty_id_invalid")));
    let (status, body) = post(&w.state, &uri, json!({"token": token, "answer": "yes"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["outcome"].as_str(), body["code"].as_str()), (Some("done"), Some("nanty.outcome.done")));
    let (_, body) = post(&w.state, &uri, json!({"token": token, "answer": "yes"})).await;
    assert_eq!(body["code"], "nanty.outcome.already_decided.web");
    let (status, body) = post(&w.state, "/api/nanty/undo/0123456789abcdef", json!({"token": token})).await;
    assert_eq!((status, body["outcome"].as_str()), (StatusCode::OK, Some("expired")));
}

#[tokio::test]
async fn attention_response_does_not_change() {
    // Sem Revisor, o plano não espera a revisão do plano e já pode ser aprovado.
    let w = World::new("attention", |c| c.agents.reviewer.model.clear());
    let view = crate::server::attention::build_attention(&w.state).await;
    let project = view.projects.iter().find(|p| p.project_id == "p1").expect("p1");
    assert!(project.plan_awaiting_approval);
    assert_eq!(project.plan_spec_id.as_deref(), Some(SPEC), "o Nanty enxerga a spec");
    let json = serde_json::to_value(&view).expect("json");
    let keys: Vec<&String> = json["projects"][0].as_object().expect("obj").keys().collect();
    assert_eq!(keys, ["name", "paused_or_stalled", "pending_choices", "pending_permissions", "plan_awaiting_approval", "project_id"]);
}

#[tokio::test]
async fn the_loop_offers_the_plan_card_with_its_spec_id() {
    // Sem Revisor, o plano não espera a revisão do plano e já pode ser aprovado.
    let w = World::new("loop-observe", |c| c.agents.reviewer.model.clear());
    let cfg = w.state.config().read().await.clone();
    let found = observe::observe_projects(&w.state, &cfg).await.expect("observou");
    let plan = found.iter().find(|s| s.kind == SignalKind::PlanWaiting).expect("plano");
    assert_eq!(plan.facts["spec_id"], SPEC);
}

#[tokio::test]
async fn a_refused_start_keeps_the_api_error_code_translated_by_the_dictionary() {
    let w = World::new("refused", |_| {});
    *w.starter.fail.lock().unwrap_or_else(PoisonError::into_inner) = Some("spec_not_in_progress");
    w.round(&[blocked()]).await;
    let card = w.card("p1:run_blocked").expect("cartão");
    let out = act::decide(&w.state, &card.id, card.token.as_deref().expect("token"), Answer::Yes, DecidedBy::Web)
        .await
        .expect("decide");
    assert_eq!((out.outcome, &out.params["error"]), ("failed", &json!("spec_not_in_progress")));
    for lang in crate::i18n::Lang::ALL {
        let reason = crate::i18n::text(lang, "serverError.spec_not_in_progress");
        let text = act::outcome_text(lang, &out);
        assert!(text.contains(&reason) && text != reason, "{lang:?}: {text}");
    }
    let done = &w.nanty().snapshot().done[0];
    assert_eq!((done.result.as_str(), &done.params["error"]), ("failed", &json!("spec_not_in_progress")));
    assert!(w.nanty().journal_tail(100).iter().any(|e| e.kind == JournalKind::Result && e.code == "failed"));
}
