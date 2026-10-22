//! Único ponto de I/O sobre o filesystem do projeto do usuário.
//!
//! O projeto é controlado por quem usa o Nants, então qualquer componente pode ser
//! symlink, FIFO ou dispositivo. Regras:
//! - nenhum componente de caminho relativo ao projeto pode ser symlink;
//! - leituras só em arquivos regulares, com `O_NOFOLLOW | O_NONBLOCK` e limite de tamanho;
//! - escritas nunca sobrescrevem (`create_new`) e criam diretórios um a um, revalidando;
//! - erros são tipados e mapeados para mensagens estáticas (sem caminhos do SO).

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// `O_NOFOLLOW` no Linux: falha se o último componente for symlink.
pub const O_NOFOLLOW: i32 = 0o400000;
/// `O_NONBLOCK` no Linux: abrir FIFO sem bloquear.
pub const O_NONBLOCK: i32 = 0o4000;
/// `ELOOP` no Linux: symlink encontrado com `O_NOFOLLOW`.
const ELOOP: i32 = 40;
/// `ENOTDIR` no Linux: um componente intermediário é arquivo, não diretório.
const ENOTDIR: i32 = 20;
/// Tamanho máximo lido de um arquivo do projeto (1 MiB).
pub const MAX_READ_BYTES: u64 = 1024 * 1024;

/// Falhas de I/O seguro. Nunca carregam caminhos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeFsError {
    /// Caminho ausente.
    Missing,
    /// Symlink em algum componente.
    Unsafe,
    /// Não é arquivo regular (diretório, FIFO, dispositivo, socket).
    NotRegular,
    /// Arquivo maior que o limite.
    TooLarge,
    /// Conteúdo que não é UTF-8.
    Invalid,
    /// Outra falha de I/O.
    Io,
}

impl SafeFsError {
    /// Mensagem estática para respostas HTTP.
    pub fn static_message(self) -> &'static str {
        match self {
            Self::Unsafe => "diretório do projeto inseguro (symlink)",
            Self::NotRegular | Self::TooLarge | Self::Invalid => {
                "diretório do projeto inseguro (arquivo especial ou grande demais)"
            }
            Self::Missing | Self::Io => "falha de I/O no diretório do projeto",
        }
    }
}

/// Separa `rel` em componentes normais. Rejeita vazio, `.`, `..` e caminhos absolutos.
pub fn split_rel(rel: &str) -> Result<Vec<&str>, SafeFsError> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err(SafeFsError::Unsafe);
    }
    Ok(parts)
}

/// Erro de `symlink_metadata`: ausente vira `Missing`.
fn meta_error(e: &std::io::Error) -> SafeFsError {
    if e.kind() == std::io::ErrorKind::NotFound {
        SafeFsError::Missing
    } else {
        SafeFsError::Io
    }
}

/// Erro de abertura com `O_NOFOLLOW`: symlink vira `Unsafe`.
fn open_error(e: &std::io::Error) -> SafeFsError {
    match (e.raw_os_error(), e.kind()) {
        (Some(ELOOP), _) => SafeFsError::Unsafe,
        (_, std::io::ErrorKind::NotFound) => SafeFsError::Missing,
        _ => SafeFsError::Io,
    }
}

/// Percorre cada componente de `rel` sob `root` com `symlink_metadata` e recusa symlink
/// em qualquer um. Componentes ausentes são aceitos (são criados depois, se preciso).
pub fn check_chain(root: &Path, rel: &str) -> Result<PathBuf, SafeFsError> {
    let parts = split_rel(rel)?;
    let root_meta = fs::symlink_metadata(root).map_err(|e| meta_error(&e))?;
    if root_meta.file_type().is_symlink() {
        return Err(SafeFsError::Unsafe);
    }
    let mut cur = root.to_path_buf();
    for part in parts {
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return Err(SafeFsError::Unsafe),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // Componente intermediário que é arquivo: tipo inadequado, não falha de I/O.
            Err(e) if e.raw_os_error() == Some(ENOTDIR) => return Err(SafeFsError::NotRegular),
            Err(_) => return Err(SafeFsError::Io),
        }
    }
    Ok(cur)
}

