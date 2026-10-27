//! Instantâneo dos arquivos da pasta do projeto (spec 130 T02, C-10): o que um programa que roda e
//! termina criou ou mudou vem da diferença entre dois instantâneos (antes de subir e depois de
//! sair), por tamanho e data de modificação. Inclui os arquivos ignorados pelo `.gitignore` (uma
//! saída em `out/` conta) e deixa de fora `.git`, as pastas do Nants (`.nants*`, inclusive a pasta
//! pessoal da 129) e `.sdd`. Nenhuma regra por linguagem.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use crate::harness::home;

/// Teto de arquivos lidos por instantâneo: além dele, o instantâneo fica incompleto (e a lista
/// avisa o corte).
pub const MAX_SCAN: usize = 50_000;

/// Marca de um arquivo: tamanho e data de modificação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    len: u64,
    modified: Option<SystemTime>,
}

/// Arquivos regulares da pasta, por caminho relativo (`/`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeSnapshot {
    files: BTreeMap<String, Mark>,
    /// O teto [`MAX_SCAN`] foi atingido.
    pub incomplete: bool,
}

/// Como o arquivo mudou entre os dois instantâneos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
    Added,
    Changed,
}

impl FileChange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Changed => "changed",
        }
    }
}

/// Lista limitada dos arquivos criados ou mudados.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedFiles {
    /// Até o limite pedido, em ordem de caminho.
    pub files: Vec<(String, FileChange)>,
    /// Quantos mudaram ao todo.
    pub total: usize,
    /// A lista foi cortada (mais que o limite) ou um instantâneo ficou incompleto.
    pub truncated: bool,
}

/// `true` para o que nunca entra: `.git` em qualquer nível, e na raiz as pastas do Nants
/// (`.nants`, `.nants-home`, `.nants-outputs`...) e `.sdd`.
fn skipped(rel: &str, name: &str, at_root: bool) -> bool {
    if name == ".git" {
        return true;
    }
    at_root && (name == ".sdd" || name.starts_with(".nants") || home::is_home_path(rel))
}

impl TreeSnapshot {
    /// Lê a pasta `root` (sem seguir links simbólicos).
    pub fn scan(root: &Path) -> Self {
        Self::scan_limited(root, MAX_SCAN)
    }

    fn scan_limited(root: &Path, max: usize) -> Self {
        let mut snap = Self::default();
        let mut stack: Vec<String> = vec![String::new()];
        while let Some(rel) = stack.pop() {
            let Ok(entries) = fs::read_dir(if rel.is_empty() { root.to_path_buf() } else { root.join(&rel) }) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
                let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                if skipped(&path, &name, rel.is_empty()) {
                    continue;
                }
                let Ok(meta) = entry.path().symlink_metadata() else { continue };
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.is_file() {
                    if snap.files.len() >= max {
                        snap.incomplete = true;
                        return snap;
                    }
                    snap.files.insert(path, Mark { len: meta.len(), modified: meta.modified().ok() });
                }
            }
        }
        snap
    }

    /// Arquivos de `after` criados ou mudados desde `self`, até `limit`.
    pub fn changes(&self, after: &Self, limit: usize) -> ChangedFiles {
        let all: Vec<(String, FileChange)> = after
            .files
            .iter()
            .filter_map(|(path, mark)| match self.files.get(path) {
                None => Some((path.clone(), FileChange::Added)),
                Some(before) if before != mark => Some((path.clone(), FileChange::Changed)),
                Some(_) => None,
            })
            .collect();
        let total = all.len();
        let files: Vec<_> = all.into_iter().take(limit).collect();
        ChangedFiles { truncated: total > files.len() || self.incomplete || after.incomplete, files, total }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-snap-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn lists_created_and_changed_files_including_ignored_ones_and_skips_nants_places() {
        let root = tmp("diff");
        fs::write(root.join(".gitignore"), "out/\n").expect("w");
        fs::write(root.join("kept.txt"), "igual").expect("w");
        fs::write(root.join("edited.txt"), "antes").expect("w");
        for dir in [".git", ".nants", ".nants-home", ".nants-outputs", ".sdd", "src/.git"] {
            fs::create_dir_all(root.join(dir)).expect("d");
        }
        let before = TreeSnapshot::scan(&root);
        fs::create_dir_all(root.join("out")).expect("d");
        fs::write(root.join("out/report.csv"), "a,b\n").expect("w");
        fs::write(root.join("edited.txt"), "depois, maior").expect("w");
        for file in [".git/index", ".nants/compose.yaml", ".nants-home/.profile", ".nants-outputs/x", ".sdd/s.md", "src/.git/HEAD"] {
            fs::write(root.join(file), "x").expect("w");
        }
        let after = TreeSnapshot::scan(&root);
        let got = before.changes(&after, 10);
        assert_eq!(
            got.files,
            vec![("edited.txt".to_string(), FileChange::Changed), ("out/report.csv".to_string(), FileChange::Added)]
        );
        assert_eq!(got.total, 2);
        assert!(!got.truncated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn list_is_limited_with_a_cut_notice_and_symlinks_are_not_followed() {
        let root = tmp("limit");
        let before = TreeSnapshot::scan(&root);
        for i in 0..5 {
            fs::write(root.join(format!("f{i}.txt")), "x").expect("w");
        }
        std::os::unix::fs::symlink("/etc", root.join("link")).expect("link");
        let after = TreeSnapshot::scan(&root);
        let got = before.changes(&after, 3);
        assert_eq!(got.files.len(), 3);
        assert_eq!(got.total, 5);
        assert!(got.truncated);
        assert!(got.files.iter().all(|(p, _)| p.starts_with('f')), "{got:?}");
        // Teto de leitura: instantâneo incompleto avisa o corte.
        let capped = TreeSnapshot::scan_limited(&root, 2);
        assert!(capped.incomplete);
        assert!(before.changes(&capped, 10).truncated);
        let _ = fs::remove_dir_all(&root);
    }
}
