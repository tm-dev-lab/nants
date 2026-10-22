//! Testes do ciclo de vida Compose fora dos testes (`envs.rs`), com executor falso.
//! Os helpers (`pub(crate)`) são reaproveitados pelos testes de app, relatório e remoção.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::*;
use crate::harness::compose::{ComposeOutput, PublishedPort};
use crate::sdd::environment::env_hash;

/// Saída de `compose ps --format json` com uma porta publicada (IPv4 e IPv6) e uma interna.
pub(crate) const PS_JSON: &str = concat!(
    r#"{"Service":"web","Publishers":[{"URL":"0.0.0.0","TargetPort":8080,"PublishedPort":18080,"Protocol":"tcp"},"#,
    r#"{"URL":"::","TargetPort":8080,"PublishedPort":18080,"Protocol":"tcp"},"#,
    r#"{"URL":"","TargetPort":5432,"PublishedPort":0,"Protocol":"tcp"}]}"#,
    "\n",
    r#"{"Service":"db","Publishers":[]}"#,
    "\n"
);

/// Executor falso: registra todas as chamadas (args completos) e responde por padrão.
#[derive(Default)]
pub(crate) struct Rec {
    pub calls: Mutex<Vec<Vec<String>>>,
    pub ps_output: Mutex<String>,
    pub docker_ps: Mutex<String>,
    pub fail_sub: Mutex<Option<String>>,
    /// Conteúdo do override no momento do `up` (o arquivo é regravado a cada chamada).
    pub up_override: Mutex<String>,
    /// Dorme antes de responder a este subcomando (bloqueio do daemon).
    pub delay_sub: Mutex<Option<(String, Duration)>>,
}

impl Rec {
    pub fn with_ports() -> Arc<Self> {
        let rec = Self::default();
        *rec.ps_output.lock().unwrap_or_else(PoisonError::into_inner) = PS_JSON.to_string();
        Arc::new(rec)
    }

    /// Subcomando compose (token após o último `-f <arquivo>`) ou o primeiro argumento (docker puro).
    fn sub(args: &[String]) -> String {
        match args.iter().rposition(|a| a == "-f") {
            Some(i) => args.get(i + 2).cloned().unwrap_or_default(),
            None => args.first().cloned().unwrap_or_default(),
        }
    }

    /// Chamadas ao compose como `projeto: args após o override`.
    pub fn compose_calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|a| a.first().is_some_and(|f| f == "compose"))
            .map(|a| {
                let i = a.iter().rposition(|x| x == "-f").map_or(0, |i| i + 2);
                (a[2].clone(), a[i..].to_vec())
            })
            .collect()
    }

    pub fn docker_calls(&self, first: &str) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|a| a.first().is_some_and(|f| f == first))
            .cloned()
            .collect()
    }
}

impl ComposeRunner for Rec {
    fn run(&self, call: &ComposeCall) -> Result<crate::harness::compose::ComposeOutput, String> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call.args.clone());
        let sub = Rec::sub(&call.args);
        if sub == "up" {
            if let Some(i) = call.args.iter().rposition(|x| x == "-f") {
                let text = std::fs::read_to_string(&call.args[i + 1]).unwrap_or_default();
                *self.up_override.lock().unwrap_or_else(PoisonError::into_inner) = text;
            }
        }
        let delay = self.delay_sub.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some((d, t)) = delay {
            if d == sub {
                std::thread::sleep(t);
            }
        }
        let fail = self.fail_sub.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if fail.as_deref() == Some(sub.as_str()) {
            return Ok(ComposeOutput { code: Some(1), output: "boom".into(), timed_out: false });
        }
        let output = match (call.args.first().map(String::as_str), sub.as_str()) {
            (Some("compose"), "ps") => self.ps_output.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            (Some("compose"), "logs") => "web-1  | servidor no ar\n".to_string(),
            (Some("ps"), _) => self.docker_ps.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            _ => String::new(),
        };
        Ok(ComposeOutput { code: Some(0), output, timed_out: false })
    }
}

