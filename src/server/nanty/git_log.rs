//! Histórico do Git para o Nanty (spec 140 T09, C-10): a base da ferramenta da T10.
//!
//! O repositório é escrito por agentes e lido sem ninguém olhando, então a configuração dele não
//! pode rodar programa nenhum:
//! - **Ambiente limpo:** o processo nasce sem as variáveis herdadas (nenhum `GIT_*`, nem
//!   `GIT_CONFIG_COUNT`/`GIT_CONFIG_PARAMETERS`, nem `PAGER`), sem a configuração do sistema
//!   (`GIT_CONFIG_NOSYSTEM`) e sem a global (`GIT_CONFIG_GLOBAL=/dev/null`). `GIT_CEILING_DIRECTORIES`
//!   impede subir para um repositório acima da raiz, e `GIT_OPTIONAL_LOCKS=0` evita gravar o índice.
//! - **Configuração local neutralizada** onde ela executa algo: `--no-pager`, `core.pager`,
//!   `core.fsmonitor`, `core.hooksPath`, `diff.external` vazio, `log.showSignature=false`,
//!   `--no-ext-diff`, `--no-textconv` e `--no-show-signature`.
//! - **`safe.directory`** não é tocado: um repositório de outro dono continua recusado pelo Git.
//! - **Só o resumo:** hash curto, autor sem e-mail, data e estatística (`--shortstat`). Sem `-p`.
//! - **Tetos:** `-n` limitado e prazo (`[nanty.read]`); o processo morre no prazo. A saída acima de
//!   1 MiB é cortada (o `git` é encerrado), nunca vira prazo estourado. `GIT_NO_LAZY_FETCH` impede
//!   buscar objeto faltante na rede.
//! - **Só repositório do projeto:** `.git` link, ou arquivo `gitdir:` que aponta para fora da raiz,
//!   é recusado.
//! - **Texto do Git** (assunto, autor) passa pelo redator e vai no envelope de dado não confiável.
//!   Os erros são códigos, sem caminho.

use std::path::Path;
use std::process::Stdio;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use super::files::{envelope, Root};
use super::redact;
use crate::config::NantyRead;
use crate::server::events::SecretSet;

/// Maior saída lida do `git`, em bytes.
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
/// Teto do assunto do commit, em caracteres.
const SUBJECT_MAX_CHARS: usize = 200;
/// Teto do nome do autor, em caracteres.
const AUTHOR_MAX_CHARS: usize = 80;
/// Separador de commit (três NUL: nenhum campo vazio fica entre dois outros) e de campo no formato
/// pedido (NUL não aparece no texto do commit).
const FIELD: char = '\0';
const RECORD: &str = "\0\0\0";
/// Maior arquivo `.git` (`gitdir:`) aceito, em bytes.
const GITDIR_FILE_MAX_BYTES: u64 = 4096;

/// Por que a leitura do Git não aconteceu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitReadError {
    /// A raiz não tem `.git`.
    NotRepo,
    /// O `.git` é link.
    Symlink,
    /// O `.git` é um arquivo `gitdir:` que aponta para fora do projeto.
    Outside,
    /// O prazo acabou.
    Timeout,
    /// O `git` falhou (repositório vazio, de outro dono, corrompido).
    Failed,
    /// Não deu para rodar o `git`.
    Io,
}

impl GitReadError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotRepo => "nanty_git_not_repo",
            Self::Symlink => "nanty_git_symlink",
            Self::Outside => "nanty_git_outside",
            Self::Timeout => "nanty_git_timeout",
            Self::Failed => "nanty_git_failed",
            Self::Io => "nanty_git_io",
        }
    }
}

/// Um commit, já redigido. Sem `Debug`: o texto do Git não vai ao registro.
pub struct Commit {
    pub hash: String,
    pub author: String,
    /// Instante do commit (epoch, segundos).
    pub committed_at: u64,
    pub subject: String,
    pub files: u64,
    pub insertions: u64,
    pub deletions: u64,
}

impl Commit {
    pub fn to_json(&self) -> Value {
        json!({
            "hash": self.hash,
            "author": self.author,
            "committed_at": self.committed_at,
            "subject": self.subject,
            "files": self.files,
            "insertions": self.insertions,
            "deletions": self.deletions,
        })
    }
}

/// Os commits no envelope de dado não confiável.
pub fn untrusted(commits: &[Commit]) -> String {
    envelope(&json!({ "commits": commits.iter().map(Commit::to_json).collect::<Vec<_>>() }))
}

