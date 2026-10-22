//! Formatação automática por módulo (spec 078): seleção dos formatadores pelos arquivos alterados,
//! montagem do comando por `workdir` e a parte Git do ciclo (snapshot, descarte, commit só do escopo).
//! A execução dos comandos fica com o runner, que sabe se o projeto roda no host ou no Compose.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::schema::Formatter;
use crate::harness::git::Git;
use crate::harness::tools::glob_match_path;

/// Formatadores (em ordem) cujos `paths` casam com ao menos um arquivo de `changed`.
pub fn select<'a>(formatters: &'a [Formatter], changed: &[String]) -> Vec<&'a Formatter> {
    formatters
        .iter()
        .filter(|f| f.paths.iter().any(|glob| changed.iter().any(|path| glob_match_path(glob, path))))
        .collect()
}

/// `true` quando `path` cabe em `allowed_scope`. Nunca cabem `.git`, `.nants/**` (ambiente aprovado)
/// nem `.sdd/**` (estado do Nants), mesmo com escopo largo.
pub fn in_scope(path: &str, scope: &[String]) -> bool {
    let first = path.split('/').next().unwrap_or_default();
    if first == ".nants" || first == ".sdd" || path.split('/').any(|c| c == ".git") {
        return false;
    }
    scope.iter().any(|glob| glob_match_path(glob, path))
}

