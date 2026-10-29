//! Harness de agentes: wrappers sobre ferramentas externas (Git, worktrees).

pub mod allowlist;
pub mod browser;
pub mod compose;
pub mod git;
pub mod home;
pub mod limits;
pub mod progress;
pub mod web;
pub mod tools;
pub mod tree_snapshot;

use std::io;
use std::string::FromUtf8Error;

/// Erros ao executar o binário `git`.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("falha ao iniciar `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: io::Error,
    },
    #[error("`{command}` terminou com código {code:?}: {stderr}")]
    Failed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("argumento inválido para {name}: {value:?} (vazio ou começando com '-')")]
    InvalidArg { name: &'static str, value: String },
    #[error("saída não UTF-8 de `{command}`: {source}")]
    Utf8 {
        command: String,
        #[source]
        source: FromUtf8Error,
    },
}
