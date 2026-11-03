//! Leitor de arquivos próprio do Nanty (spec 140 T09, C-08): a base das ferramentas de leitura
//! profunda da T10.
//!
//! - **Raiz:** a pasta cadastrada do projeto, que pode ser um link. Ela é resolvida uma vez
//!   ([`Root::open`]) e fica aberta num descritor; tudo abaixo é aberto a partir dele, nunca por
//!   caminho do SO. Só projetos observados ([`Root::for_project`]).
//! - **Abertura:** `openat2` com `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`.
//!   Sem `openat2` no kernel (ou barrado por seccomp), uma caminhada componente a componente por
//!   `openat` com `O_NOFOLLOW` (e `O_DIRECTORY` nas pastas), sempre do descritor da pasta anterior.
//!   Os dois recusam link em qualquer componente no próprio ato de abrir: não existe janela entre
//!   conferir e abrir, e trocar uma pasta por um link no meio do caminho só faz a abertura falhar.
//! - **Caminho:** relativo, com `/`; `..`, caminho absoluto, NUL e caracteres de controle são
//!   recusados antes de tocar no disco.
//! - **Bloqueio:** por nome em qualquer componente ([`is_secret_name`]) e por pasta
//!   ([`BLOCKED_DIRS`] e `.sdd/kb/sources`). Na listagem, o que está bloqueado nem aparece.
//! - **Conteúdo:** só arquivo regular, até o teto por arquivo; chave privada ([`has_private_key`]) e
//!   binário (NUL ou UTF-8 inválido) são recusados. O texto passa pelo redator ([`redact::scrub`])
//!   antes da faixa de linhas e do corte.
//! - **Tetos** (`[nanty.read]`, provisórios): por arquivo, por turno ([`Budget`]) e por listagem, com
//!   leitura por faixa de linhas.
//! - **Envelope:** o nome do arquivo vai sempre dentro do envelope de dado não confiável
//!   ([`envelope`]), junto do conteúdo. Os erros são códigos, sem caminho, e nada aqui escreve no
//!   registro.
//!
//! **Outros sistemas:** o Nants publica só para Linux (`RELEASE.md`). Fora do Linux, [`Root::open`]
//! recusa com `nanty_read_unsupported`.
//!
//! As funções bloqueiam (disco): quem está num laço assíncrono chama por `spawn_blocking`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::redact;
use super::tools::{seal, UNTRUSTED_CLOSE, UNTRUSTED_OPEN};
use crate::config::{Config, NantyRead};
use crate::kb::{has_private_key, is_secret_name};
use crate::server::events::SecretSet;

/// Pastas que o leitor nunca abre nem lista, em qualquer nível (sem diferenciar caixa).
pub const BLOCKED_DIRS: &[&str] = &[".git", ".ssh", ".aws", ".docker", ".kube", "secrets", "node_modules", "target", "dist"];
/// Caminhos bloqueados a partir da raiz do projeto (as fontes da base de conhecimento).
const BLOCKED_PREFIXES: &[&[&str]] = &[&[".sdd", "kb", "sources"]];
/// Maior caminho relativo aceito, em bytes.
const MAX_REL_BYTES: usize = 1024;
/// Mais componentes aceitos num caminho.
const MAX_DEPTH: usize = 32;
/// Maior nome de entrada mostrado, em caracteres.
const MAX_NAME_CHARS: usize = 255;

/// Por que a leitura não aconteceu. Nunca carrega caminho.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Caminho inválido: vazio, absoluto, com `..`, NUL, controle ou longo demais.
    BadPath,
    /// Link simbólico em algum componente.
    Symlink,
    /// Pasta bloqueada (`.git`, `.ssh`, `node_modules` e as outras).
    Blocked,
    /// Nome com cara de segredo.
    Secret,
    /// O arquivo traz uma chave privada.
    PrivateKey,
    /// Conteúdo binário.
    Binary,
    /// Arquivo acima do teto.
    TooLarge,
    /// Não existe.
    Missing,
    /// Não é arquivo regular.
    NotFile,
    /// Um componente não é pasta.
    NotDir,
    /// O teto do turno acabou.
    Budget,
    /// Projeto desconhecido ou fora da observação do Nanty.
    NotObserved,
    /// Sistema sem o leitor seguro.
    Unsupported,
    /// Outra falha de I/O.
    Io,
}

