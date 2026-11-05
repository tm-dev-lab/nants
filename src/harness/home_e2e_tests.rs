//! Spec 141 T01: e2e com Docker de verdade (ignorado por padrão; precisa do Docker e baixa a imagem
//! do Playwright, grande, se ela não estiver na máquina). Rode só ele:
//! `NANTS_TEST_REAL_DOCKER=1 scripts/test.sh cargo test --lib harness::home::e2e -- --ignored`.
//! A spec 141 T02 acrescenta aqui a bateria da raiz com uma worktree suja presente
//! (`root_battery_does_not_see_task_worktrees`, com a imagem `node:20-slim`).
//!
//! O cenário do defeito: o Chromium do Playwright deixa na pasta pessoal um link quebrado do
//! PulseAudio, e um `node --test` sem caminho, que varre a árvore, saía com erro antes de rodar teste
//! quando a pasta pessoal ficava dentro dela. Aqui o Playwright roda de verdade, pelo mesmo caminho
//! do Nants (`BrowserTests`, a mesma imagem), e depois o `node --test` sem caminho roda no workspace
//! do Compose (`ComposeEnv`, com `prepare`), na mesma área de trabalho. Os limites de memória e de
//! processos dos containers são os do Nants (spec 134 T08). No fim saem os containers, a rede e,
//! se o teste a baixou, a imagem; nada de outros projetos é tocado.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::harness::browser::BrowserTests;
use crate::harness::compose::ComposeEnv;
use crate::harness::home::{self, CONTAINER_HOME, LEGACY_HOME_DIR};
use crate::harness::limits::ContainerLimits;
use crate::sdd::environment::env_hash;

const PLAYWRIGHT: &str = "1.49.1";

fn image() -> String {
    format!("mcr.microsoft.com/playwright:v{PLAYWRIGHT}-noble")
}

