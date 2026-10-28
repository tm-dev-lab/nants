//! Estado SDD versionável em `<repo>/.sdd/`: tipos, parser Markdown+YAML e carga.

pub mod capabilities;
pub mod coverage;
pub mod docker_command;
pub mod environment;
pub mod execution_order;
pub mod field_citations;
pub mod failure_artifacts;
pub mod formatting;
pub mod images;
pub mod reconciler;
pub mod plan_diff;
pub mod references;
pub mod role_guidance;
pub mod scope_warnings;
pub mod screenshots;
pub mod skills;
pub mod spec_sections;
pub mod test_levels;
mod parser;
mod schema;

use std::io;
use std::path::{Path, PathBuf};

pub use parser::{
    load_sdd, read_settings, read_spec, read_task, split_frontmatter, write_settings, write_spec,
    write_task,
};
pub use schema::{
    check_name_valid, normalize_formatter_glob, valid_optional_command, validate_lockfiles, MAX_LOCKFILES, normalize_workdir, validate_failure_artifacts, validate_formatters, Formatter, MAX_FAILURE_ARTIFACTS, MAX_FORMATTERS, DEFAULT_LIMIT, DEFAULT_REVIEW_DIFF_BUDGET, LIMIT_RANGE, check_run_url, check_test_command_shape, validate_checks, Check, Confidence, Environment, MAX_CHECKS, Finding, FindingState, Arbitration, ArbitrationDecision, Sdd, Settings, Spec, SpecMeta, SpecStatus, Task, TaskMeta, TASK_KIND_FINAL_FIX,
    TaskStatus, TestCommandError, TestRun, Verdict, TEST_COMMAND_MAX_CHARS, check_run_command_shape, FieldIssue, RUN_COMMAND_MAX_CHARS,
};

/// Erros do estado SDD.
#[derive(Debug, thiserror::Error)]
pub enum SddError {
    #[error("erro de I/O em {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frontmatter ausente{} (esperado bloco YAML entre linhas '---')", at(.path))]
    MissingFrontmatter { path: Option<PathBuf> },
    #[error("YAML inválido{}: {source}", at(.path))]
    InvalidYaml {
        path: Option<PathBuf>,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("erro ao serializar YAML: {0}")]
    Serialize(#[source] serde_yaml::Error),
    #[error("repositório sem .sdd inicializado em {}", .0.display())]
    NotInitialized(PathBuf),
    #[error("test_command inválido na spec: uma linha, até 200 caracteres, sem caracteres de controle")]
    InvalidTestCommand,
    #[error("checks inválidos no settings: até 8, nome [a-z0-9_-]{{1,32}} único, comando de uma linha")]
    InvalidChecks,
}

/// Sufixo de localização para mensagens de erro de YAML.
fn at(path: &Option<PathBuf>) -> String {
    match path {
        Some(p) => format!(" em {}", p.display()),
        None => String::new(),
    }
}

/// Caminho do `.sdd` de um repositório.
pub fn sdd_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".sdd")
}
