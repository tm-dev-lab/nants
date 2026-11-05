//! Testes de navegador dos agentes (spec 036): `run_browser_tests` roda o comando num container
//! Playwright efêmero (`docker run --rm`), com só a worktree montada em /work (`.git` e `.nants` só
//! leitura, spec 141 T01) e a pasta pessoal em `/nants/home`. Argv fixo (sem
//! shell no host), ambiente do host limpo, timeout próprio e `docker rm -f` no estouro e no
//! encerramento. A imagem é escolhida pela versão exata de `@playwright/test` da worktree.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::compose::{controlled_env, is_docker_failure, ComposeCall, ComposeOutput, ComposeRunner, RealComposeRunner};
use super::home;
use super::limits::ContainerLimits;
use super::progress::CommandProgress;

/// Timeout padrão de uma execução (a primeira pode baixar a imagem).
pub const BROWSER_TIMEOUT: Duration = Duration::from_secs(600);
const RM_TIMEOUT: Duration = Duration::from_secs(60);
/// Timeout padrão do download da imagem (etapa separada da execução).
pub const PULL_TIMEOUT: Duration = Duration::from_secs(1200);
const INSPECT_TIMEOUT: Duration = Duration::from_secs(60);

/// Resposta quando não há Docker utilizável.
pub const UNAVAILABLE: &str =
    "indisponível: Docker não está acessível neste ambiente; não é possível rodar testes de navegador. Marque o critério como (manual)";

/// Prefixo dos containers do projeto (usado também pelo sweep).
pub fn container_prefix(project_id: &str) -> String {
    format!("nants-{project_id}-browser-")
}

/// Versão exata de `@playwright/test` no package.json da worktree.
pub fn playwright_version(worktree: &Path) -> Result<String, String> {
    let text = fs::read_to_string(worktree.join("package.json")).map_err(|_| {
        "package.json não encontrado na raiz: crie-o com `@playwright/test` em versão exata (ex.: \"1.49.1\")".to_string()
    })?;
    let doc: serde_json::Value = serde_json::from_str(&text).map_err(|_| "package.json inválido".to_string())?;
    let version = ["devDependencies", "dependencies"]
        .iter()
        .find_map(|k| doc.get(k).and_then(|d| d.get("@playwright/test")).and_then(|v| v.as_str()))
        .ok_or("package.json não declara `@playwright/test`: adicione-o em devDependencies com versão exata")?;
    let exact = !version.is_empty()
        && !version.starts_with('.')
        && !version.ends_with('.')
        && !version.contains("..")
        && version.chars().all(|c| c.is_ascii_digit() || c == '.');
    if !exact {
        return Err(format!(
            "`@playwright/test` precisa de versão exata (só dígitos e pontos, ex.: \"1.49.1\"); encontrado \"{version}\". Fixe a versão sem ^ ou ~"
        ));
    }
    Ok(version.to_string())
}

fn image_name(version: &str) -> String {
    format!("mcr.microsoft.com/playwright:v{version}-noble")
}

/// Argv do `docker run` (sem o programa `docker`). `host_home` é a pasta pessoal (no host) da
/// área de trabalho, montada em [`home::CONTAINER_HOME`].
pub fn run_args(name: &str, worktree: &Path, host_home: &Path, version: &str, command: &str, user: Option<(u32, u32)>) -> Vec<String> {
    let mut a: Vec<String> = ["run", "--rm", "--name"].map(String::from).to_vec();
    a.push(name.into());
    a.extend(["--init", "--ipc=host"].map(String::from));
    // Spec 134 T08: os testes de navegador rodam código do projeto; mesmos limites dos containers
    // do Compose (memória sem swap extra e processos).
    a.extend(ContainerLimits::host().docker_run_args());
    if let Some((uid, gid)) = user {
        a.extend(["--user".into(), format!("{uid}:{gid}")]);
    }
    a.extend(["-v".into(), format!("{}:/work", worktree.to_string_lossy())]);
    // Spec 141 T01: o mesmo isolamento estrutural do Compose (spec 062): `.git` (arquivo ou
    // diretório) e `.nants` só leitura. O teste do Playwright é código escrito pelo agente. Só
    // arquivo ou diretório de verdade: o Docker segue um link da origem no host e montaria o alvo.
    for prot in [".git", ".nants"] {
        let p = worktree.join(prot);
        if p.symlink_metadata().is_ok_and(|m| m.is_file() || m.is_dir()) {
            a.extend(["-v".into(), format!("{}:/work/{prot}:ro", p.to_string_lossy())]);
        }
    }
    // Spec 141 T02 (C-02): na raiz, as worktrees das tarefas ficam fora da vista, como no Compose.
    if let Some(hidden) = home::hidden_from_root(worktree) {
        a.extend(["--mount".into(), format!("type=tmpfs,destination=/work/{hidden},tmpfs-size=1048576")]);
    }
    // Spec 129 T01: a mesma pasta pessoal persistente dos comandos do workspace; spec 141 T01: fora
    // de `/work`, por bind da pasta do diretório de dados do Nants.
    a.extend(["-v".into(), format!("{}:{}", host_home.to_string_lossy(), home::CONTAINER_HOME), "-w".into(), "/work".into()]);
    a.extend(["-e".into(), "CI=1".into(), "-e".into(), format!("HOME={}", home::CONTAINER_HOME)]);
    a.push(image_name(version));
    a.extend(["sh".into(), "-c".into(), command.into()]);
    a
}

