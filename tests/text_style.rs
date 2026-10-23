//! Spec 095 T08 (decisões 11 e 12): os textos que o próprio Nants escreve (mensagens do servidor,
//! relatório, rastreabilidade, avisos, prompts dos agentes) não têm travessão, meia-risca nem
//! emoji, não falam em "YOLO" (agora "aprovação automática") e não mandam usar o botão antigo
//! "Executar para testar" (agora "Iniciar o app").
//!
//! A varredura lê as strings literais de todo o código de produção de `src/` (comentários, char
//! literais e itens `#[cfg(test)]` ficam de fora; arquivos `*_tests.rs` e `testdata/` também). As
//! exceções ficam em `EXEMPT`, cada uma com o motivo.

use std::fs;
use std::path::{Path, PathBuf};

/// Literais isentos: arquivo (relativo a `src/`), o literal exato e o motivo.
const EXEMPT: &[(&str, &str, &str)] = &[
    (
        "agents/runner.rs",
        "verificação humana — não bloqueia",
        "marca antiga, só reconhecida nos critérios já gravados nas tarefas (não é escrita de novo)",
    ),
    (
        "server/recovery.rs",
        "- Rodada autônoma (YOLO) #",
        "prefixo antigo, só contado nas tarefas já gravadas (não é escrito de novo)",
    ),
    (
        "server/recovery.rs",
        "- Correção de ambiente (YOLO) #",
        "prefixo antigo, só contado nas tarefas já gravadas (não é escrito de novo)",
    ),
];

/// Emoji: aproximação de `\p{Extended_Pictographic}` (a mesma classe do `EMOJI` da interface),
/// sem as marcas de critério (U+2713, U+2717) e as setas simples, que não são pictográficas.
fn is_emoji(c: char) -> bool {
    matches!(
        c as u32,
        0xA9 | 0xAE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x2199
            | 0x21A9..=0x21AA
            | 0x231A..=0x231B
            | 0x2328
            | 0x23CF
            | 0x23E9..=0x23F3
            | 0x23F8..=0x23FA
            | 0x24C2
            | 0x25AA..=0x25AB
            | 0x25B6
            | 0x25C0
            | 0x25FB..=0x25FE
            | 0x2600..=0x26FF
            | 0x2702
            | 0x2705
            | 0x2708..=0x270D
            | 0x270F
            | 0x2712
            | 0x2714
            | 0x2716
            | 0x271D
            | 0x2721
            | 0x2728
            | 0x2733..=0x2734
            | 0x2744
            | 0x2747
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2763..=0x2764
            | 0x2795..=0x2797
            | 0x27A1
            | 0x27B0
            | 0x27BF
            | 0x2934..=0x2935
            | 0x2B05..=0x2B07
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1FAFF
            | 0x1FC00..=0x1FFFD
    )
}

/// Primeiro problema de estilo no texto, ou `None`.
fn style_problem(text: &str) -> Option<String> {
    for c in text.chars() {
        if c == '\u{2014}' {
            return Some("travessão".into());
        }
        if c == '\u{2013}' {
            return Some("meia-risca".into());
        }
        if is_emoji(c) {
            return Some(format!("emoji {c}"));
        }
    }
    for escape in ["\\u{2014}", "\\u{2013}"] {
        if text.contains(escape) {
            return Some(format!("escape {escape}"));
        }
    }
    if without_placeholders(text).contains("YOLO") {
        return Some("\"YOLO\" (use \"aprovação automática\")".into());
    }
    if text.contains("Executar para testar") {
        return Some("\"Executar para testar\" (use \"Iniciar o app\")".into());
    }
    None
}

