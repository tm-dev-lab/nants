//! Execução no Docker Compose do projeto (spec 030, T03): projeto `nants-<id>-test`, serviço
//! `workspace` com a worktree em /work, estado limpo antes dos testes e ambiente de subprocesso
//! controlado (nunca herda segredos do processo do Nants).

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_yaml::{Mapping, Value};

use crate::config::ViewPorts;

use crate::sdd::environment::{env_hash, COMPOSE_REL, WORKSPACE_SERVICE, WORKTREE_VAR};

/// Limite de saída capturada por comando.
const MAX_OUTPUT: usize = 64 * 1024;
/// Timeout dos passos de preparo (down, build, up).
const PREPARE_TIMEOUT: Duration = Duration::from_secs(900);
const POLL: Duration = Duration::from_millis(20);

/// Uma chamada a `docker` (args fixos, sem shell).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeCall {
    /// Argumentos após o programa `docker` (começam por `compose`).
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Ambiente COMPLETO do subprocesso (o runner real usa `env_clear`).
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

/// Saída de uma chamada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeOutput {
    pub code: Option<i32>,
    pub output: String,
    pub timed_out: bool,
}

impl ComposeOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
}

/// Executor de chamadas ao compose (trocado por um falso nos testes).
pub trait ComposeRunner: Send + Sync {
    fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String>;
}

/// Executor real: subprocesso `docker` com ambiente limpo.
#[derive(Debug, Clone)]
pub struct RealComposeRunner {
    program: String,
}

impl Default for RealComposeRunner {
    fn default() -> Self {
        Self { program: "docker".into() }
    }
}

impl RealComposeRunner {
    /// Programa alternativo (testes).
    pub fn with_program(program: impl Into<String>) -> Self {
        Self { program: program.into() }
    }
}

/// Captura limitada que guarda o início e o fim da saída (spec 087 T01): o resumo das suítes de
/// teste costuma ficar no fim, então o excedente sai do meio, com a contagem do que foi omitido.
#[derive(Debug)]
pub(crate) struct HeadTail {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    head_cap: usize,
    tail_cap: usize,
    omitted: usize,
}

impl HeadTail {
    /// Divide `total` em cabeça (1/4) e cauda (3/4).
    pub(crate) fn new(total: usize) -> Self {
        let head_cap = total / 4;
        Self { head: Vec::new(), tail: VecDeque::new(), head_cap, tail_cap: total - head_cap, omitted: 0 }
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        let room = self.head_cap.saturating_sub(self.head.len()).min(data.len());
        let (first, mut rest) = data.split_at(room);
        self.head.extend_from_slice(first);
        if rest.is_empty() {
            return;
        }
        if rest.len() >= self.tail_cap {
            self.omitted += self.tail.len() + rest.len() - self.tail_cap;
            self.tail.clear();
            rest = &rest[rest.len() - self.tail_cap..];
        }
        self.tail.extend(rest.iter().copied());
        let excess = self.tail.len().saturating_sub(self.tail_cap);
        if excess > 0 {
            self.tail.drain(..excess);
            self.omitted += excess;
        }
    }

    /// Cabeça + marcador `[… N bytes omitidos …]` (quando houve corte) + cauda; a cauda começa
    /// num início de caractere UTF-8.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        let mut out = self.head;
        let mut tail: Vec<u8> = self.tail.into_iter().collect();
        if self.omitted > 0 {
            let skip = tail.iter().take_while(|b| (**b & 0xC0) == 0x80).count();
            tail.drain(..skip);
            out.extend_from_slice(format!("\n[… {} bytes omitidos …]\n", self.omitted + skip).as_bytes());
        }
        out.extend_from_slice(&tail);
        out
    }
}

fn read_limited(mut r: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut capture = HeadTail::new(MAX_OUTPUT);
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => capture.push(&buf[..n]),
            }
        }
        capture.into_bytes()
    })
}

impl ComposeRunner for RealComposeRunner {
    fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
        let mut child = Command::new(&self.program)
            .args(&call.args)
            .current_dir(&call.cwd)
            .env_clear()
            .envs(call.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("não foi possível iniciar o docker: {e}"))?;
        let out = child.stdout.take().map(read_limited);
        let err = child.stderr.take().map(read_limited);
        let deadline = Instant::now() + call.timeout;
        let (mut code, mut timed_out) = (None, false);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    code = status.code();
                    break;
                }
                Ok(None) => {}
                Err(e) => return Err(format!("falha ao aguardar o docker: {e}")),
            }
            if Instant::now() >= deadline {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            thread::sleep(POLL);
        }
        let mut bytes = out.and_then(|h| h.join().ok()).unwrap_or_default();
        bytes.extend(err.and_then(|h| h.join().ok()).unwrap_or_default());
        let mut output = String::from_utf8_lossy(&bytes).into_owned();
        if timed_out {
            output.push_str(&format!("\n[tempo limite de {}s excedido]\n", call.timeout.as_secs()));
        }
        Ok(ComposeOutput { code, output, timed_out })
    }
}

/// Ambiente do subprocesso: só PATH, HOME, `DOCKER_*` do host e `NANTS_WORKTREE`.
pub fn controlled_env<I>(host: I, worktree: &Path) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut env: Vec<(String, String)> = host
        .into_iter()
        .filter(|(k, _)| k == "PATH" || k == "HOME" || k.starts_with("DOCKER_"))
        .collect();
    env.push((WORKTREE_VAR.to_string(), worktree.to_string_lossy().into_owned()));
    env
}

/// Nome do projeto Compose dos testes.
pub fn test_project_name(project_id: &str) -> String {
    format!("nants-{project_id}-test")
}

/// Nome do projeto Compose da execução para teste manual ("Executar para testar").
pub fn run_project_name(project_id: &str) -> String {
    format!("nants-{project_id}-run")
}

/// Qual dos dois projetos Compose fixos um [`ComposeEnv`] controla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeKind {
    /// `nants-<id>-test`: testes, checks e comandos dos agentes.
    Test,
    /// `nants-<id>-run`: "Executar para testar".
    Run,
}

/// Porta publicada no host por um serviço do compose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedPort {
    pub service: String,
    pub target: u16,
    pub published: u16,
}

/// Linhas de log retornadas por `logs --tail`.
const LOG_TAIL: &str = "200";
/// Timeout de consultas rápidas (ps, logs).
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Persistência do hash aprovado (settings), chamada ao aprovar uma mudança de ambiente.
pub type PersistHash = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// Compose de testes de um projeto Nants.
pub struct ComposeEnv {
    project_id: String,
    runner: Arc<dyn ComposeRunner>,
    /// Hash dos arquivos de ambiente da última imagem construída.
    built: Mutex<Option<String>>,
    /// Hash aprovado no plano (settings); revalidado em TODA chamada.
    approved: Mutex<Option<Vec<String>>>,
    /// Grava o novo hash aprovado (settings do projeto) quando o humano aprova uma mudança.
    persist: Option<PersistHash>,
    /// Diretório privado dos overrides gerados (fora da worktree e do /tmp).
    base_dir: Option<PathBuf>,
    kind: ComposeKind,
    /// Modo `--lan` no `-run`: faixa das portas de visualização; cada porta do workspace é
    /// republicada em 0.0.0.0 com uma porta de host desta faixa (spec 097).
    lan_ports: Option<ViewPorts>,
    /// Portas do host remapeadas (pedida, usada) no último `up_run` por estarem ocupadas.
    remaps: Mutex<Vec<(u16, u16)>>,
}

/// Diretório de dados do Nants para os overrides: `$XDG_DATA_HOME/nants/compose` ou
/// `~/.local/share/nants/compose`.
fn default_base_dir() -> Option<PathBuf> {
    // Spec 075 T03: testes nunca escrevem na pasta de dados real do usuário.
    #[cfg(test)]
    {
        return Some(std::env::temp_dir().join(format!("nants-test-compose-{}", std::process::id())));
    }
    #[cfg(not(test))]
    default_base_dir_real()
}

#[cfg_attr(test, allow(dead_code))]
fn default_base_dir_real() -> Option<PathBuf> {
    crate::config::data_dir().map(|d| d.join("compose"))
}

/// Garante `path` como diretório PRÓPRIO (uid atual), não symlink e modo 0700.
fn ensure_private_dir(path: &Path) -> Result<(), String> {
    let me = fs::metadata("/proc/self").map_err(|e| format!("uid indisponível: {e}"))?.uid();
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("não foi possível criar o diretório do override: {e}")),
    }
    let meta = fs::symlink_metadata(path).map_err(|e| format!("diretório do override ilegível: {e}"))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("diretório do override é symlink ou não é diretório".into());
    }
    if meta.uid() != me {
        return Err("diretório do override pertence a outro usuário".into());
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("não foi possível restringir o diretório do override: {e}"))?;
    }
    Ok(())
}

impl std::fmt::Debug for ComposeEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposeEnv").field("project_id", &self.project_id).finish()
    }
}

impl ComposeEnv {
    pub fn new(project_id: impl Into<String>, runner: Arc<dyn ComposeRunner>) -> Self {
        Self {
            project_id: project_id.into(),
            runner,
            built: Mutex::new(None),
            approved: Mutex::new(None),
            persist: None,
            base_dir: default_base_dir(),
            kind: ComposeKind::Test,
            lan_ports: None,
            remaps: Mutex::new(Vec::new()),
        }
    }

    /// Projeto Compose controlado (padrão: `-test`).
    pub fn with_kind(mut self, kind: ComposeKind) -> Self {
        self.kind = kind;
        self
    }

    /// Modo `--lan` (só vale no `-run`): as portas do `workspace` são republicadas em 0.0.0.0 com
    /// portas de host da faixa `range`. `None` mantém o comportamento local.
    pub fn with_lan_view_ports(mut self, range: Option<ViewPorts>) -> Self {
        self.lan_ports = range;
        self
    }

