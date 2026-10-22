//! Testes de navegador dos agentes (spec 036): `run_browser_tests` roda o comando num container
//! Playwright efêmero (`docker run --rm`), com só a worktree montada em /work. Argv fixo (sem
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

/// Argv do `docker run` (sem o programa `docker`).
pub fn run_args(name: &str, worktree: &Path, version: &str, command: &str, user: Option<(u32, u32)>) -> Vec<String> {
    let mut a: Vec<String> = ["run", "--rm", "--name"].map(String::from).to_vec();
    a.push(name.into());
    a.extend(["--init", "--ipc=host"].map(String::from));
    if let Some((uid, gid)) = user {
        a.extend(["--user".into(), format!("{uid}:{gid}")]);
    }
    a.extend(["-v".into(), format!("{}:/work", worktree.to_string_lossy()), "-w".into(), "/work".into()]);
    a.extend(["-e", "CI=1", "-e", "HOME=/tmp"].map(String::from));
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
        }
    }

    pub fn real(project_id: impl Into<String>) -> Self {
        Self::new(project_id, Arc::new(RealComposeRunner::default()))
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
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
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let name = format!("{}{n}", container_prefix(&self.project_id));
        let me = fs::metadata("/proc/self").ok().map(|m| (m.uid(), m.gid()));
        let args = run_args(&name, worktree, &version, command, me);
        self.active.lock().unwrap_or_else(PoisonError::into_inner).insert(name.clone());
        let out: ComposeOutput = match self.runner.run(&self.docker_call(args, worktree, self.timeout)) {
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
        let fake = Arc::new(Fake::default());
        let b = BrowserTests::new("p1", fake.clone());
        let text = b.run(&d, "npx playwright test").expect("run");
        assert!(text.starts_with("exit: 0"));
        let calls = fake.calls.lock().unwrap_or_else(PoisonError::into_inner);
        let run = calls.iter().find(|c| c.args[0] == "run").expect("run");
        let a = &run.args;
        assert_eq!(&a[..4], ["run", "--rm", "--name", "nants-p1-browser-1"]);
        assert!(a.contains(&"--init".to_string()) && a.contains(&"--ipc=host".to_string()));
        let mounts: Vec<_> = a.windows(2).filter(|w| w[0] == "-v").collect();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0][1], format!("{}:/work", d.display()));
        assert!(a.contains(&"mcr.microsoft.com/playwright:v1.49.1-noble".to_string()));
        assert_eq!(&a[a.len() - 3..], ["sh", "-c", "npx playwright test"]);
        assert!(run.env.iter().all(|(k, _)| k == "PATH" || k == "HOME" || k.starts_with("DOCKER_") || k == "NANTS_WORKTREE"));
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
