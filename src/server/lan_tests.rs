//! Testes do acesso pela rede local (spec 111, T02): ambiente, regra de firewall, decisão do modo,
//! adoção, escolha de porta, conferência em paralelo e a API. Nada consulta o Windows de verdade:
//! o executor é falso. Pastas temporárias com guarda `Drop`.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::server::portproxy::NETSH_ARGS;
use crate::server::prereqs::CmdOutput;
use crate::server::{router, AppState};

const THIS_WSL: Ipv4Addr = Ipv4Addr::new(172, 21, 154, 252);
const TABLE: &str = "\r\nEscuta em ipv4:             Conectar-se a ipv4:\r\n\r\nEndereço        Porta       Endereço        Porta\r\n--------------- ----------  --------------- ----------\r\n0.0.0.0         3000        172.21.154.252  3000\r\n0.0.0.0         3001        172.21.154.252  3001\r\n0.0.0.0         4000        172.21.150.1    4000\r\n";

fn ok(s: &str) -> Option<CmdOutput> {
    Some(CmdOutput { success: true, stdout: s.into(), stderr: String::new() })
}

/// Pasta temporária própria, apagada no fim (passe ou falhe).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("nants-lan-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("pasta temporária");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Windows falso: saídas de `netsh`, `wslinfo` e PowerShell (`None` = não executou), com atraso
/// opcional do PowerShell. Só aceita as consultas de leitura.
fn windows(netsh: Option<&'static str>, mode: Option<&'static str>, rules: Option<&'static str>, ps_delay: Duration) -> WinTools {
    WinTools {
        run: Arc::new(move |p, a| match p {
            "netsh.exe" => {
                assert_eq!(a, NETSH_ARGS, "só show");
                netsh.and_then(ok)
            }
            "wslinfo" => {
                assert_eq!(a, WSLINFO_ARGS);
                mode.and_then(ok)
            }
            "powershell.exe" => {
                assert_eq!(a, FIREWALL_ARGS, "só Get-NetFirewallRule");
                std::thread::sleep(ps_delay);
                rules.and_then(ok)
            }
            other => panic!("programa inesperado: {other}"),
        }),
        exists: Arc::new(|p| ["netsh.exe", "wslinfo", "powershell.exe"].contains(&p)),
    }
}

#[test]
fn environment_from_wslinfo_and_failures() {
    assert_eq!(env_from_output(ok("nat\n")), NetworkEnv::WslNat);
    assert_eq!(env_from_output(ok("Mirrored\r\n")), NetworkEnv::WslMirrored);
    assert_eq!(env_from_output(ok("virtioproxy")), NetworkEnv::WslUnknown);
    assert_eq!(env_from_output(ok("")), NetworkEnv::WslUnknown);
    assert_eq!(env_from_output(Some(CmdOutput { success: false, stdout: "nat".into(), stderr: String::new() })), NetworkEnv::WslUnknown);
    assert_eq!(env_from_output(None), NetworkEnv::WslUnknown);
    let tools = windows(None, Some("mirrored"), None, Duration::ZERO);
    assert_eq!(detect_env(&tools, true), NetworkEnv::WslMirrored);
    // Fora do WSL não executa nada.
    let never = WinTools { run: Arc::new(|p, _| panic!("executou {p}")), exists: Arc::new(|_| true) };
    assert_eq!(detect_env(&never, false), NetworkEnv::Linux);
    assert_eq!(firewall_with(&never, false), FirewallState::Absent);
    // Sem wslinfo em lugar nenhum: desconhecido, tratado como NAT nas instruções.
    let missing = WinTools { run: Arc::new(|p, _| panic!("executou {p}")), exists: Arc::new(|_| false) };
    assert_eq!(detect_env(&missing, true), NetworkEnv::WslUnknown);
    assert!(NetworkEnv::WslUnknown.nat_like() && NetworkEnv::WslNat.nat_like() && !NetworkEnv::WslMirrored.nat_like());
    assert_eq!(firewall_with(&missing, true), FirewallState::Unknown);
}