    /// Portas do host trocadas no último `up_run` (pedida, usada) porque a pedida estava ocupada.
    pub fn port_remaps(&self) -> Vec<(u16, u16)> {
        self.remaps.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Nome do projeto Compose controlado.
    pub fn project_name(&self) -> String {
        match self.kind {
            ComposeKind::Test => test_project_name(&self.project_id),
            ComposeKind::Run => run_project_name(&self.project_id),
        }
    }

    /// Hash aprovado dos arquivos de ambiente: divergência recusa `prepare`, `exec` e `down`.
    pub fn with_approved_hash(mut self, hash: Option<String>) -> Self {
        self.approved = Mutex::new(hash.map(|h| vec![h]));
        self
    }

    /// Conjunto de hashes aprovados; qualquer um deles é aceito. Conjunto VAZIO falha fechado:
    /// nenhuma chamada é executada.
    pub fn with_approved_hashes(mut self, hashes: Vec<String>) -> Self {
        self.approved = Mutex::new(Some(hashes));
        self
    }

    /// Define como o hash aprovado é gravado no settings (ver [`Self::accept_environment`]).
    pub fn with_persist(mut self, persist: PersistHash) -> Self {
        self.persist = Some(persist);
        self
    }

    /// Mudança de ambiente aprovada pelo humano: grava o hash no settings, passa a aceitá-lo nas
    /// próximas chamadas e força o rebuild no próximo `prepare`. Se a gravação falha, nada muda.
    pub fn accept_environment(&self, hash: &str) -> Result<(), String> {
        if let Some(persist) = &self.persist {
            persist(hash)?;
        }
        {
            let mut guard = self.approved.lock().unwrap_or_else(PoisonError::into_inner);
            let set = guard.get_or_insert_with(Vec::new);
            if !set.iter().any(|h| h == hash) {
                set.push(hash.to_string());
            }
        }
        *self.built.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }

    /// Diretório base dos overrides (testes).
    pub fn with_base_dir(mut self, dir: PathBuf) -> Self {
        self.base_dir = Some(dir);
        self
    }

    /// Com o executor real do `docker`.
    pub fn real(project_id: impl Into<String>) -> Self {
        Self::new(project_id, Arc::new(RealComposeRunner::default())).with_approved_hashes(Vec::new())
    }

    /// Diretório privado (0700, do uid atual, sem symlink) do projeto.
    fn override_dir(&self) -> Result<PathBuf, String> {
        let base = self.base_dir.clone().ok_or("diretório de dados do Nants indisponível (HOME)")?;
        if let Some(parent) = base.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("não foi possível criar {}: {e}", parent.display()))?;
        }
        ensure_private_dir(&base)?;
        let dir = base.join(self.project_name());
        ensure_private_dir(&dir)?;
        Ok(dir)
    }

    fn call(&self, dir: &Path, override_file: &Path, tail: &[&str], timeout: Duration) -> ComposeCall {
        let mut args: Vec<String> = vec![
            "compose".into(),
            "-p".into(),
            self.project_name(),
            "-f".into(),
            COMPOSE_REL.into(),
            "-f".into(),
            override_file.to_string_lossy().into_owned(),
        ];
        args.extend(tail.iter().map(|s| s.to_string()));
        ComposeCall { args, cwd: dir.to_path_buf(), env: controlled_env(std::env::vars(), dir), timeout }
    }

    fn step(&self, call: &ComposeCall, what: &str) -> Result<(), String> {
        let out = self.runner.run(call)?;
        if out.success() {
            Ok(())
        } else {
            Err(format!("docker compose {what} falhou: {}", diagnose(&out)))
        }
    }

    fn load(&self, dir: &Path) -> Result<(Value, PathBuf, String), String> {
        self.load_with(dir, None)
    }

    /// Como [`Self::load`]; `run_command` (já normalizado) vira o `command` do workspace no override.
    fn load_with(&self, dir: &Path, run_command: Option<&str>) -> Result<(Value, PathBuf, String), String> {
        let current = env_hash(dir).ok_or("ambiente docker sem .nants/compose.yaml")?;
        let approved = self.approved.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(set) = &approved {
            if set.is_empty() {
                return Err("nenhum hash de ambiente aprovado no settings".into());
            }
            if !set.contains(&current) {
                return Err("arquivos de ambiente (.nants) diferem do aprovado no plano".into());
            }
        }
        let text = fs::read_to_string(dir.join(COMPOSE_REL)).map_err(|e| format!("compose.yaml ilegível: {e}"))?;
        let doc: Value = serde_yaml::from_str(&text).map_err(|_| "compose.yaml inválido".to_string())?;
        let mut remaps = Vec::new();
        let ov = write_override(
            &self.override_dir()?,
            &self.project_id,
            dir,
            &doc,
            run_command,
            self.lan_ports.filter(|_| run_command.is_some()),
            &mut remaps,
        )?;
        if run_command.is_some() {
            *self.remaps.lock().unwrap_or_else(PoisonError::into_inner) = remaps;
        }
        Ok((doc, ov, current))
    }

    /// Estado limpo antes de um `test_command`: override da worktree, `down -v`, build (só se o
    /// hash dos arquivos de ambiente mudou) e `up -d --wait` dos serviços de apoio.
    pub fn prepare(&self, dir: &Path) -> Result<(), String> {
        let (doc, ov, current) = self.load(dir)?;
        self.step(&self.call(dir, &ov, &["down", "-v", "--remove-orphans"], PREPARE_TIMEOUT), "down")?;
        let has_build = doc
            .get("services")
            .and_then(|s| s.get(WORKSPACE_SERVICE))
            .is_some_and(|w| w.get("build").is_some());
        let stale = self.built.lock().unwrap_or_else(PoisonError::into_inner).as_deref() != Some(current.as_str());
        if has_build && stale {
            self.step(&self.call(dir, &ov, &["build", WORKSPACE_SERVICE], PREPARE_TIMEOUT), "build")?;
        }
        *self.built.lock().unwrap_or_else(PoisonError::into_inner) = Some(current);
        let support: Vec<String> = service_names(&doc).into_iter().filter(|s| s != WORKSPACE_SERVICE).collect();
        if !support.is_empty() {
            let mut tail = vec!["up", "-d", "--wait"];
            tail.extend(support.iter().map(String::as_str));
            self.step(&self.call(dir, &ov, &tail, PREPARE_TIMEOUT), "up")?;
        }
        Ok(())
    }

    /// `docker compose run --rm --entrypoint sh workspace -lc <command>` (o entrypoint do compose do
    /// projeto nunca engole o comando). `Err` = falha do Docker (daemon,
    /// subprocesso); o código de saída do comando vai no resultado.
    pub fn exec(&self, dir: &Path, command: &str, timeout: Duration) -> Result<ComposeOutput, String> {
        let (_, ov, _) = self.load(dir)?;
        let call = self.call(dir, &ov, &["run", "--rm", "-T", "--entrypoint", "sh", WORKSPACE_SERVICE, "-lc", command], timeout);
        let out = self.runner.run(&call)?;
        if !out.success() && is_docker_failure(&out.output) {
            return Err(format!("{DOCKER_UNAVAILABLE}: {}", diagnose(&out)));
        }
        Ok(out)
    }

    /// Devolve ao uid do host as entradas de `dir` criadas por outro dono (ex.: root de imagens
    /// antigas), via container efêmero do workspace (`--user 0`, `chown`, sem shell e sem sudo).
    /// Restrito a `dir` canonicalizado; symlinks não são seguidos. Sem entradas alheias, não chama
    /// o docker. Devolve quantas entradas foram reparadas.
    pub fn repair_ownership(&self, dir: &Path) -> Result<usize, String> {
        let (uid, gid) = host_ids()?;
        self.repair_as(dir, uid, gid)
    }

    fn repair_as(&self, dir: &Path, uid: u32, gid: u32) -> Result<usize, String> {
        let abs = dir.canonicalize().map_err(|e| format!("diretório inválido: {e}"))?;
        let rels = foreign_entries(&abs, uid);
        if rels.is_empty() {
            return Ok(0);
        }
        let (_, ov, _) = self.load(&abs)?;
        let tail = chown_tail(uid, gid, &rels)?;
        let tail: Vec<&str> = tail.iter().map(String::as_str).collect();
        self.step(&self.call(&abs, &ov, &tail, PREPARE_TIMEOUT), "run chown")?;
        Ok(rels.len())
    }

    /// Derruba o projeto de testes (volumes e redes). Idempotente.
    pub fn down(&self, dir: &Path) -> Result<(), String> {
        self.down_with(dir, true)
    }

    /// `down --remove-orphans`, com `-v` só se `volumes`. Imagens são sempre mantidas. Idempotente.
    pub fn down_with(&self, dir: &Path, volumes: bool) -> Result<(), String> {
        let (_, ov, _) = self.load(dir)?;
        let mut tail = vec!["down"];
        if self.kind == ComposeKind::Run {
            // Parada rápida: o processo do container não é o PID 1 real (init: true repassa sinais).
            tail.extend(["--timeout", "5"]);
        }
        if volumes {
            tail.push("-v");
        }
        tail.push("--remove-orphans");
        self.step(&self.call(dir, &ov, &tail, PREPARE_TIMEOUT), "down")
    }

    /// Sobe o projeto de execução: `up -d --build --wait` (todos os serviços). `run_command` é o
    /// comando INTERNO que o `workspace` executa (`sh -lc <run_command>`), já normalizado (039).
    pub fn up_run(&self, dir: &Path, run_command: &str) -> Result<(), String> {
        let (_, ov, _) = self.load_with(dir, Some(run_command))?;
        self.step(&self.call(dir, &ov, &["up", "-d", "--build", "--wait"], PREPARE_TIMEOUT), "up")
    }

    /// Observa o `workspace` após o `up` por até `window`: devolve `Err` se o container saiu
    /// (o `--wait` só garante "running" no instante). Encerra antes se uma porta publicada responder
    /// (ao menos 1 byte a um GET); porta muda com container vivo é ok (apps lentos). Janela zero = sem observação.
    pub fn observe_run(&self, dir: &Path, window: Duration) -> Result<(), String> {
        if window.is_zero() {
            return Ok(());
        }
        let (_, ov, _) = self.load(dir)?;
        let start = Instant::now();
        loop {
            let out = self.runner.run(&self.call(dir, &ov, &["ps", "--all", "--format", "json"], QUERY_TIMEOUT))?;
            if out.success() {
                if let Some(state) = workspace_exit(&out.output) {
                    return Err(format!("o app encerrou logo após subir ({state})"));
                }
                if parse_ps_ports(&out.output).iter().any(|p| port_responds(p.published)) {
                    return Ok(());
                }
            }
            if start.elapsed() >= window {
                return Ok(());
            }
            thread::sleep(OBSERVE_POLL.min(window.saturating_sub(start.elapsed())));
        }
    }

    /// Portas TCP publicadas no host (`ps --format json`), sem duplicatas (IPv4/IPv6).
    pub fn published_ports(&self, dir: &Path) -> Result<Vec<PublishedPort>, String> {
        let (_, ov, _) = self.load(dir)?;
        let out = self.runner.run(&self.call(dir, &ov, &["ps", "--format", "json"], QUERY_TIMEOUT))?;
        if !out.success() {
            return Err(format!("docker compose ps falhou: {}", diagnose(&out)));
        }
        Ok(parse_ps_ports(&out.output))
    }

    /// Últimas `lines` linhas de log do `workspace` (diagnóstico de falha do `up`).
    pub fn workspace_logs(&self, dir: &Path, lines: u32) -> Result<String, String> {
        let (_, ov, _) = self.load(dir)?;
        let n = lines.to_string();
        let tail = ["logs", "--no-color", "--tail", n.as_str(), WORKSPACE_SERVICE];
        let out = self.runner.run(&self.call(dir, &ov, &tail, QUERY_TIMEOUT))?;
        if !out.success() {
            return Err(format!("docker compose logs falhou: {}", diagnose(&out)));
        }
        Ok(out.output)
    }

    /// Logs recentes (`logs --no-color --tail 200`).
    pub fn logs(&self, dir: &Path) -> Result<String, String> {
        let (_, ov, _) = self.load(dir)?;
        let out = self.runner.run(&self.call(dir, &ov, &["logs", "--no-color", "--tail", LOG_TAIL], QUERY_TIMEOUT))?;
        if !out.success() {
            return Err(format!("docker compose logs falhou: {}", diagnose(&out)));
        }
        Ok(out.output)
    }
}

