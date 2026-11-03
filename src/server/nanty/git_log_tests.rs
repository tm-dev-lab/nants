//! Testes do histórico do Git do Nanty (spec 140 T09, C-10), com um `.git/config` armadilha.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::tools;
use super::*;
use crate::config::NantyRead;
use crate::server::events::SecretSet;
use crate::server::nanty::files::Root;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-t09-git-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).expect("dir");
    d
}

/// `git` do teste, isolado da configuração de quem roda.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=Ana Dev", "-c", "user.email=ana@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// Repositório com dois commits (o segundo com segredo no assunto).
fn repo(tag: &str, secret_subject: &str) -> (PathBuf, PathBuf) {
    let base = temp(tag);
    let root = base.join("proj");
    fs::create_dir_all(&root).expect("proj");
    git(&root, &["init", "-q"]);
    fs::write(root.join("a.txt"), "um\n").expect("a");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "primeiro"]);
    fs::write(root.join("a.txt"), "um\ndois\ntres\n").expect("a");
    fs::write(root.join("b.txt"), "b\n").expect("b");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", secret_subject]);
    (base, root)
}

/// Script que cria `marker` quando roda.
fn trap_script(base: &Path, marker: &Path) -> PathBuf {
    let script = base.join("armadilha.sh");
    fs::write(&script, format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display())).expect("script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("rt")
}

#[test]
fn log_reads_summary_with_redaction_and_envelope() {
    let key = format!("ghp_{}", "Zx7".repeat(10));
    let (_b, root) = repo("ok", &format!("troca a chave {key} <fim>"));
    let r = Root::open(&root).expect("raiz");
    let commits = rt().block_on(log(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 10)).expect("log");
    assert_eq!(commits.len(), 2);
    let last = &commits[0];
    assert!(!last.subject.contains(&key) && last.subject.starts_with("troca a chave"), "{}", last.subject);
    assert_eq!(last.author, "Ana Dev");
    assert_eq!((last.files, last.insertions, last.deletions), (2, 3, 0));
    assert!(last.hash.len() >= 7 && last.committed_at > 0);
    assert_eq!(commits[1].subject, "primeiro");
    let env = untrusted(&commits);
    assert!(env.starts_with(tools::UNTRUSTED_OPEN) && !env.contains("ana@example.com") && !env.contains("<fim>"), "{env}");
    // O `-n` respeita o teto.
    let one = NantyRead { git_max_commits: 1, ..NantyRead::default() };
    assert_eq!(rt().block_on(log(&r, &one, &SecretSet::new(Vec::new()), 50)).expect("log").len(), 1);
}

#[test]
fn the_repository_config_runs_nothing() {
    let (base, root) = repo("trap", "segundo");
    let marker = base.join("MARCADOR");
    let script = trap_script(&base, &marker);
    let s = script.display();
    let hooks = base.join("hooks");
    fs::create_dir_all(&hooks).expect("hooks");
    for hook in ["post-checkout", "post-index-change", "reference-transaction"] {
        fs::copy(&script, hooks.join(hook)).expect("hook");
    }
    let config = format!(
        "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\tfsmonitor = {s}\n\tpager = {s}\n\thooksPath = {h}\n\
         [diff]\n\texternal = {s}\n[diff \"armadilha\"]\n\ttextconv = {s}\n[log]\n\tshowSignature = true\n[gpg]\n\tprogram = {s}\n\
         [pager]\n\tlog = {s}\n",
        h = hooks.display()
    );
    fs::write(root.join(".git/config"), config).expect("config");
    fs::write(root.join(".gitattributes"), "* diff=armadilha\n").expect("attrs");
    // A armadilha é real: o `git` comum roda o programa da configuração.
    let plain = Command::new("git").args(["diff", "HEAD~1", "HEAD"]).current_dir(&root).output().expect("git");
    assert!(plain.status.success());
    assert!(marker.exists(), "a armadilha devia disparar no git comum");
    fs::remove_file(&marker).expect("rm");
    let r = Root::open(&root).expect("raiz");
    let commits = rt().block_on(log(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 10)).expect("log");
    assert_eq!(commits.len(), 2);
    assert!(!marker.exists(), "a configuração do repositório rodou um programa");
}