/// Texto sem os nomes de variável de `format!` (`{YOLO_AUTO_PREFIX}`), que são identificadores.
fn without_placeholders(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len = after.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(after.len());
        let is_ident = name_len > 0 && after[name_len..].starts_with(['}', ':']);
        match after.find('}') {
            Some(close) if is_ident => rest = &after[close + 1..],
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Strings literais do código de produção: `(linha, conteúdo)`. Ignora comentários, char literais
/// e o item que segue cada `#[cfg(test)]` (até o `;` ou o `}` que fecha o bloco).
fn production_strings(source: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut line = 1;
    let mut i = 0;
    // Item `#[cfg(test)]` em curso: `Some(0)` antes da primeira chave; `Some(n)`, n chaves abertas.
    let mut test_item: Option<usize> = None;
    let starts_with = |at: usize, pat: &str| pat.chars().enumerate().all(|(k, p)| chars.get(at + k) == Some(&p));
    let is_ident = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric() || *c == '_');
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if starts_with(i, "//") {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if starts_with(i, "/*") {
            i += 2;
            while i < chars.len() && !starts_with(i, "*/") {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        if test_item.is_none() && starts_with(i, "#[cfg(test)]") {
            test_item = Some(0);
            i += "#[cfg(test)]".len();
            continue;
        }
        // String crua: r"..." / r#"..."# (também br"...").
        if c == 'r' && !is_ident(i.checked_sub(1).and_then(|p| chars.get(p))) || (c == 'b' && chars.get(i + 1) == Some(&'r')) {
            let mut j = if c == 'b' { i + 2 } else { i + 1 };
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                let start_line = line;
                let mut text = String::new();
                j += 1;
                let close: String = std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
                while j < chars.len() && !starts_with(j, &close) {
                    if chars[j] == '\n' {
                        line += 1;
                    }
                    text.push(chars[j]);
                    j += 1;
                }
                if test_item.is_none() {
                    out.push((start_line, text));
                }
                i = j + close.chars().count();
                continue;
            }
        }
        if c == '"' {
            let start_line = line;
            let mut text = String::new();
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '\\' {
                    text.push('\\');
                    j += 1;
                }
                if let Some(&ch) = chars.get(j) {
                    if ch == '\n' {
                        line += 1;
                    }
                    text.push(ch);
                }
                j += 1;
            }
            if test_item.is_none() {
                out.push((start_line, text));
            }
            i = j + 1;
            continue;
        }
        if c == '\'' {
            // Char literal ('x', '\n', '\u{2014}') ou lifetime ('a).
            if chars.get(i + 1) == Some(&'\\') {
                let mut j = i + 2;
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                i = j + 1;
            } else if chars.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(depth) = test_item {
            match c {
                '{' => test_item = Some(depth + 1),
                '}' if depth <= 1 => test_item = None,
                '}' => test_item = Some(depth - 1),
                ';' if depth == 0 => test_item = None,
                _ => {}
            }
        }
        i += 1;
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "testdata") {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_test_file(rel: &str) -> bool {
    rel.ends_with("_tests.rs") || rel.ends_with("/tests.rs")
}

#[test]
fn lexer_skips_comments_char_literals_and_test_items() {
    let source = "// travessão \u{2014} em comentário\nconst A: &str = \"ok: x\";\nfn f() { let c = '\u{2014}'; let l: &'static str = r#\"cru \"aspas\"\"#; }\n\
        #[cfg(test)]\nmod tests {\n    fn g() { let s = \"teste \u{2014} isento\"; { \"aninhado\"; } }\n}\nconst B: &str = \"depois\";\n";
    let strings: Vec<String> = production_strings(source).into_iter().map(|(_, s)| s).collect();
    assert_eq!(strings, vec!["ok: x".to_string(), "cru \"aspas\"".to_string(), "depois".to_string()]);
    assert_eq!(production_strings("#[cfg(test)]\nuse x::y;\nconst C: &str = \"c\";").len(), 1);
}

#[test]
fn style_problem_catches_dashes_emoji_yolo_and_old_button() {
    for bad in ["a \u{2014} b", "1\u{2013}2", "pronto \u{1F680}", "ok \u{2705}", "modo YOLO", "use Executar para testar", "\\u{2014}"] {
        assert!(style_problem(bad).is_some(), "{bad}");
    }
    for good in ["G1: critério (T01, commit abc)", "\u{2713} feito", "\u{2717} falhou", "a \u{2192} b", "e-mail", "yolo: true", "{YOLO_AUTO_PREFIX}: {x}"] {
        assert_eq!(style_problem(good), None, "{good}");
    }
}

#[test]
fn nants_texts_have_no_dashes_emoji_yolo_or_old_button_name() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    assert!(files.len() > 50, "varredura sem arquivos: {}", files.len());
    for (_, _, reason) in EXEMPT {
        assert!(reason.len() > 20, "exceção sem motivo");
    }
    let mut used = vec![false; EXEMPT.len()];
    let mut problems = Vec::new();
    let mut scanned: Vec<(String, String)> = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&src).unwrap_or(path).to_string_lossy().replace('\\', "/");
        if is_test_file(&rel) {
            continue;
        }
        let Ok(source) = fs::read_to_string(path) else { continue };
        for (line, text) in production_strings(&source) {
            scanned.push((rel.clone(), text.clone()));
            let Some(problem) = style_problem(&text) else { continue };
            if let Some(k) = EXEMPT.iter().position(|(f, lit, _)| *f == rel && *lit == text) {
                used[k] = true;
                continue;
            }
            problems.push(format!("src/{rel}:{line}: {problem}: {:?}", text.chars().take(120).collect::<String>()));
        }
    }
    // A varredura chega aos textos que importam (nada de produção ficou de fora por engano).
    for (file, marker) in [
        // Spec 115 T03: o aviso saiu de `approvals.rs` para o dicionário (`system.run.yolo_ignored`).
        ("i18n/pt_br.rs", "Aprovação automática ignorada"),
        ("server/approvals.rs", "aprovada automaticamente"),
        ("server/recovery.rs", "## Aprovação automática: autonomia"),
        ("server/report.rs", "Executar fora do Nants"),
        // Spec 102: os dois dicionários do servidor e do terminal entram na varredura.
        ("i18n/pt_br.rs", "sem tarefa declarada"),
        ("i18n/pt_br.rs", "diagnóstico do ambiente"),
        ("i18n/en.rs", "no task declared"),
        ("i18n/en.rs", "environment check"),
        ("agents/runner.rs", "verificação humana, não bloqueia"),
        ("agents/prompts.rs", "ESTILO DA ESCRITA"),
        // Spec 113 T04: o nome do botão vem do dicionário (idioma do relatório).
        ("agents/prompts.rs", "START_APP_LABEL"),
        ("i18n/pt_br.rs", "Iniciar o app"),
        ("i18n/en.rs", "Start the app"),
        ("server/judge.rs", "juiz de segurança da aprovação automática"),
        ("harness/tools/generate_image.rs", "generate_image"),
    ] {
        assert!(scanned.iter().any(|(f, t)| f == file && t.contains(marker)), "a varredura não viu {marker:?} em {file}");
    }
    assert!(problems.is_empty(), "textos do Nants fora do estilo:\n{}", problems.join("\n"));
    let stale: Vec<_> = EXEMPT.iter().zip(&used).filter(|(_, u)| !**u).map(|(e, _)| e.1).collect();
    assert!(stale.is_empty(), "exceções que não são mais usadas: {stale:?}");
}

/// Spec 102: o dicionário en não carrega texto em português (acentos do pt-BR denunciam sobra).
#[test]
fn english_dictionary_has_no_portuguese_leftovers() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/i18n/en.rs");
    let source = fs::read_to_string(&path).expect("en.rs");
    let strings = production_strings(&source);
    assert!(strings.len() > 50, "dicionário en sem textos: {}", strings.len());
    let accents = ['ã', 'õ', 'ç', 'á', 'é', 'í', 'ó', 'ú', 'â', 'ê', 'ô', 'à'];
    for (line, text) in strings {
        assert!(!text.chars().flat_map(char::to_lowercase).any(|c| accents.contains(&c)), "src/i18n/en.rs:{line}: {text}");
        assert_eq!(style_problem(&text), None, "src/i18n/en.rs:{line}: {text}");
    }
}
