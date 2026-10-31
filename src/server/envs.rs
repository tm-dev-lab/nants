//! Ciclo de vida dos projetos Compose do Nants fora dos testes (spec 030, T05): fábrica de
//! `ComposeEnv` por projeto e os gatilhos de cleanup (fim da spec, remoção do projeto, shutdown
//! e órfãos no startup, por label `nants.project`). Tudo aqui é bloqueante: chame em `spawn_blocking`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::ViewPorts;
use crate::harness::compose::{controlled_env, ComposeCall, ComposeEnv, ComposeKind, ComposeRunner, RealComposeRunner};
use crate::sdd::{read_settings, sdd_dir, Environment};

use super::runs::persist_environment_hash;
use super::AppState;

/// Limite do cleanup no shutdown e na remoção.
pub const CLEANUP_TIMEOUT: Duration = Duration::from_secs(120);
/// Prazo global da varredura de órfãos no startup (roda em segundo plano).
pub const SWEEP_TIMEOUT: Duration = Duration::from_secs(60);
const LIST_TIMEOUT: Duration = Duration::from_secs(30);

/// Fábrica de `ComposeEnv` (produção: `docker` real; testes: runner falso).
#[derive(Clone, Default)]
pub struct ComposeProvider {
    runner: Option<Arc<dyn ComposeRunner>>,
    base_dir: Option<PathBuf>,
}

impl std::fmt::Debug for ComposeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComposeProvider")
    }
}

impl ComposeProvider {
    /// Executor e diretório de overrides substitutos (testes).
    pub fn with_runner(runner: Arc<dyn ComposeRunner>, base_dir: PathBuf) -> Self {
        Self { runner: Some(runner), base_dir: Some(base_dir) }
    }

    fn runner(&self) -> Arc<dyn ComposeRunner> {
        self.runner.clone().unwrap_or_else(|| Arc::new(RealComposeRunner::default()))
    }

    /// `ComposeEnv` do projeto, só se o settings dele está em `environment: docker`. Os hashes
    /// aprovados vêm do settings (conjunto vazio falha fechado).
    pub fn for_project(&self, root: &Path, project_id: &str, kind: ComposeKind) -> Option<Arc<ComposeEnv>> {
        self.for_project_lan(root, project_id, kind, None)
    }

    /// Como [`Self::for_project`]; `lan` (modo `--lan`, faixa das portas de visualização)
    /// republica as portas do `-run` em 0.0.0.0 com portas de host da faixa (spec 097).
    pub fn for_project_lan(&self, root: &Path, project_id: &str, kind: ComposeKind, lan: Option<ViewPorts>) -> Option<Arc<ComposeEnv>> {
        let settings = read_settings(&sdd_dir(root).join("settings.yaml")).ok()?;
        if settings.environment != Environment::Docker {
            return None;
        }
        let project_root = root.to_path_buf();
        let mut env = ComposeEnv::new(project_id, self.runner())
            .with_kind(kind)
            .with_lan_view_ports(lan.filter(|_| kind == ComposeKind::Run))
            .with_approved_hashes(settings.approved_hashes())
            .with_persist(Arc::new(move |hash| persist_environment_hash(&project_root, hash)));
        if let Some(base) = &self.base_dir {
            env = env.with_base_dir(base.clone());
        }
        Some(Arc::new(env))
    }
}

/// Fim da spec: derruba o `-test` (volumes e redes; imagens e o `-run` ficam). `Ok(true)` se
/// havia ambiente e foi limpo, `Ok(false)` em modo host.
pub fn cleanup_test(provider: &ComposeProvider, root: &Path, project_id: &str) -> Result<bool, String> {
    match provider.for_project(root, project_id, ComposeKind::Test) {
        Some(env) => env.down_with(root, true).map(|()| true),
        None => Ok(false),
    }
}

