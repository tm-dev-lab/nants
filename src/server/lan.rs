//! Acesso pela rede local como opção salva (spec 111).
//!
//! - **Ambiente:** Linux direto, WSL no modo NAT, WSL no modo espelhado ou WSL com o modo de rede
//!   desconhecido (`wslinfo --networking-mode`; qualquer falha dá desconhecido, tratado como NAT nas
//!   instruções e nunca motivo de adoção).
//! - **Windows, só leitura:** a tabela do `netsh ... portproxy show` ([`super::portproxy`]) e a regra
//!   de firewall "Nants" (`Get-NetFirewallRule`). As consultas rodam em paralelo, cada uma com
//!   [`NETSH_TIMEOUT`]; fora do WSL nada roda. O Nants nunca altera o Windows.
//! - **Decisão do modo ao subir** ([`decide_mode`]): `--lan` ou `--host` explícitos, depois a opção
//!   salva `[server] lan`, depois a adoção (WSL NAT com o redirecionamento da porta para este WSL e
//!   a regra "Nants") e, por fim, o modo local.
//! - **Porta que sempre abre** ([`candidate_ports`]): no modo local, a porta redirecionada pelo
//!   Windows conta como ocupada.
//! - **API** de Configurações > Acesso pela rede: `GET` e `PUT /api/config/network`.

use std::net::Ipv4Addr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::error::ApiError;
use super::portproxy::{self, Redirect, RedirectState, WinTools, NETSH_TIMEOUT};
use super::prereqs::CmdOutput;
use super::state::AppState;

/// Onde procurar o `wslinfo`: pelo PATH e, sem ele, no caminho que o WSL instala.
pub const WSLINFO_PROGRAMS: [&str; 2] = ["wslinfo", "/usr/bin/wslinfo"];
pub const WSLINFO_ARGS: [&str; 1] = ["--networking-mode"];
/// Onde procurar o PowerShell do Windows: pelo PATH da interop e no caminho padrão.
pub const POWERSHELL_PROGRAMS: [&str; 2] =
    ["powershell.exe", "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"];
/// Nome da regra de firewall criada pelo script de Configurações > Acesso pela rede.
pub const FIREWALL_RULE: &str = "Nants";
/// Consulta só de leitura: imprime quantas regras "Nants" existem (0 quando nenhuma) e sai com 2
/// em qualquer outro erro (que vira "não deu para conferir").
pub const FIREWALL_QUERY: &str = "try { @(Get-NetFirewallRule -DisplayName 'Nants' -ErrorAction Stop).Count } catch { if ($_.CategoryInfo.Category -eq 'ObjectNotFound') { 0 } else { exit 2 } }";
pub const FIREWALL_ARGS: [&str; 4] = ["-NoProfile", "-NonInteractive", "-Command", FIREWALL_QUERY];
/// Consulta só de leitura dos IPv4 do Windows (spec 112): uma linha `ip|interface|gw|up` por
/// endereço, com `gw` = 1 se a interface tem rota padrão e `up` = 1 se o adaptador está Up.
pub const WINDOWS_IPS_QUERY: &str = "$gw = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | ForEach-Object { $_.InterfaceIndex }); $up = @(Get-NetAdapter -ErrorAction SilentlyContinue | Where-Object { $_.Status -eq 'Up' } | ForEach-Object { $_.ifIndex }); Get-NetIPAddress -AddressFamily IPv4 -ErrorAction Stop | ForEach-Object { '{0}|{1}|{2}|{3}' -f $_.IPAddress, $_.InterfaceAlias, [int]($gw -contains $_.InterfaceIndex), [int]($up -contains $_.InterfaceIndex) }";
pub const WINDOWS_IPS_ARGS: [&str; 4] = ["-NoProfile", "-NonInteractive", "-Command", WINDOWS_IPS_QUERY];
/// Validade da lista de IPs do Windows em memória (a tela consulta o código a cada segundo).
pub const WINDOWS_IPS_TTL: Duration = Duration::from_secs(30);

