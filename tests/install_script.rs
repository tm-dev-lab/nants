//! `scripts/install.sh` (spec 103, T04): roda o instalador de verdade com `/bin/sh`, um servidor
//! HTTP local (porta escolhida pelo sistema) servindo o `latest.json` e os pacotes, um `uname`
//! falso e um PATH mínimo só com as ferramentas permitidas. HOME é uma pasta temporária apagada no
//! fim (guarda com `Drop`); nada fora dela é tocado.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

/// Pasta temporária do teste, apagada no fim mesmo em falha (guarda com `Drop`, CLAUDE.md).
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(tag: &str) -> TempDir {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("nants-install-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("diretório temporário");
    TempDir(dir)
}

fn write(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("pasta");
    }
    std::fs::write(path, body).expect("arquivo");
}

fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, body.as_bytes());
    let mut perms = std::fs::metadata(path).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
}

fn script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh")
}

/// Servidor HTTP mínimo: serve os arquivos de `root`; registra os caminhos pedidos.
struct Server {
    url: String,
    hits: Arc<Mutex<Vec<String>>>,
}

fn serve(root: PathBuf) -> Server {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(match stream.try_clone() {
                Ok(s) => s,
                Err(_) => continue,
            });
            let mut first = String::new();
            if reader.read_line(&mut first).is_err() {
                continue;
            }
            loop {
                let mut l = String::new();
                match reader.read_line(&mut l) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if l == "\r\n" || l == "\n" => break,
                    Ok(_) => {}
                }
            }
            let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
            if let Ok(mut h) = log.lock() {
                h.push(path.clone());
            }
            let file = root.join(path.trim_start_matches('/'));
            let resp = match std::fs::read(&file) {
                Ok(body) if !path.contains("..") && file.is_file() => {
                    let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
                    r.extend_from_slice(&body);
                    r
                }
                _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            };
            let _ = stream.write_all(&resp);
        }
    });
    Server { url: format!("http://127.0.0.1:{port}"), hits }
}

/// `nants` falso empacotado: `--version` e `doctor` (que, como sem Docker, sai com 1).
fn fake_nants(version: &str, arch: &str) -> String {
    format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo \"nants {version}\" ;;\n  doctor) echo \"doctor {version} {arch}\" >> \"$FAKE_LOG\"; echo \"DOCTOR FALSO {version}\"; exit 1 ;;\nesac\n"
    )
}

fn sha256(path: &Path) -> String {
    let out = Command::new("sha256sum").arg(path).output().expect("sha256sum");
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or("").to_string()
}

/// Gera `vX/nants-X-<arch>-linux.tar.gz` e o `.sha256` em `root`; devolve o SHA.
fn package(root: &Path, version: &str, arch: &str) -> String {
    let stage = root.join(format!("stage-{version}-{arch}"));
    write_exec(&stage.join("nants"), &fake_nants(version, arch));
    let file = format!("nants-{version}-{arch}-linux.tar.gz");
    let dir = root.join(format!("files/v{version}"));
    std::fs::create_dir_all(&dir).expect("pasta");
    let st = Command::new("tar").arg("-czf").arg(dir.join(&file)).arg("-C").arg(&stage).arg("nants").status().expect("tar");
    assert!(st.success());
    let sum = sha256(&dir.join(&file));
    write(&dir.join(format!("{file}.sha256")), format!("{sum}  {file}\n").as_bytes());
    sum
}

struct Fixture {
    base: TempDir,
    server: Server,
    home: PathBuf,
    bin: PathBuf,
    log: PathBuf,
}

/// O manifesto no formato do `release.sh` (multilinha) ou compacto numa linha só.
fn manifest(f_url: &str, version: &str, sums: &[(&str, String)], compact: bool) -> String {
    let assets: serde_json::Map<String, serde_json::Value> = sums
        .iter()
        .map(|(arch, sum)| {
            (
                format!("{arch}-linux"),
                serde_json::json!({
                    "url": format!("{f_url}/files/v{version}/nants-{version}-{arch}-linux.tar.gz"),
                    "sha256": sum,
                }),
            )
        })
        .collect();
    let v = serde_json::json!({ "version": version, "date": "2026-10-21T12:00:00Z", "assets": assets });
    if compact {
        v.to_string()
    } else {
        serde_json::to_string_pretty(&v).expect("json")
    }
}

