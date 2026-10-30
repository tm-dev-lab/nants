//! Execução no Docker Compose do projeto (spec 030, T03): projeto `nants-<id>-test`, serviço
//! `workspace` com a worktree em /work, estado limpo antes dos testes e ambiente de subprocesso
//! controlado (nunca herda segredos do processo do Nants).

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_yaml::{Mapping, Value};

use crate::config::ViewPorts;
use crate::harness::home;
use crate::harness::limits::ContainerLimits;
use crate::harness::progress::CommandProgress;

use crate::sdd::environment::{env_hash, COMPOSE_REL, WORKSPACE_SERVICE, WORKTREE_VAR};

/// Limite de saída capturada por comando.
const MAX_OUTPUT: usize = 64 * 1024;
/// Timeout dos passos de preparo (down, build, up).
const PREPARE_TIMEOUT: Duration = Duration::from_secs(900);
const POLL: Duration = Duration::from_millis(20);

/// Spec 137 T09: chave, no override, da rede da rodada offline. É uma rede `internal: true`: corta
/// a saída para a internet e mantém os serviços irmãos (um banco usado pelos testes) acessíveis pelo
/// nome. Não se usa `network_mode: none` porque ele não combina com `networks` (o Compose recusa os
/// dois juntos) e isolaria o `workspace` também dos serviços irmãos.
pub const OFFLINE_NETWORK: &str = "nants-offline";
/// Arquivo do override da rodada offline, ao lado do override de sempre.
const OFFLINE_OVERRIDE_FILE: &str = "override-offline.yaml";

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

/// Destino de cada pedaço da saída enquanto o comando roda (spec 130 T03: a saída ao vivo do
/// Console). Recebe o fluxo ([`STDOUT`] ou [`STDERR`]) e os bytes crus; quem mostra decide o corte
/// e a redação (por fluxo: as linhas de um não se misturam com as do outro).
pub type OutputSink = Arc<dyn Fn(usize, &[u8]) + Send + Sync>;
/// Fluxo da saída padrão no [`OutputSink`].
pub const STDOUT: usize = 0;
/// Fluxo da saída de erro no [`OutputSink`].
pub const STDERR: usize = 1;

/// Executor de chamadas ao compose (trocado por um falso nos testes).
pub trait ComposeRunner: Send + Sync {
    fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String>;

    /// Como [`Self::run`], copiando cada pedaço da saída em `progress` enquanto o comando roda
    /// (spec 123 T01: o sinal de vida da tela). O resultado e a captura não mudam. Padrão: sem
    /// progresso (executores que não leem a saída aos poucos).
    fn run_watched(&self, call: &ComposeCall, progress: Option<&Arc<CommandProgress>>) -> Result<ComposeOutput, String> {
        let _ = progress;
        self.run(call)
    }

    /// Spec 130 T03: como [`Self::run`], copiando cada pedaço da saída em `sink` e encerrando o
    /// processo quando `cancel` vira `true` (o resultado vem sem código e sem `timed_out`).
    /// Padrão: roda inteiro e entrega a saída de uma vez (executores que não leem aos poucos).
    fn run_streamed(&self, call: &ComposeCall, sink: OutputSink, cancel: Arc<AtomicBool>) -> Result<ComposeOutput, String> {
        let _ = cancel;
        let out = self.run(call)?;
        sink(STDOUT, out.output.as_bytes());
        Ok(out)
    }
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

/// Quanto o executor espera os pipes depois de matar o cliente (tempo esgotado ou parada). Um
/// processo que herdou os pipes e segue vivo não segura o retorno (spec 134 T08).
const PIPE_GRACE: Duration = Duration::from_millis(500);

/// Leitura de um pipe numa captura compartilhada: quem espera pode tirar o que já chegou sem
/// aguardar o fim do pipe ([`take_capture`]).
fn read_limited(
    mut r: impl Read + Send + 'static,
    progress: Option<Arc<CommandProgress>>,
    sink: Option<(usize, OutputSink)>,
) -> (thread::JoinHandle<()>, Arc<Mutex<HeadTail>>) {
    let capture = Arc::new(Mutex::new(HeadTail::new(MAX_OUTPUT)));
    let shared = Arc::clone(&capture);
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    shared.lock().unwrap_or_else(PoisonError::into_inner).push(&buf[..n]);
                    if let Some(p) = &progress {
                        p.push(&buf[..n]);
                    }
                    if let Some((stream, s)) = &sink {
                        s(*stream, &buf[..n]);
                    }
                }
            }
        }
    });
    (handle, capture)
}

/// O que a captura tem agora. Com `wait_all`, espera o pipe fechar; sem, espera no máximo até
/// `until` e devolve o que chegou (a thread de leitura segue sozinha até o pipe fechar).
fn take_capture(reader: Option<(thread::JoinHandle<()>, Arc<Mutex<HeadTail>>)>, wait_all: bool, until: Instant) -> Vec<u8> {
    let Some((handle, capture)) = reader else { return Vec::new() };
    if wait_all {
        let _ = handle.join();
    } else {
        while !handle.is_finished() && Instant::now() < until {
            thread::sleep(POLL);
        }
    }
    let mut guard = capture.lock().unwrap_or_else(PoisonError::into_inner);
    std::mem::replace(&mut *guard, HeadTail::new(MAX_OUTPUT)).into_bytes()
}

/// Mata o cliente e todo o grupo de processos dele (o `docker` roda o plugin `compose` como
/// filho, e é ele que segura os pipes). Spec 134 T08.
fn kill_tree(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: `kill` só recebe números; o grupo `pid` foi criado no spawn (`process_group(0)`)
        // e só contém o cliente e os filhos dele.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Variável que libera o Docker real só nos testes reais marcados `#[ignore]` (`real_*`).
#[cfg(test)]
pub(crate) const TEST_REAL_DOCKER_VAR: &str = "NANTS_TEST_REAL_DOCKER";

/// Sentinela dos testes (spec 113 T01): em `cfg(test)` o `docker` real é recusado, venha de
/// onde vier (executor Compose padrão, navegador, prerequisitos), salvo com
/// `NANTS_TEST_REAL_DOCKER=1`. Um `down -v` com nome de projeto de teste apagaria volumes reais
/// do host; quem precisa de Compose num teste injeta um executor falso (`envs::tests::Rec`).
/// `Some(motivo)` quando a chamada deve ser recusada.
#[cfg(test)]
pub(crate) fn refuse_real_docker_in_tests(program: &str) -> Option<String> {
    let name = Path::new(program).file_name().and_then(|n| n.to_str()).unwrap_or(program);
    if name != "docker" || std::env::var(TEST_REAL_DOCKER_VAR).is_ok_and(|v| v == "1") {
        return None;
    }
    let reason = format!(
        "teste chamou o docker real ({program}); injete um executor falso (envs::tests::Rec) \
         ou, num teste real #[ignore], rode com {TEST_REAL_DOCKER_VAR}=1"
    );
    eprintln!("SENTINELA: {reason}");
    Some(reason)
}

impl ComposeRunner for RealComposeRunner {
    fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
        self.run_watched(call, None)
    }

    fn run_watched(&self, call: &ComposeCall, progress: Option<&Arc<CommandProgress>>) -> Result<ComposeOutput, String> {
        self.spawn_and_wait(call, progress.cloned(), None, None)
    }

    fn run_streamed(&self, call: &ComposeCall, sink: OutputSink, cancel: Arc<AtomicBool>) -> Result<ComposeOutput, String> {
        self.spawn_and_wait(call, None, Some(sink), Some(cancel))
    }
}