/// Derruba `-test` e `-run` do projeto (remoção do projeto). `run_volumes` só com confirmação do
/// usuário; `test_volumes` apaga os volumes (temporários) do `-test`. Spec 133 T12: as imagens que o
/// Compose construiu para o projeto (rótulo `nants.project`) saem junto.
/// Ambos são tentados sempre; devolve os erros.
pub fn teardown(provider: &ComposeProvider, root: &Path, project_id: &str, test_volumes: bool, run_volumes: bool) -> Vec<String> {
    let mut errors = Vec::new();
    if let Some(env) = provider.for_project(root, project_id, ComposeKind::Test) {
        if let Err(e) = env.down_removing_images(root, test_volumes) {
            errors.push(e);
        }
    }
    if let Some(env) = provider.for_project(root, project_id, ComposeKind::Run) {
        if let Err(e) = env.down_removing_images(root, run_volumes) {
            errors.push(e);
        }
    }
    errors
}

/// Shutdown gracioso: `down` (sem `-v`) do `-test` e do `-run` de cada projeto cadastrado em modo docker.
pub fn shutdown_all(provider: &ComposeProvider, projects: &[(String, PathBuf)]) {
    for (id, root) in projects {
        for kind in [ComposeKind::Test, ComposeKind::Run] {
            if let Some(env) = provider.for_project(root, id, kind) {
                if let Err(e) = env.down_with(root, false) {
                    eprintln!("aviso: não foi possível derrubar o ambiente de {id}: {e}");
                }
            }
        }
    }
}

/// Resultado da varredura de órfãos.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Projetos cadastrados cujos containers foram derrubados.
    pub cleaned: Vec<String>,
    /// Projetos com label `nants.project` que não estão cadastrados (só aviso).
    pub unknown: Vec<String>,
}

fn docker_call(args: Vec<String>) -> ComposeCall {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    ComposeCall { env: controlled_env(std::env::vars(), &cwd), args, cwd, timeout: LIST_TIMEOUT }
}

/// Recurso Docker com o rótulo `nants.project` (spec 138 T02): o tipo e o projeto do rótulo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledResource {
    /// `container` ou `network`.
    pub kind: &'static str,
    pub project: String,
}

/// Lista containers e redes com o rótulo `nants.project` (só leitura: nada é parado nem apagado).
/// Erro se o `docker` falhar (daemon parado, sem Docker).
pub fn labeled_resources(runner: &dyn ComposeRunner) -> Result<Vec<LabeledResource>, String> {
    let mut out = Vec::new();
    for (kind, list) in [("container", ["ps", "-a"]), ("network", ["network", "ls"])] {
        let mut args: Vec<String> = list.map(String::from).to_vec();
        args.extend(["--filter", "label=nants.project", "--format", "{{.ID}}\t{{.Label \"nants.project\"}}"].map(String::from));
        let reply = runner.run(&docker_call(args))?;
        if !reply.success() {
            return Err(format!("docker {kind}: {:?}", reply.code));
        }
        for line in reply.output.lines() {
            if let Some((id, project)) = line.split_once('\t') {
                let (id, project) = (id.trim(), project.trim());
                if !id.is_empty() && !project.is_empty() {
                    out.push(LabeledResource { kind, project: project.to_string() });
                }
            }
        }
    }
    Ok(out)
}

