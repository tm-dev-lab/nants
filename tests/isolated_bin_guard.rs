//! Trava (spec 138 T13): nenhum teste de integração sobe o Nants sobre a pasta de dados real.
//!
//! - O binário só é citado em `tests/support/isolated_bin.rs`, que dá a todo filho `HOME`,
//!   `XDG_DATA_HOME` e `XDG_CONFIG_HOME` temporários. Qualquer outro arquivo de `tests/` que cite o
//!   binário (`serve`, sem argumentos ou outro subcomando) falha aqui.
//! - O servidor em processo (`AppState::new`) usado por um teste de integração passa por
//!   `.with_data_dir(..)` no mesmo comando: fora do `cfg(test)` da biblioteca, ele usaria a pasta real.

use std::path::{Path, PathBuf};

const HELPER: &str = "isolated_bin.rs";

/// Nome da variável do binário, montado em tempo de execução para este arquivo não se acusar.
fn bin_var() -> String {
    ["CARGO", "BIN", "EXE", "nants"].join("_")
}

/// Remove o trecho de comentário `//` de cada linha (o suficiente para estes arquivos de teste).
fn code_only(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Linhas (1-based) que citam o binário diretamente.
fn direct_bin_uses(src: &str) -> Vec<usize> {
    let needle = bin_var();
    code_only(src)
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(&needle))
        .map(|(i, _)| i + 1)
        .collect()
}

/// Linhas (1-based) com `AppState::new(` cujo comando (até o `;`) não passa por `.with_data_dir(`.
fn app_state_without_data_dir(src: &str) -> Vec<usize> {
    let code = code_only(src);
    let needle = ["AppState", "::new("].concat();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(pos) = code[from..].find(&needle) {
        let at = from + pos;
        let rest = &code[at..];
        let stmt = &rest[..rest.find(';').unwrap_or(rest.len())];
        if !stmt.contains(".with_data_dir(") {
            out.push(code[..at].matches('\n').count() + 1);
        }
        from = at + needle.len();
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "fixtures") {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn binary_and_in_process_server_never_use_the_real_data_dir() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(files.len() > 5, "varredura sem arquivos: {files:?}");
    let mut problems = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file).expect("lê o teste");
        let name = file.strip_prefix(&root).unwrap_or(file).display().to_string();
        if file.file_name().is_some_and(|n| n != HELPER) {
            for line in direct_bin_uses(&src) {
                problems.push(format!("{name}:{line}: sobe o binário sem `isolated_bin::nants`"));
            }
        }
        for line in app_state_without_data_dir(&src) {
            problems.push(format!("{name}:{line}: `AppState::new` sem `.with_data_dir(..)` temporário"));
        }
    }
    assert!(problems.is_empty(), "testes fora do isolamento da pasta de dados:\n{}", problems.join("\n"));
}

#[test]
fn the_scan_catches_direct_calls_and_ignores_comments() {
    let var = bin_var();
    let bad = format!("fn t() {{\n    let c = Command::new(env!(\"{var}\")).arg(\"serve\");\n}}\n");
    assert_eq!(direct_bin_uses(&bad), vec![2]);
    let bare = format!("let out = Command::new(env!(\"{var}\")).output();\n");
    assert_eq!(direct_bin_uses(&bare), vec![1]);
    let comment = format!("//! sobe `{var} serve`\nlet c = isolated_bin::nants(&dir);\n");
    assert!(direct_bin_uses(&comment).is_empty());

    let state = ["AppState", "::new(p, c, 0)"].concat();
    assert_eq!(app_state_without_data_dir(&format!("let a = 1;\nlet s = {state};\n")), vec![2]);
    assert!(app_state_without_data_dir(&format!("let s = {state}\n    .with_data_dir(d);\n")).is_empty());
}
