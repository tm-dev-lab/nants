//! Comandos de teste em modo docker (spec 039): o runner já executa `test_command` e `checks`
//! DENTRO do serviço `workspace`; um comando que invoque `docker`/`podman` falharia lá dentro.
//! Este módulo desembrulha o formato embrulhado exato e recusa qualquer outra invocação.

use super::schema::{Check, Environment, Formatter, Settings};

/// Mensagem de recusa (sem eco do comando informado).
pub const DOCKER_COMMAND_MSG: &str = "em modo docker, test_command, checks e formatters já rodam DENTRO do serviço workspace: \
não invoque docker, docker compose nem podman; use o comando interno (por exemplo: npm ci && npm test)";

const TOOLS: [&str; 4] = ["docker", "docker-compose", "podman", "podman-compose"];
const SEPARATORS: &[char] = &[';', '&', '|', '(', ')', '<', '>', '"', '\'', '`', '$', '{', '}', '=', ','];

/// `true` quando algum token do comando é `docker`, `docker-compose`, `podman` (ou caminho até eles).
pub fn invokes_container_tool(command: &str) -> bool {
    command
        .split(|c: char| c.is_whitespace() || SEPARATORS.contains(&c))
        .filter(|t| !t.is_empty())
        .any(|t| TOOLS.contains(&t.rsplit('/').next().unwrap_or(t)))
}

/// Primeiro token (separado por espaço) e o resto sem espaços à esquerda.
fn next_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    Some((&s[..end], s[end..].trim_start()))
}

/// Extrai `<X>` de `docker compose [-f F] run [--rm ..]|exec [-T ..] workspace sh|bash [-lc|-c] '<X>'`
/// (aspas simples ou duplas). `None` se o comando não tem exatamente essa forma.
pub fn unwrap_workspace_command(command: &str) -> Option<String> {
    let (first, mut rest) = next_token(command.trim())?;
    match first {
        "docker" => {
            let (sub, r) = next_token(rest)?;
            if sub != "compose" {
                return None;
            }
            rest = r;
        }
        "docker-compose" => {}
        _ => return None,
    }
    let mut verb_seen = false;
    loop {
        let (tok, r) = next_token(rest)?;
        rest = r;
        match tok {
            "-f" | "--file" => {
                let (_, r) = next_token(rest)?;
                rest = r;
            }
            "run" | "exec" if !verb_seen => verb_seen = true,
            "workspace" if verb_seen => break,
            t if verb_seen && matches!(t, "--rm" | "-T" | "--no-deps" | "--service-ports") => {}
            _ => return None,
        }
    }
    let (shell, r) = next_token(rest)?;
    if !matches!(shell, "sh" | "bash") {
        return None;
    }
    let (flag, r) = next_token(r)?;
    if !matches!(flag, "-lc" | "-c") {
        return None;
    }
    let quoted = r.trim();
    let quote = quoted.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let inner = quoted.strip_prefix(quote)?.strip_suffix(quote)?;
    if inner.trim().is_empty() || inner.contains(quote) {
        return None;
    }
    if quote == '"' && inner.contains(['$', '`', '\\']) {
        return None;
    }
    Some(inner.trim().to_string())
}

/// Comando equivalente FORA do Nants para um comando interno do `workspace` (forma de execução do
/// README e do relatório). Aspas simples internas são escapadas para manter o embrulho válido.
pub fn wrap_workspace(inner: &str) -> String {
    let escaped = inner.trim().replace('\'', "'\\''");
    format!("docker compose -f .nants/compose.yaml run --rm --service-ports workspace sh -lc '{escaped}'")
}

/// Normaliza um comando de modo docker: desembrulha o formato embrulhado e recusa o que ainda
/// invoca docker/podman. `Err` traz a mensagem estática de orientação.
pub fn normalize(command: &str) -> Result<String, &'static str> {
    let inner = unwrap_workspace_command(command).unwrap_or_else(|| command.trim().to_string());
    if invokes_container_tool(&inner) {
        Err(DOCKER_COMMAND_MSG)
    } else {
        Ok(inner)
    }
}

/// Normaliza `test_command` (se não vazio) e `checks`. `Err` se algum ainda invoca docker.
pub fn normalize_all(test_command: &str, checks: &[Check]) -> Result<(String, Vec<Check>), &'static str> {
    let test = if test_command.trim().is_empty() { String::new() } else { normalize(test_command)? };
    let mut out = Vec::with_capacity(checks.len());
    for c in checks {
        out.push(Check { command: normalize(&c.command)?, ..c.clone() });
    }
    Ok((test, out))
}

/// Normaliza os comandos dos formatadores (spec 078) como os de `checks`.
pub fn normalize_formatters(formatters: &[Formatter]) -> Result<Vec<Formatter>, &'static str> {
    formatters
        .iter()
        .map(|f| Ok(Formatter { command: normalize(&f.command)?, ..f.clone() }))
        .collect()
}

/// Migração idempotente: em projeto docker, desembrulha `test_command`/`checks`/`formatters` no formato
/// embrulhado exato. Devolve `true` se alterou `settings` (o chamador grava).
pub fn migrate_wrapped(settings: &mut Settings) -> bool {
    if settings.environment != Environment::Docker {
        return false;
    }
    let mut changed = false;
    if let Some(inner) = unwrap_workspace_command(&settings.test_command) {
        settings.test_command = inner;
        changed = true;
    }
    for c in &mut settings.checks {
        if let Some(inner) = unwrap_workspace_command(&c.command) {
            c.command = inner;
            changed = true;
        }
    }
    for f in &mut settings.formatters {
        if let Some(inner) = unwrap_workspace_command(&f.command) {
            f.command = inner;
            changed = true;
        }
    }
    changed
}