#[test]
fn firewall_rule_present_absent_or_unknown() {
    assert_eq!(firewall_from_output(ok("1\r\n")), FirewallState::Present);
    assert_eq!(firewall_from_output(ok("2")), FirewallState::Present);
    assert_eq!(firewall_from_output(ok("0\r\n")), FirewallState::Absent);
    assert_eq!(firewall_from_output(ok("Get-NetFirewallRule : erro")), FirewallState::Unknown);
    assert_eq!(firewall_from_output(Some(CmdOutput { success: false, stdout: String::new(), stderr: "x".into() })), FirewallState::Unknown);
    assert_eq!(firewall_from_output(None), FirewallState::Unknown);
    assert!(FIREWALL_QUERY.starts_with("try { @(Get-NetFirewallRule -DisplayName 'Nants'"));
    for verb in ["New-", "Set-", "Remove-", "Enable-", "Disable-", "netsh"] {
        assert!(!FIREWALL_QUERY.contains(verb), "só leitura: {verb}");
    }
}

#[test]
fn redirect_target_this_wsl_other_none_unknown() {
    let table = portproxy::redirects(TABLE).expect("tabela");
    assert_eq!(target_of(Some(&table), 3000, &[THIS_WSL]), RedirectTarget::ThisWsl);
    assert_eq!(target_of(Some(&table), 4000, &[THIS_WSL]), RedirectTarget::Other);
    assert_eq!(target_of(Some(&table), 3005, &[THIS_WSL]), RedirectTarget::None);
    assert_eq!(target_of(None, 3000, &[THIS_WSL]), RedirectTarget::Unknown);
    // O IP do WSL mudou: a mesma tabela aponta para outro endereço.
    assert_eq!(target_of(Some(&table), 3000, &[Ipv4Addr::new(172, 30, 0, 2)]), RedirectTarget::Other);
}

#[test]
fn mode_decision_covers_every_combination() {
    use ModeSource::*;
    for saved in [None, Some(false), Some(true)] {
        for adoptable in [false, true] {
            // `--lan` vence tudo.
            assert_eq!(decide_mode(true, false, saved, adoptable), ModeDecision { lan: true, source: Flag });
            // `--host` explícito vence a opção salva e a adoção.
            assert_eq!(decide_mode(false, true, saved, adoptable), ModeDecision { lan: false, source: Host });
            let got = decide_mode(false, false, saved, adoptable);
            let want = match (saved, adoptable) {
                (Some(v), _) => ModeDecision { lan: v, source: Saved },
                (None, true) => ModeDecision { lan: true, source: Adopted },
                (None, false) => ModeDecision { lan: false, source: Default },
            };
            assert_eq!(got, want, "{saved:?} {adoptable}");
        }
    }
    // A escolha de desligar vale: nunca há adoção com `lan = false`.
    assert!(!decide_mode(false, false, Some(false), true).lan);
}

#[test]
fn adoption_needs_nat_redirect_to_this_wsl_and_the_rule() {
    use FirewallState::*;
    use NetworkEnv::*;
    use RedirectTarget::{None as NoRedirect, Other, ThisWsl, Unknown as TUnknown};
    for env in [Linux, WslNat, WslMirrored, WslUnknown] {
        for target in [ThisWsl, Other, NoRedirect, TUnknown] {
            for fw in [Present, Absent, Unknown] {
                let want = env == WslNat && target == ThisWsl && fw == Present;
                assert_eq!(adoption_applies(env, target, fw), want, "{env:?} {target:?} {fw:?}");
            }
        }
    }
}

