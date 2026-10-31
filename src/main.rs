use std::env;
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use nants::config::{self, Config, ConfigError};
use nants::i18n::{text, tr, Lang};
use nants::server::{self, AppState};
use tokio::net::TcpListener;

const DEFAULT_HOST: &str = "127.0.0.1";
/// Quantas portas seguintes são tentadas quando a padrão está ocupada (spec 103).
const PORT_FALLBACK_TRIES: u16 = server::doctor::PORT_FALLBACK_TRIES;

/// Argumentos de `nants serve`.
#[derive(Debug, Default, PartialEq, Eq)]
struct ServeArgs {
    host: Option<String>,
    port: Option<u16>,
    config: Option<PathBuf>,
    allow_remote: bool,
    lan: bool,
    no_open: bool,
}

/// Ajuda do executável no idioma do terminal (spec 102).
fn usage(lang: Lang) -> String {
    text(lang, "cli.usage")
}

/// Interpreta os argumentos de `nants serve` sem dependências externas.
fn parse_serve_args(args: &[String], lang: Lang) -> Result<ServeArgs, String> {
    let mut parsed = ServeArgs::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--port" => {
                let value = next_value(&mut iter, "--port", lang)?;
                parsed.port = Some(
                    value
                        .parse()
                        .map_err(|_| tr(lang, "cli.bad_port", &[("value", &value)]))?,
                );
            }
            "--host" => parsed.host = Some(next_value(&mut iter, "--host", lang)?),
            "--config" => parsed.config = Some(PathBuf::from(next_value(&mut iter, "--config", lang)?)),
            "--allow-remote" => parsed.allow_remote = true,
            "--lan" => parsed.lan = true,
            "--no-open" => parsed.no_open = true,
            other => return Err(tr(lang, "cli.unknown_arg", &[("arg", other)])),
        }
    }
    if parsed.lan && parsed.host.is_some() {
        return Err(text(lang, "cli.lan_with_host"));
    }
    Ok(parsed)
}

fn next_value(iter: &mut std::slice::Iter<'_, String>, flag: &str, lang: Lang) -> Result<String, String> {
    iter.next()
        .cloned()
        .ok_or_else(|| tr(lang, "cli.needs_value", &[("flag", flag)]))
}

/// Descrição segura de um erro de configuração. Erros de TOML não são exibidos
/// com o trecho da linha de origem (que pode conter `api_key`), só a posição.
fn describe_config_error(err: &ConfigError, lang: Lang) -> String {
    match err {
        ConfigError::Parse(e) => match e.span() {
            Some(span) => tr(lang, "cli.config_invalid_at", &[("position", &span.start.to_string())]),
            None => text(lang, "cli.config_invalid"),
        },
        ConfigError::Serialize(_) => text(lang, "cli.config_serialize"),
        ConfigError::Io(e) => tr(lang, "cli.config_io", &[("kind", &e.kind().to_string())]),
        ConfigError::MissingEnv(name) => tr(lang, "cli.config_missing_env", &[("name", name)]),
        ConfigError::HomeDirNotFound => err.to_string(),
    }
}

/// Faixa das portas de visualização (spec 097) para a porta efetiva `port`. Em `--lan`, a
/// configurada ou a padrão; fora dele, só valida a configurada (erro claro na inicialização).
fn view_ports_for(
    lan: bool,
    configured: Option<config::LanViewPortsConfig>,
    port: u16,
) -> Result<Option<config::ViewPorts>, String> {
    if lan || configured.is_some() {
        config::ViewPorts::resolve(configured, port).map(Some)
    } else {
        Ok(None)
    }
}