/// Ferramentas permitidas ao instalador (o resto do sistema fica fora do PATH).
const TOOLS: &[&str] = &[
    "curl", "tar", "gzip", "sha256sum", "mktemp", "sed", "tr", "grep", "head", "cp", "mv", "chmod", "rm", "mkdir",
];

fn which(tool: &str) -> Option<PathBuf> {
    ["/usr/bin", "/bin", "/usr/local/bin"].iter().map(|d| Path::new(d).join(tool)).find(|p| p.exists())
}

/// Servidor com a 0.4.2 (x86_64 e aarch64) como a última e a 0.3.0 disponível em `v0.3.0/`.
fn fixture(tag: &str, compact: bool, omit: &[&str]) -> Fixture {
    let base = temp_dir(tag);
    let root = base.0.join("www");
    let sum_x = package(&root, "0.4.2", "x86_64");
    let sum_a = package(&root, "0.4.2", "aarch64");
    package(&root, "0.3.0", "x86_64");
    let server = serve(root.clone());
    let json = manifest(&server.url, "0.4.2", &[("x86_64", sum_x), ("aarch64", sum_a)], compact);
    write(&root.join("latest.json"), json.as_bytes());

    let bin = base.0.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    for tool in TOOLS.iter().filter(|t| !omit.contains(t)) {
        let real = which(tool).unwrap_or_else(|| panic!("ferramenta ausente no sistema: {tool}"));
        std::os::unix::fs::symlink(real, bin.join(tool)).expect("symlink");
    }
    write_exec(
        &bin.join("uname"),
        "#!/bin/sh\ncase \"$1\" in\n  -s) echo \"$FAKE_OS\" ;;\n  -m) echo \"$FAKE_ARCH\" ;;\n  -r) echo \"$FAKE_KERNEL\" ;;\nesac\n",
    );
    let home = base.0.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let log = base.0.join("calls.log");
    Fixture { base, server, home, bin, log }
}

#[derive(Clone, Copy)]
struct Env<'a> {
    os: &'a str,
    arch: &'a str,
    kernel: &'a str,
    lang: &'a str,
    in_path: bool,
}

const LINUX: Env<'static> =
    Env { os: "Linux", arch: "x86_64", kernel: "6.8.0-45-generic", lang: "pt_BR.UTF-8", in_path: false };