/// Intervalo entre consultas durante a observação pós-`up`.
const OBSERVE_POLL: Duration = Duration::from_millis(1000);

/// `Some(descrição)` se o serviço `workspace` consta como encerrado no `ps --all --format json`.
fn workspace_exit(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let items: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Array(a)) => a,
        Ok(v @ serde_json::Value::Object(_)) => vec![v],
        _ => trimmed.lines().filter_map(|l| serde_json::from_str(l.trim()).ok()).collect(),
    };
    items.iter().find_map(|i| {
        if i.get("Service").and_then(|v| v.as_str()) != Some(WORKSPACE_SERVICE) {
            return None;
        }
        let state = i.get("State").and_then(|v| v.as_str())?;
        if state == "exited" || state == "dead" {
            let code = i.get("ExitCode").and_then(|v| v.as_i64()).map_or(String::new(), |c| format!(", código {c}"));
            Some(format!("{state}{code}"))
        } else {
            None
        }
    })
}

/// A porta publicada responde de verdade? O docker-proxy aceita o connect mesmo com o backend
/// morto, então exige ao menos 1 byte de resposta a um GET (fechada sem bytes = não pronta).
fn port_responds(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    let wait = Some(Duration::from_millis(500));
    let _ = (s.set_read_timeout(wait), s.set_write_timeout(wait));
    if s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return false;
    }
    let mut b = [0u8; 1];
    matches!(s.read(&mut b), Ok(n) if n > 0)
}

/// Extrai as portas publicadas da saída de `compose ps --format json` (array JSON ou uma linha
/// JSON por container, conforme a versão do Compose).
pub fn parse_ps_ports(text: &str) -> Vec<PublishedPort> {
    let trimmed = text.trim();
    let items: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Array(a)) => a,
        Ok(v @ serde_json::Value::Object(_)) => vec![v],
        _ => trimmed.lines().filter_map(|l| serde_json::from_str(l.trim()).ok()).collect(),
    };
    let mut ports: Vec<PublishedPort> = Vec::new();
    for item in &items {
        let service = item.get("Service").and_then(|v| v.as_str()).unwrap_or("");
        let Some(pubs) = item.get("Publishers").and_then(|v| v.as_array()) else { continue };
        for p in pubs {
            let num = |k: &str| p.get(k).and_then(|v| v.as_u64()).and_then(|n| u16::try_from(n).ok());
            let tcp = p.get("Protocol").and_then(|v| v.as_str()).is_none_or(|x| x == "tcp");
            let (Some(target), Some(published)) = (num("TargetPort"), num("PublishedPort")) else { continue };
            if published == 0 || !tcp || service.is_empty() {
                continue;
            }
            let port = PublishedPort { service: service.to_string(), target, published };
            if !ports.contains(&port) {
                ports.push(port);
            }
        }
    }
    ports.sort_by(|a, b| (a.published, &a.service).cmp(&(b.published, &b.service)));
    ports
}

/// Saída do Docker indicando porta do host em uso.
pub(crate) fn is_port_conflict(output: &str) -> bool {
    output.contains("port is already allocated") || output.contains("address already in use")
}

pub(crate) fn is_docker_failure(output: &str) -> bool {
    output.contains("Cannot connect to the Docker daemon")
        || output.contains("Is the docker daemon running")
        || output.contains("failed to connect to the docker API")
}

/// Prefixos que `diagnose` coloca no erro quando a causa NÃO é arquivo do ambiente.
pub(crate) const DIAG_TIMEOUT: &str = "tempo limite excedido";
pub(crate) const DIAG_PORT: &str = "porta do host já em uso";
pub(crate) const DIAG_DAEMON: &str = "o daemon do Docker não está acessível";
/// Prefixo do erro de `exec` quando o docker não está disponível.
pub(crate) const DOCKER_UNAVAILABLE: &str = "docker indisponível";

/// `true` quando o motivo do erro (de `step`/`exec`) tem causa fora dos arquivos do ambiente.
pub(crate) fn is_non_file_cause(reason: &str) -> bool {
    [DIAG_TIMEOUT, DIAG_PORT, DIAG_DAEMON, DOCKER_UNAVAILABLE].iter().any(|m| reason.contains(m)) || is_port_conflict(reason) || is_docker_failure(reason)
}

fn diagnose(out: &ComposeOutput) -> String {
    let tail: String = out.output.trim_end().chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
    if out.timed_out {
        format!("{DIAG_TIMEOUT}\n{tail}")
    } else if is_port_conflict(&out.output) {
        format!("{DIAG_PORT}\n{tail}")
    } else if is_docker_failure(&out.output) {
        format!("{DIAG_DAEMON}\n{tail}")
    } else {
        tail
    }
}

