use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::schema::{check_run_url, check_test_command_shape, valid_optional_command, validate_checks, validate_failure_artifacts, validate_formatters, validate_lockfiles, Sdd, Settings, Spec, SpecMeta, Task};
use super::{sdd_dir, SddError};

const FENCE: &str = "---";

/// Separa o frontmatter `--- ... ---` do corpo. Retorna `(yaml, corpo)` como fatias do original.
pub fn split_frontmatter(content: &str) -> Result<(&str, &str), SddError> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or(SddError::MissingFrontmatter { path: None })?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == FENCE {
            return Ok((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err(SddError::MissingFrontmatter { path: None })
}

fn parse_doc<T: DeserializeOwned>(content: &str) -> Result<(T, String), SddError> {
    let (yaml, body) = split_frontmatter(content)?;
    let meta = serde_yaml::from_str(yaml).map_err(|source| SddError::InvalidYaml {
        path: None,
        source,
    })?;
    Ok((meta, body.to_string()))
}

fn render_doc<T: Serialize>(meta: &T, body: &str) -> Result<String, SddError> {
    let yaml = serde_yaml::to_string(meta).map_err(SddError::Serialize)?;
    Ok(format!("{FENCE}\n{yaml}{FENCE}\n{body}"))
}

impl Spec {
    /// Interpreta o conteúdo de um `spec.md`. `test_command` vazio é aceito (equivale a ausente);
    /// valor não vazio com forma inválida é recusado.
    pub fn parse(content: &str) -> Result<Self, SddError> {
        let (mut frontmatter, body): (SpecMeta, String) = parse_doc(content)?;
        if let Some(raw) = frontmatter.test_command.as_deref() {
            if !raw.trim().is_empty() && check_test_command_shape(raw).is_err() {
                return Err(SddError::InvalidTestCommand);
            }
        }
        // Campos de execução para teste são opcionais: inválido = ignorado, a spec segue válida.
        frontmatter.run_command = frontmatter
            .run_command
            .as_deref()
            .and_then(|raw| check_test_command_shape(raw).ok());
        frontmatter.run_url = frontmatter
            .run_url
            .as_deref()
            .and_then(|raw| check_run_url(raw).map(|(url, _)| url));
        frontmatter.checks = frontmatter
            .checks
            .as_deref()
            .and_then(validate_checks)
            .filter(|c| !c.is_empty());
        // Formatadores: itens inválidos são ignorados; lista vazia equivale a ausente (settings intocado).
        frontmatter.formatters = frontmatter
            .formatters
            .as_deref()
            .map(validate_formatters)
            .filter(|f| !f.is_empty());
        // Artefatos de falha (spec 079): globs inválidos ignorados; vazio equivale a ausente.
        frontmatter.failure_artifacts = frontmatter
            .failure_artifacts
            .as_deref()
            .map(validate_failure_artifacts)
            .filter(|f| !f.is_empty());
        // Capturas de tela (spec 082 T03): mesma regra dos artefatos de falha.
        frontmatter.screenshots = frontmatter
            .screenshots
            .as_deref()
            .map(validate_failure_artifacts)
            .filter(|f| !f.is_empty());
        // Referências visuais (spec 084 T02): nomes de anexos válidos, até 4; vazio equivale a ausente.
        frontmatter.references = frontmatter
            .references
            .as_deref()
            .map(super::references::validate)
            .filter(|r| !r.is_empty());
        // Spec 124 T05: níveis de teste e instalação; inválidos ou vazios equivalem a ausentes.
        frontmatter.full_test_command = valid_optional_command(frontmatter.full_test_command.as_deref());
        frontmatter.install_command = valid_optional_command(frontmatter.install_command.as_deref());
        frontmatter.lockfiles = frontmatter
            .lockfiles
            .as_deref()
            .map(validate_lockfiles)
            .filter(|l| !l.is_empty());
        frontmatter.environment = frontmatter
            .environment
            .as_deref()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| e == "docker" || e == "host");
        Ok(Self { frontmatter, body })
    }

    /// Gera o conteúdo de `spec.md`, preservando o corpo.
    pub fn render(&self) -> Result<String, SddError> {
        render_doc(&self.frontmatter, &self.body)
    }
}

impl Task {
    /// Interpreta o conteúdo de um `tasks/Txx.md`.
    pub fn parse(content: &str) -> Result<Self, SddError> {
        let (frontmatter, body) = parse_doc(content)?;
        Ok(Self { frontmatter, body })
    }

    /// Gera o conteúdo de `tasks/Txx.md`, preservando o corpo.
    pub fn render(&self) -> Result<String, SddError> {
        render_doc(&self.frontmatter, &self.body)
    }
}

impl Settings {
    /// Interpreta o conteúdo de `settings.yaml`.
    pub fn parse(content: &str) -> Result<Self, SddError> {
        let mut settings: Self =
            serde_yaml::from_str(content).map_err(|source| SddError::InvalidYaml { path: None, source })?;
        settings.checks = validate_checks(&settings.checks).ok_or(SddError::InvalidChecks)?;
        settings.formatters = validate_formatters(&settings.formatters);
        settings.failure_artifacts = validate_failure_artifacts(&settings.failure_artifacts);
        settings.screenshots = validate_failure_artifacts(&settings.screenshots);
        settings.full_test_command = valid_optional_command(settings.full_test_command.as_deref());
        settings.install_command = valid_optional_command(settings.install_command.as_deref());
        settings.lockfiles = validate_lockfiles(&settings.lockfiles);
        let (lo, hi) = (*super::schema::LIMIT_RANGE.start(), *super::schema::LIMIT_RANGE.end());
        settings.max_attempts = settings.max_attempts.clamp(lo, hi);
        settings.max_test_cycles = settings.max_test_cycles.clamp(lo, hi);
        Ok(settings)
    }

    /// Gera o conteúdo de `settings.yaml`.
    pub fn render(&self) -> Result<String, SddError> {
        serde_yaml::to_string(self).map_err(SddError::Serialize)
    }
}

/// Erro de I/O associado a `path`.
fn io_at(path: &Path) -> impl FnOnce(io::Error) -> SddError + '_ {
    move |source| SddError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Anexa o caminho de origem a erros de YAML vindos de leitura de arquivo.
fn with_path(err: SddError, path: &Path) -> SddError {
    match err {
        SddError::InvalidYaml { source, .. } => SddError::InvalidYaml {
            path: Some(path.to_path_buf()),
            source,
        },
        SddError::MissingFrontmatter { .. } => SddError::MissingFrontmatter {
            path: Some(path.to_path_buf()),
        },
        other => other,
    }
}

fn read_file(path: &Path) -> Result<String, SddError> {
    fs::read_to_string(path).map_err(io_at(path))
}

fn write_file(path: &Path, content: &str) -> Result<(), SddError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(io_at(parent))?;
    }
    fs::write(path, content).map_err(io_at(path))
}