/// `true` para `localhost`, `127.0.0.0/8` e `::1` (com ou sem colchetes).
fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    // Idioma das mensagens para a pessoa (spec 102): LC_ALL > LC_MESSAGES > LANG. A linha
    // `nants listening on` é invariável (o smoke a lê).
    let lang = Lang::from_env();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("nants {}", nants::version());
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage(lang));
        return ExitCode::SUCCESS;
    }
    match args.first().map(String::as_str) {
        Some("serve") => serve_command(&args[1..], false, lang).await,
        Some("doctor") => doctor_command(&args[1..], lang).await,
        Some("uninstall") => uninstall_command(&args[1..], lang),
        // `nants` sem subcomando (com ou só com opções) = `nants serve` abrindo o navegador.
        None => serve_command(&[], true, lang).await,
        Some(flag) if flag.starts_with("--") => serve_command(&args, true, lang).await,
        Some(other) => {
            eprintln!("{}\n{}", tr(lang, "cli.unknown_command", &[("command", other)]), usage(lang));
            ExitCode::from(2)
        }
    }
}

/// Interpreta os argumentos de `nants doctor` (só `--config PATH`).
fn parse_doctor_args(args: &[String], lang: Lang) -> Result<Option<PathBuf>, String> {
    let mut config = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => config = Some(PathBuf::from(next_value(&mut iter, "--config", lang)?)),
            other => return Err(tr(lang, "cli.unknown_arg", &[("arg", other)])),
        }
    }
    Ok(config)
}

/// `nants doctor`: diagnóstico somente leitura. Sai com 1 se faltar algo obrigatório (Docker,
/// daemon com permissão, Compose v2 ou Git).
async fn doctor_command(args: &[String], lang: Lang) -> ExitCode {
    let config_arg = match parse_doctor_args(args, lang) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}\n{}", tr(lang, "cli.error", &[("message", &e)]), usage(lang));
            return ExitCode::from(2);
        }
    };
    let config_path = match config_arg {
        Some(path) => Ok(path),
        None => config::default_path(),
    };
    let cfg = match &config_path {
        Ok(path) => match Config::load_from(path) {
            Ok(cfg) => cfg,
            Err(e) => {
                let path = path.display().to_string();
                let reason = describe_config_error(&e, lang);
                println!("{}", tr(lang, "cli.doctor_config_unreadable", &[("path", &path), ("reason", &reason)]));
                Config::default()
            }
        },
        Err(e) => {
            println!("{}", tr(lang, "cli.doctor_config_default", &[("reason", &e.to_string())]));
            Config::default()
        }
    };
    let input = server::doctor::DoctorInput {
        runner: server::prereqs::real_runner(server::prereqs::PREREQ_CMD_TIMEOUT),
        windows: server::portproxy::WinTools::real(),
        port: cfg.server.port,
        disk_paths: server::doctor::disk_paths(&cfg),
        wsl: server::netinfo::running_on_wsl(),
        saved_lan: cfg.server.lan,
        local_ips: server::netinfo::local_ipv4_addrs(),
        releases_base: env::var(server::doctor::RELEASES_URL_ENV).ok(),
        local_version: nants::version().to_string(),
        version_timeout: server::doctor::VERSION_CHECK_TIMEOUT,
    };
    let items = server::doctor::collect(input).await;
    print!("{}", server::doctor::format_report(nants::version(), &items, lang));
    if server::doctor::required_ok(&items) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Interpreta os argumentos de `nants uninstall` (só `--purge`).
fn parse_uninstall_args(args: &[String], lang: Lang) -> Result<bool, String> {
    let mut purge = false;
    for arg in args {
        match arg.as_str() {
            "--purge" => purge = true,
            other => return Err(tr(lang, "cli.unknown_arg", &[("arg", other)])),
        }
    }
    Ok(purge)
}