impl ReadError {
    /// O código estável do erro.
    pub fn code(self) -> &'static str {
        match self {
            Self::BadPath => "nanty_read_bad_path",
            Self::Symlink => "nanty_read_symlink",
            Self::Blocked => "nanty_read_blocked",
            Self::Secret => "nanty_read_secret",
            Self::PrivateKey => "nanty_read_private_key",
            Self::Binary => "nanty_read_binary",
            Self::TooLarge => "nanty_read_too_large",
            Self::Missing => "nanty_read_missing",
            Self::NotFile => "nanty_read_not_file",
            Self::NotDir => "nanty_read_not_dir",
            Self::Budget => "nanty_read_budget",
            Self::NotObserved => "nanty_read_not_observed",
            Self::Unsupported => "nanty_read_unsupported",
            Self::Io => "nanty_read_io",
        }
    }
}

/// O que ainda pode ser lido neste turno, em bytes de texto devolvido (nomes e conteúdo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    left: u64,
}

impl Budget {
    /// O teto de um turno novo, pela configuração.
    pub fn new(limits: &NantyRead) -> Self {
        Self { left: limits.turn_bytes() }
    }

    /// Bytes que ainda cabem.
    pub fn left(&self) -> u64 {
        self.left
    }

    /// Desconta o que foi devolvido (spec 140 T10: as ferramentas que montam o resultado à parte).
    pub(crate) fn spend(&mut self, bytes: usize) {
        self.left = self.left.saturating_sub(bytes as u64);
    }
}

/// Faixa de linhas pedida: a primeira (a partir de 1) e quantas. Sem `count`, até o teto.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineRange {
    pub start: usize,
    pub count: Option<usize>,
}

/// O texto lido de um arquivo. Sem `Debug` de propósito: o nome e o conteúdo não vão ao registro.
pub struct FileText {
    /// O caminho relativo pedido, redigido.
    pub name: String,
    /// Primeira e última linha devolvidas (a partir de 1; `0` e `0` no arquivo vazio).
    pub first_line: usize,
    pub last_line: usize,
    pub total_lines: usize,
    /// O texto das linhas devolvidas, redigido.
    pub text: String,
    /// `true` se o teto (de linhas ou do turno) cortou antes do fim da faixa pedida.
    pub truncated: bool,
}

impl FileText {
    pub fn to_json(&self) -> Value {
        json!({
            "file": self.name,
            "first_line": self.first_line,
            "last_line": self.last_line,
            "total_lines": self.total_lines,
            "truncated": self.truncated,
            "text": self.text,
        })
    }

    /// No envelope de dado não confiável, com o nome dentro.
    pub fn untrusted(&self) -> String {
        envelope(&self.to_json())
    }
}

/// O fim de um arquivo, lido por [`Root::tail`]. Sem `Debug`, como [`FileText`].
pub(crate) struct Tail {
    pub text: String,
    pub from_start: bool,
}

/// Tipo de uma entrada da listagem. Link aparece como link e nunca é seguido.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Link,
    Other,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::File => "file",
            Self::Link => "link",
            Self::Other => "other",
        }
    }
}

/// Uma entrada da listagem. Sem `Debug`, como [`FileText`].
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// Tamanho, só nos arquivos regulares.
    pub bytes: Option<u64>,
}

/// Uma pasta listada. Sem `Debug`, como [`FileText`].
pub struct Listing {
    /// O caminho relativo pedido (vazio na raiz), redigido.
    pub name: String,
    /// Entradas visíveis, em ordem de nome, até o teto.
    pub entries: Vec<Entry>,
    /// Entradas que o bloqueio escondeu.
    pub hidden: usize,
    /// `true` se o teto (da listagem ou do turno) cortou.
    pub truncated: bool,
}

