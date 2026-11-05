//! Pasta pessoal persistente da área de trabalho (spec 129 T01; fora da árvore desde a spec 141 T01).
//!
//! Cada comando do workspace é um container novo (`run --rm`). Com o `HOME` dentro do container,
//! tudo o que um comando grava na pasta pessoal (instalação de usuário, executáveis, configuração
//! de ferramenta) some no comando seguinte, em qualquer ecossistema. Por isso o `HOME` do container
//! é [`CONTAINER_HOME`], um caminho reservado FORA de `/work`, montado por bind de uma pasta do
//! diretório de dados do Nants, uma por área de trabalho ([`host_home`]). Cada área de trabalho tem
//! um "lugar" próprio, que NÃO é montado, com a pasta pessoal `home` dentro (a única montada), a
//! marca da migração e as sobras da pasta antiga em migração:
//! - `<dados>/homes/<projeto>/root/home` para a raiz do projeto;
//! - `<dados>/homes/<projeto>/tasks/<spec>/<tarefa>/home` para a worktree de cada tarefa.
//!
//! Por que fora de `/work` (spec 141 T01): até a 141 ela ficava em `/work/.nants-home`, e o que as
//! ferramentas deixavam ali (o link quebrado do PulseAudio que o Chromium do Playwright grava, por
//! exemplo) entrava na varredura dos executores de teste (`node --test` sem caminho saía com erro
//! antes de rodar teste). Por que bind e não volume nomeado: o `prepare` faz `down -v` antes de todo
//! `test_command`, e um volume nasce do root.
//!
//! - Criada pelo processo do Nants (dono = uid do host, modo 0700) antes do primeiro comando
//!   ([`ready`]); o container roda com o mesmo uid e grava nela.
//! - O `.profile` dela põe `$HOME/.local/bin` na frente do `PATH` dos shells de login (`sh -lc`),
//!   sem mexer no `PATH` da imagem.
//! - Spec 133 T12: antes disso, o `.profile` devolve o `PATH` da imagem, que o `/etc/profile` de
//!   várias distribuições (Alpine, Debian) refaz nos shells de login e perde (`/usr/local/go/bin`, o
//!   `ENV PATH` do Dockerfile). O `PATH` da imagem vem do ambiente inicial do próprio shell
//!   (`/proc/$$/environ`, o que o `docker run` entregou: o `Env` da imagem mais o `environment` do
//!   Compose), sem tabela por distribuição e sem chamada extra ao Docker.
//! - Ciclo de vida igual ao da instalação da pasta: [`reset`] apaga tudo menos [`KEEP_ON_RESET`]
//!   quando o estado da instalação é esquecido (rodada limpa, instalação que falhou); [`remove`]
//!   a leva junto com a worktree; [`remove_project`] com o projeto; [`sweep_orphans`] no início do
//!   servidor apaga as que sobraram de projetos ou worktrees que não existem mais.
//! - Legado: a pasta antiga [`LEGACY_HOME_DIR`] dentro da área de trabalho é migrada por
//!   [`migrate_legacy`] (via [`ready`]), uma vez só por área de trabalho (marca [`MIGRATED`] no
//!   lugar dela, fora do alcance dos containers): primeiro ela sai do alcance do container por um
//!   `rename` atômico (que move um link como link, sem segui-lo), depois o cache de download vem, o
//!   resto sai, e o estado da instalação é esquecido, para a instalação rodar de novo no `HOME`
//!   novo. Um `.nants-home` criado depois da marca é só uma pasta do projeto.
//!
//! Nenhuma regra por linguagem: o Nants não sabe o que as ferramentas gravam aqui. Também não grava
//! credencial nenhuma na pasta.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