/// Plano do `nants uninstall` pelo ambiente: o executável que o `install.sh` grava, as pastas de
/// configuração e de dados e a pasta dos projetos (da configuração, se legível; senão a padrão).
fn uninstall_plan(purge: bool) -> Option<nants::uninstall::Plan> {
    let home = env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute())?;
    let executable = home.join(".local").join("bin").join("nants");
    let config_path = config::default_path().ok();
    // Configuração que existe mas não pode ser lida: a pasta dos projetos configurada é
    // desconhecida, então nada de cair no padrão (o `--purge` preserva configuração e dados).
    let (cfg, config_unreadable) = match &config_path {
        Some(p) if p.exists() => match Config::load_from(p) {
            Ok(cfg) => (Some(cfg), None),
            Err(_) => (None, Some(p.clone())),
        },
        _ => (Some(Config::default()), None),
    };
    let running_exe = env::current_exe()
        .ok()
        .filter(|exe| exe.exists() && std::fs::canonicalize(exe).ok() != std::fs::canonicalize(&executable).ok());
    Some(nants::uninstall::Plan {
        purge,
        executable,
        config_dir: config_path.and_then(|p| p.parent().map(PathBuf::from)),
        data_dir: config::data_dir(),
        projects_root: cfg.and_then(|c| c.projects_root_path()),
        running_exe,
        config_unreadable,
    })
}

/// `nants uninstall [--purge]`: confirmação lida do terminal; nunca apaga os projetos nem o Docker.
fn uninstall_command(args: &[String], lang: Lang) -> ExitCode {
    let purge = match parse_uninstall_args(args, lang) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}\n{}", tr(lang, "cli.error", &[("message", &e)]), usage(lang));
            return ExitCode::from(2);
        }
    };
    let Some(plan) = uninstall_plan(purge) else {
        eprintln!("{}", tr(lang, "cli.error", &[("message", &text(lang, "uninstall.no_home"))]));
        return ExitCode::FAILURE;
    };
    let mut out = std::io::stdout();
    match nants::uninstall::run(&plan, lang, &mut out, &mut nants::uninstall::confirm_from_tty) {
        nants::uninstall::Outcome::Done => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Abre a porta do servidor. Com a porta escolhida pelo usuário (`--port`, inclusive 0) ou no
/// acesso pela rede local, só tenta a pedida. Sem isso, se a porta estiver ocupada, tenta as
/// `PORT_FALLBACK_TRIES` seguintes e devolve a primeira livre.
///
/// `redirected` são as portas que o Windows redireciona para o WSL (spec 111; vazio fora do modo
/// local no WSL ou quando não deu para conferir): no modo local elas contam como ocupadas, porque
/// o navegador do Windows não abriria a página nelas. Com a porta pedida explicitamente e
/// redirecionada, falha com a explicação.
async fn bind_listener(
    host: &str,
    port: u16,
    allow_fallback: bool,
    redirected: &[u16],
    lang: Lang,
) -> Result<TcpListener, String> {
    let (port_s, host_s) = (port.to_string(), host.to_string());
    let port_redirected = redirected.contains(&port);
    if port_redirected && !allow_fallback {
        return Err(tr(lang, "cli.port_redirected_explicit", &[("port", &port_s)]));
    }
    if !port_redirected {
        match TcpListener::bind((host, port)).await {
            Ok(listener) => return Ok(listener),
            Err(e) if allow_fallback && e.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                return Err(tr(lang, "cli.port_in_use", &[("port", &port_s), ("host", &host_s)]));
            }
            Err(e) => {
                return Err(tr(lang, "cli.bind_failed", &[("host", &host_s), ("port", &port_s), ("error", &e.to_string())]));
            }
        }
    }
    let last = port.saturating_add(PORT_FALLBACK_TRIES);
    for next in server::lan::candidate_ports(port, PORT_FALLBACK_TRIES, redirected).into_iter().filter(|p| *p != port) {
        if let Ok(listener) = TcpListener::bind((host, next)).await {
            let key = if port_redirected { "cli.port_redirected" } else { "cli.port_fallback" };
            println!("{}", tr(lang, key, &[("port", &port_s), ("next", &next.to_string())]));
            return Ok(listener);
        }
    }
    Err(tr(lang, "cli.ports_busy", &[("port", &port_s), ("last", &last.to_string()), ("host", &host_s)]))
}