impl RealComposeRunner {
    /// Roda o `docker` com ambiente limpo e stdin fechado, captura a saída limitada (copiando-a em
    /// `progress` e `sink`) e mata o processo no tempo limite ou quando `cancel` vira `true`.
    fn spawn_and_wait(
        &self,
        call: &ComposeCall,
        progress: Option<Arc<CommandProgress>>,
        sink: Option<OutputSink>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<ComposeOutput, String> {
        #[cfg(test)]
        if let Some(reason) = refuse_real_docker_in_tests(&self.program) {
            return Err(reason);
        }
        let mut child = Command::new(&self.program)
            .args(&call.args)
            .current_dir(&call.cwd)
            // Grupo de processos próprio: no tempo esgotado, o grupo inteiro morre (kill_tree).
            .process_group(0)
            .env_clear()
            .envs(call.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("não foi possível iniciar o docker: {e}"))?;
        let out = child.stdout.take().map(|r| read_limited(r, progress.clone(), sink.clone().map(|s| (STDOUT, s))));
        let err = child.stderr.take().map(|r| read_limited(r, progress, sink.map(|s| (STDERR, s))));
        let deadline = Instant::now() + call.timeout;
        let (mut code, mut timed_out, mut killed) = (None, false, false);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    code = status.code();
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    kill_tree(&mut child);
                    return Err(format!("falha ao aguardar o docker: {e}"));
                }
            }
            if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
                kill_tree(&mut child);
                killed = true;
                break;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                kill_tree(&mut child);
                killed = true;
                break;
            }
            thread::sleep(POLL);
        }
        // Spec 134 T08: depois de matar o cliente, os pipes não seguram o retorno: quem chama
        // precisa voltar logo para remover o container (que segue vivo no daemon) pelo nome.
        let until = Instant::now() + PIPE_GRACE;
        let mut bytes = take_capture(out, !killed, until);
        bytes.extend(take_capture(err, !killed, until));
        // Saída crua: quem mostra a saída marca o tempo limite pelo `timed_out`, no idioma de quem lê
        // (o runner, na saída dos testes, spec 115 T04).
        let output = String::from_utf8_lossy(&bytes).into_owned();
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
    /// Spec 134 T08: teto de memória e de processos de cada container (padrão: o da máquina).
    limits: ContainerLimits,
    /// Limites efetivos do `workspace` no último override gravado (o teto ou o menor do projeto).
    applied: Mutex<Option<ContainerLimits>>,
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
            limits: ContainerLimits::host(),
            applied: Mutex::new(None),
        }
    }

    /// Teto de memória e de processos dos containers (testes; o padrão vem da máquina).
    pub fn with_limits(mut self, limits: ContainerLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Limites efetivos do `workspace` (spec 134 T08): os do último override gravado ou, antes
    /// dele, o teto. Para dizer ao modelo qual limite o comando atingiu.
    pub fn workspace_limits(&self) -> ContainerLimits {
        self.applied.lock().unwrap_or_else(PoisonError::into_inner).unwrap_or(self.limits)
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
        self.load_variant(dir, run_command, None)
    }

    /// Como [`Self::load_with`]; com `offline` (o nome da rede interna), grava o override da rodada
    /// offline ([`OFFLINE_OVERRIDE_FILE`]), com a rede `internal: true` (spec 137 T09).
    fn load_variant(&self, dir: &Path, run_command: Option<&str>, offline: Option<&str>) -> Result<(Value, PathBuf, String), String> {
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
        // Spec 129 T01: a pasta pessoal persistente existe antes de qualquer comando do workspace.
        home::ensure(dir).map_err(|e| format!("pasta pessoal ({}) indisponível: {e}", home::HOME_DIR))?;
        let mut remaps = Vec::new();
        let ov = write_override(
            &self.override_dir()?,
            &self.project_id,
            dir,
            &doc,
            run_command,
            self.lan_ports.filter(|_| run_command.is_some()),
            &mut remaps,
            self.limits,
            offline,
        )?;
        let workspace = doc.get("services").and_then(|s| s.get(WORKSPACE_SERVICE));
        *self.applied.lock().unwrap_or_else(PoisonError::into_inner) = Some(self.limits.effective(workspace));
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
        // Spec 137 T09: a rede da rodada offline não está no compose de sempre, então o `down` não a
        // remove. Se o Nants morreu no meio de uma rodada offline, ela sobra; sai aqui, pelo nome
        // (melhor esforço: o `-f` já ignora a rede que não existe).
        let _ = self.remove_offline_network(dir);
        let has_build = doc
            .get("services")
            .and_then(|s| s.get(WORKSPACE_SERVICE))
            .is_some_and(|w| w.get("build").is_some());
        let stale = self.built.lock().unwrap_or_else(PoisonError::into_inner).as_deref() != Some(current.as_str());
        if has_build && stale {
            self.step(&self.call(dir, &ov, &["build", WORKSPACE_SERVICE], PREPARE_TIMEOUT), "build")?;
        }
        *self.built.lock().unwrap_or_else(PoisonError::into_inner) = Some(current);
        self.up_support(dir, &ov, &support_services(&doc))
    }

    /// `up -d --wait` dos serviços de apoio `support` (nada sem serviço de apoio).
    fn up_support(&self, dir: &Path, ov: &Path, support: &[String]) -> Result<(), String> {
        if support.is_empty() {
            return Ok(());
        }
        let mut tail = vec!["up", "-d", "--wait"];
        tail.extend(support.iter().map(String::as_str));
        self.step(&self.call(dir, ov, &tail, PREPARE_TIMEOUT), "up")
    }

    /// `docker network rm -f <rede da rodada offline>`: a rede que não existe não é erro.
    fn remove_offline_network(&self, dir: &Path) -> Result<(), String> {
        let net = self.offline_network_name();
        let out = self.runner.run(&self.docker_call(dir, &["network", "rm", "-f", net.as_str()]))?;
        if out.success() {
            Ok(())
        } else {
            Err(format!("docker network rm falhou: {}", diagnose(&out)))
        }
    }

    /// Nome (no Docker) da rede interna da rodada offline deste projeto.
    pub fn offline_network_name(&self) -> String {
        format!("{}-{OFFLINE_NETWORK}", self.project_name())
    }

    /// Spec 137 T09: a rodada offline. Roda `command` no `workspace` como [`Self::exec_watched`],
    /// mas pelo override da rodada offline: o `workspace` fica só na rede `internal: true` (sem
    /// internet) e os serviços de apoio entram nela também, sem sair das redes deles (seguem
    /// acessíveis pelo nome). Não recria o estado (`down -v`) nem instala nada: vem depois da
    /// rodada normal verde e usa o mesmo cache de dependências (a worktree, a pasta pessoal e os
    /// volumes). No fim, também quando falha, os serviços de apoio voltam ao override de sempre e
    /// a rede interna é removida; se isso falhar, os serviços de apoio saem com os volumes anônimos
    /// deles (`rm -s -f -v`, os nomeados ficam) e a rede sai pelo nome. `Err` = falha do Docker (o código de saída do comando vai no resultado).
    pub fn exec_offline(
        &self,
        dir: &Path,
        command: &str,
        timeout: Duration,
        progress: Option<&Arc<CommandProgress>>,
    ) -> Result<ComposeOutput, String> {
        let net = self.offline_network_name();
        let (doc, ov, _) = self.load_variant(dir, None, Some(&net))?;
        let support = support_services(&doc);
        // A sobra de uma rodada anterior interrompida sai antes (melhor esforço, como no `prepare`).
        let _ = self.remove_offline_network(dir);
        let ran = self.up_support(dir, &ov, &support).and_then(|()| {
            let name = unique_container_name(&self.project_name(), "offline");
            let tail = ["run", "--rm", "--name", name.as_str(), "-T", "--entrypoint", "sh", WORKSPACE_SERVICE, "-lc", command];
            let ran = self.runner.run_watched(&self.call(dir, &ov, &tail, timeout), progress);
            self.stop_if_left_running(dir, &name, ran)
        });
        let cleanup = self.leave_offline(dir, &ov, &support);
        let out = ran?;
        cleanup?;
        if !out.success() && is_docker_failure(&out.output) {
            return Err(format!("{DOCKER_UNAVAILABLE}: {}", diagnose(&out)));
        }
        Ok(out)
    }

    /// Fim da rodada offline: serviços de apoio de volta ao override de sempre e a rede interna
    /// removida; na falha, `rm -s -f -v` dos serviços de apoio pelo override offline (só os volumes
    /// anônimos deles saem; os nomeados, o cache, ficam) e a rede pelo nome.
    fn leave_offline(&self, dir: &Path, offline_ov: &Path, support: &[String]) -> Result<(), String> {
        let remove_net = || self.remove_offline_network(dir);
        let back = if support.is_empty() {
            Ok(())
        } else {
            self.load(dir).and_then(|(_, ov, _)| self.up_support(dir, &ov, support))
        };
        if back.and_then(|()| remove_net()).is_ok() {
            return Ok(());
        }
        // Revisão 1 (F-01): `rm --stop --force --volumes` (o `rm -s -f -v`) em vez de `down` sem `-v`. O `down` removeria os
        // containers e deixaria órfãos (sem rótulo, fora do alcance do próximo `down -v`) os volumes
        // anônimos presos a eles, como o `VOLUME` da imagem do postgres. O `rm -v` apaga só esses;
        // os volumes nomeados (o cache de dependências) ficam.
        let removed = if support.is_empty() {
            Ok(())
        } else {
            let mut tail = vec!["rm", "--stop", "--force", "--volumes"];
            tail.extend(support.iter().map(String::as_str));
            self.step(&self.call(dir, offline_ov, &tail, PREPARE_TIMEOUT), "rm")
        };
        removed.and_then(|()| remove_net())
    }

    /// `docker compose run --rm --entrypoint sh workspace -lc <command>` (o entrypoint do compose do
    /// projeto nunca engole o comando). `Err` = falha do Docker (daemon,
    /// subprocesso); o código de saída do comando vai no resultado.
    pub fn exec(&self, dir: &Path, command: &str, timeout: Duration) -> Result<ComposeOutput, String> {
        self.exec_watched(dir, command, timeout, None)
    }

    /// Como [`Self::exec`], com o progresso da saída em `progress` enquanto roda (spec 123 T01).
    pub fn exec_watched(
        &self,
        dir: &Path,
        command: &str,
        timeout: Duration,
        progress: Option<&Arc<CommandProgress>>,
    ) -> Result<ComposeOutput, String> {
        let (_, ov, _) = self.load(dir)?;
        let name = unique_container_name(&self.project_name(), "exec");
        let tail = ["run", "--rm", "--name", name.as_str(), "-T", "--entrypoint", "sh", WORKSPACE_SERVICE, "-lc", command];
        let ran = self.runner.run_watched(&self.call(dir, &ov, &tail, timeout), progress);
        let out = self.stop_if_left_running(dir, &name, ran)?;
        if !out.success() && is_docker_failure(&out.output) {
            return Err(format!("{DOCKER_UNAVAILABLE}: {}", diagnose(&out)));
        }
        Ok(out)
    }

    /// Spec 134 T08: o tempo limite (ou a falha do cliente) mata só o processo `docker compose` no
    /// host; o container segue vivo, com tudo o que roda dentro dele (o `--rm` só age quando ele
    /// termina sozinho). Nesses casos o container é removido pelo nome (`docker rm -f`, que encerra
    /// todos os processos dele). O resultado da execução volta como veio.
    fn stop_if_left_running(&self, dir: &Path, name: &str, ran: Result<ComposeOutput, String>) -> Result<ComposeOutput, String> {
        let left_running = match &ran {
            Ok(o) => o.timed_out,
            Err(_) => true,
        };
        if left_running {
            let _ = self.remove_container(dir, name);
        }
        ran
    }

    /// Spec 129 T02: roda `command` no `workspace` num container que NÃO é removido ao fim (`run`
    /// sem `--rm`, nome do Nants e rótulo `nants.project`), lista com [`Self::container_diff`] o que
    /// ele gravou e remove o container logo depois, também quando a execução ou a listagem falham.
    /// `Err` = falha do Docker (o código de saída do comando vai no resultado). Spec 131 T05: a
    /// listagem que falha não apaga o resultado do comando (ele é a instalação de verdade); vai em
    /// [`KeptRun::diff`]. `progress` recebe a saída enquanto roda, como em [`Self::exec_watched`].
    pub fn run_kept(&self, dir: &Path, command: &str, timeout: Duration, progress: Option<&Arc<CommandProgress>>) -> Result<KeptRun, String> {
        let (_, ov, _) = self.load(dir)?;
        let name = unique_container_name(&self.project_name(), "kept");
        let label = format!("nants.project={}", self.project_id);
        let tail = ["run", "--name", name.as_str(), "--label", label.as_str(), "-T", "--entrypoint", "sh", WORKSPACE_SERVICE, "-lc", command];
        let ran = self.runner.run_watched(&self.call(dir, &ov, &tail, timeout), progress);
        let diff = match &ran {
            Ok(_) => self.container_diff(dir, &name),
            Err(e) => Err(e.clone()),
        };
        let cleanup_error = self.remove_container(dir, &name).err();
        let output = ran?;
        if !output.success() && is_docker_failure(&output.output) {
            return Err(format!("{DOCKER_UNAVAILABLE}: {}", diagnose(&output)));
        }
        Ok(KeptRun { output, diff, container: name, cleanup_error })
    }

    /// Spec 130 T03 (Console): roda o comando digitado pelo usuário no `workspace` num container
    /// efêmero (`run --rm -T`, sem TTY, stdin fechado), com o mesmo override de sempre (`.git` e
    /// `.nants` só leitura, uid do host, pasta pessoal da 129), o nome `container` e o rótulo
    /// `nants.project`. A saída vai a `sink` enquanto roda; `cancel` ou o tempo limite encerram o
    /// cliente, e quem chama remove o container pelo nome ([`Self::remove_container`]). `Err` =
    /// falha do Docker (o código de saída do comando vai no resultado).
    pub fn console_run(
        &self,
        dir: &Path,
        container: &str,
        command: &str,
        timeout: Duration,
        sink: OutputSink,
        cancel: Arc<AtomicBool>,
    ) -> Result<ComposeOutput, String> {
        let (_, ov, _) = self.load(dir)?;
        let label = format!("nants.project={}", self.project_id);
        let tail = ["run", "--rm", "--name", container, "--label", label.as_str(), "-T", "--entrypoint", "sh", WORKSPACE_SERVICE, "-lc", command];
        let out = self.runner.run_streamed(&self.call(dir, &ov, &tail, timeout), sink, cancel)?;
        if !out.success() && is_docker_failure(&out.output) {
            return Err(format!("{DOCKER_UNAVAILABLE}: {}", diagnose(&out)));
        }
        Ok(out)
    }

    /// Nome do container de um comando do Console (spec 130 T03): `<projeto Compose>-console-<exec>`.
    pub fn console_container_name(&self, exec_id: &str) -> String {
        format!("{}-console-{exec_id}", self.project_name())
    }

    /// Spec 129 T02 (C-09): `docker diff <container>`, a função única do Nants para saber o que um
    /// container que terminou gravou fora das montagens (as montagens, como `/work`, não aparecem).
    pub fn container_diff(&self, dir: &Path, container: &str) -> Result<Vec<DiffEntry>, String> {
        let out = self.runner.run(&self.docker_call(dir, &["diff", container]))?;
        if !out.success() {
            return Err(format!("docker diff falhou: {}", diagnose(&out)));
        }
        Ok(parse_docker_diff(&out.output))
    }

    /// `docker rm -f <container>` (idempotente para o chamador: o container some mesmo parado).
    pub fn remove_container(&self, dir: &Path, container: &str) -> Result<(), String> {
        let out = self.runner.run(&self.docker_call(dir, &["rm", "-f", container]))?;
        if out.success() {
            Ok(())
        } else {
            Err(format!("docker rm falhou: {}", diagnose(&out)))
        }
    }

    /// Chamada direta ao `docker` (sem o prefixo do Compose), com o mesmo ambiente controlado.
    fn docker_call(&self, dir: &Path, args: &[&str]) -> ComposeCall {
        ComposeCall {
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: dir.to_path_buf(),
            env: controlled_env(std::env::vars(), dir),
            timeout: QUERY_TIMEOUT,
        }
    }

    /// Medida das capacidades do ambiente (spec 124 T04): `run --rm -T --no-deps --entrypoint sh
    /// workspace -c <script>`, sem subir os serviços de apoio. `Err` = falha do Docker.
    pub fn probe(&self, dir: &Path, script: &str, timeout: Duration) -> Result<ComposeOutput, String> {
        let (_, ov, _) = self.load(dir)?;
        let name = unique_container_name(&self.project_name(), "probe");
        let tail = ["run", "--rm", "--name", name.as_str(), "-T", "--no-deps", "--entrypoint", "sh", WORKSPACE_SERVICE, "-c", script];
        let ran = self.runner.run(&self.call(dir, &ov, &tail, timeout));
        self.stop_if_left_running(dir, &name, ran)
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
        self.down_full(dir, volumes, false)
    }

    /// Spec 133 T12: como [`Self::down_with`], e apaga também as imagens que o Compose construiu
    /// para o projeto (`--rmi local`: as do `build` sem `image` próprio, como
    /// `nants-<id>-test-workspace`, que levam o rótulo `nants.project`). Só na remoção do projeto.
    pub fn down_removing_images(&self, dir: &Path, volumes: bool) -> Result<(), String> {
        self.down_full(dir, volumes, true)
    }

    fn down_full(&self, dir: &Path, volumes: bool, images: bool) -> Result<(), String> {
        let (_, ov, _) = self.load(dir)?;
        let mut tail = vec!["down"];
        if self.kind == ComposeKind::Run {
            // Parada rápida: o processo do container não é o PID 1 real (init: true repassa sinais).
            tail.extend(["--timeout", "5"]);
        }
        if volumes {
            tail.push("-v");
        }
        if images {
            tail.extend(["--rmi", "local"]);
        }
        tail.push("--remove-orphans");
        self.step(&self.call(dir, &ov, &tail, PREPARE_TIMEOUT), "down")
    }

    /// Sobe o projeto de execução: `up -d --build` (todos os serviços). `run_command` é o comando
    /// INTERNO que o `workspace` executa (`sh -lc <run_command>`), já normalizado (039). Sem `--wait`
    /// (spec 130 T02): um comando que roda e termina não é falha; quem decide o modo é
    /// [`Self::observe_run`].
    pub fn up_run(&self, dir: &Path, run_command: &str) -> Result<(), String> {
        let (_, ov, _) = self.load_with(dir, Some(run_command))?;
        self.step(&self.call(dir, &ov, &["up", "-d", "--build"], PREPARE_TIMEOUT), "up")
    }

    /// Estado do `-run` pelo `ps --all --format json`: o `workspace` (vivo ou encerrado, com o
    /// código e o nome do container) e as portas publicadas.
    pub fn run_state(&self, dir: &Path) -> Result<RunState, String> {
        let (_, ov, _) = self.load(dir)?;
        let out = self.runner.run(&self.call(dir, &ov, &["ps", "--all", "--format", "json"], QUERY_TIMEOUT))?;
        if !out.success() {
            return Err(format!("docker compose ps falhou: {}", diagnose(&out)));
        }
        Ok(parse_run_state(&out.output))
    }

    /// Spec 130 T02: observa o `workspace` depois do `up` por até `window` e classifica pelo
    /// comportamento, nunca pela tecnologia: saiu → [`RunMode::Finished`] (com o código); uma porta
    /// publicada responde HTTP → [`RunMode::Page`]; vivo ao fim da janela → [`RunMode::Running`].
    /// Janela zero: uma consulta só, sem sondar portas. As portas em `ignore` (a do Nants) nunca são
    /// sondadas.
    pub fn observe_run(&self, dir: &Path, window: Duration, ignore: &[u16]) -> Result<Observed, String> {
        let start = Instant::now();
        loop {
            let state = self.run_state(dir)?;
            if state.workspace_exited() {
                return Ok(Observed { mode: RunMode::Finished, state });
            }
            if !window.is_zero() && state.ports.iter().any(|p| !ignore.contains(&p.published) && port_speaks_http(p.published)) {
                return Ok(Observed { mode: RunMode::Page, state });
            }
            if start.elapsed() >= window {
                return Ok(Observed { mode: RunMode::Running, state });
            }
            thread::sleep(OBSERVE_POLL.min(window.saturating_sub(start.elapsed())));
        }
    }

    /// Saída do `workspace` sem o prefixo do serviço (`logs --no-color --no-log-prefix --tail`), para
    /// o modo "terminou" (spec 130 T02).
    pub fn workspace_output(&self, dir: &Path, lines: u32) -> Result<String, String> {
        let (_, ov, _) = self.load(dir)?;
        let n = lines.to_string();
        let tail = ["logs", "--no-color", "--no-log-prefix", "--tail", n.as_str(), WORKSPACE_SERVICE];
        let out = self.runner.run(&self.call(dir, &ov, &tail, QUERY_TIMEOUT))?;
        if !out.success() {
            return Err(format!("docker compose logs falhou: {}", diagnose(&out)));
        }
        Ok(out.output)
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

/// Resultado de [`ComposeEnv::run_kept`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptRun {
    pub output: ComposeOutput,
    /// O que o container gravou, pelo `docker diff`; `Err` com o motivo quando a listagem falhou.
    pub diff: Result<Vec<DiffEntry>, String>,
    /// Nome do container (já removido, salvo `cleanup_error`).
    pub container: String,
    /// Motivo, quando a remoção do container falhou.
    pub cleanup_error: Option<String>,
}

/// Tipo de uma linha do `docker diff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Changed,
    Deleted,
}