/// Cópia do `settings.yaml` real do app-test-04 (fixture de teste).
#[cfg(test)]
pub(crate) const APP_TEST_04_SETTINGS: &str = "test_command: docker compose -f .nants/compose.yaml run --rm workspace sh -lc 'npm ci && npm test'\nbase_branch: main\nmerge_strategy: squash\nchecks:\n- name: typecheck\n  command: docker compose -f .nants/compose.yaml run --rm workspace sh -lc 'npm ci && npm run typecheck'\n  blocking: true\n- name: build\n  command: docker compose -f .nants/compose.yaml run --rm workspace sh -lc 'npm ci && npm run build'\n  blocking: true\nenvironment: docker\n";

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = APP_TEST_04_SETTINGS;

    #[test]
    fn unwraps_run_exec_and_quotes() {
        let w = |c: &str| unwrap_workspace_command(c);
        assert_eq!(
            w("docker compose -f .nants/compose.yaml run --rm workspace sh -lc 'npm ci && npm test'").as_deref(),
            Some("npm ci && npm test")
        );
        assert_eq!(
            w("docker compose -f .nants/compose.yaml exec -T workspace sh -lc \"npm test\"").as_deref(),
            Some("npm test")
        );
        assert_eq!(w("docker compose run --rm workspace bash -c 'make'").as_deref(), Some("make"));
        assert_eq!(
            w("docker compose -f .nants/compose.yaml run --rm --service-ports workspace sh -lc 'npm ci && npm run dev -- --host 0.0.0.0 --port 5173 --strictPort'").as_deref(),
            Some("npm ci && npm run dev -- --host 0.0.0.0 --port 5173 --strictPort")
        );
        assert_eq!(w("npm test"), None);
        assert_eq!(w("docker compose run --rm other sh -lc 'x'"), None);
        assert_eq!(w("docker compose run --rm workspace sh -lc 'a' && docker ps"), None);
        assert_eq!(w("docker compose run --rm workspace sh -lc 'a' 'b'"), None);
    }

    #[test]
    fn wrap_workspace_roundtrips_and_escapes_quotes() {
        let w = wrap_workspace(" npm ci && npm test ");
        assert_eq!(
            w,
            "docker compose -f .nants/compose.yaml run --rm --service-ports workspace sh -lc 'npm ci && npm test'"
        );
        assert_eq!(unwrap_workspace_command(&w).as_deref(), Some("npm ci && npm test"));
        assert!(wrap_workspace("echo 'a'").contains("'\\''a'\\''"));
    }

    #[test]
    fn rejects_container_tools() {
        assert!(normalize("docker ps").is_err());
        assert!(normalize("npm ci && podman run x").is_err());
        assert!(normalize("/usr/bin/docker compose up").is_err());
        assert!(normalize("docker-compose run workspace sh -lc 'docker ps'").is_err());
        assert_eq!(normalize("npm run docker:build").as_deref(), Ok("npm run docker:build"));
        assert_eq!(
            normalize("docker compose run --rm workspace sh -lc 'npm test'").as_deref(),
            Ok("npm test")
        );
        assert!(DOCKER_COMMAND_MSG.contains("npm ci && npm test"));
    }

    #[test]
    fn migrates_real_app_test_04_settings_idempotently() {
        let mut s = Settings::parse(REAL).expect("parse");
        assert!(migrate_wrapped(&mut s));
        assert_eq!(s.test_command, "npm ci && npm test");
        assert_eq!(s.checks[0].command, "npm ci && npm run typecheck");
        assert_eq!(s.checks[1].command, "npm ci && npm run build");
        assert!(s.checks.iter().all(|c| c.blocking));
        assert!(!migrate_wrapped(&mut s));
    }

    #[test]
    fn migrate_wrapped_also_unwraps_saved_formatters() {
        let wrapped = "docker compose -f .nants/compose.yaml run --rm workspace sh -lc 'npm ci && npx prettier --write .'";
        let text = format!(
            "{REAL}formatters:\n- name: web\n  command: \"{wrapped}\"\n  workdir: web\n  paths: [\"web/**\"]\n- name: rust\n  command: cargo fmt\n"
        );
        let mut s = Settings::parse(&text).expect("parse");
        assert_eq!(s.formatters.len(), 2);
        assert!(migrate_wrapped(&mut s));
        assert_eq!(s.formatters[0].command, "npm ci && npx prettier --write .");
        assert_eq!(s.formatters[0].workdir, "web");
        assert_eq!(s.formatters[1].command, "cargo fmt");
        assert!(!migrate_wrapped(&mut s));
        // Só formatadores embrulhados já bastam para marcar a alteração.
        let mut only = Settings::parse(&text.replace("test_command: docker compose", "test_command: npm test #")).expect("parse");
        only.test_command = "npm test".into();
        only.checks.clear();
        only.formatters[0].command = wrapped.into();
        assert!(migrate_wrapped(&mut only));
    }

    #[test]
    fn host_projects_are_not_migrated() {
        let mut s = Settings::parse(&REAL.replace("environment: docker\n", "")).expect("parse");
        assert!(!migrate_wrapped(&mut s));
    }
}
