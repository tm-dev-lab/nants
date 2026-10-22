//! `GET /api/system/prereqs`: diagnóstico dos pré-requisitos do Nants (Git, Docker, Compose v2).
//!
//! Subprocessos sem shell e com timeout curto. O Nants só diagnostica: o campo `fix` traz o
//! comando/instrução para o usuário executar; nada é instalado nem executado com sudo.
//! Resultado em cache por 30 s. O executor de comandos é injetável (testes usam um falso).

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::Json;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};

use super::events::{message_params, valid_message_code, SecretSet};
use super::AppState;
use crate::i18n::{Lang, Msg, Neutral};

/// Validade do cache do diagnóstico.
pub const PREREQS_CACHE_TTL: Duration = Duration::from_secs(30);
/// Timeout de cada subprocesso.
pub const PREREQ_CMD_TIMEOUT: Duration = Duration::from_secs(5);
/// Teto total da conferência na hora (sem cache) das ações que executam (spec 093 T02).
pub const FRESH_CHECK_CEILING: Duration = Duration::from_secs(8);
const MAX_OUT: u64 = 8 * 1024;

/// Resultado de um comando executado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Executor injetável: `(programa, argumentos)`; `None` = não executou (ausente/timeout).
pub type CmdRunner = Arc<dyn Fn(&str, &[&str]) -> Option<CmdOutput> + Send + Sync>;

/// Item do diagnóstico. Na API (serialização abaixo): `id`, `label` e `detail` em pt-BR (reserva),
/// `ok`, `version`, `fix`, `info` e, desde a spec 102 T05, o detalhe na forma neutra de idioma
/// (`detail_code`, `detail_params` e, numa lista, `detail_parts`), que a interface traduz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrereqItem {
    pub id: &'static str,
    pub label: &'static str,
    pub ok: bool,
    pub version: Option<String>,
    pub detail: Option<String>,
    pub fix: Option<String>,
    /// Item só informativo (spec 103 T02: WSL, imagens do Playwright, versão): nunca bloqueia.
    pub info: bool,
    /// Aviso que não bloqueia (spec 111: acesso pela rede local diferente do Windows). O item segue
    /// `ok` (não muda a prontidão); a API manda `"warn": true` para Primeiros passos.
    pub warn: bool,
    /// O `detail` por chave do dicionário (spec 102): o `nants doctor` o mostra no idioma do
    /// terminal; a API o manda na forma neutra ([`Msg::neutral`]).
    pub detail_msg: Option<Msg>,
}

/// Prefixo das chaves de detalhe; o `detail_code` da API é a chave sem ele.
const DETAIL_PREFIX: &str = "doctor.detail.";
/// Teto de itens de `detail_parts`.
const MAX_DETAIL_PARTS: usize = 8;

/// `{code, params}` saneados como os das mensagens com `code` (`message_params`), ou `None` com
/// um `code` fora do formato.
fn neutral_json(neutral: &Neutral) -> Option<(String, serde_json::Map<String, serde_json::Value>)> {
    if !valid_message_code(&neutral.code) {
        return None;
    }
    Some((neutral.code.clone(), message_params(&SecretSet::default(), &neutral.params)))
}

impl Serialize for PrereqItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("id", self.id)?;
        map.serialize_entry("label", self.label)?;
        map.serialize_entry("ok", &self.ok)?;
        if let Some(version) = &self.version {
            map.serialize_entry("version", version)?;
        }
        if let Some(detail) = &self.detail {
            map.serialize_entry("detail", detail)?;
        }
        if let Some(neutral) = self.detail_msg.as_ref().map(|m| m.neutral(DETAIL_PREFIX)) {
            if let Some((code, params)) = neutral_json(&neutral) {
                map.serialize_entry("detail_code", &code)?;
                map.serialize_entry("detail_params", &params)?;
                let parts: Vec<serde_json::Value> = neutral
                    .parts
                    .iter()
                    .filter_map(neutral_json)
                    .take(MAX_DETAIL_PARTS)
                    .map(|(code, params)| serde_json::json!({"code": code, "params": params}))
                    .collect();
                if !parts.is_empty() {
                    map.serialize_entry("detail_parts", &parts)?;
                }
            }
        }
        if let Some(fix) = &self.fix {
            map.serialize_entry("fix", fix)?;
        }
        if self.info {
            map.serialize_entry("info", &true)?;
        }
        if self.warn {
            map.serialize_entry("warn", &true)?;
        }
        map.end()
    }
}