/// Aspas simples para o shell.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Lista para o briefing do Revisor: `nome: comando (em workdir)`, um por linha; vazia sem formatadores.
pub fn briefing_list(formatters: &[Formatter]) -> String {
    formatters
        .iter()
        .map(|f| {
            let place = if f.workdir == "." { String::new() } else { format!(" (em {})", f.workdir) };
            format!("- {}: `{}`{place}", f.name, f.command)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Comando para o workspace do Compose (cujo diretório de trabalho é a raiz da worktree): entra no
/// `workdir` antes de rodar o formatador.
pub fn workspace_command(f: &Formatter) -> String {
    if f.workdir == "." {
        f.command.clone()
    } else {
        format!("cd -- {} && {}", shell_quote(&f.workdir), f.command)
    }
}

/// Diretório de execução no host: `wt/workdir`, que precisa existir e, resolvidos os symlinks,
/// ficar dentro da worktree.
pub fn host_dir(wt: &Path, f: &Formatter) -> Result<PathBuf, String> {
    let root = wt.canonicalize().map_err(|e| format!("worktree inacessível: {e}"))?;
    let dir = root
        .join(&f.workdir)
        .canonicalize()
        .map_err(|_| format!("workdir `{}` não existe na worktree", f.workdir))?;
    if !dir.starts_with(&root) || !dir.is_dir() {
        return Err(format!("workdir `{}` fora da worktree", f.workdir));
    }
    Ok(dir)
}

fn git_output(wt: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("--literal-pathspecs")
        .args(args)
        .current_dir(wt)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))
}

fn git_ok(wt: &Path, args: &[&str]) -> Result<(), String> {
    let out = git_output(wt, args)?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!("git {} falhou: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
}

/// Fixa o estado atual da árvore no índice: ponto de retorno de [`discard`].
pub fn snapshot(wt: &Path) -> Result<(), String> {
    git_ok(wt, &["add", "-A"])
}

/// Caminhos com mudança (modificada, nova ou removida) desde o último [`snapshot`]; o que já estava
/// no índice não conta.
pub fn changed_paths(wt: &Path) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for args in [
        &["diff", "--name-only", "--no-renames", "-z"][..],
        &["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        let out = git_output(wt, args)?;
        if !out.status.success() {
            return Err(format!("git {} falhou: {}", args[0], String::from_utf8_lossy(&out.stderr).trim()));
        }
        paths.extend(
            String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter(|p| !p.is_empty())
                .map(str::to_string),
        );
    }
    Ok(paths)
}

/// `true` quando alguma mudança desde o último [`snapshot`] cai dentro de `scope`.
pub fn touches_scope(wt: &Path, scope: &[String]) -> Result<bool, String> {
    Ok(changed_paths(wt)?.iter().any(|p| in_scope(p, scope)))
}

/// Volta a árvore ao último [`snapshot`]: restaura os rastreados e remove os não rastreados
/// (arquivos ignorados, como `node_modules`, ficam).
pub fn discard(wt: &Path) -> Result<(), String> {
    git_ok(wt, &["checkout", "--", "."])?;
    git_ok(wt, &["clean", "-fdq"])
}

/// Descarta tudo o que não está no HEAD (último recurso após falha inesperada).
pub fn reset_to_head(wt: &Path) -> Result<(), String> {
    git_ok(wt, &["reset", "-q", "--hard", "HEAD"])?;
    git_ok(wt, &["clean", "-fdq"])
}

/// Fecha o ciclo: indexa o que os formatadores mudaram, restaura o que cai fora de `scope` e
/// commita o restante em um único commit `message`. Devolve os arquivos commitados (vazio = nada
/// mudou, sem commit).
pub fn finalize(wt: &Path, scope: &[String], message: &str) -> Result<Vec<String>, String> {
    let git = Git::new(wt.to_path_buf());
    git.add_all().map_err(|e| e.to_string())?;
    let mut kept = Vec::new();
    for path in git.staged_paths().map_err(|e| e.to_string())? {
        if in_scope(&path, scope) {
            kept.push(path);
            continue;
        }
        git_ok(wt, &["reset", "-q", "HEAD", "--", &path])?;
        let in_head = git_output(wt, &["cat-file", "-e", &format!("HEAD:{path}")])?.status.success();
        if in_head {
            git_ok(wt, &["checkout", "--", &path])?;
        } else {
            let target = wt.join(&path);
            if target.is_dir() {
                let _ = std::fs::remove_dir_all(&target);
            } else {
                let _ = std::fs::remove_file(&target);
            }
        }
    }
    if kept.is_empty() {
        return Ok(kept);
    }
    git.commit(message).map_err(|e| e.to_string())?;
    kept.sort();
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fmt(name: &str, workdir: &str, paths: &[&str]) -> Formatter {
        Formatter {
            name: name.into(),
            command: "true".into(),
            workdir: workdir.into(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn names<'a>(v: &[&'a Formatter]) -> Vec<&'a str> {
        v.iter().map(|f| f.name.as_str()).collect()
    }

    fn out_text(wt: &Path, args: &[&str]) -> String {
        String::from_utf8_lossy(&git_output(wt, args).expect("git").stdout).trim().to_string()
    }

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-fmt-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "T"],
            &["config", "user.email", "t@e.x"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git_ok(&dir, args).expect("git");
        }
        fs::write(dir.join(".gitignore"), "ignorado/\n").expect("w");
        fs::create_dir_all(dir.join("src")).expect("d");
        fs::create_dir_all(dir.join("docs")).expect("d");
        fs::write(dir.join("src/a.txt"), "a\n").expect("w");
        fs::write(dir.join("docs/b.txt"), "b\n").expect("w");
        git_ok(&dir, &["add", "-A"]).expect("add");
        git_ok(&dir, &["commit", "-qm", "inicial"]).expect("commit");
        dir
    }

    #[test]
    fn select_by_paths_in_order() {
        let all = vec![
            fmt("web", "web", &["web/**"]),
            fmt("rust", ".", &["**/*.rs", "Cargo.toml"]),
            fmt("tudo", ".", &["**"]),
        ];
        assert_eq!(names(&select(&all, &["web/src/app.ts".into()])), vec!["web", "tudo"]);
        assert_eq!(names(&select(&all, &["src/lib.rs".into()])), vec!["rust", "tudo"]);
        assert_eq!(names(&select(&all, &["Cargo.toml".into()])), vec!["rust", "tudo"]);
        assert_eq!(names(&select(&all[..2], &["README.md".into()])), Vec::<&str>::new());
        assert!(select(&[], &["a".into()]).is_empty());
    }

    #[test]
    fn scope_excludes_state_dirs_even_with_wide_scope() {
        let wide = vec!["**".to_string()];
        assert!(in_scope("src/a.rs", &wide));
        assert!(!in_scope(".sdd/settings.yaml", &wide));
        assert!(!in_scope(".nants/compose.yaml", &wide));
        assert!(!in_scope("x/.git/config", &wide));
        assert!(!in_scope("docs/b.txt", &["src/**".to_string()]));
    }

    #[test]
    fn workspace_command_enters_workdir_with_quoting() {
        let mut f = fmt("web", "web app", &["**"]);
        f.command = "npx prettier --write .".into();
        assert_eq!(workspace_command(&f), "cd -- 'web app' && npx prettier --write .");
        f.workdir = "o'k".into();
        assert_eq!(workspace_command(&f), "cd -- 'o'\\''k' && npx prettier --write .");
        f.workdir = "-rf".into();
        assert!(workspace_command(&f).starts_with("cd -- '-rf' && "), "workdir com hífen não vira opção do cd");
        f.workdir = ".".into();
        assert_eq!(workspace_command(&f), "npx prettier --write .");
    }

    #[test]
    fn briefing_list_names_commands_and_workdirs() {
        assert_eq!(briefing_list(&[]), "");
        let mut web = fmt("web", "web", &["web/**"]);
        web.command = "npx prettier --write .".into();
        let mut root = fmt("rust", ".", &["**"]);
        root.command = "cargo fmt".into();
        assert_eq!(briefing_list(&[web, root]), "- web: `npx prettier --write .` (em web)\n- rust: `cargo fmt`");
    }

    #[test]
    fn host_dir_stays_inside_the_worktree() {
        let wt = repo("hostdir");
        assert!(host_dir(&wt, &fmt("a", "src", &["**"])).expect("ok").ends_with("src"));
        assert!(host_dir(&wt, &fmt("a", ".", &["**"])).is_ok());
        assert!(host_dir(&wt, &fmt("a", "nao-existe", &["**"])).is_err());
        let outside = std::env::temp_dir().join(format!("nants-fmt-out-{}", std::process::id()));
        fs::create_dir_all(&outside).expect("d");
        std::os::unix::fs::symlink(&outside, wt.join("link")).expect("symlink");
        assert!(host_dir(&wt, &fmt("a", "link", &["**"])).is_err(), "symlink para fora é recusado");
    }

    #[test]
    fn finalize_commits_only_scope_and_reverts_the_rest() {
        let wt = repo("finalize");
        let before = out_text(&wt, &["rev-parse", "HEAD"]);
        snapshot(&wt).expect("snapshot");
        fs::write(wt.join("src/a.txt"), "A\n").expect("w"); // no escopo
        fs::write(wt.join("src/novo.txt"), "n\n").expect("w"); // no escopo, novo
        fs::write(wt.join("docs/b.txt"), "B\n").expect("w"); // fora, rastreado
        fs::write(wt.join("docs/criado.txt"), "c\n").expect("w"); // fora, novo
        fs::create_dir_all(wt.join("ignorado")).expect("d");
        fs::write(wt.join("ignorado/x"), "x").expect("w"); // ignorado: nem aparece
        assert_eq!(changed_paths(&wt).expect("status").len(), 4, "o ignorado não aparece");
        assert!(touches_scope(&wt, &["src/**".to_string()]).expect("status"));
        let files = finalize(&wt, &["src/**".to_string()], "style(s1): T01 - formatação automática").expect("finalize");
        assert_eq!(files, vec!["src/a.txt", "src/novo.txt"]);
        assert_ne!(out_text(&wt, &["rev-parse", "HEAD"]), before);
        assert_eq!(fs::read_to_string(wt.join("docs/b.txt")).expect("r"), "b\n", "fora do escopo restaurado");
        assert!(!wt.join("docs/criado.txt").exists(), "novo fora do escopo removido");
        assert!(wt.join("ignorado/x").exists());
        assert!(changed_paths(&wt).expect("status").is_empty(), "árvore limpa");
        assert_eq!(out_text(&wt, &["log", "-1", "--format=%s"]), "style(s1): T01 - formatação automática");
        let shown = out_text(&wt, &["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(shown.lines().collect::<Vec<_>>(), vec!["src/a.txt", "src/novo.txt"]);
    }

    #[test]
    fn finalize_without_changes_makes_no_commit() {
        let wt = repo("nochange");
        let before = out_text(&wt, &["rev-parse", "HEAD"]);
        snapshot(&wt).expect("snapshot");
        assert_eq!(finalize(&wt, &["**".to_string()], "m").expect("finalize"), Vec::<String>::new());
        assert_eq!(out_text(&wt, &["rev-parse", "HEAD"]), before);
        // Só mudanças fora do escopo: também sem commit, e a árvore volta ao estado anterior.
        fs::write(wt.join("docs/b.txt"), "B\n").expect("w");
        assert!(finalize(&wt, &["src/**".to_string()], "m").expect("finalize").is_empty());
        assert_eq!(out_text(&wt, &["rev-parse", "HEAD"]), before);
        assert_eq!(fs::read_to_string(wt.join("docs/b.txt")).expect("r"), "b\n");
    }

    #[test]
    fn discard_returns_to_the_last_snapshot_keeping_earlier_work() {
        let wt = repo("discard");
        fs::write(wt.join("src/a.txt"), "do primeiro\n").expect("w");
        snapshot(&wt).expect("snapshot");
        fs::write(wt.join("src/a.txt"), "do segundo\n").expect("w");
        fs::write(wt.join("src/lixo.txt"), "x\n").expect("w");
        fs::create_dir_all(wt.join("ignorado")).expect("d");
        fs::write(wt.join("ignorado/x"), "x").expect("w");
        discard(&wt).expect("discard");
        assert_eq!(fs::read_to_string(wt.join("src/a.txt")).expect("r"), "do primeiro\n");
        assert!(!wt.join("src/lixo.txt").exists());
        assert!(wt.join("ignorado/x").exists(), "ignorados não são tocados");
    }
}