/// O `HOME` dos containers da área de trabalho: caminho reservado, fora de `/work`.
pub const CONTAINER_HOME: &str = "/nants/home";
/// Spec 141 T02: as worktrees das tarefas, relativas à raiz do projeto. Nas rodadas da raiz elas
/// ficam fora da vista do container (ver [`hidden_from_root`]).
pub const TASK_WORKTREES: &str = ".sdd/worktrees";
/// Pasta das pastas pessoais, no diretório de dados do Nants.
pub const HOMES_DIR: &str = "homes";
/// Pasta pessoal da raiz do projeto, em `<dados>/homes/<projeto>/`.
const ROOT_HOME: &str = "root";
/// Pastas pessoais das worktrees, em `<dados>/homes/<projeto>/tasks/<spec>/<tarefa>`.
const TASKS_HOMES: &str = "tasks";
/// A pasta pessoal (a montada), dentro do lugar da área de trabalho.
const HOME_SUBDIR: &str = "home";
/// Marca, no lugar da área de trabalho (não montado), de que a pasta antiga já foi tratada.
pub const MIGRATED: &str = "migrated";
/// Prefixo das pastas antigas tiradas da área de trabalho e ainda não apagadas, no lugar dela.
const MOVED_PREFIX: &str = "legacy-";
/// Nome da pasta pessoal antiga (spec 129 a 140), na raiz da área de trabalho. As exclusões dela no
/// código ficam enquanto houver legado; [`migrate_legacy`] a tira das áreas de trabalho.
pub const LEGACY_HOME_DIR: &str = ".nants-home";
/// Linha do `.git/info/exclude` da pasta antiga (fica enquanto houver legado).
pub const LEGACY_EXCLUDE_LINE: &str = ".nants-home/";
/// Única entrada mantida por [`reset`] e trazida da pasta antiga por [`migrate_legacy`]: cache de
/// download (não satisfaz importação nenhuma, só evita baixar de novo).
pub const KEEP_ON_RESET: &str = ".cache";
/// Arquivo lido pelos shells de login (`sh -lc`) da pasta pessoal.
const PROFILE: &str = ".profile";
/// Spec 133 T12: primeira linha do `.profile`. Lê o `PATH` do ambiente inicial do shell de login
/// (`/proc/$$/environ`: o que o contêiner recebeu, antes do `/etc/profile`), põe esse `PATH` na
/// frente e mantém depois dele, uma vez só, as pastas que o `/etc/profile` acrescentou. Sem o
/// arquivo, sem `tr`/`sed` ou sem `PATH` nele, não muda nada. Só variáveis `nants_*`, apagadas no fim.
pub const IMAGE_PATH_LINE: &str = r#"nants_ip=$(tr '\000' '\n' < "/proc/$$/environ" 2>/dev/null | sed -n 's/^PATH=//p' | head -n 1); if [ -n "$nants_ip" ]; then nants_r=$PATH; PATH=$nants_ip; while [ -n "$nants_r" ]; do nants_d=${nants_r%%:*}; case "$nants_r" in *:*) nants_r=${nants_r#*:} ;; *) nants_r= ;; esac; if [ -n "$nants_d" ]; then case ":$PATH:" in *":$nants_d:"*) ;; *) PATH="$PATH:$nants_d" ;; esac; fi; done; export PATH; fi; unset nants_ip nants_r nants_d"#;
/// Linha do `.profile`: `$HOME/.local/bin` na frente do `PATH`, uma vez só.
pub const PATH_LINE: &str = r#"case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) PATH="$HOME/.local/bin:$PATH"; export PATH ;; esac"#;

/// Diretório das pastas pessoais: `<dados do Nants>/homes`. Nos testes, uma pasta temporária
/// própria do processo (nunca a pasta de dados real do usuário).
pub fn homes_base() -> Option<PathBuf> {
    #[cfg(test)]
    {
        Some(std::env::temp_dir().join(format!("nants-test-homes-{}", std::process::id())))
    }
    #[cfg(not(test))]
    {
        crate::config::data_dir().map(|d| d.join(HOMES_DIR))
    }
}

/// `true` se o primeiro componente de `rel` (relativo, `/`) é a pasta pessoal antiga.
pub fn is_legacy_home_path(rel: &str) -> bool {
    rel.trim_start_matches("./").split('/').next() == Some(LEGACY_HOME_DIR)
}

/// Nome aceito como componente de caminho das pastas pessoais (id de projeto, spec ou tarefa):
/// letras, dígitos, `_`, `-` e `.`, sem começar por `.`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// `(spec, tarefa)` quando `dir` é a worktree de uma tarefa (`.../.sdd/worktrees/<spec>/<tarefa>`).
fn task_of(dir: &Path) -> Option<(String, String)> {
    let parts: Vec<&str> = dir
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [.., ".sdd", "worktrees", spec, task] => Some(((*spec).to_string(), (*task).to_string())),
        _ => None,
    }
}

/// Spec 141 T02 (C-02): `Some(TASK_WORKTREES)` quando `dir` é a raiz de um projeto (não a worktree
/// de uma tarefa) e tem as worktrees das tarefas, pastas de verdade (`.sdd` e `.sdd/worktrees`, sem
/// link). Medido com Docker: o `node --test` sem caminho do Node 20 varre as pastas com ponto e
/// rodava os testes das worktrees na bateria da raiz. O container da raiz recebe então uma pasta
/// vazia (tmpfs) por cima dela; o host e as rodadas das worktrees não mudam.
pub fn hidden_from_root(dir: &Path) -> Option<&'static str> {
    if task_of(dir).is_some() {
        return None;
    }
    let real_dir = |p: PathBuf| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    (real_dir(dir.join(".sdd")) && real_dir(dir.join(TASK_WORKTREES))).then_some(TASK_WORKTREES)
}

/// Pasta dos dados de um projeto em `homes`, com o id conferido.
pub fn project_homes(homes: &Path, project_id: &str) -> io::Result<PathBuf> {
    if !valid_name(project_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "id de projeto inválido para a pasta pessoal"));
    }
    Ok(homes.join(project_id))
}

/// Lugar (no host, nunca montado) da área de trabalho `dir` do projeto: o da worktree da tarefa
/// quando `dir` é uma, senão o da raiz. Guarda a pasta pessoal, a marca da migração e as sobras da
/// pasta antiga. Só calcula o caminho; não cria nada.
fn slot_of(homes: &Path, project_id: &str, dir: &Path) -> io::Result<PathBuf> {
    let base = project_homes(homes, project_id)?;
    match task_of(dir) {
        Some((spec, task)) if valid_name(&spec) && valid_name(&task) => Ok(base.join(TASKS_HOMES).join(spec).join(task)),
        Some(_) => Err(io::Error::new(io::ErrorKind::InvalidInput, "nome de worktree inválido para a pasta pessoal")),
        None => Ok(base.join(ROOT_HOME)),
    }
}