/// Ids obrigatórios da máquina, na ordem do diagnóstico (estáveis: a prontidão, a interface e o
/// `nants doctor` dependem deles).
pub const REQUIRED_IDS: [&str; 5] = ["git", "docker_cli", "docker_daemon", "docker_permission", "docker_compose"];

/// Executor real: sem shell, stdin nulo, timeout com kill.
pub fn real_runner(timeout: Duration) -> CmdRunner {
    Arc::new(move |prog: &str, args: &[&str]| {
        // Sentinela dos testes (spec 113 T01): nenhum teste chama o docker real.
        #[cfg(test)]
        if crate::harness::compose::refuse_real_docker_in_tests(prog).is_some() {
            return None;
        }
        let mut child = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let out = child.stdout.take()?;
        let err = child.stderr.take()?;
        // Bytes e não UTF-8 obrigatório: o `netsh.exe` do Windows responde em CP850 ou Latin-1
        // (spec 111); o que não for UTF-8 vira caractere de substituição em vez de saída vazia.
        let read_lossy = |mut pipe: Box<dyn Read + Send>| {
            let mut buf = Vec::new();
            let _ = (&mut pipe).take(MAX_OUT).read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        };
        let t_out = std::thread::spawn(move || read_lossy(Box::new(out)));
        let t_err = std::thread::spawn(move || read_lossy(Box::new(err)));
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break Some(st),
                Ok(None) if start.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let stdout = t_out.join().unwrap_or_default();
        let stderr = t_err.join().unwrap_or_default();
        status.map(|st| CmdOutput { success: st.success(), stdout, stderr })
    })
}

/// Diagnóstico com cache.
#[derive(Clone)]
pub struct PrereqChecker {
    runner: CmdRunner,
    ttl: Duration,
    /// Teto da conferência na hora ([`FRESH_CHECK_CEILING`]; testes usam prazo curto).
    fresh_ceiling: Duration,
    cache: Arc<Mutex<Option<(Instant, Vec<PrereqItem>)>>>,
}

impl std::fmt::Debug for PrereqChecker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrereqChecker")
    }
}