/// IPv4 de rede do Windows pela saída de [`WINDOWS_IPS_QUERY`]: sem loopback, APIPA (169.254) e
/// interfaces virtuais (vEthernet do WSL e do Hyper-V); primeiro as com rota padrão e Up. Havendo
/// alguma com rota padrão ou Up, as demais ficam de fora. `None`: não deu para conferir.
pub fn windows_ips_from_output(out: Option<CmdOutput>) -> Option<Vec<Ipv4Addr>> {
    let out = out.filter(|o| o.success)?;
    let mut found: Vec<(Ipv4Addr, bool, bool)> = Vec::new();
    for line in out.stdout.lines() {
        let fields: Vec<&str> = line.trim().split('|').collect();
        let [ip, alias, gw, up] = fields.as_slice() else {
            continue;
        };
        let Ok(ip) = ip.trim().parse::<Ipv4Addr>() else {
            continue;
        };
        let alias = alias.trim().to_ascii_lowercase();
        let virtual_if = alias.starts_with("vethernet") || alias.contains("wsl") || alias.contains("hyper-v") || alias.contains("loopback");
        if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || virtual_if {
            continue;
        }
        if !found.iter().any(|(f, _, _)| *f == ip) {
            found.push((ip, gw.trim() == "1", up.trim() == "1"));
        }
    }
    if found.iter().any(|(_, gw, up)| *gw || *up) {
        found.retain(|(_, gw, up)| *gw || *up);
    }
    // Ordem estável: rota padrão e Up primeiro.
    found.sort_by_key(|(_, gw, up)| std::cmp::Reverse((*gw && *up, *gw, *up)));
    Some(found.into_iter().map(|(ip, _, _)| ip).collect())
}

/// IPv4 de rede do Windows (só no WSL; fora dele, `None`). Só leitura, com o prazo do executor.
pub fn windows_ips_with(tools: &WinTools, wsl: bool) -> Option<Vec<Ipv4Addr>> {
    if !wsl {
        return None;
    }
    windows_ips_from_output(tools.run_first(&POWERSHELL_PROGRAMS, &WINDOWS_IPS_ARGS))
}

/// Prazo total da conferência em paralelo (o prazo de cada consulta e uma folga para as threads).
pub const PROBE_DEADLINE: Duration = Duration::from_millis(NETSH_TIMEOUT.as_millis() as u64 + 300);
/// Comando para reiniciar o Nants, mostrado pela interface.
pub const RESTART_COMMAND: &str = "nants";

/// Ambiente de rede do servidor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkEnv {
    Linux,
    WslNat,
    WslMirrored,
    /// WSL sem o modo conferido: tratado como NAT nas instruções, nunca motivo de adoção.
    WslUnknown,
}

impl NetworkEnv {
    /// Forma da API: `linux`, `wsl_nat`, `wsl_mirrored` ou `wsl_unknown`.
    pub fn as_str(self) -> &'static str {
        match self {
            NetworkEnv::Linux => "linux",
            NetworkEnv::WslNat => "wsl_nat",
            NetworkEnv::WslMirrored => "wsl_mirrored",
            NetworkEnv::WslUnknown => "wsl_unknown",
        }
    }

    pub fn is_wsl(self) -> bool {
        self != NetworkEnv::Linux
    }

    /// Precisa do `portproxy` (NAT ou desconhecido).
    pub fn nat_like(self) -> bool {
        matches!(self, NetworkEnv::WslNat | NetworkEnv::WslUnknown)
    }
}

/// Modo de rede do WSL pela saída do `wslinfo --networking-mode` (já sabendo que é WSL).
pub fn env_from_output(out: Option<CmdOutput>) -> NetworkEnv {
    let Some(out) = out.filter(|o| o.success) else {
        return NetworkEnv::WslUnknown;
    };
    match out.stdout.trim().to_ascii_lowercase().as_str() {
        "nat" => NetworkEnv::WslNat,
        "mirrored" => NetworkEnv::WslMirrored,
        _ => NetworkEnv::WslUnknown,
    }
}

