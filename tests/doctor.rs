//! `nants doctor` (spec 103, T02): roda o binário com `git` e `docker` falsos no PATH (scripts sh),
//! sem depender do Docker real nem da rede, e confere as linhas e o código de saída.

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

#[path = "support/isolated_bin.rs"]
mod isolated_bin;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("nants-doctor-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("diretório temporário");
    dir
}

fn script(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
    let mut perms = std::fs::metadata(&path).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
}

const FAKE_DOCKER: &str = r#"case "$1" in
  --version) echo "Docker version 29.0.0, build fake" ;;
  info) echo "29.0.0" ;;
  compose) echo "2.40.0" ;;
  image) echo "v1.48.0-noble" ;;
  *) exit 1 ;;
esac"#;

fn run_doctor(dir: &Path, bin: &Path) -> Output {
    run_doctor_in(dir, bin, "pt_BR.UTF-8")
}

/// `nants doctor` com o locale dado em `LC_ALL` (spec 102: LC_ALL > LC_MESSAGES > LANG).
fn run_doctor_in(dir: &Path, bin: &Path, lc_all: &str) -> Output {
    isolated_bin::nants(dir)
        .args(["doctor", "--config"])
        .arg(dir.join("config.toml"))
        .env("PATH", bin)
        // Endereço inválido de propósito: o item da versão não consulta a rede no teste.
        .env("NANTS_RELEASES_URL", "ftp://sem-rede.invalid")
        .env("LC_ALL", lc_all)
        .env("LC_MESSAGES", "pt_BR.UTF-8")
        .env("LANG", "pt_BR.UTF-8")
        .stdin(Stdio::null())
        .output()
        .expect("executa o nants doctor")
}

#[test]
fn doctor_ok_with_everything_and_fails_without_docker() {
    let dir = temp_dir("ok");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    script(&bin, "git", "echo 'git version 2.43.0'");
    script(&bin, "docker", FAKE_DOCKER);

    let out = run_doctor(&dir, &bin);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    for line in [
        "OK    Git: git version 2.43.0",
        "OK    Docker daemon: 29.0.0",
        "OK    Docker Compose v2: 2.40.0",
        "Porta do Nants",
        "Espaço em disco",
        "INFO  WSL",
        "INFO  Imagens do Playwright: já baixadas: v1.48.0-noble",
        "INFO  Versão do Nants",
        "Tudo pronto",
    ] {
        assert!(text.contains(line), "faltou '{line}' em:\n{text}");
    }
    assert!(!text.contains('\u{2014}') && !text.contains('\u{2013}'), "{text}");

    // Sem docker no PATH: obrigatório faltando, código de saída diferente de zero.
    std::fs::remove_file(bin.join("docker")).expect("remove docker");
    let out = run_doctor(&dir, &bin);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("FALHA Docker CLI: docker não encontrado. Ação: Instale o Docker Engine"), "{text}");
    assert!(text.contains("Falta algo obrigatório"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Spec 102: com `LC_ALL=C` (ou outro locale que não é pt), o doctor fala inglês, inclusive com
/// `LANG` em pt (LC_ALL vence).
#[test]
fn doctor_follows_the_terminal_locale() {
    let dir = temp_dir("locale");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    script(&bin, "git", "echo 'git version 2.43.0'");
    script(&bin, "docker", FAKE_DOCKER);
    for locale in ["C", "en_US.UTF-8", "POSIX"] {
        let out = run_doctor_in(&dir, &bin, locale);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{text}");
        for line in ["environment check", "OK    Git: git version 2.43.0", "Nants port", "Disk space", "All set"] {
            assert!(text.contains(line), "{locale}: faltou '{line}' em:\n{text}");
        }
        assert!(!text.contains("diagnóstico") && !text.contains("Tudo pronto"), "{text}");
    }
    std::fs::remove_file(bin.join("docker")).expect("remove docker");
    let out = run_doctor_in(&dir, &bin, "C.UTF-8");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("FAIL  Docker CLI: docker not found. Action: Install Docker Engine"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
