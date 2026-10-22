//! `docker` falso para os testes que sobem o binário (spec 113 T01).
//!
//! O servidor chama `docker ps` na varredura de órfãos do startup. Nenhum teste pode alcançar o
//! Docker real do host (um `down -v` com nome de projeto de teste apagaria volumes reais): os
//! filhos recebem no PATH uma pasta com um `docker` que só falha. A pasta fica no `TMPDIR` da
//! rodada (`scripts/test.sh` apaga no fim).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Pasta com o `docker` falso, uma por processo de teste.
fn fake_docker_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("nants-fake-docker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("pasta do docker falso");
        let script = dir.join("docker");
        std::fs::write(&script, "#!/bin/sh\necho 'docker falso dos testes: o Docker real nunca é usado' >&2\nexit 1\n")
            .expect("docker falso");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        dir
    })
}

/// Põe o `docker` falso à frente do PATH do filho.
pub fn without_real_docker(cmd: &mut Command) -> &mut Command {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![fake_docker_dir().to_path_buf()];
    dirs.extend(std::env::split_paths(&path));
    cmd.env("PATH", std::env::join_paths(dirs).expect("PATH"))
}
