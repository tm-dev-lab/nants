//! Testes mais baratos (spec 124 T05): reaproveitar a rodada do Desenvolvedor, níveis de teste e
//! instalação das dependências só quando o arquivo de trava muda. Nada aqui conhece uma stack.
//!
//! # O hash da árvore
//!
//! [`tree_hash`] é o `tree` que o Git gravaria com o conteúdo do disco da pasta naquele momento:
//! - entram todos os arquivos que o Git veria, rastreados ou não (código, testes, configuração,
//!   arquivos de trava, `.nants/` com o ambiente), com conteúdo, nome e modo (executável ou link);
//! - ficam fora os caminhos ignorados pelo `.gitignore` do projeto (dependências instaladas e
//!   artefatos de build), o `.sdd/` (estado do fluxo) e o `.nants-outputs/` (saídas longas das
//!   ferramentas).
//!
//! # Quando a rodada do Desenvolvedor dispensa a do runner
//!
//! Ela nunca substitui a rodada limpa final: a bateria completa (`full_test_command` ou, sem ela,
//! o `test_command`) sempre roda pelo runner, com o estado recriado, antes da revisão, e é só essa
//! que a integração aceita para não repetir na base. A do Desenvolvedor dispensa o nível rápido e
//! as checagens bloqueantes da tentativa (estas rodam de novo na integração).
//!
//! Todas as condições, senão o runner repete (na dúvida, repete):
//! - o comando é exatamente (sem espaços nas bordas) o `test_command`, o `full_test_command` ou o
//!   de uma checagem bloqueante do projeto;
//! - rodou no mesmo ambiente do runner (no container do projeto quando ele é Docker; no host,
//!   só comandos sem nada que o shell interprete, porque ali a ferramenta não usa shell);
//! - o ambiente aprovado é o mesmo (modo e hash dos arquivos do ambiente);
//! - passou (código de saída 0, sem tempo esgotado) e coube no limite de tempo da rodada do runner;
//! - a árvore não mudou durante a rodada e é idêntica à que o runner vai testar;
//! - nenhuma rodada do mesmo comando sobre a mesma árvore falhou.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use super::{Environment, Settings};
use crate::harness::git::Git;

/// Caminhos fora do hash da árvore, além dos ignorados pelo `.gitignore`.
pub const TREE_EXCLUDES: &[&str] = &[".sdd", ".nants-outputs"];

/// Rodadas guardadas no registro (as mais antigas saem).
const LEDGER_MAX_RUNS: usize = 32;

/// Arquivo do estado da instalação, no diretório Git privado da pasta (some com a worktree).
const INSTALL_STATE_FILE: &str = "nants-install.state";

/// Hash da árvore de `dir` (ver o módulo); `None` em qualquer erro do Git.
pub fn tree_hash(dir: &Path) -> Option<String> {
    Git::new(dir.to_path_buf()).worktree_tree_hash(TREE_EXCLUDES).ok().filter(|h| !h.is_empty())
}

/// `true` se o comando não tem nada que o shell interprete (só palavras separadas por espaço):
/// então rodar com `sh -c` (runner no host) e sem shell (ferramenta do Desenvolvedor no host) é o
/// mesmo. A primeira palavra não pode ser atribuição de variável.
pub fn plain_command(command: &str) -> bool {
    let command = command.trim();
    let ok = |c: char| c.is_ascii_alphanumeric() || " -_./:,@+%=".contains(c);
    !command.is_empty()
        && command.chars().all(ok)
        && !command.split_whitespace().next().is_some_and(|w| w.contains('='))
}

/// Chave do ambiente em que a rodada vale: o modo e o hash dos arquivos aprovados.
pub fn environment_key(settings: &Settings) -> String {
    format!("{:?}|{}", settings.environment, settings.environment_hash.as_deref().unwrap_or(""))
}

/// Uma rodada de comando do Desenvolvedor observada pela ferramenta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteRun {
    /// Comando exatamente como rodou (sem espaços nas bordas).
    pub command: String,
    /// Hash da árvore antes e depois da rodada (ver [`tree_hash`]).
    pub tree_before: Option<String>,
    pub tree_after: Option<String>,
    /// Rodou no container do projeto (Docker).
    pub in_container: bool,
    pub passed: bool,
    pub elapsed: Duration,
    pub started: SystemTime,
    /// Saída como o Desenvolvedor a viu.
    pub output: String,
}

/// Rodada guardada, com a chave do ambiente de quando rodou.
#[derive(Debug, Clone)]
struct Recorded {
    run: SuiteRun,
    environment: String,
}