/// Uma linha do `docker diff` (`A /caminho`, `C /caminho` ou `D /caminho`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    pub kind: DiffKind,
    pub path: String,
}

/// Lê a saída do `docker diff`; linhas fora do formato são ignoradas.
pub fn parse_docker_diff(text: &str) -> Vec<DiffEntry> {
    text.lines()
        .filter_map(|line| {
            let (kind, path) = line.trim_end().split_once(' ')?;
            let kind = match kind {
                "A" => DiffKind::Added,
                "C" => DiffKind::Changed,
                "D" => DiffKind::Deleted,
                _ => return None,
            };
            let path = path.trim();
            path.starts_with('/').then(|| DiffEntry { kind, path: path.to_string() })
        })
        .collect()
}

/// Spec 129 T02 (C-02): caminhos gravados fora de `/work` que ainda existem ao fim (entradas `A` ou
/// `C`; as `D` são ignoradas). O `docker diff` de um container encerrado já é o estado final.
pub fn written_outside_work(entries: &[DiffEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.kind != DiffKind::Deleted)
        .filter(|e| e.path != "/work" && !e.path.starts_with("/work/"))
        .map(|e| e.path.clone())
        .collect()
}

/// Spec 129 T02: os caminhos principais de uma lista, agrupados pelos `depth` primeiros
/// componentes, com a contagem de cada grupo, do maior para o menor (até `max`). Grupo que é só o
/// ancestral de outro (`/tmp` diante de `/tmp/.local/lib`) não entra: ele não diz onde foi gravado.
pub fn main_paths(paths: &[String], depth: usize, max: usize) -> Vec<(String, usize)> {
    let mut groups: Vec<(String, usize)> = Vec::new();
    for p in paths {
        let key: String = p.split('/').filter(|c| !c.is_empty()).take(depth.max(1)).fold(String::new(), |mut acc, c| {
            acc.push('/');
            acc.push_str(c);
            acc
        });
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, n)) => *n += 1,
            None => groups.push((key, 1)),
        }
    }
    let keys: Vec<String> = groups.iter().map(|(k, _)| k.clone()).collect();
    groups.retain(|(k, _)| !keys.iter().any(|other| other.len() > k.len() && other.starts_with(&format!("{k}/"))));
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    groups.truncate(max);
    groups
}

/// Nome de um container de comando: `<projeto>-<kind>-<pid>-<seq>-<nanos>`, único no processo e
/// com o prefixo do projeto do Nants (`kept` em [`ComposeEnv::run_kept`], `exec` e `probe`).
fn unique_container_name(project: &str, kind: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    format!("{project}-{kind}-{}-{}-{nanos}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst))
}

/// Intervalo entre consultas durante a observação pós-`up`.
const OBSERVE_POLL: Duration = Duration::from_millis(1000);

/// Modo de execução observado (spec 130 T02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Uma porta publicada responde HTTP: o navegador com o app.
    Page,
    /// Segue vivo sem página: log ao vivo, portas e o botão de parar.
    Running,
    /// Rodou e terminou: saída, código e arquivos.
    Finished,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Running => "running",
            Self::Finished => "finished",
        }
    }
}

/// O `workspace` no `ps --all`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceState {
    /// Nome do container (para o `docker diff`).
    pub container: Option<String>,
    /// `State` do Compose (`running`, `exited`, `dead`, `restarting`...).
    pub state: String,
    /// `ExitCode` (só tem sentido encerrado).
    pub exit_code: Option<i64>,
}

/// Estado do projeto `-run` ([`ComposeEnv::run_state`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunState {
    /// `None` quando o `workspace` não aparece (container removido): conta como encerrado.
    pub workspace: Option<WorkspaceState>,
    pub ports: Vec<PublishedPort>,
}

impl RunState {
    /// O `workspace` saiu (ou sumiu).
    pub fn workspace_exited(&self) -> bool {
        self.workspace.as_ref().is_none_or(|w| w.state == "exited" || w.state == "dead")
    }

    /// Código de saída do `workspace` encerrado.
    pub fn exit_code(&self) -> Option<i64> {
        self.workspace.as_ref().and_then(|w| w.exit_code)
    }

    /// Nome do container do `workspace`.
    pub fn container(&self) -> Option<&str> {
        self.workspace.as_ref().and_then(|w| w.container.as_deref())
    }
}

/// Resultado de [`ComposeEnv::observe_run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub mode: RunMode,
    pub state: RunState,
}

/// Itens do `ps --format json` (array JSON ou uma linha JSON por container, conforme a versão).
fn ps_items(text: &str) -> Vec<serde_json::Value> {
    let trimmed = text.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Array(a)) => a,
        Ok(v @ serde_json::Value::Object(_)) => vec![v],
        _ => trimmed.lines().filter_map(|l| serde_json::from_str(l.trim()).ok()).collect(),
    }
}

/// Lê o `ps --all --format json` do `-run`.
pub fn parse_run_state(text: &str) -> RunState {
    let items = ps_items(text);
    let workspace = items.iter().find(|i| i.get("Service").and_then(|v| v.as_str()) == Some(WORKSPACE_SERVICE)).map(|i| WorkspaceState {
        container: i.get("Name").and_then(|v| v.as_str()).filter(|n| !n.is_empty()).map(str::to_string),
        state: i.get("State").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        exit_code: i.get("ExitCode").and_then(|v| v.as_i64()),
    });
    RunState { workspace, ports: parse_ps_ports(text) }
}