#[test]
fn startup_only_checks_what_the_decision_needs() {
    assert_eq!(startup_needs(true, false, None, 3000), None, "--lan");
    assert_eq!(startup_needs(false, true, None, 3000), None, "--host");
    assert_eq!(startup_needs(false, false, Some(true), 3000), None, "ligada");
    assert_eq!(
        startup_needs(false, false, Some(false), 3000),
        Some(Needs { env: false, table: true, firewall: FirewallNeed::Skip }),
        "desligada: só a tabela, para pular a porta"
    );
    assert_eq!(
        startup_needs(false, false, None, 3000),
        Some(Needs { env: true, table: true, firewall: FirewallNeed::IfAdoptable { port: 3000 } })
    );
}

#[test]
fn port_choice_skips_redirected_ports() {
    assert_eq!(candidate_ports(3000, 3, &[]), vec![3000, 3001, 3002, 3003]);
    assert_eq!(candidate_ports(3000, 3, &[3000, 3001]), vec![3002, 3003]);
    assert_eq!(candidate_ports(3000, 20, &(3000..=3010).collect::<Vec<_>>()).first(), Some(&3011));
    assert_eq!(candidate_ports(u16::MAX, 20, &[]), vec![u16::MAX]);
    let table = portproxy::redirects(TABLE).expect("tabela");
    assert_eq!(redirected_ports(Some(&table)), vec![3000, 3001, 4000]);
    assert!(redirected_ports(None).is_empty(), "sem conferência, nada muda");
}

#[test]
fn probe_runs_in_parallel_and_skips_powershell_when_adoption_is_impossible() {
    let slow = Duration::from_millis(600);
    // Tudo junto: o prazo é o da consulta mais lenta, não a soma.
    let tools = windows(Some(TABLE), Some("nat"), Some("1"), slow);
    let start = Instant::now();
    let view = probe(&tools, true, Needs::ALL, &[THIS_WSL], PROBE_DEADLINE);
    assert!(start.elapsed() < slow * 2, "{:?}", start.elapsed());
    assert_eq!(view.env, NetworkEnv::WslNat);
    assert_eq!(view.firewall, FirewallState::Present);
    assert_eq!(view.redirect(3000), RedirectState::Redirected);
    assert_eq!(view.target(3000, &[THIS_WSL]), RedirectTarget::ThisWsl);
    // Sem redirecionamento da porta, a adoção é impossível: não espera o PowerShell.
    let tools = windows(Some(""), Some("nat"), Some("1"), Duration::from_secs(2));
    let start = Instant::now();
    let needs = Needs { env: true, table: true, firewall: FirewallNeed::IfAdoptable { port: 3000 } };
    let view = probe(&tools, true, needs, &[THIS_WSL], PROBE_DEADLINE);
    assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
    assert_eq!(view.firewall, FirewallState::Unknown);
    assert_eq!(view.redirect(3000), RedirectState::NotRedirected);
    // Possível pelo resto: espera a regra.
    let tools = windows(Some(TABLE), Some("nat"), Some("1"), Duration::from_millis(50));
    assert_eq!(probe(&tools, true, needs, &[THIS_WSL], PROBE_DEADLINE).firewall, FirewallState::Present);
    // Prazo total curto: o que não respondeu fica desconhecido.
    let tools = windows(Some(TABLE), Some("nat"), Some("1"), Duration::from_secs(2));
    let start = Instant::now();
    let view = probe(&tools, true, Needs::ALL, &[THIS_WSL], Duration::from_millis(300));
    assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
    assert_eq!(view.firewall, FirewallState::Unknown);
    // Fora do WSL: nada roda.
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let never = WinTools {
        run: Arc::new(move |_, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            None
        }),
        exists: Arc::new(|_| true),
    };
    let view = probe(&never, false, Needs::ALL, &[], PROBE_DEADLINE);
    assert_eq!(view.env, NetworkEnv::Linux);
    assert_eq!(view.redirect(3000), RedirectState::NotRedirected);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

const HOST: &str = "127.0.0.1:3000";

fn req(method: Method, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("requisição")
}