/// Conferência do Windows ao subir (spec 111): só no WSL e só o que a decisão precisa, em paralelo
/// e com prazo curto. `None` quando nada precisa ser conferido.
async fn startup_windows_view(
    parsed: &ServeArgs,
    saved: Option<bool>,
    port: u16,
    local_ips: &[std::net::Ipv4Addr],
) -> Option<server::lan::WindowsView> {
    if !server::netinfo::running_on_wsl() || port == 0 {
        return None;
    }
    let needs = server::lan::startup_needs(parsed.lan, parsed.host.is_some(), saved, port)?;
    let ips = local_ips.to_vec();
    tokio::task::spawn_blocking(move || {
        let tools = server::portproxy::WinTools::real();
        server::lan::probe(&tools, true, needs, &ips, server::lan::PROBE_DEADLINE)
    })
    .await
    .ok()
}

/// URL para abrir no navegador: sempre por loopback quando o servidor escuta em todas as
/// interfaces (`--lan`); senão, o próprio endereço ligado.
fn browser_url(addr: std::net::SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        format!("http://127.0.0.1:{}", addr.port())
    } else {
        format!("http://{addr}")
    }
}

/// Link de entrada de uso único deste computador (spec 112), no endereço que o navegador abre
/// (loopback quando a escuta é em todas as interfaces).
fn entry_link_url(addr: std::net::SocketAddr, token: &str) -> String {
    format!("{}{}?t={token}", browser_url(addr), server::devices::PAIR_LINK_PATH)
}

/// A escuta exige dispositivo autorizado (spec 112): qualquer endereço fora do loopback.
fn listen_requires_device(addr: std::net::SocketAddr) -> bool {
    !addr.ip().to_canonical().is_loopback()
}