impl Listing {
    pub fn to_json(&self) -> Value {
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|e| json!({ "name": e.name, "kind": e.kind.as_str(), "bytes": e.bytes }))
            .collect();
        json!({ "dir": self.name, "entries": entries, "hidden": self.hidden, "truncated": self.truncated })
    }

    /// No envelope de dado não confiável, com os nomes dentro.
    pub fn untrusted(&self) -> String {
        envelope(&self.to_json())
    }
}

/// O JSON dentro do envelope de dado não confiável, selado ([`seal`]).
pub fn envelope(value: &Value) -> String {
    let body = seal(&serde_json::to_string(value).unwrap_or_default());
    format!("{UNTRUSTED_OPEN}\n{body}\n{UNTRUSTED_CLOSE}")
}

/// Como abrir abaixo da raiz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Strategy {
    /// `openat2` e, se o kernel recusar a chamada, a caminhada.
    Auto,
    /// Só a caminhada por `openat` (os testes cobrem os dois caminhos).
    Walk,
}

/// A raiz de um projeto, resolvida uma vez e aberta. Sem `Debug`: o caminho não vai ao registro.
pub struct Root {
    dir: sys::Dir,
    path: PathBuf,
    strategy: Strategy,
}

impl Root {
    /// Abre a raiz cadastrada. Ela pode ser um link: é resolvida aqui, uma vez, e o que vem abaixo
    /// é aberto a partir do descritor.
    pub fn open(path: &Path) -> Result<Self, ReadError> {
        let canonical = path.canonicalize().map_err(|e| io_error(&e))?;
        let dir = sys::open_root(&canonical)?;
        Ok(Self { dir, path: canonical, strategy: Strategy::Auto })
    }

    /// A raiz de um projeto observado pelo Nanty; qualquer outro id é recusado.
    pub fn for_project(cfg: &Config, project_id: &str) -> Result<Self, ReadError> {
        let project = cfg
            .projects
            .iter()
            .find(|p| p.id == project_id && cfg.nanty.observes(&p.id))
            .ok_or(ReadError::NotObserved)?;
        Self::open(Path::new(&project.path))
    }

