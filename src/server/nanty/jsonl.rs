//! Arquivo JSONL com rotação (spec 138 T01): o registro do Nanty e a conversa.
//!
//! Uma linha por entrada, acrescentada no fim. Quando o arquivo atual passaria de `max_bytes`, ele
//! vira `<nome>.1.jsonl`, e cada geração antiga sobe um número. Depois, a mais velha sai enquanto
//! houver mais de `keep` gerações antigas ou todas juntas passarem de `total_bytes` (spec 140 T05).
//! Quem usa o arquivo pode segurar a geração mais velha ([`JsonlLog::append_with`]): a conversa
//! segura a linha ainda não resumida, e a geração fica até poder sair. Os arquivos são 0600, abertos
//! com `O_NOFOLLOW`; linha inválida na leitura é pulada.
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

/// Gerações antigas guardadas no máximo, mesmo seguradas: acima disso, a mais velha sai sempre, e
/// quem segura recebe `forced` para registrar o que saiu ([`JsonlLog::append_with`]).
pub const HARD_MAX_GENERATIONS: usize = 32;
/// Teto fixo de leitura de uma geração: duas vezes o maior arquivo que a configuração aceita
/// ([`crate::config::MAX_CONVERSATION_BYTES`]). Ele não segue o limite em vigor, então baixar o
/// limite nunca deixa ilegível uma geração já gravada (revisão 1 da spec 140 T05, F-01).
pub const READ_CEILING: u64 = crate::config::MAX_CONVERSATION_BYTES * 2;

/// Uma geração lida.
enum Generation {
    Missing,
    Text(String),
    /// Existe, mas não deu para ler (grande demais, symlink, erro de leitura).
    Unreadable,
}

/// Limites da rotação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Tamanho do arquivo atual antes de girar.
    pub max_bytes: u64,
    /// Gerações antigas guardadas (`.1` a `.keep`).
    pub keep: usize,
    /// Teto das gerações antigas juntas.
    pub total_bytes: u64,
}

/// A geração mais velha pode sair? A resposta de quem usa o arquivo ([`JsonlLog::append_with`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// Pode sair.
    Drop,
    /// Fica até a próxima rotação.
    Hold,
}

/// Arquivo JSONL com rotação.
#[derive(Debug, Clone)]
pub struct JsonlLog {
    dir: PathBuf,
    /// Nome sem a extensão (`journal`, `conversation`).
    stem: &'static str,
    /// Os limites, sob o mutex que serializa as operações sobre o arquivo (compartilhado entre os
    /// clones).
    limits: Arc<Mutex<Limits>>,
    /// Quantas vezes alguma linha saiu (rotação ou filtro): quem guarda um índice do arquivo sabe que
    /// precisa refazê-lo.
    removals: Arc<AtomicU64>,
    /// Teto de leitura de uma geração ([`READ_CEILING`]; os testes baixam).
    read_cap: Arc<AtomicU64>,
}

/// Falha de gravação (sem detalhe: nunca carrega caminho).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlError;

impl JsonlLog {
    /// Sem teto total: só `keep` gerações antigas.
    pub fn new(dir: PathBuf, stem: &'static str, max_bytes: u64, keep: usize) -> Self {
        Self::with_limits(dir, stem, Limits { max_bytes, keep, total_bytes: u64::MAX })
    }

    pub fn with_limits(dir: PathBuf, stem: &'static str, limits: Limits) -> Self {
        Self {
            dir,
            stem,
            limits: Arc::new(Mutex::new(limits)),
            removals: Arc::new(AtomicU64::new(0)),
            read_cap: Arc::new(AtomicU64::new(READ_CEILING)),
        }
    }

    /// Baixa o teto de leitura (testes: uma geração ilegível sem gravar megabytes).
    #[cfg(test)]
    pub fn set_read_cap(&self, cap: u64) {
        self.read_cap.store(cap, Ordering::Release);
    }

