pub mod agents;
pub mod config;
pub mod harness;
pub mod i18n;
pub mod kb;
pub mod sdd;
pub mod server;
pub mod uninstall;

/// Retorna a versão do crate `nants`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_package_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        assert!(!version().is_empty());
    }
}
