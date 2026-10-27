//! Pasta pessoal persistente da área de trabalho (spec 129 T01).
//!
//! Cada comando do workspace é um container novo (`run --rm`). Com `HOME` fora de `/work`, tudo o
//! que um comando grava na pasta pessoal (instalação de usuário, executáveis, configuração de
//! ferramenta) some no comando seguinte, em qualquer ecossistema. Aqui o `HOME` do container é
//! [`CONTAINER_HOME`], uma pasta dentro da área de trabalho montada em `/work`:
//! - criada pelo processo do Nants (dono = uid do host) antes do primeiro comando ([`ensure`]);
//! - fora do Git: `.gitignore` próprio com `*` e a linha [`EXCLUDE_LINE`] no `.git/info/exclude`;
//! - o `.profile` dela põe `$HOME/.local/bin` na frente do `PATH` dos shells de login (`sh -lc`),
//!   sem mexer no `PATH` da imagem;
//! - ciclo de vida igual ao da instalação da pasta: [`reset`] apaga tudo menos [`KEEP_ON_RESET`]
//!   quando o estado da instalação é esquecido (rodada limpa, instalação que falhou), e a remoção
//!   da worktree a leva junto.
//!
//! Nenhuma regra por linguagem: o Nants não sabe o que as ferramentas gravam aqui. Também não grava
//! credencial nenhuma na pasta.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Nome reservado da pasta, na raiz da área de trabalho.
pub const HOME_DIR: &str = ".nants-home";
/// O `HOME` dos containers da área de trabalho (`/work` é a raiz montada).
pub const CONTAINER_HOME: &str = "/work/.nants-home";
/// Linha do `.git/info/exclude` gravada pelo Nants.
pub const EXCLUDE_LINE: &str = ".nants-home/";
/// Única entrada mantida por [`reset`]: cache de download (não satisfaz importação nenhuma, só
/// evita baixar de novo).
pub const KEEP_ON_RESET: &str = ".cache";
/// Arquivo lido pelos shells de login (`sh -lc`) da pasta pessoal.
const PROFILE: &str = ".profile";
/// Linha do `.profile`: `$HOME/.local/bin` na frente do `PATH`, uma vez só.
pub const PATH_LINE: &str = r#"case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) PATH="$HOME/.local/bin:$PATH"; export PATH ;; esac"#;

/// `true` se o primeiro componente de `rel` (relativo, `/`) é a pasta pessoal.
pub fn is_home_path(rel: &str) -> bool {
    rel.trim_start_matches("./").split('/').next() == Some(HOME_DIR)
}

/// Garante a pasta pessoal em `dir` com o `.gitignore` (`*`) e o `.profile` com [`PATH_LINE`].
/// Recusa symlink ou entrada que não seja diretório no lugar dela. Idempotente; o `.profile` que
/// uma ferramenta já alterou só recebe a linha que falta, no fim.
pub fn ensure(dir: &Path) -> io::Result<PathBuf> {
    let home = dir.join(HOME_DIR);
    match fs::symlink_metadata(&home) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(io::Error::other(format!("{HOME_DIR} não é um diretório"))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match DirBuilder::new().mode(0o755).create(&home) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        },
        Err(e) => return Err(e),
    }
    write_new(&home.join(".gitignore"), "*\n")?;
    let profile = home.join(PROFILE);
    match fs::symlink_metadata(&profile) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => write_new(&profile, &format!("{PATH_LINE}\n"))?,
        Ok(m) if m.is_file() => {
            let mut text = String::new();
            OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&profile)?.read_to_string(&mut text)?;
            if !text.lines().any(|l| l.trim() == PATH_LINE) {
                let mut f = OpenOptions::new().append(true).custom_flags(libc::O_NOFOLLOW).open(&profile)?;
                let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
                f.write_all(format!("{sep}{PATH_LINE}\n").as_bytes())?;
            }
        }
        // Symlink ou outra coisa: não segue nem substitui.
        Ok(_) => {}
        Err(e) => return Err(e),
    }
    Ok(home)
}