/// A porta publicada responde HTTP de verdade? O docker-proxy aceita o connect mesmo com o backend
/// morto, e um serviço que não é HTTP pode responder outra coisa: exige que a resposta a um GET
/// comece por `HTTP/`.
pub fn port_speaks_http(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    let wait = Some(Duration::from_millis(500));
    let _ = (s.set_read_timeout(wait), s.set_write_timeout(wait));
    if s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n").is_err() {
        return false;
    }
    let mut got = Vec::new();
    let mut b = [0u8; 5];
    while got.len() < 5 {
        match s.read(&mut b[..5 - got.len()]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&b[..n]),
        }
    }
    got == b"HTTP/"
}

/// Extrai as portas publicadas da saída de `compose ps --format json` (array JSON ou uma linha
/// JSON por container, conforme a versão do Compose).
pub fn parse_ps_ports(text: &str) -> Vec<PublishedPort> {
    let items = ps_items(text);
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
    override_yaml_with(project_id, worktree, doc, run_command, lan_ports, &mut Vec::new(), &|_, _| true, &|_| None, ContainerLimits::host(), None)
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
    limits: ContainerLimits,
    offline: Option<&str>,
) -> Result<String, String> {
    let label = Value::Sequence(vec![Value::String(format!("nants.project={project_id}"))]);
    if offline.is_some() && doc.get("networks").and_then(|n| n.get(OFFLINE_NETWORK)).is_some() {
        return Err(format!("compose.yaml usa o nome de rede reservado `{OFFLINE_NETWORK}`"));
    }
    let abs = worktree.canonicalize().map_err(|e| format!("worktree inválida: {e}"))?;
    let src = abs.to_string_lossy().replace('$', "$$");
    let mut services = Mapping::new();
    for name in service_names(doc) {
        let mut svc = Mapping::new();
        svc.insert("labels".into(), label.clone());
        // Spec 134 T08: limites de memória e de processos em TODO serviço. O override é juntado por
        // cima do compose do projeto, então um valor maior (ou nenhum) dele vira o teto.
        let base = doc.get("services").and_then(|s| s.get(name.as_str()));
        svc.extend(limits.override_keys(base));
        // Spec 133 T12: a imagem que o Compose constrói para o serviço também leva o rótulo (o
        // Compose junta este `build` ao do arquivo do projeto, que segue com o contexto).
        if doc.get("services").and_then(|s| s.get(name.as_str())).is_some_and(|s| s.get("build").is_some()) {
            let mut build = Mapping::new();
            build.insert("labels".into(), label.clone());
            svc.insert("build".into(), Value::Mapping(build));
        }
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
            // Spec 129 T01: `HOME` dentro de `/work` (persiste entre os containers, um por comando).
            // O `.profile` dela põe `$HOME/.local/bin` na frente do `PATH` nos shells de login.
            let mut env = Mapping::new();
            env.insert("HOME".into(), home::CONTAINER_HOME.into());
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
        if offline.is_some() {
            svc.insert("networks".into(), offline_service_networks(&name, base));
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
    if let Some(name) = offline {
        let mut def = Mapping::new();
        def.insert("name".into(), Value::String(name.to_string()));
        def.insert("internal".into(), Value::Bool(true));
        def.insert("labels".into(), label.clone());
        let nets = root.entry("networks".into()).or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Value::Mapping(m) = nets {
            m.insert(OFFLINE_NETWORK.into(), Value::Mapping(def));
        }
    }
    serde_yaml::to_string(&Value::Mapping(root)).map_err(|e| format!("override inválido: {e}"))
}

/// Serviços de apoio (todos menos o `workspace`).
fn support_services(doc: &Value) -> Vec<String> {
    service_names(doc).into_iter().filter(|s| s != WORKSPACE_SERVICE).collect()
}

/// Spec 137 T09: redes de um serviço na rodada offline, com `!override` (a lista do compose do
/// projeto é substituída, não concatenada). O `workspace` fica só na rede interna; os serviços de
/// apoio ficam nas redes deles (a `default`, quando o compose não lista nenhuma) e na interna.
fn offline_service_networks(name: &str, base: Option<&Value>) -> Value {
    let mut nets = Mapping::new();
    if name != WORKSPACE_SERVICE {
        match base.and_then(|b| b.get("networks")) {
            Some(Value::Sequence(list)) => {
                for n in list {
                    nets.insert(n.clone(), Value::Null);
                }
            }
            Some(Value::Mapping(m)) => {
                for (k, v) in m {
                    nets.insert(k.clone(), v.clone());
                }
            }
            _ => {}
        }
        if nets.is_empty() {
            nets.insert("default".into(), Value::Null);
        }
    }
    nets.insert(OFFLINE_NETWORK.into(), Value::Null);
    Value::Tagged(Box::new(serde_yaml::value::TaggedValue { tag: serde_yaml::value::Tag::new("override"), value: Value::Mapping(nets) }))
}

/// Grava o override em `dir` (privado): remove o arquivo anterior (unlink não segue symlink) e
/// cria um novo com `create_new` (O_EXCL: nunca segue symlink) e modo 0600.
#[allow(clippy::too_many_arguments)]
fn write_override(
    dir: &Path,
    project_id: &str,
    worktree: &Path,
    doc: &Value,
    run_command: Option<&str>,
    lan_ports: Option<ViewPorts>,
    remaps: &mut Vec<(u16, u16)>,
    limits: ContainerLimits,
    offline: Option<&str>,
) -> Result<PathBuf, String> {
    let text = override_yaml_with(project_id, worktree, doc, run_command, lan_ports, remaps, &host_port_free, &host_port_alloc, limits, offline)?;
    let path = dir.join(if offline.is_some() { OFFLINE_OVERRIDE_FILE } else { "override.yaml" });
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
        /// Linhas escritas aos poucos durante o atraso (no progresso, quando há), spec 123 T01.
        pub stream: Mutex<Vec<String>>,
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

    /// Token após o segundo `-f <arquivo>`; numa chamada direta ao `docker` (sem `compose`), o
    /// primeiro argumento (`diff`, `rm`).
    pub fn sub_of(args: &[String]) -> &str {
        if args.first().is_some_and(|a| a != "compose") {
            return args[0].as_str();
        }
        let last_f = args.iter().rposition(|a| a == "-f").unwrap_or(0);
        args.get(last_f + 2).map_or("", String::as_str)
    }

    impl ComposeRunner for FakeRunner {
        fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
            self.run_watched(call, None)
        }

        fn run_watched(&self, call: &ComposeCall, progress: Option<&Arc<CommandProgress>>) -> Result<ComposeOutput, String> {
            self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call.clone());
            let delay = *self.delay.lock().unwrap_or_else(PoisonError::into_inner);
            let stream = self.stream.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if let Some(d) = delay {
                // O atraso é repartido entre as linhas: cada uma sai depois da sua fatia.
                let slices = u32::try_from(stream.len().max(1)).unwrap_or(u32::MAX);
                let step = d / slices;
                for line in &stream {
                    thread::sleep(step);
                    if let Some(p) = progress {
                        p.push(line.as_bytes());
                        p.push(b"\n");
                    }
                }
                if stream.is_empty() {
                    thread::sleep(d);
                }
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
        assert_eq!(fake.subcommands(), vec!["down", "network", "build", "up", "run"]);
        let calls = fake.calls.lock().expect("lock");
        assert_eq!(&calls[0].args[..5], ["compose", "-p", "nants-p1-test", "-f", ".nants/compose.yaml"]);
        assert_eq!(&calls[0].args[7..], ["down", "-v", "--remove-orphans"]);
        // Spec 137 T09: o `down` não conhece a rede da rodada offline; a sobra sai pelo nome.
        assert_eq!(calls[1].args, ["network", "rm", "-f", "nants-p1-test-nants-offline"]);
        assert_eq!(&calls[3].args[7..], ["up", "-d", "--wait", "db"]);
        assert_eq!(&calls[4].args[7..10], ["run", "--rm", "--name"]);
        assert!(calls[4].args[10].starts_with("nants-p1-test-exec-"), "{:?}", calls[4].args);
        assert_eq!(&calls[4].args[11..], ["-T", "--entrypoint", "sh", "workspace", "-lc", "cargo test"]);
        assert_eq!(calls[4].cwd, dir);
    }

    /// Lê o override gravado de uma chamada do Compose (o segundo `-f`).
    fn override_of(call: &ComposeCall) -> Value {
        serde_yaml::from_str(&fs::read_to_string(&call.args[6]).expect("override")).expect("yaml")
    }

    /// Spec 134 T08: todo serviço do override leva memória (sem swap extra), processos e o
    /// encerramento por falta de memória ligado; o menor valor do projeto vale, o maior vira o teto.
    #[test]
    fn override_caps_memory_and_processes_of_every_service() {
        const MIB: u64 = 1024 * 1024;
        let compose = "services:\n  workspace:\n    image: alpine\n    mem_limit: 64g\n    pids_limit: -1\n    oom_kill_disable: true\n  db:\n    image: postgres:16\n    mem_limit: 128m\n    pids_limit: 40\n  cache:\n    image: redis\n";
        let dir = project("limits", compose);
        let fake = Arc::new(FakeRunner::default());
        let cap = ContainerLimits { memory: 256 * MIB, pids: 64 };
        let env = env_with(&fake).with_limits(cap);
        assert_eq!(env.workspace_limits(), cap, "antes do primeiro override, o teto");
        let hash_before = env_hash(&dir);
        env.exec(&dir, "make check", Duration::from_secs(5)).expect("exec");
        let calls = fake.calls.lock().expect("lock");
        let ov = override_of(&calls[0]);
        let limits = |svc: &str| {
            let s = &ov["services"][svc];
            (s["mem_limit"].as_u64(), s["memswap_limit"].as_u64(), s["pids_limit"].as_u64(), s["oom_kill_disable"].as_bool())
        };
        assert_eq!(limits("workspace"), (Some(256 * MIB), Some(256 * MIB), Some(64), Some(false)));
        assert_eq!(limits("db"), (Some(128 * MIB), Some(128 * MIB), Some(40), Some(false)));
        assert_eq!(limits("cache"), (Some(256 * MIB), Some(256 * MIB), Some(64), Some(false)));
        assert_eq!(env.workspace_limits(), cap);
        // O override fica fora da worktree e não muda o hash de aprovação do ambiente.
        assert_eq!(env_hash(&dir), hash_before);
        assert!(!calls[0].args[6].starts_with(&*dir.to_string_lossy()), "override fora da worktree");

        // Sem `with_limits`, o teto é o da máquina.
        let doc: Value = serde_yaml::from_str(COMPOSE).expect("yaml");
        let text = override_yaml("p1", &dir, &doc, Some("npm start"), None).expect("override");
        let v: Value = serde_yaml::from_str(&text).expect("parse");
        let host = ContainerLimits::host();
        for svc in ["workspace", "db"] {
            assert_eq!(v["services"][svc]["mem_limit"].as_u64(), Some(host.memory), "{svc}");
            assert_eq!(v["services"][svc]["pids_limit"].as_u64(), Some(host.pids), "{svc}");
        }
    }

    /// Redes (sem a tag `!override`) de um serviço do override da rodada offline.
    fn offline_nets(ov: &Value, svc: &str) -> Vec<String> {
        let Value::Tagged(t) = &ov["services"][svc]["networks"] else { panic!("sem !override: {ov:?}") };
        assert_eq!(t.tag, serde_yaml::value::Tag::new("override"));
        t.value.as_mapping().expect("mapa").keys().filter_map(|k| k.as_str().map(String::from)).collect()
    }

    /// Spec 137 T09 (critérios 1 e 2): a rodada offline entra pelo override, com uma rede
    /// `internal: true` com nome e rótulo do projeto. O `workspace` fica só nela; os serviços de
    /// apoio ficam nas redes deles e nela; os limites da 134/T08 seguem em todo serviço.
    #[test]
    fn offline_override_puts_workspace_only_on_the_internal_network() {
        const MIB: u64 = 1024 * 1024;
        let compose = "services:\n  workspace:\n    image: alpine\n    networks: [back]\n  db:\n    image: postgres:16\n    networks:\n      back:\n        aliases: [banco]\n  cache:\n    image: redis\nnetworks:\n  back: {}\n";
        let dir = project("offline-ov", compose);
        let doc: Value = serde_yaml::from_str(compose).expect("yaml");
        let cap = ContainerLimits { memory: 256 * MIB, pids: 64 };
        let text = override_yaml_with("p1", &dir, &doc, None, None, &mut Vec::new(), &|_, _| true, &|_| None, cap, Some("nants-p1-test-nants-offline")).expect("ov");
        let ov: Value = serde_yaml::from_str(&text).expect("yaml");
        assert_eq!(offline_nets(&ov, "workspace"), vec![OFFLINE_NETWORK]);
        assert_eq!(offline_nets(&ov, "db"), vec!["back", OFFLINE_NETWORK]);
        assert_eq!(offline_nets(&ov, "cache"), vec!["default", OFFLINE_NETWORK]);
        let Value::Tagged(db) = &ov["services"]["db"]["networks"] else { panic!("tag") };
        assert_eq!(db.value["back"]["aliases"][0].as_str(), Some("banco"), "as opções da rede do projeto ficam");
        let net = &ov["networks"][OFFLINE_NETWORK];
        assert_eq!(net["internal"].as_bool(), Some(true), "{text}");
        assert_eq!(net["name"].as_str(), Some("nants-p1-test-nants-offline"));
        assert_eq!(net["labels"][0].as_str(), Some("nants.project=p1"));
        assert_eq!(ov["networks"]["back"]["labels"][0].as_str(), Some("nants.project=p1"));
        assert!(!text.contains("network_mode"), "{text}");
        for svc in ["workspace", "db", "cache"] {
            assert_eq!(ov["services"][svc]["mem_limit"].as_u64(), Some(256 * MIB), "{svc}");
            assert_eq!(ov["services"][svc]["pids_limit"].as_u64(), Some(64), "{svc}");
            assert_eq!(ov["services"][svc]["labels"][0].as_str(), Some("nants.project=p1"), "{svc}");
        }

        // A rodada normal não ganha rede nenhuma do Nants.
        let normal = override_yaml("p1", &dir, &doc, None, None).expect("ov");
        assert!(!normal.contains(OFFLINE_NETWORK) && !normal.contains("internal"), "{normal}");

        // O nome reservado no compose do projeto é recusado (não se mistura com a rede dele).
        let clash: Value = serde_yaml::from_str("services:\n  workspace:\n    image: alpine\nnetworks:\n  nants-offline: {}\n").expect("yaml");
        let err = override_yaml_with("p1", &dir, &clash, None, None, &mut Vec::new(), &|_, _| true, &|_| None, cap, Some("x")).expect_err("reservado");
        assert!(err.contains(OFFLINE_NETWORK), "{err}");
    }

    /// Spec 137 T09 (critérios 1 e 3): a rodada offline não recria o estado (sem `down -v`, sem
    /// build, sem instalação): liga os serviços de apoio à rede interna, roda o comando no
    /// `workspace` pelo override offline, devolve os serviços ao override de sempre e remove a rede.
    #[test]
    fn exec_offline_runs_on_the_offline_override_and_removes_the_network() {
        let dir = project("offline-seq", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake);
        let out = env.exec_offline(&dir, "npm test", Duration::from_secs(5), None).expect("offline");
        assert!(out.success());
        assert_eq!(fake.subcommands(), vec!["network", "up", "run", "up", "network"]);
        let calls = fake.calls.lock().expect("lock");
        // A sobra de uma rodada interrompida sai antes do `up` (o `-f` ignora a rede que não existe).
        assert_eq!(calls[0].args, ["network", "rm", "-f", "nants-p1-test-nants-offline"]);
        assert!(calls[1].args[6].ends_with(OFFLINE_OVERRIDE_FILE), "{:?}", calls[1].args);
        assert_eq!(&calls[1].args[7..], ["up", "-d", "--wait", "db"]);
        assert!(calls[2].args[6].ends_with(OFFLINE_OVERRIDE_FILE));
        assert_eq!(&calls[2].args[7..10], ["run", "--rm", "--name"]);
        assert!(calls[2].args[10].starts_with("nants-p1-test-offline-"), "{:?}", calls[2].args);
        assert_eq!(&calls[2].args[11..], ["-T", "--entrypoint", "sh", "workspace", "-lc", "npm test"]);
        assert_eq!(offline_nets(&override_of(&calls[2]), "workspace"), vec![OFFLINE_NETWORK]);
        // Volta ao override de sempre (sem a rede interna) e remove a rede pelo nome.
        assert!(calls[3].args[6].ends_with("/override.yaml"), "{:?}", calls[3].args);
        assert!(override_of(&calls[3])["networks"].get(OFFLINE_NETWORK).is_none());
        assert_eq!(calls[4].args, ["network", "rm", "-f", "nants-p1-test-nants-offline"]);
        assert!(calls.iter().all(|c| !c.args.iter().any(|a| a == "-v")), "o cache de dependências fica");
        drop(calls);

        // Sem serviço de apoio: só o comando e a remoção da rede.
        let solo = project("offline-solo", "services:\n  workspace:\n    image: alpine\n");
        let fake = Arc::new(FakeRunner::default());
        env_with(&fake).exec_offline(&solo, "make test", Duration::from_secs(5), None).expect("offline");
        assert_eq!(fake.subcommands(), vec!["network", "run", "network"]);
    }

    /// Spec 137 T09: o comando que falha devolve o código (é resultado, não falha do Docker) e a
    /// rede sai mesmo assim; se a volta falhar, um `down` sem `-v` pelo override offline remove tudo.
    #[test]
    fn exec_offline_cleans_up_on_failure() {
        let dir = project("offline-fail", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        fake.reply("run", 7, "getaddrinfo ENOTFOUND registry.npmjs.org");
        let out = env_with(&fake).exec_offline(&dir, "npm test", Duration::from_secs(5), None).expect("resultado");
        assert_eq!(out.code, Some(7));
        assert_eq!(fake.subcommands(), vec!["network", "up", "run", "up", "network"]);

        let fake = Arc::new(FakeRunner::default());
        fake.reply("network", 1, "network has active endpoints");
        let err = env_with(&fake).exec_offline(&dir, "npm test", Duration::from_secs(5), None).expect_err("rede ficou");
        assert!(err.contains("network rm"), "{err}");
        assert_eq!(fake.subcommands(), vec!["network", "up", "run", "up", "network", "rm", "network"]);
        let calls = fake.calls.lock().expect("lock");
        // Revisão 1 (F-01): os serviços de apoio saem com os volumes anônimos deles (nada órfão); o
        // `-v` do `rm` não alcança os volumes nomeados, e nenhum `down` roda.
        assert_eq!(&calls[5].args[7..], ["rm", "--stop", "--force", "--volumes", "db"]);
        assert!(calls[5].args[6].ends_with(OFFLINE_OVERRIDE_FILE));
        assert!(calls.iter().all(|c| crate::harness::compose::fake::sub_of(&c.args) != "down"), "nenhum down, nem com -v");
        drop(calls);

        // A volta dos serviços de apoio falha: o mesmo caminho de recuperação.
        let fake = Arc::new(FakeRunner::default());
        fake.reply("up", 1, "dependency failed to start");
        let err = env_with(&fake).exec_offline(&dir, "npm test", Duration::from_secs(5), None).expect_err("up falhou");
        assert!(err.contains("up"), "{err}");
        assert_eq!(fake.subcommands(), vec!["network", "up", "up", "rm", "network"], "o up offline e o da volta falham");
    }

    /// Spec 134 T08 (critério 2): no tempo limite o `docker compose` do host morre, mas o container
    /// seguiria vivo; o Nants o remove pelo nome (`docker rm -f`, que encerra todos os processos).
    #[test]
    fn timeout_removes_the_named_container_and_its_processes() {
        let dir = project("timeout", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        fake.reply_timeout("run", "t/00_runner.t ..\n");
        let env = env_with(&fake);
        let out = env.exec(&dir, "perl t/runner.pl", Duration::from_secs(5)).expect("exec");
        assert!(out.timed_out);
        let calls = fake.calls.lock().expect("lock").clone();
        assert_eq!(fake.subcommands(), vec!["run", "rm"]);
        let name = calls[0].args[10].clone();
        assert!(name.starts_with("nants-p1-test-exec-"), "{name}");
        assert_eq!(calls[1].args, ["rm", "-f", name.as_str()]);

        // Sonda das capacidades: o mesmo.
        let fake = Arc::new(FakeRunner::default());
        fake.reply_timeout("run", "");
        let env = env_with(&fake);
        assert!(env.probe(&dir, "uname -a", Duration::from_secs(5)).expect("probe").timed_out);
        let calls = fake.calls.lock().expect("lock").clone();
        assert!(calls[0].args[10].starts_with("nants-p1-test-probe-"), "{:?}", calls[0].args);
        assert_eq!(calls[1].args, ["rm", "-f", calls[0].args[10].as_str()]);

        // Comando que termina no tempo: o `--rm` já removeu; nada a mais.
        let fake = Arc::new(FakeRunner::default());
        fake.reply("run", 1, "falhou");
        let env = env_with(&fake);
        assert_eq!(env.exec(&dir, "make", Duration::from_secs(5)).expect("exec").code, Some(1));
        assert_eq!(fake.subcommands(), vec!["run"]);
        // Nomes diferentes a cada comando.
        assert_ne!(unique_container_name("p", "exec"), unique_container_name("p", "exec"));
    }

    /// Executor que falha ao aguardar o cliente: o container pode ter ficado vivo.
    struct FailingRunner {
        calls: Mutex<Vec<ComposeCall>>,
    }

    impl ComposeRunner for FailingRunner {
        fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
            self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call.clone());
            if call.args.first().is_some_and(|a| a == "rm") {
                return Ok(ComposeOutput { code: Some(0), output: String::new(), timed_out: false });
            }
            Err("falha ao aguardar o docker".into())
        }
    }

    #[test]
    fn client_failure_also_removes_the_container() {
        let dir = project("clientfail", COMPOSE);
        let runner = Arc::new(FailingRunner { calls: Mutex::new(Vec::new()) });
        let env = super::fake::test_env("p1", runner.clone());
        assert!(env.exec(&dir, "make", Duration::from_secs(5)).is_err());
        let calls = runner.calls.lock().expect("lock");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].args, ["rm", "-f", calls[0].args[10].as_str()]);
    }

    #[test]
    fn override_runs_workspace_with_host_uid_and_persistent_home() {
        let dir = project("uid", COMPOSE);
        let doc: Value = serde_yaml::from_str(COMPOSE).expect("yaml");
        let text = override_yaml("p1", &dir, &doc, None, None).expect("override");
        let v: Value = serde_yaml::from_str(&text).expect("parse");
        let (uid, gid) = host_ids().expect("ids");
        let ws = &v["services"]["workspace"];
        assert_eq!(ws["user"].as_str(), Some(format!("{uid}:{gid}").as_str()));
        assert_eq!(ws["environment"]["HOME"].as_str(), Some("/work/.nants-home"));
        // O `PATH` da imagem não é tocado pelo override.
        assert!(ws["environment"].get("PATH").is_none());
        assert!(v["services"]["db"].get("user").is_none());
    }

    /// Executor falso que simula cada `run --rm` do workspace como um container novo: um processo
    /// `sh -lc` próprio, só com o `HOME` do override (com `/work` trocado pela pasta montada) e um
    /// `PATH` mínimo, sem nada herdado do comando anterior.
    #[derive(Default)]
    struct ShellRunner {
        calls: Mutex<Vec<ComposeCall>>,
    }

    impl ComposeRunner for ShellRunner {
        fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
            self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call.clone());
            let ok = ComposeOutput { code: Some(0), output: String::new(), timed_out: false };
            if fake::sub_of(&call.args) != "run" {
                return Ok(ok);
            }
            let ov: Value = serde_yaml::from_str(&fs::read_to_string(&call.args[6]).expect("override")).expect("yaml");
            let home = ov["services"][WORKSPACE_SERVICE]["environment"]["HOME"].as_str().expect("HOME").to_string();
            let root = call.cwd.canonicalize().expect("raiz");
            let host_home = match home.strip_prefix("/work") {
                Some(rest) => format!("{}{rest}", root.display()),
                None => home,
            };
            let script = call.args.last().expect("comando");
            let out = Command::new("sh")
                .args(["-lc", script])
                .env_clear()
                .env("HOME", host_home)
                .env("PATH", "/usr/bin:/bin")
                .current_dir(&root)
                .output()
                .expect("sh");
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            Ok(ComposeOutput { code: out.status.code(), output: text, timed_out: false })
        }
    }

    /// Spec 129 T01: o que um comando grava em `$HOME` (um arquivo e um executável em
    /// `$HOME/.local/bin`) está no comando seguinte, em outro container, sem regra por linguagem.
    #[test]
    fn personal_folder_persists_between_commands_and_its_bin_is_on_the_path() {
        let dir = project("home", COMPOSE);
        let runner = Arc::new(ShellRunner::default());
        let env = super::fake::test_env("p1", runner.clone());
        // A pasta não existe antes; nasce com o primeiro comando, do uid do host e fora do Git.
        assert!(!dir.join(home::HOME_DIR).exists());
        env.prepare(&dir).expect("prepare");
        let h = dir.join(home::HOME_DIR);
        let uid = fs::metadata("/proc/self").expect("self").uid();
        assert_eq!(fs::metadata(&h).expect("pasta").uid(), uid);
        assert_eq!(fs::read_to_string(h.join(".gitignore")).expect("gi"), "*\n");
        let write = "echo gravado > \"$HOME/estado.txt\" && mkdir -p \"$HOME/.local/bin\" \
                     && printf '#!/bin/sh\\necho ferramenta-ok\\n' > \"$HOME/.local/bin/nants-probe-tool\" \
                     && chmod +x \"$HOME/.local/bin/nants-probe-tool\"";
        let first = env.exec(&dir, write, Duration::from_secs(20)).expect("exec");
        assert!(first.success(), "{}", first.output);
        let second = env.exec(&dir, "cat \"$HOME/estado.txt\" && nants-probe-tool", Duration::from_secs(20)).expect("exec");
        assert!(second.success(), "{}", second.output);
        assert!(second.output.contains("gravado") && second.output.contains("ferramenta-ok"), "{}", second.output);
        // O que o comando gravou ficou na área de trabalho, dentro da pasta pessoal.
        assert!(h.join("estado.txt").exists() && h.join(".local/bin/nants-probe-tool").exists());
        // A remoção da área de trabalho leva a pasta junto.
        fs::remove_dir_all(&dir).expect("apagável sem sudo");
        assert!(!h.exists());
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

    /// Spec 122 T02: as íntegras guardadas na raiz do projeto (fora de `.nants/`) não mudam o hash
    /// aprovado, não tocam o `.dockerignore` versionado do projeto e não sujam a base, nem depois
    /// de um `prepare`.
    #[test]
    fn stored_outputs_leave_the_environment_and_the_base_untouched() {
        use crate::agents::spill::{Spill, OUTPUTS_DIR};
        let dir = project("outputs-base", COMPOSE);
        fs::write(dir.join(".nants/Dockerfile.dockerignore"), "node_modules").expect("w");
        let vcs = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=Nants Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git");
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        vcs(&["init", "-q", "-b", "main"]);
        vcs(&["add", "."]);
        vcs(&["commit", "-q", "-m", "inicial"]);
        let good = env_hash(&dir).expect("hash");
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake).with_approved_hash(Some(good.clone()));
        let mut spill = Spill::new(&dir, "architect");
        let rel = spill.store(&"x".repeat(4096)).expect("guardada");
        assert!(rel.starts_with(&format!("{OUTPUTS_DIR}/")), "{rel}");
        assert_eq!(env_hash(&dir), Some(good), "o hash do ambiente não muda");
        env.prepare(&dir).expect("o hash continua aprovado");
        assert!(fake.subcommands().iter().any(|x| x == "build"));
        assert_eq!(fs::read_to_string(dir.join(".nants/Dockerfile.dockerignore")).expect("r"), "node_modules");
        assert_eq!(vcs(&["status", "--porcelain", "--untracked-files=all"]), "", "base limpa");
        drop(spill);
        let _ = fs::remove_dir_all(&dir);
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
        // Spec 133 T12: a imagem construída para o projeto leva o rótulo; o contexto segue o do
        // arquivo do projeto, e o serviço sem `build` não ganha um.
        assert_eq!(ws["build"]["labels"][0].as_str(), Some("nants.project=p1"));
        assert!(ws["build"].get("context").is_none() && ws["build"].get("dockerfile").is_none());
        assert!(v["services"]["db"].get("build").is_none());
    }

    /// Spec 129 T05 (C-14): o padrão `${NANTS_WORKTREE:-..}` vale só para o uso manual; no Nants, o
    /// override monta o caminho absoluto da worktree em /work e o subprocesso recebe a variável.
    #[test]
    fn worktree_default_is_manual_only_and_override_mounts_the_absolute_path() {
        let compose = COMPOSE.replace("${NANTS_WORKTREE}", "${NANTS_WORKTREE:-..}");
        assert_eq!(crate::sdd::environment::validate_compose(&compose), Ok(()));
        let dir = project("ov-default", &compose);
        let doc: Value = serde_yaml::from_str(&compose).expect("yaml");
        let text = override_yaml("p1", &dir, &doc, None, None).expect("ov");
        let v: Value = serde_yaml::from_str(&text).expect("parse");
        let ws = &v["services"]["workspace"];
        let abs = dir.canonicalize().expect("c");
        assert_eq!(ws["volumes"][0]["target"].as_str(), Some("/work"));
        assert_eq!(ws["volumes"][0]["source"].as_str(), abs.to_str());
        assert!(!text.contains("NANTS_WORKTREE"), "o override nunca depende da variável: {text}");
        let env = controlled_env(Vec::new(), &abs);
        assert_eq!(env.iter().find(|(k, _)| k == WORKTREE_VAR).map(|(_, v)| v.as_str()), abs.to_str());
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
        let text = override_yaml_with("p1", &dir, &doc, Some("run"), None, &mut remaps, &|_, p| p != 3000, &|_| Some(41234), ContainerLimits::host(), None).expect("ov");
        assert_eq!(remaps, vec![(3000, 41234)]);
        let v: Value = serde_yaml::from_str(&text).expect("parse");
        let Value::Tagged(t) = &v["services"]["workspace"]["ports"] else { panic!("sem tag: {text}") };
        let list = t.value.as_sequence().expect("lista");
        assert_eq!(list[0].as_str(), Some("127.0.0.1:41234:3000"));
        assert_eq!(list[1].as_str(), Some("127.0.0.1:5173:5173"));
        // Tudo livre: nenhum override de portas; sem comando (-test): idem.
        let mut none = Vec::new();
        let free = override_yaml_with("p1", &dir, &doc, Some("run"), None, &mut none, &|_, _| true, &|_| None, ContainerLimits::host(), None).expect("ov");
        assert!(none.is_empty() && !free.contains("ports"), "{free}");
        let test = override_yaml_with("p1", &dir, &doc, None, None, &mut none, &|_, _| false, &|_| Some(1), ContainerLimits::host(), None).expect("ov");
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
        let text = override_yaml_with("p1", &dir, &doc, Some("npm run dev"), Some(VIEW), &mut remaps, &|_, _| true, &|_| Some(41234), ContainerLimits::host(), None).expect("ov");
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
        let text = override_yaml_with("p1", &dir, &doc, Some("run"), Some(VIEW), &mut Vec::new(), &busy, &|_| Some(41234), ContainerLimits::host(), None).expect("ov");
        let list = lan_list(&text);
        assert_eq!(list.iter().filter_map(Value::as_str).collect::<Vec<_>>(), ["0.0.0.0:3002:5173", "0.0.0.0:3004:8000", "0.0.0.0:3005:8001"]);
    }

    #[test]
    fn lan_range_exhausted_fails_with_clear_message_and_no_random_port() {
        let compose = "services:\n  workspace:\n    image: x\n    ports:\n      - \"127.0.0.1:5173:5173\"\n      - \"8080\"\n";
        let dir = project("lanfull", compose);
        let doc: Value = serde_yaml::from_str(compose).expect("yaml");
        let expected = "Todas as portas de visualização (3001 a 3010) estão em uso. Pare outro app na Execução e tente de novo.";
        let err = override_yaml_with("p1", &dir, &doc, Some("run"), Some(VIEW), &mut Vec::new(), &|_, _| false, &|_| Some(41234), ContainerLimits::host(), None).expect_err("esgotada");
        assert_eq!(err, expected);
        // Uma livre só, duas portas pedidas: também esgota (nunca repete nem sai da faixa).
        let one = |_: &str, p: u16| p == 3010;
        let err = override_yaml_with("p1", &dir, &doc, Some("run"), Some(VIEW), &mut Vec::new(), &one, &|_| Some(41234), ContainerLimits::host(), None).expect_err("esgotada");
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
        let with_busy = override_yaml_with("p1", &dir, &doc, Some("run"), None, &mut Vec::new(), &|_, p| p != 5173, &|_| Some(41234), ContainerLimits::host(), None).expect("ov");
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

    /// Linha do `ps --all --format json` do `workspace`, com uma porta publicada opcional.
    fn ps_workspace(state: &str, code: i64, port: Option<u16>) -> String {
        let pubs = port.map_or(String::new(), |p| format!(r#"{{"URL":"127.0.0.1","TargetPort":8080,"PublishedPort":{p},"Protocol":"tcp"}}"#));
        format!(r#"{{"Service":"workspace","Name":"nants-p1-run-workspace-1","State":"{state}","ExitCode":{code},"Publishers":[{pubs}]}}"#)
    }

    /// Spec 130 T02: o `ps --all` diz se o `workspace` saiu, com o código e o nome do container.
    #[test]
    fn run_state_reads_workspace_exit_code_and_container() {
        let text = format!("{}\n{}\n", ps_workspace("exited", 2, None), r#"{"Service":"db","State":"running","Publishers":[]}"#);
        let state = parse_run_state(&text);
        assert!(state.workspace_exited());
        assert_eq!(state.exit_code(), Some(2));
        assert_eq!(state.container(), Some("nants-p1-run-workspace-1"));
        let live = parse_run_state(&ps_workspace("running", 0, Some(18080)));
        assert!(!live.workspace_exited());
        assert_eq!(live.ports.len(), 1);
        // Sem o `workspace` (container removido): conta como encerrado, sem código.
        let gone = parse_run_state(r#"[{"Service":"db","State":"running"}]"#);
        assert!(gone.workspace_exited());
        assert_eq!(gone.exit_code(), None);
    }

    /// Spec 130 T02: a observação classifica pelo comportamento: saiu, porta HTTP, vivo sem página.
    #[test]
    fn observe_run_classifies_finished_page_and_running() {
        let dir = project("observe", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        fake.reply("ps", 0, &ps_workspace("exited", 0, None));
        let env = env_with(&fake).with_kind(ComposeKind::Run);
        let seen = env.observe_run(&dir, Duration::ZERO, &[]).expect("observe");
        assert_eq!(seen.mode, RunMode::Finished);
        assert_eq!(seen.state.exit_code(), Some(0));
        assert_eq!(&fake.calls.lock().expect("lock")[0].args[7..], ["ps", "--all", "--format", "json"]);

        // Porta que responde HTTP: página.
        let http = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = http.local_addr().expect("addr").port();
        thread::spawn(move || {
            for mut s in http.incoming().flatten() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.0 200 OK\r\n\r\nok");
            }
        });
        let fake = Arc::new(FakeRunner::default());
        fake.reply("ps", 0, &ps_workspace("running", 0, Some(port)));
        let env = env_with(&fake).with_kind(ComposeKind::Run);
        assert_eq!(env.observe_run(&dir, Duration::from_secs(2), &[]).expect("observe").mode, RunMode::Page);
        // A porta do Nants nunca é sondada; janela zero também não sonda.
        assert_eq!(env.observe_run(&dir, Duration::from_millis(50), &[port]).expect("observe").mode, RunMode::Running);
        assert_eq!(env.observe_run(&dir, Duration::ZERO, &[]).expect("observe").mode, RunMode::Running);

        // Porta que responde outra coisa (não HTTP): segue "rodando" ao fim da janela.
        let other = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = other.local_addr().expect("addr").port();
        thread::spawn(move || {
            for mut s in other.incoming().flatten() {
                let _ = s.write_all(b"-ERR protocolo\r\n");
            }
        });
        let fake = Arc::new(FakeRunner::default());
        fake.reply("ps", 0, &ps_workspace("running", 0, Some(port)));
        let env = env_with(&fake).with_kind(ComposeKind::Run);
        assert_eq!(env.observe_run(&dir, Duration::from_millis(300), &[]).expect("observe").mode, RunMode::Running);
    }

    #[test]
    fn workspace_output_has_no_service_prefix() {
        let dir = project("output", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake).with_kind(ComposeKind::Run);
        env.workspace_output(&dir, 400).expect("logs");
        let calls = fake.calls.lock().expect("lock");
        assert_eq!(&calls[0].args[7..], ["logs", "--no-color", "--no-log-prefix", "--tail", "400", "workspace"]);
    }

    #[test]
    fn up_run_writes_command_into_override_and_workspace_logs_target_workspace() {
        let dir = project("uprun", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        let env = env_with(&fake).with_kind(ComposeKind::Run);
        env.up_run(&dir, "npm run dev -- --host 0.0.0.0").expect("up");
        let ov = {
            let calls = fake.calls.lock().expect("lock");
            assert_eq!(&calls[0].args[7..], ["up", "-d", "--build"]);
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

    /// Spec 134 T08 (finding F-01): no tempo esgotado, o executor volta logo, mesmo com um processo
    /// filho do cliente ainda segurando os pipes (como o plugin `compose` do `docker`), e esse
    /// grupo de processos morre junto; a saída que já chegou volta.
    #[test]
    fn timeout_returns_without_waiting_for_pipes_held_by_children() {
        let dir = std::env::temp_dir().join(format!("nants-compose-pipes-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let marker = format!("nants-pipe-holder-{}", std::process::id());
        let call = ComposeCall {
            // O filho em segundo plano herda stdout/stderr e vive 60 s; o `sh` dorme também.
            args: vec!["-c".into(), format!("echo antes; sh -c 'sleep 60; echo {marker}' & sleep 60")],
            cwd: dir.clone(),
            env: controlled_env(std::env::vars(), &dir),
            timeout: Duration::from_millis(300),
        };
        let started = Instant::now();
        let out = RealComposeRunner::with_program("sh").run(&call).expect("run");
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5), "esperou os pipes: {:?}", started.elapsed());
        assert!(out.output.contains("antes"), "{}", out.output);
        // O grupo morreu: nenhum `sleep 60` do teste sobrou.
        thread::sleep(Duration::from_millis(200));
        let ps = Command::new("ps").args(["-eo", "args"]).output().expect("ps");
        assert!(!String::from_utf8_lossy(&ps.stdout).contains(&marker), "filho do cliente seguiu vivo");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Sentinela (spec 113 T01): nos testes, o executor real recusa o `docker` antes de criar o
    /// processo; outros programas seguem normais.
    #[test]
    fn real_runner_refuses_real_docker_in_tests() {
        if std::env::var(TEST_REAL_DOCKER_VAR).is_ok_and(|v| v == "1") {
            return; // rodada real explícita (`#[ignore]`): a sentinela está desligada de propósito
        }
        let call = ComposeCall {
            args: ["compose", "-p", "nants-proj-test", "down", "-v"].map(String::from).to_vec(),
            cwd: std::env::temp_dir(),
            env: Vec::new(),
            timeout: Duration::from_secs(5),
        };
        for program in ["docker", "/usr/bin/docker"] {
            let err = RealComposeRunner::with_program(program).run(&call).expect_err("docker real recusado");
            assert!(err.contains("teste chamou o docker real"), "{err}");
        }
        assert!(RealComposeRunner::default().run(&call).is_err());
        assert_eq!(refuse_real_docker_in_tests("sh"), None);
    }

    /// Teste real: precisa de docker. `NANTS_TEST_REAL_DOCKER=1 cargo test -- --ignored real_compose_roundtrip`.
    /// Todos os `real_*` daqui pedem a variável (sentinela [`refuse_real_docker_in_tests`]).
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
    /// Teste real (spec 133 T12): imagem Alpine com `ENV PATH` próprio, construída pelo Compose. O
    /// `/etc/profile` do Alpine refaz o `PATH` no `sh -lc`; o `.profile` da pasta pessoal o devolve,
    /// e o programa do `PATH` da imagem é achado. A imagem construída leva o rótulo `nants.project`
    /// e sai na remoção do projeto (`down --rmi local`).
    /// `NANTS_TEST_REAL_DOCKER=1 cargo test -- --ignored real_login_shell_keeps_the_image_path`.
    #[test]
    #[ignore]
    fn real_login_shell_keeps_the_image_path() {
        let compose = "services:\n  workspace:\n    build:\n      context: .\n      dockerfile: Dockerfile\n    command: sleep infinity\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("realpath", compose);
        fs::write(
            dir.join(".nants/Dockerfile"),
            "FROM alpine:3\nRUN mkdir -p /opt/nants-sonda/bin && printf '#!/bin/sh\\necho sonda-ok\\n' > /opt/nants-sonda/bin/nants-sonda && chmod +x /opt/nants-sonda/bin/nants-sonda\nENV PATH=/opt/nants-sonda/bin:$PATH\n",
        )
        .expect("Dockerfile");
        let env = ComposeEnv::real(format!("itestpath{}", std::process::id()));
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let image = format!("{}-workspace", env.project_name());
        let result = env.prepare(&dir).and_then(|()| env.exec(&dir, "command -v nants-sonda && nants-sonda && echo \"$PATH\"", Duration::from_secs(120)));
        let label = Command::new("docker")
            .args(["image", "inspect", "--format", "{{index .Config.Labels \"nants.project\"}}", image.as_str()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let down = env.down_removing_images(&dir, true);
        let left = Command::new("docker").args(["image", "inspect", image.as_str()]).output().map(|o| o.status.success());
        let out = result.expect("docker");
        down.expect("down");
        assert!(out.success() && out.output.contains("/opt/nants-sonda/bin/nants-sonda") && out.output.contains("sonda-ok"), "{}", out.output);
        assert!(out.output.contains("/work/.nants-home/.local/bin:/opt/nants-sonda/bin:"), "{}", out.output);
        assert_eq!(label.expect("inspect").as_str(), format!("itestpath{}", std::process::id()));
        assert!(!left.expect("inspect"), "a imagem construída saiu com o down --rmi local");
        fs::remove_dir_all(&dir).expect("apagável");
    }

    /// Teste real: precisa de docker. O workspace roda com o uid do host e o reparo devolve a posse
    /// de arquivos criados como root. `NANTS_TEST_REAL_DOCKER=1 cargo test -- --ignored real_workspace_uid_and_repair`.
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

    /// Remove, no fim do teste real, tudo o que leva o rótulo do projeto de teste (também quando o
    /// teste falha no meio): containers, depois o projeto Compose e a pasta.
    struct RealCleanup<'a> {
        env: &'a ComposeEnv,
        dir: PathBuf,
        label: String,
    }

    impl Drop for RealCleanup<'_> {
        fn drop(&mut self) {
            let ids = Command::new("docker").args(["ps", "-aq", "--filter", &format!("label={}", self.label)]).output();
            if let Ok(o) = ids {
                for id in String::from_utf8_lossy(&o.stdout).split_whitespace() {
                    let _ = Command::new("docker").args(["rm", "-f", id]).output();
                }
            }
            let _ = self.env.down(&self.dir);
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// Teste real (spec 134 T08): os limites se aplicam a um container de verdade e o tempo limite
    /// encerra o container. NÃO roda sem pedido: `NANTS_TEST_REAL_DOCKER=1 scripts/test.sh cargo
    /// test --lib -- --ignored real_container_limits_stop_fork_and_memory`.
    ///
    /// Seguro MESMO SE A PROTEÇÃO FALHAR: os limites de teste são pequenos (256 MiB, 64 processos);
    /// o próprio script cria no máximo 200 processos (`sleep` curtos, sem recursão) e aloca no
    /// máximo 384 MiB (um `tail -c` de tamanho fixo), bem abaixo da RAM livre de uma máquina de
    /// 8 GB; tudo tem tempo limite curto, e a limpeza ([`RealCleanup`]) roda até no pânico.
    #[test]
    #[ignore]
    fn real_container_limits_stop_fork_and_memory() {
        const MIB: u64 = 1024 * 1024;
        let compose = "services:\n  workspace:\n    image: alpine:3\n    command: sleep infinity\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("reallimits", compose);
        let id = format!("itestlim{}", std::process::id());
        let cap = ContainerLimits { memory: 256 * MIB, pids: 64 };
        let env = ComposeEnv::real(id.clone()).with_limits(cap);
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let _cleanup = RealCleanup { env: &env, dir: dir.clone(), label: format!("nants.project={id}") };
        let wait = Duration::from_secs(90);

        // 1. Processos: 200 no máximo, de 3 s cada; com o limite de 64, o fork é recusado.
        let forks = env
            .exec(&dir, "i=0; while [ $i -lt 200 ]; do sleep 3 & i=$((i+1)); done; wait; echo fim", wait)
            .expect("docker");
        assert!(!forks.timed_out, "{}", forks.output);
        assert_eq!(crate::harness::limits::detect_hit(forks.code, forks.timed_out, &forks.output), Some(crate::harness::limits::LimitHit::Pids), "{forks:?}");

        // 2. Memória: 384 MiB num buffer de tamanho fixo; com o limite de 256 MiB, o 137.
        let mem = env.exec(&dir, "head -c 402653184 /dev/zero | tail -c 402653184 > /dev/null; echo \"saida=$?\"", wait).expect("docker");
        assert!(!mem.timed_out, "{}", mem.output);
        assert!(mem.output.contains("saida=137") || mem.code == Some(137), "{mem:?}");

        // 3. Tempo limite: o container é encerrado, não só o cliente.
        let started = Instant::now();
        let slow = env.exec(&dir, "sleep 600", Duration::from_secs(5)).expect("docker");
        assert!(slow.timed_out);
        assert!(started.elapsed() < Duration::from_secs(30), "exec demorou {:?}", started.elapsed());
        let left = Command::new("docker")
            .args(["ps", "-q", "--filter", &format!("label=nants.project={id}"), "--filter", "name=exec"])
            .output()
            .expect("docker ps");
        assert!(String::from_utf8_lossy(&left.stdout).trim().is_empty(), "container de comando ainda vivo");
    }

    /// Teste real (spec 137 T09): na rodada offline o `workspace` não sai para a internet, mas
    /// alcança o serviço irmão pelo nome; no fim, a rede interna não sobra e o irmão volta à rede de
    /// sempre. NÃO roda sem pedido: `NANTS_TEST_REAL_DOCKER=1 scripts/test.sh cargo test --lib --
    /// --ignored real_offline_round_cuts_internet_and_keeps_siblings`. A imagem (precisa de `bash` e
    /// `perl`, como a `ubuntu:24.04`) vem de `NANTS_TEST_OFFLINE_IMAGE`; nada é baixado nem
    /// construído além dela. Limites pequenos, tempo curto e a limpeza ([`RealCleanup`]) até no pânico.
    #[test]
    #[ignore]
    fn real_offline_round_cuts_internet_and_keeps_siblings() {
        const MIB: u64 = 1024 * 1024;
        let image = std::env::var("NANTS_TEST_OFFLINE_IMAGE").unwrap_or_else(|_| "ubuntu:24.04".into());
        // O irmão responde uma linha a cada conexão na porta 8080 (`$$`: escape do Compose).
        let server = "perl -MIO::Socket::INET -e '$$s=IO::Socket::INET->new(LocalPort=>8080,Listen=>5,ReuseAddr=>1) or die; while($$c=$$s->accept){print $$c \"irmao-ok\\n\"; close $$c}'";
        let compose = format!(
            "services:\n  workspace:\n    image: {image}\n    command: sleep infinity\n  db:\n    image: {image}\n    command: [\"sh\", \"-c\", {}]\n",
            serde_json::to_string(server).expect("json")
        );
        let dir = project("realoffline", &compose);
        fs::write(
            dir.join("sonda.sh"),
            "getent hosts db >/dev/null && echo dns=ok\n\
             for i in 1 2 3 4 5 6 7 8 9 10; do l=$(timeout 3 cat < /dev/tcp/db/8080 2>/dev/null) && { echo \"irmao=$l\"; break; }; sleep 1; done\n\
             if timeout 5 bash -c ': < /dev/tcp/1.1.1.1/80' 2>/dev/null; then echo internet=sim; else echo internet=nao; fi\n",
        )
        .expect("sonda");
        let id = format!("itestoff{}", std::process::id());
        let env = ComposeEnv::real(id.clone()).with_limits(ContainerLimits { memory: 256 * MIB, pids: 64 });
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let _cleanup = RealCleanup { env: &env, dir: dir.clone(), label: format!("nants.project={id}") };
        env.prepare(&dir).expect("prepare");
        let out = env.exec_offline(&dir, "bash /work/sonda.sh", Duration::from_secs(90), None).expect("docker");
        assert!(out.success(), "{}", out.output);
        assert!(out.output.contains("dns=ok") && out.output.contains("irmao=irmao-ok"), "{}", out.output);
        assert!(out.output.contains("internet=nao"), "{}", out.output);
        let net = Command::new("docker").args(["network", "inspect", env.offline_network_name().as_str()]).output().expect("inspect");
        assert!(!net.status.success(), "a rede interna sobrou");
        // O irmão segue de pé, na rede de sempre: a rodada normal seguinte o alcança.
        let after = env.exec(&dir, "bash -c 'timeout 5 cat < /dev/tcp/db/8080'", Duration::from_secs(60)).expect("docker");
        assert!(after.output.contains("irmao-ok"), "{}", after.output);
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

    /// Teste real do 053 (130 T02): o comando sai com 3 logo após subir; a observação classifica
    /// como "terminou", com o código e a saída.
    #[test]
    #[ignore]
    fn real_run_early_exit_is_detected() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("realexit", compose);
        let env = ComposeEnv::real(format!("itestexit{}", std::process::id())).with_kind(ComposeKind::Run);
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let result = env.up_run(&dir, "sleep 3; echo falhou-cedo; exit 3").and_then(|()| env.observe_run(&dir, Duration::from_secs(8), &[]));
        let logs = env.workspace_output(&dir, 50);
        let down = env.down_with(&dir, true);
        down.expect("down");
        let seen = result.expect("observação");
        assert_eq!(seen.mode, RunMode::Finished, "{seen:?}");
        assert_eq!(seen.state.exit_code(), Some(3));
        assert!(logs.expect("logs").contains("falhou-cedo"));
    }

    /// Teste real do 053 (130 T02): com porta publicada (docker-proxy aceita o connect), o app que
    /// morre depois do `up` ainda é "terminou", e não "página".
    #[test]
    #[ignore]
    fn real_run_early_exit_with_published_port_is_detected() {
        let compose = "services:\n  workspace:\n    image: alpine:3\n    ports:\n      - \"127.0.0.1::8080\"\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n";
        let dir = project("realexitport", compose);
        let env = ComposeEnv::real(format!("itestexitp{}", std::process::id())).with_kind(ComposeKind::Run);
        let env = env.with_approved_hashes(vec![env_hash(&dir).expect("hash")]);
        let result = env.up_run(&dir, "sleep 3; exit 3").and_then(|()| env.observe_run(&dir, Duration::from_secs(8), &[]));
        let down = env.down_with(&dir, true);
        down.expect("down");
        let seen = result.expect("observação");
        assert_eq!(seen.mode, RunMode::Finished, "{seen:?}");
        assert_eq!(seen.state.exit_code(), Some(3));
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
    /// publicada (sem healthcheck).
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

    /// Diff do Test 8 (pip com `HOME=/tmp`, resumido): diretórios `C`, entradas `A` e uma `D`.
    const DIFF_TEST8: &str = "C /tmp\nA /tmp/.cache\nA /tmp/.cache/pip\nA /tmp/.cache/pip/x.body\nA /tmp/.local\nA /tmp/.local/lib\nA /tmp/.local/lib/python3.12\nA /tmp/.local/lib/python3.12/site-packages\nA /tmp/.local/lib/python3.12/site-packages/pytest.py\nA /tmp/.local/bin\nA /tmp/.local/bin/pytest\nD /tmp/apagado\nA /work/fica.txt\nlixo sem formato\n";

    #[test]
    fn docker_diff_is_parsed_and_only_live_entries_outside_work_count() {
        let entries = parse_docker_diff(DIFF_TEST8);
        assert_eq!(entries.len(), 13, "a linha fora do formato some");
        assert_eq!(entries[0], DiffEntry { kind: DiffKind::Changed, path: "/tmp".into() });
        let outside = written_outside_work(&entries);
        assert_eq!(outside.len(), 11, "sem a D e sem o que está em /work: {outside:?}");
        assert!(!outside.iter().any(|p| p == "/tmp/apagado" || p.starts_with("/work")));
        let main = main_paths(&outside, 3, 5);
        assert_eq!(main[0], ("/tmp/.local/lib".to_string(), 4));
        assert!(main.contains(&("/tmp/.cache/pip".to_string(), 2)) && main.contains(&("/tmp/.local/bin".to_string(), 2)), "{main:?}");
        assert!(!main.iter().any(|(k, _)| k == "/tmp" || k == "/tmp/.local" || k == "/tmp/.cache"), "ancestral não entra: {main:?}");
        assert_eq!(main_paths(&outside, 3, 1).len(), 1);
        assert!(written_outside_work(&parse_docker_diff("")).is_empty());
    }

    #[test]
    fn run_kept_runs_without_rm_lists_the_diff_and_removes_the_container() {
        let dir = project("kept", COMPOSE);
        let fake = Arc::new(FakeRunner::default());
        fake.reply("run", 0, "instalado");
        fake.reply("diff", 0, DIFF_TEST8);
        let env = env_with(&fake);
        let kept = env.run_kept(&dir, "make deps", Duration::from_secs(5), None).expect("run_kept");
        assert!(kept.output.success() && kept.cleanup_error.is_none());
        assert_eq!(kept.diff.as_ref().map(Vec::len), Ok(13));
        assert_eq!(fake.subcommands(), vec!["run", "diff", "rm"]);
        let calls = fake.calls.lock().expect("lock");
        let run = &calls[0].args[7..];
        assert!(!run.iter().any(|a| a == "--rm"), "{run:?}");
        assert_eq!(&run[..2], ["run", "--name"]);
        assert_eq!(run[2], kept.container);
        assert!(kept.container.starts_with("nants-p1-test-kept-"), "{}", kept.container);
        assert_eq!(&run[3..], ["--label", "nants.project=p1", "-T", "--entrypoint", "sh", "workspace", "-lc", "make deps"]);
        assert_eq!(calls[1].args, ["diff", kept.container.as_str()]);
        assert_eq!(calls[2].args, ["rm", "-f", kept.container.as_str()]);
    }

    #[test]
    fn run_kept_removes_the_container_even_when_the_command_or_docker_fails() {
        let dir = project("keptfail", COMPOSE);
        // O comando falha: o diff e a remoção acontecem igual.
        let fake = Arc::new(FakeRunner::default());
        fake.reply("run", 1, "erro da instalação");
        let env = env_with(&fake);
        let kept = env.run_kept(&dir, "make deps", Duration::from_secs(5), None).expect("run_kept");
        assert_eq!(kept.output.code, Some(1));
        assert_eq!(fake.subcommands(), vec!["run", "diff", "rm"]);
        // O diff falha: o container é removido, o resultado do comando fica e o erro vai no diff.
        let fake = Arc::new(FakeRunner::default());
        fake.reply("run", 0, "instalado");
        fake.reply("diff", 1, "No such container");
        let kept = env_with(&fake).run_kept(&dir, "make deps", Duration::from_secs(5), None).expect("run_kept");
        assert!(kept.output.success() && kept.diff.is_err_and(|e| e.contains("No such container")));
        assert_eq!(fake.subcommands(), vec!["run", "diff", "rm"]);
        // O docker não sobe o comando (erro do executor): a remoção ainda é tentada.
        struct FailRun(Mutex<Vec<String>>);
        impl ComposeRunner for FailRun {
            fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
                let sub = super::fake::sub_of(&call.args).to_string();
                self.0.lock().unwrap_or_else(PoisonError::into_inner).push(sub.clone());
                if sub == "run" {
                    return Err("docker ausente".into());
                }
                Ok(ComposeOutput { code: Some(0), output: String::new(), timed_out: false })
            }
        }
        let failing = Arc::new(FailRun(Mutex::new(Vec::new())));
        let env = super::fake::test_env("p1", failing.clone());
        assert!(env.run_kept(&dir, "make deps", Duration::from_secs(5), None).is_err());
        assert_eq!(*failing.0.lock().expect("lock"), vec!["run".to_string(), "rm".to_string()]);
        // A remoção falha: o resultado volta com o motivo, para o registro.
        let fake = Arc::new(FakeRunner::default());
        fake.reply("rm", 1, "device busy");
        let kept = env_with(&fake).run_kept(&dir, "make deps", Duration::from_secs(5), None).expect("run_kept");
        assert!(kept.cleanup_error.is_some_and(|e| e.contains("device busy")));
        let _ = fs::remove_dir_all(&dir);
    }
}
