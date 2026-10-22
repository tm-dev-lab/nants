//! `nants uninstall` e `nants uninstall --purge` (spec 103, decisão 7).
//!
//! - Remove o executável que o `install.sh` grava (`~/.local/bin/nants`). Com `--purge`, também a
//!   configuração (`$XDG_CONFIG_HOME/nants` ou `~/.config/nants`) e os dados do Nants
//!   (`$XDG_DATA_HOME/nants` ou `~/.local/share/nants`).
//! - Sempre pede confirmação lida do terminal (`/dev/tty`); sem terminal, recusa e explica.
//! - Nunca apaga a pasta dos projetos (`~/NantsProjects` ou a configurada) nem os recursos Docker
//!   com o rótulo `nants.project`: avisa e mostra o comando para listá-los. Uma pasta do `--purge`
//!   que contenha a pasta dos projetos (ou esteja dentro dela) também fica.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::i18n::{text, tr, Lang};

/// Comando para a pessoa listar os recursos Docker dos projetos do Nants.
pub const DOCKER_LIST_COMMAND: &str = "docker ps -a --filter label=nants.project && docker volume ls --filter label=nants.project && docker network ls --filter label=nants.project";

/// Comando de instalação, mostrado no fim.
pub const INSTALL_COMMAND: &str = crate::server::doctor::INSTALL_COMMAND;

/// O que o `nants uninstall` remove e o que ele preserva.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub purge: bool,
    /// Onde o `install.sh` grava o executável.
    pub executable: PathBuf,
    /// Pasta da configuração do Nants.
    pub config_dir: Option<PathBuf>,
    /// Pasta de dados do Nants.
    pub data_dir: Option<PathBuf>,
    /// Pasta dos projetos (nunca apagada).
    pub projects_root: Option<PathBuf>,
    /// Executável em execução, quando não é o instalado (fica onde está).
    pub running_exe: Option<PathBuf>,
    /// Arquivo de configuração que existe mas não pôde ser lido. Nesse caso a pasta dos projetos
    /// configurada é desconhecida e o `--purge` preserva as pastas de configuração e de dados.
    pub config_unreadable: Option<PathBuf>,
}

/// Resposta da confirmação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Yes,
    No,
    /// Não há terminal para perguntar (ex.: rodando por um script ou sem sessão).
    NoTerminal,
}

/// Resultado do `nants uninstall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Removeu tudo o que existia (ou não havia nada para remover).
    Done,
    /// A pessoa não confirmou, ou não havia terminal: nada foi removido.
    NotConfirmed,
    /// Alguma remoção falhou.
    Failed,
}

/// `true` se `a` está dentro de `b` ou é a própria `b` (comparação por componentes).
fn within(a: &Path, b: &Path) -> bool {
    a.starts_with(b)
}

impl Plan {
    /// Pastas que o `--purge` apaga de fato: existentes, chamadas `nants` e sem relação com a pasta
    /// dos projetos. A segunda lista são as preservadas, com a chave do motivo: contêm os projetos,
    /// ou a configuração não pôde ser lida (a pasta dos projetos pode estar dentro delas).
    fn purge_dirs(&self) -> (Vec<(&'static str, PathBuf)>, Vec<(&'static str, PathBuf)>) {
        let mut remove = Vec::new();
        let mut kept = Vec::new();
        if !self.purge {
            return (remove, kept);
        }
        for (key, dir) in [("uninstall.item.config", &self.config_dir), ("uninstall.item.data", &self.data_dir)] {
            let Some(dir) = dir else { continue };
            if !dir.exists() || dir.file_name().is_none_or(|n| n != "nants") {
                continue;
            }
            let touches_projects = self
                .projects_root
                .as_ref()
                .is_some_and(|root| within(root, dir) || within(dir, root));
            if self.config_unreadable.is_some() {
                kept.push(("uninstall.keep.config_unreadable", dir.clone()));
            } else if touches_projects {
                kept.push(("uninstall.keep.inside_projects", dir.clone()));
            } else {
                remove.push((key, dir.clone()));
            }
        }
        (remove, kept)
    }