fn service_names(doc: &Value) -> Vec<String> {
    doc.get("services")
        .and_then(Value::as_mapping)
        .map(|m| m.keys().filter_map(|k| k.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// uid:gid do processo do Nants.
fn host_ids() -> Result<(u32, u32), String> {
    let m = fs::metadata("/proc/self").map_err(|e| format!("uid indisponível: {e}"))?;
    Ok((m.uid(), m.gid()))
}

/// Entradas sob `dir` que NÃO pertencem a `uid`, como caminhos relativos (sem seguir symlink e
/// sem descer em entrada de outro dono: o `chown -R` cobre o conteúdo). Ignora `.git` e
/// `.sdd/worktrees` da raiz.
pub fn foreign_entries(dir: &Path, uid: u32) -> Vec<PathBuf> {
    fn walk(root: &Path, rel: &Path, uid: u32, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(root.join(rel)) else { return };
        for e in rd.flatten() {
            let r = rel.join(e.file_name());
            if r == Path::new(".git") || r == Path::new(".sdd/worktrees") {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(e.path()) else { continue };
            if meta.uid() != uid {
                out.push(r);
            } else if meta.is_dir() {
                walk(root, &r, uid, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, Path::new(""), uid, &mut out);
    out.sort();
    out
}

/// Argumentos (após o prefixo do compose) do `chown` efêmero: imagem do workspace, `--user 0`,
/// sem shell. Só caminhos relativos normais (sem `..`, absolutos ou controle) são aceitos.
fn chown_tail(uid: u32, gid: u32, rels: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut tail: Vec<String> =
        ["run", "--rm", "--no-deps", "-T", "--user", "0", "--entrypoint", "chown", WORKSPACE_SERVICE, "-h", "-R"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    tail.push(format!("{uid}:{gid}"));
    tail.push("--".into());
    for r in rels {
        let ok = r.components().all(|c| matches!(c, std::path::Component::Normal(_)));
        let text = r.to_string_lossy();
        if !ok || r.as_os_str().is_empty() || text.chars().any(char::is_control) {
            return Err(format!("caminho inválido para reparo de posse: {}", r.display()));
        }
        tail.push(format!("/work/{text}"));
    }
    Ok(tail)
}

/// Endereço do host das portas publicadas em `--lan`.
const LAN_HOST_IP: &str = "0.0.0.0";

/// Partes de uma porta na sintaxe curta: portas do container (intervalo expandido) e o sufixo
/// de protocolo (`/udp`). `None` se a entrada não é reconhecida.
fn short_port_targets(s: &str) -> Option<(Vec<u16>, String)> {
    let (body, proto) = match s.split_once('/') {
        Some((b, pr)) => (b, format!("/{pr}")),
        None => (s, String::new()),
    };
    // IPv6 entre colchetes: `[::1]:8080:80`; o endereço é descartado (em `--lan` é sempre 0.0.0.0).
    let body = match body.strip_prefix('[') {
        Some(rest) => rest.split_once("]:")?.1,
        None => body,
    };
    let parts: Vec<&str> = body.split(':').collect();
    let container = match parts.as_slice() {
        [c] | [_, c] | [_, _, c] => *c,
        _ => return None,
    };
    let targets = match container.split_once('-') {
        Some((a, b)) => {
            let (a, b) = (a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?);
            if a > b {
                return None;
            }
            (a..=b).collect()
        }
        None => vec![container.trim().parse::<u16>().ok()?],
    };
    Some((targets, proto))
}

/// Portas do serviço em `--lan` (spec 097): cada porta do container recebe a primeira porta livre
/// da faixa `range` como porta do host, publicada em 0.0.0.0. A porta do container não muda;
/// mapeamentos só com a porta do container também entram na faixa. Sem porta livre, devolve a
/// mensagem de faixa esgotada (nunca cai numa porta fora da faixa). `Ok(None)` sem portas.
fn lan_view_port_list(
    doc: &Value,
    service: &str,
    range: ViewPorts,
    free: &dyn Fn(&str, u16) -> bool,
) -> Result<Option<Value>, String> {
    let Some(list) = doc.get("services").and_then(|s| s.get(service)).and_then(|s| s.get("ports")).and_then(Value::as_sequence) else {
        return Ok(None);
    };
    if list.is_empty() {
        return Ok(None);
    }
    let mut used: Vec<u16> = Vec::new();
    let mut next = || -> Result<u16, String> {
        let port = range
            .ports()
            .find(|p| !used.contains(p) && free(LAN_HOST_IP, *p))
            .ok_or_else(|| range.exhausted_message())?;
        used.push(port);
        Ok(port)
    };
    let mut out = Vec::new();
    for p in list {
        match p {
            Value::String(s) => match short_port_targets(s) {
                Some((targets, proto)) => {
                    for target in targets {
                        out.push(Value::String(format!("{LAN_HOST_IP}:{}:{target}{proto}", next()?)));
                    }
                }
                None => out.push(p.clone()),
            },
            Value::Number(n) => match n.as_u64().and_then(|n| u16::try_from(n).ok()) {
                Some(target) => out.push(Value::String(format!("{LAN_HOST_IP}:{}:{target}", next()?))),
                None => out.push(p.clone()),
            },
            Value::Mapping(m) if m.get("target").is_some() => {
                let mut m = m.clone();
                m.insert("host_ip".into(), LAN_HOST_IP.into());
                m.insert("published".into(), Value::Number(u64::from(next()?).into()));
                out.push(Value::Mapping(m));
            }
            other => out.push(other.clone()),
        }
    }
    Ok(Some(Value::Sequence(out)))
}

/// Override gerado pelo Nants: monta a worktree em /work no `workspace` (não confia no caminho do
/// compose do modelo) e aplica o label `nants.project=<id>` a serviços, volumes e redes.
#[cfg(test)]
fn override_yaml(
    project_id: &str,
    worktree: &Path,
    doc: &Value,
    run_command: Option<&str>,
    lan_ports: Option<ViewPorts>,
) -> Result<String, String> {
    override_yaml_with(project_id, worktree, doc, run_command, lan_ports, &mut Vec::new(), &|_, _| true, &|_| None)
}

/// Porta do host livre para bind em `ip`?
fn host_port_free(ip: &str, port: u16) -> bool {
    match ip.parse::<std::net::IpAddr>() {
        Ok(addr) => std::net::TcpListener::bind((addr, port)).is_ok(),
        Err(_) => true,
    }
}

/// Porta livre escolhida pelo SO (bind em :0).
fn host_port_alloc(ip: &str) -> Option<u16> {
    let addr = ip.parse::<std::net::IpAddr>().ok()?;
    std::net::TcpListener::bind((addr, 0)).ok()?.local_addr().ok().map(|a| a.port())
}

/// Troca a porta do host ocupada por uma livre nas entradas fixas de `ports` (container intacto).
fn remap_busy_ports(
    list: Value,
    remaps: &mut Vec<(u16, u16)>,
    free: &dyn Fn(&str, u16) -> bool,
    alloc: &dyn Fn(&str) -> Option<u16>,
) -> Value {
    let Value::Sequence(items) = list else { return list };
    let mut pick = |ip: &str, want: u16| -> Option<u16> {
        if free(ip, want) {
            return None;
        }
        let new = alloc(ip)?;
        remaps.push((want, new));
        Some(new)
    };
    let out = items
        .into_iter()
        .map(|p| match p {
            Value::String(s) => {
                let (body, proto) = match s.split_once('/') {
                    Some((b, pr)) => (b.to_string(), Some(pr.to_string())),
                    None => (s.clone(), None),
                };
                if proto.as_deref().is_some_and(|x| x != "tcp") || body.contains('[') {
                    return Value::String(s);
                }
                let parts: Vec<&str> = body.split(':').collect();
                let (ip, host, rest) = match parts.as_slice() {
                    [h, c] => ("0.0.0.0", *h, vec![*c]),
                    [ip, h, c] => (*ip, *h, vec![*c]),
                    _ => return Value::String(s),
                };
                let Ok(want) = host.parse::<u16>() else { return Value::String(s) };
                match pick(ip, want) {
                    Some(new) => {
                        let mut v = if parts.len() == 3 { format!("{ip}:{new}") } else { new.to_string() };
                        v.push(':');
                        v.push_str(rest[0]);
                        if let Some(pr) = proto {
                            v.push('/');
                            v.push_str(&pr);
                        }
                        Value::String(v)
                    }
                    None => Value::String(s),
                }
            }
            Value::Mapping(mut m) => {
                let published = match m.get("published") {
                    Some(Value::Number(n)) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
                    Some(Value::String(s)) => s.parse::<u16>().ok(),
                    _ => None,
                };
                let tcp = m.get("protocol").and_then(Value::as_str).is_none_or(|x| x == "tcp");
                let ip = m.get("host_ip").and_then(Value::as_str).unwrap_or("0.0.0.0").to_string();
                if let (Some(want), true) = (published, tcp) {
                    if let Some(new) = pick(&ip, want) {
                        m.insert("published".into(), Value::Number(u64::from(new).into()));
                    }
                }
                Value::Mapping(m)
            }
            other => other,
        })
        .collect();
    Value::Sequence(out)
}

#[allow(clippy::too_many_arguments)]
fn override_yaml_with(
    project_id: &str,
    worktree: &Path,
    doc: &Value,
    run_command: Option<&str>,
    lan_ports: Option<ViewPorts>,
    remaps: &mut Vec<(u16, u16)>,
    free: &dyn Fn(&str, u16) -> bool,
    alloc: &dyn Fn(&str) -> Option<u16>,
) -> Result<String, String> {
    let label = Value::Sequence(vec![Value::String(format!("nants.project={project_id}"))]);
    let abs = worktree.canonicalize().map_err(|e| format!("worktree inválida: {e}"))?;
    let src = abs.to_string_lossy().replace('$', "$$");
    let mut services = Mapping::new();
    for name in service_names(doc) {
        let mut svc = Mapping::new();
        svc.insert("labels".into(), label.clone());
        if name == WORKSPACE_SERVICE {
            let mut bind = Mapping::new();
            bind.insert("type".into(), "bind".into());
            bind.insert("source".into(), Value::String(src.clone()));
            bind.insert("target".into(), "/work".into());
            let mut vols = vec![Value::Mapping(bind)];
            // Isolamento estrutural (spec 062): `.git` (arquivo ou diretório) e `.nants` só leitura.
            for prot in [".git", ".nants"] {
                if abs.join(prot).symlink_metadata().is_ok() {
                    let mut ro = Mapping::new();
                    ro.insert("type".into(), "bind".into());
                    ro.insert("source".into(), Value::String(abs.join(prot).to_string_lossy().replace('$', "$$")));
                    ro.insert("target".into(), Value::String(format!("/work/{prot}")));
                    ro.insert("read_only".into(), Value::Bool(true));
                    vols.push(Value::Mapping(ro));
                }
            }
            svc.insert("volumes".into(), Value::Sequence(vols));
            svc.insert("working_dir".into(), "/work".into());
            // Runtime com o uid do host: arquivos criados na worktree não ficam como root.
            let (uid, gid) = host_ids()?;
            svc.insert("user".into(), Value::String(format!("{uid}:{gid}")));
            let mut env = Mapping::new();
            env.insert("HOME".into(), "/tmp".into());
            svc.insert("environment".into(), Value::Mapping(env));
            if run_command.is_some() {
                // `init: true` repassa SIGTERM ao processo (senão o `down` espera 10 s).
                svc.insert("init".into(), Value::Bool(true));
            }
            if let Some(cmd) = run_command {
                // Forma exec (lista) com o comando como UM argumento; `$` escapado contra a
                // interpolação do Compose.
                // O entrypoint também é fixado: o do compose do projeto (ex.: `/bin/sh -c`) não
                // pode engolir o comando.
                svc.insert("entrypoint".into(), Value::Sequence(["sh", "-lc"].map(|a| Value::String(a.into())).to_vec()));
                svc.insert("command".into(), Value::Sequence(vec![Value::String(cmd.replace('$', "$$"))]));
            }
            // `!override` substitui a lista do compose base (senão o Compose a concatenaria).
            let tagged = |ports: Value| Value::Tagged(Box::new(serde_yaml::value::TaggedValue { tag: serde_yaml::value::Tag::new("override"), value: ports }));
            if let Some(range) = lan_ports {
                // `--lan` (spec 097): toda porta do host vem da faixa de visualização.
                if let Some(ports) = lan_view_port_list(doc, WORKSPACE_SERVICE, range, free)? {
                    svc.insert("ports".into(), tagged(ports));
                }
            } else if run_command.is_some() {
                let base = doc.get("services").and_then(|s| s.get(WORKSPACE_SERVICE)).and_then(|s| s.get("ports")).filter(|p| p.as_sequence().is_some_and(|l| !l.is_empty())).cloned();
                if let Some(ports) = base {
                    let before = remaps.len();
                    let ports = remap_busy_ports(ports, remaps, free, alloc);
                    if remaps.len() > before {
                        svc.insert("ports".into(), tagged(ports));
                    }
                }
            }
        }
        services.insert(Value::String(name), Value::Mapping(svc));
    }
    let mut root = Mapping::new();
    root.insert("services".into(), Value::Mapping(services));
    for top in ["volumes", "networks"] {
        if let Some(m) = doc.get(top).and_then(Value::as_mapping) {
            let mut out = Mapping::new();
            for k in m.keys() {
                let mut def = Mapping::new();
                def.insert("labels".into(), label.clone());
                out.insert(k.clone(), Value::Mapping(def));
            }
            root.insert(top.into(), Value::Mapping(out));
        }
    }
    serde_yaml::to_string(&Value::Mapping(root)).map_err(|e| format!("override inválido: {e}"))
}

/// Grava o override em `dir` (privado): remove o arquivo anterior (unlink não segue symlink) e
/// cria um novo com `create_new` (O_EXCL: nunca segue symlink) e modo 0600.
fn write_override(
    dir: &Path,
    project_id: &str,
    worktree: &Path,
    doc: &Value,
    run_command: Option<&str>,
    lan_ports: Option<ViewPorts>,
    remaps: &mut Vec<(u16, u16)>,
) -> Result<PathBuf, String> {
    let text = override_yaml_with(project_id, worktree, doc, run_command, lan_ports, remaps, &host_port_free, &host_port_alloc)?;
    let path = dir.join("override.yaml");
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("não foi possível substituir o override: {e}")),
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("não foi possível gravar o override: {e}"))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("não foi possível gravar o override: {e}"))?;
    Ok(path)
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;

    /// Runner falso: registra chamadas e responde por subcomando.
    #[derive(Default)]
    pub struct FakeRunner {
        pub calls: Mutex<Vec<ComposeCall>>,
        replies: Mutex<Vec<(String, ComposeOutput)>>,
        /// Espera antes de responder (simula comando demorado).
        pub delay: Mutex<Option<Duration>>,
    }

    impl FakeRunner {
        pub fn reply(&self, sub: &str, code: i32, output: &str) {
            self.replies
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((sub.into(), ComposeOutput { code: Some(code), output: output.into(), timed_out: false }));
        }

        /// Resposta de tempo esgotado, com a saída capturada até o corte.
        pub fn reply_timeout(&self, sub: &str, output: &str) {
            self.replies
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((sub.into(), ComposeOutput { code: None, output: output.into(), timed_out: true }));
        }

        /// Subcomandos na ordem das chamadas.
        pub fn subcommands(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .map(|c| sub_of(&c.args).to_string())
                .collect()
        }
    }

    /// `ComposeEnv` de teste com diretório base próprio (sem corrida entre testes paralelos).
    pub fn test_env(id: &str, runner: Arc<dyn ComposeRunner>) -> ComposeEnv {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir()
            .join(format!("nants-compose-base-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        ComposeEnv::new(id, runner).with_base_dir(base)
    }

    /// Token após o segundo `-f <arquivo>`.
    pub fn sub_of(args: &[String]) -> &str {
        let last_f = args.iter().rposition(|a| a == "-f").unwrap_or(0);
        args.get(last_f + 2).map_or("", String::as_str)
    }

    impl ComposeRunner for FakeRunner {
        fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
            self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call.clone());
            let delay = *self.delay.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(d) = delay {
                thread::sleep(d);
            }
            let sub = sub_of(&call.args);
            let replies = self.replies.lock().unwrap_or_else(PoisonError::into_inner);
            Ok(replies
                .iter()
                .find(|(s, _)| s == sub)
                .map(|(_, o)| o.clone())
                .unwrap_or(ComposeOutput { code: Some(0), output: String::new(), timed_out: false }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeRunner;
    use super::*;

    #[test]
    fn head_tail_capture_keeps_start_and_end() {
        let mut small = HeadTail::new(16);
        small.push(b"abc");
        assert_eq!(small.into_bytes(), b"abc");

        let mut cap = HeadTail::new(16);
        for chunk in ["inicio-", "meio ".repeat(50).as_str(), "fim: resumo"] {
            cap.push(chunk.as_bytes());
        }
        let text = String::from_utf8(cap.into_bytes()).expect("utf8");
        assert!(text.starts_with("inic"), "{text}");
        assert!(text.ends_with("fim: resumo"), "{text}");
        assert!(text.contains("bytes omitidos …]"), "{text}");

        // Cauda começa em início de caractere UTF-8.
        let mut multi = HeadTail::new(8);
        multi.push("ççççççççç".as_bytes());
        multi.push(b"x");
        let bytes = multi.into_bytes();
        let marker = bytes.iter().position(|b| *b == b']').expect("marcador");
        assert!(std::str::from_utf8(&bytes[marker + 1..]).is_ok(), "{bytes:?}");
    }

    const COMPOSE: &str = "services:\n  workspace:\n    build:\n      context: .\n      dockerfile: Dockerfile\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n  db:\n    image: postgres:16\nvolumes:\n  data:\n";

    fn project(tag: &str, compose: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-compose-t-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".nants")).expect("mkdir");
        fs::write(dir.join(COMPOSE_REL), compose).expect("w");
        fs::write(dir.join(".nants/Dockerfile"), "FROM alpine\n").expect("w");
        dir
    }

    fn env_with(fake: &Arc<FakeRunner>) -> ComposeEnv {
        super::fake::test_env("p1", fake.clone())
    }

    #[test]
    fn prepare_runs_clean_sequence_then_exec_runs_in_workspace() {
        let dir = project("seq", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake);
        env.prepare(&dir).expect("prepare");
        env.exec(&dir, "cargo test", Duration::from_secs(5)).expect("exec");
        assert_eq!(fake.subcommands(), vec!["down", "build", "up", "run"]);
        let calls = fake.calls.lock().expect("lock");
        assert_eq!(&calls[0].args[..5], ["compose", "-p", "nants-p1-test", "-f", ".nants/compose.yaml"]);
        assert_eq!(&calls[0].args[7..], ["down", "-v", "--remove-orphans"]);
        assert_eq!(&calls[2].args[7..], ["up", "-d", "--wait", "db"]);
        assert_eq!(&calls[3].args[7..], ["run", "--rm", "-T", "--entrypoint", "sh", "workspace", "-lc", "cargo test"]);
        assert_eq!(calls[3].cwd, dir);
    }

    #[test]
    fn override_runs_workspace_with_host_uid_and_home_tmp() {
        let dir = project("uid", COMPOSE);
        let doc: Value = serde_yaml::from_str(COMPOSE).expect("yaml");
        let text = override_yaml("p1", &dir, &doc, None, None).expect("override");
        let v: Value = serde_yaml::from_str(&text).expect("parse");
        let (uid, gid) = host_ids().expect("ids");
        let ws = &v["services"]["workspace"];
        assert_eq!(ws["user"].as_str(), Some(format!("{uid}:{gid}").as_str()));
        assert_eq!(ws["environment"]["HOME"].as_str(), Some("/tmp"));
        assert!(v["services"]["db"].get("user").is_none());
    }

    #[test]
    fn repair_builds_restricted_chown_argv_without_shell() {
        let dir = project("repair", COMPOSE);
        fs::create_dir_all(dir.join("node_modules/a")).expect("mk");
        fs::write(dir.join("keep.txt"), "x").expect("w");
        std::os::unix::fs::symlink("/etc", dir.join("link")).expect("ln");
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake);
        let (uid, gid) = host_ids().expect("ids");
        // Nenhuma entrada alheia: o docker nem é chamado.
        assert_eq!(env.repair_ownership(&dir).expect("ok"), 0);
        assert!(fake.calls.lock().expect("lock").is_empty());
        // Finge que o "dono atual" é outro: tudo é alheio (symlink incluso, sem ser seguido).
        let n = env.repair_as(&dir, uid.wrapping_add(1), gid).expect("repair");
        assert!(n >= 3);
        let calls = fake.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        let a = &calls[0].args;
        let pos = a.iter().position(|x| x == "run").expect("run");
        assert_eq!(
            &a[pos..pos + 11],
            ["run", "--rm", "--no-deps", "-T", "--user", "0", "--entrypoint", "chown", "workspace", "-h", "-R"]
        );
        assert_eq!(a[pos + 11], format!("{}:{gid}", uid.wrapping_add(1)));
        assert_eq!(a[pos + 12], "--");
        assert!(a[pos + 13..].iter().all(|p| p.starts_with("/work/") && !p.contains("..")));
        assert!(a.iter().any(|p| p == "/work/node_modules"));
        assert!(!a.iter().any(|p| p == "/work/node_modules/a"), "não desce em entrada alheia");
        assert!(!a.iter().any(|p| p == "sh"));
    }

    #[test]
    fn chown_tail_rejects_escaping_paths() {
        assert!(chown_tail(1, 1, &[PathBuf::from("../x")]).is_err());
        assert!(chown_tail(1, 1, &[PathBuf::from("/etc")]).is_err());
        assert!(chown_tail(1, 1, &[PathBuf::from("a/b")]).is_ok());
    }

    #[test]
    fn rebuild_only_when_environment_hash_changes() {
        let dir = project("hash", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake);
        env.prepare(&dir).expect("p1");
        env.prepare(&dir).expect("p2");
        let count = |s: &str| fake.subcommands().iter().filter(|x| *x == s).count();
        assert_eq!(count("build"), 1);
        assert_eq!(count("down"), 2);
        fs::write(dir.join(".nants/Dockerfile"), "FROM alpine:3\n").expect("w");
        env.prepare(&dir).expect("p3");
        assert_eq!(count("build"), 2);
    }

    #[test]
    fn approved_hash_is_revalidated_on_every_call() {
        let dir = project("appr", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let good = env_hash(&dir).expect("hash");
        let env = env_with(&fake).with_approved_hash(Some(good));
        assert!(env.prepare(&dir).is_ok());
        assert!(env.exec(&dir, "x", Duration::from_secs(1)).is_ok());
        // Alteração depois da aprovação (ex.: pelo Desenvolvedor): exec, prepare e down recusam.
        fs::write(dir.join(".nants/Dockerfile"), "FROM evil\n").expect("w");
        let calls_before = fake.calls.lock().expect("lock").len();
        for r in [
            env.exec(&dir, "x", Duration::from_secs(1)).map(|_| ()),
            env.prepare(&dir),
            env.down(&dir),
        ] {
            assert!(r.expect_err("diverge").contains("aprovado"));
        }
        assert_eq!(fake.calls.lock().expect("lock").len(), calls_before, "nada executado");
    }

    #[test]
    fn accepting_a_new_hash_persists_it_and_forces_rebuild() {
        let dir = project("accept", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let saved = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = saved.clone();
        let good = env_hash(&dir).expect("hash");
        let env = env_with(&fake)
            .with_approved_hash(Some(good))
            .with_persist(Arc::new(move |h| {
                sink.lock().expect("lock").push(h.to_string());
                Ok(())
            }));
        env.prepare(&dir).expect("p1");
        fs::write(dir.join(".nants/Dockerfile"), "FROM alpine:3\n").expect("w");
        assert!(env.prepare(&dir).is_err(), "sem aprovação, diverge");
        let new = env_hash(&dir).expect("hash");
        env.accept_environment(&new).expect("accept");
        assert_eq!(saved.lock().expect("lock").as_slice(), [new]);
        env.prepare(&dir).expect("p2");
        let builds = fake.subcommands().iter().filter(|x| *x == "build").count();
        assert_eq!(builds, 2, "rebuild forçado");
    }

    #[test]
    fn empty_approved_set_fails_closed() {
        let dir = project("emptyset", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake).with_approved_hashes(Vec::new());
        assert!(env.prepare(&dir).expect_err("fechado").contains("nenhum hash"));
        assert!(env.exec(&dir, "x", Duration::from_secs(1)).is_err());
        assert!(env.down(&dir).is_err());
        assert!(fake.calls.lock().expect("lock").is_empty());
    }

    #[test]
    fn failed_persist_keeps_old_approval() {
        let dir = project("acceptfail", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let good = env_hash(&dir).expect("hash");
        let env = env_with(&fake)
            .with_approved_hash(Some(good))
            .with_persist(Arc::new(|_| Err("disco cheio".into())));
        fs::write(dir.join(".nants/Dockerfile"), "FROM alpine:3\n").expect("w");
        let new = env_hash(&dir).expect("hash");
        assert!(env.accept_environment(&new).is_err());
        assert!(env.prepare(&dir).is_err());
    }

    #[test]
    fn override_dir_is_private_and_refuses_symlinks() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("nants-compose-priv-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let victim = std::env::temp_dir().join(format!("nants-compose-victim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&victim);
        fs::create_dir_all(&victim).expect("victim");
        let dir = project("priv", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let env = ComposeEnv::new("p1", fake.clone()).with_base_dir(base.clone());
        env.prepare(&dir).expect("ok");
        let pdir = base.join("nants-p1-test");
        let mode = |p: &Path| fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&pdir), 0o700);
        assert_eq!(mode(&base), 0o700);
        assert_eq!(mode(&pdir.join("override.yaml")), 0o600);
        // Permissão frouxa é corrigida.
        fs::set_permissions(&pdir, fs::Permissions::from_mode(0o777)).expect("chmod");
        env.prepare(&dir).expect("ok2");
        assert_eq!(mode(&pdir), 0o700);
        // Symlink no lugar do override: o alvo não é tocado.
        let target = victim.join("alvo.txt");
        fs::write(&target, "intacto").expect("w");
        fs::remove_file(pdir.join("override.yaml")).expect("rm");
        symlink(&target, pdir.join("override.yaml")).expect("ln");
        env.prepare(&dir).expect("ok3");
        assert_eq!(fs::read_to_string(&target).expect("r"), "intacto");
        // Symlink no lugar do diretório do projeto: recusa.
        fs::remove_dir_all(&pdir).expect("rm dir");
        symlink(&victim, &pdir).expect("ln dir");
        let err = env.prepare(&dir).expect_err("symlink");
        assert!(err.contains("symlink"), "{err}");
        assert!(!victim.join("override.yaml").exists());
    }

    #[test]
    fn docker_failures_are_errors_with_clear_reason() {
        let dir = project("err", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        fake.reply("build", 1, "failed to solve: boom");
        let err = env_with(&fake).prepare(&dir).expect_err("build");
        assert!(err.contains("build falhou") && err.contains("boom"), "{err}");

        let fake = Arc::new(FakeRunner::default());
        fake.reply("down", 1, "Cannot connect to the Docker daemon at unix:///var/run/docker.sock");
        let err = env_with(&fake).prepare(&dir).expect_err("daemon");
        assert!(err.contains("daemon do Docker"), "{err}");

        let fake = Arc::new(FakeRunner::default());
        fake.reply("run", 1, "Cannot connect to the Docker daemon");
        assert!(env_with(&fake).exec(&dir, "x", Duration::from_secs(1)).is_err());
        let fake = Arc::new(FakeRunner::default());
        fake.reply("run", 2, "1 test failed");
        let out = env_with(&fake).exec(&dir, "x", Duration::from_secs(1)).expect("falha de teste não é do docker");
        assert_eq!(out.code, Some(2));
    }

    #[test]
    fn override_mounts_worktree_and_labels_resources() {
        let dir = project("ov", COMPOSE);
        let doc: Value = serde_yaml::from_str(COMPOSE).expect("yaml");
        let text = override_yaml("p1", &dir, &doc, None, None).expect("ov");
        let v: Value = serde_yaml::from_str(&text).expect("parse");
        let ws = &v["services"]["workspace"];
        assert_eq!(ws["volumes"][0]["target"].as_str(), Some("/work"));
        assert_eq!(ws["volumes"][0]["source"].as_str(), dir.canonicalize().expect("c").to_str());
        assert_eq!(v["services"]["db"]["labels"][0].as_str(), Some("nants.project=p1"));
        assert_eq!(v["volumes"]["data"]["labels"][0].as_str(), Some("nants.project=p1"));
    }

    #[test]
    fn override_run_command_is_a_safe_exec_list_only_on_workspace() {
        let dir = project("cmd", COMPOSE);
        let doc: Value = serde_yaml::from_str(COMPOSE).expect("yaml");
        let evil = "npm run dev'\n  privileged: true\n#\" $HOME";
        let text = override_yaml("p1", &dir, &doc, Some(evil), None).expect("ov");
        let v: Value = serde_yaml::from_str(&text).expect("parse");
        let cmd = v["services"]["workspace"]["command"].as_sequence().expect("lista");
        assert_eq!(cmd[0].as_str(), Some(evil.replace('$', "$$").as_str()));
        assert_eq!(cmd.len(), 1);
        let ep = v["services"]["workspace"]["entrypoint"].as_sequence().expect("entrypoint");
        assert_eq!((ep[0].as_str(), ep[1].as_str(), ep.len()), (Some("sh"), Some("-lc"), 2));
        assert!(v["services"]["db"].get("entrypoint").is_none());
        assert!(v["services"]["workspace"].get("privileged").is_none());
        assert!(v["services"]["db"].get("command").is_none());
        let none = override_yaml("p1", &dir, &doc, None, None).expect("ov");
        assert!(!none.contains("command"));
        assert_eq!(v["services"]["workspace"]["init"].as_bool(), Some(true));
        assert!(!none.contains("init"));
    }

    #[test]
    fn busy_host_port_is_remapped_in_run_override_and_free_one_is_kept() {
        let compose = "services:\n  workspace:\n    image: x\n    ports:\n      - \"127.0.0.1:3000:3000\"\n      - \"127.0.0.1:5173:5173\"\n";
        let dir = project("busy", compose);
        let doc: Value = serde_yaml::from_str(compose).expect("yaml");
        let mut remaps = Vec::new();
        let text = override_yaml_with("p1", &dir, &doc, Some("run"), None, &mut remaps, &|_, p| p != 3000, &|_| Some(41234)).expect("ov");
        assert_eq!(remaps, vec![(3000, 41234)]);
        let v: Value = serde_yaml::from_str(&text).expect("parse");
        let Value::Tagged(t) = &v["services"]["workspace"]["ports"] else { panic!("sem tag: {text}") };
        let list = t.value.as_sequence().expect("lista");
        assert_eq!(list[0].as_str(), Some("127.0.0.1:41234:3000"));
        assert_eq!(list[1].as_str(), Some("127.0.0.1:5173:5173"));
        // Tudo livre: nenhum override de portas; sem comando (-test): idem.
        let mut none = Vec::new();
        let free = override_yaml_with("p1", &dir, &doc, Some("run"), None, &mut none, &|_, _| true, &|_| None).expect("ov");
        assert!(none.is_empty() && !free.contains("ports"), "{free}");
        let test = override_yaml_with("p1", &dir, &doc, None, None, &mut none, &|_, _| false, &|_| Some(1)).expect("ov");
        assert!(none.is_empty() && !test.contains("ports"), "{test}");
    }

    #[test]
    fn override_mounts_git_and_nants_read_only() {
        let compose = "services:\n  workspace:\n    image: x\n";
        let dir = project("romounts", compose);
        let doc: Value = serde_yaml::from_str(compose).expect("yaml");
        let plain = override_yaml("p1", &dir, &doc, None, None).expect("ov");
        assert!(!plain.contains("/work/.g"), "{plain}");
        fs::write(dir.join(".git"), "gitdir: /x\n").expect("f");
        fs::create_dir_all(dir.join(".nants")).expect("d");
        for run in [None, Some("run")] {
            let text = override_yaml("p1", &dir, &doc, run, None).expect("ov");
            let v: Value = serde_yaml::from_str(&text).expect("parse");
            let vols = v["services"]["workspace"]["volumes"].as_sequence().expect("vols");
            assert_eq!(vols.len(), 3, "{text}");
            for (i, t) in [(1, "/work/.git"), (2, "/work/.nants")] {
                assert_eq!(vols[i]["target"].as_str(), Some(t));
                assert_eq!(vols[i]["read_only"].as_bool(), Some(true));
            }
        }
    }

    #[test]
    fn busy_port_remap_handles_long_syntax_and_real_probe() {
        let list: Value = serde_yaml::from_str("- target: 3000\n  published: 3000\n  host_ip: 127.0.0.1\n- \"3000:3000\"\n- \"8080\"\n").expect("y");
        let mut remaps = Vec::new();
        let out = remap_busy_ports(list, &mut remaps, &|_, _| false, &|_| Some(40000));
        let l = out.as_sequence().expect("l");
        assert_eq!(l[0]["published"].as_u64(), Some(40000));
        assert_eq!(l[1].as_str(), Some("40000:3000"));
        assert_eq!(l[2].as_str(), Some("8080"));
        // Sonda real: porta ocupada por listener local é vista como ocupada.
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = l.local_addr().expect("a").port();
        assert!(!host_port_free("127.0.0.1", port));
        assert!(host_port_alloc("127.0.0.1").is_some());
    }

    #[test]
    fn port_conflict_errors_are_recognized() {
        assert!(is_port_conflict("Bind for 127.0.0.1:3000 failed: port is already allocated"));
        assert!(is_port_conflict("listen tcp 0.0.0.0:80: bind: address already in use"));
        assert!(!is_port_conflict("Cannot connect to the Docker daemon"));
    }

    /// Faixa de visualização dos testes (a padrão de `--port 3000`).
    const VIEW: ViewPorts = ViewPorts { start: 3001, end: 3010 };

    fn lan_list(text: &str) -> Vec<Value> {
        let v: Value = serde_yaml::from_str(text).expect("parse");
        let Value::Tagged(t) = &v["services"]["workspace"]["ports"] else { panic!("sem tag: {text}") };
        t.value.as_sequence().expect("lista").clone()
    }

    #[test]
    fn override_lan_ports_come_from_view_range_only_for_workspace() {
        let compose = "services:\n  workspace:\n    image: x\n    ports:\n      - \"127.0.0.1:5173:5173\"\n      - \"127.0.0.1::8080\"\n      - target: 9000\n        host_ip: 127.0.0.1\n      - \"4000\"\n      - \"5000:5000/udp\"\n      - \"[::1]:6000:6000\"\n      - 7000\n  db:\n    image: y\n    ports:\n      - \"127.0.0.1:5432:5432\"\n";
        let dir = project("lan", compose);
        let doc: Value = serde_yaml::from_str(compose).expect("yaml");
        // A porta pedida (5173) está livre, mas em `--lan` a do host vem da faixa mesmo assim.
        let mut remaps = Vec::new();
        let text = override_yaml_with("p1", &dir, &doc, Some("npm run dev"), Some(VIEW), &mut remaps, &|_, _| true, &|_| Some(41234)).expect("ov");
        assert!(text.contains("ports: !override"), "{text}");
        assert!(remaps.is_empty(), "{remaps:?}");
        let list = lan_list(&text);
        assert_eq!(list[0].as_str(), Some("0.0.0.0:3001:5173"));
        assert_eq!(list[1].as_str(), Some("0.0.0.0:3002:8080"));
        assert_eq!(list[2]["host_ip"].as_str(), Some("0.0.0.0"));
        assert_eq!(list[2]["target"].as_u64(), Some(9000));
        assert_eq!(list[2]["published"].as_u64(), Some(3003));
        // Só a porta do container: também recebe porta da faixa, em 0.0.0.0.
        assert_eq!(list[3].as_str(), Some("0.0.0.0:3004:4000"));
        assert_eq!(list[4].as_str(), Some("0.0.0.0:3005:5000/udp"));
        assert_eq!(list[5].as_str(), Some("0.0.0.0:3006:6000"));
        assert_eq!(list[6].as_str(), Some("0.0.0.0:3007:7000"));
        // Outros serviços (banco) ficam fora da faixa e intactos.
        let v: Value = serde_yaml::from_str(&text).expect("parse");
        assert!(v["services"]["db"].get("ports").is_none(), "{text}");
        assert!(v["services"]["db"]["labels"].is_sequence());
    }

    #[test]
    fn lan_range_skips_busy_ports_and_expands_container_ranges() {
        let compose = "services:\n  workspace:\n    image: x\n    ports:\n      - \"5173:5173\"\n      - \"8000-8001:8000-8001\"\n";
        let dir = project("lanbusy", compose);
        let doc: Value = serde_yaml::from_str(compose).expect("yaml");
        let busy = |ip: &str, p: u16| ip == "0.0.0.0" && p != 3001 && p != 3003;
        let text = override_yaml_with("p1", &dir, &doc, Some("run"), Some(VIEW), &mut Vec::new(), &busy, &|_| Some(41234)).expect("ov");
        let list = lan_list(&text);
        assert_eq!(list.iter().filter_map(Value::as_str).collect::<Vec<_>>(), ["0.0.0.0:3002:5173", "0.0.0.0:3004:8000", "0.0.0.0:3005:8001"]);
    }

    #[test]
    fn lan_range_exhausted_fails_with_clear_message_and_no_random_port() {
        let compose = "services:\n  workspace:\n    image: x\n    ports:\n      - \"127.0.0.1:5173:5173\"\n      - \"8080\"\n";
        let dir = project("lanfull", compose);
        let doc: Value = serde_yaml::from_str(compose).expect("yaml");
        let expected = "Todas as portas de visualização (3001 a 3010) estão em uso. Pare outro app na Execução e tente de novo.";
        let err = override_yaml_with("p1", &dir, &doc, Some("run"), Some(VIEW), &mut Vec::new(), &|_, _| false, &|_| Some(41234)).expect_err("esgotada");
        assert_eq!(err, expected);
        // Uma livre só, duas portas pedidas: também esgota (nunca repete nem sai da faixa).
        let one = |_: &str, p: u16| p == 3010;
        let err = override_yaml_with("p1", &dir, &doc, Some("run"), Some(VIEW), &mut Vec::new(), &one, &|_| Some(41234)).expect_err("esgotada");
        assert_eq!(err, expected);
        // No `up_run` real (sonda de verdade): faixa de uma porta ocupada por um listener.
        let l = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind");
        let port = l.local_addr().expect("a").port();
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake).with_kind(ComposeKind::Run).with_lan_view_ports(Some(ViewPorts { start: port, end: port }));
        let err = env.up_run(&dir, "npm run dev").expect_err("esgotada");
        assert!(err.contains(&format!("({port} a {port}) estão em uso")), "{err}");
        assert!(fake.subcommands().is_empty(), "nada sobe com a faixa esgotada");
    }

    #[test]
    fn without_lan_override_is_unchanged() {
        let compose = "services:\n  workspace:\n    image: x\n    ports:\n      - \"127.0.0.1:5173:5173\"\n      - \"127.0.0.1::8080\"\n  db:\n    image: y\n    ports:\n      - \"127.0.0.1::5432\"\n";
        let dir = project("nolan", compose);
        let doc: Value = serde_yaml::from_str(compose).expect("yaml");
        let off = override_yaml("p1", &dir, &doc, Some("npm run dev"), None).expect("ov");
        assert!(!off.contains("ports"), "{off}");
        // Byte a byte igual ao de antes: o `--lan` não interfere sem faixa, e o `-test` ignora a faixa.
        let with_busy = override_yaml_with("p1", &dir, &doc, Some("run"), None, &mut Vec::new(), &|_, p| p != 5173, &|_| Some(41234)).expect("ov");
        assert_eq!(lan_list(&with_busy)[0].as_str(), Some("127.0.0.1:41234:5173"));
        assert_eq!(lan_list(&with_busy)[1].as_str(), Some("127.0.0.1::8080"));
        let test_kind = override_yaml("p1", &dir, &doc, None, None).expect("ov");
        let plain_again = override_yaml("p1", &dir, &doc, None, None).expect("ov");
        assert_eq!(test_kind, plain_again);
        assert!(!test_kind.contains("ports"), "{test_kind}");
    }

    #[test]
    fn lan_ports_only_apply_to_run_projects_with_command() {
        let dir = project("lan2", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake).with_lan_view_ports(Some(VIEW));
        env.prepare(&dir).expect("prepare");
        let ov = fs::read_to_string(env.override_dir().expect("dir").join("override.yaml")).expect("ov");
        assert!(!ov.contains("override"), "{ov}");
    }

    #[test]
    fn up_run_writes_command_into_override_and_workspace_logs_target_workspace() {
        let dir = project("uprun", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake).with_kind(ComposeKind::Run);
        env.up_run(&dir, "npm run dev -- --host 0.0.0.0").expect("up");
        let ov = {
            let calls = fake.calls.lock().expect("lock");
            assert_eq!(&calls[0].args[7..], ["up", "-d", "--build", "--wait"]);
            fs::read_to_string(calls[0].args[6].clone()).expect("override")
        };
        assert!(ov.contains("npm run dev -- --host 0.0.0.0"), "{ov}");
        env.workspace_logs(&dir, 50).expect("logs");
        let calls = fake.calls.lock().expect("lock");
        assert_eq!(&calls[1].args[7..], ["logs", "--no-color", "--tail", "50", "workspace"]);
        let ov2 = fs::read_to_string(calls[1].args[6].clone()).expect("override");
        assert!(!ov2.contains("npm run dev"));
    }

    #[test]
    fn controlled_env_drops_secrets() {
        let host = [
            ("PATH", "/bin"),
            ("HOME", "/h"),
            ("DOCKER_HOST", "unix:///x"),
            ("AWS_SECRET_ACCESS_KEY", "s"),
            ("ANTHROPIC_API_KEY", "k"),
            ("NANTS_TOKEN", "t"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let env = controlled_env(host, Path::new("/wt"));
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["PATH", "HOME", "DOCKER_HOST", "NANTS_WORKTREE"]);
    }

    #[test]
    fn real_runner_spawns_with_cleared_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("nants-compose-envdump-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        let script = dir.join("fake-docker");
        fs::write(&script, "#!/bin/sh\n/usr/bin/env\n").expect("w");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
        let runner = RealComposeRunner::with_program("sh");
        // Mesmo que o chamador entregue uma variável sensível, o runner real só passa `call.env`;
        // aqui `call.env` vem de `controlled_env` aplicado ao ambiente completo deste processo.
        let call = ComposeCall {
            args: vec![script.to_string_lossy().into_owned()],
            cwd: dir.clone(),
            env: controlled_env(std::env::vars().chain([("SEGREDO_X".to_string(), "v".to_string())]), &dir),
            timeout: Duration::from_secs(10),
        };
        let out = runner.run(&call).expect("run");
        assert!(out.success(), "{}", out.output);
        let allowed = ["PATH", "HOME", "NANTS_WORKTREE", "PWD", "SHLVL", "_", "OLDPWD"];
        for line in out.output.lines() {
            let k = line.split('=').next().unwrap_or("");
            assert!(allowed.contains(&k) || k.starts_with("DOCKER_"), "vazou: {k}");
        }
        assert!(out.output.contains("NANTS_WORKTREE="));
    }

    /// Teste real: precisa de docker. `cargo test -- --ignored real_compose_roundtrip`.
    #[test]
    #[ignore]
    fn real_compose_roundtrip() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    command: sleep infinity\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n  db:\n    image: alpine:3\n    command: sleep infinity\n";
        let dir = project("real", compose);
        fs::write(dir.join("marker.txt"), "ola").expect("w");
        let env = ComposeEnv::real(format!("itest{}", std::process::id()));
        let result = env
            .prepare(&dir)
            .and_then(|()| env.exec(&dir, "cat /work/marker.txt && echo ok", Duration::from_secs(120)));
        let _ = env.down(&dir);
        let out = result.expect("docker");
        assert!(out.success() && out.output.contains("ola") && out.output.contains("ok"), "{}", out.output);
    }
    /// Teste real: precisa de docker. O workspace roda com o uid do host e o reparo devolve a posse
    /// de arquivos criados como root. `cargo test -- --ignored real_workspace_uid_and_repair`.
    #[test]
    #[ignore]
    fn real_workspace_uid_and_repair() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    command: sleep infinity\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("realuid", compose);
        let env = ComposeEnv::real(format!("itest{}", std::process::id()));
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let (uid, _) = host_ids().expect("ids");
        // 1. Arquivo criado no container pelo workspace fica com o uid do host.
        let out = env
            .prepare(&dir)
            .and_then(|()| env.exec(&dir, "mkdir /work/criado && touch /work/criado/a.txt", Duration::from_secs(120)));
        let out = out.expect("docker");
        assert!(out.success(), "{}", out.output);
        assert_eq!(fs::metadata(dir.join("criado/a.txt")).expect("meta").uid(), uid);
        // 2. Arquivo de root (container sem --user) é detectado e reparado sem sudo.
        let status = Command::new("docker")
            .args(["run", "--rm", "-v"])
            .arg(format!("{}:/work", dir.canonicalize().expect("canon").display()))
            .args(["alpine:3", "sh", "-c", "mkdir -p /work/node_modules/p && touch /work/node_modules/p/f /work/dist"])
            .status()
            .expect("docker run");
        assert!(status.success());
        assert_ne!(fs::metadata(dir.join("node_modules")).expect("meta").uid(), uid, "deveria ser root");
        assert_eq!(foreign_entries(&dir, uid).len(), 2);
        let repaired = env.repair_ownership(&dir);
        let _ = env.down(&dir);
        assert_eq!(repaired.expect("reparo"), 2);
        assert_eq!(fs::metadata(dir.join("node_modules/p/f")).expect("meta").uid(), uid);
        assert_eq!(fs::metadata(dir.join("dist")).expect("meta").uid(), uid);
        assert!(foreign_entries(&dir, uid).is_empty());
        fs::remove_dir_all(&dir).expect("apagável sem sudo");
    }

    /// Teste real (spec 062): `.git` e `.nants` são somente leitura no workspace; o resto é gravável.
    #[test]
    #[ignore]
    fn real_git_and_nants_are_read_only() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    command: sleep infinity\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("realro", compose);
        fs::write(dir.join(".git"), "gitdir: /nonexistent\n").expect("git");
        fs::create_dir_all(dir.join(".nants")).expect("nants");
        fs::write(dir.join(".nants/a"), "x").expect("a");
        let env = ComposeEnv::real(format!("itest{}", std::process::id()));
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let rm = env.prepare(&dir).and_then(|()| env.exec(&dir, "rm -rf /work/.nants /work/.git", Duration::from_secs(120))).expect("docker");
        let touch = env.exec(&dir, "touch /work/x && mkdir -p /work/target/d && echo ok", Duration::from_secs(120)).expect("docker");
        let _ = env.down(&dir);
        assert!(!rm.success(), "rm deveria falhar: {}", rm.output);
        assert!(touch.success() && dir.join("x").exists(), "{}", touch.output);
        assert!(dir.join(".nants/a").exists() && dir.join(".git").exists());
        fs::remove_dir_all(&dir).expect("apagável");
    }

    /// Teste real do `-run`: sobe, lê a porta publicada e os logs, derruba com volumes.
    #[test]
    #[ignore]
    fn real_run_roundtrip() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    command: sleep infinity\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n  web:\n    image: alpine:3\n    command: sh -c 'echo servidor-no-ar; sleep infinity'\n    ports:\n      - \"127.0.0.1::8080\"\n";
        let dir = project("realrun", compose);
        let env = ComposeEnv::real(format!("itest{}", std::process::id())).with_kind(ComposeKind::Run);
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let result = env.up_run(&dir, "sleep infinity").and_then(|()| {
            let ports = env.published_ports(&dir)?;
            let logs = env.logs(&dir)?;
            Ok((ports, logs))
        });
        let down = env.down_with(&dir, true);
        let (ports, logs) = result.expect("docker");
        down.expect("down");
        assert!(ports.iter().any(|p| p.service == "web" && p.target == 8080 && p.published > 0), "{ports:?}");
        assert!(logs.contains("servidor-no-ar"), "{logs}");
    }

    /// Spec 067: compose com `entrypoint: ["/bin/sh", "-c"]` não pode engolir o comando.
    #[test]
    #[ignore]
    fn real_exec_ignores_project_entrypoint() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    entrypoint: [\"/bin/sh\", \"-c\"]\n    command: [\"sleep infinity\"]\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("realep", compose);
        let env = ComposeEnv::real(format!("itest{}", std::process::id())).with_kind(ComposeKind::Test);
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let result = env.prepare(&dir).and_then(|()| {
            let ok = env.exec(&dir, "echo marcador", Duration::from_secs(60))?;
            let fail = env.exec(&dir, "exit 3", Duration::from_secs(60))?;
            Ok((ok, fail))
        });
        let down = env.down_with(&dir, true);
        let (ok, fail) = result.expect("docker");
        down.expect("down");
        assert!(ok.output.contains("marcador"), "{}", ok.output);
        assert_eq!(fail.code, Some(3), "{}", fail.output);
    }

    /// Teste real do 053: o comando sai com 3 logo após subir; a observação detecta e há logs.
    #[test]
    #[ignore]
    fn real_run_early_exit_is_detected() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("realexit", compose);
        let env = ComposeEnv::real(format!("itestexit{}", std::process::id())).with_kind(ComposeKind::Run);
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let result = env.up_run(&dir, "sleep 3; echo falhou-cedo; exit 3").and_then(|()| env.observe_run(&dir, Duration::from_secs(8)));
        let logs = env.workspace_logs(&dir, 50);
        let down = env.down_with(&dir, true);
        down.expect("down");
        let err = result.expect_err("deveria detectar a saída");
        assert!(err.contains("código 3"), "{err}");
        assert!(logs.expect("logs").contains("falhou-cedo"));
    }

    /// Teste real do 053: com porta publicada (docker-proxy aceita o connect), o app que morre
    /// depois do `up` ainda é detectado.
    #[test]
    #[ignore]
    fn real_run_early_exit_with_published_port_is_detected() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    ports:\n      - \"127.0.0.1::8080\"\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("realexitport", compose);
        let env = ComposeEnv::real(format!("itestexitp{}", std::process::id())).with_kind(ComposeKind::Run);
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let result = env.up_run(&dir, "sleep 3; exit 3").and_then(|()| env.observe_run(&dir, Duration::from_secs(8)));
        let down = env.down_with(&dir, true);
        down.expect("down");
        let err = result.expect_err("deveria detectar a saída");
        assert!(err.contains("código 3") || err.contains("exited"), "{err}");
    }

    /// Teste real do 050: com `init: true` o `down` do run para `sh -lc 'sleep 300'` em < 7 s.
    #[test]
    #[ignore]
    fn real_run_down_is_fast() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("realfast", compose);
        let env = ComposeEnv::real(format!("itestfast{}", std::process::id())).with_kind(ComposeKind::Run);
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let up = env.up_run(&dir, "sleep 300");
        let t = std::time::Instant::now();
        let down = env.down_with(&dir, false);
        let took = t.elapsed();
        up.expect("up");
        down.expect("down");
        assert!(took < Duration::from_secs(7), "down levou {took:?}");
        let _ = env.down_with(&dir, true);
    }

    /// Teste real do 049: com `--lan` a porta é publicada em 0.0.0.0 e atende pelo IP da eth0.
    #[test]
    #[ignore]
    fn real_lan_port_is_reachable_on_interface_ip() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n    ports:\n      - \"127.0.0.1::8080\"\n";
        let dir = project("reallan", compose);
        let env = ComposeEnv::real(format!("itestlan{}", std::process::id())).with_kind(ComposeKind::Run).with_lan_view_ports(Some(ViewPorts { start: 47001, end: 47010 }));
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let ip = crate::server::netinfo::primary_ipv4().expect("IP da interface");
        let result = env.up_run(&dir, "while true; do printf 'HTTP/1.0 200 OK\\r\\n\\r\\nnants-049-ok' | nc -l -p 8080; done").and_then(|()| env.published_ports(&dir));
        let body = result.as_ref().ok().and_then(|ports| ports.first()).and_then(|p| {
            let mut c = None;
            for _ in 0..20 {
                c = std::net::TcpStream::connect((ip, p.published)).ok();
                if c.is_some() { break; }
                std::thread::sleep(Duration::from_millis(250));
            }
            let mut c = c?;
            c.write_all(b"GET / HTTP/1.0\r\n\r\n").ok()?;
            let mut out = String::new();
            c.read_to_string(&mut out).ok()?;
            Some(out)
        });
        let down = env.down_with(&dir, true);
        let ports = result.expect("docker");
        down.expect("down");
        assert!(ports.iter().any(|p| p.target == 8080 && p.published > 0), "{ports:?}");
        assert!(body.is_some_and(|b| b.contains("nants-049-ok")), "sem resposta em {ip}");
    }

    /// Teste real do 048: o `run_command` vira o command do workspace, que serve a porta
    /// publicada (sem healthcheck; `--wait` basta com o container rodando).
    #[test]
    #[ignore]
    fn real_run_command_serves_published_port() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n    ports:\n      - \"127.0.0.1::8080\"\n";
        let dir = project("realcmd", compose);
        let env = ComposeEnv::real(format!("itestcmd{}", std::process::id())).with_kind(ComposeKind::Run);
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let result = env.up_run(&dir, "echo iniciando; while true; do printf 'HTTP/1.0 200 OK\\r\\n\\r\\nnants-048-ok' | nc -l -p 8080; done").and_then(|()| {
            let ports = env.published_ports(&dir)?;
            let logs = env.logs(&dir)?;
            Ok((ports, logs))
        });
        let body = result.as_ref().ok().and_then(|(ports, _)| ports.first()).and_then(|p| {
            let mut c = None;
            for _ in 0..20 {
                c = std::net::TcpStream::connect(("127.0.0.1", p.published)).ok();
                if c.is_some() { break; }
                std::thread::sleep(Duration::from_millis(250));
            }
            let mut c = c?;
            c.write_all(b"GET /index.html HTTP/1.0\r\n\r\n").ok()?;
            let mut out = String::new();
            c.read_to_string(&mut out).ok()?;
            Some(out)
        });
        let down = env.down_with(&dir, true);
        let (ports, logs) = result.expect("docker");
        down.expect("down");
        assert!(ports.iter().any(|p| p.service == "workspace" && p.target == 8080 && p.published > 0), "{ports:?}");
        assert!(logs.contains("iniciando"), "{logs}");
        assert!(body.is_some_and(|b| b.contains("nants-048-ok")), "porta não serviu o arquivo");
    }
}
