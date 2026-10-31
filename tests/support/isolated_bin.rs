//! Único ponto que sobe o binário `nants` nos testes de integração (spec 138 T13).
//!
//! Todo filho recebe `HOME`, `XDG_DATA_HOME` e `XDG_CONFIG_HOME` dentro de uma pasta temporária:
//! nenhum teste lê ou grava a configuração e os dados reais do usuário (`~/.config/nants`,
//! `~/.local/share/nants`), nem segura a trava da instalação real do Nanty. A trava
//! `tests/isolated_bin_guard.rs` falha se outro arquivo de `tests/` citar o binário diretamente.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Pasta de configuração (`XDG_CONFIG_HOME`) do filho, dentro de `home`.
pub fn config_home(home: &Path) -> PathBuf {
    home.join(".config")
}

/// Pasta de dados (`XDG_DATA_HOME`) do filho, dentro de `home`.
pub fn data_home(home: &Path) -> PathBuf {
    home.join(".local").join("share")
}

fn isolate(cmd: &mut Command, home: &Path) {
    cmd.env("HOME", home).env("XDG_CONFIG_HOME", config_home(home)).env("XDG_DATA_HOME", data_home(home));
}

/// `nants` com `HOME` e XDG em `home` (uma pasta temporária do teste), herdando o resto do ambiente.
pub fn nants(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nants"));
    isolate(&mut cmd, home);
    cmd
}

/// Como [`nants`], mas com o ambiente limpo: só `HOME` e XDG; o teste acrescenta o que precisar.
pub fn nants_env_clear(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nants"));
    cmd.env_clear();
    isolate(&mut cmd, home);
    cmd
}