pub(crate) const COMPOSE_YAML: &str = "services:\n  workspace:\n    image: alpine\n  web:\n    image: alpine\n    ports:\n      - \"18080:8080\"\n";

/// Projeto (raiz) em modo docker com `.nants/compose.yaml` aprovado no settings.
pub(crate) fn docker_project(root: &Path) {
    fs::create_dir_all(root.join(".nants")).expect("nants");
    fs::create_dir_all(root.join(".sdd")).expect("sdd");
    fs::write(root.join(".nants/compose.yaml"), COMPOSE_YAML).expect("compose");
    let hash = env_hash(root).expect("hash");
    fs::write(
        root.join(".sdd/settings.yaml"),
        format!(
            "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\nenvironment: docker\nenvironment_hash: \"{hash}\"\n"
        ),
    )
    .expect("settings");
}

/// Provider com executor falso e diretório de overrides próprio.
pub(crate) fn provider(rec: &Arc<Rec>) -> ComposeProvider {
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = std::env::temp_dir().join(format!("nants-envs-base-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    ComposeProvider::with_runner(rec.clone(), base)
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nants-envs-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("dir");
    dir
}

#[test]
fn cleanup_test_runs_down_with_volumes_only_in_docker_mode() {
    let (docker, host) = (tmp("c-docker"), tmp("c-host"));
    docker_project(&docker);
    fs::create_dir_all(host.join(".sdd")).expect("sdd");
    fs::write(host.join(".sdd/settings.yaml"), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\n").expect("s");
    let rec = Rec::with_ports();
    let p = provider(&rec);
    assert_eq!(cleanup_test(&p, &host, "h"), Ok(false));
    assert!(rec.compose_calls().is_empty(), "modo host não chama o docker");
    assert_eq!(cleanup_test(&p, &docker, "d"), Ok(true));
    let calls = rec.compose_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "nants-d-test");
    assert_eq!(calls[0].1, ["down", "-v", "--remove-orphans"]);
}

#[test]
fn teardown_downs_both_projects_and_removes_run_volumes_only_when_confirmed() {
    let root = tmp("t");
    docker_project(&root);
    for (confirm, run_args) in [(false, vec!["down", "--timeout", "5", "--remove-orphans"]), (true, vec!["down", "--timeout", "5", "-v", "--remove-orphans"])] {
        let rec = Rec::with_ports();
        assert!(teardown(&provider(&rec), &root, "p", true, confirm).is_empty());
        let calls = rec.compose_calls();
        assert_eq!(calls[0].0, "nants-p-test");
        assert_eq!(calls[0].1, ["down", "-v", "--remove-orphans"]);
        assert_eq!(calls[1].0, "nants-p-run");
        assert_eq!(calls[1].1, run_args);
    }
}

#[test]
fn shutdown_all_downs_test_and_run_without_volumes() {
    let (docker, host) = (tmp("s-docker"), tmp("s-host"));
    docker_project(&docker);
    let rec = Rec::with_ports();
    shutdown_all(&provider(&rec), &[("d".into(), docker), ("h".into(), host)]);
    let calls = rec.compose_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!((calls[0].0.as_str(), calls[1].0.as_str()), ("nants-d-test", "nants-d-run"));
    assert_eq!(calls[0].1, ["down", "--remove-orphans"]);
    assert_eq!(calls[1].1, ["down", "--timeout", "5", "--remove-orphans"]);
}

#[test]
fn sweep_downs_registered_inactive_warns_unknown_and_never_uses_volume_flags() {
    let (a, b) = (tmp("sw-a"), tmp("sw-b"));
    docker_project(&a);
    docker_project(&b);
    let rec = Rec::with_ports();
    *rec.docker_ps.lock().expect("lock") = "c1\tpa\nc2\tpa\nc3\tpb\nc4\tdesconhecido\n".into();
    let projects = vec![("pa".to_string(), a), ("pb".to_string(), b)];
    let report = sweep_orphans(&provider(&rec), &projects, &|id| id == "pb").expect("sweep");
    assert_eq!(report.cleaned, vec!["pa"]);
    assert_eq!(report.unknown, vec!["desconhecido"]);
    let listed = &rec.docker_calls("ps")[0];
    assert!(listed.contains(&"label=nants.project".to_string()), "{listed:?}");
    let calls = rec.compose_calls();
    assert_eq!(calls.len(), 2, "só o projeto cadastrado e inativo");
    assert!(calls.iter().all(|(p, a)| p.starts_with("nants-pa-") && a.ends_with(&["--remove-orphans".to_string()])), "{calls:?}");
    assert!(rec.docker_calls("rm").is_empty(), "compose resolveu; nada de rm");
}

#[test]
fn sweep_falls_back_to_rm_when_compose_is_unusable() {
    let root = tmp("sw-host");
    fs::create_dir_all(root.join(".sdd")).expect("sdd");
    fs::write(root.join(".sdd/settings.yaml"), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\n").expect("s");
    let rec = Rec::with_ports();
    *rec.docker_ps.lock().expect("lock") = "c1\tph\nc2\tph\n".into();
    let report = sweep_orphans(&provider(&rec), &[("ph".into(), root)], &|_| false).expect("sweep");
    assert_eq!(report.cleaned, vec!["ph"]);
    assert_eq!(rec.docker_calls("rm"), vec![vec!["rm".to_string(), "-f".into(), "c1".into(), "c2".into()]]);
}

#[test]
fn run_env_up_ps_and_logs_use_run_project() {
    let root = tmp("run");
    docker_project(&root);
    let rec = Rec::with_ports();
    let env = provider(&rec).for_project(&root, "p", ComposeKind::Run).expect("docker");
    env.up_run(&root, "npm run dev").expect("up");
    let ports = env.published_ports(&root).expect("ports");
    assert_eq!(ports, vec![PublishedPort { service: "web".into(), target: 8080, published: 18080 }]);
    assert!(env.logs(&root).expect("logs").contains("no ar"));
    let calls = rec.compose_calls();
    assert!(calls.iter().all(|(p, _)| p == "nants-p-run"));
    assert_eq!(calls[0].1, ["up", "-d", "--build", "--wait"]);
    assert_eq!(calls[1].1, ["ps", "--format", "json"]);
    assert_eq!(calls[2].1, ["logs", "--no-color", "--tail", "200"]);
    let _ = Duration::from_secs(0);
}

#[test]
fn diverging_environment_hash_fails_closed_for_run_project() {
    let root = tmp("hash");
    docker_project(&root);
    fs::write(root.join(".nants/compose.yaml"), format!("{COMPOSE_YAML}  extra:\n    image: alpine\n")).expect("w");
    let rec = Rec::with_ports();
    let env = provider(&rec).for_project(&root, "p", ComposeKind::Run).expect("docker");
    assert!(env.up_run(&root, "npm run dev").is_err());
    assert!(rec.compose_calls().is_empty());
}

#[tokio::test]
async fn startup_sweep_does_not_delay_the_server() {
    use axum::body::Body;
    use axum::http::{header, Request};
    use tower::ServiceExt;
    let rec = Rec::with_ports();
    *rec.delay_sub.lock().expect("lock") = Some(("ps".into(), Duration::from_millis(1500)));
    let state = crate::server::AppState::new(std::env::temp_dir().join("nants-envs-sweep.toml"), crate::config::Config::default(), 3000)
        .with_compose_provider(provider(&rec)).with_app_observe_window(Duration::ZERO);
    let sweep = tokio::spawn({
        let state = state.clone();
        async move { startup_sweep(&state).await }
    });
    let started = std::time::Instant::now();
    let req = Request::builder().uri("/api/config").header(header::HOST, "127.0.0.1:3000").body(Body::empty()).expect("req");
    let resp = crate::server::router(state).oneshot(req).await.expect("resp");
    assert!(resp.status().is_success() || resp.status().is_client_error());
    assert!(started.elapsed() < Duration::from_millis(1000), "servidor esperou o sweep");
    assert!(!sweep.is_finished(), "o sweep ainda deveria estar rodando");
    let _ = sweep.await;
}

#[test]
fn project_that_becomes_active_during_the_sweep_is_not_torn_down() {
    let root = tmp("sw-active");
    docker_project(&root);
    let rec = Rec::with_ports();
    *rec.docker_ps.lock().expect("lock") = "c1\tpa\n".into();
    let checks = AtomicUsize::new(0);
    let report = sweep_orphans(&provider(&rec), &[("pa".into(), root)], &|_| checks.fetch_add(1, Ordering::SeqCst) >= 1)
        .expect("sweep");
    assert!(report.cleaned.is_empty());
    assert!(rec.compose_calls().is_empty(), "{:?}", rec.compose_calls());
    assert!(rec.docker_calls("rm").is_empty());
}

#[tokio::test]
async fn concurrent_docker_starts_are_refused() {
    let root = tmp("conc");
    docker_project(&root);
    let rec = Rec::with_ports();
    *rec.delay_sub.lock().expect("lock") = Some(("up".into(), Duration::from_millis(500)));
    let events = crate::server::events::EventBus::new();
    let apps = crate::server::app::AppManager::new(events).with_observe_window(Duration::ZERO);
    let env = || provider(&rec).for_project(&root, "p", ComposeKind::Run).expect("docker");
    let (a, b) = tokio::join!(
        apps.launch_docker("p", &root, "s", env(), "npm run dev"),
        async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(apps.is_busy("p"));
            apps.launch_docker("p", &root, "s", env(), "npm run dev").await
        }
    );
    assert!(a.is_ok(), "{a:?}");
    assert!(matches!(b, Err(crate::server::app::LaunchError::AlreadyRunning)));
    assert_eq!(rec.compose_calls().iter().filter(|(_, a)| a[0] == "up").count(), 1);
}

#[test]
fn sweep_removes_leftover_browser_containers_of_inactive_registered_projects() {
    let a = tmp("sw-br");
    let rec = Rec::with_ports();
    *rec.docker_ps.lock().expect("lock") = "b1\tnants-pa-browser-2\nb2\tnants-pz-browser-1\n".into();
    let projects = vec![("pa".to_string(), a)];
    let _ = sweep_orphans(&provider(&rec), &projects, &|_| false);
    let rms = rec.docker_calls("rm");
    assert!(rms.iter().any(|c| c.contains(&"b1".to_string()) && !c.contains(&"b2".to_string())), "{rms:?}");
}

#[tokio::test]
async fn container_that_exits_after_up_fails_with_logs_and_is_downed() {
    let root = tmp("early-exit");
    docker_project(&root);
    let rec = Rec::with_ports();
    *rec.ps_output.lock().expect("lock") = r#"{"Service":"workspace","State":"exited","ExitCode":3,"Publishers":[]}"#.into();
    let events = crate::server::events::EventBus::new();
    let apps = crate::server::app::AppManager::new(events).with_observe_window(Duration::from_secs(2));
    let env = provider(&rec).for_project(&root, "p", ComposeKind::Run).expect("docker");
    let r = apps.launch_docker("p", &root, "s", env, "npm ci").await;
    match r {
        Err(crate::server::app::LaunchError::Docker(m)) => assert!(m.contains("código 3"), "{m}"),
        other => panic!("esperava Docker, veio {other:?}"),
    }
    let calls = rec.compose_calls();
    assert!(calls.iter().any(|(_, a)| a[0] == "down"), "{calls:?}");
    assert!(!apps.is_running("p"));
}
