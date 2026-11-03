//! Spec 140 T09 (C-10, F-02): o único `git add` amplo do Nants é o `Git::stage`
//! (`src/harness/git.rs`), que não desce em repositório aninhado: tira os gitlinks do pathspec e
//! afasta o `.git` das pastas novas. Um `add -A` comum desce num gitlink e roda o `git` filho dele,
//! com a configuração e os atributos escritos pelo agente (validado no Git 2.43, no teste
//! `nested_repo_config_runs_nothing`); nenhuma opção `-c` impede isso.
//!
//! A trava lê os tokens de produção de `src/**/*.rs` (sem comentários, sem os itens `#[cfg(test)]`
//! e sem os arquivos `*_tests.rs`) e falha se algum literal `"add"` (ou `"stage"`) vier seguido, na
//! mesma instrução, de uma forma ampla (`-A`, `--all`, `.`, `-u`, `--update`, `--no-ignore-removal`
//! ou `:/`), ou se um literal trouxer o comando inteiro (`add -A`, `add .` e afins), fora do corpo
//! de `fn stage` em `harness/git.rs`.

#[path = "support/rust_tokens.rs"]
mod rust_tokens;

use rust_tokens::{matching, production_sources, production_tokens, Token};

/// Formas amplas do `git add`.
const BROAD: &[&str] = &["-A", "--all", ".", "-u", "--update", "--no-ignore-removal", ":/"];
/// Tokens olhados depois do `"add"`, no máximo (a instrução termina antes, no `;`).
const WINDOW: usize = 16;
/// O único lugar permitido: arquivo e função.
const ALLOWED: (&str, &str) = ("harness/git.rs", "stage");

/// Corpo (`{` .. `}`) de `fn <name>` nos tokens, se houver.
fn fn_body(toks: &[Token], name: &str) -> Option<(usize, usize)> {
    let at = toks.windows(2).position(|w| w[0].is_ident("fn") && w[1].is_ident(name))?;
    let open = (at..toks.len()).find(|&k| toks[k].is_punct('{'))?;
    Some((open, matching(toks, open)))
}

/// Literal com o comando inteiro: `add` seguido de uma forma ampla.
fn whole_command(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.windows(2).any(|w| (w[0] == "add" || w[0] == "stage") && BROAD.contains(&w[1]))
}

/// As linhas com `add` amplo fora do lugar permitido.
fn violations(file: &str, toks: &[Token]) -> Vec<usize> {
    let allowed = if file == ALLOWED.0 { fn_body(toks, ALLOWED.1) } else { None };
    let inside = |k: usize| allowed.is_some_and(|(a, b)| k > a && k < b);
    let mut out = Vec::new();
    for (k, tok) in toks.iter().enumerate() {
        let Some(text) = tok.str() else { continue };
        let broad = if text == "add" || text == "stage" {
            toks.iter()
                .skip(k + 1)
                .take(WINDOW)
                .take_while(|t| !t.is_punct(';'))
                .filter_map(Token::str)
                .any(|s| BROAD.contains(&s))
        } else {
            whole_command(text)
        };
        if broad && !inside(k) {
            out.push(tok.line);
        }
    }
    out
}

#[test]
fn only_the_stage_helper_runs_a_broad_git_add() {
    let mut found = Vec::new();
    let mut saw_helper = false;
    for (file, text) in production_sources() {
        let toks = production_tokens(&text);
        if file == ALLOWED.0 {
            saw_helper = fn_body(&toks, ALLOWED.1).is_some();
        }
        found.extend(violations(&file, &toks).into_iter().map(|line| format!("{file}:{line}")));
    }
    assert!(saw_helper, "fn {} não existe em {}", ALLOWED.1, ALLOWED.0);
    assert!(found.is_empty(), "git add amplo fora do Git::stage (use Git::add_all): {found:?}");
}

#[test]
fn the_detector_catches_each_form_and_lets_tests_and_the_helper_pass() {
    let bad = r#"
fn a() { git_ok(wt, &["add", "-A"]) }
fn b() { Command::new("git").args(["add", "--", "."]).output(); }
fn c() { run(["-c", "x=y", "add", "--all"]); }
fn d() { run(["add", "-u"]); }
fn e() { sh("git add -A && git commit"); }
fn f() { run(["stage", "."]); }
"#;
    assert_eq!(violations("sdd/x.rs", &production_tokens(bad)).len(), 6);
    let fine = r#"
fn a() { run(["add", "--", path]); run(["worktree", "add", "-B", branch]); }
const W: &[&str] = &["add", "commit", "restore"];
fn stage() { run(["add", "-A", "--", "."]); }
#[cfg(test)]
mod t { fn g() { git(&["add", "-A"]); } }
"#;
    assert!(violations("sdd/x.rs", &production_tokens(fine)).len() == 1, "só o stage fora do git.rs conta");
    assert!(violations(ALLOWED.0, &production_tokens(fine)).is_empty());
}
