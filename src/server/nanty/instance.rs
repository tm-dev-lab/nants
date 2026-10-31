//! Instância única do Nanty por pasta de dados (spec 138 T01, C-05).
//!
//! Um `flock` exclusivo em `<pasta de dados>/nanty/instance.lock`: dentro da mesma instalação, só
//! um processo liga o Nanty. Instalações diferentes (outra pessoa, outra máquina ou outra pasta de
//! dados na mesma máquina) têm arquivos diferentes e nunca se enxergam. A trava some sozinha quando
//! o processo termina (o sistema solta o `flock` ao fechar o arquivo), mesmo numa queda.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use super::jsonl::ensure_private_dir;

/// Nome do arquivo da trava, dentro da pasta do Nanty.
pub const LOCK_FILE: &str = "instance.lock";

/// Trava segurada enquanto viver (soltar = fechar o arquivo).
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// Resultado da tentativa de travar.
#[derive(Debug)]
pub enum LockOutcome {
    /// Este processo é o dono do Nanty desta pasta de dados.
    Acquired(InstanceLock),
    /// Outro processo com a mesma pasta de dados já tem o Nanty ligado.
    Busy,
    /// Não deu para criar ou abrir a trava (pasta insegura ou erro de I/O).
    Failed,
}

/// Tenta travar a pasta do Nanty (`dir`) sem esperar.
pub fn try_lock(dir: &Path) -> LockOutcome {
    if ensure_private_dir(dir).is_err() {
        return LockOutcome::Failed;
    }
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(LOCK_FILE))
    {
        Ok(file) => file,
        Err(_) => return LockOutcome::Failed,
    };
    // SAFETY: o descritor pertence ao `file`, vivo durante a chamada.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return LockOutcome::Acquired(InstanceLock { _file: file });
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EWOULDBLOCK) => LockOutcome::Busy,
        _ => LockOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nants-nanty-lock-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn same_data_dir_is_exclusive_and_released_on_drop() {
        let d = dir("same");
        let first = try_lock(&d);
        assert!(matches!(first, LockOutcome::Acquired(_)), "{first:?}");
        // `flock` vale por arquivo aberto: uma segunda abertura (como outro processo) fica de fora.
        assert!(matches!(try_lock(&d), LockOutcome::Busy));
        drop(first);
        assert!(matches!(try_lock(&d), LockOutcome::Acquired(_)), "a trava sai com o dono");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn different_data_dirs_never_see_each_other() {
        let (a, b) = (dir("inst-a"), dir("inst-b"));
        let la = try_lock(&a);
        let lb = try_lock(&b);
        assert!(matches!(la, LockOutcome::Acquired(_)) && matches!(lb, LockOutcome::Acquired(_)));
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn symlinked_lock_file_fails_closed() {
        let d = dir("link");
        std::fs::create_dir_all(&d).expect("dir");
        std::os::unix::fs::symlink(d.join("alvo"), d.join(LOCK_FILE)).expect("ln");
        assert!(matches!(try_lock(&d), LockOutcome::Failed));
        let _ = std::fs::remove_dir_all(&d);
    }
}