fn run(f: &Fixture, e: Env<'_>, extra: &[(&str, &str)]) -> Output {
    let mut path = f.bin.display().to_string();
    if e.in_path {
        path = format!("{}:{path}", f.home.join(".local/bin").display());
    }
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(script_path())
        .env_clear()
        .env("PATH", path)
        .env("HOME", &f.home)
        .env("SHELL", "/bin/bash")
        .env("LANG", e.lang)
        .env("NANTS_RELEASES_URL", &f.server.url)
        .env("FAKE_OS", e.os)
        .env("FAKE_ARCH", e.arch)
        .env("FAKE_KERNEL", e.kernel)
        .env("FAKE_LOG", &f.log)
        .env("TMPDIR", f.base.0.join("tmp"))
        .current_dir(&f.base.0);
    std::fs::create_dir_all(f.base.0.join("tmp")).expect("tmp");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().expect("install.sh")
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn installed(f: &Fixture) -> Option<String> {
    std::fs::read_to_string(f.home.join(".local/bin/nants")).ok()
}

/// Tudo o que existe em HOME depois da instalação (caminhos relativos).
fn home_files(f: &Fixture) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push(p.strip_prefix(base).unwrap_or(&p).display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(&f.home, &f.home, &mut out);
    out.sort();
    out
}

fn tmp_left(f: &Fixture) -> usize {
    std::fs::read_dir(f.base.0.join("tmp")).map(|d| d.count()).unwrap_or(0)
}

#[test]
fn installs_latest_for_x86_64_verifies_sha_warns_path_and_runs_doctor() {
    let f = fixture("ok", false, &[]);
    let out = run(&f, LINUX, &[]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(installed(&f).is_some_and(|b| b.contains("nants 0.4.2") && b.contains("x86_64")), "{all}");
    assert_eq!(home_files(&f), [".local/bin/nants"], "nada fora de ~/.local/bin");
    assert!(all.contains("Sistema: Linux, x86_64"), "{all}");
    assert!(!all.contains("WSL"), "{all}");
    assert!(all.contains("SHA-256 conferido"), "{all}");
    assert!(all.contains("Nants 0.4.2 instalado em"), "{all}");
    // PATH: aviso com a linha para incluir e o arquivo do shell.
    assert!(all.contains("não está no PATH"), "{all}");
    assert!(all.contains("export PATH=\"$HOME/.local/bin:$PATH\""), "{all}");
    assert!(all.contains("~/.bashrc"), "{all}");
    // doctor ao final (o falso sai com 1, como sem Docker) e o convite para digitar nants.
    assert!(all.contains("DOCTOR FALSO 0.4.2"), "{all}");
    assert!(std::fs::read_to_string(&f.log).unwrap_or_default().contains("doctor 0.4.2 x86_64"));
    assert!(all.contains("Digite nants"), "{all}");
    assert_eq!(tmp_left(&f), 0, "a pasta temporária do download é apagada");
    let hits = f.server.hits.lock().map(|h| h.clone()).unwrap_or_default();
    assert_eq!(hits, ["/latest.json", "/files/v0.4.2/nants-0.4.2-x86_64-linux.tar.gz"]);
}

#[test]
fn aarch64_compact_manifest_english_and_path_already_set() {
    let f = fixture("arm", true, &[]);
    let e = Env { arch: "aarch64", lang: "en_US.UTF-8", in_path: true, ..LINUX };
    let out = run(&f, e, &[]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(installed(&f).is_some_and(|b| b.contains("aarch64")), "{all}");
    assert!(all.contains("System: Linux, aarch64") && all.contains("Nants 0.4.2 installed at"), "{all}");
    assert!(!all.contains("PATH"), "sem aviso de PATH quando já está: {all}");
    assert!(all.contains("Type nants"), "{all}");
    // arm64 (nome do macOS e de algumas distros) também vira aarch64.
    let f = fixture("arm64", false, &[]);
    let out = run(&f, Env { arch: "arm64", ..LINUX }, &[]);
    assert!(out.status.success() && installed(&f).is_some_and(|b| b.contains("aarch64")), "{}", text(&out));
}

#[test]
fn messages_follow_lc_all_then_lc_messages_then_lang() {
    let f = fixture("lang", false, &[]);
    let out = run(&f, LINUX, &[("LC_ALL", "C")]);
    assert!(text(&out).contains("System: Linux"), "LC_ALL=C vence o LANG pt: {}", text(&out));
    let f = fixture("lang2", false, &[]);
    let out = run(&f, Env { lang: "en_US.UTF-8", ..LINUX }, &[("LC_MESSAGES", "pt_PT.UTF-8")]);
    assert!(text(&out).contains("Sistema: Linux"), "{}", text(&out));
}

#[test]
fn wsl_is_detected_only_as_information() {
    let f = fixture("wsl", false, &[]);
    let out = run(&f, Env { kernel: "6.6.36.6-microsoft-standard-WSL2", ..LINUX }, &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("WSL detectado"), "{}", text(&out));
    let f = fixture("wsl-env", false, &[]);
    let out = run(&f, LINUX, &[("WSL_DISTRO_NAME", "Ubuntu")]);
    assert!(out.status.success() && text(&out).contains("WSL detectado"), "{}", text(&out));
}

#[test]
fn refuses_macos_windows_and_unknown_arch_without_touching_home() {
    let cases: [(Env<'static>, &str); 4] = [
        (Env { os: "Darwin", arch: "arm64", ..LINUX }, "macOS"),
        (Env { os: "MINGW64_NT-10.0-22631", ..LINUX }, "wsl --install"),
        (Env { os: "CYGWIN_NT-10.0", lang: "en_US", ..LINUX }, "wsl --install"),
        (Env { arch: "riscv64", ..LINUX }, "riscv64"),
    ];
    for (e, expected) in cases {
        let f = fixture("refuse", false, &[]);
        let out = run(&f, e, &[]);
        assert!(!out.status.success(), "{}", text(&out));
        assert!(text(&out).contains(expected), "{}", text(&out));
        assert!(home_files(&f).is_empty() && !f.home.join(".local").exists());
        assert!(f.server.hits.lock().map(|h| h.is_empty()).unwrap_or(false), "recusa antes de baixar");
    }
}

#[test]
fn wrong_sha_installs_nothing_and_keeps_the_previous_version() {
    let f = fixture("sha", false, &[]);
    assert!(run(&f, LINUX, &[]).status.success());
    let before = installed(&f);
    // O pacote no servidor muda e o manifesto continua com o SHA antigo.
    let pkg = f.base.0.join("www/files/v0.4.2/nants-0.4.2-x86_64-linux.tar.gz");
    write(&pkg, b"pacote adulterado");
    let out = run(&f, LINUX, &[]);
    assert!(!out.status.success());
    assert!(text(&out).contains("SHA-256 do pacote não confere"), "{}", text(&out));
    assert_eq!(installed(&f), before, "o executável anterior fica intacto");
    assert_eq!(home_files(&f), [".local/bin/nants"]);
    assert_eq!(tmp_left(&f), 0);
}

#[test]
fn missing_sha256sum_or_curl_is_refused() {
    for tool in ["sha256sum", "curl"] {
        let f = fixture("tools", false, &[tool]);
        let out = run(&f, LINUX, &[]);
        assert!(!out.status.success());
        assert!(text(&out).contains(&format!("Falta o comando {tool}")), "{}", text(&out));
        assert!(installed(&f).is_none());
    }
}

#[test]
fn nants_version_installs_that_version_from_the_same_base_and_reinstall_updates() {
    let f = fixture("version", false, &[]);
    let out = run(&f, LINUX, &[("NANTS_VERSION", "0.3.0")]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(installed(&f).is_some_and(|b| b.contains("nants 0.3.0")), "{all}");
    let hits = f.server.hits.lock().map(|h| h.clone()).unwrap_or_default();
    assert_eq!(
        hits,
        [
            "/latest.json",
            "/files/v0.3.0/nants-0.3.0-x86_64-linux.tar.gz.sha256",
            "/files/v0.3.0/nants-0.3.0-x86_64-linux.tar.gz",
        ]
    );
    // Rodar de novo atualiza para a última.
    let out = run(&f, LINUX, &[]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(installed(&f).is_some_and(|b| b.contains("nants 0.4.2")), "{all}");
    assert!(all.contains("antes: 0.3.0, agora: 0.4.2"), "{all}");
    assert_eq!(home_files(&f), [".local/bin/nants"]);
}

#[test]
fn bad_or_missing_nants_version_is_refused() {
    let f = fixture("badver", false, &[]);
    let out = run(&f, LINUX, &[("NANTS_VERSION", "1.2")]);
    assert!(!out.status.success() && text(&out).contains("NANTS_VERSION inválida"), "{}", text(&out));
    // Valor de várias linhas com uma versão válida numa delas, e caracteres fora de [0-9.].
    for bad in ["0.3.0\n1.2.3", "1.2.3\n", "0.3.0;x", "0.3.0 "] {
        let out = run(&f, LINUX, &[("NANTS_VERSION", bad)]);
        assert!(!out.status.success() && text(&out).contains("NANTS_VERSION inválida"), "{bad:?}: {}", text(&out));
    }
    assert!(f.server.hits.lock().map(|h| h.is_empty()).unwrap_or(false), "recusa antes de baixar");
    let out = run(&f, LINUX, &[("NANTS_VERSION", "9.9.9")]);
    assert!(!out.status.success() && text(&out).contains("Não foi possível baixar"), "{}", text(&out));
    assert!(installed(&f).is_none());
}

#[test]
fn https_base_never_follows_a_redirect_to_http() {
    // curl falso: registra os argumentos e falha como sem rede.
    let f = fixture("proto", false, &["curl"]);
    write_exec(&f.bin.join("curl"), "#!/bin/sh\necho \"curl $*\" >> \"$FAKE_LOG\"\nexit 22\n");
    let out = run(&f, LINUX, &[("NANTS_RELEASES_URL", "https://nants.example.invalid")]);
    assert!(!out.status.success());
    let log = std::fs::read_to_string(&f.log).unwrap_or_default();
    assert!(log.contains("--proto =https --proto-redir =https"), "{log}");
    assert!(log.contains("https://nants.example.invalid/latest.json"), "{log}");
    // Base http (só no teste local): sem a restrição.
    let f = fixture("proto-http", false, &["curl"]);
    write_exec(&f.bin.join("curl"), "#!/bin/sh\necho \"curl $*\" >> \"$FAKE_LOG\"\nexit 22\n");
    let _ = run(&f, LINUX, &[]);
    let log = std::fs::read_to_string(&f.log).unwrap_or_default();
    assert!(log.contains("latest.json") && !log.contains("--proto"), "{log}");
}

#[test]
fn bad_manifest_is_refused() {
    let f = fixture("badmanifest", false, &[]);
    write(&f.base.0.join("www/latest.json"), b"{\"version\": \"0.4.2\"}");
    let out = run(&f, LINUX, &[]);
    assert!(!out.status.success() && text(&out).contains("incompleto ou inválido"), "{}", text(&out));
    write(&f.base.0.join("www/latest.json"), b"");
    std::fs::remove_file(f.base.0.join("www/latest.json")).expect("rm");
    let out = run(&f, LINUX, &[]);
    assert!(!out.status.success() && text(&out).contains("Não foi possível ler"), "{}", text(&out));
    assert!(installed(&f).is_none());
}

#[test]
fn script_shape_main_last_set_eu_no_sudo_no_prompt_and_valid_syntax() {
    let body = std::fs::read_to_string(script_path()).expect("install.sh");
    assert!(body.starts_with("#!/bin/sh\n"));
    assert_eq!(body.trim_end().lines().last(), Some("main \"$@\""));
    assert!(body.lines().any(|l| l == "set -eu"));
    for l in body.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        assert!(!l.starts_with("sudo ") && !l.contains("| sudo") && !l.contains("&& sudo"), "nunca usa sudo: {l}");
        let reads = ["; read ", "| read ", "&& read ", "while read "];
        assert!(!l.starts_with("read ") && !reads.iter().any(|r| l.contains(r)), "nada interativo: {l}");
        assert!(!l.contains("jq "), "sem jq: {l}");
    }
    assert!(!body.contains('\u{2014}') && !body.contains('\u{2013}'), "sem travessão");
    for sh in ["dash", "bash"] {
        if let Some(path) = which(sh) {
            let mut cmd = Command::new(path);
            if sh == "bash" {
                cmd.arg("--posix");
            }
            let out = cmd.arg("-n").arg(script_path()).output().expect("sh -n");
            assert!(out.status.success(), "{sh} -n: {}", text(&out));
        }
    }
}

#[test]
fn a_truncated_download_runs_nothing() {
    let f = fixture("trunc", false, &[]);
    let body = std::fs::read_to_string(script_path()).expect("install.sh");
    let cut = &body[..body.rfind("main \"$@\"").expect("main")];
    for part in [&cut[..cut.len() / 2], cut] {
        let partial = f.base.0.join("partial.sh");
        write(&partial, part.as_bytes());
        let _ = Command::new("/bin/sh")
            .arg(&partial)
            .env_clear()
            .env("PATH", f.bin.display().to_string())
            .env("HOME", &f.home)
            .env("NANTS_RELEASES_URL", &f.server.url)
            .env("FAKE_OS", "Linux")
            .env("FAKE_ARCH", "x86_64")
            .env("FAKE_KERNEL", "x")
            .output()
            .expect("sh");
        assert!(home_files(&f).is_empty(), "script pela metade não instala nada");
        assert!(f.server.hits.lock().map(|h| h.is_empty()).unwrap_or(false), "nem baixa nada");
    }
}