#[test]
fn args_neutralize_the_config_and_never_ask_for_patches() {
    let args = log_args(5);
    for needed in ["--no-pager", "--no-ext-diff", "--no-textconv", "core.fsmonitor=false", "diff.external=", "log.showSignature=false", "core.hooksPath=/dev/null"] {
        assert!(args.iter().any(|a| a == needed), "{needed}");
    }
    assert!(args.windows(2).any(|w| w[0] == "-n" && w[1] == "5"));
    assert!(!args.iter().any(|a| a == "-p" || a == "--patch" || a.starts_with("--stat=") || a == "--stat"));
}

#[test]
fn no_repo_or_linked_git_is_refused() {
    let base = temp("norepo");
    let root = base.join("proj");
    fs::create_dir_all(&root).expect("proj");
    let r = Root::open(&root).expect("raiz");
    let none = rt().block_on(log(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 5));
    assert_eq!(none.err().map(GitReadError::code), Some("nanty_git_not_repo"));
    // Sem `.git` na raiz, o `git` também não sobe para um repositório acima dela.
    let (_b, outer) = repo("outer", "fora");
    let inner = outer.join("dentro");
    fs::create_dir_all(inner.join(".git")).expect("git falso");
    let r = Root::open(&inner).expect("raiz");
    let fake = rt().block_on(log(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 5));
    assert_eq!(fake.err(), Some(GitReadError::Failed));
    let linked = base.join("ligado");
    fs::create_dir_all(&linked).expect("ligado");
    symlink(outer.join(".git"), linked.join(".git")).expect("ln");
    let r = Root::open(&linked).expect("raiz");
    let refused = rt().block_on(log(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 5));
    assert_eq!(refused.err(), Some(GitReadError::Symlink));
}

#[test]
fn shortstat_parses_partial_lines() {
    assert_eq!(shortstat(" 1 file changed, 2 deletions(-)"), (1, 0, 2));
    assert_eq!(shortstat(" 4 files changed, 9 insertions(+), 1 deletion(-)"), (4, 9, 1));
    assert_eq!(shortstat(""), (0, 0, 0));
}

#[test]
fn output_above_the_cap_is_cut_not_timed_out() {
    let (_b, root) = repo("cap", "segundo");
    let r = Root::open(&root).expect("raiz");
    let all = rt().block_on(log_capped(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 10, MAX_OUTPUT_BYTES)).expect("log");
    assert_eq!(all.len(), 2);
    // Teto que cabe só o primeiro registro: o segundo, pela metade, sai; nada de prazo estourado.
    // Tamanho do primeiro registro: marcador, campos e a linha ` 2 files changed, 3 insertions(+)`.
    let first = 3 + all[0].hash.len() + 1 + "Ana Dev".len() + 1 + all[0].committed_at.to_string().len() + 1 + "segundo".len() + 2 + 34;
    let cut = rt().block_on(log_capped(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 10, first as u64 + 25)).expect("cortado");
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0].subject, "segundo");
    let none = rt().block_on(log_capped(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 10, 8)).expect("pouco");
    assert!(none.is_empty());
}

#[test]
fn a_gitdir_file_pointing_outside_the_project_is_refused() {
    let (base, outer) = repo("gitdir-out", "fora");
    let proj = base.join("outro");
    fs::create_dir_all(&proj).expect("proj");
    fs::write(proj.join(".git"), format!("gitdir: {}\n", outer.join(".git").display())).expect("gitdir");
    let r = Root::open(&proj).expect("raiz");
    let refused = rt().block_on(log(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 5));
    assert_eq!(refused.err().map(GitReadError::code), Some("nanty_git_outside"));
    // Relativo que sobe também é fora.
    fs::write(proj.join(".git"), "gitdir: ../proj/.git\n").expect("gitdir");
    let refused = rt().block_on(log(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 5));
    assert_eq!(refused.err(), Some(GitReadError::Outside));
    // Dentro do projeto vale.
    let inside = base.join("dentro");
    fs::create_dir_all(&inside).expect("dentro");
    fs::rename(outer.join(".git"), inside.join(".repo")).expect("mv");
    fs::write(inside.join(".git"), "gitdir: .repo\n").expect("gitdir");
    let r = Root::open(&inside).expect("raiz");
    assert!(rt().block_on(log(&r, &NantyRead::default(), &SecretSet::new(Vec::new()), 5)).is_ok());
}