impl PrereqChecker {
    pub fn new(runner: CmdRunner) -> Self {
        Self {
            runner,
            ttl: PREREQS_CACHE_TTL,
            fresh_ceiling: FRESH_CHECK_CEILING,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    pub fn real() -> Self {
        Self::new(real_runner(PREREQ_CMD_TIMEOUT))
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Troca o teto da conferência na hora (testes).
    pub fn with_fresh_ceiling(mut self, ceiling: Duration) -> Self {
        self.fresh_ceiling = ceiling;
        self
    }

    /// Teto da conferência na hora.
    pub fn fresh_ceiling(&self) -> Duration {
        self.fresh_ceiling
    }

    /// Diagnóstico sem cache (bloqueante; chamar em `spawn_blocking`). O resultado renova o cache.
    pub fn check_fresh(&self) -> Vec<PrereqItem> {
        let items = diagnose(&self.runner);
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((Instant::now(), items.clone()));
        items
    }

    /// `true` quando CLI, daemon, permissão e Compose v2 do Docker estão ok (bloqueante).
    pub fn docker_available(&self) -> bool {
        let items = self.check();
        ["docker_cli", "docker_daemon", "docker_permission", "docker_compose"]
            .iter()
            .all(|id| items.iter().any(|i| i.id == *id && i.ok))
    }

    /// Itens do diagnóstico (bloqueante; chamar em `spawn_blocking`).
    pub fn check(&self) -> Vec<PrereqItem> {
        {
            let guard = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((at, items)) = guard.as_ref() {
                if at.elapsed() < self.ttl {
                    return items.clone();
                }
            }
        }
        self.check_fresh()
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().chars().take(120).collect()
}

/// Rótulo pt-BR do item pelo `id` estável (dicionário `doctor.label.<id>`, spec 102).
pub fn label_for(id: &'static str) -> &'static str {
    crate::i18n::lookup(Lang::PtBr, &format!("doctor.label.{id}")).unwrap_or(id)
}

/// Ação sugerida pt-BR do item pelo `id` estável (dicionário `doctor.fix.<id>`, spec 102).
pub fn fix_for(id: &str) -> String {
    crate::i18n::text(Lang::PtBr, &format!("doctor.fix.{id}"))
}

fn item(id: &'static str, ok: bool, version: Option<String>, detail: Option<Msg>) -> PrereqItem {
    PrereqItem {
        id,
        label: label_for(id),
        ok,
        version,
        detail: detail.as_ref().map(|m| m.render(Lang::PtBr)),
        fix: if ok { None } else { Some(fix_for(id)) },
        info: false,
        warn: false,
        detail_msg: detail,
    }
}

/// Detalhe por chave do dicionário (spec 102).
fn d(key: &'static str) -> Option<Msg> {
    Some(Msg::new(key))
}
const DEP: &str = "doctor.detail.depends_on_cli";

/// Diagnóstico dos itens obrigatórios ([`REQUIRED_IDS`]), sem cache (bloqueante). Usado pelo
/// [`PrereqChecker`] do servidor e pelo `nants doctor`.
pub fn diagnose(run: &CmdRunner) -> Vec<PrereqItem> {
    let mut items = Vec::with_capacity(5);

    match run("git", &["--version"]) {
        Some(o) if o.success => items.push(item("git", true, Some(first_line(&o.stdout)), None)),
        _ => items.push(item("git", false, None, d("doctor.detail.git_missing"))),
    }

    let Some(cli) = run("docker", &["--version"]).filter(|o| o.success) else {
        items.push(item("docker_cli", false, None, d("doctor.detail.docker_missing")));
        items.push(item("docker_daemon", false, None, d(DEP)));
        items.push(item("docker_permission", false, None, d(DEP)));
        items.push(item("docker_compose", false, None, d(DEP)));
        return items;
    };
    items.push(item("docker_cli", true, Some(first_line(&cli.stdout)), None));

    match run("docker", &["info", "--format", "{{.ServerVersion}}"]) {
        Some(o) if o.success => {
            items.push(item("docker_daemon", true, Some(first_line(&o.stdout)), None));
            items.push(item("docker_permission", true, None, None));
        }
        other => {
            let err = other.map(|o| o.stderr.to_lowercase()).unwrap_or_default();
            if err.contains("permission denied") {
                items.push(item("docker_daemon", false, None, d("doctor.detail.daemon_unchecked_permission")));
                items.push(item("docker_permission", false, None, d("doctor.detail.permission_denied")));
            } else {
                items.push(item("docker_daemon", false, None, d("doctor.detail.daemon_unreachable")));
                items.push(item("docker_permission", false, None, d("doctor.detail.permission_unchecked")));
            }
        }
    }

    match run("docker", &["compose", "version", "--short"]) {
        Some(o) if o.success => items.push(item("docker_compose", true, Some(first_line(&o.stdout)), None)),
        _ => items.push(item("docker_compose", false, None, d("doctor.detail.compose_missing"))),
    }
    items
}

/// `GET /api/system/prereqs`: os itens obrigatórios (com cache) e, depois deles, os itens da
/// máquina que o `nants doctor` também mostra (espaço em disco, ambiente, acesso pela rede local e
/// imagens do Playwright; spec 103 T02 e spec 111). A porta e a versão ficam só no `doctor`: o
/// servidor já está numa porta e a interface não consulta a internet.
///
/// A consulta ao Windows (só leitura, com prazo curto) roda em paralelo com o resto; se ela falhar,
/// o item `wsl` fica como veio e o da rede não entra.
pub async fn get_prereqs(State(state): State<AppState>) -> Json<Vec<PrereqItem>> {
    let checker = state.prereqs().clone();
    let (paths, saved) = {
        let cfg = state.config().read().await;
        (super::doctor::disk_paths(&cfg), cfg.server.lan)
    };
    let probe = state.net_probe().clone();
    let wsl = probe.wsl;
    let port = super::lan::lan_port(state.network_run(), state.bound_port());
    let network = tokio::task::spawn_blocking(move || {
        let ips = probe.ips();
        let view = super::lan::probe(&probe.tools, probe.wsl, super::lan::Needs::ALL, &ips, super::lan::PROBE_DEADLINE);
        (view, ips)
    });
    let machine = tokio::task::spawn_blocking(move || {
        let mut items = checker.check();
        let extras = super::doctor::machine_extras(&checker.runner, &items, &paths, wsl);
        items.extend(extras);
        items
    });
    let (network, machine) = tokio::join!(network, machine);
    let mut items = machine.unwrap_or_default();
    if let Ok((view, ips)) = network {
        super::doctor::place_network_items(&mut items, &view, saved, port, &ips);
    }
    Json(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Sentinela (spec 113 T01): o executor real dos prerequisitos não chama o docker nos testes.
    #[test]
    fn real_runner_refuses_real_docker_in_tests() {
        if std::env::var("NANTS_TEST_REAL_DOCKER").is_ok_and(|v| v == "1") {
            return;
        }
        let run = real_runner(Duration::from_secs(5));
        assert!(run("docker", &["info"]).is_none());
        assert!(run("sh", &["-c", "true"]).is_some_and(|o| o.success));
    }

    fn ok(s: &str) -> Option<CmdOutput> {
        Some(CmdOutput { success: true, stdout: s.into(), stderr: String::new() })
    }
    fn fail(e: &str) -> Option<CmdOutput> {
        Some(CmdOutput { success: false, stdout: String::new(), stderr: e.into() })
    }
    fn runner(f: impl Fn(&str, &[&str]) -> Option<CmdOutput> + Send + Sync + 'static) -> CmdRunner {
        Arc::new(f)
    }
    fn by_id<'a>(v: &'a [PrereqItem], id: &str) -> &'a PrereqItem {
        v.iter().find(|i| i.id == id).expect("item")
    }

    #[test]
    fn all_ok() {
        let r = runner(|p, a| match (p, a.first().copied()) {
            ("git", _) => ok("git version 2.43.0\n"),
            ("docker", Some("--version")) => ok("Docker version 29.0.0, build x"),
            ("docker", Some("info")) => ok("29.0.0\n"),
            _ => ok("2.40.0\n"),
        });
        let v = diagnose(&r);
        assert_eq!(v.len(), 5);
        assert!(v.iter().all(|i| i.ok && i.fix.is_none()));
        assert_eq!(by_id(&v, "git").version.as_deref(), Some("git version 2.43.0"));
        assert_eq!(by_id(&v, "docker_compose").version.as_deref(), Some("2.40.0"));
    }

    #[test]
    fn docker_missing_marks_dependents() {
        let r = runner(|p, _| if p == "git" { ok("git version 2") } else { None });
        let v = diagnose(&r);
        assert!(by_id(&v, "git").ok);
        for id in ["docker_cli", "docker_daemon", "docker_permission", "docker_compose"] {
            let i = by_id(&v, id);
            assert!(!i.ok && i.fix.is_some(), "{id}");
        }
    }

    #[test]
    fn permission_denied_suggests_usermod() {
        let r = runner(|p, a| match (p, a.first().copied()) {
            ("docker", Some("info")) => {
                fail("permission denied while trying to connect to the Docker daemon socket")
            }
            _ => ok("x"),
        });
        let v = diagnose(&r);
        let perm = by_id(&v, "docker_permission");
        assert!(!perm.ok);
        assert!(perm.fix.as_deref().unwrap_or("").contains("usermod -aG docker $USER"));
        assert!(by_id(&v, "docker_compose").ok);
    }

    #[test]
    fn daemon_down_and_git_missing() {
        let r = runner(|p, a| match (p, a.first().copied()) {
            ("git", _) => None,
            ("docker", Some("info")) => fail("Cannot connect to the Docker daemon"),
            _ => ok("x"),
        });
        let v = diagnose(&r);
        assert!(!by_id(&v, "git").ok);
        assert!(!by_id(&v, "docker_daemon").ok);
        assert!(by_id(&v, "docker_cli").ok);
    }

    #[test]
    fn api_json_has_the_neutral_detail_and_keeps_the_pt_br_text() {
        let r = runner(|p, _| if p == "git" { ok("git version 2") } else { None });
        let v = diagnose(&r);
        let cli = serde_json::to_value(by_id(&v, "docker_cli")).expect("json");
        assert_eq!(cli["detail"], "docker não encontrado");
        assert_eq!(cli["detail_code"], "docker_missing");
        assert_eq!(cli["detail_params"], serde_json::json!({}));
        assert!(cli.get("detail_parts").is_none());
        assert!(cli["fix"].as_str().unwrap_or("").starts_with("Instale o Docker Engine"));
        assert!(cli.get("info").is_none() && cli.get("detail_msg").is_none());
        let daemon = serde_json::to_value(by_id(&v, "docker_daemon")).expect("json");
        assert_eq!(daemon["detail_code"], "depends_on_cli");
        let git = serde_json::to_value(by_id(&v, "git")).expect("json");
        assert_eq!(git, serde_json::json!({"id": "git", "label": "Git", "ok": true, "version": "git version 2"}));
        // Parâmetros passam pelo mesmo saneamento das mensagens com `code`.
        let mut item = by_id(&v, "docker_cli").clone();
        item.detail_msg = Some(Msg::new("doctor.detail.playwright_some").arg("tags", "a\u{202e}b"));
        let json = serde_json::to_value(&item).expect("json");
        assert_eq!(json["detail_params"]["tags"], "a b");
        item.detail_msg = Some(Msg::new("Código Inválido"));
        assert!(serde_json::to_value(&item).expect("json").get("detail_code").is_none());
    }

    #[test]
    fn cache_reuses_result_within_ttl() {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let c = PrereqChecker::new(runner(move |_, _| {
            n2.fetch_add(1, Ordering::SeqCst);
            ok("x")
        }));
        c.check();
        let after_first = n.load(Ordering::SeqCst);
        c.check();
        assert_eq!(n.load(Ordering::SeqCst), after_first);
        let c0 = c.clone().with_ttl(Duration::ZERO);
        c0.check();
        assert!(n.load(Ordering::SeqCst) > after_first);
    }

    #[test]
    fn real_runner_missing_binary_is_none() {
        let r = real_runner(Duration::from_secs(1));
        assert!(r("nants-binario-inexistente-xyz", &[]).is_none());
    }

    #[test]
    fn real_runner_timeout_kills() {
        let r = real_runner(Duration::from_millis(100));
        let t = Instant::now();
        assert!(r("sleep", &["5"]).is_none());
        assert!(t.elapsed() < Duration::from_secs(3));
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::server::router;
    use axum::body::Body;
    use axum::http::{header, Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn route_returns_items_json() {
        let r: CmdRunner = Arc::new(|_, _| {
            Some(CmdOutput { success: true, stdout: "v1".into(), stderr: String::new() })
        });
        let state = AppState::new(
            std::path::PathBuf::from("config.toml"),
            crate::config::Config::default(),
            3000,
        )
        .with_prereqs(PrereqChecker::new(r));
        let req = Request::builder()
            .uri("/api/system/prereqs")
            .header(header::HOST, "127.0.0.1:3000")
            .body(Body::empty())
            .expect("req");
        let resp = router(state).oneshot(req).await.expect("resp");
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("body");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let ids: Vec<&str> = v.as_array().expect("array").iter().filter_map(|i| i["id"].as_str()).collect();
        assert_eq!(
            ids,
            [
                "git",
                "docker_cli",
                "docker_daemon",
                "docker_permission",
                "docker_compose",
                "disk",
                "wsl",
                "windows_port_redirect",
                "playwright_images"
            ]
        );
        assert_eq!(v[0]["ok"], true);
        assert!(v[0].get("info").is_none(), "itens obrigatórios não levam info");
        assert_eq!(v[6]["info"], true);
        assert_eq!(v[8]["info"], true);
        assert!(v[8]["detail"].as_str().unwrap_or("").contains("v1"));
        // Spec 102 T05: o detalhe também na forma neutra de idioma.
        assert_eq!(v[8]["detail_code"], "playwright_some");
        assert_eq!(v[8]["detail_params"], serde_json::json!({"tags": "v1"}));
        // Spec 111: o ambiente e a rede local, como no `nants doctor` (os testes rodam como Linux).
        assert_eq!(v[6]["detail_code"], "wsl_absent", "{}", v[6]);
        assert_eq!(v[7]["detail_code"], "lan_linux_off", "{}", v[7]);
        assert_eq!(v[7]["detail_params"], serde_json::json!({"port": "3000"}));
        assert_eq!((v[7]["ok"].as_bool(), v[7]["info"].as_bool()), (Some(true), Some(true)));
        assert!(v.as_array().expect("array").iter().all(|i| i.get("warn").is_none()), "sem aviso no Linux");
        match v[5]["detail_code"].as_str() {
            Some("disk") => {
                let parts = v[5]["detail_parts"].as_array().expect("partes");
                assert!(!parts.is_empty());
                assert!(parts.iter().all(|p| p["code"] == "disk_free" && p["params"]["size_bytes"].is_u64() && p["params"]["path"].is_string()));
            }
            other => assert_eq!(other, Some("disk_unmeasured"), "{}", v[5]),
        }
        assert!(v[0].get("detail_code").is_none(), "item ok sem detalhe não leva código");
    }
}
