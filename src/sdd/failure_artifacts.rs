//! Artefatos de falha (spec 079): arquivos de diagnóstico que a suíte grava ao falhar (ex.: o
//! `error-context.md` do Playwright), declarados em `failure_artifacts` e anexados à evidência.
//!
//! A busca é no sistema de arquivos (arquivos ignorados pelo git contam), não segue symlinks e só
//! desce em diretórios que ainda podem casar com algum glob.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::harness::tools::glob_match_path;

/// Título da seção anexada à saída da falha.
pub const SECTION: &str = "## Artefatos da falha";
/// Máximo de arquivos anexados (os mais recentes).
pub const MAX_FILES: usize = 3;
/// Orçamento total (bytes) do conteúdo dos arquivos anexados.
pub const MAX_TOTAL_BYTES: usize = 6 * 1024;
/// Bytes lidos de cada arquivo antes do scrub e do corte.
const READ_CAP_BYTES: u64 = 64 * 1024;
/// Bytes iniciais examinados em busca de NUL (sinal de binário).
const BINARY_SNIFF_BYTES: usize = 8 * 1024;
/// Teto de entradas visitadas na busca (proteção contra árvores enormes).
const MAX_VISITED: usize = 20_000;
/// Folga para relógios/sistemas de arquivos com resolução grossa de mtime.
const MTIME_SLACK: Duration = Duration::from_secs(2);
/// Diretórios nunca percorridos abaixo da raiz da busca.
const SKIP_DIRS: [&str; 2] = [".git", "node_modules"];

/// `true` se o diretório `dir` (relativo, segmentos) ainda pode conter um caminho que case com `glob`.
fn dir_may_match(glob: &str, dir: &[&str]) -> bool {
    let pat: Vec<&str> = glob.split('/').filter(|s| !s.is_empty()).collect();
    match pat.iter().position(|s| *s == "**") {
        // `**` dentro da profundidade do diretório: basta o trecho antes dele casar.
        Some(i) if i <= dir.len() => glob_match_path(&pat[..i].join("/"), &dir[..i].join("/")),
        // Sem `**` alcançável: o padrão precisa ser mais fundo e casar segmento a segmento.
        _ => pat.len() > dir.len() && glob_match_path(&pat[..dir.len()].join("/"), &dir.join("/")),
    }
}

/// Arquivo candidato: caminho relativo (segmentos separados por `/`) e mtime.
pub(crate) struct Found {
    pub(crate) rel: String,
    pub(crate) modified: SystemTime,
}

fn walk(root: &Path, rel: &mut Vec<String>, globs: &[String], since: SystemTime, visited: &mut usize, out: &mut Vec<Found>) {
    let dir = rel.iter().fold(root.to_path_buf(), |p, s| p.join(s));
    let Ok(entries) = fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        *visited += 1;
        if *visited > MAX_VISITED {
            return;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        // `symlink_metadata` não segue o link: symlinks são ignorados (nunca saem da worktree).
        let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        rel.push(name);
        if meta.is_dir() {
            let segs: Vec<&str> = rel.iter().map(String::as_str).collect();
            if !SKIP_DIRS.contains(&segs[segs.len() - 1]) && globs.iter().any(|g| dir_may_match(g, &segs)) {
                walk(root, rel, globs, since, visited, out);
            }
        } else if meta.is_file() {
            let path = rel.join("/");
            if globs.iter().any(|g| glob_match_path(g, &path)) {
                if let Ok(modified) = meta.modified() {
                    if modified + MTIME_SLACK >= since {
                        out.push(Found { rel: path, modified });
                    }
                }
            }
        }
        rel.pop();
    }
}

/// Arquivos de `root` que casam com `globs`, modificados a partir de `since` (com a folga de mtime),
/// do mais recente ao mais antigo (empate: caminho). Sem seguir symlinks e sem sair da raiz; também
/// usada pela coleta de capturas de tela (spec 082).
pub(crate) fn find(root: &Path, globs: &[String], since: SystemTime) -> Vec<Found> {
    let mut found = Vec::new();
    if globs.is_empty() {
        return found;
    }
    let mut visited = 0;
    walk(root, &mut Vec::new(), globs, since, &mut visited, &mut found);
    found.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.rel.cmp(&b.rel)));
    found
}

/// Lê até [`READ_CAP_BYTES`] do arquivo, só se for texto (spec 081 T02): `None` para binário (NUL nos
/// primeiros [`BINARY_SNIFF_BYTES`]) ou UTF-8 inválido. Um caractere multibyte cortado pelo teto de
/// leitura não conta como inválido (o resto fica de fora).
fn read_capped(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(READ_CAP_BYTES).read_to_end(&mut buf).ok()?;
    if buf[..buf.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return None;
    }
    match String::from_utf8(buf) {
        Ok(text) => Some(text),
        Err(err) => {
            let utf8 = err.utf8_error();
            let capped = err.as_bytes().len() as u64 == READ_CAP_BYTES;
            if capped && utf8.error_len().is_none() {
                let valid = utf8.valid_up_to();
                let mut bytes = err.into_bytes();
                bytes.truncate(valid);
                String::from_utf8(bytes).ok()
            } else {
                None
            }
        }
    }
}