/// Lê e interpreta `settings.yaml`.
pub fn read_settings(path: &Path) -> Result<Settings, SddError> {
    let content = read_file(path)?;
    Settings::parse(&content).map_err(|e| with_path(e, path))
}

/// Grava `settings.yaml`, criando diretórios pai se necessário.
pub fn write_settings(path: &Path, settings: &Settings) -> Result<(), SddError> {
    write_file(path, &settings.render()?)
}

/// Lê e interpreta um `spec.md`.
pub fn read_spec(path: &Path) -> Result<Spec, SddError> {
    let content = read_file(path)?;
    Spec::parse(&content).map_err(|e| with_path(e, path))
}

/// Grava um `spec.md`, criando diretórios pai se necessário.
pub fn write_spec(path: &Path, spec: &Spec) -> Result<(), SddError> {
    write_file(path, &spec.render()?)
}

/// Lê e interpreta um `tasks/Txx.md`.
pub fn read_task(path: &Path) -> Result<Task, SddError> {
    let content = read_file(path)?;
    Task::parse(&content).map_err(|e| with_path(e, path))
}

/// Grava um `tasks/Txx.md`, criando diretórios pai se necessário.
pub fn write_task(path: &Path, task: &Task) -> Result<(), SddError> {
    write_file(path, &task.render()?)
}

/// Lista entradas de `dir` que satisfazem `keep`, ordenadas por caminho.
/// Diretório ausente retorna lista vazia.
fn sorted_entries(dir: &Path, keep: fn(&Path) -> bool) -> Result<Vec<PathBuf>, SddError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_at(dir)(e)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.map_err(io_at(dir))?.path();
        if keep(&path) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Carrega o estado SDD de `repo_root/.sdd`.
