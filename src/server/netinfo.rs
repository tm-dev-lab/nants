//! Endereços IPv4 locais (via `getifaddrs`), usados no modo `--lan`.

use std::net::Ipv4Addr;

/// IPv4 locais não-loopback das interfaces ativas, com o nome da interface.
pub fn local_ipv4_ifaces() -> Vec<(String, Ipv4Addr)> {
    let mut out: Vec<(String, Ipv4Addr)> = Vec::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs preenche a lista, percorrida só leitura e liberada com freeifaddrs.
    unsafe {
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            let up = ifa.ifa_flags & (libc::IFF_UP as u32) != 0;
            if up && !ifa.ifa_addr.is_null() && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if !ip.is_loopback() && !out.iter().any(|(_, o)| *o == ip) {
                    let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                    out.push((name, ip));
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    out
}

/// IPv4 locais não-loopback das interfaces ativas.
pub fn local_ipv4_addrs() -> Vec<Ipv4Addr> {
    local_ipv4_ifaces().into_iter().map(|(_, ip)| ip).collect()
}

/// IP da interface principal (a `eth0` do WSL; senão o primeiro não-loopback).
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    let all = local_ipv4_ifaces();
    all.iter().find(|(n, _)| n == "eth0").or_else(|| all.first()).map(|(_, ip)| *ip)
}

/// O servidor roda no WSL? Decide pelo conteúdo de `/proc/sys/kernel/osrelease` e de
/// `/proc/version` (qualquer um contendo "microsoft", sem distinguir maiúsculas). Função pura:
/// recebe o conteúdo já lido (`None` = arquivo ausente ou ilegível).
pub fn is_wsl_from(osrelease: Option<&str>, version: Option<&str>) -> bool {
    [osrelease, version].into_iter().flatten().any(|t| t.to_ascii_lowercase().contains("microsoft"))
}

/// Detecção do WSL no servidor (spec 097), lida uma vez por processo. Só o booleano sai daqui:
/// nenhum endereço do WSL é lido nem exposto.
pub fn running_on_wsl() -> bool {
    static WSL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WSL.get_or_init(|| {
        let read = |p: &str| std::fs::read_to_string(p).ok();
        is_wsl_from(read("/proc/sys/kernel/osrelease").as_deref(), read("/proc/version").as_deref())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_detection_reads_osrelease_or_version_case_insensitive() {
        assert!(is_wsl_from(Some("6.6.87.2-microsoft-standard-WSL2\n"), None));
        assert!(is_wsl_from(None, Some("Linux version 4.4.0-19041-Microsoft (Microsoft@Microsoft.com)")));
        assert!(is_wsl_from(Some("5.15.0-generic"), Some("Linux version ... MICROSOFT ...")));
        assert!(!is_wsl_from(Some("6.8.0-45-generic\n"), Some("Linux version 6.8.0-45-generic (buildd@lcy02)")));
        assert!(!is_wsl_from(None, None));
        assert!(!is_wsl_from(Some(""), Some("")));
    }

    #[test]
    fn local_ipv4_addrs_excludes_loopback() {
        assert!(local_ipv4_addrs().iter().all(|ip| !ip.is_loopback()));
    }
}