/// Ambiente: fora do WSL é Linux, sem executar nada.
pub fn detect_env(tools: &WinTools, wsl: bool) -> NetworkEnv {
    if !wsl {
        return NetworkEnv::Linux;
    }
    env_from_output(tools.run_first(&WSLINFO_PROGRAMS, &WSLINFO_ARGS))
}

/// Regra de firewall "Nants" no Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallState {
    Present,
    Absent,
    Unknown,
}

impl FirewallState {
    /// Forma da API: `yes`, `no` ou `unknown`.
    pub fn as_str(self) -> &'static str {
        match self {
            FirewallState::Present => "yes",
            FirewallState::Absent => "no",
            FirewallState::Unknown => "unknown",
        }
    }
}

/// Estado pela saída da consulta (`None` = não executou ou tempo esgotado).
pub fn firewall_from_output(out: Option<CmdOutput>) -> FirewallState {
    let Some(out) = out.filter(|o| o.success) else {
        return FirewallState::Unknown;
    };
    match out.stdout.trim().parse::<u32>() {
        Ok(0) => FirewallState::Absent,
        Ok(_) => FirewallState::Present,
        Err(_) => FirewallState::Unknown,
    }
}

/// Confere a regra. Fora do WSL não executa nada (não há Windows).
pub fn firewall_with(tools: &WinTools, wsl: bool) -> FirewallState {
    if !wsl {
        return FirewallState::Absent;
    }
    firewall_from_output(tools.run_first(&POWERSHELL_PROGRAMS, &FIREWALL_ARGS))
}

/// Para onde o Windows manda a porta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectTarget {
    /// Para um IPv4 deste WSL.
    ThisWsl,
    /// Para outro endereço (por exemplo, o IP antigo do WSL, que muda quando o Windows reinicia).
    Other,
    None,
    Unknown,
}

impl RedirectTarget {
    /// Forma da API: `this_wsl`, `other`, `none` ou `unknown`.
    pub fn as_str(self) -> &'static str {
        match self {
            RedirectTarget::ThisWsl => "this_wsl",
            RedirectTarget::Other => "other",
            RedirectTarget::None => "none",
            RedirectTarget::Unknown => "unknown",
        }
    }
}

/// Destino do redirecionamento da `port` (`table` `None` = não deu para conferir).
pub fn target_of(table: Option<&[Redirect]>, port: u16, local_ips: &[Ipv4Addr]) -> RedirectTarget {
    let Some(table) = table else {
        return RedirectTarget::Unknown;
    };
    let mut found = table.iter().filter(|r| r.listen_port == port).peekable();
    if found.peek().is_none() {
        return RedirectTarget::None;
    }
    if found.any(|r| r.connect_addr.parse::<Ipv4Addr>().is_ok_and(|ip| local_ips.contains(&ip))) {
        RedirectTarget::ThisWsl
    } else {
        RedirectTarget::Other
    }
}

/// O que o Windows mostra (só leitura).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsView {
    pub env: NetworkEnv,
    /// Tabela do `portproxy`; `None` = não deu para conferir (ou não consultada).
    pub table: Option<Vec<Redirect>>,
    pub firewall: FirewallState,
}

impl WindowsView {
    /// Estado da porta.
    pub fn redirect(&self, port: u16) -> RedirectState {
        if !self.env.is_wsl() {
            return RedirectState::NotRedirected;
        }
        portproxy::state_in(self.table.as_deref(), port)
    }

    /// Destino da porta.
    pub fn target(&self, port: u16, local_ips: &[Ipv4Addr]) -> RedirectTarget {
        if !self.env.is_wsl() {
            return RedirectTarget::None;
        }
        target_of(self.table.as_deref(), port, local_ips)
    }
}

/// Quando esperar a regra de firewall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallNeed {
    /// Não consulta.
    Skip,
    /// Sempre espera o resultado.
    Wait,
    /// Consulta em paralelo, mas só espera quando a adoção é possível pelo resto (WSL NAT e a
    /// `port` redirecionada para este WSL); senão, segue sem esperar o PowerShell.
    IfAdoptable { port: u16 },
}