async fn serve_command(args: &[String], open_browser: bool, lang: Lang) -> ExitCode {
    let err = |message: &str| tr(lang, "cli.error", &[("message", message)]);
    let parsed = match parse_serve_args(args, lang) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("{}\n{}", err(&e), usage(lang));
            return ExitCode::from(2);
        }
    };
    let open_browser = open_browser && !parsed.no_open;
    let explicit_port = parsed.port.is_some();
    if let Some(host) = parsed.host.as_deref().filter(|h| !is_loopback_host(h)) {
        if !parsed.allow_remote {
            eprintln!("{}", err(&tr(lang, "cli.not_loopback", &[("host", host)])));
            return ExitCode::from(2);
        }
        eprintln!("{}", tr(lang, "cli.exposed", &[("host", host)]));
    }

    let config_path = match parsed.config.clone() {
        Some(path) => path,
        None => match config::default_path() {
            Ok(path) => path,
            Err(e) => {
                eprintln!("{}", err(&e.to_string()));
                return ExitCode::FAILURE;
            }
        },
    };
    let mut cfg = match Config::load_from(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            let path = config_path.display().to_string();
            let reason = describe_config_error(&e, lang);
            eprintln!("{}", tr(lang, "cli.config_load_failed", &[("path", &path), ("reason", &reason)]));
            return ExitCode::FAILURE;
        }
    };
    let port = parsed.port.unwrap_or(cfg.server.port);

    // Modo desta execução (spec 111): `--lan`/`--host`, a opção salva, a adoção ou o modo local.
    let local_ips = server::netinfo::local_ipv4_addrs();
    let windows = startup_windows_view(&parsed, cfg.server.lan, port, &local_ips).await;
    let adoptable = windows
        .as_ref()
        .is_some_and(|v| server::lan::adoption_applies(v.env, v.target(port, &local_ips), v.firewall));
    let decision = server::lan::decide_mode(parsed.lan, parsed.host.is_some(), cfg.server.lan, adoptable);
    if decision.source == server::lan::ModeSource::Adopted {
        cfg.server.lan = Some(true);
        if let Err(e) = server::config_api::save_atomic(&config_path, &cfg) {
            eprintln!("{}", tr(lang, "cli.lan_save_failed", &[("error", &describe_config_error(&e, lang))]));
        }
        println!("{}", text(lang, "cli.lan_adopted"));
    }
    let lan = decision.lan;
    let host = if lan {
        "0.0.0.0".to_string()
    } else {
        parsed.host.clone().unwrap_or_else(|| DEFAULT_HOST.to_string())
    };
    // No modo local, as portas redirecionadas pelo Windows contam como ocupadas.
    let redirected = match (&windows, lan) {
        (Some(view), false) => server::lan::redirected_ports(view.table.as_deref()),
        _ => Vec::new(),
    };
    // Faixa das portas de visualização (spec 097): validada antes de abrir a porta. Com
    // `--port 0` a porta efetiva só é conhecida depois do bind e a faixa é validada de novo lá.
    if port != 0 {
        if let Err(e) = view_ports_for(lan, cfg.server.lan_view_ports, port) {
            eprintln!("{}", err(&e));
            return ExitCode::from(2);
        }
    }
    let configured_view_ports = cfg.server.lan_view_ports;

    let listener = match bind_listener(&host, port, !explicit_port && !lan && port != 0, &redirected, lang).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("{}", err(&e));
            return ExitCode::FAILURE;
        }
    };
    let addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("{}", tr(lang, "cli.local_addr_failed", &[("error", &e.to_string())]));
            return ExitCode::FAILURE;
        }
    };
    println!("nants listening on http://{addr}");
    let view_ports = match view_ports_for(lan, configured_view_ports, addr.port()) {
        Ok(range) => range,
        Err(e) => {
            eprintln!("{}", err(&e));
            return ExitCode::from(2);
        }
    };
    let run = server::lan::NetworkRun { lan, source: decision.source, requested_port: port };
    let exposed = listen_requires_device(addr);
    let mut state = AppState::new(config_path, cfg, addr.port()).with_network_run(run).with_exposed(exposed);
    if lan {
        for ip in server::netinfo::local_ipv4_addrs() {
            println!("  http://{ip}:{}", addr.port());
        }
        if let Some(range) = view_ports {
            let (start, end) = (range.start.to_string(), range.end.to_string());
            println!("{}", tr(lang, "cli.view_ports", &[("start", &start), ("end", &end)]));
            state = state.with_view_ports(range);
        }
        eprintln!("{}", text(lang, "cli.lan_warning"));
        state = state.with_host_policy(server::security::HostPolicy::Lan);
    }
    // Em modo rede ou com escuta fora do loopback (spec 112), o navegador deste computador entra
    // pelo link de uso único.
    let entry_link = if lan || exposed {
        match state.devices().issue_entry_link(lang) {
            Ok(token) => {
                let url = entry_link_url(addr, &token);
                println!("{}", tr(lang, "cli.entry_link", &[("url", &url)]));
                Some(url)
            }
            Err(_) => {
                eprintln!("{}", err(&text(lang, "cli.entry_link_failed")));
                None
            }
        }
    } else {
        None
    };
    if open_browser {
        let url = match entry_link {
            Some(link) => link,
            None => {
                let url = browser_url(addr);
                println!("{}", tr(lang, "cli.open_browser", &[("url", &url)]));
                url
            }
        };
        // Em paralelo e com tempo limite curto: o servidor nunca espera pelo navegador.
        tokio::spawn(async move {
            if server::browser_open::open_default(&url).await.is_none() {
                eprintln!("{}", tr(lang, "cli.open_failed", &[("url", &url)]));
            }
        });
    }
    // Órfãos Docker de execuções anteriores (label nants.project): derrubados sem apagar volumes.
    // Em segundo plano: um daemon lento não atrasa o início do servidor.
    tokio::spawn({
        let state = state.clone();
        async move { server::envs::startup_sweep(&state).await }
    });
    // Nanty (spec 138 T01, C-05): o laço sobe aqui, nunca no `AppState::new`, e só se este processo
    // pegar a trava da pasta de dados; ele para sozinho no `shutdown_signal()`.
    let nanty = server::nanty::start(&state);
    let served = server::serve(listener, state).await;
    nanty.stop().await;
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", tr(lang, "cli.server_error", &[("error", &e.to_string())]));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn parse_serve_args_pt(list: &[String]) -> Result<ServeArgs, String> {
        parse_serve_args(list, Lang::PtBr)
    }

    fn parse_doctor_args_pt(list: &[String]) -> Result<Option<PathBuf>, String> {
        parse_doctor_args(list, Lang::PtBr)
    }

    #[test]
    fn usage_and_argument_errors_follow_the_terminal_language() {
        let pt = usage(Lang::PtBr);
        let en = usage(Lang::En);
        assert!(pt.starts_with("uso: nants") && pt.contains("confere o ambiente"), "{pt}");
        assert!(en.starts_with("usage: nants") && en.contains("checks the environment"), "{en}");
        for u in [&pt, &en] {
            assert!(u.contains("--no-open") && u.contains("nants serve") && u.contains("nants doctor"), "{u}");
            assert!(u.contains("nants uninstall --purge"), "{u}");
            assert!(u.contains("[--lan]") && u.contains("\n--lan ") && u.contains("3001"), "{u}");
            assert!(u.contains("\n--allow-remote "), "{u}");
        }
        assert!(pt.contains("acesso pela rede local") && en.contains("access from the local network"));
        assert!(pt.contains("só nesta execução") && pt.contains("Configurações > Acesso pela rede"), "{pt}");
        assert!(en.contains("for this run only") && en.contains("Settings > Network access"), "{en}");
        assert_eq!(parse_serve_args(&args(&["--wat"]), Lang::En), Err("unknown argument: --wat".into()));
        assert_eq!(parse_serve_args(&args(&["--wat"]), Lang::PtBr), Err("argumento desconhecido: --wat".into()));
        assert_eq!(parse_serve_args(&args(&["--port"]), Lang::En), Err("--port needs a value".into()));
        assert_eq!(parse_serve_args(&args(&["--port", "x"]), Lang::En), Err("invalid port: x".into()));
        assert!(parse_serve_args(&args(&["--lan", "--host", "h"]), Lang::En).expect_err("lan").contains("cannot be combined"));
    }

    #[test]
    fn parses_all_serve_flags() {
        let parsed = parse_serve_args_pt(&args(&[
            "--port", "8080", "--host", "::1", "--config", "/tmp/c.toml", "--allow-remote",
        ]))
        .expect("válido");
        assert_eq!(
            parsed,
            ServeArgs {
                host: Some("::1".into()),
                port: Some(8080),
                config: Some(PathBuf::from("/tmp/c.toml")),
                allow_remote: true,
                lan: false,
                no_open: false,
            }
        );
    }

    #[test]
    fn parses_lan_and_rejects_lan_with_host() {
        let parsed = parse_serve_args_pt(&args(&["--lan", "--port", "4000"])).expect("válido");
        assert!(parsed.lan && parsed.host.is_none() && parsed.port == Some(4000));
        assert!(parse_serve_args_pt(&args(&["--lan", "--host", "0.0.0.0"])).is_err());
        assert!(parse_serve_args_pt(&args(&["--host", "127.0.0.1", "--lan"])).is_err());
    }

    #[test]
    fn view_ports_are_resolved_and_validated_at_startup() {
        use config::{LanViewPortsConfig, ViewPorts};
        assert_eq!(view_ports_for(true, None, 3000), Ok(Some(ViewPorts { start: 3001, end: 3010 })));
        assert_eq!(view_ports_for(true, None, 4000), Ok(Some(ViewPorts { start: 4001, end: 4010 })));
        assert_eq!(view_ports_for(false, None, 3000), Ok(None));
        let custom = Some(LanViewPortsConfig { start: 5000, count: 3 });
        assert_eq!(view_ports_for(true, custom, 3000), Ok(Some(ViewPorts { start: 5000, end: 5002 })));
        let bad = Some(LanViewPortsConfig { start: 2999, count: 5 });
        assert!(view_ports_for(true, bad, 3000).expect_err("inclui a porta").contains("lan_view_ports"));
        assert!(view_ports_for(false, bad, 3000).is_err(), "configuração inválida falha mesmo sem --lan");
        assert!(view_ports_for(true, None, 65530).is_err());
    }

    #[test]
    fn entry_link_goes_through_loopback_with_the_single_use_token() {
        let any: std::net::SocketAddr = "0.0.0.0:3000".parse().expect("addr");
        assert_eq!(entry_link_url(any, "abc123"), "http://127.0.0.1:3000/pair?t=abc123");
        let host: std::net::SocketAddr = "192.168.1.5:3002".parse().expect("addr");
        assert_eq!(entry_link_url(host, "x"), "http://192.168.1.5:3002/pair?t=x");
        for exposed in ["0.0.0.0:3000", "192.168.1.5:3000", "[::]:3000", "[fe80::1]:3000"] {
            assert!(listen_requires_device(exposed.parse().expect("addr")), "{exposed}");
        }
        for local in ["127.0.0.1:3000", "127.0.0.2:3000", "[::1]:3000"] {
            assert!(!listen_requires_device(local.parse().expect("addr")), "{local}");
        }
        for lang in [Lang::PtBr, Lang::En] {
            let line = tr(lang, "cli.entry_link", &[("url", "http://127.0.0.1:3000/pair?t=x")]);
            assert!(line.ends_with("http://127.0.0.1:3000/pair?t=x") && line.contains("10"), "{line}");
            let u = usage(lang);
            assert!(u.contains("10 minut"), "{u}");
        }
    }

    #[test]
    fn parses_no_open() {
        assert!(parse_serve_args_pt(&args(&["--no-open"])).expect("válido").no_open);
    }

    #[test]
    fn browser_url_uses_loopback_for_unspecified_address() {
        let lan: std::net::SocketAddr = "0.0.0.0:3000".parse().expect("addr");
        assert_eq!(browser_url(lan), "http://127.0.0.1:3000");
        let local: std::net::SocketAddr = "127.0.0.1:3002".parse().expect("addr");
        assert_eq!(browser_url(local), "http://127.0.0.1:3002");
    }

    #[tokio::test]
    async fn bind_listener_falls_back_only_when_allowed() {
        let busy = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = busy.local_addr().expect("addr").port();
        let err = bind_listener("127.0.0.1", port, false, &[], Lang::PtBr).await.expect_err("ocupada");
        assert!(err.contains("já está em uso") && err.contains("--port"), "{err}");
        let err = bind_listener("127.0.0.1", port, false, &[], Lang::En).await.expect_err("ocupada");
        assert!(err.contains("is already in use") && err.contains("--port"), "{err}");
        if port < u16::MAX - PORT_FALLBACK_TRIES {
            let other = bind_listener("127.0.0.1", port, true, &[], Lang::PtBr).await.expect("fallback");
            let chosen = other.local_addr().expect("addr").port();
            assert!(chosen > port && chosen <= port + PORT_FALLBACK_TRIES, "{chosen}");
        }
    }

    #[tokio::test]
    async fn redirected_ports_count_as_busy_and_explicit_one_fails_with_the_reason() {
        // Porta livre (aberta e fechada) tratada como redirecionada pelo Windows.
        let probe = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);
        if port >= u16::MAX - PORT_FALLBACK_TRIES {
            return;
        }
        // A seguinte também está redirecionada: o Nants pula as duas.
        let redirected = [port, port + 1];
        let chosen = bind_listener("127.0.0.1", port, true, &redirected, Lang::PtBr).await.expect("pula");
        let chosen = chosen.local_addr().expect("addr").port();
        assert!(chosen > port + 1 && chosen <= port + PORT_FALLBACK_TRIES, "{chosen}");
        // `--port` explícito numa porta redirecionada falha com a explicação, nos dois idiomas.
        let err = bind_listener("127.0.0.1", port, false, &redirected, Lang::PtBr).await.expect_err("explícita");
        assert!(err.contains("redirecionada pelo Windows") && err.contains("Configurações > Acesso pela rede"), "{err}");
        let err = bind_listener("127.0.0.1", port, false, &redirected, Lang::En).await.expect_err("explícita");
        assert!(err.contains("redirected by Windows") && err.contains("Settings > Network access"), "{err}");
        // Sem conferência (lista vazia), nada muda: usa a pedida.
        let same = bind_listener("127.0.0.1", port, false, &[], Lang::PtBr).await.expect("livre");
        assert_eq!(same.local_addr().expect("addr").port(), port);
    }

    #[test]
    fn redirect_messages_explain_and_point_to_the_settings() {
        for (lang, words) in [
            (Lang::PtBr, ["a porta 3000 está redirecionada pelo Windows", "o Nants está na porta 3001", "Configurações > Acesso pela rede"]),
            (Lang::En, ["port 3000 is redirected by Windows", "Nants is on port 3001", "Settings > Network access"]),
        ] {
            let line = tr(lang, "cli.port_redirected", &[("port", "3000"), ("next", "3001")]);
            for w in words {
                assert!(line.contains(w), "{line}");
            }
        }
        assert_eq!(
            text(Lang::PtBr, "cli.lan_adopted"),
            "Acesso pela rede local ligado (o Windows já está configurado). Para desligar: Configurações > Acesso pela rede."
        );
        assert!(text(Lang::En, "cli.lan_adopted").starts_with("Local network access turned on (Windows is already set up)."));
    }

    #[test]
    fn parses_doctor_args() {
        assert_eq!(parse_doctor_args_pt(&[]), Ok(None));
        assert_eq!(parse_doctor_args_pt(&args(&["--config", "/tmp/c.toml"])), Ok(Some(PathBuf::from("/tmp/c.toml"))));
        assert!(parse_doctor_args_pt(&args(&["--pull"])).is_err());
        assert!(parse_doctor_args_pt(&args(&["--config"])).is_err());
    }

    #[test]
    fn parses_uninstall_args() {
        assert_eq!(parse_uninstall_args(&[], Lang::PtBr), Ok(false));
        assert_eq!(parse_uninstall_args(&args(&["--purge"]), Lang::PtBr), Ok(true));
        assert_eq!(parse_uninstall_args(&args(&["--yes"]), Lang::En), Err("unknown argument: --yes".into()));
    }

    #[test]
    fn empty_args_use_defaults() {
        assert_eq!(parse_serve_args_pt(&[]).expect("vazio"), ServeArgs::default());
    }

    #[test]
    fn rejects_bad_port_missing_value_and_unknown_flags() {
        assert!(parse_serve_args_pt(&args(&["--port", "abc"])).is_err());
        assert!(parse_serve_args_pt(&args(&["--port", "70000"])).is_err());
        assert!(parse_serve_args_pt(&args(&["--host"])).is_err());
        assert!(parse_serve_args_pt(&args(&["--wat"])).is_err());
    }

    #[test]
    fn config_error_description_never_echoes_secret_line() {
        let src = "api_key = \"sk-SEGREDO\" lixo";
        let err: ConfigError = toml::from_str::<Config>(src)
            .expect_err("toml inválido")
            .into();
        let msg = describe_config_error(&err, Lang::PtBr);
        assert!(!msg.contains("sk-SEGREDO"), "vazou segredo: {msg}");
        assert!(msg.contains("config inválido"), "{msg}");
        let msg = describe_config_error(&err, Lang::En);
        assert!(!msg.contains("sk-SEGREDO") && msg.contains("invalid config"), "{msg}");
    }

    #[test]
    fn loopback_detection() {
        for host in ["127.0.0.1", "127.8.9.1", "localhost", "::1", "[::1]", "LOCALHOST"] {
            assert!(is_loopback_host(host), "{host}");
        }
        for host in ["0.0.0.0", "192.168.1.10", "::", "example.com", "[::]"] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }
}