#[derive(Debug, Default)]
struct LedgerState {
    /// Comandos observados (os do projeto que o runner aceita reaproveitar).
    watched: Vec<String>,
    /// Chave do ambiente em vigor ([`environment_key`]).
    environment: String,
    runs: VecDeque<Recorded>,
}

/// Registro compartilhado entre o runner e a ferramenta do Desenvolvedor: o runner diz quais
/// comandos observar; a ferramenta registra cada rodada deles; o runner consulta na entrega.
/// Fica só na memória da execução: nada que o Desenvolvedor escreva no disco o altera.
#[derive(Debug, Clone, Default)]
pub struct SuiteLedger {
    inner: Arc<Mutex<LedgerState>>,
}

/// É um canal, não um dado do pedido: dois pedidos iguais continuam iguais com registros diferentes.
impl PartialEq for SuiteLedger {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for SuiteLedger {}

impl SuiteLedger {
    fn state(&self) -> std::sync::MutexGuard<'_, LedgerState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Define os comandos observados e o ambiente em vigor (a cada sessão do Desenvolvedor).
    pub fn watch(&self, commands: Vec<String>, environment: String) {
        let mut state = self.state();
        state.watched = commands.into_iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
        state.environment = environment;
    }

    /// O comando é um dos observados?
    pub fn watches(&self, command: &str) -> bool {
        let command = command.trim();
        self.state().watched.iter().any(|w| w == command)
    }

    /// Registra uma rodada (só de comando observado).
    pub fn record(&self, mut run: SuiteRun) {
        run.command = run.command.trim().to_string();
        let mut state = self.state();
        if !state.watched.contains(&run.command) {
            return;
        }
        let environment = state.environment.clone();
        state.runs.push_back(Recorded { run, environment });
        while state.runs.len() > LEDGER_MAX_RUNS {
            state.runs.pop_front();
        }
    }

