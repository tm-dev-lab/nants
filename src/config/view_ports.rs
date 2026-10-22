//! Faixa das portas de visualização do app em `--lan` (spec 097, decisões 1 e 2).
//!
//! Em `--lan`, cada porta publicada pelo serviço `workspace` do `-run` sai desta faixa, para que
//! uma liberação única na rede do computador cubra todos os apps. O padrão são as
//! [`DEFAULT_VIEW_PORTS`] portas seguintes à porta efetiva do Nants; `lan_view_ports` muda a faixa.

use super::LanViewPortsConfig;

/// Tamanho da faixa padrão.
pub const DEFAULT_VIEW_PORTS: u16 = 10;
/// Tamanho máximo da faixa.
pub const MAX_VIEW_PORTS: u16 = 100;
/// Menor porta aceita na faixa (fora das portas privilegiadas).
pub const MIN_VIEW_PORT: u16 = 1024;

/// Começo fixo da mensagem de faixa esgotada (reconhecida no caminho do conflito de porta).
pub const VIEW_PORTS_EXHAUSTED_PREFIX: &str = "Todas as portas de visualização (";

/// Faixa resolvida e validada, com as duas pontas inclusas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewPorts {
    pub start: u16,
    pub end: u16,
}

impl ViewPorts {
    /// Faixa em uso: a configurada ou, sem configuração, a padrão derivada de `nants_port`.
    pub fn resolve(configured: Option<LanViewPortsConfig>, nants_port: u16) -> Result<Self, String> {
        match configured {
            Some(c) => Self::validate(c.start, c.count, nants_port).map_err(|e| format!("lan_view_ports inválida: {e}")),
            None => Self::validate(u32::from(nants_port) + 1, DEFAULT_VIEW_PORTS, nants_port).map_err(|e| {
                format!("faixa padrão das portas de visualização inválida para a porta {nants_port}: {e}; configure lan_view_ports em [server]")
            }),
        }
    }

    /// Faixa padrão derivada da porta, se válida.
    pub fn default_for(nants_port: u16) -> Option<Self> {
        Self::resolve(None, nants_port).ok()
    }

    fn validate(start: impl Into<u32>, count: u16, nants_port: u16) -> Result<Self, String> {
        let start: u32 = start.into();
        if count == 0 || count > MAX_VIEW_PORTS {
            return Err(format!("o tamanho da faixa precisa ficar entre 1 e {MAX_VIEW_PORTS} (recebido {count})"));
        }
        let end = start + u32::from(count) - 1;
        if start < u32::from(MIN_VIEW_PORT) || end > u32::from(u16::MAX) {
            return Err(format!("a faixa {start} a {end} precisa ficar entre {MIN_VIEW_PORT} e {}", u16::MAX));
        }
        if (start..=end).contains(&u32::from(nants_port)) {
            return Err(format!("a faixa {start} a {end} inclui a porta do Nants ({nants_port})"));
        }
        let (Ok(start), Ok(end)) = (u16::try_from(start), u16::try_from(end)) else {
            return Err("faixa fora do intervalo de portas".into());
        };
        Ok(Self { start, end })
    }

    /// Portas da faixa, em ordem.
    pub fn ports(&self) -> std::ops::RangeInclusive<u16> {
        self.start..=self.end
    }

    /// Mensagem da faixa esgotada (decisão 3), com os números reais.
    pub fn exhausted_message(&self) -> String {
        format!(
            "{VIEW_PORTS_EXHAUSTED_PREFIX}{} a {}) estão em uso. Pare outro app na Execução e tente de novo.",
            self.start, self.end
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(start: u16, count: u16) -> Option<LanViewPortsConfig> {
        Some(LanViewPortsConfig { start, count })
    }

    #[test]
    fn default_range_follows_effective_port() {
        assert_eq!(ViewPorts::resolve(None, 3000), Ok(ViewPorts { start: 3001, end: 3010 }));
        assert_eq!(ViewPorts::resolve(None, 4000), Ok(ViewPorts { start: 4001, end: 4010 }));
        assert_eq!(ViewPorts::default_for(8080), Some(ViewPorts { start: 8081, end: 8090 }));
        // Perto do teto ou abaixo de 1024: erro claro em vez de faixa truncada.
        let high = ViewPorts::resolve(None, 65530).expect_err("passa de 65535");
        assert!(high.contains("lan_view_ports"), "{high}");
        assert!(ViewPorts::resolve(None, 80).is_err());
        assert_eq!(ViewPorts::default_for(65535), None);
    }

    #[test]
    fn configured_range_is_validated() {
        assert_eq!(ViewPorts::resolve(cfg(5000, 5), 3000), Ok(ViewPorts { start: 5000, end: 5004 }));
        assert_eq!(ViewPorts::resolve(cfg(65535, 1), 3000), Ok(ViewPorts { start: 65535, end: 65535 }));
        assert_eq!(ViewPorts::resolve(cfg(1024, 100), 3000), Ok(ViewPorts { start: 1024, end: 1123 }));
        let with_nants = ViewPorts::resolve(cfg(2995, 10), 3000).expect_err("inclui a porta");
        assert!(with_nants.contains("porta do Nants (3000)"), "{with_nants}");
        assert!(ViewPorts::resolve(cfg(3000, 1), 3000).is_err());
        assert!(ViewPorts::resolve(cfg(1023, 5), 3000).expect_err("baixa").contains("entre 1024 e 65535"));
        assert!(ViewPorts::resolve(cfg(65530, 10), 3000).is_err());
        assert!(ViewPorts::resolve(cfg(5000, 0), 3000).expect_err("vazia").contains("entre 1 e 100"));
        assert!(ViewPorts::resolve(cfg(5000, 101), 3000).is_err());
    }

    #[test]
    fn exhausted_message_uses_real_numbers() {
        let r = ViewPorts { start: 3001, end: 3010 };
        assert_eq!(
            r.exhausted_message(),
            "Todas as portas de visualização (3001 a 3010) estão em uso. Pare outro app na Execução e tente de novo."
        );
        assert_eq!(r.ports().count(), 10);
    }

    #[test]
    fn config_toml_roundtrip_keeps_range_and_omits_when_absent() {
        let parsed: crate::config::Config =
            toml::from_str("[server]\nport = 3000\nlan_view_ports = { start = 4100, count = 20 }\n").expect("toml");
        assert_eq!(parsed.server.lan_view_ports, cfg(4100, 20));
        let text = toml::to_string(&parsed).expect("ser");
        assert!(text.contains("lan_view_ports"), "{text}");
        let plain = toml::to_string(&crate::config::Config::default()).expect("ser");
        assert!(!plain.contains("lan_view_ports"), "{plain}");
        assert!(toml::from_str::<crate::config::Config>("[server]\nlan_view_ports = { start = 4100 }\n").is_err());
    }
}
