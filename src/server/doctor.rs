//! `nants doctor` (spec 103 T02): diagnóstico do ambiente, somente leitura.
//!
//! Reaproveita o [`prereqs::diagnose`](super::prereqs::diagnose) do servidor (Git, Docker CLI,
//! daemon, permissão no socket e Compose v2, com os mesmos `id`) e acrescenta:
//! - `port`: uma porta livre para o Nants (a configurada ou uma das seguintes, como no `nants`);
//! - `disk`: espaço livre na pasta de dados e na pasta de projetos;
//! - `wsl`: o ambiente (Linux, WSL NAT, WSL espelhado; informativo; spec 111);
//! - `windows_port_redirect`: o acesso pela rede local salvo comparado com o Windows (spec 111;
//!   aviso que não bloqueia, ver [`network_item`] e [`super::lan`]);
//! - `playwright_images`: imagens `mcr.microsoft.com/playwright` já baixadas (informativo; nada é
//!   baixado aqui);
//! - `version`: versão local x `latest.json` publicado (informativo; sem rede ou sem manifesto, só
//!   avisa que não foi possível verificar).
//!
//! Só os itens obrigatórios ([`REQUIRED_IDS`]) decidem o código de saída. Primeiros passos mostra
//! os mesmos itens da máquina pelo `GET /api/system/prereqs` ([`machine_extras`]).

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::lan::{FirewallState, NetworkEnv, RedirectTarget};
use super::portproxy::WinTools;
use super::prereqs::{fix_for, label_for, CmdRunner, PrereqItem, REQUIRED_IDS};
use crate::config::Config;
use crate::i18n::{Lang, Msg};

/// Quantas portas seguintes o `nants` tenta quando a padrão está ocupada (spec 103, decisão 2).
pub const PORT_FALLBACK_TRIES: u16 = 20;
/// Espaço livre mínimo recomendado (imagens Docker e projetos).
pub const MIN_FREE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Repositório das imagens do Playwright usadas no teste de navegador.
pub const PLAYWRIGHT_REPO: &str = "mcr.microsoft.com/playwright";
/// Variável que troca o endereço base das versões publicadas (onde fica o `latest.json`).
pub const RELEASES_URL_ENV: &str = "NANTS_RELEASES_URL";
/// Endereço base padrão das versões publicadas: o `latest.json` é publicado junto com o site
/// (spec 103 T03), em `https://nants.tmdevlab.com/latest.json`, com as URLs dos pacotes na
/// Release do GitHub de `tm-dev-lab/nants` (spec 110).
pub const DEFAULT_RELEASES_URL: &str = "https://nants.tmdevlab.com";
/// Comando de instalação e atualização (reinstalar = atualizar).
pub const INSTALL_COMMAND: &str = "curl -fsSL https://nants.tmdevlab.com/install.sh | sh";
/// Tempo limite da consulta ao `latest.json`.
pub const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_MANIFEST_BYTES: usize = 64 * 1024;

fn info(id: &'static str, detail: Msg, fix: Option<String>) -> PrereqItem {
    PrereqItem {
        id,
        label: label_for(id),
        ok: true,
        version: None,
        detail: Some(detail.render(Lang::PtBr)),
        fix,
        info: true,
        warn: false,
        detail_msg: Some(detail),
    }
}

fn check(id: &'static str, ok: bool, detail: Msg) -> PrereqItem {
    PrereqItem {
        id,
        label: label_for(id),
        ok,
        version: None,
        detail: Some(detail.render(Lang::PtBr)),
        fix: if ok { None } else { Some(fix_for(id)) },
        info: false,
        warn: false,
        detail_msg: Some(detail),
    }
}

fn item_ok(items: &[PrereqItem], id: &str) -> bool {
    items.iter().any(|i| i.id == id && i.ok)
}

/// `true` quando nenhum item obrigatório falhou (ausente conta como falha).
pub fn required_ok(items: &[PrereqItem]) -> bool {
    REQUIRED_IDS.iter().all(|id| item_ok(items, id))
}

/// Porta para o Nants. `is_free` testa uma porta; a configurada 0 deixa o sistema escolher.
pub fn port_item(port: u16, is_free: impl Fn(u16) -> bool) -> PrereqItem {
    if port == 0 {
        return check("port", true, Msg::new("doctor.detail.port_auto"));
    }
    if is_free(port) {
        return check("port", true, Msg::new("doctor.detail.port_free").arg("port", port.to_string()));
    }
    let last = port.saturating_add(PORT_FALLBACK_TRIES);
    match (port.saturating_add(1)..=last).find(|p| is_free(*p)) {
        Some(next) => check(
            "port",
            true,
            Msg::new("doctor.detail.port_fallback").arg("port", port.to_string()).arg("next", next.to_string()),
        ),
        None => check(
            "port",
            false,
            Msg::new("doctor.detail.port_all_busy").arg("port", port.to_string()).arg("last", last.to_string()),
        ),
    }
}