/// Executor de testes de navegador de um projeto.
pub struct BrowserTests {
    project_id: String,
    runner: Arc<dyn ComposeRunner>,
    counter: AtomicUsize,
    active: Mutex<BTreeSet<String>>,
    timeout: Duration,
    pull_timeout: Duration,
    /// Spec 141 T01: diretório das pastas pessoais (`<dados>/homes`).
    homes_dir: Option<std::path::PathBuf>,
}

impl std::fmt::Debug for BrowserTests {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserTests").field("project_id", &self.project_id).finish()
    }
}

impl BrowserTests {
    pub fn new(project_id: impl Into<String>, runner: Arc<dyn ComposeRunner>) -> Self {
        Self {
            project_id: project_id.into(),
            runner,
            counter: AtomicUsize::new(0),
            active: Mutex::new(BTreeSet::new()),
            timeout: BROWSER_TIMEOUT,
            pull_timeout: PULL_TIMEOUT,
            homes_dir: home::homes_base(),
        }
    }

    /// Diretório das pastas pessoais (testes; o padrão é `<dados>/homes`).
    pub fn with_homes_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.homes_dir = Some(dir);
        self
    }

    pub fn real(project_id: impl Into<String>) -> Self {
        Self::new(project_id, Arc::new(RealComposeRunner::default()))
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Limite de tempo de cada execução dos testes (sem o download da imagem, que tem o próprio).
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn with_pull_timeout(mut self, timeout: Duration) -> Self {
        self.pull_timeout = timeout;
        self
    }

    fn docker_call(&self, args: Vec<String>, cwd: &Path, timeout: Duration) -> ComposeCall {
        ComposeCall { args, cwd: cwd.to_path_buf(), env: controlled_env(std::env::vars(), cwd), timeout }
    }

    fn remove(&self, name: &str, cwd: &Path) {
        let call = self.docker_call(vec!["rm".into(), "-f".into(), name.into()], cwd, RM_TIMEOUT);
        let _ = self.runner.run(&call);
        self.active.lock().unwrap_or_else(PoisonError::into_inner).remove(name);
    }

    /// Remove os containers ainda ativos (encerramento da execução/servidor). Idempotente.
    pub fn remove_all(&self, cwd: &Path) {
        let names: Vec<String> = self.active.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect();
        for n in names {
            self.remove(&n, cwd);
        }
    }

    /// Roda `command` no container. Bloqueante: chame em `spawn_blocking`. `Ok(texto)` sempre
    /// que o resultado vai ao modelo (inclusive "indisponível"); `Err` = erro de uso.
    pub fn run(&self, worktree: &Path, command: &str) -> Result<String, String> {
        self.run_watched(worktree, command, None)
    }

    /// Como [`Self::run`], com o progresso da saída dos testes em `progress` (spec 123 T01). O
    /// download da imagem não entra no progresso: só a execução do comando.
    pub fn run_watched(&self, worktree: &Path, command: &str, progress: Option<&Arc<CommandProgress>>) -> Result<String, String> {
        if command.trim().is_empty() {
            return Err("comando vazio".into());
        }
        let version = playwright_version(worktree)?;
        let image = image_name(&version);
        let inspect = self.docker_call(vec!["image".into(), "inspect".into(), image.clone()], worktree, INSPECT_TIMEOUT);
        match self.runner.run(&inspect) {
            Err(_) => return Ok(UNAVAILABLE.into()),
            Ok(o) if o.success() => {}
            Ok(o) if is_docker_failure(&o.output) => return Ok(UNAVAILABLE.into()),
            Ok(_) => {
                // Imagem ausente: baixa em etapa própria (timeout maior), fora do timeout dos testes.
                let pull = self.docker_call(vec!["pull".into(), image.clone()], worktree, self.pull_timeout);
                match self.runner.run(&pull) {
                    Err(_) => return Ok(UNAVAILABLE.into()),
                    Ok(o) if o.timed_out => {
                        return Ok(format!(
                            "o download da imagem {image} não terminou em {} s; nenhum teste foi executado. Tente novamente mais tarde ou marque o critério como (manual)",
                            self.pull_timeout.as_secs()
                        ))
                    }
                    Ok(o) if !o.success() => {
                        if is_docker_failure(&o.output) {
                            return Ok(UNAVAILABLE.into());
                        }
                        let tail: String = o.output.trim_end().chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
                        return Ok(format!("falha ao baixar a imagem {image}; nenhum teste foi executado: {tail}"));
                    }
                    Ok(_) => {}
                }
            }
        }
        // Spec 141 T01: a mesma pasta pessoal do Compose para esta área de trabalho (com a pasta
        // antiga migrada, se houver).
        let homes = self.homes_dir.as_deref().ok_or("pasta pessoal indisponível: diretório de dados do Nants indisponível (HOME)")?;
        let host_home = home::ready(homes, &self.project_id, worktree).map_err(|e| format!("pasta pessoal indisponível: {e}"))?;
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let name = format!("{}{n}", container_prefix(&self.project_id));
        let me = fs::metadata("/proc/self").ok().map(|m| (m.uid(), m.gid()));
        let args = run_args(&name, worktree, &host_home, &version, command, me);
        self.active.lock().unwrap_or_else(PoisonError::into_inner).insert(name.clone());
        // Spec 123 T02: imagem pronta (o download tem limite próprio); o relógio dos testes começa.
        if let Some(p) = progress {
            p.mark_started();
        }
        let out: ComposeOutput = match self.runner.run_watched(&self.docker_call(args, worktree, self.timeout), progress) {
            Ok(o) => o,
            Err(_) => {
                self.remove(&name, worktree);
                return Ok(UNAVAILABLE.into());
            }
        };
        if out.timed_out {
            self.remove(&name, worktree);
            return Ok(format!(
                "o comando não terminou em {} s e o container foi removido. Não repita o mesmo comando: verifique servidores/watchers pendurados e rode só um subconjunto",
                self.timeout.as_secs()
            ));
        }
        self.active.lock().unwrap_or_else(PoisonError::into_inner).remove(&name);
        if !out.success() && is_docker_failure(&out.output) {
            return Ok(UNAVAILABLE.into());
        }
        let code = out.code.map_or_else(|| "sinal".to_string(), |c| c.to_string());
        Ok(format!("exit: {code}\n{}", out.output))
    }
}