    fn read_generation(&self, generation: usize) -> Generation {
        let path = self.path(generation);
        if fs::symlink_metadata(&path).is_err() {
            return Generation::Missing;
        }
        match read_private(&path, self.read_cap.load(Ordering::Acquire)) {
            Some(text) => Generation::Text(text),
            None => Generation::Unreadable,
        }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, Limits> {
        self.limits.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Troca os limites (a configuração mudou): valem da próxima rotação em diante.
    pub fn set_limits(&self, limits: Limits) {
        *self.guard() = limits;
    }

    /// Quantas vezes alguma linha saiu do arquivo.
    pub fn removals(&self) -> u64 {
        self.removals.load(Ordering::Acquire)
    }

    fn removed(&self) {
        self.removals.fetch_add(1, Ordering::AcqRel);
    }

    /// Caminho da geração `n` (0 = o atual).
    pub fn path(&self, generation: usize) -> PathBuf {
        match generation {
            0 => self.dir.join(format!("{}.jsonl", self.stem)),
            n => self.dir.join(format!("{}.{n}.jsonl", self.stem)),
        }
    }

    /// A maior geração antiga presente (`.1` a `.n`). Um buraco deixado por uma rotação interrompida
    /// não esconde as gerações depois dele (revisão 1, F-02).
    fn generations(&self) -> usize {
        (1..=HARD_MAX_GENERATIONS + 1).rev().find(|n| fs::symlink_metadata(self.path(*n)).is_ok()).unwrap_or(0)
    }

    /// Acrescenta uma entrada, girando antes se o arquivo atual passaria do limite.
    pub fn append<T: Serialize>(&self, entry: &T) -> Result<(), JsonlError> {
        self.append_with(entry, |_, _| Release::Drop)
    }

    /// Como [`Self::append`], perguntando a `release` se a geração mais velha pode sair. Ele recebe o
    /// texto dela (`None`: ela existe, mas não deu para ler) e `forced`: acima de
    /// [`HARD_MAX_GENERATIONS`] gerações, ela sai mesmo com [`Release::Hold`], e quem segura
    /// registra o que saiu antes de responder.
    pub fn append_with<T: Serialize>(
        &self,
        entry: &T,
        mut release: impl FnMut(Option<&str>, bool) -> Release,
    ) -> Result<(), JsonlError> {
        let mut line = serde_json::to_string(entry).map_err(|_| JsonlError)?;
        let limits = self.guard();
        line.push('\n');
        ensure_private_dir(&self.dir)?;
        let current = self.path(0);
        let size = match fs::symlink_metadata(&current) {
            Ok(m) if m.file_type().is_file() => m.len(),
            Ok(_) => return Err(JsonlError),
            Err(_) => 0,
        };
        if size > 0 && size + line.len() as u64 > limits.max_bytes {
            self.rotate(&limits, &mut release)?;
        } else if size > 0 && !ends_with_newline(&current) {
            // Uma linha cortada por uma queda: a nova começa na linha seguinte, e só a cortada é pulada.
            line.insert(0, '\n');
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

    /// Gira as gerações: cada uma sobe um número e o atual vira `.1`. Depois, a mais velha sai
    /// enquanto houver mais de `keep` gerações antigas ou elas juntas passarem do teto, se `release`
    /// deixar.
    ///
    /// Com um buraco (uma rotação interrompida), cada geração sobe para o número seguinte, que já
    /// está livre: a ordem é do maior para o menor, então nenhuma é sobrescrita.
    fn rotate(&self, limits: &Limits, release: &mut impl FnMut(Option<&str>, bool) -> Release) -> Result<(), JsonlError> {
        let mut n = self.generations();
        for g in (0..=n).rev() {
            let from = self.path(g);
            if fs::symlink_metadata(&from).is_ok() {
                fs::rename(&from, self.path(g + 1)).map_err(|_| JsonlError)?;
            }
        }
        n += 1;
        while n > 0 {
            let total: u64 = (1..=n).filter_map(|g| fs::symlink_metadata(self.path(g)).ok()).map(|m| m.len()).sum();
            let forced = n > HARD_MAX_GENERATIONS;
            if !forced && n <= limits.keep && total <= limits.total_bytes {
                break;
            }
            let answer = match self.read_generation(n) {
                // Um buraco: não há o que tirar neste número.
                Generation::Missing => {
                    n -= 1;
                    continue;
                }
                Generation::Text(text) => release(Some(&text), forced),
                Generation::Unreadable => release(None, forced),
            };
            if answer == Release::Hold && !forced {
                break;
            }
            fs::remove_file(self.path(n)).map_err(|_| JsonlError)?;
            self.removed();
            n -= 1;
        }
        Ok(())
    }

    /// As últimas `limit` entradas, da mais velha para a mais nova, lendo das gerações antigas só
    /// o que faltar. Linhas que não são do tipo pedido são puladas.
    pub fn tail<T: DeserializeOwned>(&self, limit: usize) -> Vec<T> {
        let _limits = self.guard();
        let mut out: Vec<T> = Vec::new();
        for generation in 0..=self.generations() {
            if out.len() >= limit {
                break;
            }
            let Generation::Text(text) = self.read_generation(generation) else {
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

    /// Todas as entradas de todas as gerações, da mais velha para a mais nova, ou erro se alguma
    /// geração existe e não dá para ler (spec 140 T06, revisão 1, F-01): quem decide a cobertura do
    /// resumo ou a sequência das linhas nunca o faz sobre uma leitura parcial.
    pub fn all_strict<T: DeserializeOwned>(&self) -> Result<Vec<T>, JsonlError> {
        let _limits = self.guard();
        let mut out: Vec<T> = Vec::new();
        for generation in 0..=self.generations() {
            let text = match self.read_generation(generation) {
                Generation::Missing => continue,
                Generation::Unreadable => return Err(JsonlError),
                Generation::Text(text) => text,
            };
            let mut older: Vec<T> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
            older.append(&mut out);
            out = older;
        }
        Ok(out)
    }

    /// Reescreve todas as gerações mantendo só as entradas que passam em `keep`, preservando a
    /// ordem (a limpeza de um projeto removido). Uma geração que existe e não dá para ler é um erro:
    /// a limpeza não fica pela metade em silêncio. As outras gerações são limpas mesmo assim.
    pub fn retain<T: Serialize + DeserializeOwned>(&self, keep: impl Fn(&T) -> bool) -> Result<(), JsonlError> {
        let _limits = self.guard();
        let mut failed = false;
        for generation in 0..=self.generations() {
            let path = self.path(generation);
            let text = match self.read_generation(generation) {
                Generation::Missing => continue,
                Generation::Unreadable => {
                    failed = true;
                    continue;
                }
                Generation::Text(text) => text,
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
            self.removed();
        }
        if failed {
            return Err(JsonlError);
        }
        Ok(())
    }

    /// Passa `f` por todas as entradas, da mais velha para a mais nova, e regrava (de forma atômica)
    /// só as gerações em que `f` mudou alguma (`true`). As linhas que não são do tipo pedido ficam
    /// como estão. Uma geração ilegível é um erro, depois de passar pelas outras.
    pub fn map_all<T: Serialize + DeserializeOwned>(&self, mut f: impl FnMut(&mut T) -> bool) -> Result<(), JsonlError> {
        let _limits = self.guard();
        let mut failed = false;
        for generation in (0..=self.generations()).rev() {
            let text = match self.read_generation(generation) {
                Generation::Missing => continue,
                Generation::Unreadable => {
                    failed = true;
                    continue;
                }
                Generation::Text(text) => text,
            };
            let mut changed = false;
            let mut body = String::with_capacity(text.len());
            for raw in text.lines() {
                let mapped = serde_json::from_str::<T>(raw).ok().and_then(|mut entry| f(&mut entry).then_some(entry));
                match mapped {
                    Some(entry) => {
                        changed = true;
                        body.push_str(&serde_json::to_string(&entry).map_err(|_| JsonlError)?);
                    }
                    None => body.push_str(raw),
                }
                body.push('\n');
            }
            if changed {
                write_atomic(&self.path(generation), body.as_bytes())?;
            }
        }
        if failed {
            return Err(JsonlError);
        }
        Ok(())
    }
}

/// O arquivo termina numa quebra de linha (ou não dá para ler o último byte: conta como sim).
fn ends_with_newline(path: &Path) -> bool {
    use std::io::{Seek, SeekFrom};
    let last = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path).and_then(|mut f| {
        f.seek(SeekFrom::End(-1))?;
        let mut byte = [0u8; 1];
        f.read_exact(&mut byte)?;
        Ok(byte[0])
    });
    last.map_or(true, |b| b == b'\n')
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

    /// Spec 140 T05: quem segura a geração mais velha a mantém além do limite, até o fixo
    /// [`HARD_MAX_GENERATIONS`], quando ela sai com `forced`; cada saída conta em `removals`.
    #[test]
    fn a_held_generation_stays_until_the_hard_ceiling() {
        let d = dir("hold");
        let log = JsonlLog::new(d.clone(), "c", 20, 1);
        let mut forced_seen = 0;
        for n in 0..(HARD_MAX_GENERATIONS as u32 + 4) {
            log.append_with(&E { n, p: "ab".into() }, |text, forced| {
                assert!(text.is_some_and(|t| !t.is_empty()));
                forced_seen += usize::from(forced);
                Release::Hold
            })
            .expect("append");
        }
        assert!(log.path(HARD_MAX_GENERATIONS).is_file() && !log.path(HARD_MAX_GENERATIONS + 1).exists());
        assert!(forced_seen > 0);
        assert_eq!(log.removals(), forced_seen as u64);
        let before = log.removals();
        log.set_limits(Limits { max_bytes: 20, keep: 1, total_bytes: u64::MAX });
        log.append(&E { n: 999, p: "ab".into() }).expect("append");
        assert!(!log.path(2).exists(), "liberada, volta ao limite numa rotação");
        assert!(log.removals() > before);
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
