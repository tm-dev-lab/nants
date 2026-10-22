//! `nants uninstall` (spec 103, T04): roda o binário numa sessão nova, sem terminal de controle
//! (`setsid`), com HOME e XDG temporários (guarda com `Drop`). Sem terminal, a confirmação não
//! pode ser lida: nada é removido e a mensagem explica como confirmar. O fluxo com confirmação e o
//! `--purge` são testados em `nants::uninstall` (confirmação injetada).

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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
    let dir = std::env::temp_dir().join(format!("nants-uninstall-it-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("diretório temporário");
    TempDir(dir)
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().expect("pai")).expect("pasta");
    std::fs::write(path, "x").expect("arquivo");
}

fn uninstall(home: &Path, args: &[&str], lang: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nants"));
    cmd.arg("uninstall")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("cfg"))
        .env("XDG_DATA_HOME", home.join("dados"))
        .env("LANG", lang)
        .stdin(Stdio::null());
    // Sessão nova: sem terminal de controle, /dev/tty não abre.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.output().expect("nants uninstall")
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn without_a_terminal_nothing_is_removed_and_the_message_explains() {
    let t = temp_dir("notty");
    let h = &t.0;
    let files = [
        h.join(".local/bin/nants"),
        h.join("cfg/nants/config.toml"),
        h.join("dados/nants/ui/state.json"),
        h.join("NantsProjects/app/README.md"),
    ];
    files.iter().for_each(|f| touch(f));
    // Configuração válida (vazia = padrão), para o --purge planejar as pastas.
    std::fs::write(&files[1], "").expect("config");

    let out = uninstall(h, &["--purge"], "pt_BR.UTF-8");
    let all = text(&out);
    assert!(!out.status.success(), "{all}");
    assert!(all.contains("Sem terminal para confirmar") && all.contains("nants uninstall --purge"), "{all}");
    assert!(all.contains(&h.join("cfg/nants").display().to_string()), "{all}");
    assert!(all.contains(&h.join("dados/nants").display().to_string()), "{all}");
    assert!(all.contains(&h.join("NantsProjects").display().to_string()), "{all}");
    assert!(all.contains("docker ps -a --filter label=nants.project"), "{all}");
    assert!(files.iter().all(|f| f.exists()), "nada removido sem confirmação");

    let out = uninstall(h, &[], "en_US.UTF-8");
    let all = text(&out);
    assert!(!out.status.success() && all.contains("No terminal to confirm"), "{all}");
    assert!(all.contains("nants uninstall --purge"), "mostra como apagar também a configuração: {all}");
    assert!(files.iter().all(|f| f.exists()));
}

#[test]
fn purge_with_unreadable_config_does_not_plan_to_remove_config_or_data() {
    let t = temp_dir("badcfg");
    let h = &t.0;
    touch(&h.join(".local/bin/nants"));
    // Configuração inválida que apontaria os projetos para dentro da pasta de dados.
    std::fs::create_dir_all(h.join("cfg/nants")).expect("pasta");
    std::fs::write(h.join("cfg/nants/config.toml"), "projects_root = \"dados/nants/projetos\" [quebrado").expect("config");
    touch(&h.join("dados/nants/projetos/app/README.md"));

    let out = uninstall(h, &["--purge"], "pt_BR.UTF-8");
    let all = text(&out);
    let plan = all.split("Não são removidos:").next().unwrap_or("");
    assert!(plan.contains("o executável") && !plan.contains("a configuração:") && !plan.contains("os dados do Nants:"), "{all}");
    assert!(all.contains("não pôde ser lida"), "{all}");
    assert!(!all.contains("NantsProjects"), "sem a configuração, a pasta dos projetos é desconhecida: {all}");
    assert!(h.join("dados/nants/projetos/app/README.md").exists());
}

#[test]
fn nothing_installed_exits_ok_and_unknown_flag_is_an_error() {
    let t = temp_dir("nothing");
    let out = uninstall(&t.0, &[], "pt_BR.UTF-8");
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("Nada para remover"), "{}", text(&out));

    let out = uninstall(&t.0, &["--yes"], "pt_BR.UTF-8");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(text(&out).contains("argumento desconhecido: --yes"), "{}", text(&out));
}