/// Testa se a porta está livre em 127.0.0.1 (abre e fecha na hora).
pub fn loopback_port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Bytes livres para o usuário no sistema de arquivos de `path` (`statvfs`).
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c` é uma string C válida e `st` aponta para memória do tamanho de `statvfs`.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: `statvfs` retornou 0, então preencheu a estrutura.
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// Primeiro ancestral existente (a pasta pode ainda não ter sido criada).
fn existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors().find(|p| p.exists())
}

/// Pastas cujo espaço livre importa: dados do Nants e raiz dos projetos.
pub fn disk_paths(cfg: &Config) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in [crate::config::data_dir(), cfg.projects_root_path()].into_iter().flatten() {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// Espaço livre nas pastas. Abaixo de [`MIN_FREE_BYTES`] em alguma delas, falha (não obrigatório).
pub fn disk_item(paths: &[PathBuf], free: impl Fn(&Path) -> Option<u64>) -> PrereqItem {
    let measured: Vec<(&PathBuf, u64)> = paths
        .iter()
        .filter_map(|p| existing_ancestor(p).and_then(&free).map(|b| (p, b)))
        .collect();
    if measured.is_empty() {
        return info("disk", Msg::new("doctor.detail.disk_unmeasured"), None);
    }
    // O tamanho vai em bytes ([`Msg::arg_gb`]): o terminal e a interface (spec 102 T05) formatam
    // "12,3 GB" ou "12.3 GB" no idioma de cada um.
    let list = measured
        .iter()
        .map(|(p, b)| Msg::new("doctor.detail.disk_free").arg_gb("size", *b).arg("path", p.display().to_string()))
        .collect();
    let ok = measured.iter().all(|(_, b)| *b >= MIN_FREE_BYTES);
    check("disk", ok, Msg::new("doctor.detail.disk").arg_join("list", list, "; "))
}

/// WSL detectado (informativo).
pub fn wsl_item(wsl: bool) -> PrereqItem {
    let key = if wsl { "doctor.detail.wsl_detected" } else { "doctor.detail.wsl_absent" };
    info("wsl", Msg::new(key), None)
}

/// Ambiente (spec 111): Linux, WSL NAT, WSL espelhado ou WSL com o modo desconhecido (informativo).
/// O `nants doctor` e Primeiros passos (`GET /api/system/prereqs`) mostram este item no lugar do
/// [`wsl_item`] ([`place_network_items`]).
pub fn env_item(env: NetworkEnv) -> PrereqItem {
    let key = match env {
        NetworkEnv::Linux => "doctor.detail.wsl_absent",
        NetworkEnv::WslNat => "doctor.detail.env_wsl_nat",
        NetworkEnv::WslMirrored => "doctor.detail.env_wsl_mirrored",
        NetworkEnv::WslUnknown => "doctor.detail.env_wsl_unknown",
    };
    info("wsl", Msg::new(key), None)
}

/// Id do item do acesso pela rede local (o da T01, estável).
pub const NETWORK_ID: &str = "windows_port_redirect";

fn network_warn(detail: Msg) -> PrereqItem {
    PrereqItem { warn: true, fix: Some(fix_for(NETWORK_ID)), ..check(NETWORK_ID, true, detail) }
}

/// Acesso pela rede local (spec 111): a opção salva comparada com o Windows, pela regra única
/// [`super::lan::network_check`] (a mesma do campo `coherence` da API). Nunca bloqueia: OK, AVISO
/// (com o que fazer) ou INFO (não deu para conferir; no Linux direto, só informa a opção).
pub fn network_item(env: NetworkEnv, saved: Option<bool>, target: RedirectTarget, firewall: FirewallState, port: u16) -> PrereqItem {
    use super::lan::CheckLevel;
    let check_result = super::lan::network_check(env, saved, target, firewall);
    let detail = Msg::new(check_result.detail_key());
    let detail = if check_result.has_port() { detail.arg("port", port.to_string()) } else { detail };
    match check_result.level() {
        CheckLevel::Ok => check(NETWORK_ID, true, detail),
        CheckLevel::Warn => network_warn(detail),
        CheckLevel::Info => info(NETWORK_ID, detail, None),
    }
}

/// Troca o item `wsl` pelo ambiente ([`env_item`]) e põe logo depois dele o item "Acesso pela rede
/// local" ([`network_item`]) da `port`. A mesma regra para o `nants doctor` e para a API.
pub fn place_network_items(
    items: &mut Vec<PrereqItem>,
    view: &super::lan::WindowsView,
    saved: Option<bool>,
    port: u16,
    local_ips: &[Ipv4Addr],
) {
    let at = items.iter().position(|i| i.id == "wsl").map_or(items.len(), |i| i + 1);
    if let Some(env) = items.get_mut(at.saturating_sub(1)).filter(|i| i.id == "wsl") {
        *env = env_item(view.env);
    }
    let target = view.target(port, local_ips);
    items.insert(at, network_item(view.env, saved, target, view.firewall, port));
}

/// Imagens do Playwright já baixadas (informativo; nada é baixado aqui).
pub fn playwright_item(run: &CmdRunner, docker_ok: bool) -> PrereqItem {
    const ID: &str = "playwright_images";
    if !docker_ok {
        return info(ID, Msg::new("doctor.detail.playwright_no_docker"), None);
    }
    let out = run("docker", &["image", "ls", "--format", "{{.Tag}}", PLAYWRIGHT_REPO]).filter(|o| o.success);
    let Some(out) = out else {
        return info(ID, Msg::new("doctor.detail.playwright_list_failed"), None);
    };
    let mut tags: Vec<String> = out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "<none>")
        .map(|t| t.chars().take(60).collect())
        .collect();
    tags.sort();
    tags.dedup();
    if tags.is_empty() {
        return info(ID, Msg::new("doctor.detail.playwright_none"), None);
    }
    info(ID, Msg::new("doctor.detail.playwright_some").arg("tags", tags.join(", ")), None)
}

/// Itens extras da máquina exibidos também em Primeiros passos: espaço em disco, WSL e imagens do
/// Playwright. `machine` é o resultado de [`super::prereqs::diagnose`] (decide se o Docker responde).
pub fn machine_extras(run: &CmdRunner, machine: &[PrereqItem], paths: &[PathBuf], wsl: bool) -> Vec<PrereqItem> {
    let docker_ok = ["docker_cli", "docker_daemon", "docker_permission"].iter().all(|id| item_ok(machine, id));
    vec![disk_item(paths, free_bytes), wsl_item(wsl), playwright_item(run, docker_ok)]
}

/// `MAJOR.MINOR.PATCH` (com `v` opcional; sufixo `-pre`/`+build` ignorado).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Versão local x publicada. `latest` é a versão lida do manifesto ou o motivo da falha.
pub fn version_item(local: &str, latest: Result<String, Msg>) -> PrereqItem {
    const ID: &str = "version";
    let unknown = |why: Msg| info(ID, Msg::new("doctor.detail.version_unknown").arg("local", local).arg_msg("why", why), None);
    let latest = match latest {
        Ok(v) => v,
        Err(why) => return unknown(why),
    };
    match (parse_version(local), parse_version(&latest)) {
        (Some(l), Some(r)) if r > l => info(
            ID,
            Msg::new("doctor.detail.version_available")
                .arg("local", local)
                .arg("latest", latest.trim().trim_start_matches('v')),
            Some(crate::i18n::tr(Lang::PtBr, "doctor.fix.version", &[("command", INSTALL_COMMAND)])),
        ),
        (Some(_), Some(_)) => check(ID, true, Msg::new("doctor.detail.version_latest").arg("local", local)),
        _ => unknown(Msg::new("doctor.why.bad_manifest_version")),
    }
}

/// Endereço do `latest.json`: `NANTS_RELEASES_URL` (ou o padrão) + `/latest.json`.
pub fn latest_manifest_url(base: Option<String>) -> Result<String, Msg> {
    let base = base.filter(|b| !b.trim().is_empty()).unwrap_or_else(|| DEFAULT_RELEASES_URL.to_string());
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(Msg::new("doctor.why.no_url"));
    }
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return Err(Msg::new("doctor.why.bad_url"));
    }
    Ok(format!("{base}/latest.json"))
}

/// Lê a versão publicada no `latest.json` (`{"version": "X.Y.Z", ...}`), com tempo limite curto.
pub async fn fetch_latest_version(url: &str, timeout: Duration) -> Result<String, Msg> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("nants/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| Msg::new("doctor.why.no_client"))?;
    let resp = client.get(url).send().await.map_err(|_| Msg::new("doctor.why.no_connection"))?;
    if !resp.status().is_success() {
        return Err(Msg::new("doctor.why.http").arg("status", resp.status().as_u16().to_string()));
    }
    let bytes = resp.bytes().await.map_err(|_| Msg::new("doctor.why.incomplete"))?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Msg::new("doctor.why.too_large"));
    }
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| Msg::new("doctor.why.invalid"))?;
    v.get("version")
        .and_then(serde_json::Value::as_str)
        .map(|s| s.chars().take(40).collect())
        .ok_or_else(|| Msg::new("doctor.why.no_version"))
}

/// Item da versão: consulta o manifesto (sem rede ou sem manifesto, só informa).
pub async fn version_check(local: &str, base: Option<String>, timeout: Duration) -> PrereqItem {
    let latest = match latest_manifest_url(base) {
        Ok(url) => fetch_latest_version(&url, timeout).await,
        Err(why) => Err(why),
    };
    version_item(local, latest)
}

/// Entradas do `nants doctor` (injetáveis nos testes).
pub struct DoctorInput {
    pub runner: CmdRunner,
    /// Consultas ao Windows (spec 111), com o prazo próprio [`super::portproxy::NETSH_TIMEOUT`].
    pub windows: WinTools,
    pub port: u16,
    pub disk_paths: Vec<PathBuf>,
    pub wsl: bool,
    /// Opção salva `[server] lan` (`None` = nunca escolhida).
    pub saved_lan: Option<bool>,
    /// IPv4 deste WSL (destino esperado do redirecionamento).
    pub local_ips: Vec<Ipv4Addr>,
    pub releases_base: Option<String>,
    pub local_version: String,
    pub version_timeout: Duration,
}

/// Todos os itens do `nants doctor`, na ordem de exibição. A parte bloqueante (subprocessos,
/// portas, disco) roda em `spawn_blocking`, em paralelo com a consulta da versão; as consultas ao
/// Windows rodam em paralelo entre si.
pub async fn collect(input: DoctorInput) -> Vec<PrereqItem> {
    let DoctorInput {
        runner,
        windows,
        port,
        disk_paths,
        wsl,
        saved_lan,
        local_ips,
        releases_base,
        local_version,
        version_timeout,
    } = input;
    let blocking = tokio::task::spawn_blocking(move || {
        let view = super::lan::probe(&windows, wsl, super::lan::Needs::ALL, &local_ips, super::lan::PROBE_DEADLINE);
        let mut items = super::prereqs::diagnose(&runner);
        let extras = machine_extras(&runner, &items, &disk_paths, wsl);
        items.push(port_item(port, loopback_port_free));
        items.extend(extras);
        place_network_items(&mut items, &view, saved_lan, port, &local_ips);
        items
    });
    let version = version_check(&local_version, releases_base, version_timeout);
    let (items, version) = tokio::join!(blocking, version);
    let mut items = items.unwrap_or_default();
    items.push(version);
    items
}

/// Rótulo do estado na linha: `OK`, `FALHA`/`FAIL`, `AVISO`/`WARN` ou `INFO`.
pub fn status_word(item: &PrereqItem, lang: Lang) -> String {
    let key = if !item.ok {
        "doctor.status.fail"
    } else if item.warn {
        "doctor.status.warn"
    } else if item.info {
        "doctor.status.info"
    } else {
        "doctor.status.ok"
    };
    crate::i18n::text(lang, key)
}

/// Uma linha por item, no idioma do terminal (spec 102): estado, rótulo e ação pelo `id` estável
/// do item; o detalhe pela chave do dicionário (versões e saídas de comando ficam como vieram).
pub fn format_line(item: &PrereqItem, lang: Lang) -> String {
    let label = crate::i18n::lookup(lang, &format!("doctor.label.{}", item.id)).unwrap_or(item.label);
    let mut line = format!("{:<6}{label}", status_word(item, lang));
    let detail = match (item.version.as_deref().filter(|v| !v.is_empty()), &item.detail_msg) {
        (Some(v), _) => Some(v.to_string()),
        (None, Some(msg)) => Some(msg.render(lang)),
        (None, None) => item.detail.clone(),
    };
    if let Some(d) = detail.filter(|d| !d.is_empty()) {
        line.push_str(": ");
        line.push_str(&d);
    }
    if let Some(fix) = item.fix.as_deref().filter(|f| !f.is_empty()) {
        let key = format!("doctor.fix.{}", item.id);
        let fix = match crate::i18n::lookup(lang, &key) {
            Some(_) => crate::i18n::tr(lang, &key, &[("command", INSTALL_COMMAND)]),
            None => fix.to_string(),
        };
        line.push_str(&crate::i18n::tr(lang, "doctor.action", &[("fix", &fix)]));
    }
    line
}

/// Relatório completo (linhas e resumo final) no idioma do terminal.
pub fn format_report(version: &str, items: &[PrereqItem], lang: Lang) -> String {
    let mut out = crate::i18n::tr(lang, "doctor.title", &[("version", version)]);
    out.push_str("\n\n");
    for item in items {
        out.push_str(&format_line(item, lang));
        out.push('\n');
    }
    out.push('\n');
    let summary = if required_ok(items) { "doctor.summary.ok" } else { "doctor.summary.fail" };
    out.push_str(&crate::i18n::text(lang, summary));
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::prereqs::CmdOutput;
    use std::sync::Arc;

    fn ok(s: &str) -> Option<CmdOutput> {
        Some(CmdOutput { success: true, stdout: s.into(), stderr: String::new() })
    }
    fn fail(e: &str) -> Option<CmdOutput> {
        Some(CmdOutput { success: false, stdout: String::new(), stderr: e.into() })
    }
    fn by_id<'a>(v: &'a [PrereqItem], id: &str) -> &'a PrereqItem {
        v.iter().find(|i| i.id == id).expect("item")
    }

    /// Runner falso: máquina completa; `images` é a saída de `docker image ls`.
    fn healthy(images: &'static str) -> CmdRunner {
        Arc::new(move |p, a| match (p, a.first().copied()) {
            ("git", _) => ok("git version 2.43.0\n"),
            ("docker", Some("--version")) => ok("Docker version 29.0.0, build x"),
            ("docker", Some("info")) => ok("29.0.0\n"),
            ("docker", Some("compose")) => ok("2.40.0\n"),
            ("docker", Some("image")) => {
                assert_eq!(a, ["image", "ls", "--format", "{{.Tag}}", PLAYWRIGHT_REPO]);
                ok(images)
            }
            _ => None,
        })
    }

    fn no_forbidden_glyphs(s: &str) {
        for c in ['\u{2014}', '\u{2013}'] {
            assert!(!s.contains(c), "travessão em {s}");
        }
    }

    #[test]
    fn port_free_busy_with_fallback_and_all_busy() {
        assert!(port_item(3000, |_| true).ok);
        let fb = port_item(3000, |p| p == 3003);
        assert!(fb.ok && fb.detail.as_deref().unwrap_or("").contains("usará a 3003"), "{fb:?}");
        let none = port_item(3000, |_| false);
        assert!(!none.ok && !none.info);
        assert!(none.detail.as_deref().unwrap_or("").contains("3000 a 3020"));
        assert!(none.fix.as_deref().unwrap_or("").contains("--port"));
        assert!(port_item(0, |_| false).ok);
    }

    #[test]
    fn real_port_probe_detects_busy_port() {
        let busy = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = busy.local_addr().expect("addr").port();
        assert!(!loopback_port_free(port));
    }

    #[test]
    fn disk_ok_low_and_unmeasurable() {
        let p = vec![PathBuf::from("/"), PathBuf::from("/inexistente/nants/projetos")];
        let plenty = disk_item(&p, |_| Some(20 * 1024 * 1024 * 1024));
        assert!(plenty.ok && !plenty.info);
        assert!(plenty.detail.as_deref().unwrap_or("").contains("20,0 GB livres em /inexistente/nants/projetos"));
        let low = disk_item(&p, |path| Some(if path == Path::new("/") { 1024 * 1024 * 1024 } else { u64::MAX }));
        assert!(!low.ok && low.fix.is_some());
        let unknown = disk_item(&p, |_| None);
        assert!(unknown.ok && unknown.info);
        // Medição real: a raiz sempre existe.
        assert!(free_bytes(Path::new("/")).is_some());
    }

    #[test]
    fn wsl_is_informative() {
        assert!(wsl_item(true).info && wsl_item(true).ok);
        assert!(wsl_item(false).detail.as_deref().unwrap_or("").contains("não detectado"));
    }

    #[test]
    fn environment_item_names_linux_nat_and_mirrored() {
        let line = |env, lang| format_line(&env_item(env), lang);
        assert!(line(NetworkEnv::Linux, Lang::PtBr).starts_with("INFO  WSL: não detectado (Linux)"));
        assert!(line(NetworkEnv::WslNat, Lang::PtBr).contains("WSL no modo de rede NAT"), "{}", line(NetworkEnv::WslNat, Lang::PtBr));
        assert!(line(NetworkEnv::WslMirrored, Lang::PtBr).contains("WSL no modo de rede espelhado"));
        assert!(line(NetworkEnv::WslUnknown, Lang::PtBr).contains("tratado como NAT"));
        assert!(line(NetworkEnv::WslNat, Lang::En).contains("WSL in NAT networking mode"));
        assert!(line(NetworkEnv::WslMirrored, Lang::En).contains("WSL in mirrored networking mode"));
        assert!(env_item(NetworkEnv::WslNat).info);
    }

    #[test]
    fn network_item_compares_the_saved_option_with_windows() {
        use FirewallState::{Absent, Present, Unknown as FwUnknown};
        use NetworkEnv::{Linux, WslMirrored, WslNat, WslUnknown};
        use RedirectTarget::{None as NoRedirect, Other, ThisWsl, Unknown};
        let kind = |i: &PrereqItem| {
            assert!(i.ok, "nunca bloqueia: {i:?}");
            assert_eq!(i.warn, i.fix.is_some(), "aviso traz a orientação: {i:?}");
            if i.warn {
                "warn"
            } else if i.info {
                "info"
            } else {
                "ok"
            }
        };
        let cases = [
            // Rede ligada com o Windows configurado: OK.
            (WslNat, Some(true), ThisWsl, Present, "ok", "lan_on_ok"),
            (WslNat, Some(true), ThisWsl, FwUnknown, "ok", "lan_on_ok"),
            (WslMirrored, Some(true), NoRedirect, Present, "ok", "lan_on_mirrored_ok"),
            // Rede ligada sem o Windows configurado: AVISO, rodar o script.
            (WslNat, Some(true), NoRedirect, Present, "warn", "lan_on_no_redirect"),
            (WslUnknown, Some(true), NoRedirect, FwUnknown, "warn", "lan_on_no_redirect"),
            (WslNat, Some(true), Other, Present, "warn", "lan_on_other_target"),
            (WslNat, Some(true), ThisWsl, Absent, "warn", "lan_on_no_firewall"),
            (WslMirrored, Some(true), NoRedirect, Absent, "warn", "lan_on_no_firewall"),
            // Rede desligada com o Windows redirecionando: AVISO, desfazer ou ligar.
            (WslNat, Some(false), ThisWsl, Present, "warn", "windows_redirect_on"),
            (WslNat, None, Other, Present, "warn", "windows_redirect_on"),
            (WslNat, None, ThisWsl, Absent, "warn", "windows_redirect_on"),
            (WslUnknown, None, ThisWsl, Present, "warn", "windows_redirect_on"),
            (WslMirrored, Some(false), ThisWsl, Present, "warn", "windows_redirect_on"),
            // Nunca escolhida e o Windows configurado: o próximo início adota.
            (WslNat, None, ThisWsl, Present, "ok", "lan_will_adopt"),
            // Desligada e sem redirecionamento: OK.
            (WslNat, Some(false), NoRedirect, Absent, "ok", "windows_redirect_off"),
            (WslNat, None, NoRedirect, FwUnknown, "ok", "windows_redirect_off"),
            // Não deu para conferir: INFO.
            (WslNat, Some(false), Unknown, FwUnknown, "info", "windows_redirect_unknown"),
            (WslNat, Some(true), Unknown, Present, "info", "windows_redirect_unknown"),
            (WslMirrored, Some(true), NoRedirect, FwUnknown, "info", "firewall_unknown"),
            // Linux direto: só informa a opção.
            (Linux, Some(true), NoRedirect, Absent, "info", "lan_linux_on"),
            (Linux, None, NoRedirect, Absent, "info", "lan_linux_off"),
            (Linux, Some(false), NoRedirect, Absent, "info", "lan_linux_off"),
        ];
        for (env, saved, target, fw, want, key) in cases {
            let item = network_item(env, saved, target, fw, 3000);
            let case = format!("{env:?} {saved:?} {target:?} {fw:?}");
            assert_eq!(kind(&item), want, "{case}");
            let msg = item.detail_msg.as_ref().expect("detalhe");
            assert_eq!(msg.neutral("doctor.detail.").code, key, "{case}");
        }
        let warn = network_item(WslNat, Some(false), ThisWsl, Present, 3000);
        let line = format_line(&warn, Lang::PtBr);
        assert!(line.starts_with("AVISO Acesso pela rede local: o Windows está redirecionando a porta 3000"), "{line}");
        assert!(line.contains("Configurações > Acesso pela rede") && line.contains("PowerShell como administrador"), "{line}");
        let line = format_line(&warn, Lang::En);
        assert!(line.starts_with("WARN  Local network access: Windows is redirecting port 3000"), "{line}");
        assert!(line.contains("Settings > Network access") && line.contains("PowerShell as administrator"), "{line}");
        // O aviso não muda o "Tudo pronto".
        let mut items = crate::server::prereqs::diagnose(&healthy(""));
        items.push(warn);
        assert!(required_ok(&items));
        assert!(format_report("0.1.0", &items, Lang::PtBr).contains("Tudo pronto"));
    }

    /// Windows falso: `netsh` com a saída dada, `wslinfo` com o modo e o PowerShell com a contagem.
    fn windows(netsh: Option<&'static str>, mode: Option<&'static str>, rules: Option<&'static str>) -> WinTools {
        WinTools {
            run: Arc::new(move |p, a| match p {
                "netsh.exe" => {
                    assert_eq!(a, crate::server::portproxy::NETSH_ARGS);
                    netsh.and_then(ok)
                }
                "wslinfo" => mode.and_then(ok),
                "powershell.exe" => {
                    assert_eq!(a, crate::server::lan::FIREWALL_ARGS, "só leitura");
                    rules.and_then(ok)
                }
                other => panic!("programa inesperado: {other}"),
            }),
            exists: Arc::new(|p| ["netsh.exe", "wslinfo", "powershell.exe"].contains(&p)),
        }
    }

    #[tokio::test]
    async fn collect_shows_the_environment_and_the_network_item() {
        let pt_out = "Escuta em ipv4:  Conectar-se a ipv4:\r\n\r\nEndereço Porta Endereço Porta\r\n--- --- --- ---\r\n0.0.0.0 3000 172.21.154.252 3000\r\n";
        let wsl_input = |netsh, mode, rules, saved| {
            let mut i = input(healthy(""));
            i.windows = windows(netsh, mode, rules);
            i.wsl = true;
            i.port = 3000;
            i.saved_lan = saved;
            i.local_ips = vec![Ipv4Addr::new(172, 21, 154, 252)];
            i
        };
        // Desligada (nunca escolhida) e redirecionada sem a regra: aviso.
        let items = collect(wsl_input(Some(pt_out), Some("nat\n"), Some("0\r\n"), None)).await;
        let ids: Vec<&str> = items.iter().map(|i| i.id).collect();
        let at = ids.iter().position(|i| *i == "wsl").expect("wsl");
        assert_eq!(ids.get(at + 1), Some(&NETWORK_ID), "{ids:?}");
        assert!(by_id(&items, "wsl").detail.as_deref().unwrap_or("").contains("modo de rede NAT"));
        assert!(by_id(&items, NETWORK_ID).warn);
        assert!(required_ok(&items));
        // Ligada e configurada: OK.
        let items = collect(wsl_input(Some(pt_out), Some("nat"), Some("1"), Some(true))).await;
        let item = by_id(&items, NETWORK_ID);
        assert!(item.ok && !item.warn && !item.info, "{item:?}");
        // Espelhado, ligada, sem a regra: aviso.
        let items = collect(wsl_input(Some(""), Some("mirrored"), Some("0"), Some(true))).await;
        assert!(by_id(&items, "wsl").detail.as_deref().unwrap_or("").contains("espelhado"));
        assert!(by_id(&items, NETWORK_ID).warn);
        // Sem interop: só informa, e o ambiente fica desconhecido.
        let items = collect(wsl_input(None, None, None, Some(false))).await;
        assert!(by_id(&items, NETWORK_ID).info);
        assert!(by_id(&items, "wsl").detail.as_deref().unwrap_or("").contains("tratado como NAT"));
        // Fora do WSL: nada do Windows roda (o executor falso entraria em pânico) e o item só informa.
        let mut linux = input(healthy(""));
        linux.windows = WinTools { run: Arc::new(|p, _| panic!("executou {p}")), exists: Arc::new(|_| true) };
        let items = collect(linux).await;
        assert!(by_id(&items, NETWORK_ID).info);
        assert!(by_id(&items, "wsl").detail.as_deref().unwrap_or("").contains("não detectado"));
    }

    #[test]
    fn playwright_lists_existing_images_without_pulling() {
        let none = playwright_item(&healthy(""), true);
        assert!(none.info && none.ok);
        assert!(none.detail.as_deref().unwrap_or("").contains("cerca de 2 GB"));
        let some = playwright_item(&healthy("v1.50.0-noble\nv1.48.0-noble\n<none>\n"), true);
        assert!(some.detail.as_deref().unwrap_or("").contains("já baixadas: v1.48.0-noble, v1.50.0-noble"), "{some:?}");
        // Sem Docker, nem tenta listar.
        let never: CmdRunner = Arc::new(|_, _| panic!("não deve executar"));
        let off = playwright_item(&never, false);
        assert!(off.info && off.detail.as_deref().unwrap_or("").contains("Docker indisponível"));
        let failed: CmdRunner = Arc::new(|_, _| fail("boom"));
        assert!(playwright_item(&failed, true).detail.as_deref().unwrap_or("").contains("não foi possível listar"));
    }

    #[test]
    fn versions_compare_and_never_fail() {
        assert_eq!(parse_version("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
        let newer = version_item("0.1.0", Ok("0.2.0".into()));
        assert!(newer.ok && newer.info);
        assert!(newer.detail.as_deref().unwrap_or("").contains("a 0.2.0 está disponível"));
        assert!(newer.fix.as_deref().unwrap_or("").contains(INSTALL_COMMAND));
        let same = version_item("0.1.0", Ok("0.1.0".into()));
        assert!(same.ok && !same.info && same.fix.is_none());
        assert!(version_item("0.2.0", Ok("0.1.0".into())).fix.is_none());
        for latest in [Err(Msg::new("doctor.why.no_connection")), Ok("lixo".to_string())] {
            let item = version_item("0.1.0", latest);
            assert!(item.ok && item.info, "{item:?}");
            assert!(item.detail.as_deref().unwrap_or("").contains("não foi possível verificar"));
        }
    }

    #[test]
    fn manifest_url_from_env_or_default() {
        assert_eq!(latest_manifest_url(Some("https://x.test/rel/".into())).as_deref(), Ok("https://x.test/rel/latest.json"));
        assert!(latest_manifest_url(Some("ftp://x".into())).is_err());
        assert_eq!(latest_manifest_url(None).as_deref(), Ok("https://nants.tmdevlab.com/latest.json"));
        assert_eq!(latest_manifest_url(Some("  ".into())).as_deref(), Ok("https://nants.tmdevlab.com/latest.json"));
    }

    async fn serve_manifest(status: u16, body: &'static str) -> String {
        use axum::http::StatusCode;
        let app = axum::Router::new().route(
            "/latest.json",
            axum::routing::get(move || async move { (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn version_check_reads_manifest_and_tolerates_errors() {
        let t = Duration::from_secs(3);
        let base = serve_manifest(200, r#"{"version":"9.0.0","assets":{}}"#).await;
        let item = version_check("0.1.0", Some(base), t).await;
        assert!(item.detail.as_deref().unwrap_or("").contains("a 9.0.0 está disponível"), "{item:?}");
        let missing = serve_manifest(404, "não há").await;
        let item = version_check("0.1.0", Some(missing), t).await;
        assert!(item.ok && item.info && item.detail.as_deref().unwrap_or("").contains("HTTP 404"), "{item:?}");
        let bad = serve_manifest(200, "{}").await;
        assert!(version_check("0.1.0", Some(bad), t).await.detail.as_deref().unwrap_or("").contains("sem versão"));
        // Porta fechada: sem conexão, só informa.
        let closed = {
            let l = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
            l.local_addr().expect("addr").port()
        };
        let item = version_check("0.1.0", Some(format!("http://127.0.0.1:{closed}")), t).await;
        assert!(item.ok && item.info, "{item:?}");
    }

    fn input(runner: CmdRunner) -> DoctorInput {
        DoctorInput {
            runner,
            windows: windows(None, None, None),
            port: 0,
            disk_paths: vec![PathBuf::from("/")],
            wsl: false,
            saved_lan: None,
            local_ips: Vec::new(),
            releases_base: Some("ftp://invalido".into()),
            local_version: "0.1.0".into(),
            version_timeout: Duration::from_secs(1),
        }
    }

    #[tokio::test]
    async fn collect_orders_items_and_exit_depends_only_on_required() {
        let items = collect(input(healthy("v1.48.0-noble"))).await;
        let ids: Vec<&str> = items.iter().map(|i| i.id).collect();
        assert_eq!(
            ids,
            [
                "git",
                "docker_cli",
                "docker_daemon",
                "docker_permission",
                "docker_compose",
                "port",
                "disk",
                "wsl",
                "windows_port_redirect",
                "playwright_images",
                "version"
            ]
        );
        assert!(required_ok(&items));
        let report = format_report("0.1.0", &items, Lang::PtBr);
        assert!(report.contains("OK    Git: git version 2.43.0"), "{report}");
        assert!(report.contains("INFO  Versão do Nants"), "{report}");
        assert!(report.contains("Tudo pronto"));
        no_forbidden_glyphs(&report);

        // Sem Docker: obrigatórios falham, o resto continua informando.
        let no_docker: CmdRunner = Arc::new(|p, _| if p == "git" { ok("git version 2") } else { None });
        let items = collect(input(no_docker)).await;
        assert!(!required_ok(&items));
        let report = format_report("0.1.0", &items, Lang::PtBr);
        assert!(report.contains("FALHA Docker CLI: docker não encontrado. Ação: Instale o Docker Engine"), "{report}");
        assert!(by_id(&items, "playwright_images").detail.as_deref().unwrap_or("").contains("Docker indisponível"));
        assert!(report.contains("Falta algo obrigatório"));
        no_forbidden_glyphs(&report);

        // Falha só em item não obrigatório (porta, disco) não muda o código de saída.
        let mut items = collect(input(healthy(""))).await;
        for i in items.iter_mut().filter(|i| i.id == "port" || i.id == "disk") {
            i.ok = false;
        }
        assert!(required_ok(&items));
    }

    #[test]
    fn permission_denied_is_required_failure() {
        let r: CmdRunner = Arc::new(|p, a| match (p, a.first().copied()) {
            ("docker", Some("info")) => fail("permission denied while trying to connect"),
            _ => ok("x"),
        });
        let items = crate::server::prereqs::diagnose(&r);
        assert!(!required_ok(&items));
        let line = format_line(by_id(&items, "docker_permission"), Lang::PtBr);
        assert!(line.starts_with("FALHA ") && line.contains("usermod -aG docker"), "{line}");
    }

    #[tokio::test]
    async fn report_in_english_uses_the_dictionary_by_item_id() {
        let items = collect(input(healthy("v1.48.0-noble"))).await;
        let report = format_report("0.1.0", &items, Lang::En);
        for line in [
            "nants 0.1.0: environment check",
            "OK    Git: git version 2.43.0",
            "OK    Docker permission",
            "OK    Nants port",
            "INFO  WSL: not detected (Linux)",
            "INFO  Playwright images: already downloaded: v1.48.0-noble",
            "INFO  Nants version: 0.1.0 installed; could not check for a new version (versions address is invalid)",
            "All set. Type nants to open Nants in the browser.",
        ] {
            assert!(report.contains(line), "faltou {line:?} em:\n{report}");
        }
        no_forbidden_glyphs(&report);
        let no_docker: CmdRunner = Arc::new(|p, _| if p == "git" { ok("git version 2") } else { None });
        let items = collect(input(no_docker)).await;
        let report = format_report("0.1.0", &items, Lang::En);
        assert!(report.contains("FAIL  Docker CLI: docker not found. Action: Install Docker Engine"), "{report}");
        assert!(report.contains("FAIL  Docker daemon: depends on the Docker CLI"), "{report}");
        assert!(report.contains("Something required is missing"), "{report}");
        // Nenhum texto em pt-BR sobra no relatório em inglês.
        for pt in ["não ", "ção", "Ação", "FALHA", "Porta do", "Espaço", "instalada", "obrigatório"] {
            assert!(!report.contains(pt), "sobrou {pt:?} em:\n{report}");
        }
    }

    #[test]
    fn every_item_detail_and_fix_exists_in_both_languages() {
        let p = vec![PathBuf::from("/")];
        let mut items = vec![
            port_item(0, |_| true),
            port_item(3000, |_| true),
            port_item(3000, |p| p == 3001),
            port_item(3000, |_| false),
            disk_item(&p, |_| Some(1024)),
            disk_item(&p, |_| None),
            wsl_item(true),
            wsl_item(false),
            env_item(NetworkEnv::Linux),
            env_item(NetworkEnv::WslNat),
            env_item(NetworkEnv::WslMirrored),
            env_item(NetworkEnv::WslUnknown),
            playwright_item(&healthy(""), true),
            playwright_item(&healthy("v1"), true),
            playwright_item(&healthy(""), false),
            version_item("0.1.0", Ok("0.2.0".into())),
            version_item("0.1.0", Ok("0.1.0".into())),
            version_item("0.1.0", Ok("x".into())),
        ];
        for why in ["no_url", "bad_url", "no_client", "no_connection", "incomplete", "too_large", "invalid", "no_version"] {
            let key: &'static str = Box::leak(format!("doctor.why.{why}").into_boxed_str());
            items.push(version_item("0.1.0", Err(Msg::new(key))));
        }
        items.push(version_item("0.1.0", Err(Msg::new("doctor.why.http").arg("status", "500"))));
        for env in [NetworkEnv::Linux, NetworkEnv::WslNat, NetworkEnv::WslMirrored, NetworkEnv::WslUnknown] {
            for saved in [None, Some(false), Some(true)] {
                for target in [RedirectTarget::ThisWsl, RedirectTarget::Other, RedirectTarget::None, RedirectTarget::Unknown] {
                    for fw in [FirewallState::Present, FirewallState::Absent, FirewallState::Unknown] {
                        items.push(network_item(env, saved, target, fw, 3000));
                    }
                }
            }
        }
        let absent: CmdRunner = Arc::new(|_, _| None);
        let denied: CmdRunner = Arc::new(|p, a| match (p, a.first().copied()) {
            ("docker", Some("info")) => fail("permission denied"),
            _ => ok("x"),
        });
        let down: CmdRunner = Arc::new(|p, a| match (p, a.first().copied()) {
            ("docker", Some("info")) => fail("down"),
            ("docker", Some("compose")) => None,
            _ => ok("x"),
        });
        for runner in [absent, denied, down] {
            items.extend(crate::server::prereqs::diagnose(&runner));
        }
        for item in &items {
            for lang in [Lang::PtBr, Lang::En] {
                let line = format_line(item, lang);
                assert!(!line.contains("doctor."), "chave sem texto em {line}");
                assert!(!line.contains('{'), "parâmetro não preenchido em {line}");
                no_forbidden_glyphs(&line);
            }
            // pt-BR continua igual ao campo da API.
            if let (Some(msg), None) = (&item.detail_msg, &item.version) {
                assert_eq!(Some(msg.render(Lang::PtBr)), item.detail);
            }
        }
        let low = disk_item(&p, |_| Some(1536 * 1024 * 1024));
        assert!(format_line(&low, Lang::En).contains("1.5 GB free in /"), "{}", format_line(&low, Lang::En));
        assert!(format_line(&low, Lang::PtBr).contains("1,5 GB livres em /"));
    }
}
