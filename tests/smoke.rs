//! Testes de fumaça do servidor e do binário.
//!
//! - In-process: `serve_with_shutdown` em 127.0.0.1:0 com `AppState` e config temporários.
//! - Binário real: `CARGO_BIN_EXE_nants serve --port 0 --config <tmp>` com a porta lida do stdout,
//!   encerrado por SIGINT; o guard de bind (`--host 0.0.0.0` sem `--allow-remote`) sai com código 2.
//!
//! Cliente HTTP cru (TcpStream + `Connection: close`) para controlar o cabeçalho `Host`.
//! O nome do bundle JS é extraído do próprio `index.html` servido, não do sistema de arquivos.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nants::config::{AgentConfig, Agents, Config, Connection};
use nants::server::{serve_with_shutdown, AppState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

/// Chave literal usada nas configs de teste: nunca pode aparecer em respostas.
const SECRET_KEY: &str = "sk-SMOKE-LITERAL-KEY-98765";

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Envia uma requisição HTTP/1.1 crua com o `Host` informado e lê a resposta até o fim.
async fn raw_request(addr: &str, method: &str, path: &str, host: &str) -> Response {
    let mut stream = TcpStream::connect(addr).await.expect("conexão ao servidor");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.expect("envio");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("resposta dentro do prazo")
        .expect("leitura");
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Response {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("separador de cabeçalhos");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = raw[split + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("linha de status");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("código de status");
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Response { status, headers, body }
}

/// Extrai o caminho do script de módulo do HTML servido.
fn script_src(html: &str) -> String {
    let marker = "<script type=\"module\" crossorigin src=\"";
    let start = html
        .find(marker)
        .map(|i| i + marker.len())
        .or_else(|| html.find("src=\"").map(|i| i + "src=\"".len()))
        .expect("script src no index.html");
    let rest = &html[start..];
    let end = rest.find('"').expect("fim do src");
    rest[..end].to_string()
}

/// Diretório temporário único (sem dependência extra).
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("nants-smoke-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("diretório temporário");
    dir
}

fn smoke_config() -> Config {
    let agent = AgentConfig {
        connection_id: "openai-direct".to_string(),
        model: "gpt-4o".to_string(),
        ..AgentConfig::default()
    };
    Config {
        connections: vec![Connection {
            id: "openai-direct".to_string(),
            name: "OpenAI direto".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: SECRET_KEY.to_string(),
            wire_api: nants::config::WireApi::default(),
        }],
        agents: Agents {
            architect: agent.clone(),
            developer: agent.clone(),
            reviewer: agent,
        },
        ..Config::default()
    }
}

/// Asserções comuns: SPA, bundle imutável, fallback de rota, API protegida e 404 JSON.
/// Devolve o caminho do bundle JS descoberto no index.
async fn assert_spa_and_api(addr: &str, host: &str) -> String {
    // GET / : HTML com CSP restritiva e o contêiner da SPA.
    let index = raw_request(addr, "GET", "/", host).await;
    assert_eq!(index.status, 200);
    assert!(index.header("content-type").unwrap_or("").starts_with("text/html"));
    let csp = index.header("content-security-policy").expect("CSP");
    assert!(csp.contains("default-src 'self'"), "{csp}");
    assert!(!csp.contains("unsafe-inline"), "{csp}");
    let html = index.text();
    assert!(html.contains("<div id=\"root\">"), "{html}");
    assert!(!html.contains("<script>"), "script inline proibido: {html}");

    // O bundle é descoberto no index.html servido e servido com cache imutável.
    let js_path = script_src(&html);
    assert!(js_path.starts_with("/assets/") && js_path.ends_with(".js"), "{js_path}");
    let js = raw_request(addr, "GET", &js_path, host).await;
    assert_eq!(js.status, 200, "{js_path}");
    assert!(js.header("content-type").unwrap_or("").starts_with("text/javascript"));
    assert_eq!(js.header("cache-control"), Some("public, max-age=31536000, immutable"));

    // Rota de cliente cai no index (200).
    let route = raw_request(addr, "GET", "/projects/x", host).await;
    assert_eq!(route.status, 200);
    assert_eq!(route.text(), html);

    // API com Host correto: JSON sem a chave literal.
    let config = raw_request(addr, "GET", "/api/config", host).await;
    assert_eq!(config.status, 200);
    assert!(config.header("content-type").unwrap_or("").starts_with("application/json"));
    let config_body = config.text();
    assert!(config_body.contains("openai-direct"), "{config_body}");
    assert!(!config_body.contains(SECRET_KEY), "chave vazou: {config_body}");

    // Host estranho: 403 (DNS rebinding).
    let evil = raw_request(addr, "GET", "/api/config", "evil.example").await;
    assert_eq!(evil.status, 403);

    // Rota de API inexistente: 404 JSON, nunca o index.
    let nope = raw_request(addr, "GET", "/api/nope", host).await;
    assert_eq!(nope.status, 404);
    assert!(nope.header("content-type").unwrap_or("").starts_with("application/json"));
    assert!(!nope.text().contains("<div"));

    // Barra codificada no caminho cru: 404 (T04/B).
    let encoded_path = format!("/assets%2f{}", js_path.trim_start_matches("/assets/"));
    let encoded = raw_request(addr, "GET", &encoded_path, host).await;
    assert_eq!(encoded.status, 404);

    js_path
}

#[tokio::test]
async fn servidor_in_process_serve_spa_api_e_desliga_limpo() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("endereço").port();
    let addr = format!("127.0.0.1:{port}");
    let host = format!("127.0.0.1:{port}");

    let state = AppState::new(temp_dir("inproc").join("config.toml"), smoke_config(), port);
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(serve_with_shutdown(listener, state, async move {
        let _ = rx.await;
    }));

    assert_spa_and_api(&addr, &host).await;

    // Desligamento limpo: o serve termina logo após o sinal.
    tx.send(()).expect("sinal de shutdown");
    let result = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("serve terminou em menos de 2 s")
        .expect("task do servidor");
    assert!(result.is_ok(), "{result:?}");
}

/// Processo filho do binário real; mata em Drop para não vazar em caso de falha.
struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Config TOML com a chave literal (lida pelo binário; nunca deve sair pela API).
fn write_config(dir: &Path) -> PathBuf {
    let path = dir.join("config.toml");
    let toml = format!(
        "[server]\nhost = \"127.0.0.1\"\nport = 3000\n\n\
         [[connections]]\nid = \"openai-direct\"\nname = \"OpenAI direto\"\n\
         provider_type = \"openai\"\nbase_url = \"https://api.openai.com/v1\"\n\
         api_key = \"{SECRET_KEY}\"\n"
    );
    std::fs::write(&path, toml).expect("escrita da config");
    path
}

/// Lê linhas do stdout do filho até a linha `nants listening on <url>` (em até `limit`).
/// Uma thread drena o stdout inteiro, para o filho nunca bloquear em escrita.
fn read_listening_url(stdout: std::process::ChildStdout, limit: Duration) -> String {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = rx
            .recv_timeout(remaining)
            .expect("linha 'nants listening on' dentro do prazo");
        if let Some(url) = line.trim().strip_prefix("nants listening on ") {
            return url.to_string();
        }
    }
}

#[tokio::test]
async fn binario_real_serve_com_porta_efemera_e_encerra_com_sigint() {
    let dir = temp_dir("bin");
    let config = write_config(&dir);

    let mut child = Command::new(env!("CARGO_BIN_EXE_nants"))
        .args(["serve", "--port", "0", "--config"])
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn do binário");
    let pid = child.id();
    let stdout = child.stdout.take().expect("stdout");
    let mut guard = ChildGuard(Some(child));

    let url = read_listening_url(stdout, Duration::from_secs(20));
    let addr = url.strip_prefix("http://").expect("url http").to_string();
    let port: u16 = addr.rsplit(':').next().and_then(|p| p.parse().ok()).expect("porta");
    assert_ne!(port, 0, "a porta efetiva deve ser a ligada, não 0");
    assert_eq!(addr, format!("127.0.0.1:{port}"));

    assert_spa_and_api(&addr, &addr).await;

    // SIGINT: encerramento gracioso em menos de 5 s.
    let status = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .expect("kill -INT");
    assert!(status.success());
    let started = Instant::now();
    let child_status = loop {
        let child = guard.0.as_mut().expect("filho presente");
        if let Some(st) = child.try_wait().expect("try_wait") {
            break st;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "não encerrou em 5 s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(child_status.success(), "{child_status:?}");
    guard.0 = None;
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn binario_recusa_bind_nao_loopback_sem_allow_remote() {
    let output = Command::new(env!("CARGO_BIN_EXE_nants"))
        .args(["serve", "--host", "0.0.0.0", "--port", "0"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn do binário");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--allow-remote"), "{stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("listening"));
}