impl Drop for BrowserTests {
    fn drop(&mut self) {
        self.remove_all(&std::env::temp_dir());
    }
}

/// Ids de containers `nants-<id>-browser-*` (linhas `ID\tNome`) de projetos em `projects`.
pub fn browser_container_ids(ps: &str, projects: &[String]) -> Vec<String> {
    ps.lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(id, name)| {
            !id.trim().is_empty() && projects.iter().any(|p| name.trim().starts_with(&container_prefix(p)))
        })
        .map(|(id, _)| id.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<ComposeCall>>,
        out: Mutex<Option<Result<ComposeOutput, String>>>,
        pull: Mutex<Option<Result<ComposeOutput, String>>>,
        missing: std::sync::atomic::AtomicBool,
    }
    impl ComposeRunner for Fake {
        fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
            self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call.clone());
            match call.args.first().map(String::as_str) {
                Some("rm") => return Ok(ComposeOutput { code: Some(0), output: String::new(), timed_out: false }),
                Some("image") => {
                    let missing = self.missing.load(Ordering::SeqCst);
                    return Ok(ComposeOutput { code: Some(i32::from(missing)), output: String::new(), timed_out: false });
                }
                Some("pull") => {
                    return self.pull.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or(Ok(ComposeOutput {
                        code: Some(0),
                        output: String::new(),
                        timed_out: false,
                    }))
                }
                _ => {}
            }
            self.out
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .unwrap_or(Ok(ComposeOutput { code: Some(0), output: "ok".into(), timed_out: false }))
        }
    }

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn tree(version: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nants-browser-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("tmp");
        fs::write(d.join("package.json"), format!(r#"{{"devDependencies":{{"@playwright/test":"{version}"}}}}"#)).expect("w");
        d
    }

    /// Spec 123 T02: o relógio do comando (largada do progresso) só começa no `docker run` dos
    /// testes; a conferência e o download da imagem, com limite próprio, ficam de fora.
    #[test]
    fn progress_starts_only_at_the_tests_run_not_during_the_image_pull() {
        struct Probe(Arc<CommandProgress>, Mutex<Vec<(String, bool)>>);
        impl ComposeRunner for Probe {
            fn run(&self, call: &ComposeCall) -> Result<ComposeOutput, String> {
                self.1.lock().unwrap_or_else(PoisonError::into_inner).push((call.args[..2].join(" "), self.0.has_started()));
                // Imagem ausente: a conferência falha e o download acontece.
                let code = if call.args[..2] == ["image", "inspect"] { 1 } else { 0 };
                Ok(ComposeOutput { code: Some(code), output: String::new(), timed_out: false })
            }
        }
        let d = tree("1.49.1");
        let progress = Arc::new(CommandProgress::gated());
        let probe = Arc::new(Probe(progress.clone(), Mutex::default()));
        let text = BrowserTests::new("p1", probe.clone()).run_watched(&d, "npx playwright test", Some(&progress)).expect("run");
        assert!(text.starts_with("exit: 0"), "{text}");
        let seen = probe.1.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let started = |prefix: &str| seen.iter().find(|(a, _)| a.starts_with(prefix)).map(|(_, s)| *s);
        assert_eq!((started("image inspect"), started("pull"), started("run --rm")), (Some(false), Some(false), Some(true)), "{seen:?}");
    }

    #[test]
    fn version_must_be_exact() {
        assert_eq!(playwright_version(&tree("1.49.1")).as_deref(), Ok("1.49.1"));
        for bad in ["^1.49.1", "~1.49", "latest", "1.49.1; rm", "", "1..2", ".1"] {
            assert!(playwright_version(&tree(bad)).is_err(), "{bad}");
        }
        let d = tree("1.0.0");
        fs::write(d.join("package.json"), "{}").expect("w");
        assert!(playwright_version(&d).unwrap_err().contains("@playwright/test"));
        fs::remove_file(d.join("package.json")).expect("rm");
        assert!(playwright_version(&d).unwrap_err().contains("package.json"));
    }

    #[test]
    fn argv_is_fixed_and_mounts_only_worktree() {
        let d = tree("1.49.1");
        let homes = d.join("dados-homes");
        let fake = Arc::new(Fake::default());
        let b = BrowserTests::new("p1", fake.clone()).with_homes_dir(homes.clone());
        let text = b.run(&d, "npx playwright test").expect("run");
        assert!(text.starts_with("exit: 0"));
        let calls = fake.calls.lock().unwrap_or_else(PoisonError::into_inner);
        let run = calls.iter().find(|c| c.args[0] == "run").expect("run");
        let a = &run.args;
        assert_eq!(&a[..4], ["run", "--rm", "--name", "nants-p1-browser-1"]);
        assert!(a.contains(&"--init".to_string()) && a.contains(&"--ipc=host".to_string()));
        // Spec 134 T08: limites de memória e de processos também no container do navegador.
        for limit in ContainerLimits::host().docker_run_args() {
            assert!(a.contains(&limit), "{limit} em {a:?}");
        }
        let mounts: Vec<_> = a.windows(2).filter(|w| w[0] == "-v").collect();
        // Sem `.git` nem `.nants`: a worktree e a pasta pessoal (fora de `/work`).
        let home = homes.join("p1/root/home");
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0][1], format!("{}:/work", d.display()));
        assert_eq!(mounts[1][1], format!("{}:/nants/home", home.display()));
        assert!(a.contains(&"mcr.microsoft.com/playwright:v1.49.1-noble".to_string()));
        assert_eq!(&a[a.len() - 3..], ["sh", "-c", "npx playwright test"]);
        assert!(a.contains(&"HOME=/nants/home".to_string()));
        assert!(home.join(".profile").exists() && !d.join(home::LEGACY_HOME_DIR).exists());
        assert!(run.env.iter().all(|(k, _)| k == "PATH" || k == "HOME" || k.starts_with("DOCKER_") || k == "NANTS_WORKTREE"));
    }

    /// Spec 141 T01: o container do navegador monta `.git` e `.nants` só leitura, como o Compose
    /// (spec 062), e migra a pasta pessoal antiga da área de trabalho.
    #[test]
    fn git_and_nants_are_mounted_read_only_and_the_legacy_home_is_migrated() {
        let d = tree("1.49.1");
        fs::write(d.join(".git"), "gitdir: /x\n").expect("git");
        fs::create_dir_all(d.join(".nants")).expect("nants");
        fs::create_dir_all(d.join(home::LEGACY_HOME_DIR).join(".cache")).expect("legado");
        let homes = d.join("dados-homes");
        let fake = Arc::new(Fake::default());
        BrowserTests::new("p1", fake.clone()).with_homes_dir(homes.clone()).run(&d, "x").expect("run");
        let calls = fake.calls.lock().unwrap_or_else(PoisonError::into_inner);
        let a = &calls.iter().find(|c| c.args[0] == "run").expect("run").args;
        let mounts: Vec<String> = a.windows(2).filter(|w| w[0] == "-v").map(|w| w[1].clone()).collect();
        assert_eq!(
            mounts,
            [
                format!("{}:/work", d.display()),
                format!("{}/.git:/work/.git:ro", d.display()),
                format!("{}/.nants:/work/.nants:ro", d.display()),
                format!("{}:/nants/home", homes.join("p1/root/home").display()),
            ]
        );
        assert!(!d.join(home::LEGACY_HOME_DIR).exists());
        assert!(homes.join("p1/root/home/.cache").is_dir());
    }

    /// Spec 141 T01 (revisão 1): `.git` ou `.nants` que é link não vira montagem (o Docker seguiria
    /// o link no host e montaria o alvo).
    #[test]
    fn linked_git_or_nants_is_not_mounted() {
        let d = tree("1.49.1");
        let target = tree("1.49.1");
        std::os::unix::fs::symlink(&target, d.join(".git")).expect("ln");
        std::os::unix::fs::symlink(&target, d.join(".nants")).expect("ln");
        let a = run_args("n", &d, Path::new("/dados/homes/p1/root/home"), "1.49.1", "x", None);
        let mounts: Vec<String> = a.windows(2).filter(|w| w[0] == "-v").map(|w| w[1].clone()).collect();
        assert_eq!(mounts, [format!("{}:/work", d.display()), "/dados/homes/p1/root/home:/nants/home".to_string()]);
    }

    /// Spec 141 T02: na raiz com worktrees, uma pasta vazia por cima delas; na worktree, nada.
    #[test]
    fn root_hides_task_worktrees_and_the_worktree_does_not() {
        let d = tree("1.49.1");
        let wt = d.join(".sdd/worktrees/s1/T01");
        fs::create_dir_all(&wt).expect("wt");
        let tmpfs = |dir: &Path| -> Vec<String> {
            let a = run_args("n", dir, Path::new("/dados/homes/p1/root/home"), "1.49.1", "x", None);
            a.windows(2).filter(|w| w[0] == "--mount").map(|w| w[1].clone()).collect()
        };
        assert_eq!(tmpfs(&d), ["type=tmpfs,destination=/work/.sdd/worktrees,tmpfs-size=1048576"]);
        assert!(tmpfs(&wt).is_empty());
    }

    #[test]
    fn timeout_removes_container() {
        let d = tree("1.49.1");
        let fake = Arc::new(Fake::default());
        *fake.out.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Ok(ComposeOutput { code: None, output: String::new(), timed_out: true }));
        let b = BrowserTests::new("p1", fake.clone()).with_timeout(Duration::from_secs(3));
        let text = b.run(&d, "x").expect("run");
        assert!(text.contains("não terminou em 3 s"));
        let calls = fake.calls.lock().unwrap_or_else(PoisonError::into_inner);
        let run = calls.iter().find(|c| c.args[0] == "run").expect("run");
        assert_eq!(run.timeout, Duration::from_secs(3));
        assert_eq!(calls.last().expect("last").args, ["rm", "-f", "nants-p1-browser-1"]);
    }

    fn kinds(fake: &Fake) -> Vec<String> {
        fake.calls.lock().unwrap_or_else(PoisonError::into_inner).iter().map(|c| c.args[0].clone()).collect()
    }

    #[test]
    fn present_image_skips_pull() {
        let d = tree("1.49.1");
        let fake = Arc::new(Fake::default());
        BrowserTests::new("p1", fake.clone()).run(&d, "x").expect("run");
        assert_eq!(kinds(&fake), ["image", "run"]);
    }

    #[test]
    fn missing_image_pulls_then_runs() {
        let d = tree("1.49.1");
        let fake = Arc::new(Fake::default());
        fake.missing.store(true, Ordering::SeqCst);
        let text = BrowserTests::new("p1", fake.clone()).run(&d, "x").expect("run");
        assert!(text.starts_with("exit: 0"));
        assert_eq!(kinds(&fake), ["image", "pull", "run"]);
    }

    #[test]
    fn pull_timeout_is_clear_and_runs_nothing() {
        let d = tree("1.49.1");
        let fake = Arc::new(Fake::default());
        fake.missing.store(true, Ordering::SeqCst);
        *fake.pull.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Ok(ComposeOutput { code: None, output: String::new(), timed_out: true }));
        let b = BrowserTests::new("p1", fake.clone()).with_pull_timeout(Duration::from_secs(7));
        let text = b.run(&d, "x").expect("run");
        assert!(text.contains("download da imagem") && text.contains("7 s"), "{text}");
        assert_eq!(kinds(&fake), ["image", "pull"]);
        assert_eq!(fake.calls.lock().unwrap_or_else(PoisonError::into_inner)[1].timeout, Duration::from_secs(7));
    }

    #[test]
    fn drop_removes_active_containers() {
        let fake = Arc::new(Fake::default());
        {
            let b = BrowserTests::new("p1", fake.clone());
            b.active.lock().unwrap_or_else(PoisonError::into_inner).insert("nants-p1-browser-9".into());
        }
        let calls = fake.calls.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(calls[0].args, ["rm", "-f", "nants-p1-browser-9"]);
    }

    #[test]
    fn without_docker_reports_unavailable() {
        let d = tree("1.49.1");
        for out in [
            Err("não foi possível iniciar o docker".to_string()),
            Ok(ComposeOutput { code: Some(1), output: "Cannot connect to the Docker daemon".into(), timed_out: false }),
        ] {
            let fake = Arc::new(Fake::default());
            *fake.out.lock().unwrap_or_else(PoisonError::into_inner) = Some(out);
            let text = BrowserTests::new("p1", fake).run(&d, "x").expect("run");
            assert!(text.starts_with("indisponível") && text.contains("(manual)"), "{text}");
        }
    }

    #[test]
    fn sweep_filter_matches_only_registered_browser_containers() {
        let ps = "a1\tnants-p1-browser-1\nb2\tnants-p2-browser-3\nc3\tnants-p1-test-workspace-1\nd4\tnants-p10-browser-1\n";
        assert_eq!(browser_container_ids(ps, &["p1".into()]), ["a1"]);
        assert_eq!(browser_container_ids(ps, &["p1".into(), "p2".into()]), ["a1", "b2"]);
    }

    /// Real: precisa de Docker e baixa a imagem do Playwright (grande, vários minutos na primeira
    /// vez). Rode com `NANTS_TEST_REAL_DOCKER=1 cargo test -- --ignored real_playwright_version`.
    #[test]
    #[ignore]
    fn real_playwright_version() {
        let d = tree("1.49.1");
        let b = BrowserTests::real("realtest");
        let text = b.run(&d, "npx playwright --version").expect("run");
        // Sem `npm install`, o npx baixa a versão corrente: aqui só se prova imagem + execução no container.
        assert!(text.contains("exit: 0") && text.contains("Version "), "{text}");
    }
}