/// Consultas a fazer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Needs {
    pub env: bool,
    pub table: bool,
    pub firewall: FirewallNeed,
}

impl Needs {
    pub const ALL: Needs = Needs { env: true, table: true, firewall: FirewallNeed::Wait };
}

fn spawn_query<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

fn wait_until<T>(rx: Option<mpsc::Receiver<T>>, deadline: Instant) -> Option<T> {
    rx?.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()
}

/// Consulta o Windows em paralelo, com prazo total `deadline`. Fora do WSL não executa nada.
/// Bloqueante: no código assíncrono, chame dentro de `spawn_blocking`.
pub fn probe(tools: &WinTools, wsl: bool, needs: Needs, local_ips: &[Ipv4Addr], deadline: Duration) -> WindowsView {
    if !wsl {
        return WindowsView { env: NetworkEnv::Linux, table: Some(Vec::new()), firewall: FirewallState::Absent };
    }
    let until = Instant::now() + deadline;
    let env_rx = needs.env.then(|| {
        let t = tools.clone();
        spawn_query(move || detect_env(&t, true))
    });
    let table_rx = needs.table.then(|| {
        let t = tools.clone();
        spawn_query(move || portproxy::read_table(&t, true))
    });
    let fw_rx = (needs.firewall != FirewallNeed::Skip).then(|| {
        let t = tools.clone();
        spawn_query(move || firewall_with(&t, true))
    });
    let env = wait_until(env_rx, until).unwrap_or(NetworkEnv::WslUnknown);
    let table = wait_until(table_rx, until).flatten();
    let wait_firewall = match needs.firewall {
        FirewallNeed::Skip => false,
        FirewallNeed::Wait => true,
        FirewallNeed::IfAdoptable { port } => {
            env == NetworkEnv::WslNat && target_of(table.as_deref(), port, local_ips) == RedirectTarget::ThisWsl
        }
    };
    let firewall = if wait_firewall { wait_until(fw_rx, until).unwrap_or(FirewallState::Unknown) } else { FirewallState::Unknown };
    WindowsView { env, table, firewall }
}

/// Adoção (decisão 3): WSL NAT, a porta redirecionada para este WSL e a regra "Nants" presente.
pub fn adoption_applies(env: NetworkEnv, target: RedirectTarget, firewall: FirewallState) -> bool {
    env == NetworkEnv::WslNat && target == RedirectTarget::ThisWsl && firewall == FirewallState::Present
}

/// Resultado da comparação entre a opção salva e o Windows (spec 111): a regra única do item do
/// `nants doctor` ([`super::doctor::network_item`]) e do campo `coherence` da API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkCheck {
    /// Linux direto: só informa a opção.
    LinuxOn,
    LinuxOff,
    /// Ligada e o Windows de acordo (NAT: redirecionada para este WSL; espelhado: regra presente).
    OnOk,
    OnMirroredOk,
    /// Ligada no NAT sem redirecionamento: falta rodar o script de ligar.
    OnNoRedirect,
    /// Ligada no NAT, redirecionada para outro endereço: rodar o script de ligar de novo.
    OnOtherTarget,
    /// Ligada sem a regra de firewall "Nants".
    OnNoFirewall,
    /// Nada salvo e o Windows já configurado para este WSL: o próximo início liga (adoção).
    WillAdopt,
    /// Desligada (ou nada salvo sem adoção) com a porta redirecionada: rodar o script de desfazer.
    RedirectOn,
    /// Desligada e sem redirecionamento.
    RedirectOff,
    /// Não deu para conferir o redirecionamento ou a regra.
    RedirectUnknown,
    FirewallUnknown,
}

/// Nível do item do `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckLevel {
    Ok,
    Warn,
    Info,
}