/// Pasta pessoal (no host) da área de trabalho `dir` do projeto, a montada em [`CONTAINER_HOME`]:
/// `home` dentro do lugar dela. Só calcula o caminho; não cria nada.
pub fn host_home(homes: &Path, project_id: &str, dir: &Path) -> io::Result<PathBuf> {
    Ok(slot_of(homes, project_id, dir)?.join(HOME_SUBDIR))
}

/// Garante a pasta pessoal `home` (e as pastas acima dela, modo 0700) com o `.profile` com
/// [`IMAGE_PATH_LINE`] e [`PATH_LINE`], nessa ordem. Recusa symlink ou entrada que não seja
/// diretório no lugar dela. Idempotente; o `.profile` que uma ferramenta já alterou só recebe a
/// linha que falta: [`IMAGE_PATH_LINE`] no começo (o `PATH` da imagem é a base do que as outras
/// linhas acrescentam) e [`PATH_LINE`] no fim.
pub fn ensure(home: &Path) -> io::Result<()> {
    match fs::symlink_metadata(home) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(io::Error::other("a pasta pessoal não é um diretório")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match DirBuilder::new().recursive(true).mode(0o700).create(home) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        },
        Err(e) => return Err(e),
    }
    // Criada agora por outro caminho (corrida) ou trocada: confere de novo, sem seguir symlink.
    if !fs::symlink_metadata(home)?.is_dir() {
        return Err(io::Error::other("a pasta pessoal não é um diretório"));
    }
    let profile = home.join(PROFILE);
    match fs::symlink_metadata(&profile) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => write_new(&profile, &format!("{IMAGE_PATH_LINE}\n{PATH_LINE}\n"))?,
        Ok(m) if m.is_file() => {
            let mut text = String::new();
            OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&profile)?.read_to_string(&mut text)?;
            let has = |line: &str| text.lines().any(|l| l.trim() == line);
            let (image, local) = (has(IMAGE_PATH_LINE), has(PATH_LINE));
            if !image || !local {
                let mut next = if image { String::new() } else { format!("{IMAGE_PATH_LINE}\n") };
                next.push_str(&text);
                if !local {
                    if !next.is_empty() && !next.ends_with('\n') {
                        next.push('\n');
                    }
                    next.push_str(PATH_LINE);
                    next.push('\n');
                }
                let mut f = OpenOptions::new().write(true).truncate(true).custom_flags(libc::O_NOFOLLOW).open(&profile)?;
                f.write_all(next.as_bytes())?;
            }
        }
        // Symlink ou outra coisa: não segue nem substitui.
        Ok(_) => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// A pasta pessoal da área de trabalho `dir`, pronta para o próximo comando: garantida
/// ([`ensure`]) e, na primeira vez, com a pasta antiga migrada ([`migrate_legacy`]). A marca
/// [`MIGRATED`] (no lugar da área de trabalho, fora do alcance dos containers) faz a migração rodar
/// uma vez só: um `.nants-home` criado depois é só uma pasta do projeto e não esquece a instalação
/// de novo. Uma migração que falha antes de tirar a pasta antiga do lugar fica para a próxima
/// chamada (aviso no log), sem impedir o comando e sem esquecer a instalação.
pub fn ready(homes: &Path, project_id: &str, dir: &Path) -> io::Result<PathBuf> {
    let slot = slot_of(homes, project_id, dir)?;
    let home = slot.join(HOME_SUBDIR);
    ensure(&home)?;
    let mark = slot.join(MIGRATED);
    if fs::symlink_metadata(&mark).is_ok() {
        return Ok(home);
    }
    match migrate_legacy(dir, &slot, &home) {
        Ok(()) => write_new(&mark, "")?,
        Err(e) => eprintln!("nants: pasta pessoal antiga ({LEGACY_HOME_DIR}) sem migração: {}", e.kind()),
    }
    Ok(home)
}

/// Nome único da pasta antiga tirada da área de trabalho.
fn moved_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    format!("{MOVED_PREFIX}{}-{nanos}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst))
}