/// Chama a rota; o `Host` padrão passa a usar a porta ligada do estado.
async fn call(state: &AppState, mut request: Request<Body>) -> (StatusCode, Value) {
    if request.headers().get(header::HOST).is_some_and(|h| h == HOST) {
        let host = format!("127.0.0.1:{}", state.bound_port());
        request.headers_mut().insert(header::HOST, host.parse().expect("host"));
    }
    let resp = router(state.clone()).oneshot(request).await.expect("resposta");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.expect("corpo");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn wsl_probe(netsh: Option<&'static str>, mode: Option<&'static str>, rules: Option<&'static str>) -> NetProbe {
    NetProbe { tools: windows(netsh, mode, rules, Duration::ZERO), wsl: true, local_ips: Some(vec![THIS_WSL]) }
}

#[tokio::test]
async fn api_reports_environment_option_mode_ports_and_windows() {
    let dir = TempDir::new("api");
    let state = AppState::new(dir.path().join("config.toml"), Config::default(), 3000)
        .with_net_probe(wsl_probe(Some(TABLE), Some("nat"), Some("1")));
    let (status, body) = call(&state, req(Method::GET, "/api/config/network", "")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({
            "env": "wsl_nat",
            "saved_lan": null,
            "lan": false,
            "mode_source": "default",
            "port": 3000,
            "lan_port": 3000,
            "view_ports": {"start": 3001, "end": 3010},
            "windows_redirect": "yes",
            "windows_redirect_target": "this_wsl",
            "coherence": "will_adopt",
            "firewall_rule": "yes",
            "restart_required": false,
            "restart_command": "nants",
        })
    );
    // Modo desta execução vindo da adoção, com a porta pulada (pedida 3000, ligada 3002).
    let mut cfg = Config::default();
    cfg.server.lan = Some(true);
    let run = NetworkRun { lan: true, source: ModeSource::Adopted, requested_port: 3000 };
    let state = AppState::new(dir.path().join("config.toml"), cfg, 3002)
        .with_net_probe(wsl_probe(Some(""), Some("mirrored"), None))
        .with_network_run(run);
    let (_, body) = call(&state, req(Method::GET, "/api/config/network", "")).await;
    assert_eq!(body["env"], "wsl_mirrored");
    assert_eq!(body["saved_lan"], true);
    assert_eq!(body["mode_source"], "adopted");
    assert_eq!((body["port"].as_u64(), body["lan_port"].as_u64()), (Some(3002), Some(3000)));
    assert_eq!(body["windows_redirect"], "no");
    assert_eq!(body["windows_redirect_target"], "none");
    assert_eq!(body["firewall_rule"], "unknown");
    // Sem interop: tudo desconhecido, sem erro.
    let state = AppState::new(dir.path().join("config.toml"), Config::default(), 3000).with_net_probe(wsl_probe(None, None, None));
    let (status, body) = call(&state, req(Method::GET, "/api/config/network", "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((body["env"].as_str(), body["windows_redirect"].as_str(), body["firewall_rule"].as_str()), (Some("wsl_unknown"), Some("unknown"), Some("unknown")));
    // Linux (o padrão dos testes): nada do Windows.
    let state = AppState::new(dir.path().join("config.toml"), Config::default(), 3000);
    let (_, body) = call(&state, req(Method::GET, "/api/config/network", "")).await;
    assert_eq!((body["env"].as_str(), body["windows_redirect"].as_str()), (Some("linux"), Some("no")));
}

#[tokio::test]
async fn api_follows_the_settings_rules() {
    let dir = TempDir::new("guard");
    let state = AppState::new(dir.path().join("config.toml"), Config::default(), 3000);
    // Host de fora é recusado, como nas outras rotas de Configurações.
    let bad_host = Request::builder()
        .method(Method::GET)
        .uri("/api/config/network")
        .header(header::HOST, "evil.example:3000")
        .body(Body::empty())
        .expect("requisição");
    let (status, _) = call(&state, bad_host).await;
    assert!(status.is_client_error(), "{status}");
    // Mutação sem JSON é recusada.
    let no_json = Request::builder()
        .method(Method::PUT)
        .uri("/api/config/network")
        .header(header::HOST, HOST)
        .body(Body::from(r#"{"lan":true}"#))
        .expect("requisição");
    let (status, _) = call(&state, no_json).await;
    assert!(status.is_client_error(), "{status}");
    for bad in [r#"{"lan":"sim"}"#, r#"{}"#, r#"{"lan":true,"extra":1}"#, "lixo"] {
        let (status, _) = call(&state, req(Method::PUT, "/api/config/network", bad)).await;
        assert!(status.is_client_error(), "{bad}: {status}");
    }
    assert!(!dir.path().join("config.toml").exists(), "recusa não grava");
}

/// Integração: gravar `server.lan` pela rota preserva o resto do arquivo.
#[tokio::test]
async fn saving_the_option_keeps_every_other_key() {
    let dir = TempDir::new("save");
    let path = dir.path().join("config.toml");
    let original = r#"projects_root = "/srv/projetos"
last_yolo = true

[server]
host = "127.0.0.1"
port = 3100
lan_view_ports = { start = 4100, count = 5 }

[[connections]]
id = "c1"
name = "Minha conexão"
base_url = "https://api.example.com/v1"
api_key = "env:NANTS_TESTE_CHAVE"
wire_api = "responses"

[agents.architect]
connection_id = "c1"
model = "m-arq"
effort = "high"
max_steps = 77

[pricing."m-arq"]
input_per_mtok = 1.5
output_per_mtok = 6.0
"#;
    std::fs::write(&path, original).expect("config");
    let before = Config::load_from(&path).expect("carrega");
    assert_eq!(before.server.lan, None);
    let state = AppState::new(path.clone(), before.clone(), 3100);
    let (status, body) = call(&state, req(Method::PUT, "/api/config/network", r#"{"lan":true}"#)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"saved_lan": true, "lan": false, "applies_on_next_start": true, "restart_required": true, "restart_command": "nants"})
    );
    let after = Config::load_from(&path).expect("recarrega");
    assert_eq!(after.server.lan, Some(true));
    let mut expected = before.clone();
    expected.server.lan = Some(true);
    assert_eq!(after, expected, "só a opção mudou");
    let raw = std::fs::read_to_string(&path).expect("texto");
    assert!(raw.contains("env:NANTS_TESTE_CHAVE") && raw.contains("lan = true"), "{raw}");
    assert_eq!(state.config().read().await.server.lan, Some(true), "memória também");
    // Desligar grava `false` (a escolha vale e impede a adoção).
    let (status, body) = call(&state, req(Method::PUT, "/api/config/network", r#"{"lan":false}"#)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restart_required"], false);
    let after = Config::load_from(&path).expect("recarrega");
    assert_eq!(after.server.lan, Some(false));
    expected.server.lan = Some(false);
    assert_eq!(after, expected);
}

/// `GET /api/system/prereqs` (Primeiros passos) com os mesmos itens do `nants doctor`: o ambiente
/// no lugar do `wsl` e, logo depois, o acesso pela rede local, com `warn` quando difere do Windows.
#[tokio::test]
async fn prereqs_api_shows_environment_and_network_like_the_doctor() {
    let dir = TempDir::new("prereqs");
    let machine = crate::server::prereqs::PrereqChecker::new(Arc::new(|_: &str, _: &[&str]| ok("v1")));
    let item = |body: &Value, id: &str| -> Value {
        body.as_array().and_then(|a| a.iter().find(|i| i["id"] == id).cloned()).unwrap_or(Value::Null)
    };
    let ids = |body: &Value| -> Vec<String> {
        body.as_array().map(|a| a.iter().filter_map(|i| i["id"].as_str().map(String::from)).collect()).unwrap_or_default()
    };
    // Rede ligada e o Windows de acordo (NAT, redirecionada para este WSL, regra presente): OK.
    let mut cfg = Config::default();
    cfg.server.lan = Some(true);
    let state = AppState::new(dir.path().join("config.toml"), cfg, 3000)
        .with_prereqs(machine.clone())
        .with_net_probe(wsl_probe(Some(TABLE), Some("nat"), Some("1")));
    let (status, body) = call(&state, req(Method::GET, "/api/system/prereqs", "")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order = ids(&body);
    let wsl_at = order.iter().position(|i| i == "wsl").expect("wsl");
    assert_eq!(order.get(wsl_at + 1).map(String::as_str), Some("windows_port_redirect"), "{order:?}");
    assert_eq!(item(&body, "wsl")["detail_code"], "env_wsl_nat");
    let lan = item(&body, "windows_port_redirect");
    assert_eq!(lan["detail_code"], "lan_on_ok", "{lan}");
    assert_eq!(lan["ok"], true);
    assert!(lan.get("warn").is_none(), "{lan}");
    // Rede desligada com o Windows ainda redirecionando: aviso que não bloqueia, com o que fazer.
    let mut cfg = Config::default();
    cfg.server.lan = Some(false);
    let state = AppState::new(dir.path().join("config.toml"), cfg, 3001)
        .with_prereqs(machine.clone())
        .with_net_probe(wsl_probe(Some(TABLE), Some("nat"), Some("1")))
        .with_network_run(NetworkRun { lan: false, source: ModeSource::Saved, requested_port: 3000 });
    let (_, body) = call(&state, req(Method::GET, "/api/system/prereqs", "")).await;
    let lan = item(&body, "windows_port_redirect");
    assert_eq!(lan["detail_code"], "windows_redirect_on", "{lan}");
    assert_eq!((lan["ok"].as_bool(), lan["warn"].as_bool()), (Some(true), Some(true)), "{lan}");
    assert!(lan["fix"].as_str().is_some_and(|f| !f.is_empty()), "{lan}");
    // Espelhado sem a regra de firewall: aviso; sem interop: só informa, sem aviso.
    let mut cfg = Config::default();
    cfg.server.lan = Some(true);
    let state = AppState::new(dir.path().join("config.toml"), cfg.clone(), 3000)
        .with_prereqs(machine.clone())
        .with_net_probe(wsl_probe(Some(""), Some("mirrored"), Some("0")));
    let (_, body) = call(&state, req(Method::GET, "/api/system/prereqs", "")).await;
    assert_eq!(item(&body, "wsl")["detail_code"], "env_wsl_mirrored");
    assert_eq!(item(&body, "windows_port_redirect")["detail_code"], "lan_on_no_firewall");
    let state = AppState::new(dir.path().join("config.toml"), cfg, 3000)
        .with_prereqs(machine)
        .with_net_probe(wsl_probe(None, None, None));
    let (_, body) = call(&state, req(Method::GET, "/api/system/prereqs", "")).await;
    assert_eq!(item(&body, "wsl")["detail_code"], "env_wsl_unknown");
    let lan = item(&body, "windows_port_redirect");
    assert_eq!((lan["detail_code"].as_str(), lan["info"].as_bool()), (Some("windows_redirect_unknown"), Some(true)), "{lan}");
    assert!(lan.get("warn").is_none());
}

/// Coerência (revisão da T03, F-01): a regra única do servidor, também no campo `coherence` da API.
#[test]
fn coherence_codes_follow_the_doctor_rule() {
    use FirewallState::{Absent, Present, Unknown as FwUnknown};
    use NetworkEnv::{Linux, WslMirrored, WslNat, WslUnknown};
    use RedirectTarget::{None as NoRedirect, Other, ThisWsl, Unknown};
    let cases = [
        // Nada salvo com a adoção possível: o próximo início liga; nada de desfazer.
        (WslNat, None, ThisWsl, Present, "will_adopt"),
        // Nada salvo, redirecionada para outro endereço (sem adoção): desfazer.
        (WslNat, None, Other, Present, "still_redirected"),
        (WslNat, None, ThisWsl, Absent, "still_redirected"),
        (WslUnknown, None, ThisWsl, Present, "still_redirected"),
        // Desligada com redirecionamento: desfazer; sem redirecionamento: ok.
        (WslNat, Some(false), ThisWsl, Present, "still_redirected"),
        (WslNat, Some(false), NoRedirect, Absent, "ok"),
        (WslNat, None, NoRedirect, FwUnknown, "ok"),
        // Ligada no NAT sem redirecionamento: falta o script.
        (WslNat, Some(true), NoRedirect, Present, "needs_script"),
        (WslNat, Some(true), Other, Present, "other_target"),
        (WslNat, Some(true), ThisWsl, Absent, "missing_rule"),
        (WslNat, Some(true), ThisWsl, Present, "ok"),
        (WslNat, Some(true), ThisWsl, FwUnknown, "ok"),
        (WslMirrored, Some(true), NoRedirect, Absent, "missing_rule"),
        (WslMirrored, Some(true), NoRedirect, Present, "ok"),
        (WslMirrored, Some(true), NoRedirect, FwUnknown, "unknown"),
        (WslNat, Some(true), Unknown, Present, "unknown"),
        (WslNat, Some(false), Unknown, FwUnknown, "unknown"),
        (Linux, Some(true), NoRedirect, Absent, "ok"),
        (Linux, None, NoRedirect, Absent, "ok"),
    ];
    for (env, saved, target, firewall, expected) in cases {
        assert_eq!(network_check(env, saved, target, firewall).coherence(), expected, "{env:?} {saved:?} {target:?} {firewall:?}");
    }
}

#[tokio::test]
async fn api_reports_coherence_from_the_server_rule() {
    let dir = TempDir::new("coherence");
    // Execução com --lan e nada salvo, com o Windows já configurado: adoção, sem pedir desfazer.
    let run = NetworkRun { lan: true, source: ModeSource::Flag, requested_port: 3000 };
    let state = AppState::new(dir.path().join("config.toml"), Config::default(), 3000)
        .with_net_probe(wsl_probe(Some(TABLE), Some("nat"), Some("1")))
        .with_network_run(run);
    let (_, body) = call(&state, req(Method::GET, "/api/config/network", "")).await;
    assert_eq!(body["coherence"], "will_adopt", "{body}");
    // Desligada com redirecionamento: desfazer.
    let mut cfg = Config::default();
    cfg.server.lan = Some(false);
    let state = AppState::new(dir.path().join("config.toml"), cfg, 3000).with_net_probe(wsl_probe(Some(TABLE), Some("nat"), Some("1")));
    let (_, body) = call(&state, req(Method::GET, "/api/config/network", "")).await;
    assert_eq!(body["coherence"], "still_redirected", "{body}");
    // Ligada no NAT sem redirecionamento: falta o script.
    let mut cfg = Config::default();
    cfg.server.lan = Some(true);
    let state = AppState::new(dir.path().join("config.toml"), cfg, 3000).with_net_probe(wsl_probe(Some(""), Some("nat"), Some("1")));
    let (_, body) = call(&state, req(Method::GET, "/api/config/network", "")).await;
    assert_eq!(body["coherence"], "needs_script", "{body}");
    // Nada salvo com a porta redirecionada para outro endereço: desfazer (sem adoção).
    let state = AppState::new(dir.path().join("config.toml"), Config::default(), 4000).with_net_probe(wsl_probe(Some(TABLE), Some("nat"), Some("1")));
    let (_, body) = call(&state, req(Method::GET, "/api/config/network", "")).await;
    assert_eq!((body["windows_redirect_target"].as_str(), body["coherence"].as_str()), (Some("other"), Some("still_redirected")), "{body}");
}