    /// O caminho resolvido da raiz (para o Git). Nunca vai ao modelo nem ao registro.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn walking(mut self) -> Self {
        self.strategy = Strategy::Walk;
        self
    }

    /// O texto inteiro de `rel`, ainda sem o redator, até o teto por arquivo, com as mesmas recusas
    /// de [`Self::read`] e sem gastar o teto do turno (spec 140 T10). Serve a quem interpreta o arquivo
    /// (o YAML de uma tarefa) e devolve só alguns campos: quem devolve redige cada campo inteiro antes de
    /// cortar e paga no [`Budget`] o que devolver.
    pub(crate) fn read_raw(&self, rel: &str, limits: &NantyRead) -> Result<String, ReadError> {
        let parts = parse_rel(rel, false)?;
        check_policy(&parts)?;
        let bytes = sys::read_file(&self.dir, &parts, self.strategy, limits.file_bytes())?;
        if has_private_key(&bytes) {
            return Err(ReadError::PrivateKey);
        }
        if bytes.contains(&0) {
            return Err(ReadError::Binary);
        }
        String::from_utf8(bytes).map_err(|_| ReadError::Binary)
    }

    /// Os últimos `want` bytes de `rel` (spec 140 T10: os registros JSONL dos agentes, lidos de trás
    /// para a frente), pelo mesmo caminho seguro, sem o redator e sem gastar o teto do turno. `from_start`
    /// diz se a leitura alcançou o começo do arquivo; senão, a primeira linha pode estar cortada. Quem
    /// usa redige cada campo inteiro antes de cortar e paga o que devolver.
    pub(crate) fn tail(&self, rel: &str, want: u64) -> Result<Tail, ReadError> {
        let parts = parse_rel(rel, false)?;
        check_policy(&parts)?;
        let (bytes, from_start) = sys::read_tail(&self.dir, &parts, self.strategy, want)?;
        if bytes.contains(&0) {
            return Err(ReadError::Binary);
        }
        Ok(Tail { text: String::from_utf8_lossy(&bytes).into_owned(), from_start })
    }

    /// Lê `rel` dentro do teto do arquivo, da faixa e do turno.
    pub fn read(&self, rel: &str, range: LineRange, limits: &NantyRead, secrets: &SecretSet, budget: &mut Budget) -> Result<FileText, ReadError> {
        let parts = parse_rel(rel, false)?;
        check_policy(&parts)?;
        if budget.left == 0 {
            return Err(ReadError::Budget);
        }
        let text = self.read_raw(rel, limits)?;
        // O redator vem antes da faixa e do corte, para nunca sobrar um pedaço de segredo.
        let clean = redact::scrub(secrets, &text);
        let name = redact::scrub(secrets, &parts.join("/"));
        let lines: Vec<&str> = clean.lines().collect();
        let total = lines.len();
        let start = range.start.max(1);
        let want = range.count.unwrap_or(usize::MAX).min(limits.range_lines()).max(1);
        let mut room = (budget.left as usize).saturating_sub(name.len());
        let mut out = String::new();
        let mut taken = 0usize;
        // A faixa passou do teto de linhas e o arquivo continua depois dela.
        let mut truncated = range.count.is_none_or(|c| c > want) && (start - 1).saturating_add(want) < total;
        for line in lines.iter().skip(start - 1).take(want) {
            let need = line.len() + usize::from(taken > 0);
            if need > room {
                if taken == 0 && room > 0 {
                    // Nem a primeira linha cabe: vai o pedaço que cabe, cortado em fronteira de caractere.
                    let mut cut = room.min(line.len());
                    while !line.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    out.push_str(&line[..cut]);
                    taken = 1;
                }
                truncated = true;
                break;
            }
            if taken > 0 {
                out.push('\n');
            }
            out.push_str(line);
            room -= need;
            taken += 1;
        }
        if taken == 0 && start <= total {
            return Err(ReadError::Budget);
        }
        budget.spend(name.len() + out.len());
        let (first, last) = if taken == 0 { (0, 0) } else { (start, start + taken - 1) };
        Ok(FileText { name, first_line: first, last_line: last, total_lines: total, text: out, truncated })
    }

    /// Lista `rel` (vazio ou `.` = a raiz), sem o que está bloqueado, até o teto da listagem e do turno.
    pub fn list(&self, rel: &str, limits: &NantyRead, secrets: &SecretSet, budget: &mut Budget) -> Result<Listing, ReadError> {
        let parts = parse_rel(rel, true)?;
        check_policy(&parts)?;
        if budget.left == 0 {
            return Err(ReadError::Budget);
        }
        let raw = sys::list_dir(&self.dir, &parts, self.strategy)?;
        let name = redact::scrub(secrets, &parts.join("/"));
        let mut room = (budget.left as usize).saturating_sub(name.len());
        let mut hidden = 0;
        let mut visible: Vec<(String, EntryKind, Option<u64>)> = Vec::new();
        for (entry, kind, bytes) in raw {
            let Some(entry) = entry.filter(|n| shown(&parts, n)) else {
                hidden += 1;
                continue;
            };
            visible.push((entry, kind, bytes));
        }
        visible.sort_by(|a, b| a.0.cmp(&b.0));
        let mut truncated = visible.len() > limits.list_entries();
        let mut entries = Vec::new();
        for (entry, kind, bytes) in visible.into_iter().take(limits.list_entries()) {
            let entry = redact::scrub(secrets, &entry);
            // Nome, tipo e tamanho: uma estimativa curta do que a entrada ocupa no JSON.
            let need = entry.len() + 32;
            if need > room {
                truncated = true;
                break;
            }
            room -= need;
            entries.push(Entry { name: entry, kind, bytes });
        }
        if entries.is_empty() && truncated {
            return Err(ReadError::Budget);
        }
        budget.spend((budget.left as usize).saturating_sub(room));
        Ok(Listing { name, entries, hidden, truncated })
    }
}

/// O nome entra na listagem: legível, de uma linha e fora do bloqueio.
fn shown(parent: &[String], name: &str) -> bool {
    if name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return false;
    }
    let mut parts = parent.to_vec();
    parts.push(name.to_string());
    check_policy(&parts).is_ok()
}

