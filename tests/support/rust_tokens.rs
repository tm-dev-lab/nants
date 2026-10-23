//! Lexer mínimo do Rust para as varreduras de idioma (spec 115 T01): identificadores, strings
//! literais (com os escapes resolvidos), pontuação e a linha de cada um. Comentários, char
//! literais, lifetimes e números ficam de fora; os itens `#[cfg(test)]` (e `mod tests { }`) também.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Ident(String),
    Str(String),
    Punct(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
}

impl Token {
    pub fn is_ident(&self, name: &str) -> bool {
        matches!(&self.tok, Tok::Ident(i) if i == name)
    }

    pub fn is_punct(&self, c: char) -> bool {
        self.tok == Tok::Punct(c)
    }

    pub fn ident(&self) -> Option<&str> {
        match &self.tok {
            Tok::Ident(i) => Some(i),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match &self.tok {
            Tok::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Resolve os escapes de uma string comum (`\n`, `\"`, `\u{..}`, quebra de linha com `\`).
fn unescape(raw: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i];
        if c != '\\' {
            out.push(c);
            i += 1;
            continue;
        }
        let Some(&next) = raw.get(i + 1) else { break };
        i += 2;
        match next {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            '\n' => {
                while raw.get(i).is_some_and(|c| c.is_whitespace()) {
                    i += 1;
                }
            }
            'u' if raw.get(i) == Some(&'{') => {
                let close = raw[i..].iter().position(|c| *c == '}').map_or(raw.len(), |p| i + p);
                let hex: String = raw[i + 1..close.min(raw.len())].iter().collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
                i = close + 1;
            }
            'x' => {
                let hex: String = raw[i..(i + 2).min(raw.len())].iter().collect();
                if let Some(ch) = u8::from_str_radix(&hex, 16).ok().map(char::from) {
                    out.push(ch);
                }
                i += 2;
            }
            other => out.push(other),
        }
    }
    out
}

/// Todos os tokens do arquivo, sem comentários (inclusive os de teste).
pub fn tokens(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut line = 1;
    let mut i = 0;
    let starts_with = |at: usize, pat: &str| pat.chars().enumerate().all(|(k, p)| chars.get(at + k) == Some(&p));
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';
    let count_lines = |from: usize, to: usize| chars[from..to.min(chars.len())].iter().filter(|c| **c == '\n').count();
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
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
            let mut depth = 0;
            let start = i;
            while i < chars.len() {
                if starts_with(i, "/*") {
                    depth += 1;
                    i += 2;
                } else if starts_with(i, "*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            line += count_lines(start, i);
            continue;
        }
        // String crua: r"..", r#".."#, br"..".
        let raw_start = if c == 'r' { Some(i + 1) } else if c == 'b' && chars.get(i + 1) == Some(&'r') { Some(i + 2) } else { None };
        if let Some(mut j) = raw_start {
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                let close: String = std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
                let body_start = j + 1;
                let mut k = body_start;
                while k < chars.len() && !starts_with(k, &close) {
                    k += 1;
                }
                let text: String = chars[body_start..k.min(chars.len())].iter().collect();
                out.push(Token { tok: Tok::Str(text), line });
                line += count_lines(i, k);
                i = k + close.chars().count();
                continue;
            }
        }
        // String comum (e b"..").
        let quote_at = if c == '"' { Some(i) } else if c == 'b' && chars.get(i + 1) == Some(&'"') { Some(i + 1) } else { None };
        if let Some(q) = quote_at {
            let mut k = q + 1;
            while k < chars.len() && chars[k] != '"' {
                if chars[k] == '\\' {
                    k += 1;
                }
                k += 1;
            }
            let text = unescape(&chars[q + 1..k.min(chars.len())]);
            out.push(Token { tok: Tok::Str(text), line });
            line += count_lines(i, k);
            i = k + 1;
            continue;
        }
        if c == '\'' {
            // Char literal ('x', '\n', '\u{2014}') ou lifetime ('a).
            if chars.get(i + 1) == Some(&'\\') {
                let mut k = i + 2;
                while k < chars.len() && chars[k] != '\'' {
                    k += 1;
                }
                i = k + 1;
            } else if chars.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
                while i < chars.len() && is_ident_char(chars[i]) {
                    i += 1;
                }
            }
            continue;
        }
        if c.is_ascii_digit() {
            while i < chars.len() && (is_ident_char(chars[i]) || (chars[i] == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit))) {
                i += 1;
            }
            continue;
        }
        if is_ident_char(c) {
            let start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            out.push(Token { tok: Tok::Ident(chars[start..i].iter().collect()), line });
            continue;
        }
        out.push(Token { tok: Tok::Punct(c), line });
        i += 1;
    }
    out
}

/// Índice do token que fecha o grupo aberto em `open` (`(`, `[` ou `{`), ou o fim.
pub fn matching(toks: &[Token], open: usize) -> usize {
    let mut depth = 0i32;
    for (k, t) in toks.iter().enumerate().skip(open) {
        match &t.tok {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return k;
                }
            }
            _ => {}
        }
    }
    toks.len()
}

/// Fim (inclusive) do que o atributo anota, a partir de `start`: um item (`fn`, `mod`, `impl`, que
/// acabam no `}` do bloco), uma instrução (até o `;`) ou um campo de struct, na declaração ou no
/// literal (até a `,` do mesmo nível, ou logo antes do `)`/`]`/`}` que fecha o grupo, sem consumi-lo).
fn item_end(toks: &[Token], start: usize) -> usize {
    let mut k = start;
    while k < toks.len() {
        match &toks[k].tok {
            Tok::Punct(';' | ',') => return k,
            Tok::Punct(')' | ']' | '}') => return k.saturating_sub(1),
            Tok::Punct('{') => {
                let close = matching(toks, k);
                // Bloco seguido de `,`, `;`, `.`, `?` ou de um fechamento: era expressão (campo
                // `a: S { .. },`, `let x = S { .. };`); senão o bloco fecha o item.
                let continues = toks.get(close + 1).is_some_and(|t| matches!(t.tok, Tok::Punct(',' | ';' | '.' | '?' | ')' | ']' | '}')));
                if !continues {
                    return close;
                }
                k = close + 1;
            }
            Tok::Punct('(' | '[') => k = matching(toks, k) + 1,
            _ => k += 1,
        }
    }
    toks.len()
}

/// Atributo `#[cfg(...)]` que liga só nos testes (`cfg(test)`, `cfg(all(test, ..))`), sem `not`.
fn is_test_cfg(attr: &[Token]) -> bool {
    attr.first().is_some_and(|t| t.is_ident("cfg")) && attr.iter().any(|t| t.is_ident("test")) && !attr.iter().any(|t| t.is_ident("not"))
}

/// Tokens do código de produção: sem os itens `#[cfg(test)]` e sem os `mod tests { }`.
pub fn production_tokens(source: &str) -> Vec<Token> {
    let toks = tokens(source);
    let mut out = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        if toks[i].is_punct('#') && toks.get(i + 1).is_some_and(|t| t.is_punct('[')) {
            let close = matching(&toks, i + 1);
            if is_test_cfg(&toks[(i + 2).min(close)..close]) {
                i = item_end(&toks, close + 1) + 1;
                continue;
            }
        }
        if toks[i].is_ident("mod") && toks.get(i + 1).is_some_and(|t| t.is_ident("tests")) && toks.get(i + 2).is_some_and(|t| t.is_punct('{')) {
            i = matching(&toks, i + 2) + 1;
            continue;
        }
        out.push(toks[i].clone());
        i += 1;
    }
    out
}

/// Arquivos `.rs` de `dir` (recursivo, sem `testdata/`), em ordem.
pub fn rust_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "testdata") {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// Arquivo só de testes (`foo_tests.rs`, `tests.rs`), incluído por um `#[cfg(test)] mod`.
pub fn is_test_file(rel: &str) -> bool {
    rel.ends_with("_tests.rs") || rel.ends_with("/tests.rs") || rel == "tests.rs"
}

/// `src/` do projeto e os arquivos de produção, com o caminho relativo a `src/`.
pub fn production_sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    rust_files(&src)
        .into_iter()
        .filter_map(|path| {
            let rel = path.strip_prefix(&src).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if is_test_file(&rel) {
                return None;
            }
            fs::read_to_string(&path).ok().map(|text| (rel, text))
        })
        .collect()
}

/// Constantes de texto do arquivo: `const NOME: &str = "texto";` (também `pub`, `&'static str`).
pub fn str_consts(toks: &[Token]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for k in 0..toks.len() {
        if !toks[k].is_ident("const") && !toks[k].is_ident("static") {
            continue;
        }
        let Some(name) = toks.get(k + 1).and_then(Token::ident) else { continue };
        if !toks.get(k + 2).is_some_and(|t| t.is_punct(':')) {
            continue;
        }
        let Some(eq) = toks[k..].iter().take(8).position(|t| t.is_punct('=')).map(|p| k + p) else { continue };
        let ty_is_str = toks[k + 3..eq].iter().any(|t| t.is_ident("str")) && !toks[k + 3..eq].iter().any(|t| t.is_punct('['));
        if !ty_is_str {
            continue;
        }
        if let (Some(text), true) = (toks.get(eq + 1).and_then(Token::str), toks.get(eq + 2).is_some_and(|t| t.is_punct(';'))) {
            out.push((name.to_string(), text.to_string()));
        }
    }
    out
}

#[test]
fn lexer_reads_strings_and_skips_comments_chars_and_test_items() {
    let source = "// \"comentário\"\nconst A: &str = \"ok: \\\"x\\\"\";\nfn f() { let c = '\"'; let l: &'static str = r#\"cru \"aspas\"\"#; }\n\
        #[cfg(test)]\nmod t {\n    fn g() { let s = \"de teste\"; { \"aninhado\"; } }\n}\n#[cfg(test)]\nuse x::y;\nmod tests { const T: &str = \"t\"; }\n\
        /* bloco /* aninhado */ \"fora\" */ const B: &str = \"linha \\\n    continua\";\n";
    let toks = production_tokens(source);
    let strings: Vec<&str> = toks.iter().filter_map(Token::str).collect();
    assert_eq!(strings, vec!["ok: \"x\"", "cru \"aspas\"", "linha continua"]);
    assert_eq!(toks.iter().find(|t| t.str() == Some("linha continua")).map(|t| t.line), Some(11));
    assert_eq!(str_consts(&toks), vec![("A".to_string(), "ok: \"x\"".to_string()), ("B".to_string(), "linha continua".to_string())]);
}

/// O `#[cfg(test)]` de um campo remove só o campo: o item seguinte continua na varredura.
#[test]
fn test_only_fields_do_not_swallow_the_next_item() {
    let source = "struct X {\n    a: u8,\n    #[cfg(test)]\n    gates: Arc<Mutex<Vec<u8>>>,\n}\n\
        impl X { fn f() { ApiError::new(S, \"texto de produção\"); } }\n\
        fn g() { let v = X { a: 1, #[cfg(test)] gates: f(1, 2), b: \"depois do campo\" }; }\n\
        struct Y {\n    #[cfg(test)]\n    z: bool\n}\nconst C: &str = \"fim\";\n\
        #[cfg(test)]\nfn h() { \"some\" }\nconst D: &str = \"fica\";\n";
    let toks = production_tokens(source);
    let strings: Vec<&str> = toks.iter().filter_map(Token::str).collect();
    assert_eq!(strings, vec!["texto de produção", "depois do campo", "fim", "fica"]);
    assert!(!toks.iter().any(|t| t.is_ident("gates") || t.is_ident("z") || t.is_ident("h")));
    assert!(toks.iter().any(|t| t.is_ident("impl")) && toks.iter().any(|t| t.is_ident("Y")));
}

/// Trava com o código real: o `impl AppState` vem depois de um campo `#[cfg(test)]`, e a
/// `struct Fetched` depois dos campos `#[cfg(test)]` do `WebClient`.
#[test]
fn real_sources_keep_items_after_test_only_fields() {
    let read = |rel: &str| fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel)).expect(rel);
    let state = production_tokens(&read("server/state.rs"));
    assert!(state.iter().any(|t| t.is_ident("begin_architect_turn")), "impl AppState sumiu da varredura");
    let web = production_tokens(&read("harness/web.rs"));
    assert!(web.iter().any(|t| t.is_ident("Fetched")), "struct Fetched sumiu da varredura");
}