///
/// `settings.yaml` ausente (ou `.sdd` inexistente) retorna `NotInitialized`.
/// Specs são os diretórios em `.sdd/specs/`; tarefas são os `*.md` em `tasks/`.
pub fn load_sdd(repo_root: &Path) -> Result<Sdd, SddError> {
    let dir = sdd_dir(repo_root);
    let settings = match read_settings(&dir.join("settings.yaml")) {
        Err(SddError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Err(SddError::NotInitialized(dir));
        }
        other => other?,
    };

    let mut specs = Vec::new();
    for spec_dir in sorted_entries(&dir.join("specs"), Path::is_dir)? {
        let spec = read_spec(&spec_dir.join("spec.md"))?;
        let mut tasks = Vec::new();
        for file in sorted_entries(&spec_dir.join("tasks"), Path::is_file)? {
            if file.extension() == Some(OsStr::new("md")) {
                tasks.push(read_task(&file)?);
            }
        }
        specs.push((spec, tasks));
    }

    Ok(Sdd { settings, specs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdd::{SpecStatus, TaskStatus, Verdict};
    use std::env;
    use std::process;

    /// Diretório temporário único por processo e teste.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("nants-sdd-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    const SPEC_EXAMPLE: &str = r#"---
id: "001-autenticacao-jwt"
title: "Autenticação JWT e proteção de rotas privadas"
status: "IN_PROGRESS" # DRAFT | AWAITING_APPROVAL | IN_PROGRESS | COMPLETED | BLOCKED
base_branch: "main"
created_at: "2026-10-13T21:00:00Z"
---

## Contexto e Problema
Descrição objetiva gerada pelo Arquiteto após refinamento com o humano.

## Abordagem Técnica
Resumo da decisão arquitetural.

## Critérios de Aceitação Globais
- [ ] Critério 1
- [ ] Critério 2
"#;

    const TASK_EXAMPLE: &str = r#"---
id: "T03"
spec_id: "001-autenticacao-jwt"
title: "Middleware HTTP e extração segura de credenciais"
status: "IN_PROGRESS" # TODO | IN_PROGRESS | IN_REVIEW | CHANGES_REQUESTED | APPROVED | INTEGRATING | DONE | BLOCKED
depends_on: ["T01", "T02"]
branch: "sdd/001-autenticacao-jwt/T03"
attempts: 1
max_attempts: 3
last_review_verdict: "CHANGES_REQUESTED" # NONE | APPROVED | CHANGES_REQUESTED
allowed_scope:
  - "src/http/**"
  - "tests/**"
campo_desconhecido: "tolerado"
---

## Objetivo
Implementar o filtro de requisições para ler o header Authorization e bloquear acessos sem token válido.

## Critérios de Aceitação
- Extrair token Bearer com segurança sem utilizar `unwrap()`.
- Criar testes cobrindo header ausente, malformado e válido.

## Findings Abertos
- id: "F-01"
  severity: "HIGH"
  location: "src/http/auth_mw.rs:28"
  description: "Divisão direta da string falha se o header Bearer vier vazio sem espaço."
  recommendation: "Usar strip_prefix('Bearer ').map(str::trim)."

## Histórico de Diálogo dos Agentes (Stream Log)
- timestamp: "2026-10-13T20:38:12Z"
  agent: "architect"
  model_badge: "Azure East US • gpt-4o"
  message: "Etapa anterior integrada. @Desenvolvedor, crie o filtro de requisições..."
"#;

    const SETTINGS_EXAMPLE: &str = r#"base_branch: "main"
merge_strategy: "squash"
test_command: "cargo test"
allowed_commands:
  - "cargo test*"
  - "cargo check*"
  - "git status"
denied_commands:
  - "rm -rf *"
  - "git push*"
  - "sudo *"
"#;

    #[test]
    fn spec_example_parses_and_round_trips() {
        let spec = Spec::parse(SPEC_EXAMPLE).expect("exemplo válido");
        assert_eq!(spec.frontmatter.id, "001-autenticacao-jwt");
        assert_eq!(spec.frontmatter.status, SpecStatus::InProgress);
        assert!(spec.body.starts_with("\n## Contexto e Problema"));

        let rendered = spec.render().expect("render");
        let reparsed = Spec::parse(&rendered).expect("reparse");
        assert_eq!(reparsed, spec);
        assert_eq!(reparsed.body, spec.body, "corpo deve ser preservado");
    }

    #[test]
    fn task_example_parses_and_round_trips_with_findings_body() {
        let task = Task::parse(TASK_EXAMPLE).expect("exemplo válido");
        let meta = &task.frontmatter;
        assert_eq!(meta.id, "T03");
        assert_eq!(meta.status, TaskStatus::InProgress);
        assert_eq!(meta.depends_on, vec!["T01", "T02"]);
        assert_eq!(meta.attempts, 1);
        assert_eq!(meta.max_attempts, 3);
        assert_eq!(meta.last_verdict, Verdict::ChangesRequested);
        assert_eq!(meta.allowed_scope, vec!["src/http/**", "tests/**"]);
        assert_eq!(meta.reviewed_sha, None);
        assert_eq!(meta.test_evidence, None);
        assert_eq!(meta.next_action, None);
        assert!(task.body.contains("- id: \"F-01\""), "Findings preservados no corpo");

        let rendered = task.render().expect("render");
        let reparsed = Task::parse(&rendered).expect("reparse");
        assert_eq!(reparsed, task);
        assert_eq!(reparsed.body, task.body);
    }

    #[test]
    fn task_optional_fields_round_trip() {
        let content = TASK_EXAMPLE.replace(
            "campo_desconhecido",
            "reviewed_sha: \"abc123\"\ntest_evidence: \"cargo test ok\"\nnext_action: \"revisar\"\ncampo_desconhecido",
        );
        let task = Task::parse(&content).expect("parse");
        assert_eq!(task.frontmatter.reviewed_sha.as_deref(), Some("abc123"));
        assert_eq!(task.frontmatter.next_action.as_deref(), Some("revisar"));
        let reparsed = Task::parse(&task.render().expect("render")).expect("reparse");
        assert_eq!(reparsed, task);
    }

    #[test]
    fn task_covers_parse_ignores_malformed_and_round_trips() {
        // Spec 086 T07: ausente equivale a vazio e não aparece no render.
        let plain = Task::parse(TASK_EXAMPLE).expect("parse");
        assert!(plain.frontmatter.covers.is_empty());
        assert!(!plain.render().expect("render").contains("covers"));

        let content = TASK_EXAMPLE.replace(
            "campo_desconhecido",
            "covers: [\"G1\", \"g2\", \"G0\", 7, \"G3\", \"G1\", \"T01\"]\ncampo_desconhecido",
        );
        let task = Task::parse(&content).expect("covers malformado não invalida a tarefa");
        assert_eq!(task.frontmatter.covers, vec!["G1", "G3"]);
        let rendered = task.render().expect("render");
        assert!(rendered.contains("covers:"), "{rendered}");
        assert_eq!(Task::parse(&rendered).expect("reparse"), task);

        let scalar = TASK_EXAMPLE.replace("campo_desconhecido", "covers: \"G2, G4\"\ncampo_desconhecido");
        assert_eq!(Task::parse(&scalar).expect("parse").frontmatter.covers, vec!["G2", "G4"]);
        let odd = TASK_EXAMPLE.replace("campo_desconhecido", "covers: {a: 1}\ncampo_desconhecido");
        assert!(Task::parse(&odd).expect("parse").frontmatter.covers.is_empty());
    }

    #[test]
    fn settings_example_parses_and_round_trips() {
        let settings = Settings::parse(SETTINGS_EXAMPLE).expect("parse");
        assert_eq!(settings.base_branch, "main");
        assert_eq!(settings.denied_commands.len(), 3);
        let reparsed = Settings::parse(&settings.render().expect("render")).expect("reparse");
        assert_eq!(reparsed, settings);
    }

    #[test]
    fn settings_lists_default_to_empty() {
        let settings = Settings::parse(
            "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"cargo test\"\n",
        )
        .expect("parse");
        assert!(settings.allowed_commands.is_empty());
        assert!(settings.denied_commands.is_empty());
    }

    #[test]
    fn settings_skills_default_on_and_false_round_trips() {
        let base = "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"cargo test\"\n";
        let on = Settings::parse(base).expect("parse");
        assert!(on.skills, "ausente = ligado");
        assert!(!on.render().expect("render").contains("skills"), "padrão não é gravado");
        let off = Settings::parse(&format!("{base}skills: false\n")).expect("parse");
        assert!(!off.skills);
        let again = Settings::parse(&off.render().expect("render")).expect("reparse");
        assert!(!again.skills);
    }

    #[test]
    fn missing_frontmatter_is_rejected() {
        let err = Spec::parse("# Sem frontmatter\n").expect_err("deve falhar");
        assert!(matches!(err, SddError::MissingFrontmatter { path: None }), "obtido: {err:?}");
        let err = Task::parse("---\nid: \"T01\"\n").expect_err("sem fechamento");
        assert!(matches!(err, SddError::MissingFrontmatter { path: None }), "obtido: {err:?}");
    }

    #[test]
    fn missing_frontmatter_on_read_carries_path() {
        let dir = temp_dir("missing-fm-path");
        let path = dir.join("T01.md");
        write_file(&path, "# sem frontmatter\n").expect("write");
        let err = read_task(&path).expect_err("deve falhar");
        match err {
            SddError::MissingFrontmatter { path: Some(p) } => assert_eq!(p, path),
            other => panic!("obtido: {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_uses_blueprint_verdict_key_and_reads_legacy_key() {
        let task = Task::parse(TASK_EXAMPLE).expect("exemplo válido");
        let rendered = task.render().expect("render");
        assert!(rendered.contains("last_review_verdict:"), "{rendered}");
        assert!(!rendered.contains("last_verdict:"), "{rendered}");

        let legacy = TASK_EXAMPLE.replace("last_review_verdict:", "last_verdict:");
        let parsed = Task::parse(&legacy).expect("chave antiga deve ser aceita");
        assert_eq!(parsed.frontmatter.last_verdict, Verdict::ChangesRequested);
    }

    #[test]
    fn invalid_yaml_is_rejected_with_path_on_read() {
        let dir = temp_dir("invalid-yaml");
        let path = dir.join("T01.md");
        write_file(&path, "---\nid: [\n---\n\ncorpo\n").expect("write");
        let err = read_task(&path).expect_err("deve falhar");
        match err {
            SddError::InvalidYaml { path: Some(p), .. } => assert_eq!(p, path),
            other => panic!("obtido: {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_status_is_rejected() {
        let content = TASK_EXAMPLE.replace("status: \"IN_PROGRESS\"", "status: \"FOO\"");
        let err = Task::parse(&content).expect_err("status inválido");
        assert!(matches!(err, SddError::InvalidYaml { .. }), "obtido: {err:?}");
    }

    #[test]
    fn status_enums_use_screaming_snake_case() {
        let status: TaskStatus = serde_yaml::from_str("CHANGES_REQUESTED").expect("parse");
        assert_eq!(status, TaskStatus::ChangesRequested);
        let status: SpecStatus = serde_yaml::from_str("AWAITING_APPROVAL").expect("parse");
        assert_eq!(status, SpecStatus::AwaitingApproval);
        let verdict: Verdict = serde_yaml::from_str("NONE").expect("parse");
        assert_eq!(verdict, Verdict::None);
    }

    #[test]
    fn write_then_read_spec_and_task_by_path() {
        let dir = temp_dir("write-read");
        let spec = Spec::parse(SPEC_EXAMPLE).expect("spec");
        let task = Task::parse(TASK_EXAMPLE).expect("task");
        let spec_path = dir.join("specs/001/spec.md");
        let task_path = dir.join("specs/001/tasks/T03.md");
        write_spec(&spec_path, &spec).expect("write spec");
        write_task(&task_path, &task).expect("write task");
        assert_eq!(read_spec(&spec_path).expect("read spec"), spec);
        assert_eq!(read_task(&task_path).expect("read task"), task);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_sdd_reads_specs_and_tasks_in_name_order() {
        let root = temp_dir("load");
        let sdd = root.join(".sdd");
        write_file(&sdd.join("settings.yaml"), SETTINGS_EXAMPLE).expect("settings");

        let spec_b = SPEC_EXAMPLE.replace("001-autenticacao-jwt", "002-b");
        let spec_a = SPEC_EXAMPLE.replace("001-autenticacao-jwt", "001-a");
        write_file(&sdd.join("specs/002-b/spec.md"), &spec_b).expect("spec b");
        write_file(&sdd.join("specs/001-a/spec.md"), &spec_a).expect("spec a");
        write_file(&sdd.join("specs/001-a/tasks/T02.md"), TASK_EXAMPLE).expect("t02");
        write_file(&sdd.join("specs/001-a/tasks/T01.md"), TASK_EXAMPLE).expect("t01");
        write_file(&sdd.join("specs/001-a/tasks/notas.txt"), "ignorado").expect("txt");
        write_file(&sdd.join("specs/stray.txt"), "ignorado").expect("stray");

        let loaded = load_sdd(&root).expect("load");
        assert_eq!(loaded.settings, Settings::parse(SETTINGS_EXAMPLE).expect("settings"));
        assert_eq!(loaded.specs.len(), 2);
        let (spec, tasks) = &loaded.specs[0];
        assert_eq!(spec.frontmatter.id, "001-a");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].frontmatter.id, "T03", "conteúdo de TASK_EXAMPLE");
        assert_eq!(loaded.specs[1].0.frontmatter.id, "002-b");
        assert!(loaded.specs[1].1.is_empty(), "sem diretório tasks");
        let _ = fs::remove_dir_all(&root);
    }

    /// Spec 124 T05: os campos novos são opcionais; inválidos equivalem a ausentes, e o settings
    /// antigo segue lendo e gravando igual.
    #[test]
    fn test_levels_fields_are_optional_and_validated() {
        let base = "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"cargo test\"\n";
        let legacy = Settings::parse(base).expect("settings antigo");
        assert!(legacy.full_test_command.is_none() && legacy.install_command.is_none() && legacy.lockfiles.is_empty());
        assert!(!legacy.render().expect("render").contains("lockfiles"));
        let full = Settings::parse(&format!(
            "{base}full_test_command: \"make all\"\ninstall_command: \"  \"\nlockfiles:\n  - ./a.lock\n  - a.lock\n  - /abs.lock\n  - \"x/*.lock\"\n"
        ))
        .expect("settings novo");
        assert_eq!(full.full_test_command.as_deref(), Some("make all"));
        assert!(full.install_command.is_none(), "vazio = ausente");
        assert_eq!(full.lockfiles, vec!["a.lock".to_string()]);
        let spec = Spec::parse(&SPEC_EXAMPLE.replacen(
            "base_branch: \"main\"",
            "base_branch: \"main\"\nfull_test_command: \"make all\"\ninstall_command: \"make deps\"\nlockfiles: [\"deps.lock\", \"../x\"]",
            1,
        ))
        .expect("spec");
        assert_eq!(spec.frontmatter.full_test_command.as_deref(), Some("make all"));
        assert_eq!(spec.frontmatter.install_command.as_deref(), Some("make deps"));
        assert_eq!(spec.frontmatter.lockfiles, Some(vec!["deps.lock".to_string()]));
    }

    #[test]
    fn spec_test_command_is_optional_and_round_trips() {
        let legacy = Spec::parse(SPEC_EXAMPLE).expect("spec antiga, sem test_command");
        assert_eq!(legacy.frontmatter.test_command, None);
        assert!(!legacy.render().expect("render").contains("test_command"));

        let content = SPEC_EXAMPLE.replace(
            "base_branch: \"main\"\n",
            "base_branch: \"main\"\ntest_command: \"node --test\"\n",
        );
        let spec = Spec::parse(&content).expect("com test_command");
        assert_eq!(spec.frontmatter.test_command.as_deref(), Some("node --test"));
        let rendered = spec.render().expect("render");
        assert!(rendered.contains("test_command: node --test") || rendered.contains("test_command: \"node --test\""));
        let reparsed = Spec::parse(&rendered).expect("reparse");
        assert_eq!(reparsed, spec);
        assert_eq!(reparsed.body, spec.body);
    }

    #[test]
    fn spec_invalid_test_command_is_rejected_with_static_error() {
        let bad = [
            r#""a\nb""#.to_string(),
            r#""a\rb""#.to_string(),
            r#""a\tb""#.to_string(),
            r#""a\u0000b""#.to_string(),
            format!("\"{}\"", "x".repeat(201)),
        ];
        for value in bad {
            let content = SPEC_EXAMPLE.replace(
                "base_branch: \"main\"\n",
                &format!("base_branch: \"main\"\ntest_command: {value}\n"),
            );
            let err = Spec::parse(&content).expect_err(&value);
            assert!(matches!(err, SddError::InvalidTestCommand), "{value}: {err:?}");
        }
        let limit = format!("\"{}\"", "x".repeat(200));
        let content = SPEC_EXAMPLE.replace(
            "base_branch: \"main\"\n",
            &format!("base_branch: \"main\"\ntest_command: {limit}\n"),
        );
        assert!(Spec::parse(&content).is_ok(), "200 caracteres são aceitos");
    }

    #[test]
    fn blank_spec_test_command_is_absent_for_the_gate() {
        let content = SPEC_EXAMPLE.replace(
            "base_branch: \"main\"\n",
            "base_branch: \"main\"\ntest_command: \"   \"\n",
        );
        let spec = Spec::parse(&content).expect("vazio é aceito pelo parser");
        assert_eq!(spec.frontmatter.valid_test_command(), None);
    }

    #[test]
    fn test_command_shape_is_one_shared_rule() {
        use crate::sdd::{check_test_command_shape, TestCommandError};
        assert_eq!(check_test_command_shape("  cargo test  ").as_deref(), Ok("cargo test"));
        assert_eq!(check_test_command_shape(" \t "), Err(TestCommandError::Empty));
        assert!(check_test_command_shape(&"x".repeat(200)).is_ok());
        assert_eq!(check_test_command_shape(&"x".repeat(201)), Err(TestCommandError::Invalid));
        for bad in ["a\nb", "a\rb", "a\u{0}b", "a\tb"] {
            assert_eq!(check_test_command_shape(bad), Err(TestCommandError::Invalid), "{bad:?}");
        }
    }

    #[test]
    fn load_sdd_without_sdd_dir_is_not_initialized() {
        let root = temp_dir("not-init");
        fs::create_dir_all(&root).expect("mkdir");
        let err = load_sdd(&root).expect_err("deve falhar");
        match err {
            SddError::NotInitialized(p) => assert_eq!(p, root.join(".sdd")),
            other => panic!("obtido: {other:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn run_url_table_accepts_only_loopback_with_explicit_port() {
        use crate::sdd::check_run_url;
        for ok in [
            "http://localhost:3000",
            "http://127.0.0.1:8080/app",
            "http://[::1]:5173/",
            "http://localhost:1024",
            "http://localhost:65535/a/b?x=1",
        ] {
            assert!(check_run_url(ok).is_some(), "{ok}");
        }
        for bad in [
            "https://localhost:3000",
            "http://localhost",
            "http://localhost:80",
            "http://localhost:1023",
            "http://localhost:65536",
            "http://localhost:",
            "http://user@localhost:3000",
            "http://user:pw@localhost:3000",
            "http://example.com:3000",
            "http://localhost.evil.com:3000",
            "http://localhost:3000@evil.com",
            "http://localhost:3000@evil.com/x",
            "http://0.0.0.0:3000",
            "http://localhost:3000?x=1",
            "http://localhost:3000#a",
            "http://localhost:3000/a b",
            "http://localhost:3000/\u{7}",
            "ftp://localhost:3000",
            "localhost:3000",
            "",
        ] {
            assert!(check_run_url(bad).is_none(), "{bad:?}");
        }
        assert_eq!(check_run_url("http://[::1]:5173/").map(|(_, p)| p), Some(5173));
    }

    #[test]
    fn invalid_run_fields_are_ignored_and_spec_stays_valid() {
        let content = SPEC_EXAMPLE.replace(
            "base_branch: \"main\"\n",
            "base_branch: \"main\"\nrun_command: \"a\\nb\"\nrun_url: \"https://x.com:3000\"\n",
        );
        let spec = Spec::parse(&content).expect("spec válida");
        assert_eq!(spec.frontmatter.run_command, None);
        assert_eq!(spec.frontmatter.run_url, None);
        let good = SPEC_EXAMPLE.replace(
            "base_branch: \"main\"\n",
            "base_branch: \"main\"\nrun_command: \"npm start\"\nrun_url: \"http://localhost:3000/\"\n",
        );
        let spec = Spec::parse(&good).expect("spec");
        assert_eq!(spec.frontmatter.run_command.as_deref(), Some("npm start"));
        assert_eq!(spec.frontmatter.run_url.as_deref(), Some("http://localhost:3000/"));
        let again = Spec::parse(&spec.render().expect("render")).expect("round trip");
        assert_eq!(again, spec);
    }

    #[test]
    fn settings_checks_default_empty_roundtrip_and_validation() {
        let base = "base_branch: main\nmerge_strategy: squash\ntest_command: cargo test\n";
        let s = Settings::parse(base).expect("sem checks");
        assert!(s.checks.is_empty());
        assert!(!s.render().expect("render").contains("checks"));

        let yaml = format!("{base}checks:\n  - name: lint\n    command: npm run lint\n  - name: build\n    command: npm run build\n    blocking: true\n");
        let s = Settings::parse(&yaml).expect("com checks");
        assert_eq!(s.checks.len(), 2);
        assert!(!s.checks[0].blocking && s.checks[1].blocking);
        assert_eq!(Settings::parse(&s.render().expect("render")).expect("rt"), s);

        for bad in [
            "checks:\n  - name: Lint\n    command: x\n",
            "checks:\n  - name: a\n    command: \"x\\ny\"\n",
            "checks:\n  - name: a\n    command: x\n  - name: a\n    command: y\n",
            "checks:\n  - name: a\n    command: \"\"\n",
        ] {
            assert!(matches!(Settings::parse(&format!("{base}{bad}")), Err(SddError::InvalidChecks)), "{bad}");
        }
        let nine: String = (0..9).map(|i| format!("  - name: c{i}\n    command: x\n")).collect();
        assert!(Settings::parse(&format!("{base}checks:\n{nine}")).is_err());
    }

    #[test]
    fn settings_limits_default_five_roundtrip_and_range() {
        let base = "base_branch: main\nmerge_strategy: squash\ntest_command: cargo test\n";
        let s = Settings::parse(base).expect("antigo");
        assert_eq!((s.max_attempts, s.max_test_cycles), (5, 5));
        assert!(!s.render().expect("render").contains("max_"));
        let s = Settings::parse(&format!("{base}max_attempts: 7\nmax_test_cycles: 1\n")).expect("custom");
        assert_eq!((s.max_attempts, s.max_test_cycles), (7, 1));
        assert_eq!(Settings::parse(&s.render().expect("render")).expect("rt"), s);
        let s = Settings::parse(&format!("{base}max_attempts: 20\nmax_test_cycles: 0\n")).expect("clamp");
        assert_eq!((s.max_attempts, s.max_test_cycles), (10, 1));
    }

    #[test]
    fn spec_checks_frontmatter_validated_and_round_trips() {
        let mk = |checks: &str| format!("---\nid: s\ntitle: t\nstatus: DRAFT\nbase_branch: main\ncreated_at: x\n{checks}---\ncorpo\n");
        let ok = Spec::parse(&mk("checks:\n  - name: build\n    command: npm run build\n    blocking: true\n")).expect("ok");
        let c = ok.frontmatter.checks.clone().expect("checks");
        assert_eq!(c.len(), 1);
        assert!(c[0].blocking);
        assert_eq!(Spec::parse(&ok.render().expect("render")).expect("rt"), ok);
        let bad = Spec::parse(&mk("checks:\n  - name: BAD\n    command: x\n")).expect("spec segue válida");
        assert!(bad.frontmatter.checks.is_none(), "inválidos são ignorados");
        assert!(Spec::parse(&mk("")).expect("sem").frontmatter.checks.is_none());
    }

    #[test]
    fn settings_formatters_default_empty_defaults_roundtrip_and_validation() {
        let base = "base_branch: main\nmerge_strategy: squash\ntest_command: cargo test\n";
        let s = Settings::parse(base).expect("sem formatters");
        assert!(s.formatters.is_empty());
        assert!(!s.render().expect("render").contains("formatters"));

        // Padrões: workdir "." e paths ["**"].
        let s = Settings::parse(&format!("{base}formatters:\n  - name: rust\n    command: cargo fmt\n")).expect("padrão");
        assert_eq!(s.formatters[0].workdir, ".");
        assert_eq!(s.formatters[0].paths, vec!["**"]);
        // Normaliza workdir e paths; viaja inteiro no round-trip.
        let yaml = format!(
            "{base}formatters:\n  - name: web\n    command: npx prettier --write .\n    workdir: ./web//app/\n    paths: [\"./web/**\", \"*.md\"]\n  - name: py\n    command: ruff format .\n    workdir: api\n    paths: [\"api/**/*.py\"]\n"
        );
        let s = Settings::parse(&yaml).expect("módulos");
        assert_eq!(s.formatters.len(), 2);
        assert_eq!(s.formatters[0].workdir, "web/app");
        assert_eq!(s.formatters[0].paths, vec!["web/**", "*.md"]);
        assert_eq!(Settings::parse(&s.render().expect("render")).expect("rt"), s);
    }

    #[test]
    fn invalid_formatters_are_ignored_item_by_item() {
        let base = "base_branch: main\nmerge_strategy: squash\ntest_command: cargo test\n";
        let item = |name: &str, command: &str, extra: &str| format!("  - name: {name}\n    command: {command}\n{extra}");
        let ok = item("ok", "cargo fmt", "");
        for bad in [
            item("Nome", "x", ""),                               // nome fora do padrão
            item("a", "\"\"", ""),                               // comando vazio
            item("a", "\"x\\ny\"", ""),                          // comando com quebra de linha
            item("a", "x", "    workdir: ../fora\n"),            // sobe diretório
            item("a", "x", "    workdir: web/../../x\n"),        // idem, no meio
            item("a", "x", "    workdir: /abs\n"),               // absoluto
            item("a", "x", "    paths: [\"../x/**\"]\n"),        // glob com ..
            item("a", "x", "    paths: [\"/etc/**\"]\n"),        // glob absoluto
            item("a", "x", "    paths: [\"\"]\n"),               // glob vazio
            item("ok", "outro", ""),                             // nome repetido
        ] {
            let s = Settings::parse(&format!("{base}formatters:\n{ok}{bad}")).expect("settings segue válido");
            assert_eq!(s.formatters.len(), 1, "{bad}");
            assert_eq!(s.formatters[0].command, "cargo fmt", "{bad}");
        }
        // Mais de 8: valem os 8 primeiros.
        let ten: String = (0..10).map(|i| item(&format!("f{i}"), "x", "")).collect();
        let s = Settings::parse(&format!("{base}formatters:\n{ten}")).expect("limite");
        assert_eq!(s.formatters.len(), crate::sdd::MAX_FORMATTERS);
        assert_eq!(s.formatters[7].name, "f7");
        // `paths` vazio equivale ao padrão.
        let s = Settings::parse(&format!("{base}formatters:\n{}", item("a", "x", "    paths: []\n"))).expect("vazio");
        assert_eq!(s.formatters[0].paths, vec!["**"]);
    }

    #[test]
    fn failure_artifacts_settings_and_spec_validated_item_by_item() {
        let base = "base_branch: main\nmerge_strategy: squash\ntest_command: cargo test\n";
        let s = Settings::parse(base).expect("sem campo");
        assert!(s.failure_artifacts.is_empty());
        assert!(!s.render().expect("render").contains("failure_artifacts"));
        let s = Settings::parse(&format!(
            "{base}failure_artifacts: [\"./test-results//**/error-context.md\", \"../fora\", \"/abs/x\", \"C:/x\", \"a\\\\b\", \"\", \"logs/*.log\", \"logs/*.log\"]\n"
        ))
        .expect("inválidos ignorados");
        assert_eq!(s.failure_artifacts, vec!["test-results/**/error-context.md", "logs/*.log"]);
        assert_eq!(Settings::parse(&s.render().expect("render")).expect("rt"), s);
        let many: String = (0..12).map(|i| format!("  - d{i}/*.md\n")).collect();
        let s = Settings::parse(&format!("{base}failure_artifacts:\n{many}")).expect("limite");
        assert_eq!(s.failure_artifacts.len(), crate::sdd::MAX_FAILURE_ARTIFACTS);
        assert_eq!(s.failure_artifacts[7], "d7/*.md");

        let mk = |f: &str| format!("---\nid: s\ntitle: t\nstatus: DRAFT\nbase_branch: main\ncreated_at: x\n{f}---\ncorpo\n");
        let ok = Spec::parse(&mk("failure_artifacts: [\"test-results/**/error-context.md\", \"../x\"]\n")).expect("ok");
        assert_eq!(ok.frontmatter.failure_artifacts, Some(vec!["test-results/**/error-context.md".to_string()]));
        assert_eq!(Spec::parse(&ok.render().expect("render")).expect("rt"), ok);
        assert!(Spec::parse(&mk("failure_artifacts: [\"/abs\"]\n")).expect("válida").frontmatter.failure_artifacts.is_none());
        assert!(Spec::parse(&mk("failure_artifacts: []\n")).expect("vazia").frontmatter.failure_artifacts.is_none());
        assert!(Spec::parse(&mk("")).expect("sem").frontmatter.failure_artifacts.is_none());
    }

    #[test]
    fn screenshots_settings_and_spec_validated_like_failure_artifacts() {
        // Spec 082 T03: `screenshots` é irmão de `failure_artifacts`, com a mesma validação.
        let base = "base_branch: main\nmerge_strategy: squash\ntest_command: cargo test\n";
        let s = Settings::parse(base).expect("sem campo");
        assert!(s.screenshots.is_empty());
        assert!(!s.render().expect("render").contains("screenshots"));
        let s = Settings::parse(&format!(
            "{base}screenshots: [\"./test-results//screens/**/*.png\", \"../fora/*.png\", \"/abs/x.png\", \"shots/*.jpg\"]\n"
        ))
        .expect("inválidos ignorados");
        assert_eq!(s.screenshots, vec!["test-results/screens/**/*.png", "shots/*.jpg"]);
        assert_eq!(Settings::parse(&s.render().expect("render")).expect("rt"), s);
        let many: String = (0..12).map(|i| format!("  - d{i}/*.png\n")).collect();
        let s = Settings::parse(&format!("{base}screenshots:\n{many}")).expect("limite");
        assert_eq!(s.screenshots.len(), crate::sdd::MAX_FAILURE_ARTIFACTS);

        let mk = |f: &str| format!("---\nid: s\ntitle: t\nstatus: DRAFT\nbase_branch: main\ncreated_at: x\n{f}---\ncorpo\n");
        let ok = Spec::parse(&mk("screenshots: [\"test-results/screens/**/*.png\", \"../x\"]\n")).expect("ok");
        assert_eq!(ok.frontmatter.screenshots, Some(vec!["test-results/screens/**/*.png".to_string()]));
        assert_eq!(Spec::parse(&ok.render().expect("render")).expect("rt"), ok);
        assert!(Spec::parse(&mk("screenshots: [\"/abs\"]\n")).expect("válida").frontmatter.screenshots.is_none());
        assert!(Spec::parse(&mk("screenshots: []\n")).expect("vazia").frontmatter.screenshots.is_none());
        assert!(Spec::parse(&mk("")).expect("sem").frontmatter.screenshots.is_none());
    }

    #[test]
    fn spec_references_keep_valid_attachment_names_up_to_four() {
        // Spec 084 T02: nomes de anexos do chat; inválidos ignorados; vazio = ausente; sem o campo no render.
        let mk = |f: &str| format!("---\nid: s\ntitle: t\nstatus: DRAFT\nbase_branch: main\ncreated_at: x\n{f}---\ncorpo\n");
        let id = |c: char| c.to_string().repeat(32);
        let none = Spec::parse(&mk("")).expect("sem");
        assert!(none.frontmatter.references.is_none());
        assert!(!none.render().expect("render").contains("references"));
        let raw = format!(
            "references: [\"{a}.png\", \"../{b}.png\", \"{b}.gif\", \"x.png\", \"{b}.jpeg\", \"{c}.webp\", \"{d}.jpg\", \"{e}.png\"]\n",
            a = id('a'),
            b = id('b'),
            c = id('c'),
            d = id('d'),
            e = id('e')
        );
        let ok = Spec::parse(&mk(&raw)).expect("ok");
        let expected: Vec<String> =
            vec![format!("{}.png", id('a')), format!("{}.jpeg", id('b')), format!("{}.webp", id('c')), format!("{}.jpg", id('d'))];
        assert_eq!(ok.frontmatter.references, Some(expected));
        assert_eq!(Spec::parse(&ok.render().expect("render")).expect("rt"), ok);
        assert!(Spec::parse(&mk("references: [\"x.png\"]\n")).expect("inválida").frontmatter.references.is_none());
        assert!(Spec::parse(&mk("references: []\n")).expect("vazia").frontmatter.references.is_none());
    }

    #[test]
    fn spec_formatters_frontmatter_validated_and_round_trips() {
        let mk = |f: &str| format!("---\nid: s\ntitle: t\nstatus: DRAFT\nbase_branch: main\ncreated_at: x\n{f}---\ncorpo\n");
        let ok = Spec::parse(&mk("formatters:\n  - name: web\n    command: npx prettier --write .\n    workdir: web\n    paths: [\"web/**\"]\n")).expect("ok");
        let f = ok.frontmatter.formatters.clone().expect("formatters");
        assert_eq!((f[0].name.as_str(), f[0].workdir.as_str()), ("web", "web"));
        assert_eq!(Spec::parse(&ok.render().expect("render")).expect("rt"), ok);
        let mixed = Spec::parse(&mk("formatters:\n  - name: BAD\n    command: x\n  - name: ok\n    command: y\n")).expect("válida");
        assert_eq!(mixed.frontmatter.formatters.expect("só o válido").len(), 1);
        let all_bad = Spec::parse(&mk("formatters:\n  - name: BAD\n    command: x\n")).expect("válida");
        assert!(all_bad.frontmatter.formatters.is_none(), "tudo inválido = ausente");
        assert!(Spec::parse(&mk("formatters: []\n")).expect("vazia").frontmatter.formatters.is_none());
        assert!(Spec::parse(&mk("")).expect("sem").frontmatter.formatters.is_none());
    }
}