    fn command(&self) -> &'static str {
        if self.purge {
            "nants uninstall --purge"
        } else {
            "nants uninstall"
        }
    }
}

fn line(out: &mut dyn Write, s: &str) {
    let _ = writeln!(out, "{s}");
}

/// Mostra o plano, pede a confirmação por `confirm` e remove. Não toca em nada fora do plano.
pub fn run(plan: &Plan, lang: Lang, out: &mut dyn Write, confirm: &mut dyn FnMut(&str) -> Confirm) -> Outcome {
    let exe_exists = plan.executable.symlink_metadata().is_ok();
    let (dirs, kept_dirs) = plan.purge_dirs();
    let exe_path = plan.executable.display().to_string();

    let mut kept: Vec<String> = Vec::new();
    if !plan.purge {
        let show = |p: &Option<PathBuf>| p.as_ref().map_or_else(String::new, |p| p.display().to_string());
        kept.push(tr(
            lang,
            "uninstall.keep.config_data",
            &[("config", &show(&plan.config_dir)), ("data", &show(&plan.data_dir))],
        ));
    }
    let unreadable = plan.config_unreadable.as_ref().map_or_else(String::new, |p| p.display().to_string());
    for (key, dir) in &kept_dirs {
        kept.push(tr(lang, key, &[("path", &dir.display().to_string()), ("config", &unreadable)]));
    }
    if let Some(root) = &plan.projects_root {
        kept.push(tr(lang, "uninstall.keep.projects", &[("path", &root.display().to_string())]));
    }
    kept.push(tr(lang, "uninstall.keep.docker", &[("command", DOCKER_LIST_COMMAND)]));
    if let Some(running) = &plan.running_exe {
        kept.push(tr(
            lang,
            "uninstall.keep.other_exe",
            &[("path", &running.display().to_string()), ("installed", &exe_path)],
        ));
    }

    if !exe_exists && dirs.is_empty() {
        line(out, &tr(lang, "uninstall.nothing", &[("path", &exe_path)]));
        line(out, &text(lang, "uninstall.keeps"));
        kept.iter().for_each(|k| line(out, k));
        return Outcome::Done;
    }

    line(out, &text(lang, "uninstall.will_remove"));
    if exe_exists {
        line(out, &tr(lang, "uninstall.item.executable", &[("path", &exe_path)]));
    }
    for (key, dir) in &dirs {
        line(out, &tr(lang, key, &[("path", &dir.display().to_string())]));
    }
    line(out, &text(lang, "uninstall.keeps"));
    kept.iter().for_each(|k| line(out, k));
    let _ = out.flush();

    match confirm(&text(lang, "uninstall.prompt")) {
        Confirm::Yes => {}
        Confirm::No => {
            line(out, &text(lang, "uninstall.canceled"));
            return Outcome::NotConfirmed;
        }
        Confirm::NoTerminal => {
            line(out, &tr(lang, "uninstall.no_terminal", &[("command", plan.command())]));
            return Outcome::NotConfirmed;
        }
    }

    let mut failed = false;
    let mut report = |path: &Path, result: std::io::Result<()>| {
        let p = path.display().to_string();
        match result {
            Ok(()) => line(out, &tr(lang, "uninstall.removed", &[("path", &p)])),
            Err(e) => {
                failed = true;
                line(out, &tr(lang, "uninstall.failed", &[("path", &p), ("error", &e.to_string())]));
            }
        }
    };
    if exe_exists {
        report(&plan.executable, std::fs::remove_file(&plan.executable));
    }
    for (_, dir) in &dirs {
        report(dir, std::fs::remove_dir_all(dir));
    }
    if failed {
        line(out, &text(lang, "uninstall.partial"));
        Outcome::Failed
    } else {
        line(out, &tr(lang, "uninstall.done", &[("command", INSTALL_COMMAND)]));
        Outcome::Done
    }
}

/// `true` para as respostas afirmativas nos dois idiomas (`s`, `sim`, `y`, `yes`).
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "s" | "sim" | "y" | "yes")
}