/// Startup: containers com label `nants.project` de projetos cadastrados e não ativos são
/// derrubados (sem `-v`); de projetos não cadastrados só se registra um aviso.
pub fn sweep_orphans(
    provider: &ComposeProvider,
    projects: &[(String, PathBuf)],
    is_active: &dyn Fn(&str) -> bool,
) -> Result<SweepReport, String> {
    let runner = provider.runner();
    let list = docker_call(
        ["ps", "-a", "--filter", "label=nants.project", "--format", "{{.ID}}\t{{.Label \"nants.project\"}}"]
            .map(String::from)
            .to_vec(),
    );
    let out = runner.run(&list)?;
    if !out.success() {
        return Err(format!("docker ps falhou: {}", out.output.trim()));
    }
    let mut by_project: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, project) in labeled_lines(&out.output) {
        by_project.entry(project).or_default().push(id);
    }
    let mut report = SweepReport::default();
    for (project, ids) in by_project {
        let Some((_, root)) = projects.iter().find(|(id, _)| *id == project) else {
            eprintln!("aviso: containers com label nants.project={project} de projeto não cadastrado; nada foi alterado");
            report.unknown.push(project);
            continue;
        };
        if is_active(&project) {
            continue;
        }
        let docker_mode = provider.for_project(root, &project, ComposeKind::Test).is_some();
        let mut ok = docker_mode;
        if docker_mode {
            // Reconfere a atividade antes de CADA down: o usuário pode ter subido o app durante a varredura.
            for kind in [ComposeKind::Test, ComposeKind::Run] {
                if is_active(&project) {
                    ok = true;
                    break;
                }
                if let Some(env) = provider.for_project(root, &project, kind) {
                    if let Err(e) = env.down_with(root, false) {
                        eprintln!("aviso: down de {project} falhou: {e}");
                        ok = false;
                    }
                }
            }
        }
        if is_active(&project) {
            continue;
        }
        if !ok {
            // Sem compose utilizável (modo host, hash divergente): remove os containers pelo label.
            let mut args: Vec<String> = ["rm", "-f"].map(String::from).to_vec();
            args.extend(ids);
            match runner.run(&docker_call(args)) {
                Ok(o) if o.success() => {}
                Ok(o) => eprintln!("aviso: docker rm falhou para {project}: {}", o.output.trim()),
                Err(e) => eprintln!("aviso: docker rm falhou para {project}: {e}"),
            }
        }
        report.cleaned.push(project);
    }
    sweep_browser_containers(&runner, projects, is_active);
    Ok(report)
}

/// Linhas `ID<TAB>projeto[<TAB>...]` da listagem com o rótulo `nants.project`: (id, projeto). Linha
/// sem id ou sem projeto fica de fora; colunas a mais são ignoradas.
fn labeled_lines(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let mut cols = line.split('\t').map(str::trim);
            let (id, project) = (cols.next()?, cols.next()?);
            (!id.is_empty() && !project.is_empty()).then(|| (id.to_string(), project.to_string()))
        })
        .collect()
}

/// Resultado da limpeza do Nanty ([`clean_orphans`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CleanReport {
    /// Projetos cadastrados e parados cujos containers foram derrubados (o [`sweep_orphans`]).
    pub cleaned: Vec<String>,
    /// Redes de projeto cadastrado e parado removidas.
    pub networks_removed: usize,
    /// Recursos (containers e redes) de projeto que esta instalação não conhece: nunca tocados,
    /// só contados (podem ser de outra instalação do Nants no mesmo Docker).
    pub foreign: usize,
}