    /// Rodada do Desenvolvedor que vale pela do runner (ver o módulo): mesmo comando, mesma árvore
    /// (`tree`), mesmo ambiente e mesmo lugar, passou, coube em `limit` e nenhuma outra rodada do
    /// mesmo comando sobre a mesma árvore falhou. Devolve a mais recente.
    pub fn reusable(
        &self,
        command: &str,
        tree: &str,
        environment: &str,
        in_container: bool,
        limit: Option<Duration>,
    ) -> Option<SuiteRun> {
        let command = command.trim();
        if tree.is_empty() || command.is_empty() {
            return None;
        }
        let state = self.state();
        if !state.watched.iter().any(|w| w == command) {
            return None;
        }
        let same: Vec<&Recorded> = state
            .runs
            .iter()
            .filter(|r| {
                r.run.command == command
                    && r.run.tree_before.as_deref() == Some(tree)
                    && r.run.tree_after.as_deref() == Some(tree)
            })
            .collect();
        if same.is_empty() || same.iter().any(|r| !r.run.passed) {
            return None;
        }
        let last = same.last()?;
        let fits = limit.is_none_or(|l| last.run.elapsed <= l);
        (last.environment == environment && last.run.in_container == in_container && fits).then(|| last.run.clone())
    }
}

/// Hash dos arquivos de trava em `dir` (caminho, tamanho e conteúdo de cada um; o ausente conta
/// como ausente): muda quando qualquer um muda, aparece ou some.
pub fn lock_hash(dir: &Path, lockfiles: &[String]) -> String {
    let mut sorted: Vec<&String> = lockfiles.iter().collect();
    sorted.sort();
    sorted.dedup();
    let mut h = Sha256::new();
    for rel in sorted {
        h.update(rel.as_bytes());
        h.update([0]);
        match std::fs::read(dir.join(rel)) {
            Ok(bytes) => {
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(&bytes);
            }
            Err(_) => h.update(b"\xffmissing"),
        }
        h.update([0]);
    }
    hex(&h.finalize())
}

/// Chave da instalação: o comando, o ambiente e o hash das travas. Igual à gravada na pasta =
/// instalação em dia.
pub fn install_key(settings: &Settings, command: &str, dir: &Path) -> String {
    let mut h = Sha256::new();
    h.update(command.trim().as_bytes());
    h.update([0]);
    h.update(environment_key(settings).as_bytes());
    h.update([0]);
    h.update(lock_hash(dir, &settings.lockfiles).as_bytes());
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Onde fica o estado da instalação da pasta: no diretório Git privado dela (a worktree removida
/// leva o estado junto; nunca entra no Git nem no hash da árvore).
fn install_state_path(dir: &Path) -> Option<PathBuf> {
    Git::new(dir.to_path_buf()).private_git_dir().ok().map(|d| d.join(INSTALL_STATE_FILE))
}

/// Linhas do estado gravado: a chave e, desde a spec 126, quando a instalação passou.
fn install_state_lines(dir: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(install_state_path(dir)?).ok()?;
    Some(text.lines().map(|l| l.trim().to_string()).collect())
}

/// Chave da última instalação bem-sucedida na pasta; `None` se nunca instalou ou não dá para ler.
pub fn installed_key(dir: &Path) -> Option<String> {
    install_state_lines(dir)?.into_iter().next().filter(|k| !k.is_empty())
}

/// Quando a última instalação bem-sucedida na pasta passou (o texto gravado com ela); `None` se
/// nunca instalou ou o estado é anterior à spec 126 (só a chave).
pub fn installed_at(dir: &Path) -> Option<String> {
    install_state_lines(dir)?.into_iter().nth(1).filter(|a| !a.is_empty())
}

/// Grava a chave da instalação que acabou de passar e quando (`at`, melhor esforço).
pub fn remember_install(dir: &Path, key: &str, at: &str) {
    if let Some(path) = install_state_path(dir) {
        let _ = std::fs::write(path, format!("{key}\n{}\n", at.trim()));
    }
}

/// Esquece a instalação da pasta: a próxima rodada instala de novo (melhor esforço).
pub fn forget_install(dir: &Path) {
    if let Some(path) = install_state_path(dir) {
        let _ = std::fs::remove_file(path);
    }
}

/// A instalação precisa rodar antes da próxima rodada nesta pasta? Sem arquivos de trava não há
/// como saber se as dependências mudaram: roda sempre (na dúvida, repete).
pub fn install_needed(settings: &Settings, command: &str, dir: &Path) -> bool {
    settings.lockfiles.is_empty() || installed_key(dir).as_deref() != Some(install_key(settings, command, dir).as_str())
}

/// Spec 126 T01: campos de teste (`test_command`, `full_test_command`) que começam com o próprio
/// `install_command` (o mesmo texto, seguido de nada, de espaço ou de um operador do shell), ou
/// seja, que repetem a instalação que o Nants já roda antes. Spec 127 T03: a mesma regra vale para
/// os formatadores e as checagens (`formatters[<nome>].command`, `checks[<nome>].command`). Vazio
/// sem `install_command`. Cada item é (campo, comando).
pub fn repeated_install(settings: &Settings) -> Vec<(String, String)> {
    let Some(install) = settings.install_command.as_deref().map(str::trim).filter(|c| !c.is_empty()) else {
        return Vec::new();
    };
    let starts_with_install = |command: &str| {
        command
            .trim()
            .strip_prefix(install)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace() || "&;|".contains(c)))
    };
    let mut out = Vec::new();
    if starts_with_install(&settings.test_command) {
        out.push(("test_command".to_string(), settings.test_command.trim().to_string()));
    }
    if let Some(full) = settings.full_test_command.as_deref().filter(|f| starts_with_install(f)) {
        out.push(("full_test_command".to_string(), full.trim().to_string()));
    }
    for f in settings.formatters.iter().filter(|f| starts_with_install(&f.command)) {
        out.push((format!("formatters[{}].command", f.name), f.command.trim().to_string()));
    }
    for c in settings.checks.iter().filter(|c| starts_with_install(&c.command)) {
        out.push((format!("checks[{}].command", c.name), c.command.trim().to_string()));
    }
    out
}

/// Spec 127 T03: o campo de [`repeated_install`] é de um comando de teste (e não de um formatador
/// ou de uma checagem)?
pub fn is_test_field(field: &str) -> bool {
    matches!(field, "test_command" | "full_test_command")
}

/// O runner pode aceitar a rodada do Desenvolvedor deste comando no ambiente do projeto? No
/// Docker, os dois rodam o mesmo `sh -lc` no container; no host, só sem nada para o shell.
pub fn reuse_possible(settings: &Settings, command: &str) -> bool {
    settings.environment == Environment::Docker || plain_command(command)
}

/// Comandos do projeto que a ferramenta do Desenvolvedor observa: o `test_command`, o
/// `full_test_command` e os das checagens bloqueantes que podem ser reaproveitados.
pub fn watched_commands(settings: &Settings) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |c: &str| {
        let c = c.trim();
        if !c.is_empty() && reuse_possible(settings, c) && !out.iter().any(|o| o == c) {
            out.push(c.to_string());
        }
    };
    push(&settings.test_command);
    if let Some(full) = &settings.full_test_command {
        push(full);
    }
    for check in settings.checks.iter().filter(|c| c.blocking) {
        push(&check.command);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn settings(yaml: &str) -> Settings {
        Settings::parse(&format!("base_branch: main\nmerge_strategy: squash\n{yaml}")).expect("settings")
    }

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-levels-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        let ok = Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&dir).status().expect("git").success();
        assert!(ok);
        dir
    }

    fn run(cmd: &str, tree: &str, passed: bool) -> SuiteRun {
        SuiteRun {
            command: cmd.into(),
            tree_before: Some(tree.into()),
            tree_after: Some(tree.into()),
            in_container: true,
            passed,
            elapsed: Duration::from_secs(5),
            started: SystemTime::now(),
            output: "ok".into(),
        }
    }

    #[test]
    fn plain_commands_and_watched_list_follow_the_environment() {
        assert!(plain_command("cargo test --lib"));
        assert!(plain_command("python -m pytest tests/unit"));
        for shell in ["npm ci && npm test", "make test | tee x", "echo $HOME", "CI=1 npm test", "go test ./...*", "a; b", ""] {
            assert!(!plain_command(shell), "{shell}");
        }
        let host = settings("test_command: \"npm ci && npm test\"\nfull_test_command: \"make full\"\nchecks:\n  - name: build\n    command: make build\n    blocking: true\n  - name: lint\n    command: make lint\n");
        assert_eq!(watched_commands(&host), vec!["make full".to_string(), "make build".to_string()]);
        let docker = settings("test_command: \"npm ci && npm test\"\nenvironment: docker\n");
        assert_eq!(watched_commands(&docker), vec!["npm ci && npm test".to_string()]);
    }

    #[test]
    fn ledger_reuses_only_a_passing_run_on_the_same_tree_command_and_environment() {
        let ledger = SuiteLedger::default();
        ledger.watch(vec!["make test".into(), " make full ".into()], "Docker|h1".into());
        assert!(ledger.watches(" make test") && ledger.watches("make full") && !ledger.watches("make other"));
        // Comando não observado não fica registrado.
        ledger.record(run("make other", "t1", true));
        assert!(ledger.reusable("make other", "t1", "Docker|h1", true, None).is_none());
        ledger.record(run("make test", "t1", true));
        assert!(ledger.reusable("make test", "t1", "Docker|h1", true, None).is_some());
        // Árvore diferente, outro ambiente, outro lugar ou sem caber no limite: repete.
        assert!(ledger.reusable("make test", "t2", "Docker|h1", true, None).is_none());
        assert!(ledger.reusable("make test", "t1", "Docker|h2", true, None).is_none());
        assert!(ledger.reusable("make test", "t1", "Docker|h1", false, None).is_none());
        assert!(ledger.reusable("make test", "t1", "Docker|h1", true, Some(Duration::from_secs(1))).is_none());
        // A árvore mudou durante a rodada: não vale.
        let mut moved = run("make full", "t1", true);
        moved.tree_after = Some("t9".into());
        ledger.record(moved);
        assert!(ledger.reusable("make full", "t1", "Docker|h1", true, None).is_none());
        // Uma falha sobre a mesma árvore invalida as que passaram.
        ledger.record(run("make test", "t1", false));
        ledger.record(run("make test", "t1", true));
        assert!(ledger.reusable("make test", "t1", "Docker|h1", true, None).is_none());
        // Sem hash, nada.
        assert!(ledger.reusable("make test", "", "Docker|h1", true, None).is_none());
    }

    #[test]
    fn install_runs_only_when_the_lockfile_changes_in_that_folder_and_environment() {
        let dir = repo("install");
        let s = settings("test_command: \"make test\"\ninstall_command: \"make deps\"\nlockfiles:\n  - deps.lock\n  - ../fora.lock\n  - \"*.lock\"\n");
        assert_eq!(s.lockfiles, vec!["deps.lock".to_string()]);
        fs::write(dir.join("deps.lock"), "a=1\n").expect("w");
        assert!(install_needed(&s, "make deps", &dir), "nunca instalou");
        remember_install(&dir, &install_key(&s, "make deps", &dir), "2026-10-25T10:00:00Z");
        assert!(!install_needed(&s, "make deps", &dir), "trava igual");
        // O estado fica fora da árvore: não aparece para o Git.
        let status = Command::new("git").args(["status", "--porcelain"]).current_dir(&dir).output().expect("git");
        assert!(!String::from_utf8_lossy(&status.stdout).contains("nants-install"));
        fs::write(dir.join("deps.lock"), "a=2\n").expect("w");
        assert!(install_needed(&s, "make deps", &dir), "trava mudou");
        remember_install(&dir, &install_key(&s, "make deps", &dir), "2026-10-25T10:00:00Z");
        assert!(!install_needed(&s, "make deps", &dir));
        // Outro ambiente ou outro comando: instala de novo.
        let mut other = s.clone();
        other.environment_hash = Some("novo".into());
        assert!(install_needed(&other, "make deps", &dir));
        assert!(install_needed(&s, "make deps2", &dir));
        // Esquecida (ex.: dependência sumiu), instala de novo.
        forget_install(&dir);
        assert!(install_needed(&s, "make deps", &dir));
        // Sem arquivos de trava, roda sempre.
        let no_locks = settings("test_command: \"make test\"\ninstall_command: \"make deps\"\n");
        remember_install(&dir, &install_key(&no_locks, "make deps", &dir), "2026-10-25T10:00:00Z");
        assert!(install_needed(&no_locks, "make deps", &dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_state_keeps_the_key_and_when_and_reads_the_old_format() {
        let dir = repo("state");
        let s = settings("test_command: \"make test\"\ninstall_command: \"make deps\"\nlockfiles: [deps.lock]\n");
        assert!(installed_at(&dir).is_none());
        let key = install_key(&s, "make deps", &dir);
        remember_install(&dir, &key, "2026-10-25T10:00:00Z");
        assert_eq!(installed_key(&dir).as_deref(), Some(key.as_str()));
        assert_eq!(installed_at(&dir).as_deref(), Some("2026-10-25T10:00:00Z"));
        // Estado gravado antes da spec 126 (só a chave): continua em dia, sem data.
        let path = install_state_path(&dir).expect("estado");
        fs::write(&path, format!("{key}\n")).expect("w");
        assert!(!install_needed(&s, "make deps", &dir));
        assert!(installed_at(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_install_finds_test_commands_that_start_with_the_install() {
        let s = settings("test_command: \"npm ci --no-audit && npm run test:fast\"\nfull_test_command: \"npm run test:full\"\ninstall_command: \"npm ci --no-audit\"\n");
        assert_eq!(repeated_install(&s), vec![("test_command".to_string(), "npm ci --no-audit && npm run test:fast".to_string())]);
        let both = settings("test_command: \"make deps; make fast\"\nfull_test_command: \" make deps \"\ninstall_command: \"make deps\"\n");
        assert_eq!(repeated_install(&both).iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["test_command", "full_test_command"]);
        // Outro comando com o mesmo começo de palavra não conta; sem instalação, nada.
        let other = settings("test_command: \"make depsx && make test\"\ninstall_command: \"make deps\"\n");
        assert!(repeated_install(&other).is_empty());
        assert!(repeated_install(&settings("test_command: \"make deps && make test\"\n")).is_empty());
    }

    /// Spec 127 T03: formatadores e checagens seguem a mesma regra dos comandos de teste.
    #[test]
    fn repeated_install_also_finds_formatters_and_checks() {
        let s = settings(
            "test_command: \"make test\"\ninstall_command: \"npm ci --no-audit --no-fund\"\n\
             formatters:\n- name: prettier\n  command: \"npm ci --no-audit --no-fund && npm exec -- prettier --write .\"\n\
             - name: clean\n  command: \"npm exec -- prettier --write .\"\n\
             checks:\n- name: lint\n  command: \" npm ci --no-audit --no-fund; npm run lint\"\n\
             - name: typecheck\n  command: \"npm ci --no-audit --no-fundx && tsc\"\n",
        );
        let found = repeated_install(&s);
        assert_eq!(
            found,
            vec![
                ("formatters[prettier].command".to_string(), "npm ci --no-audit --no-fund && npm exec -- prettier --write .".to_string()),
                ("checks[lint].command".to_string(), "npm ci --no-audit --no-fund; npm run lint".to_string()),
            ]
        );
        assert!(found.iter().all(|(f, _)| !is_test_field(f)));
        assert!(is_test_field("test_command") && is_test_field("full_test_command"));
    }

    #[test]
    fn lock_hash_sees_content_presence_and_ignores_order() {
        let dir = repo("lockhash");
        let locks = vec!["a.lock".to_string(), "b/c.lock".to_string()];
        let missing = lock_hash(&dir, &locks);
        fs::write(dir.join("a.lock"), "x").expect("w");
        let one = lock_hash(&dir, &locks);
        assert_ne!(one, missing);
        assert_eq!(lock_hash(&dir, &["b/c.lock".to_string(), "a.lock".to_string()]), one);
        fs::write(dir.join("a.lock"), "y").expect("w");
        assert_ne!(lock_hash(&dir, &locks), one);
        let _ = fs::remove_dir_all(&dir);
    }
}