fn docker(args: &[&str]) -> (bool, String) {
    match Command::new("docker").args(args).output() {
        Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (false, e.to_string()),
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=Nants e2e", "-c", "user.email=e2e@nants.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Limpeza no fim, também quando o teste falha: o ambiente do Compose (containers e rede do
/// projeto do teste), a imagem se foi o teste que a baixou, e as pastas temporárias.
struct Cleanup {
    env: ComposeEnv,
    root: PathBuf,
    base: PathBuf,
    pulled: bool,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Err(e) = self.env.down_with(&self.root, true) {
            eprintln!("e2e: down falhou: {e}");
        }
        if self.pulled {
            let (ok, out) = docker(&["image", "rm", image().as_str()]);
            if !ok {
                eprintln!("e2e: a imagem baixada pelo teste não saiu: {out}");
            }
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Algum link simbólico quebrado (alvo ausente) sob `dir`.
fn broken_links(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_symlink() && fs::metadata(e.path()).is_err() {
                out.push(e.path());
            } else if t.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out
}

#[test]
#[ignore]
fn real_playwright_then_node_test_without_path_stays_green() {
    let started = Instant::now();
    let pid = format!("e2ehome{}", std::process::id());
    let base = std::env::temp_dir().join(format!("nants-e2e-home-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let root = base.join("proj");
    let homes = base.join("dados").join(home::HOMES_DIR);
    fs::create_dir_all(root.join(".nants")).expect("projeto");
    fs::create_dir_all(root.join("e2e")).expect("e2e");
    // Projeto sintético: um teste do Playwright (Chromium de verdade) e um teste do `node:test`.
    fs::write(
        root.join("package.json"),
        format!(r#"{{"name":"e2e-home","private":true,"type":"module","devDependencies":{{"@playwright/test":"{PLAYWRIGHT}"}}}}"#),
    )
    .expect("package.json");
    fs::write(root.join(".gitignore"), "node_modules/\ntest-results/\nplaywright-report/\n").expect("gitignore");
    fs::write(root.join("playwright.config.mjs"), "export default { testDir: 'e2e', reporter: 'line' };\n").expect("config");
    fs::write(
        root.join("e2e/smoke.spec.mjs"),
        // Com som (um jogo, como no projeto do defeito): o Chromium abre o PulseAudio e deixa o link
        // `~/.config/pulse/<id>-runtime` para uma pasta do `/tmp` do container, que some com ele.
        "import { test, expect } from '@playwright/test';\ntest('abre a página com som', async ({ page }) => {\n  await page.setContent('<h1>ok</h1>');\n  await expect(page.locator('h1')).toHaveText('ok');\n  await page.evaluate(async () => {\n    const c = new AudioContext();\n    const o = c.createOscillator();\n    o.connect(c.destination);\n    o.start();\n    await c.resume();\n    await new Promise((r) => setTimeout(r, 500));\n    o.stop();\n    await c.close();\n  });\n});\n",
    )
    .expect("spec");
    fs::write(root.join("sum.test.mjs"), "import test from 'node:test';\nimport assert from 'node:assert';\ntest('soma', () => assert.strictEqual(1 + 1, 2));\n").expect("unit");
    fs::write(
        root.join(".nants/compose.yaml"),
        format!("services:\n  workspace:\n    image: {}\n    command: sleep infinity\n    volumes:\n      - ${{NANTS_WORKTREE}}:/work\n", image()),
    )
    .expect("compose");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "projeto sintético"]);

    let had_image = docker(&["image", "inspect", image().as_str()]).0;
    let env = ComposeEnv::real(pid.clone()).with_homes_dir(homes.clone());
    let env = env.with_approved_hashes(vec![env_hash(&root).expect("hash")]);
    let guard = Cleanup { env, root: root.clone(), base: base.clone(), pulled: !had_image };

    // 1. Playwright de verdade no container do navegador (o mesmo caminho do Nants).
    let browser = BrowserTests::real(pid.clone()).with_homes_dir(homes.clone()).with_timeout(Duration::from_secs(600));
    let pw = browser
        .run(&root, "npm install --no-save --no-package-lock --no-audit --no-fund --loglevel=error && npx playwright test")
        .expect("playwright");
    let pw_secs = started.elapsed().as_secs();
    assert!(pw.starts_with("exit: 0"), "{pw}");
    assert!(pw.contains("1 passed"), "{pw}");
    let host_home = guard.env.host_home(&root).expect("home");
    assert!(host_home.starts_with(&homes) && host_home.join(".npm").is_dir(), "o npm usou o HOME montado");
    // O resíduo do defeito existe, e no HOME montado, fora da árvore.
    let residue = broken_links(&host_home);
    eprintln!("e2e: links quebrados deixados no HOME (fora da árvore): {residue:?}");
    assert!(residue.iter().any(|p| p.to_string_lossy().contains(".config/pulse/")), "{residue:?}");

    // 2. `node --test` sem caminho, no workspace do Compose, na mesma área de trabalho.
    guard.env.prepare(&root).expect("prepare");
    let node = guard.env.exec(&root, "node --test", Duration::from_secs(300)).expect("node --test");
    assert!(node.success(), "{}", node.output);
    assert!(node.output.contains("pass 1") && node.output.contains("fail 0"), "{}", node.output);
    let echo = guard.env.exec(&root, "echo \"$HOME\"", Duration::from_secs(60)).expect("HOME");
    assert_eq!(echo.output.trim(), CONTAINER_HOME, "{}", echo.output);

    // 3. Nada da pasta pessoal na árvore, e a árvore limpa no Git.
    assert!(!root.join(LEGACY_HOME_DIR).exists());
    assert!(broken_links(&root).iter().all(|p| p.starts_with(root.join("node_modules"))), "{:?}", broken_links(&root));
    assert_eq!(git(&root, &["status", "--porcelain", "--untracked-files=all"]), "", "git status limpo");
    eprintln!(
        "e2e: playwright em {pw_secs} s; total em {} s; imagem já presente: {had_image}; HOME no host: {}",
        started.elapsed().as_secs(),
        host_home.display()
    );
    drop(guard);
}

/// Spec 141 T02 (C-02): imagem do `node --test` da medição. O Node 20 varre as pastas com ponto
/// (`.sdd`, `.nants`, `.nants-outputs`); o 22.23 não (medido em 2026-11-04 com `node:*-slim`).
const NODE_IMAGE: &str = "node:20-slim";

/// Limpeza do e2e da raiz: os ambientes do Compose da raiz e da worktree, a imagem se o teste a
/// baixou, e as pastas temporárias.
struct RootCleanup {
    env: ComposeEnv,
    dirs: Vec<PathBuf>,
    base: PathBuf,
    pulled: bool,
}

impl Drop for RootCleanup {
    fn drop(&mut self) {
        for d in &self.dirs {
            if let Err(e) = self.env.down_with(d, true) {
                eprintln!("e2e: down falhou em {}: {e}", d.display());
            }
        }
        if self.pulled {
            let (ok, out) = docker(&["image", "rm", NODE_IMAGE]);
            if !ok {
                eprintln!("e2e: a imagem baixada pelo teste não saiu: {out}");
            }
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Spec 141 T02: a bateria da raiz (`node --test` sem caminho, pelo Compose do Nants) com uma
/// worktree de tarefa "suja" em `.sdd/worktrees`: um teste que falha e um resíduo.
/// 1. Medição: o mesmo `/work` montado sem a proteção (um `docker run` com os limites do Nants)
///    roda o teste da worktree e falha.
/// 2. Pelo Compose do Nants, a raiz fica verde; a worktree, pelo mesmo caminho, ainda enxerga os
///    próprios testes; o Git do host segue vendo a worktree e o resíduo.
#[test]
#[ignore]
fn root_battery_does_not_see_task_worktrees() {
    let started = Instant::now();
    let pid = format!("e2eroot{}", std::process::id());
    let base = std::env::temp_dir().join(format!("nants-e2e-root-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let root = base.join("proj");
    let homes = base.join("dados").join(home::HOMES_DIR);
    fs::create_dir_all(root.join(".nants")).expect("projeto");
    fs::write(root.join("package.json"), r#"{"name":"e2e-root","private":true,"type":"module"}"#).expect("package.json");
    fs::write(root.join("sum.test.mjs"), "import test from 'node:test';\nimport assert from 'node:assert';\ntest('soma', () => assert.strictEqual(1 + 1, 2));\n").expect("unit");
    fs::write(
        root.join(".nants/compose.yaml"),
        format!("services:\n  workspace:\n    image: {NODE_IMAGE}\n    command: sleep infinity\n    volumes:\n      - ${{NANTS_WORKTREE}}:/work\n"),
    )
    .expect("compose");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "projeto sintético"]);
    fs::write(root.join(".git/info/exclude"), ".sdd/worktrees/\n").expect("exclude");
    // A worktree da tarefa, no lugar do Nants, com um teste que falha (commitado na branch dela) e
    // um resíduo (um teste solto, fora do Git).
    let wt = root.join(".sdd/worktrees/s1/T01");
    git(&root, &["worktree", "add", "-q", "-b", "s1-T01", &wt.to_string_lossy()]);
    let failing = "import test from 'node:test';\nimport assert from 'node:assert';\ntest('da worktree', () => assert.fail('teste da worktree'));\n";
    fs::write(wt.join("wip.test.mjs"), failing).expect("wip");
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-q", "-m", "tarefa em andamento"]);
    fs::create_dir_all(wt.join("test")).expect("resíduo");
    fs::write(wt.join("test/residuo.test.mjs"), failing).expect("resíduo");

    let had_image = docker(&["image", "inspect", NODE_IMAGE]).0;
    let env = ComposeEnv::real(pid.clone()).with_homes_dir(homes.clone()).with_base_dir(base.join("dados").join("compose"));
    let env = env.with_approved_hashes(vec![env_hash(&root).expect("hash"), env_hash(&wt).expect("hash wt")]);
    let guard = RootCleanup { env, dirs: vec![root.clone(), wt.clone()], base: base.clone(), pulled: !had_image };
    if !had_image {
        let (ok, out) = docker(&["pull", "-q", NODE_IMAGE]);
        assert!(ok, "pull: {out}");
    }

    // 1. Medição: sem a proteção, a raiz enxerga as worktrees.
    let mut args: Vec<String> = ["run", "--rm"].map(String::from).to_vec();
    args.extend(ContainerLimits::host().docker_run_args());
    let me = fs::metadata("/proc/self").expect("uid");
    args.extend(["--user".into(), format!("{}:{}", me.uid(), me.gid()), "-v".into(), format!("{}:/work", root.display()), "-w".into(), "/work".into()]);
    args.extend([NODE_IMAGE, "node", "--test", "--test-reporter=tap"].map(String::from));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (bare_ok, bare) = docker(&refs);
    eprintln!("e2e: raiz sem a proteção:\n{bare}");
    assert!(!bare_ok, "sem a proteção, a raiz roda os testes da worktree: {bare}");
    assert!(bare.contains(".sdd/worktrees/s1/T01"), "{bare}");

    // 2. Pelo Compose do Nants: a raiz fica verde com a worktree suja presente.
    guard.env.prepare(&root).expect("prepare raiz");
    let node = guard.env.exec(&root, "node --test --test-reporter=tap", Duration::from_secs(300)).expect("node --test raiz");
    eprintln!("e2e: raiz pelo Nants:\n{}", node.output);
    assert!(node.success(), "{}", node.output);
    assert!(node.output.contains("# pass 1") && node.output.contains("# fail 0"), "{}", node.output);
    assert!(!node.output.contains(".sdd/worktrees"), "{}", node.output);
    let ls = guard.env.exec(&root, "ls -A /work/.sdd/worktrees | wc -l", Duration::from_secs(60)).expect("ls");
    assert_eq!(ls.output.trim(), "0", "a raiz vê uma pasta vazia: {}", ls.output);

    // 3. A rodada da worktree não muda: ela enxerga os próprios testes.
    guard.env.prepare(&wt).expect("prepare worktree");
    let own = guard.env.exec(&wt, "node --test --test-reporter=tap", Duration::from_secs(300)).expect("node --test worktree");
    eprintln!("e2e: worktree pelo Nants:\n{}", own.output);
    assert!(!own.success(), "{}", own.output);
    assert!(own.output.contains("# fail 2"), "{}", own.output);

    // 4. O host não muda: a worktree, o resíduo e o Git dela seguem lá.
    assert!(wt.join("test/residuo.test.mjs").is_file());
    let canon = wt.canonicalize().expect("c");
    assert!(git(&root, &["worktree", "list", "--porcelain"]).contains(&*canon.to_string_lossy()));
    assert_eq!(git(&wt, &["status", "--porcelain"]), "?? test/\n");
    assert_eq!(git(&root, &["status", "--porcelain", "--untracked-files=all"]), "", "raiz limpa no Git");
    eprintln!("e2e: total em {} s; imagem já presente: {had_image}", started.elapsed().as_secs());
    drop(guard);
}