/// Confirmação lida do terminal (`/dev/tty`), nunca da entrada padrão: o prompt vai para o próprio
/// terminal. Sem terminal, [`Confirm::NoTerminal`].
pub fn confirm_from_tty(prompt: &str) -> Confirm {
    let Ok(tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") else {
        return Confirm::NoTerminal;
    };
    let mut writer = match tty.try_clone() {
        Ok(w) => w,
        Err(_) => return Confirm::NoTerminal,
    };
    if write!(writer, "{prompt} ").and_then(|()| writer.flush()).is_err() {
        return Confirm::NoTerminal;
    }
    let mut answer = String::new();
    match BufReader::new(tty).read_line(&mut answer) {
        Ok(n) if n > 0 && is_yes(&answer) => Confirm::Yes,
        _ => Confirm::No,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pasta temporária apagada no fim, mesmo em falha (CLAUDE.md).
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
        let dir = std::env::temp_dir().join(format!("nants-uninstall-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp");
        TempDir(dir)
    }

    /// Casa de teste com o executável, a configuração, os dados e um projeto.
    fn home(tag: &str) -> (TempDir, Plan) {
        let t = temp_dir(tag);
        let h = t.0.clone();
        for (file, body) in [
            (".local/bin/nants", "bin"),
            (".local/bin/outro", "outro"),
            (".config/nants/config.toml", "x = 1"),
            (".config/outro/config", "x"),
            (".local/share/nants/ui/state.json", "{}"),
            ("NantsProjects/app/README.md", "app"),
        ] {
            let p = h.join(file);
            std::fs::create_dir_all(p.parent().expect("pai")).expect("pasta");
            std::fs::write(&p, body).expect("arquivo");
        }
        let plan = Plan {
            purge: false,
            executable: h.join(".local/bin/nants"),
            config_dir: Some(h.join(".config/nants")),
            data_dir: Some(h.join(".local/share/nants")),
            projects_root: Some(h.join("NantsProjects")),
            running_exe: None,
            config_unreadable: None,
        };
        (t, plan)
    }

    fn run_with(plan: &Plan, lang: Lang, answer: Confirm) -> (Outcome, String, Vec<String>) {
        let mut out = Vec::new();
        let mut prompts = Vec::new();
        let outcome = run(plan, lang, &mut out, &mut |p| {
            prompts.push(p.to_string());
            answer
        });
        (outcome, String::from_utf8_lossy(&out).into_owned(), prompts)
    }

    #[test]
    fn default_removes_only_the_executable_and_keeps_config_data_projects() {
        let (t, plan) = home("default");
        let (outcome, out, prompts) = run_with(&plan, Lang::PtBr, Confirm::Yes);
        assert_eq!(outcome, Outcome::Done, "{out}");
        assert_eq!(prompts.len(), 1, "pede confirmação uma vez");
        assert!(!plan.executable.exists());
        let h = &t.0;
        assert!(h.join(".local/bin/outro").exists());
        assert!(h.join(".config/nants/config.toml").exists());
        assert!(h.join(".local/share/nants/ui/state.json").exists());
        assert!(h.join("NantsProjects/app/README.md").exists());
        assert!(out.contains("nants uninstall --purge"), "{out}");
        assert!(out.contains(DOCKER_LIST_COMMAND), "{out}");
        assert!(out.contains(&format!("{}", h.join("NantsProjects").display())), "{out}");
    }

    #[test]
    fn purge_removes_config_and_data_but_never_projects() {
        let (t, mut plan) = home("purge");
        plan.purge = true;
        let (outcome, out, _) = run_with(&plan, Lang::En, Confirm::Yes);
        assert_eq!(outcome, Outcome::Done, "{out}");
        let h = &t.0;
        assert!(!plan.executable.exists());
        assert!(!h.join(".config/nants").exists());
        assert!(!h.join(".local/share/nants").exists());
        assert!(h.join(".config/outro/config").exists(), "só a pasta nants");
        assert!(h.join(".local/bin/outro").exists());
        assert!(h.join("NantsProjects/app/README.md").exists(), "projetos nunca são apagados");
        assert!(out.contains("Nants uninstalled") && out.contains(DOCKER_LIST_COMMAND), "{out}");
    }

    #[test]
    fn purge_keeps_a_folder_that_holds_the_projects() {
        let (t, mut plan) = home("inside");
        plan.purge = true;
        let root = t.0.join(".local/share/nants/projetos");
        std::fs::create_dir_all(&root).expect("pasta");
        plan.projects_root = Some(root.clone());
        let (outcome, out, _) = run_with(&plan, Lang::PtBr, Confirm::Yes);
        assert_eq!(outcome, Outcome::Done, "{out}");
        assert!(root.exists(), "a pasta dos projetos fica");
        assert!(!t.0.join(".config/nants").exists());
        assert!(out.contains("contém a pasta dos projetos"), "{out}");
    }

    #[test]
    fn purge_with_unreadable_config_keeps_config_and_data_dirs() {
        let (t, mut plan) = home("unreadable");
        plan.purge = true;
        // A configuração (ilegível) apontava os projetos para dentro da pasta de dados.
        let projects = t.0.join(".local/share/nants/meus-projetos/app/README.md");
        std::fs::create_dir_all(projects.parent().expect("pai")).expect("pasta");
        std::fs::write(&projects, "app").expect("arquivo");
        let cfg = t.0.join(".config/nants/config.toml");
        std::fs::write(&cfg, "isto nao e toml [").expect("arquivo");
        plan.config_unreadable = Some(cfg.clone());
        plan.projects_root = None;
        let (outcome, out, _) = run_with(&plan, Lang::PtBr, Confirm::Yes);
        assert_eq!(outcome, Outcome::Done, "{out}");
        assert!(!plan.executable.exists(), "o executável sai");
        assert!(projects.exists() && cfg.exists(), "configuração e dados ficam: {out}");
        assert!(out.contains("não pôde ser lida") && out.contains(&cfg.display().to_string()), "{out}");
        std::fs::write(&plan.executable, "bin").expect("arquivo");
        let (_, out, _) = run_with(&plan, Lang::En, Confirm::No);
        assert!(out.contains("could not be read"), "{out}");
    }

    #[test]
    fn no_or_no_terminal_removes_nothing() {
        for (answer, expected) in [(Confirm::No, "Cancelado"), (Confirm::NoTerminal, "Sem terminal")] {
            let (t, mut plan) = home("no");
            plan.purge = true;
            let (outcome, out, _) = run_with(&plan, Lang::PtBr, answer);
            assert_eq!(outcome, Outcome::NotConfirmed);
            assert!(out.contains(expected), "{out}");
            assert!(plan.executable.exists() && t.0.join(".config/nants").exists() && t.0.join(".local/share/nants").exists());
        }
        let (_t, plan) = home("noterm-en");
        let (_, out, _) = run_with(&plan, Lang::En, Confirm::NoTerminal);
        assert!(out.contains("No terminal") && out.contains("nants uninstall"), "{out}");
    }

    #[test]
    fn nothing_installed_does_not_ask() {
        let (_t, plan) = home("nothing");
        std::fs::remove_file(&plan.executable).expect("rm");
        let (outcome, out, prompts) = run_with(&plan, Lang::PtBr, Confirm::Yes);
        assert_eq!(outcome, Outcome::Done);
        assert!(prompts.is_empty(), "sem nada para remover, não pergunta");
        assert!(out.contains("Nada para remover"), "{out}");
    }

    #[test]
    fn running_executable_elsewhere_is_mentioned_and_kept() {
        let (t, mut plan) = home("other");
        let other = t.0.join("dev/nants");
        std::fs::create_dir_all(other.parent().expect("pai")).expect("pasta");
        std::fs::write(&other, "dev").expect("arquivo");
        plan.running_exe = Some(other.clone());
        let (_, out, _) = run_with(&plan, Lang::PtBr, Confirm::Yes);
        assert!(other.exists());
        assert!(out.contains(&other.display().to_string()), "{out}");
    }

    #[test]
    fn yes_answers_in_both_languages() {
        for a in ["s", "S\n", " sim ", "y", "YES\n"] {
            assert!(is_yes(a), "{a}");
        }
        for a in ["", "n", "nao", "no", "talvez", "ss"] {
            assert!(!is_yes(a), "{a}");
        }
    }
}