/// Os argumentos do `git log`, com `n` já limitado pelo teto.
pub(crate) fn log_args(n: usize) -> Vec<String> {
    let mut args: Vec<String> = vec!["--no-pager".into()];
    for setting in [
        "core.pager=cat",
        "core.fsmonitor=false",
        "core.hooksPath=/dev/null",
        "diff.external=",
        "log.showSignature=false",
        "color.ui=false",
        "log.decorate=false",
    ] {
        args.push("-c".into());
        args.push(setting.into());
    }
    args.extend(
        [
            "log",
            "-n",
            &n.to_string(),
            "--no-ext-diff",
            "--no-textconv",
            "--no-show-signature",
            "--no-color",
            "--no-decorate",
            "--shortstat",
            "--format=%x00%x00%x00%h%x00%an%x00%ct%x00%s",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args
}

/// Os últimos `n` commits (até o teto), redigidos.
pub async fn log(root: &Root, limits: &NantyRead, secrets: &SecretSet, n: usize) -> Result<Vec<Commit>, GitReadError> {
    log_capped(root, limits, secrets, n, MAX_OUTPUT_BYTES).await
}

/// Como [`log`], com o teto da saída à parte (os testes usam um pequeno). Saída acima do teto é
/// cortada: o `git` é encerrado e o último registro, talvez pela metade, sai.
async fn log_capped(root: &Root, limits: &NantyRead, secrets: &SecretSet, n: usize, cap: u64) -> Result<Vec<Commit>, GitReadError> {
    let dir = root.path();
    check_git_dir(dir)?;
    let n = n.clamp(1, limits.git_commits());
    let mut cmd = command(dir);
    cmd.args(log_args(n));
    let mut child = cmd.spawn().map_err(|_| GitReadError::Io)?;
    let mut stdout = child.stdout.take().ok_or(GitReadError::Io)?;
    let run = async {
        let mut out = Vec::new();
        (&mut stdout).take(cap).read_to_end(&mut out).await.map_err(|_| GitReadError::Io)?;
        let cut = out.len() as u64 >= cap;
        // Cortada: o resto não é lido, então o `git` não pode ficar preso no cano cheio.
        drop(stdout);
        if cut {
            let _ = child.start_kill();
        }
        let status = child.wait().await.map_err(|_| GitReadError::Io)?;
        Ok::<_, GitReadError>((status, out, cut))
    };
    let (status, out, cut) = tokio::time::timeout(limits.git_timeout(), run).await.map_err(|_| GitReadError::Timeout)??;
    let text = String::from_utf8_lossy(&out);
    if cut {
        let whole = text.rfind(RECORD).map_or("", |at| &text[..at]);
        return Ok(parse(whole, secrets));
    }
    if !status.success() {
        return Err(GitReadError::Failed);
    }
    Ok(parse(&text, secrets))
}

/// O `.git` da raiz: pasta, ou arquivo `gitdir:` que aponta para dentro do projeto. Link, ausência
/// e `gitdir:` para fora são recusados.
fn check_git_dir(dir: &Path) -> Result<(), GitReadError> {
    let dot = dir.join(".git");
    let meta = std::fs::symlink_metadata(&dot).map_err(|_| GitReadError::NotRepo)?;
    if meta.file_type().is_symlink() {
        return Err(GitReadError::Symlink);
    }
    if meta.is_dir() {
        return Ok(());
    }
    if !meta.is_file() || meta.len() > GITDIR_FILE_MAX_BYTES {
        return Err(GitReadError::NotRepo);
    }
    let text = std::fs::read_to_string(&dot).map_err(|_| GitReadError::NotRepo)?;
    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:")).map(str::trim).filter(|t| !t.is_empty()).ok_or(GitReadError::NotRepo)?;
    let resolved = dir.join(target).canonicalize().map_err(|_| GitReadError::NotRepo)?;
    if resolved.starts_with(dir) && resolved != dir {
        Ok(())
    } else {
        Err(GitReadError::Outside)
    }
}

/// O `git` com o ambiente limpo, na raiz, sem entrada nem saída de erro, e morto se largado.
fn command(dir: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    cmd.env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_PAGER", "cat");
    if let Some(parent) = dir.parent() {
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    cmd.current_dir(dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    cmd
}

/// Lê a saída do formato de [`log_args`]: registros abertos por três NUL (os que sobram na ponta,
/// de um assunto vazio, saem), campos por um NUL, e a linha da estatística depois do assunto.
/// Registro malformado some.
fn parse(out: &str, secrets: &SecretSet) -> Vec<Commit> {
    out.split(RECORD)
        .filter_map(|record| {
            let mut fields = record.trim_start_matches(FIELD).splitn(4, FIELD);
            let hash = fields.next()?.trim();
            let author = fields.next()?;
            let committed_at = fields.next()?.trim().parse::<u64>().ok()?;
            let rest = fields.next().unwrap_or("");
            if hash.is_empty() || hash.len() > 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let (subject, stat) = rest.split_once('\n').unwrap_or((rest, ""));
            let (files, insertions, deletions) = shortstat(stat);
            Some(Commit {
                hash: hash.to_string(),
                author: one_line(&redact::scrub(secrets, author), AUTHOR_MAX_CHARS),
                committed_at,
                subject: one_line(&redact::scrub(secrets, subject), SUBJECT_MAX_CHARS),
                files,
                insertions,
                deletions,
            })
        })
        .collect()
}

/// ` 3 files changed, 10 insertions(+), 2 deletions(-)`, em qualquer ordem e com partes ausentes.
fn shortstat(text: &str) -> (u64, u64, u64) {
    let mut out = (0, 0, 0);
    let Some(line) = text.lines().find(|l| l.contains("changed")) else { return out };
    for part in line.split(',') {
        let mut words = part.split_whitespace();
        let Some(count) = words.next().and_then(|w| w.parse::<u64>().ok()) else { continue };
        match words.next() {
            Some(w) if w.starts_with("file") => out.0 = count,
            Some(w) if w.starts_with("insertion") => out.1 = count,
            Some(w) if w.starts_with("deletion") => out.2 = count,
            _ => {}
        }
    }
    out
}

/// Uma linha, sem controle, até `max` caracteres (o corte vem depois do redator).
fn one_line(text: &str, max: usize) -> String {
    let joined = text.split(|c: char| c.is_whitespace() || c.is_control()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
    joined.chars().take(max).collect()
}

#[cfg(test)]
#[path = "git_log_tests.rs"]
mod tests;
