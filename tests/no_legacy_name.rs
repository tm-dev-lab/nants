//! Spec 100 T03 (critério global G1): o produto se chama Nants e nenhum arquivo do produto
//! guarda o nome antigo, sem diferenciar maiúsculas. A varredura cobre `src/`, `web/src/`,
//! `web/public/`, `web/index.html`, `web/package.json`, `tests/` e `Cargo.toml`.
//!
//! Única exceção: a linha de `web/src/staleStorage.ts` que define `LEGACY_PREFIX`, usada para
//! limpar as chaves antigas do navegador. Este arquivo monta o nome antigo com `concat!` para
//! não se acusar.

use std::fs;
use std::path::{Path, PathBuf};

/// Nome antigo, montado em partes para não aparecer literal neste arquivo.
const LEGACY: &str = concat!("ne", "mo");

/// Raízes da varredura, relativas à raiz do crate.
const ROOTS: &[&str] = &[
    "src",
    "web/src",
    "web/public",
    "web/index.html",
    "web/package.json",
    "tests",
    "Cargo.toml",
];

/// Exceções: arquivo (relativo à raiz do crate), trecho que a linha precisa conter e o motivo.
const EXEMPT: &[(&str, &str, &str)] = &[(
    "web/src/staleStorage.ts",
    "LEGACY_PREFIX",
    "prefixo das chaves antigas do navegador, que a limpeza apaga",
)];

/// Extensões binárias, que não entram na varredura.
const BINARY_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "ico", "icns", "bmp", "avif", "woff", "woff2", "ttf",
    "otf", "eot", "mp3", "mp4", "webm", "wav", "ogg", "pdf", "zip", "gz", "wasm",
];

fn is_binary(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| BINARY_EXT.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .unwrap_or_else(|e| panic!("não leu {}: {e}", path.display()))
            .map(|e| e.expect("entrada de diretório").path())
            .collect();
        entries.sort();
        for entry in entries {
            collect(&entry, out);
        }
    } else if path.is_file() && !is_binary(path) {
        out.push(path.to_path_buf());
    }
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_exempt(file: &str, line: &str) -> bool {
    EXEMPT
        .iter()
        .any(|(f, needle, _)| *f == file && line.contains(needle))
}

/// Ocorrências do nome antigo (arquivo:linha: conteúdo), fora das exceções.
fn scan(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for r in ROOTS {
        let p = root.join(r);
        assert!(p.exists(), "raiz da varredura ausente: {r}");
        collect(&p, &mut files);
    }
    let mut hits = Vec::new();
    for file in &files {
        let bytes = fs::read(file).unwrap_or_else(|e| panic!("não leu {}: {e}", file.display()));
        let text = String::from_utf8_lossy(&bytes);
        let name = rel(root, file);
        for (i, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(LEGACY) && !is_exempt(&name, line) {
                hits.push(format!("{name}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    hits
}

#[test]
fn product_files_do_not_use_the_legacy_name() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let hits = scan(root);
    assert!(
        hits.is_empty(),
        "nome antigo encontrado em arquivos do produto:\n{}",
        hits.join("\n")
    );
}

#[test]
fn every_exemption_is_still_needed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (file, needle, why) in EXEMPT {
        let text = fs::read_to_string(root.join(file))
            .unwrap_or_else(|e| panic!("exceção aponta para arquivo ausente {file}: {e}"));
        let used = text
            .lines()
            .any(|l| l.contains(needle) && l.to_lowercase().contains(LEGACY));
        assert!(used, "exceção sem uso em {file} ({needle}): {why}");
    }
}

#[test]
fn scan_catches_the_legacy_name_in_any_case() {
    let dir = std::env::temp_dir().join(format!("nants-legacy-scan-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    for r in ROOTS {
        let p = dir.join(r);
        if r.contains('.') {
            fs::create_dir_all(p.parent().expect("pai")).expect("cria pasta");
            fs::write(&p, "ok\n").expect("grava");
        } else {
            fs::create_dir_all(&p).expect("cria pasta");
        }
    }
    let upper = LEGACY.to_uppercase();
    fs::write(dir.join("src/a.rs"), format!("let x = \"{upper}\";\n")).expect("grava");
    fs::write(dir.join("web/public/logo.png"), LEGACY).expect("grava");
    fs::create_dir_all(dir.join("web/src")).expect("cria pasta");
    fs::write(
        dir.join("web/src/staleStorage.ts"),
        format!("export const LEGACY_PREFIX = \"{LEGACY}.\";\nconst other = \"{LEGACY}\";\n"),
    )
    .expect("grava");
    let hits = scan(&dir);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert!(hits[0].starts_with("src/a.rs:1:"), "{hits:?}");
    assert!(hits[1].starts_with("web/src/staleStorage.ts:2:"), "{hits:?}");
}