/// Apaga o conteúdo da pasta pessoal de `dir`, menos [`KEEP_ON_RESET`], sem seguir symlink, e
/// recria só o que é do Nants (`.gitignore` e `.profile`). Sem a pasta (ou com um symlink no
/// lugar), não faz nada.
pub fn reset(dir: &Path) -> io::Result<()> {
    let home = dir.join(HOME_DIR);
    match fs::symlink_metadata(&home) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    for entry in fs::read_dir(&home)? {
        let entry = entry?;
        if entry.file_name() == KEEP_ON_RESET {
            continue;
        }
        // `file_type` da entrada não segue symlink; `remove_dir_all` também não.
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    // O `.gitignore` e o `.profile` do Nants voltam já: o cache mantido nunca aparece no Git.
    ensure(dir).map(|_| ())
}

/// Cria `path` com `text`, sem sobrescrever o que já existe (nem seguir symlink).
fn write_new(path: &Path, text: &str) -> io::Result<()> {
    match OpenOptions::new().write(true).create_new(true).mode(0o644).open(path) {
        Ok(mut f) => f.write_all(text.as_bytes()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nants-home-t-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("mkdir");
        d
    }

    #[test]
    fn ensure_creates_the_folder_owned_by_the_host_with_ignore_and_profile() {
        let d = dir("ensure");
        let home = ensure(&d).expect("ensure");
        assert_eq!(home, d.join(HOME_DIR));
        let uid = fs::metadata("/proc/self").expect("self").uid();
        assert_eq!(fs::metadata(&home).expect("meta").uid(), uid);
        assert_eq!(fs::read_to_string(home.join(".gitignore")).expect("gi"), "*\n");
        assert_eq!(fs::read_to_string(home.join(".profile")).expect("profile"), format!("{PATH_LINE}\n"));
        // Idempotente; o `.profile` alterado por uma ferramenta só ganha a linha que falta.
        fs::write(home.join(".profile"), ". \"$HOME/.tool/env\"").expect("w");
        ensure(&d).expect("de novo");
        ensure(&d).expect("de novo");
        let text = fs::read_to_string(home.join(".profile")).expect("profile");
        assert_eq!(text, format!(". \"$HOME/.tool/env\"\n{PATH_LINE}\n"));
        fs::remove_dir_all(&d).expect("rm");
    }

    #[test]
    fn ensure_refuses_a_symlink_or_file_in_place_of_the_folder() {
        let d = dir("symlink");
        let target = dir("symlink-target");
        std::os::unix::fs::symlink(&target, d.join(HOME_DIR)).expect("ln");
        assert!(ensure(&d).is_err());
        assert!(!target.join(".gitignore").exists());
        fs::remove_file(d.join(HOME_DIR)).expect("rm");
        fs::write(d.join(HOME_DIR), "x").expect("w");
        assert!(ensure(&d).is_err());
        // `reset` não segue o symlink nem apaga o arquivo.
        assert!(reset(&d).is_ok());
        assert!(d.join(HOME_DIR).is_file());
        fs::remove_dir_all(&d).expect("rm");
        fs::remove_dir_all(&target).expect("rm");
    }

    #[test]
    fn reset_keeps_only_the_download_cache_and_never_follows_links() {
        let d = dir("reset");
        let outside = dir("reset-outside");
        fs::write(outside.join("keep.txt"), "fora").expect("w");
        let home = ensure(&d).expect("ensure");
        fs::create_dir_all(home.join(".local/bin")).expect("mk");
        fs::write(home.join(".local/bin/tool"), "#!/bin/sh\n").expect("w");
        fs::create_dir_all(home.join(".cache/dl")).expect("mk");
        fs::write(home.join(".cache/dl/pkg"), "bytes").expect("w");
        std::os::unix::fs::symlink(&outside, home.join("link")).expect("ln");
        reset(&d).expect("reset");
        let mut left: Vec<String> = fs::read_dir(&home).expect("ls").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec![".cache", ".gitignore", ".profile"]);
        assert!(home.join(".cache/dl/pkg").exists());
        assert!(outside.join("keep.txt").exists());
        // Sem a pasta, nada a fazer.
        fs::remove_dir_all(&home).expect("rm");
        assert!(reset(&d).is_ok());
        fs::remove_dir_all(&d).expect("rm");
        fs::remove_dir_all(&outside).expect("rm");
    }

    #[test]
    fn home_path_is_the_first_component_only() {
        assert!(is_home_path(".nants-home/.local/bin/x"));
        assert!(is_home_path("./.nants-home"));
        assert!(!is_home_path("src/.nants-home/x"));
        assert!(!is_home_path(".nants-homes/x"));
    }
}
