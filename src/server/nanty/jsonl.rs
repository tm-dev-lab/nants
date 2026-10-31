//! Arquivo JSONL com rotação (spec 138 T01): o registro do Nanty e a conversa.
//!
//! Uma linha por entrada, acrescentada no fim. Quando o arquivo atual passaria de `max_bytes`, ele
//! vira `<nome>.1.jsonl` (o `.1` anterior vira `.2`, e assim até `keep`; o mais velho sai). Os
//! arquivos são 0600, abertos com `O_NOFOLLOW`; linha inválida na leitura é pulada.
//!
//! Cada `JsonlLog` (e os clones dele) tem um mutex próprio: acrescentar, girar, filtrar e ler
//! acontecem um de cada vez, então um `retain` nunca perde a linha de um `append` paralelo e a
//! rotação nunca roda duas vezes. Cada arquivo deve ter uma única instância (o `Store` e o
//! `Journal` guardam a sua).

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Arquivo JSONL com rotação.
#[derive(Debug, Clone)]
pub struct JsonlLog {
    dir: PathBuf,
    /// Nome sem a extensão (`journal`, `conversation`).
    stem: &'static str,
    max_bytes: u64,
    /// Gerações antigas guardadas (`.1` a `.keep`).
    keep: usize,
    /// Serializa as operações sobre o arquivo (compartilhado entre os clones).
    lock: Arc<Mutex<()>>,
}

/// Falha de gravação (sem detalhe: nunca carrega caminho).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlError;

impl JsonlLog {
    pub fn new(dir: PathBuf, stem: &'static str, max_bytes: u64, keep: usize) -> Self {
        Self { dir, stem, max_bytes, keep, lock: Arc::new(Mutex::new(())) }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Caminho da geração `n` (0 = o atual).
    pub fn path(&self, generation: usize) -> PathBuf {
        match generation {
            0 => self.dir.join(format!("{}.jsonl", self.stem)),
            n => self.dir.join(format!("{}.{n}.jsonl", self.stem)),
        }
    }

    /// Acrescenta uma entrada, girando antes se o arquivo atual passaria do limite.
    pub fn append<T: Serialize>(&self, entry: &T) -> Result<(), JsonlError> {
        let mut line = serde_json::to_string(entry).map_err(|_| JsonlError)?;
        let _guard = self.guard();
        line.push('\n');
        ensure_private_dir(&self.dir)?;
        let current = self.path(0);
        let size = match fs::symlink_metadata(&current) {
            Ok(m) if m.file_type().is_file() => m.len(),
            Ok(_) => return Err(JsonlError),
            Err(_) => 0,
        };
        if size > 0 && size + line.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&current)
            .map_err(|_| JsonlError)?;
        file.write_all(line.as_bytes()).map_err(|_| JsonlError)
    }

    /// Gira as gerações: o mais velho sai, cada uma sobe um número e o atual vira `.1`.
    fn rotate(&self) -> Result<(), JsonlError> {
        if self.keep == 0 {
            return fs::remove_file(self.path(0)).map_err(|_| JsonlError);
        }
        let _ = fs::remove_file(self.path(self.keep));
        for n in (0..self.keep).rev() {
            let from = self.path(n);
            if fs::symlink_metadata(&from).is_ok() {
                fs::rename(&from, self.path(n + 1)).map_err(|_| JsonlError)?;
            }
        }
        Ok(())
    }

    /// As últimas `limit` entradas, da mais velha para a mais nova, lendo das gerações antigas só
    /// o que faltar. Linhas que não são do tipo pedido são puladas.
    pub fn tail<T: DeserializeOwned>(&self, limit: usize) -> Vec<T> {
        let _guard = self.guard();
        let mut out: Vec<T> = Vec::new();
        for generation in 0..=self.keep {
            if out.len() >= limit {
                break;
            }
            let Some(text) = read_private(&self.path(generation), self.max_bytes.saturating_mul(2)) else {
                continue;
            };
            let mut older: Vec<T> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
            let start = older.len().saturating_sub(limit - out.len());
            older.drain(..start);
            older.append(&mut out);
            out = older;
        }
        out
    }

    /// Reescreve todas as gerações mantendo só as entradas que passam em `keep`, preservando a
    /// ordem (a limpeza de um projeto removido).
    pub fn retain<T: Serialize + DeserializeOwned>(&self, keep: impl Fn(&T) -> bool) -> Result<(), JsonlError> {
        let _guard = self.guard();
        for generation in 0..=self.keep {
            let path = self.path(generation);
            let Some(text) = read_private(&path, self.max_bytes.saturating_mul(2)) else {
                continue;
            };
            let entries: Vec<T> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
            let kept: Vec<&T> = entries.iter().filter(|e| keep(e)).collect();
            if kept.len() == entries.len() {
                continue;
            }
            let mut body = String::new();
            for entry in kept {
                body.push_str(&serde_json::to_string(entry).map_err(|_| JsonlError)?);
                body.push('\n');
            }
            write_atomic(&path, body.as_bytes())?;
        }
        Ok(())
    }
}

/// Cria a pasta (0700) se faltar; recusa symlink ou algo que não é pasta.
pub fn ensure_private_dir(dir: &Path) -> Result<(), JsonlError> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        Ok(_) => Err(JsonlError),
        Err(_) => fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|_| JsonlError),
    }
}