impl NetworkCheck {
    /// Chave do detalhe do `nants doctor`.
    pub fn detail_key(self) -> &'static str {
        match self {
            NetworkCheck::LinuxOn => "doctor.detail.lan_linux_on",
            NetworkCheck::LinuxOff => "doctor.detail.lan_linux_off",
            NetworkCheck::OnOk => "doctor.detail.lan_on_ok",
            NetworkCheck::OnMirroredOk => "doctor.detail.lan_on_mirrored_ok",
            NetworkCheck::OnNoRedirect => "doctor.detail.lan_on_no_redirect",
            NetworkCheck::OnOtherTarget => "doctor.detail.lan_on_other_target",
            NetworkCheck::OnNoFirewall => "doctor.detail.lan_on_no_firewall",
            NetworkCheck::WillAdopt => "doctor.detail.lan_will_adopt",
            NetworkCheck::RedirectOn => "doctor.detail.windows_redirect_on",
            NetworkCheck::RedirectOff => "doctor.detail.windows_redirect_off",
            NetworkCheck::RedirectUnknown => "doctor.detail.windows_redirect_unknown",
            NetworkCheck::FirewallUnknown => "doctor.detail.firewall_unknown",
        }
    }

    /// O detalhe cita a porta.
    pub fn has_port(self) -> bool {
        !matches!(self, NetworkCheck::RedirectUnknown | NetworkCheck::FirewallUnknown)
    }

    pub fn level(self) -> CheckLevel {
        match self {
            NetworkCheck::LinuxOn | NetworkCheck::LinuxOff | NetworkCheck::RedirectUnknown | NetworkCheck::FirewallUnknown => {
                CheckLevel::Info
            }
            NetworkCheck::OnOk | NetworkCheck::OnMirroredOk | NetworkCheck::WillAdopt | NetworkCheck::RedirectOff => CheckLevel::Ok,
            NetworkCheck::OnNoRedirect | NetworkCheck::OnOtherTarget | NetworkCheck::OnNoFirewall | NetworkCheck::RedirectOn => {
                CheckLevel::Warn
            }
        }
    }

    /// Forma da API (`coherence`): `ok`, `will_adopt`, `needs_script`, `other_target`,
    /// `missing_rule`, `still_redirected` ou `unknown`.
    pub fn coherence(self) -> &'static str {
        match self {
            NetworkCheck::LinuxOn | NetworkCheck::LinuxOff | NetworkCheck::OnOk | NetworkCheck::OnMirroredOk | NetworkCheck::RedirectOff => "ok",
            NetworkCheck::WillAdopt => "will_adopt",
            NetworkCheck::OnNoRedirect => "needs_script",
            NetworkCheck::OnOtherTarget => "other_target",
            NetworkCheck::OnNoFirewall => "missing_rule",
            NetworkCheck::RedirectOn => "still_redirected",
            NetworkCheck::RedirectUnknown | NetworkCheck::FirewallUnknown => "unknown",
        }
    }
}

/// A opção salva comparada com o Windows (só leitura). Nada salvo conta como desligada, salvo a
/// adoção ([`adoption_applies`]), que o próximo início faz sozinho.
pub fn network_check(env: NetworkEnv, saved: Option<bool>, target: RedirectTarget, firewall: FirewallState) -> NetworkCheck {
    let on = saved == Some(true);
    if env == NetworkEnv::Linux {
        return if on { NetworkCheck::LinuxOn } else { NetworkCheck::LinuxOff };
    }
    if on {
        if env == NetworkEnv::WslMirrored {
            return match firewall {
                FirewallState::Present => NetworkCheck::OnMirroredOk,
                FirewallState::Absent => NetworkCheck::OnNoFirewall,
                FirewallState::Unknown => NetworkCheck::FirewallUnknown,
            };
        }
        return match target {
            RedirectTarget::Unknown => NetworkCheck::RedirectUnknown,
            RedirectTarget::None => NetworkCheck::OnNoRedirect,
            RedirectTarget::Other => NetworkCheck::OnOtherTarget,
            RedirectTarget::ThisWsl if firewall == FirewallState::Absent => NetworkCheck::OnNoFirewall,
            RedirectTarget::ThisWsl => NetworkCheck::OnOk,
        };
    }
    match target {
        RedirectTarget::ThisWsl | RedirectTarget::Other => {
            if saved.is_none() && adoption_applies(env, target, firewall) {
                NetworkCheck::WillAdopt
            } else {
                NetworkCheck::RedirectOn
            }
        }
        RedirectTarget::None => NetworkCheck::RedirectOff,
        RedirectTarget::Unknown => NetworkCheck::RedirectUnknown,
    }
}