/// Limpeza dos recursos do Nants que sobraram (spec 138 T03, C-16), a ação "limpar órfãos" do
/// Nanty. Só atua sobre projetos cadastrados nesta instalação e parados pela verificação estrita
/// de atividade (`is_active` = `AppState::project_busy_strict`):
/// - reaproveita o [`sweep_orphans`] para os containers;
/// - as redes entram por `docker network ls --filter label=nants.project`: a de projeto cadastrado
///   e parado sai por `docker network rm` (o Docker recusa a rede ainda em uso).
///
/// Recurso de projeto não cadastrado nunca é removido, nem parado nem em execução, nem container nem
/// rede: pode ser de outra instalação do Nants. Nunca volumes nem imagens.
pub fn clean_orphans(
    provider: &ComposeProvider,
    projects: &[(String, PathBuf)],
    is_active: &dyn Fn(&str) -> bool,
) -> Result<CleanReport, String> {
    let sweep = sweep_orphans(provider, projects, is_active)?;
    let runner = provider.runner();
    let known = |p: &str| projects.iter().any(|(id, _)| id == p);
    let mut report = CleanReport { cleaned: sweep.cleaned, foreign: sweep.unknown.len(), ..CleanReport::default() };
    let networks = runner.run(&docker_call(
        ["network", "ls", "--filter", "label=nants.project", "--format", "{{.ID}}\t{{.Label \"nants.project\"}}"]
            .map(String::from)
            .to_vec(),
    ))?;
    if !networks.success() {
        return Err(format!("docker network ls: {:?}", networks.code));
    }
    for (id, project) in labeled_lines(&networks.output) {
        if !known(&project) {
            report.foreign += 1;
            continue;
        }
        // Reconfere a atividade antes de cada remoção, como o `sweep_orphans`.
        if is_active(&project) {
            continue;
        }
        let args = vec!["network".to_string(), "rm".to_string(), id];
        if runner.run(&docker_call(args)).is_ok_and(|o| o.success()) {
            report.networks_removed += 1;
        }
    }
    Ok(report)
}

/// Spec 036: remove containers `nants-<id>-browser-*` remanescentes de projetos cadastrados e inativos.
fn sweep_browser_containers(
    runner: &std::sync::Arc<dyn crate::harness::compose::ComposeRunner>,
    projects: &[(String, PathBuf)],
    is_active: &dyn Fn(&str) -> bool,
) {
    let list = docker_call(
        ["ps", "-a", "--filter", "name=-browser-", "--format", "{{.ID}}\t{{.Names}}"].map(String::from).to_vec(),
    );
    let Ok(out) = runner.run(&list) else { return };
    if !out.success() {
        return;
    }
    let idle: Vec<String> = projects.iter().map(|(id, _)| id.clone()).filter(|id| !is_active(id)).collect();
    let ids = crate::harness::browser::browser_container_ids(&out.output, &idle);
    if ids.is_empty() {
        return;
    }
    let mut args: Vec<String> = ["rm", "-f"].map(String::from).to_vec();
    args.extend(ids);
    if let Err(e) = runner.run(&docker_call(args)) {
        eprintln!("aviso: remoção de containers de navegador falhou: {e}");
    }
}

/// Projetos cadastrados (id, raiz).
pub async fn registered_projects(state: &AppState) -> Vec<(String, PathBuf)> {
    state.config().read().await.projects.iter().map(|p| (p.id.clone(), PathBuf::from(&p.path))).collect()
}

/// Varredura de startup com o estado do servidor (nenhum app ativo ainda).
pub async fn startup_sweep(state: &AppState) {
    let projects = registered_projects(state).await;
    let provider = state.compose().clone();
    let st = state.clone();
    let job = tokio::task::spawn_blocking(move || sweep_orphans(&provider, &projects, &|id| st.project_active(id)));
    let result = match tokio::time::timeout(SWEEP_TIMEOUT, job).await {
        Ok(r) => r,
        Err(_) => {
            eprintln!("aviso: varredura de ambientes Docker excedeu {}s; desistindo", SWEEP_TIMEOUT.as_secs());
            return;
        }
    };
    match result {
        Ok(Ok(report)) => {
            if !report.cleaned.is_empty() {
                // Aviso para quem opera o servidor, no idioma do terminal (spec 115 T04).
                let list = report.cleaned.join(", ");
                println!("{}", crate::i18n::tr(crate::i18n::Lang::from_env(), "cli.orphan_envs_removed", &[("list", &list)]));
            }
        }
        Ok(Err(e)) => eprintln!("aviso: varredura de ambientes Docker ignorada: {e}"),
        Err(_) => eprintln!("aviso: varredura de ambientes Docker interrompida"),
    }
}

#[cfg(test)]
#[path = "envs_tests.rs"]
pub(crate) mod tests;