/// Lê um arquivo regular de até `max` bytes (`O_NOFOLLOW`); ausente, grande demais ou ilegível =
/// `None`.
pub fn read_private(path: &Path, max: u64) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > max {
        return None;
    }
    let mut text = String::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .and_then(|f| f.take(max).read_to_string(&mut text))
        .ok()?;
    Some(text)
}

/// Como [`read_private`], em bytes (os áudios da conversa, T09).
pub fn read_private_bytes(path: &Path, max: u64) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > max {
        return None;
    }
    let mut data = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .and_then(|f| f.take(max).read_to_end(&mut data))
        .ok()?;
    Some(data)
}

/// Grava `body` em `path` de forma atômica (temporário 0600 na mesma pasta + `rename`).
pub fn write_atomic(path: &Path, body: &[u8]) -> Result<(), JsonlError> {
    let dir = path.parent().ok_or(JsonlError)?;
    ensure_private_dir(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).ok_or(JsonlError)?;
    // Nome único por gravação: duas gravações paralelas nunca dividem o temporário.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{seq}.tmp", std::process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(body)?;
            f.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(JsonlError);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct E {
        n: u32,
        p: String,
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nants-nanty-jsonl-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn rotates_keeps_generations_and_tail_spans_them() {
        let d = dir("rotate");
        // Cada linha tem 16 bytes ({"n":N,"p":"ab"} + \n); 40 bytes = 2 linhas por arquivo.
        let log = JsonlLog::new(d.clone(), "journal", 40, 2);
        for n in 0..9 {
            log.append(&E { n, p: "ab".into() }).expect("append");
        }
        assert!(log.path(1).is_file() && log.path(2).is_file());
        assert!(!log.path(3).exists(), "só `keep` gerações antigas");
        let all: Vec<E> = log.tail(100);
        let ns: Vec<u32> = all.iter().map(|e| e.n).collect();
        assert_eq!(ns, [4, 5, 6, 7, 8], "as mais velhas saíram com a rotação");
        let last: Vec<E> = log.tail(3);
        assert_eq!(last.iter().map(|e| e.n).collect::<Vec<_>>(), [6, 7, 8]);
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(log.path(0)).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn retain_filters_every_generation_and_skips_bad_lines() {
        let d = dir("retain");
        let log = JsonlLog::new(d.clone(), "c", 40, 1);
        for (n, p) in [(1, "aa"), (2, "bb"), (3, "aa"), (4, "bb")] {
            log.append(&E { n, p: p.into() }).expect("append");
        }
        let mut f = OpenOptions::new().append(true).open(log.path(0)).expect("open");
        f.write_all(b"lixo\n").expect("lixo");
        log.retain::<E>(|e| e.p != "aa").expect("retain");
        let left: Vec<E> = log.tail(10);
        assert_eq!(left.iter().map(|e| e.n).collect::<Vec<_>>(), [2, 4]);
        let _ = fs::remove_dir_all(&d);
    }

    /// Revisão 1 (F-02): várias threads acrescentando (com rotação) enquanto outra filtra; nenhuma
    /// linha que o filtro mantém se perde.
    #[test]
    fn parallel_appends_and_retain_lose_no_line() {
        let d = dir("parallel");
        // Limite que gira várias vezes, mas guarda tudo em 1 + 8 gerações.
        let log = JsonlLog::new(d.clone(), "j", 2_000, 8);
        let threads: Vec<_> = (0..4u32)
            .map(|t| {
                let log = log.clone();
                std::thread::spawn(move || {
                    for i in 0..60u32 {
                        let p = if i % 3 == 0 { "fora" } else { "fica" };
                        log.append(&E { n: t * 1000 + i, p: p.into() }).expect("append");
                    }
                })
            })
            .collect();
        let cleaner = {
            let log = log.clone();
            std::thread::spawn(move || {
                for _ in 0..30 {
                    log.retain::<E>(|e| e.p != "fora").expect("retain");
                }
            })
        };
        threads.into_iter().for_each(|h| h.join().expect("thread"));
        cleaner.join().expect("cleaner");
        log.retain::<E>(|e| e.p != "fora").expect("retain final");
        let mut left: Vec<u32> = log.tail::<E>(10_000).into_iter().map(|e| e.n).collect();
        left.sort_unstable();
        let mut expected: Vec<u32> =
            (0..4u32).flat_map(|t| (0..60u32).filter(|i| i % 3 != 0).map(move |i| t * 1000 + i)).collect();
        expected.sort_unstable();
        assert_eq!(left, expected);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn symlinked_file_is_refused() {
        let d = dir("link");
        fs::create_dir_all(&d).expect("dir");
        let target = d.join("alvo");
        fs::write(&target, "").expect("alvo");
        std::os::unix::fs::symlink(&target, d.join("j.jsonl")).expect("ln");
        let log = JsonlLog::new(d.clone(), "j", 1000, 1);
        assert_eq!(log.append(&E { n: 1, p: "x".into() }), Err(JsonlError));
        assert_eq!(fs::read_to_string(&target).expect("alvo"), "");
        let _ = fs::remove_dir_all(&d);
    }
}