/// De onde veio o modo desta execução.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeSource {
    /// `--lan` na linha de comando (só nesta execução).
    Flag,
    /// `--host` explícito (modo local ou o endereço pedido).
    Host,
    /// A opção salva `[server] lan`.
    Saved,
    /// Adoção do que o Windows já tem.
    Adopted,
    /// Nenhuma escolha: modo local.
    Default,
}

impl ModeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ModeSource::Flag => "flag",
            ModeSource::Host => "host",
            ModeSource::Saved => "saved",
            ModeSource::Adopted => "adopted",
            ModeSource::Default => "default",
        }
    }
}

/// Modo decidido ao subir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeDecision {
    pub lan: bool,
    pub source: ModeSource,
}

/// Decisão do modo (critério 4): `--lan` ou `--host` explícitos vencem; senão, a opção salva;
/// senão, a adoção; senão, o modo local. `adoptable` é o resultado de [`adoption_applies`] e só
/// conta sem escolha salva.
pub fn decide_mode(flag_lan: bool, explicit_host: bool, saved: Option<bool>, adoptable: bool) -> ModeDecision {
    let (lan, source) = if flag_lan {
        (true, ModeSource::Flag)
    } else if explicit_host {
        (false, ModeSource::Host)
    } else if let Some(saved) = saved {
        (saved, ModeSource::Saved)
    } else if adoptable {
        (true, ModeSource::Adopted)
    } else {
        (false, ModeSource::Default)
    };
    ModeDecision { lan, source }
}

/// Consultas necessárias ao subir: com `--lan`, `--host` ou a opção ligada, nenhuma; com a opção
/// desligada, só a tabela (para pular a porta redirecionada); sem escolha, tudo, mas o PowerShell
/// só é esperado quando a adoção é possível.
pub fn startup_needs(flag_lan: bool, explicit_host: bool, saved: Option<bool>, port: u16) -> Option<Needs> {
    if flag_lan || explicit_host || saved == Some(true) {
        return None;
    }
    if saved == Some(false) {
        return Some(Needs { env: false, table: true, firewall: FirewallNeed::Skip });
    }
    Some(Needs { env: true, table: true, firewall: FirewallNeed::IfAdoptable { port } })
}

/// Portas de escuta redirecionadas pelo Windows (vazio quando não deu para conferir: nada muda).
pub fn redirected_ports(table: Option<&[Redirect]>) -> Vec<u16> {
    table.map(|t| t.iter().map(|r| r.listen_port).collect()).unwrap_or_default()
}

/// Portas a tentar na escolha automática: `port` e as `tries` seguintes, sem as redirecionadas.
pub fn candidate_ports(port: u16, tries: u16, redirected: &[u16]) -> Vec<u16> {
    (port..=port.saturating_add(tries)).filter(|p| !redirected.contains(p)).collect()
}

/// Modo desta execução, guardado no estado para a API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkRun {
    pub lan: bool,
    pub source: ModeSource,
    /// Porta pedida (`--port` ou a configurada): a do acesso pela rede e dos scripts.
    pub requested_port: u16,
}

/// Conferência do Windows para a API (injetável nos testes).
#[derive(Clone)]
pub struct NetProbe {
    pub tools: WinTools,
    pub wsl: bool,
    /// IPv4 deste WSL; `None` lê as interfaces na hora.
    pub local_ips: Option<Vec<Ipv4Addr>>,
}

impl std::fmt::Debug for NetProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetProbe").field("wsl", &self.wsl).finish()
    }
}