/// Separa `rel` em componentes. `.` e barras repetidas somem; `..`, absoluto, NUL, controle,
/// barra invertida e caminho longo demais são recusados. Vazio só vale para a raiz (`allow_root`).
pub(crate) fn parse_rel(rel: &str, allow_root: bool) -> Result<Vec<String>, ReadError> {
    if rel.len() > MAX_REL_BYTES || rel.starts_with('/') || rel.chars().any(|c| c.is_control() || c == '\\') {
        return Err(ReadError::BadPath);
    }
    let mut parts = Vec::new();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(ReadError::BadPath),
            p => parts.push(p.to_string()),
        }
    }
    if parts.len() > MAX_DEPTH || (parts.is_empty() && !allow_root) {
        return Err(ReadError::BadPath);
    }
    Ok(parts)
}

/// Recusa por pasta e por nome, em qualquer componente.
pub(crate) fn check_policy(parts: &[String]) -> Result<(), ReadError> {
    let lower: Vec<String> = parts.iter().map(|p| p.to_ascii_lowercase()).collect();
    if lower.iter().any(|p| BLOCKED_DIRS.contains(&p.as_str())) {
        return Err(ReadError::Blocked);
    }
    if BLOCKED_PREFIXES.iter().any(|prefix| lower.len() >= prefix.len() && prefix.iter().zip(&lower).all(|(a, b)| a == b)) {
        return Err(ReadError::Blocked);
    }
    if parts.iter().any(|p| is_secret_name(p)) {
        return Err(ReadError::Secret);
    }
    Ok(())
}

fn io_error(e: &std::io::Error) -> ReadError {
    if e.kind() == std::io::ErrorKind::NotFound {
        ReadError::Missing
    } else {
        ReadError::Io
    }
}

/// A pasta `rel` abaixo de `root`, aberta pelo mesmo mecanismo do leitor (`openat2` sem link, ou a
/// caminhada por `openat` com `O_NOFOLLOW`), sem o bloqueio por nome. Serve a quem precisa agir
/// dentro de uma árvore escrita por outro processo sem seguir link nenhum (spec 140 T09: o
/// afastamento do `.git` aninhado, em `harness::git`). A raiz é resolvida uma vez.
#[cfg(target_os = "linux")]
pub(crate) fn open_dir_beneath(root: &Path, rel: &str) -> Result<std::os::fd::OwnedFd, ReadError> {
    let parts = parse_rel(rel, true)?;
    let canonical = root.canonicalize().map_err(|e| io_error(&e))?;
    sys::open_dir_beneath(&canonical, &parts)
}

#[cfg(target_os = "linux")]
mod sys {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{EntryKind, ReadError, Strategy};

    /// A pasta aberta.
    pub struct Dir(OwnedFd);

    /// `struct open_how` do `linux/openat2.h` (a do `libc` não pode ser montada fora dele).
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    /// `false` depois que o kernel recusou a chamada `openat2` (antigo ou barrado por seccomp).
    static OPENAT2: AtomicBool = AtomicBool::new(true);

