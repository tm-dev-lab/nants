//! `nants` sem subcomando (spec 103, T01): sobe em 127.0.0.1 e abre o navegador por `$BROWSER`
//! (aqui um script falso que grava a URL num arquivo), `--no-open`, porta ocupada com troca
//! automática e `--port` ocupada falhando com mensagem clara.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("nants-launcher-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("diretório temporário");
    dir
}

fn write_config(dir: &Path, port: u16) -> PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(&path, format!("[server]\nhost = \"127.0.0.1\"\nport = {port}\n")).expect("config");
    path
}

/// Script que faz o papel do navegador: grava o primeiro argumento (a URL) em `out`.
fn fake_browser(dir: &Path, out: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-browser.sh");
    std::fs::write(&script, format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", out.display()))
        .expect("script");
    let mut perms = std::fs::metadata(&script).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod");
    script
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Inicia o binário com `args` e devolve o filho e as linhas do stdout (drenadas numa thread).
fn spawn(args: &[&str], browser: Option<&Path>) -> (ChildGuard, mpsc::Receiver<String>) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nants"));
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    // Mensagens em pt-BR (spec 102: o terminal segue LC_ALL > LC_MESSAGES > LANG).
    cmd.env("LC_ALL", "pt_BR.UTF-8");
    match browser {
        Some(b) => cmd.env("BROWSER", b),
        None => cmd.env("BROWSER", "/nao/existe/navegador"),
    };
    let mut child = cmd.spawn().expect("spawn do binário");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    (ChildGuard(child), rx)
}

/// Lê linhas até achar uma com `prefix`; devolve o restante da linha e as linhas lidas antes.
fn wait_line(rx: &mpsc::Receiver<String>, prefix: &str) -> (String, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = rx.recv_timeout(remaining).unwrap_or_else(|_| panic!("linha '{prefix}' no prazo; vistas: {seen:?}"));
        if let Some(rest) = line.trim().strip_prefix(prefix) {
            return (rest.to_string(), seen);
        }
        seen.push(line);
    }
}

fn wait_file(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.is_empty() {
                return text;
            }
        }
        assert!(Instant::now() < deadline, "o navegador falso não foi chamado");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn nants_sem_subcomando_abre_o_navegador_pelo_browser() {
    let dir = temp_dir("open");
    let out = dir.join("url.txt");
    let browser = fake_browser(&dir, &out);
    let config = write_config(&dir, 3000);
    let (_guard, rx) = spawn(
        &["--port", "0", "--config", config.to_str().expect("utf8")],
        Some(&browser),
    );
    let (url, _) = wait_line(&rx, "nants listening on ");
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let (printed, _) = wait_line(&rx, "abra no navegador: ");
    assert_eq!(printed, url, "a URL é sempre impressa");
    assert_eq!(wait_file(&out), url);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_open_e_serve_nao_abrem_o_navegador() {
    for (tag, extra) in [("noopen", vec!["--no-open"]), ("serve", vec!["serve"])] {
        let dir = temp_dir(tag);
        let out = dir.join("url.txt");
        let browser = fake_browser(&dir, &out);
        let config = write_config(&dir, 3000);
        let mut args = extra.clone();
        args.extend(["--port", "0", "--config", config.to_str().expect("utf8")]);
        let (_guard, rx) = spawn(&args, Some(&browser));
        let (url, _) = wait_line(&rx, "nants listening on ");
        assert!(url.starts_with("http://127.0.0.1:"), "{url}");
        std::thread::sleep(Duration::from_millis(800));
        assert!(!out.exists(), "{tag}: o navegador não deveria ser aberto");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn porta_ocupada_sem_port_usa_a_proxima_e_informa() {
    let busy = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = busy.local_addr().expect("addr").port();
    if port > u16::MAX - 21 {
        return;
    }
    let dir = temp_dir("fallback");
    let config = write_config(&dir, port);
    let (_guard, rx) = spawn(&["serve", "--config", config.to_str().expect("utf8")], None);
    let (url, before) = wait_line(&rx, "nants listening on ");
    let chosen: u16 = url.rsplit(':').next().and_then(|p| p.parse().ok()).expect("porta");
    assert!(chosen > port && chosen <= port + 20, "porta {chosen} fora da faixa após {port}");
    let notice = format!("a porta {port} está ocupada; usando a porta {chosen}");
    assert!(before.iter().any(|l| l.contains(&notice)), "{before:?}");
    drop(busy);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn port_explicita_ocupada_falha_com_mensagem_clara() {
    let busy = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = busy.local_addr().expect("addr").port();
    let dir = temp_dir("explicit");
    let config = write_config(&dir, 3000);
    let output = Command::new(env!("CARGO_BIN_EXE_nants"))
        .args(["serve", "--port", &port.to_string(), "--config", config.to_str().expect("utf8")])
        .env("LC_ALL", "pt_BR.UTF-8")
        .stdin(Stdio::null())
        .output()
        .expect("spawn do binário");
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&format!("a porta {port} já está em uso")), "{stderr}");
    assert!(stderr.contains("--port"), "{stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("listening"));
    drop(busy);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ajuda_menciona_no_open() {
    let output = Command::new(env!("CARGO_BIN_EXE_nants")).arg("--help").output().expect("spawn");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--no-open") && stdout.contains("nants serve"), "{stdout}");
}

/// Spec 102: ajuda e erros no idioma do terminal; a linha `nants listening on` não muda.
#[test]
fn ajuda_e_erros_seguem_o_locale_do_terminal() {
    let help = |locale: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_nants")).arg("--help").env("LC_ALL", locale).output().expect("spawn");
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    assert!(help("pt_BR.UTF-8").starts_with("uso: nants"));
    assert!(help("C").starts_with("usage: nants"));
    assert!(help("en_US.UTF-8").contains("checks the environment"));
    let out = Command::new(env!("CARGO_BIN_EXE_nants")).arg("bogus").env("LC_ALL", "C").output().expect("spawn");
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("unknown command: bogus"));

    let dir = temp_dir("locale-listen");
    let config = write_config(&dir, 0);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nants"));
    cmd.args(["serve", "--port", "0", "--config", config.to_str().expect("utf8")])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = ChildGuard(cmd.spawn().expect("spawn"));
    let stdout = child.0.stdout.take().expect("stdout");
    let first = BufReader::new(stdout).lines().map_while(Result::ok).next().unwrap_or_default();
    assert!(first.starts_with("nants listening on http://"), "{first}");
    drop(child);
    let _ = std::fs::remove_dir_all(&dir);
}