/// Migra a pasta pessoal antiga de `dir`. Nada é lido nem apagado dentro da área de trabalho, que o
/// container pode alterar ao mesmo tempo:
/// 1. `<dir>/.nants-home` (diretório ou link) sai do alcance do container por um `rename` atômico
///    para o lugar `slot` (não montado). O `rename` não segue o último componente: um link chega
///    como link. Entre sistemas de arquivo (`EXDEV`), o destino é o diretório Git privado da área de
///    trabalho (mesmo disco, só leitura ou fora do container).
/// 2. Com algo tirado (agora ou numa chamada interrompida antes), o estado da instalação é
///    esquecido (a instalação roda de novo no `HOME` novo) e cada sobra é tratada por
///    [`dispose_moved`]: link só sai o link; diretório de verdade dá o [`KEEP_ON_RESET`] para `home` e
///    é apagado (o que não sai, como um arquivo de outro dono, fica no lugar, fora da árvore).
///
/// Não migra `.local` nem o `.profile`: eles podem ter `/work/.nants-home` gravado dentro. `Err`
/// só quando a pasta antiga não pôde ser tirada da área de trabalho.
pub fn migrate_legacy(dir: &Path, slot: &Path, home: &Path) -> io::Result<()> {
    let legacy = dir.join(LEGACY_HOME_DIR);
    let movable = fs::symlink_metadata(&legacy).is_ok_and(|m| m.is_dir() || m.file_type().is_symlink());
    if movable {
        match fs::rename(&legacy, slot.join(moved_name())) {
            Ok(()) => {}
            // Outra chamada tirou antes.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                let git_dir = crate::harness::git::Git::new(dir.to_path_buf()).private_git_dir().map_err(|_| io::Error::other("diretório Git privado indisponível"))?;
                let away = git_dir.join(format!("nants-{}", moved_name()));
                match fs::rename(&legacy, &away) {
                    Ok(()) => {
                        crate::sdd::test_levels::forget_install_state(dir);
                        dispose_moved(&away, home);
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
    let moved: Vec<PathBuf> = fs::read_dir(slot)?
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(MOVED_PREFIX)))
        .map(|e| e.path())
        .collect();
    if !moved.is_empty() {
        crate::sdd::test_levels::forget_install_state(dir);
        for m in &moved {
            dispose_moved(m, home);
        }
    }
    Ok(())
}

/// Trata uma pasta antiga já fora da área de trabalho (em `moved`, que o container não alcança):
/// link ou arquivo, só ele sai; diretório de verdade dá o cache para `home` (se `home` ainda não
/// tem um; numa falha, como entre sistemas de arquivo, o cache sai junto) e é apagado, sem seguir
/// link. Falhas ficam no log: a sobra fica onde está, fora da árvore, e sai com o lugar.
fn dispose_moved(moved: &Path, home: &Path) {
    let Ok(meta) = fs::symlink_metadata(moved) else { return };
    if !meta.is_dir() {
        let _ = fs::remove_file(moved);
        return;
    }
    let cache = moved.join(KEEP_ON_RESET);
    let target = home.join(KEEP_ON_RESET);
    let cache_is_dir = fs::symlink_metadata(&cache).is_ok_and(|m| m.is_dir());
    let target_free = matches!(fs::symlink_metadata(&target), Err(e) if e.kind() == io::ErrorKind::NotFound);
    if cache_is_dir && target_free {
        if let Err(e) = fs::rename(&cache, &target) {
            eprintln!("nants: cache da pasta pessoal antiga não migrado: {}", e.kind());
        }
    }
    if let Err(e) = fs::remove_dir_all(moved) {
        if fs::symlink_metadata(moved).is_ok() {
            eprintln!("nants: sobra da pasta pessoal antiga não apagada: {}", e.kind());
        }
    }
}

/// Apaga o conteúdo da pasta pessoal `home`, menos [`KEEP_ON_RESET`], sem seguir symlink, e
/// recria o `.profile` do Nants. Sem a pasta (ou com um symlink no lugar), não faz nada.
pub fn reset(home: &Path) -> io::Result<()> {
    match fs::symlink_metadata(home) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    for entry in fs::read_dir(home)? {
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
    ensure(home)
}

/// Apaga um diretório de pasta pessoal `path` (só se for diretório de verdade; sem ele, nada).
fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(_) if fs::symlink_metadata(path).is_err() => Ok(()),
            Err(e) => Err(e),
        },
        // Symlink ou arquivo no lugar: só o link sai, nunca o alvo.
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Apaga o lugar da área de trabalho `dir` (a worktree que acabou de sair: a pasta pessoal, a marca
/// e as sobras) e a pasta da spec, se ficou vazia. Só mexe dentro de `<homes>/<projeto>/`.
pub fn remove(homes: &Path, project_id: &str, dir: &Path) -> io::Result<()> {
    let base = project_homes(homes, project_id)?;
    let home = slot_of(homes, project_id, dir)?;
    if home == base || !home.starts_with(&base) {
        return Err(io::Error::other("pasta pessoal fora do projeto"));
    }
    remove_tree(&home)?;
    if let Some(spec_dir) = home.parent().filter(|p| p.parent() == Some(base.join(TASKS_HOMES).as_path())) {
        // `remove_dir` só remove pasta vazia.
        let _ = fs::remove_dir(spec_dir);
    }
    Ok(())
}

/// Apaga todas as pastas pessoais do projeto (remoção do projeto).
pub fn remove_project(homes: &Path, project_id: &str) -> io::Result<()> {
    remove_tree(&project_homes(homes, project_id)?)
}

/// Diretórios (não symlink) logo abaixo de `dir`, com o nome; vazio se `dir` não existe.
fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(|n| (n.to_string(), e.path())))
        .collect()
}

/// Varredura do início do servidor: apaga as pastas pessoais de projetos não cadastrados e as de
/// worktrees que não existem mais (`<raiz>/.sdd/worktrees/<spec>/<tarefa>` ausente). Projeto em
/// execução (`is_active`) fica de fora. Só apaga quando o Git confirma a ausência: a worktree que
/// existe (ou que não dá para conferir) mantém a pasta. Devolve o que saiu.
pub fn sweep_orphans(homes: &Path, projects: &[(String, PathBuf)], is_active: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for (id, path) in subdirs(homes) {
        if !valid_name(&id) || is_active(&id) {
            continue;
        }
        let Some((_, root)) = projects.iter().find(|(p, _)| *p == id) else {
            if remove_tree(&path).is_ok() {
                removed.push(path);
            }
            continue;
        };
        let tasks = path.join(TASKS_HOMES);
        for (spec, spec_dir) in subdirs(&tasks) {
            for (task, home) in subdirs(&spec_dir) {
                let wt = root.join(".sdd").join("worktrees").join(&spec).join(&task);
                let gone = matches!(fs::symlink_metadata(&wt), Err(e) if e.kind() == io::ErrorKind::NotFound);
                if gone && remove_tree(&home).is_ok() {
                    removed.push(home);
                }
            }
            let _ = fs::remove_dir(&spec_dir);
        }
    }
    removed
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
#[path = "home_e2e_tests.rs"]
mod e2e;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nants-home-t-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("mkdir");
        d
    }

    #[test]
    fn host_home_is_one_folder_per_worktree_and_one_for_the_root() {
        let homes = Path::new("/dados/homes");
        let wt = Path::new("/p/proj/.sdd/worktrees/141-x/T01");
        assert_eq!(host_home(homes, "p1", wt).expect("wt"), homes.join("p1/tasks/141-x/T01/home"));
        assert_eq!(host_home(homes, "p1", Path::new("/p/proj")).expect("raiz"), homes.join("p1/root/home"));
        // Uma spec chamada `root` não cai dentro da pasta da raiz.
        let odd = Path::new("/p/proj/.sdd/worktrees/root/T01");
        assert_eq!(host_home(homes, "p1", odd).expect("wt"), homes.join("p1/tasks/root/T01/home"));
        for bad in ["", "..", ".x", "a/b"] {
            assert!(host_home(homes, bad, wt).is_err(), "{bad}");
        }
        assert!(host_home(homes, "p1", Path::new("/p/.sdd/worktrees/.hidden/T01")).is_err());
    }

    /// Spec 141 T02: só a raiz com as worktrees de verdade esconde `.sdd/worktrees`; a worktree de
    /// uma tarefa, a raiz sem worktrees e os links nunca.
    #[test]
    fn only_the_root_with_real_task_worktrees_hides_them() {
        let d = dir("hidden");
        let root = d.join("proj");
        fs::create_dir_all(&root).expect("raiz");
        assert_eq!(hidden_from_root(&root), None, "sem .sdd");
        fs::create_dir_all(root.join(".sdd")).expect("sdd");
        assert_eq!(hidden_from_root(&root), None, "sem worktrees");
        let wt = root.join(".sdd/worktrees/s1/T01");
        fs::create_dir_all(&wt).expect("wt");
        assert_eq!(hidden_from_root(&root), Some(TASK_WORKTREES));
        assert_eq!(hidden_from_root(&wt), None, "a worktree da tarefa não muda");
        // Link no lugar de `.sdd/worktrees` ou de `.sdd`: nada.
        let other = d.join("outro");
        fs::create_dir_all(other.join("worktrees")).expect("outro");
        fs::remove_dir_all(root.join(TASK_WORKTREES)).expect("rm");
        std::os::unix::fs::symlink(other.join("worktrees"), root.join(TASK_WORKTREES)).expect("ln");
        assert_eq!(hidden_from_root(&root), None, "worktrees é link");
        fs::remove_dir_all(root.join(".sdd")).expect("rm");
        std::os::unix::fs::symlink(&other, root.join(".sdd")).expect("ln");
        assert_eq!(hidden_from_root(&root), None, ".sdd é link");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn ensure_creates_the_folder_owned_by_the_host_with_the_profile() {
        let d = dir("ensure");
        let home = d.join("homes/p1/root");
        ensure(&home).expect("ensure");
        let uid = fs::metadata("/proc/self").expect("self").uid();
        assert_eq!(fs::metadata(&home).expect("meta").uid(), uid);
        assert_eq!(fs::metadata(&home).expect("meta").permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::read_to_string(home.join(".profile")).expect("profile"), format!("{IMAGE_PATH_LINE}\n{PATH_LINE}\n"));
        // Idempotente; o `.profile` alterado por uma ferramenta só ganha a linha que falta: a do
        // `PATH` da imagem no começo, a do `.local/bin` no fim.
        fs::write(home.join(".profile"), ". \"$HOME/.tool/env\"").expect("w");
        ensure(&home).expect("de novo");
        ensure(&home).expect("de novo");
        let text = fs::read_to_string(home.join(".profile")).expect("profile");
        assert_eq!(text, format!("{IMAGE_PATH_LINE}\n. \"$HOME/.tool/env\"\n{PATH_LINE}\n"));
        // O `.profile` da spec 129 (só a linha do `.local/bin`) ganha a do `PATH` da imagem antes dela.
        fs::write(home.join(".profile"), format!("{PATH_LINE}\n")).expect("w");
        ensure(&home).expect("de novo");
        assert_eq!(fs::read_to_string(home.join(".profile")).expect("profile"), format!("{IMAGE_PATH_LINE}\n{PATH_LINE}\n"));
        fs::remove_dir_all(&d).expect("rm");
    }

    /// Spec 133 T12: o `.profile` gerado, lido por um shell cujo `PATH` foi refeito (como o
    /// `/etc/profile` do Alpine faz no shell de login), devolve o `PATH` com que o processo nasceu
    /// na frente, mantém as pastas que o perfil acrescentou (uma vez só) e põe `$HOME/.local/bin`
    /// antes de tudo. Não deixa variável auxiliar para trás.
    #[test]
    fn profile_restores_the_path_the_shell_started_with() {
        let d = dir("image-path");
        let home = d.join("h");
        ensure(&home).expect("ensure");
        let script = "PATH=/usr/local/sbin:/usr/bin:/bin:/usr/bin; . \"$HOME/.profile\"; printf '%s|%s' \"$PATH\" \"${nants_ip-livre}${nants_r-}${nants_d-}\"";
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", script])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/opt/imagem/bin:/usr/local/go/bin:/usr/bin:/bin")
            .output()
            .expect("sh");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let local = format!("{}/.local/bin", home.display());
        let want = format!("{local}:/opt/imagem/bin:/usr/local/go/bin:/usr/bin:/bin:/usr/local/sbin|livre");
        assert_eq!(text, want, "{}", String::from_utf8_lossy(&out.stderr));
        fs::remove_dir_all(&d).expect("rm");
    }

    #[test]
    fn ensure_refuses_a_symlink_or_file_in_place_of_the_folder() {
        let d = dir("symlink");
        let target = dir("symlink-target");
        let home = d.join("h");
        std::os::unix::fs::symlink(&target, &home).expect("ln");
        assert!(ensure(&home).is_err());
        assert!(!target.join(".profile").exists());
        // `reset` não segue o symlink.
        assert!(reset(&home).is_ok());
        fs::remove_file(&home).expect("rm");
        fs::write(&home, "x").expect("w");
        assert!(ensure(&home).is_err());
        assert!(reset(&home).is_ok());
        assert!(home.is_file());
        fs::remove_dir_all(&d).expect("rm");
        fs::remove_dir_all(&target).expect("rm");
    }

    #[test]
    fn reset_keeps_only_the_download_cache_and_never_follows_links() {
        let d = dir("reset");
        let outside = dir("reset-outside");
        fs::write(outside.join("keep.txt"), "fora").expect("w");
        let home = d.join("h");
        ensure(&home).expect("ensure");
        fs::create_dir_all(home.join(".local/bin")).expect("mk");
        fs::write(home.join(".local/bin/tool"), "#!/bin/sh\n").expect("w");
        fs::create_dir_all(home.join(".cache/dl")).expect("mk");
        fs::write(home.join(".cache/dl/pkg"), "bytes").expect("w");
        std::os::unix::fs::symlink(&outside, home.join("link")).expect("ln");
        reset(&home).expect("reset");
        let mut left: Vec<String> = fs::read_dir(&home).expect("ls").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec![".cache", ".profile"]);
        assert!(home.join(".cache/dl/pkg").exists());
        assert!(outside.join("keep.txt").exists());
        // Sem a pasta, nada a fazer.
        fs::remove_dir_all(&home).expect("rm");
        assert!(reset(&home).is_ok());
        fs::remove_dir_all(&d).expect("rm");
        fs::remove_dir_all(&outside).expect("rm");
    }

    #[test]
    fn legacy_path_is_the_first_component_only() {
        assert!(is_legacy_home_path(".nants-home/.local/bin/x"));
        assert!(is_legacy_home_path("./.nants-home"));
        assert!(!is_legacy_home_path("src/.nants-home/x"));
        assert!(!is_legacy_home_path(".nants-homes/x"));
    }

    /// Entradas `legacy-*` (sobras da pasta antiga) no lugar.
    fn moved_in(slot: &Path) -> Vec<PathBuf> {
        fs::read_dir(slot).expect("ls").flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(MOVED_PREFIX))).collect()
    }

    fn git_repo(tag: &str) -> PathBuf {
        let d = dir(tag);
        assert!(std::process::Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(&d).status().expect("git").success());
        d
    }

    /// Spec 141 T01: a pasta antiga sai da área de trabalho; só o cache de download vem para a
    /// pasta nova (o `.local` e o `.profile` antigos podem ter `/work/.nants-home` gravado). O link
    /// quebrado do PulseAudio e um link para fora saem sem seguir o alvo, e nada sobra no lugar.
    #[test]
    fn migration_brings_only_the_cache_and_removes_the_rest() {
        let d = dir("migrate");
        let outside = dir("migrate-outside");
        fs::write(outside.join("keep.txt"), "fora").expect("w");
        let legacy = d.join(LEGACY_HOME_DIR);
        fs::create_dir_all(legacy.join(".cache/pip")).expect("mk");
        fs::write(legacy.join(".cache/pip/pkg"), "baixado").expect("w");
        fs::create_dir_all(legacy.join(".local/bin")).expect("mk");
        fs::write(legacy.join(".local/bin/tool"), "#!/bin/sh\nexec /work/.nants-home/x\n").expect("w");
        fs::create_dir_all(legacy.join(".config/pulse")).expect("mk");
        std::os::unix::fs::symlink("/tmp/pulse-sumiu", legacy.join(".config/pulse/abc-runtime")).expect("ln");
        std::os::unix::fs::symlink(&outside, legacy.join("fora")).expect("ln");
        let slot = d.join("homes/p1/root");
        let home = slot.join(HOME_SUBDIR);
        ensure(&home).expect("ensure");
        migrate_legacy(&d, &slot, &home).expect("migra");
        assert!(fs::symlink_metadata(&legacy).is_err());
        assert_eq!(fs::read_to_string(home.join(".cache/pip/pkg")).expect("cache"), "baixado");
        assert!(!home.join(".local").exists());
        assert!(moved_in(&slot).is_empty(), "nada sobra no lugar");
        assert!(outside.join("keep.txt").exists(), "não segue symlink");
        migrate_legacy(&d, &slot, &home).expect("de novo, nada a fazer");
        fs::remove_dir_all(&d).expect("rm");
        fs::remove_dir_all(&outside).expect("rm");
    }

    /// Revisão 1 (F-01): a pasta antiga trocada por um link para uma pasta com `.cache` fora da
    /// área de trabalho (outro HOME, outro projeto). O link sai como link; a pasta de fora fica
    /// intacta, e o `.cache` dela não vai para a pasta montada.
    #[test]
    fn a_link_in_place_of_the_legacy_folder_is_never_followed() {
        let d = dir("migrate-link");
        let outside = dir("migrate-link-outside");
        fs::create_dir_all(outside.join(".cache/segredo")).expect("mk");
        fs::write(outside.join(".cache/segredo/token"), "x").expect("w");
        std::os::unix::fs::symlink(&outside, d.join(LEGACY_HOME_DIR)).expect("ln");
        let homes = d.join("dados/homes");
        let home = ready(&homes, "p1", &d).expect("ready");
        assert!(fs::symlink_metadata(d.join(LEGACY_HOME_DIR)).is_err(), "o link saiu");
        assert_eq!(fs::read_to_string(outside.join(".cache/segredo/token")).expect("intacto"), "x");
        assert!(!home.join(".cache").exists(), "nada de fora chegou à pasta montada");
        assert!(moved_in(&homes.join("p1/root")).is_empty());
        fs::remove_dir_all(&d).expect("rm");
        fs::remove_dir_all(&outside).expect("rm");
    }

    /// Spec 141 T01: com a pasta nova já com cache, o cache antigo não a substitui; a pasta antiga
    /// sai do mesmo jeito.
    #[test]
    fn migration_keeps_the_new_cache_when_there_is_one() {
        let d = dir("migrate-keep");
        let legacy = d.join(LEGACY_HOME_DIR);
        fs::create_dir_all(legacy.join(".cache")).expect("mk");
        fs::write(legacy.join(".cache/velho"), "v").expect("w");
        let slot = d.join("lugar");
        let home = slot.join(HOME_SUBDIR);
        fs::create_dir_all(home.join(".cache")).expect("mk");
        fs::write(home.join(".cache/novo"), "n").expect("w");
        migrate_legacy(&d, &slot, &home).expect("migra");
        assert!(home.join(".cache/novo").exists() && !home.join(".cache/velho").exists());
        assert!(!legacy.exists() && moved_in(&slot).is_empty());
        fs::remove_dir_all(&d).expect("rm");
    }

    /// Spec 141 T01: `ready` migra e esquece o estado da instalação uma vez só. Revisão 1 (F-01):
    /// um `.nants-home` criado depois (pelo agente, com `mkdir /work/.nants-home/.cache`) é só uma
    /// pasta do projeto: fica onde está e não esquece a instalação de novo.
    #[test]
    fn ready_migrates_and_forgets_the_install_state_once() {
        let d = git_repo("ready");
        let homes = d.join("dados/homes");
        let state = d.join(".git/nants-install.state");
        fs::write(&state, "chave\n2026-11-04T00:00:00Z\n").expect("w");
        fs::create_dir_all(d.join(LEGACY_HOME_DIR).join(".cache")).expect("mk");
        let home = ready(&homes, "p1", &d).expect("ready");
        assert_eq!(home, homes.join("p1/root/home"));
        assert!(home.join(".profile").exists() && home.join(".cache").is_dir());
        assert!(!d.join(LEGACY_HOME_DIR).exists());
        assert!(!state.exists(), "estado da instalação esquecido");
        assert!(homes.join("p1/root").join(MIGRATED).is_file(), "marca fora do alcance do container");
        fs::write(&state, "chave\n").expect("w");
        fs::create_dir_all(d.join(LEGACY_HOME_DIR).join(".cache")).expect("mk");
        fs::write(d.join(LEGACY_HOME_DIR).join(".cache/x"), "agente").expect("w");
        ready(&homes, "p1", &d).expect("de novo");
        assert!(state.exists(), "depois da marca, o estado fica");
        assert!(d.join(LEGACY_HOME_DIR).join(".cache/x").exists(), "a pasta nova do projeto fica");
        fs::remove_dir_all(&d).expect("rm");
    }

    /// Revisão 1 (F-01): uma sobra que não sai (um arquivo de outro dono numa pasta sem permissão
    /// de escrita) fica no lugar, fora da árvore, e não faz a instalação ser esquecida em todo
    /// comando.
    #[test]
    fn an_undeletable_leftover_does_not_force_a_reinstall_every_command() {
        let d = git_repo("ready-stuck");
        let homes = d.join("dados/homes");
        let state = d.join(".git/nants-install.state");
        let stuck = d.join(LEGACY_HOME_DIR).join("preso");
        fs::create_dir_all(&stuck).expect("mk");
        fs::write(stuck.join("de-outro-dono"), "x").expect("w");
        fs::set_permissions(&stuck, fs::Permissions::from_mode(0o500)).expect("chmod");
        fs::write(&state, "chave\n").expect("w");
        ready(&homes, "p1", &d).expect("ready");
        assert!(fs::symlink_metadata(d.join(LEGACY_HOME_DIR)).is_err(), "saiu da árvore");
        assert!(!state.exists(), "esquecida uma vez");
        let slot = homes.join("p1/root");
        let left = moved_in(&slot);
        assert_eq!(left.len(), 1, "a sobra fica no lugar, fora da árvore");
        fs::write(&state, "chave\n").expect("w");
        for _ in 0..3 {
            ready(&homes, "p1", &d).expect("de novo");
        }
        assert!(state.exists(), "não esquece de novo a cada comando");
        fs::set_permissions(left[0].join("preso"), fs::Permissions::from_mode(0o700)).expect("chmod");
        fs::remove_dir_all(&d).expect("rm");
    }

    /// Spec 141 T01: duas threads migrando a mesma área de trabalho ao mesmo tempo (execução em
    /// andamento) terminam sem erro e sem pasta antiga.
    #[test]
    fn concurrent_migrations_do_not_fail() {
        let d = dir("ready-race");
        let homes = d.join("dados/homes");
        for i in 0..50 {
            fs::create_dir_all(d.join(LEGACY_HOME_DIR).join(format!("a/b{i}"))).expect("mk");
        }
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (d, homes) = (d.clone(), homes.clone());
                std::thread::spawn(move || ready(&homes, "p1", &d).map(|_| ()))
            })
            .collect();
        for t in threads {
            t.join().expect("join").expect("ready");
        }
        assert!(!d.join(LEGACY_HOME_DIR).exists());
        fs::remove_dir_all(&d).expect("rm");
    }

    #[test]
    fn remove_takes_only_the_worktree_folder_and_the_empty_spec_folder() {
        let d = dir("remove");
        let homes = d.join("homes");
        let wt1 = d.join("proj/.sdd/worktrees/s1/T01");
        let wt2 = d.join("proj/.sdd/worktrees/s1/T02");
        let root = d.join("proj");
        for w in [&wt1, &wt2, &root] {
            ensure(&host_home(&homes, "p1", w).expect("home")).expect("ensure");
        }
        remove(&homes, "p1", &wt1).expect("rm");
        assert!(!homes.join("p1/tasks/s1/T01").exists());
        assert!(homes.join("p1/tasks/s1/T02").exists() && homes.join("p1/root").exists());
        remove(&homes, "p1", &wt2).expect("rm");
        assert!(!homes.join("p1/tasks/s1").exists(), "pasta da spec vazia sai");
        remove(&homes, "p1", &wt2).expect("de novo, sem erro");
        remove_project(&homes, "p1").expect("projeto");
        assert!(!homes.join("p1").exists());
        remove_project(&homes, "p1").expect("de novo, sem erro");
        fs::remove_dir_all(&d).expect("rm");
    }

    /// Spec 141 T01: a varredura do início apaga as pastas de projetos que não existem mais e as de
    /// worktrees que saíram, e NUNCA a de uma worktree viva, a da raiz ou as de projeto em execução.
    #[test]
    fn sweep_removes_orphans_and_never_a_live_worktree_home() {
        let d = dir("sweep");
        let homes = d.join("homes");
        let root = d.join("proj");
        let live = root.join(".sdd/worktrees/s1/T01");
        fs::create_dir_all(&live).expect("wt");
        let gone = root.join(".sdd/worktrees/s1/T02");
        let gone_spec = root.join(".sdd/worktrees/s2/T01");
        let busy_root = d.join("busy");
        let busy_gone = busy_root.join(".sdd/worktrees/s1/T09");
        for (id, w) in [("p1", &live), ("p1", &gone), ("p1", &gone_spec), ("p1", &root), ("old", &root), ("busy", &busy_gone)] {
            let h = host_home(&homes, id, w).expect("home");
            ensure(&h).expect("ensure");
            fs::write(h.join("marca"), "x").expect("w");
        }
        // Um symlink no lugar de um projeto não é seguido.
        let outside = dir("sweep-outside");
        fs::write(outside.join("keep.txt"), "fora").expect("w");
        std::os::unix::fs::symlink(&outside, homes.join("link")).expect("ln");
        let projects = vec![("p1".to_string(), root.clone()), ("busy".to_string(), busy_root.clone())];
        let mut removed = sweep_orphans(&homes, &projects, &|id| id == "busy");
        removed.sort();
        let mut want = vec![homes.join("old"), homes.join("p1/tasks/s1/T02"), homes.join("p1/tasks/s2/T01")];
        want.sort();
        assert_eq!(removed, want);
        assert!(homes.join("p1/tasks/s1/T01/home/marca").exists(), "a worktree viva mantém a pasta");
        assert!(homes.join("p1/root/home/marca").exists(), "a raiz mantém a pasta");
        assert!(homes.join("busy/tasks/s1/T09/home/marca").exists(), "projeto em execução fica de fora");
        assert!(!homes.join("p1/tasks/s2").exists());
        assert!(outside.join("keep.txt").exists());
        fs::remove_dir_all(&d).expect("rm");
        fs::remove_dir_all(&outside).expect("rm");
    }
}