    pub fn open_root(path: &Path) -> Result<Dir, ReadError> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| ReadError::BadPath)?;
        // O caminho já vem resolvido: o último componente não pode virar link agora.
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `c` é uma cadeia C válida durante a chamada.
        let fd = unsafe { libc::open(c.as_ptr(), flags) };
        owned(libc::c_long::from(fd)).map(Dir).map_err(errno_error)
    }

    /// O descritor devolvido por uma chamada, ou o `errno`.
    fn owned(fd: libc::c_long) -> Result<OwnedFd, i32> {
        if fd < 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        let fd = i32::try_from(fd).map_err(|_| libc::EBADF)?;
        // SAFETY: `fd` acabou de ser devolvido pelo kernel e é só nosso.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn errno_error(errno: i32) -> ReadError {
        match errno {
            libc::ELOOP => ReadError::Symlink,
            libc::ENOENT => ReadError::Missing,
            libc::ENOTDIR => ReadError::NotDir,
            libc::EXDEV | libc::ENAMETOOLONG => ReadError::BadPath,
            _ => ReadError::Io,
        }
    }

    /// Abre `parts` abaixo de `dir`, sem seguir link em nenhum componente.
    fn open_beneath(dir: &Dir, parts: &[String], want_dir: bool, strategy: Strategy) -> Result<OwnedFd, ReadError> {
        if parts.is_empty() {
            return dir.0.try_clone().map_err(|_| ReadError::Io);
        }
        let last = if want_dir { libc::O_DIRECTORY } else { libc::O_NONBLOCK };
        if strategy == Strategy::Auto && OPENAT2.load(Ordering::Relaxed) {
            match openat2(dir, parts, last) {
                Ok(fd) => return Ok(fd),
                Err(errno) if matches!(errno, libc::ENOSYS | libc::EPERM | libc::E2BIG | libc::EINVAL) => {
                    OPENAT2.store(false, Ordering::Relaxed);
                }
                Err(errno) => return Err(classify(dir, parts, errno)),
            }
        }
        walk(dir, parts, last).map_err(|errno| classify(dir, parts, errno))
    }

    fn openat2(dir: &Dir, parts: &[String], last: i32) -> Result<OwnedFd, i32> {
        let c = CString::new(parts.join("/")).map_err(|_| libc::EINVAL)?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | last;
        let how = OpenHow {
            flags: u64::try_from(flags).map_err(|_| libc::EINVAL)?,
            mode: 0,
            resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS,
        };
        // SAFETY: `c` e `how` vivem durante a chamada; o tamanho é o da estrutura passada.
        let fd = unsafe {
            libc::syscall(libc::SYS_openat2, dir.0.as_raw_fd(), c.as_ptr(), &how as *const OpenHow, std::mem::size_of::<OpenHow>())
        };
        owned(fd)
    }

    /// A caminhada: cada componente aberto do descritor do anterior, com `O_NOFOLLOW`.
    fn walk(dir: &Dir, parts: &[String], last: i32) -> Result<OwnedFd, i32> {
        let mut cur = dir.0.try_clone().map_err(|_| libc::EIO)?;
        for (at, part) in parts.iter().enumerate() {
            let c = CString::new(part.as_str()).map_err(|_| libc::EINVAL)?;
            let kind = if at + 1 == parts.len() { last } else { libc::O_DIRECTORY };
            let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | kind;
            // SAFETY: `cur` é um descritor aberto e `c` uma cadeia C válida durante a chamada.
            let fd = unsafe { libc::openat(cur.as_raw_fd(), c.as_ptr(), flags) };
            cur = owned(libc::c_long::from(fd))?;
        }
        Ok(cur)
    }

    /// O erro de abertura, com o `ENOTDIR` de um link (o `O_DIRECTORY` responde antes do
    /// `O_NOFOLLOW`) contado como link. Só classifica: nada é aberto aqui.
    fn classify(dir: &Dir, parts: &[String], errno: i32) -> ReadError {
        if errno == libc::ENOTDIR && any_link(dir, parts) {
            return ReadError::Symlink;
        }
        errno_error(errno)
    }

    /// `true` se algum componente alcançável é link (`fstatat` sem seguir, pasta a pasta).
    fn any_link(dir: &Dir, parts: &[String]) -> bool {
        let Ok(mut cur) = dir.0.try_clone() else { return false };
        for (at, part) in parts.iter().enumerate() {
            let Ok(c) = CString::new(part.as_str()) else { return false };
            let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: `cur` está aberto, `c` é válida e `st` tem o tamanho de `struct stat`.
            let rc = unsafe { libc::fstatat(cur.as_raw_fd(), c.as_ptr(), st.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
            if rc != 0 {
                return false;
            }
            // SAFETY: `fstatat` deu certo e preencheu `st`.
            let st = unsafe { st.assume_init() };
            if st.st_mode & libc::S_IFMT == libc::S_IFLNK {
                return true;
            }
            if at + 1 == parts.len() || st.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return false;
            }
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            // SAFETY: como acima.
            let fd = unsafe { libc::openat(cur.as_raw_fd(), c.as_ptr(), flags) };
            match owned(libc::c_long::from(fd)) {
                Ok(next) => cur = next,
                Err(_) => return false,
            }
        }
        false
    }

    pub fn open_dir_beneath(root: &Path, parts: &[String]) -> Result<OwnedFd, ReadError> {
        let dir = open_root(root)?;
        open_beneath(&dir, parts, true, Strategy::Auto)
    }

    pub fn read_file(dir: &Dir, parts: &[String], strategy: Strategy, max: u64) -> Result<Vec<u8>, ReadError> {
        let file = File::from(open_beneath(dir, parts, false, strategy)?);
        let meta = file.metadata().map_err(|_| ReadError::Io)?;
        if !meta.file_type().is_file() {
            return Err(ReadError::NotFile);
        }
        if meta.len() > max {
            return Err(ReadError::TooLarge);
        }
        let mut bytes = Vec::new();
        file.take(max + 1).read_to_end(&mut bytes).map_err(|_| ReadError::Io)?;
        if bytes.len() as u64 > max {
            return Err(ReadError::TooLarge);
        }
        Ok(bytes)
    }

    /// Os últimos `want` bytes do arquivo regular e se a leitura começou no início dele.
    pub fn read_tail(dir: &Dir, parts: &[String], strategy: Strategy, want: u64) -> Result<(Vec<u8>, bool), ReadError> {
        use std::io::{Seek, SeekFrom};
        let mut file = File::from(open_beneath(dir, parts, false, strategy)?);
        let meta = file.metadata().map_err(|_| ReadError::Io)?;
        if !meta.file_type().is_file() {
            return Err(ReadError::NotFile);
        }
        let len = meta.len();
        let start = len.saturating_sub(want);
        file.seek(SeekFrom::Start(start)).map_err(|_| ReadError::Io)?;
        let mut bytes = Vec::new();
        file.take(len - start).read_to_end(&mut bytes).map_err(|_| ReadError::Io)?;
        Ok((bytes, start == 0))
    }

    /// As entradas de uma pasta: nome (`None` se não for UTF-8), tipo sem seguir link e tamanho.
    pub fn list_dir(dir: &Dir, parts: &[String], strategy: Strategy) -> Result<Vec<(Option<String>, EntryKind, Option<u64>)>, ReadError> {
        let fd = open_beneath(dir, parts, true, strategy)?;
        // A pasta é lida pelo próprio descritor (o link mágico do `/proc` aponta para ele, não para
        // o caminho), e cada entrada é examinada sem seguir link.
        let read = std::fs::read_dir(format!("/proc/self/fd/{}", fd.as_raw_fd())).map_err(|_| ReadError::Io)?;
        let mut out = Vec::new();
        for entry in read {
            let entry = entry.map_err(|_| ReadError::Io)?;
            let name = entry.file_name().to_str().map(str::to_string);
            let (kind, bytes) = match entry.metadata() {
                Ok(m) if m.file_type().is_symlink() => (EntryKind::Link, None),
                Ok(m) if m.is_dir() => (EntryKind::Dir, None),
                Ok(m) if m.is_file() => (EntryKind::File, Some(m.len())),
                _ => (EntryKind::Other, None),
            };
            out.push((name, kind, bytes));
        }
        drop(fd);
        Ok(out)
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    //! Sem o leitor seguro fora do Linux: tudo é recusado com código.
    use std::path::Path;

    use super::{EntryKind, ReadError, Strategy};

    pub struct Dir;

    pub fn open_root(_path: &Path) -> Result<Dir, ReadError> {
        Err(ReadError::Unsupported)
    }

    pub fn read_file(_dir: &Dir, _parts: &[String], _strategy: Strategy, _max: u64) -> Result<Vec<u8>, ReadError> {
        Err(ReadError::Unsupported)
    }

    pub fn list_dir(_dir: &Dir, _parts: &[String], _strategy: Strategy) -> Result<Vec<(Option<String>, EntryKind, Option<u64>)>, ReadError> {
        Err(ReadError::Unsupported)
    }

    pub fn read_tail(_dir: &Dir, _parts: &[String], _strategy: Strategy, _want: u64) -> Result<(Vec<u8>, bool), ReadError> {
        Err(ReadError::Unsupported)
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