/// Garante que `rel` é um diretório real (não symlink) sob `root`, criando componentes
/// ausentes um a um. Revalida cada um após criar.
pub fn ensure_dirs(root: &Path, rel: &str) -> Result<(), SafeFsError> {
    check_chain(root, rel)?;
    let mut cur = root.to_path_buf();
    for part in split_rel(rel)? {
        cur.push(part);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => return Err(SafeFsError::Unsafe),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&cur).map_err(|_| SafeFsError::Io)?;
                let m = fs::symlink_metadata(&cur).map_err(|e| meta_error(&e))?;
                if !m.file_type().is_dir() {
                    return Err(SafeFsError::Unsafe);
                }
            }
            Err(_) => return Err(SafeFsError::Io),
        }
    }
    Ok(())
}

/// Remove o arquivo regular `rel` sob `root`, sem symlink em nenhum componente. Ausente → `Ok`.
pub fn remove_regular(root: &Path, rel: &str) -> Result<(), SafeFsError> {
    let path = check_chain(root, rel)?;
    match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() => fs::remove_file(&path).map_err(|_| SafeFsError::Io),
        Ok(_) => Err(SafeFsError::NotRegular),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SafeFsError::Io),
    }
}

/// Lê `rel` sob `root` como texto UTF-8. Só aceita arquivo regular, sem symlink no caminho,
/// de até `max_bytes` bytes.
pub fn read_regular(root: &Path, rel: &str, max_bytes: u64) -> Result<String, SafeFsError> {
    let path = check_chain(root, rel)?;
    let meta = fs::symlink_metadata(&path).map_err(|e| meta_error(&e))?;
    if !meta.file_type().is_file() {
        return Err(SafeFsError::NotRegular);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(&path)
        .map_err(|e| open_error(&e))?;
    let opened = file.metadata().map_err(|_| SafeFsError::Io)?;
    if !opened.is_file() {
        return Err(SafeFsError::NotRegular);
    }
    let mut buf = Vec::new();
    (&file)
        .take(max_bytes + 1)
        .read_to_end(&mut buf)
        .map_err(|_| SafeFsError::Io)?;
    if buf.len() as u64 > max_bytes {
        return Err(SafeFsError::TooLarge);
    }
    String::from_utf8(buf).map_err(|_| SafeFsError::Invalid)
}

/// Arquivo regular lido de forma limitada: `data` é `None` quando excede o limite.
#[derive(Debug)]
pub struct BoundedRead {
    pub size: u64,
    pub data: Option<Vec<u8>>,
}

/// Lê `rel` sob `root` como bytes (mesmas garantias de [`read_regular`]), sem exigir UTF-8.
/// Acima de `max_bytes` devolve só o tamanho.
pub fn read_bounded(root: &Path, rel: &str, max_bytes: u64) -> Result<BoundedRead, SafeFsError> {
    let path = check_chain(root, rel)?;
    let meta = fs::symlink_metadata(&path).map_err(|e| meta_error(&e))?;
    if !meta.file_type().is_file() {
        return Err(SafeFsError::NotRegular);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(&path)
        .map_err(|e| open_error(&e))?;
    let opened = file.metadata().map_err(|_| SafeFsError::Io)?;
    if !opened.is_file() {
        return Err(SafeFsError::NotRegular);
    }
    if opened.len() > max_bytes {
        return Ok(BoundedRead { size: opened.len(), data: None });
    }
    let mut buf = Vec::new();
    (&file).take(max_bytes + 1).read_to_end(&mut buf).map_err(|_| SafeFsError::Io)?;
    if buf.len() as u64 > max_bytes {
        return Ok(BoundedRead { size: buf.len() as u64, data: None });
    }
    Ok(BoundedRead { size: buf.len() as u64, data: Some(buf) })
}

/// Cria `rel` com `contents` sem sobrescrever. `Ok(true)` se criou; `Ok(false)` se já existia.
pub fn write_new(root: &Path, rel: &str, contents: &str) -> Result<bool, SafeFsError> {
    let parts = split_rel(rel)?;
    if let Some((_, parent)) = parts.split_last() {
        ensure_dirs(root, &parent.join("/"))?;
    }
    let path = check_chain(root, rel)?;
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .open(&path);
    let mut file = match result {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(open_error(&e)),
    };
    file.write_all(contents.as_bytes())
        .map_err(|_| SafeFsError::Io)?;
    Ok(true)
}

/// Garante que `root/.git` é diretório real (não symlink, não arquivo).
pub fn require_git_dir(root: &Path) -> Result<(), SafeFsError> {
    let meta = fs::symlink_metadata(root.join(".git")).map_err(|e| meta_error(&e))?;
    if meta.file_type().is_symlink() {
        return Err(SafeFsError::Unsafe);
    }
    if meta.file_type().is_dir() {
        Ok(())
    } else {
        Err(SafeFsError::NotRegular)
    }
}

/// Acrescenta `line` a `rel` (ex.: `.git/info/exclude`) se nenhuma linha for igual.
/// Exige `.git` diretório real; cria `info/` e o arquivo quando ausentes.
/// `Ok(true)` se acrescentou; `Ok(false)` se a linha já existia.
pub fn append_line_once(root: &Path, rel: &str, line: &str) -> Result<bool, SafeFsError> {
    require_git_dir(root)?;
    let parts = split_rel(rel)?;
    if let Some((_, parent)) = parts.split_last() {
        ensure_dirs(root, &parent.join("/"))?;
    }
    let text = match read_regular(root, rel, MAX_READ_BYTES) {
        Ok(text) => text,
        Err(SafeFsError::Missing) => String::new(),
        Err(e) => return Err(e),
    };
    if text.lines().any(|l| l.trim() == line) {
        return Ok(false);
    }
    let path = check_chain(root, rel)?;
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(&path)
        .map_err(|e| open_error(&e))?;
    if !file.metadata().map_err(|_| SafeFsError::Io)?.is_file() {
        return Err(SafeFsError::NotRegular);
    }
    let mut out = String::new();
    if !text.is_empty() && !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(line);
    out.push('\n');
    file.write_all(out.as_bytes()).map_err(|_| SafeFsError::Io)?;
    Ok(true)
}

/// Acrescenta `line` (+ `\n`) a `rel` sem criar diretórios. Se o arquivo já passa de
/// `max_bytes` (ou passaria), não grava e devolve `Ok(false)`. `Ok(true)` quando gravou.
pub fn append_bounded(root: &Path, rel: &str, line: &str, max_bytes: u64) -> Result<bool, SafeFsError> {
    let path = check_chain(root, rel)?;
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(&path)
        .map_err(|e| open_error(&e))?;
    let meta = file.metadata().map_err(|_| SafeFsError::Io)?;
    if !meta.is_file() {
        return Err(SafeFsError::NotRegular);
    }
    if meta.len() + line.len() as u64 + 1 > max_bytes {
        return Ok(false);
    }
    let mut out = String::with_capacity(line.len() + 1);
    out.push_str(line);
    out.push('\n');
    file.write_all(out.as_bytes()).map_err(|_| SafeFsError::Io)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use std::{env, process};

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("nants-safefs-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn check_chain_rejects_symlink_in_any_component() {
        let root = temp_dir("chain");
        let outside = temp_dir("chain-out");
        fs::create_dir_all(outside.join("x")).expect("x");
        symlink(&outside, root.join(".sdd")).expect("ln");
        assert_eq!(check_chain(&root, ".sdd/settings.yaml"), Err(SafeFsError::Unsafe));
        assert_eq!(check_chain(&root, ".sdd"), Err(SafeFsError::Unsafe));
        assert!(check_chain(&root, "nada/aqui.md").is_ok(), "ausente é ok");
        assert_eq!(check_chain(&root, "../fora"), Err(SafeFsError::Unsafe));
        assert_eq!(check_chain(&root, "/abs"), Err(SafeFsError::Unsafe));
    }

    #[test]
    fn file_as_intermediate_component_is_not_io_error() {
        let root = temp_dir("enotdir");
        fs::write(root.join("arq"), "x").expect("w");
        assert_eq!(check_chain(&root, "arq/filho.yaml"), Err(SafeFsError::NotRegular));
        assert_eq!(read_regular(&root, "arq/filho.yaml", 10), Err(SafeFsError::NotRegular));
        assert_eq!(ensure_dirs(&root, "arq/specs"), Err(SafeFsError::NotRegular));
    }

    #[test]
    fn dangling_symlink_is_unsafe_not_missing() {
        let root = temp_dir("dangling");
        fs::create_dir_all(root.join(".sdd")).expect("sdd");
        let target = root.join("alvo-inexistente");
        symlink(&target, root.join(".sdd/settings.yaml")).expect("ln");
        assert_eq!(
            check_chain(&root, ".sdd/settings.yaml"),
            Err(SafeFsError::Unsafe)
        );
        assert_eq!(
            write_new(&root, ".sdd/settings.yaml", "x"),
            Err(SafeFsError::Unsafe)
        );
        assert!(!target.exists(), "alvo não pode ser criado");
    }

    #[test]
    fn read_regular_reads_small_files_and_rejects_large_or_symlink() {
        let root = temp_dir("read");
        fs::write(root.join("a.txt"), "olá").expect("w");
        assert_eq!(read_regular(&root, "a.txt", 100).as_deref(), Ok("olá"));
        fs::write(root.join("big.txt"), "x".repeat(11)).expect("w");
        assert_eq!(read_regular(&root, "big.txt", 10), Err(SafeFsError::TooLarge));
        symlink(root.join("a.txt"), root.join("ln.txt")).expect("ln");
        assert_eq!(read_regular(&root, "ln.txt", 100), Err(SafeFsError::Unsafe));
        fs::create_dir_all(root.join("dir")).expect("d");
        assert_eq!(read_regular(&root, "dir", 100), Err(SafeFsError::NotRegular));
        assert_eq!(read_regular(&root, "nada.txt", 100), Err(SafeFsError::Missing));
    }

    #[test]
    fn read_regular_never_blocks_on_fifo() {
        let root = temp_dir("fifo-read");
        let fifo = root.join("pipe");
        let status = Command::new("mkfifo").arg(&fifo).status();
        if !status.is_ok_and(|s| s.success()) {
            return; // mkfifo indisponível neste ambiente
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let path = root.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_regular(&path, "pipe", 100));
        });
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("não deve bloquear");
        assert_eq!(got, Err(SafeFsError::NotRegular));
    }

    #[test]
    fn write_new_never_overwrites_and_creates_dirs_per_component() {
        let root = temp_dir("write");
        assert_eq!(write_new(&root, ".sdd/specs/a.yaml", "um"), Ok(true));
        assert_eq!(write_new(&root, ".sdd/specs/a.yaml", "dois"), Ok(false));
        assert_eq!(
            fs::read_to_string(root.join(".sdd/specs/a.yaml")).expect("r"),
            "um"
        );
    }

    #[test]
    fn ensure_dirs_refuses_symlinked_prefix_without_creating_outside() {
        let root = temp_dir("ensure");
        let outside = temp_dir("ensure-out");
        symlink(&outside, root.join(".sdd")).expect("ln");
        assert_eq!(ensure_dirs(&root, ".sdd/specs"), Err(SafeFsError::Unsafe));
        assert!(!outside.join("specs").exists());
    }

    #[test]
    fn append_line_once_is_idempotent_and_refuses_symlink_target() {
        let root = temp_dir("append");
        Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(&root)
            .status()
            .expect("init");
        assert_eq!(
            append_line_once(&root, ".git/info/exclude", ".sdd/worktrees/"),
            Ok(true)
        );
        assert_eq!(
            append_line_once(&root, ".git/info/exclude", ".sdd/worktrees/"),
            Ok(false)
        );
        let text = fs::read_to_string(root.join(".git/info/exclude")).expect("r");
        assert_eq!(text.lines().filter(|l| *l == ".sdd/worktrees/").count(), 1);

        let target = temp_dir("append-target");
        let victim = target.join("alvo.txt");
        fs::write(&victim, "intacto").expect("v");
        fs::remove_file(root.join(".git/info/exclude")).expect("rm");
        symlink(&victim, root.join(".git/info/exclude")).expect("ln");
        assert!(append_line_once(&root, ".git/info/exclude", "x").is_err());
        assert_eq!(fs::read_to_string(&victim).expect("r"), "intacto");
    }

    #[test]
    fn append_line_once_refuses_git_symlink() {
        let root = temp_dir("gitln");
        let outside = temp_dir("gitln-out");
        fs::create_dir_all(outside.join("info")).expect("i");
        symlink(&outside, root.join(".git")).expect("ln");
        assert_eq!(
            append_line_once(&root, ".git/info/exclude", "x"),
            Err(SafeFsError::Unsafe)
        );
        assert!(!outside.join("info/exclude").exists());
    }
}