impl NetProbe {
    pub fn real() -> Self {
        Self { tools: WinTools::real(), wsl: super::netinfo::running_on_wsl(), local_ips: None }
    }

    pub fn ips(&self) -> Vec<Ipv4Addr> {
        self.local_ips.clone().unwrap_or_else(super::netinfo::local_ipv4_addrs)
    }
}

/// Estado de rede para a interface (critério 7).
pub fn network_view(
    view: &WindowsView,
    saved: Option<bool>,
    run: NetworkRun,
    bound_port: u16,
    configured_view_ports: Option<crate::config::LanViewPortsConfig>,
    local_ips: &[Ipv4Addr],
) -> Value {
    let lan_port = lan_port(run, bound_port);
    let view_ports = crate::config::ViewPorts::resolve(configured_view_ports, lan_port)
        .ok()
        .map(|r| json!({"start": r.start, "end": r.end}));
    let target = view.target(lan_port, local_ips);
    let coherence = network_check(view.env, saved, target, view.firewall).coherence();
    json!({
        "env": view.env.as_str(),
        "saved_lan": saved,
        "lan": run.lan,
        "mode_source": run.source.as_str(),
        "port": bound_port,
        "lan_port": lan_port,
        "view_ports": view_ports,
        "windows_redirect": view.redirect(lan_port).as_str(),
        "windows_redirect_target": target.as_str(),
        "coherence": coherence,
        "firewall_rule": view.firewall.as_str(),
        "restart_required": saved.unwrap_or(false) != run.lan,
        "restart_command": RESTART_COMMAND,
    })
}

/// Porta do Nants na rede: a pedida ao subir (a ligada pode ter pulado uma porta redirecionada) ou,
/// com a porta escolhida pelo sistema, a ligada.
pub fn lan_port(run: NetworkRun, bound_port: u16) -> u16 {
    if run.requested_port == 0 {
        bound_port
    } else {
        run.requested_port
    }
}

/// `GET /api/config/network`: ambiente, opção salva, modo desta execução, portas e o que o
/// Windows mostra (só leitura, em paralelo e com prazo curto).
pub async fn get_network(State(state): State<AppState>) -> Response {
    let (saved, configured) = {
        let cfg = state.config().read().await;
        (cfg.server.lan, cfg.server.lan_view_ports)
    };
    let probe = state.net_probe().clone();
    let view = tokio::task::spawn_blocking(move || {
        let ips = probe.ips();
        let view = self::probe(&probe.tools, probe.wsl, Needs::ALL, &ips, PROBE_DEADLINE);
        (view, ips)
    })
    .await;
    let (view, ips) = match view {
        Ok(v) => v,
        Err(_) => {
            let env = if state.net_probe().wsl { NetworkEnv::WslUnknown } else { NetworkEnv::Linux };
            (WindowsView { env, table: None, firewall: FirewallState::Unknown }, Vec::new())
        }
    };
    Json(network_view(&view, saved, state.network_run(), state.bound_port(), configured, &ips)).into_response()
}

/// Corpo de `PUT /api/config/network`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkInput {
    lan: bool,
}

/// `PUT /api/config/network` `{lan}`: grava `[server] lan` preservando o resto da configuração. Vale
/// a partir do próximo início (a resposta diz isso e traz o comando para reiniciar).
pub async fn put_network(State(state): State<AppState>, body: Bytes) -> Result<Response, ApiError> {
    let input: NetworkInput = super::config_api::parse_body(&body)?;
    let mut guard = state.config().write().await;
    let mut next = guard.clone();
    next.server.lan = Some(input.lan);
    super::config_api::persist(&state, &next).await?;
    *guard = next;
    let run = state.network_run();
    Ok(Json(json!({
        "saved_lan": input.lan,
        "lan": run.lan,
        "applies_on_next_start": true,
        "restart_required": input.lan != run.lan,
        "restart_command": RESTART_COMMAND,
    }))
    .into_response())
}

#[cfg(test)]
#[path = "lan_tests.rs"]
mod tests;