fn cut(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Seção "## Artefatos da falha" com os arquivos de `wt` que casam com `globs` e foram modificados a
/// partir de `since`, só de texto (binários e não UTF-8 são pulados): até [`MAX_FILES`] (os mais recentes) e [`MAX_TOTAL_BYTES`] de conteúdo no total,
/// cada um com scrub (antes do corte) e truncado. Vazio quando não há globs ou nada casou.
pub fn collect(wt: &Path, globs: &[String], since: SystemTime, scrub: &dyn Fn(&str) -> String) -> String {
    if globs.is_empty() {
        return String::new();
    }
    let found = find(wt, globs, since);
    // Binários e não UTF-8 saem antes do corte: não ocupam vaga nem orçamento.
    let parts: Vec<(String, String)> = found
        .iter()
        .filter_map(|f| read_capped(&wt.join(&f.rel)).map(|t| (f.rel.clone(), scrub(&t))))
        .take(MAX_FILES)
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    let share = MAX_TOTAL_BYTES / parts.len();
    let mut out = format!("\n\n{SECTION}\n");
    for (rel, text) in &parts {
        let (body, truncated) = cut(text.trim_end(), share);
        out.push_str(&format!("\n### {}\n{body}\n", scrub(rel)));
        if truncated {
            out.push_str("[… truncado]\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-fa-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().expect("pai")).expect("mkdir");
        fs::write(p, content).expect("write");
    }

    fn set_age(root: &Path, rel: &str, age: Duration) {
        let f = fs::OpenOptions::new().write(true).open(root.join(rel)).expect("open");
        f.set_modified(SystemTime::now() - age).expect("mtime");
    }

    fn globs(g: &[&str]) -> Vec<String> {
        g.iter().map(|s| s.to_string()).collect()
    }

    fn id(t: &str) -> String {
        t.to_string()
    }

    #[test]
    fn matches_and_formats_section() {
        let root = tmp("match");
        let since = SystemTime::now() - Duration::from_secs(60);
        write(&root, "test-results/login-chromium/error-context.md", "TypeError: account.accountNumber is undefined");
        write(&root, "test-results/other.txt", "não casa");
        write(&root, "src/error-context.md", "fora do glob");
        let out = collect(&root, &globs(&["test-results/**/error-context.md"]), since, &id);
        assert!(out.contains(SECTION), "{out}");
        assert!(out.contains("### test-results/login-chromium/error-context.md"), "{out}");
        assert!(out.contains("accountNumber is undefined"), "{out}");
        assert!(!out.contains("não casa") && !out.contains("fora do glob"), "{out}");
    }

    #[test]
    fn ignores_files_older_than_the_run() {
        let root = tmp("old");
        write(&root, "test-results/a/error-context.md", "antigo");
        set_age(&root, "test-results/a/error-context.md", Duration::from_secs(3600));
        let since = SystemTime::now() - Duration::from_secs(60);
        assert_eq!(collect(&root, &globs(&["test-results/**/error-context.md"]), since, &id), "");
        write(&root, "test-results/b/error-context.md", "novo");
        let out = collect(&root, &globs(&["test-results/**/error-context.md"]), since, &id);
        assert!(out.contains("novo") && !out.contains("antigo"), "{out}");
    }

    #[test]
    fn respects_file_and_byte_limits_keeping_most_recent() {
        let root = tmp("limits");
        let since = SystemTime::now() - Duration::from_secs(600);
        for (i, age) in [(1, 50), (2, 40), (3, 30), (4, 20), (5, 10)] {
            let rel = format!("r/t{i}/error-context.md");
            write(&root, &rel, &format!("ARQ{i}\n{}", "x".repeat(10_000)));
            set_age(&root, &rel, Duration::from_secs(age));
        }
        let out = collect(&root, &globs(&["r/**/error-context.md"]), since, &id);
        assert_eq!(out.matches("### ").count(), MAX_FILES, "{out}");
        assert!(out.contains("ARQ5") && out.contains("ARQ4") && out.contains("ARQ3"), "mais recentes");
        assert!(!out.contains("ARQ1") && !out.contains("ARQ2"));
        assert_eq!(out.matches("[… truncado]").count(), MAX_FILES);
        let content: usize = out.matches('x').count();
        assert!(content <= MAX_TOTAL_BYTES, "{content}");
        assert!(out.len() < MAX_TOTAL_BYTES + 400, "{}", out.len());
    }

    #[test]
    fn scrubs_secrets_before_cutting() {
        let root = tmp("scrub");
        let since = SystemTime::now() - Duration::from_secs(60);
        write(&root, "test-results/e/error-context.md", "DATABASE_URL=postgres://u:SEGREDO123@db/x");
        let scrub = |t: &str| t.replace("SEGREDO123", "[redacted]");
        let out = collect(&root, &globs(&["test-results/**/*.md"]), since, &scrub);
        assert!(!out.contains("SEGREDO123") && out.contains("[redacted]"), "{out}");
    }

    #[test]
    fn skips_binary_and_non_utf8_files_without_taking_a_slot_or_budget() {
        // Spec 081 T02: `a.png` binário mais novo não tira o lugar nem o orçamento do `error-context.md`.
        let root = tmp("binary");
        let since = SystemTime::now() - Duration::from_secs(600);
        write(&root, "test-results/t/error-context.md", &format!("CTX\n{}", "y".repeat(MAX_TOTAL_BYTES)));
        set_age(&root, "test-results/t/error-context.md", Duration::from_secs(30));
        let png = root.join("test-results/t/a.png");
        fs::write(&png, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").expect("png");
        let zip = root.join("test-results/t/trace.zip");
        fs::write(&zip, [0x50u8, 0x4b, 0x03, 0x04, 0xff, 0xfe, 0xc3, 0x28]).expect("zip");
        let out = collect(&root, &globs(&["test-results/**/*"]), since, &id);
        assert_eq!(out.matches("### ").count(), 1, "{out}");
        assert!(out.contains("### test-results/t/error-context.md"), "{out}");
        assert!(!out.contains("a.png") && !out.contains("trace.zip"), "{out}");
        // Orçamento inteiro para o único arquivo de texto.
        assert_eq!(out.matches('y').count(), MAX_TOTAL_BYTES - "CTX\n".len(), "{out}");
    }

    #[test]
    fn read_capped_rejects_nul_and_invalid_utf8_but_accepts_cut_multibyte() {
        let root = tmp("read-capped");
        let nul = root.join("nul.txt");
        fs::write(&nul, b"texto\0resto").expect("nul");
        assert_eq!(read_capped(&nul), None);
        let bad = root.join("bad.txt");
        fs::write(&bad, [b'o', b'k', 0xc3, 0x28]).expect("bad");
        assert_eq!(read_capped(&bad), None);
        let ok = root.join("ok.md");
        fs::write(&ok, "ação ✓").expect("ok");
        assert_eq!(read_capped(&ok).as_deref(), Some("ação ✓"));
        // Caractere de 2 bytes cortado pelo teto de leitura: o prefixo válido fica.
        let big = root.join("big.md");
        let mut bytes = vec![b'a'; READ_CAP_BYTES as usize - 1];
        bytes.extend_from_slice("ç".as_bytes());
        fs::write(&big, &bytes).expect("big");
        let text = read_capped(&big).expect("texto");
        assert_eq!(text.len(), READ_CAP_BYTES as usize - 1);
    }

    #[test]
    fn empty_globs_or_nothing_found_yields_nothing() {
        let root = tmp("empty");
        let since = SystemTime::now() - Duration::from_secs(60);
        write(&root, "test-results/e/error-context.md", "x");
        assert_eq!(collect(&root, &[], since, &id), "");
        assert_eq!(collect(&root, &globs(&["nada/**/*.md"]), since, &id), "");
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinks_out_of_the_worktree() {
        let root = tmp("symlink");
        let outside = tmp("symlink-outside");
        write(&outside, "error-context.md", "VAZOU");
        fs::create_dir_all(root.join("test-results")).expect("mkdir");
        std::os::unix::fs::symlink(&outside, root.join("test-results/link")).expect("symlink dir");
        std::os::unix::fs::symlink(outside.join("error-context.md"), root.join("test-results/error-context.md"))
            .expect("symlink file");
        let since = SystemTime::now() - Duration::from_secs(60);
        assert_eq!(collect(&root, &globs(&["test-results/**/error-context.md"]), since, &id), "");
    }

    #[test]
    fn descends_only_where_a_glob_can_match() {
        assert!(dir_may_match("test-results/**/x.md", &["test-results"]));
        assert!(dir_may_match("test-results/**/x.md", &["test-results", "a", "b"]));
        assert!(!dir_may_match("test-results/**/x.md", &["src"]));
        assert!(dir_may_match("**/x.md", &["src", "deep"]));
        assert!(dir_may_match("reports/*/x.md", &["reports", "a"]));
        assert!(!dir_may_match("reports/*/x.md", &["reports", "a", "b"]));
        assert!(!dir_may_match("reports/x.md", &["reports", "x.md"]));
    }
}
