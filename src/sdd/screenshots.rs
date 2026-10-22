//! Capturas de tela (spec 082 T03): imagens PNG/JPEG que os testes e2e gravam, declaradas em
//! `screenshots` (globs no formato de `failure_artifacts`).
//!
//! - [`list`] encontra as capturas da execução corrente (mtime a partir de `since`, com a folga da
//!   busca de [`super::failure_artifacts`]), conferidas pela assinatura do formato, até
//!   [`MAX_FILE_BYTES`] cada (as maiores aparecem com o motivo) e no máximo [`MAX_NAMES`], das mais
//!   recentes para as mais antigas. A busca não segue symlinks nem sai da raiz.
//! - [`read`] lê uma captura pelo nome (caminho relativo), sem seguir symlinks.
//! - [`copy_viewed`] copia as capturas vistas pelo Revisor para `.sdd/specs/<spec>/captures/`
//!   (fora do Git pelo exclude local), com nome saneado e teto por spec.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::failure_artifacts;

/// Tamanho máximo de cada captura (bytes); as maiores são ignoradas, com o motivo na listagem.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Máximo de nomes na listagem (os mais recentes).
pub const MAX_NAMES: usize = 40;
/// Máximo de capturas anexadas por sessão de agente (`view_screenshot`).
pub const MAX_VIEWS: usize = 8;
/// Teto (bytes) das cópias de capturas por spec: acima disso, nada mais é copiado.
pub const MAX_SPEC_BYTES: u64 = 64 * 1024 * 1024;
/// Tamanho máximo do nome de uma cópia (caracteres `[a-z0-9._-]`).
pub const MAX_COPY_NAME_CHARS: usize = 120;
/// Subdiretório da spec com as cópias das capturas vistas pelo Revisor.
pub const CAPTURES_DIR: &str = "captures";
/// Título da seção dos briefings (Revisor e Desenvolvedor).
pub const SECTION_TITLE: &str = "Capturas de tela desta execução (use view_screenshot; até 8)";
/// Motivo listado para captura acima de [`MAX_FILE_BYTES`].
pub const TOO_LARGE: &str = "ignorada: maior que 2 MiB";
/// Máximo de nomes na linha de capturas de tarefas anteriores que sumiram (spec 086 T01).
pub const MAX_VANISHED: usize = 10;
/// Início da linha de capturas que sumiram no briefing do Revisor de tarefa (spec 086 T01).
pub const VANISHED_LABEL: &str = "Capturas de tarefas anteriores que esta execução não gera mais";

/// Uma captura listada: nome (caminho relativo à raiz) e, se não puder ser vista, o motivo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub name: String,
    pub skipped: Option<String>,
}

impl Capture {
    /// `true` se a captura pode ser pedida com `view_screenshot`.
    pub fn usable(&self) -> bool {
        self.skipped.is_none()
    }
}

/// Tipo MIME pela extensão (`.png`, `.jpg`, `.jpeg`, sem diferenciar maiúsculas).
pub fn media_type_of_name(name: &str) -> Option<&'static str> {
    let (_, ext) = name.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        _ => None,
    }
}

/// Tipo MIME pela assinatura: PNG `\x89PNG`, JPEG `\xFF\xD8\xFF`.
fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG") {
        Some("image/png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

/// Abre para leitura sem seguir um symlink no último componente.
fn open_nofollow(path: &Path) -> io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
}

fn head_of(path: &Path) -> Option<Vec<u8>> {
    let mut head = Vec::with_capacity(8);
    open_nofollow(path).ok()?.take(8).read_to_end(&mut head).ok()?;
    Some(head)
}

/// Capturas de `root` que casam com `globs`, modificadas a partir de `since`: só PNG/JPEG com a
/// assinatura do formato da extensão, até [`MAX_NAMES`], das mais recentes para as mais antigas.
/// As maiores que [`MAX_FILE_BYTES`] entram com o motivo em `skipped`. Vazio sem globs.
pub fn list(root: &Path, globs: &[String], since: SystemTime) -> Vec<Capture> {
    let mut out = Vec::new();
    for found in failure_artifacts::find(root, globs, since) {
        if out.len() >= MAX_NAMES {
            break;
        }
        let Some(mime) = media_type_of_name(&found.rel) else { continue };
        if found.rel.chars().any(char::is_control) {
            continue;
        }
        let path = root.join(&found.rel);
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if !meta.is_file() || head_of(&path).as_deref().and_then(sniff) != Some(mime) {
            continue;
        }
        let skipped = (meta.len() > MAX_FILE_BYTES).then(|| TOO_LARGE.to_string());
        out.push(Capture { name: found.rel, skipped });
    }
    out
}

/// Nomes que podem ser vistos (sem os ignorados), na ordem da listagem.
pub fn usable_names(captures: &[Capture]) -> Vec<String> {
    captures.iter().filter(|c| c.usable()).map(|c| c.name.clone()).collect()
}

/// Listagem para os briefings: `- nome` ou `- nome (motivo)`, uma por linha.
pub fn render_listing(captures: &[Capture]) -> String {
    captures
        .iter()
        .map(|c| match &c.skipped {
            Some(why) => format!("- {} ({why})", c.name),
            None => format!("- {}", c.name),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolve o nome pedido a `view_screenshot` (spec 086 T01): o próprio nome, se está em `available`;
/// senão, a única captura cujo nome de arquivo (último componente) é igual a ele. Ambíguo ou
/// inexistente → `None`.
pub fn resolve_name<'a>(available: &'a [String], name: &str) -> Option<&'a str> {
    if let Some(exact) = available.iter().find(|n| *n == name) {
        return Some(exact);
    }
    let mut matches = available.iter().filter(|n| n.rsplit('/').next() == Some(name));
    match (matches.next(), matches.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Lê a captura `name` (caminho relativo a `root`): componentes normais, nenhum symlink no caminho,
/// arquivo regular até [`MAX_FILE_BYTES`] e assinatura do formato da extensão. Devolve `(mime, bytes)`.
pub fn read(root: &Path, name: &str) -> Result<(&'static str, Vec<u8>), String> {
    let mime = media_type_of_name(name).ok_or("a captura não é PNG nem JPEG")?;
    let rel = Path::new(name);
    if name.is_empty() || name.contains('\\') || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("nome de captura inválido".into());
    }
    let mut path = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for (i, part) in parts.iter().enumerate() {
        path.push(part);
        let meta = fs::symlink_metadata(&path).map_err(|_| "captura não encontrada".to_string())?;
        let last = i + 1 == parts.len();
        if meta.file_type().is_symlink() || (last && !meta.is_file()) || (!last && !meta.is_dir()) {
            return Err("captura inválida (symlink ou tipo inesperado)".into());
        }
    }
    let file = open_nofollow(&path).map_err(|_| "captura ilegível".to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "captura ilegível".to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(TOO_LARGE.into());
    }
    if sniff(&bytes) != Some(mime) {
        return Err("o conteúdo não é PNG nem JPEG".into());
    }
    Ok((mime, bytes))
}

/// Letra base de um acento latino comum (á→a, ç→c, Ñ→N…); os demais caracteres ficam como estão.
fn fold_accent(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'Á' | 'À' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'A',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' => 'O',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
        'ç' => 'c',
        'Ç' => 'C',
        'ñ' => 'n',
        'Ñ' => 'N',
        _ => c,
    }
}

/// Trecho só com `[a-z0-9._-]`: acentos latinos viram a letra base; minúsculas; o resto vira `-`;
/// sem `-`/`.` repetidos nem nas bordas.
fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().map(|c| fold_accent(c).to_ascii_lowercase()) {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-' { c } else { '-' };
        if (c == '-' || c == '.') && out.ends_with(c) {
            continue;
        }
        out.push(c);
    }
    out.trim_matches(['-', '.']).to_string()
}

/// Sufixo estável de 6 hex do caminho completo (FNV-1a de 32 bits, 24 bits finais): o mesmo caminho
/// gera sempre o mesmo sufixo, em qualquer processo e ordem.
fn path_suffix(name: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{:06x}", hash & 0x00ff_ffff)
}

/// Nome da cópia com sufixo opcional antes da extensão (ver [`copy_name`]).
fn build_copy_name(task: &str, attempt: u32, name: &str, suffix: Option<&str>) -> Option<String> {
    media_type_of_name(name)?;
    // Só o nome do arquivo: as pastas do Playwright são longas e repetitivas.
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (stem, ext) = file.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    let mut task = sanitize(task);
    if task.is_empty() {
        task = "tarefa".into();
    }
    let prefix = format!("{task}-{attempt}-");
    let tail = suffix.map_or(0, |s| s.len() + 1);
    let room = MAX_COPY_NAME_CHARS.checked_sub(prefix.len() + tail + 1 + ext.len())?;
    let mut stem = sanitize(stem);
    if stem.len() > room {
        // Só ASCII depois de `sanitize`: o corte por byte cai em fronteira de caractere.
        stem = stem[..room].trim_end_matches(['-', '.']).to_string();
    }
    if stem.is_empty() {
        stem = "captura".chars().take(room.max(1)).collect();
    }
    match suffix {
        Some(s) => Some(format!("{prefix}{stem}-{s}.{ext}")),
        None => Some(format!("{prefix}{stem}.{ext}")),
    }
}

/// Nome da cópia: `<tarefa>-<tentativa>-<nome-do-arquivo-sanitizado>.<ext>` (sem as pastas), só
/// `[a-z0-9._-]` e até [`MAX_COPY_NAME_CHARS`] caracteres (nome longo é cortado no fim do nome
/// base). `None` se `name` não for PNG/JPEG. Colisões num lote: ver [`copy_names`].
pub fn copy_name(task: &str, attempt: u32, name: &str) -> Option<String> {
    build_copy_name(task, attempt, name, None)
}

/// Nomes das cópias de um lote, na ordem de `names`: como [`copy_name`], mas os caminhos distintos
/// que dariam o mesmo nome ganham o sufixo de 6 hex do caminho completo (`t02-1-home-3fa2c1.png`).
/// O resultado de cada caminho depende só dele e do conjunto, nunca da ordem.
pub fn copy_names(task: &str, attempt: u32, names: &[String]) -> Vec<Option<String>> {
    let plain: Vec<Option<String>> = names.iter().map(|n| copy_name(task, attempt, n)).collect();
    names
        .iter()
        .zip(&plain)
        .map(|(name, base)| {
            let base = base.as_ref()?;
            let clash = names.iter().zip(&plain).any(|(other, b)| other != name && b.as_ref() == Some(base));
            if clash {
                build_copy_name(task, attempt, name, Some(&path_suffix(name)))
            } else {
                Some(base.clone())
            }
        })
        .collect()
}

/// Separa o nome de uma cópia `t<NN>-<rodada>-<base>` em (`t<NN>`, número da tarefa, base). Outros
/// formatos (ex.: `final-…`) → `None`.
fn split_task_copy(file: &str) -> Option<(&str, u32, &str)> {
    let (task, rest) = file.split_once('-')?;
    let number = task.strip_prefix('t')?;
    let (attempt, base) = rest.split_once('-')?;
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    if !digits(number) || !digits(attempt) || base.is_empty() {
        return None;
    }
    Some((task, number.parse().ok()?, base))
}

/// Base sem o sufixo de colisão de 6 hex de [`copy_names`] (`home-3fa2c1.png` → `home.png`).
fn strip_collision(base: &str) -> String {
    let Some((stem, ext)) = base.rsplit_once('.') else { return base.to_string() };
    match stem.rsplit_once('-') {
        Some((head, tail))
            if !head.is_empty() && tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            format!("{head}.{ext}")
        }
        _ => base.to_string(),
    }
}

/// Capturas de tarefas anteriores que esta execução não gera mais (spec 086 T01): bases das cópias em
/// `<spec_dir>/captures/` de tarefas diferentes de `task` (prefixo `t<NN>-<rodada>-`; `final-…` fica de
/// fora), sem o sufixo de colisão, que não aparecem entre as cópias que `current` geraria (mesma regra
/// de [`copy_name`]). Devolve `(base, tarefa)` (a tarefa mais recente de cada base), ordenado por tarefa
/// e base. Não segue symlinks e ignora o que não é arquivo regular; falha de leitura → vazio.
pub fn vanished(spec_dir: &Path, task: &str, current: &[String]) -> Vec<(String, String)> {
    let dir = spec_dir.join(CAPTURES_DIR);
    if !fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
    let own = sanitize(task);
    let generated: std::collections::HashSet<String> = current
        .iter()
        .filter_map(|n| copy_name(task, 1, n))
        .filter_map(|c| split_task_copy(&c).map(|(_, _, base)| strip_collision(base)))
        .collect();
    let mut found: std::collections::BTreeMap<String, (u32, String)> = std::collections::BTreeMap::new();
    for entry in entries.flatten() {
        let Ok(file) = entry.file_name().into_string() else { continue };
        if !fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_file()) || media_type_of_name(&file).is_none() {
            continue;
        }
        let Some((prefix, number, base)) = split_task_copy(&file) else { continue };
        if prefix == own {
            continue;
        }
        let base = strip_collision(base);
        if generated.contains(&base) {
            continue;
        }
        let label = prefix.to_ascii_uppercase();
        match found.get(&base) {
            Some((n, _)) if *n >= number => {}
            _ => {
                found.insert(base, (number, label));
            }
        }
    }
    let mut out: Vec<(u32, String, String)> = found.into_iter().map(|(base, (n, label))| (n, base, label)).collect();
    out.sort();
    out.into_iter().map(|(_, base, label)| (base, label)).collect()
}

/// Linha "Capturas de tarefas anteriores que esta execução não gera mais: a.png (T02), …" com até
/// [`MAX_VANISHED`] nomes (e "…" se houver mais); vazia sem nenhuma.
pub fn render_vanished(list: &[(String, String)]) -> String {
    if list.is_empty() {
        return String::new();
    }
    let mut items: Vec<String> = list.iter().take(MAX_VANISHED).map(|(base, task)| format!("{base} ({task})")).collect();
    if list.len() > MAX_VANISHED {
        items.push("…".into());
    }
    format!("{VANISHED_LABEL}: {}", items.join(", "))
}

/// Soma dos tamanhos dos arquivos regulares de `dir` (sem descer em subdiretórios).
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .filter_map(|e| fs::symlink_metadata(e.path()).ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Grava sem seguir um symlink no destino.
fn write_nofollow(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)?.write_all(bytes)
}

/// Copia as capturas `names` (relativas a `src_root`) vistas pelo Revisor para
/// `<spec_dir>/captures/<copy_name>`. Melhor esforço: captura ilegível, destino inválido ou acima do
/// teto [`MAX_SPEC_BYTES`] da spec fica de fora. Devolve os nomes copiados, sem repetição, na ordem.
pub fn copy_viewed(spec_dir: &Path, task: &str, attempt: u32, src_root: &Path, names: &[String]) -> Vec<String> {
    let mut copied: Vec<String> = Vec::new();
    if names.is_empty() || !fs::symlink_metadata(spec_dir).is_ok_and(|m| m.is_dir()) {
        return copied;
    }
    let dir = spec_dir.join(CAPTURES_DIR);
    match fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return copied,
        Err(_) => {
            if fs::create_dir(&dir).is_err() {
                return copied;
            }
        }
    }
    let mut used = dir_bytes(&dir);
    for (name, target) in names.iter().zip(copy_names(task, attempt, names)) {
        let Some(target) = target else { continue };
        if copied.contains(&target) {
            continue;
        }
        let Ok((_, bytes)) = read(src_root, name) else { continue };
        let dest = dir.join(&target);
        let replaced = fs::symlink_metadata(&dest).ok().filter(|m| m.is_file()).map_or(0, |m| m.len());
        let total = used.saturating_sub(replaced) + bytes.len() as u64;
        if total > MAX_SPEC_BYTES {
            continue;
        }
        if write_nofollow(&dest, &bytes).is_ok() {
            used = total;
            copied.push(target);
        }
    }
    copied
}

/// Instante de um texto RFC 3339 UTC `YYYY-MM-DDTHH:MM:SSZ` (o formato gravado pelo Nants).
pub fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    let b = text.trim().as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { std::str::from_utf8(&b[from..to]).ok()?.parse().ok() };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Dias desde a época (algoritmo de dias civis, inverso do `format_utc` do servidor).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(secs).ok().map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F'];

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-shots-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn write(root: &Path, rel: &str, content: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().expect("pai")).expect("mkdir");
        fs::write(p, content).expect("write");
    }

    fn set_age(root: &Path, rel: &str, age: Duration) {
        let f = fs::OpenOptions::new().write(true).open(root.join(rel)).expect("open");
        f.set_modified(SystemTime::now() - age).expect("mtime");
    }

    fn globs(g: &[&str]) -> Vec<String> {
        g.iter().map(|s| s.to_string()).collect()
    }

    fn names(list: &[Capture]) -> Vec<&str> {
        list.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn old_files_and_wrong_content_are_left_out() {
        let root = tmp("filter");
        let since = SystemTime::now() - Duration::from_secs(60);
        write(&root, "shots/new.png", PNG);
        write(&root, "shots/photo.JPG", JPEG);
        write(&root, "shots/old.png", PNG);
        set_age(&root, "shots/old.png", Duration::from_secs(3600));
        write(&root, "shots/fake.png", b"not an image at all");
        write(&root, "shots/swapped.png", JPEG);
        write(&root, "shots/notes.txt", b"\x89PNG");
        let list = super::list(&root, &globs(&["shots/**/*"]), since);
        let mut got = names(&list);
        got.sort();
        assert_eq!(got, vec!["shots/new.png", "shots/photo.JPG"]);
        assert!(list.iter().all(Capture::usable));
    }

    #[test]
    fn oversized_is_listed_with_reason_and_not_usable() {
        let root = tmp("size");
        let since = SystemTime::now() - Duration::from_secs(60);
        let mut big = PNG.to_vec();
        big.resize(MAX_FILE_BYTES as usize + 1, 0);
        write(&root, "s/big.png", &big);
        write(&root, "s/ok.png", PNG);
        let list = super::list(&root, &globs(&["s/*.png"]), since);
        let big = list.iter().find(|c| c.name == "s/big.png").expect("listada");
        assert_eq!(big.skipped.as_deref(), Some(TOO_LARGE));
        assert_eq!(usable_names(&list), vec!["s/ok.png".to_string()]);
        assert!(render_listing(&list).contains(&format!("- s/big.png ({TOO_LARGE})")));
        assert_eq!(read(&root, "s/big.png").err().as_deref(), Some(TOO_LARGE));
    }

    #[test]
    fn most_recent_first_and_capped() {
        let root = tmp("order");
        let since = SystemTime::now() - Duration::from_secs(600);
        for i in 0..(MAX_NAMES + 5) {
            let rel = format!("s/c{i:02}.png");
            write(&root, &rel, PNG);
            set_age(&root, &rel, Duration::from_secs(500 - i as u64));
        }
        let list = super::list(&root, &globs(&["s/*.png"]), since);
        assert_eq!(list.len(), MAX_NAMES);
        assert_eq!(list[0].name, format!("s/c{:02}.png", MAX_NAMES + 4));
        assert_eq!(list[1].name, format!("s/c{:02}.png", MAX_NAMES + 3));
        assert!(!names(&list).contains(&"s/c00.png"));
        assert!(super::list(&root, &[], since).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_neither_listed_nor_read() {
        let root = tmp("link");
        let outside = tmp("link-out");
        write(&outside, "secret.png", PNG);
        fs::create_dir_all(root.join("s")).expect("mkdir");
        std::os::unix::fs::symlink(outside.join("secret.png"), root.join("s/a.png")).expect("link file");
        std::os::unix::fs::symlink(&outside, root.join("s/dir")).expect("link dir");
        let since = SystemTime::now() - Duration::from_secs(60);
        assert!(list(&root, &globs(&["s/**/*.png"]), since).is_empty());
        assert!(read(&root, "s/a.png").is_err());
        assert!(read(&root, "s/dir/secret.png").is_err());
    }

    #[test]
    fn read_rejects_paths_outside_and_checks_signature() {
        let root = tmp("read");
        write(&root, "s/a.png", PNG);
        write(&root, "s/b.png", b"texto");
        assert_eq!(read(&root, "s/a.png").expect("lê").0, "image/png");
        for bad in ["../s/a.png", "/etc/x.png", "s/../s/a.png", "./s/a.png", "", "s\\a.png", "s/a.gif"] {
            assert!(read(&root, bad).is_err(), "{bad}");
        }
        assert!(read(&root, "s/b.png").is_err());
        assert!(read(&root, "s/none.png").is_err());
    }

    #[test]
    fn copy_name_is_sanitized_and_bounded() {
        assert_eq!(copy_name("T03", 2, "test-results/screens/Home Page.PNG").as_deref(), Some("t03-2-home-page.png"));
        assert_eq!(copy_name("FINAL", 1, "a/ação.jpeg").as_deref(), Some("final-1-acao.jpeg"));
        assert_eq!(copy_name("T01", 1, "x.txt"), None);
        let long = format!("{}/fim.png", "d".repeat(300));
        assert_eq!(copy_name("T01", 3, &long).as_deref(), Some("t01-3-fim.png"));
        assert_eq!(copy_name("T01", 1, "../.png").as_deref(), Some("t01-1-captura.png"));
    }

    #[test]
    fn copy_name_uses_only_the_file_name_with_accents_folded() {
        // Spec 084 T01: o caminho do Playwright (longo e acentuado) não entra no nome da cópia.
        let path = "test-results/screens/apps-aparência-Alterar-tema-chromium-f0ffe/aparência-ALTERADA-desktop.png";
        assert_eq!(copy_name("T04", 1, path).as_deref(), Some("t04-1-aparencia-alterada-desktop.png"));
        assert_eq!(copy_name("T01", 2, "s/Ícone ÚNICO çà ñ Õ.JPG").as_deref(), Some("t01-2-icone-unico-ca-n-o.jpg"));
    }

    #[test]
    fn long_file_name_is_cut_at_the_end_of_the_stem() {
        let long = format!("dir/inicio-{}.png", "x".repeat(300));
        let name = copy_name("T01", 3, &long).expect("nome");
        assert_eq!(name.len(), MAX_COPY_NAME_CHARS, "{name}");
        assert!(name.starts_with("t01-3-inicio-xxx") && name.ends_with("x.png"), "{name}");
        assert!(name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)));
    }

    #[test]
    fn colliding_names_in_a_batch_get_stable_distinct_suffixes() {
        let batch = globs(&["desktop/home.png", "mobile/home.png", "outra.png", "desktop/home.png"]);
        let got = copy_names("T02", 1, &batch);
        let (a, b) = (got[0].clone().expect("a"), got[1].clone().expect("b"));
        assert_ne!(a, b);
        for n in [&a, &b] {
            let rest = n.strip_prefix("t02-1-home-").and_then(|r| r.strip_suffix(".png")).expect("formato");
            assert!(rest.len() == 6 && rest.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{n}");
        }
        assert_eq!(got[2].as_deref(), Some("t02-1-outra.png"), "sem colisão, sem sufixo");
        assert_eq!(got[3].as_deref(), Some(a.as_str()), "o mesmo caminho repetido não é colisão");
        // Estável: independe da ordem do lote e só depende do caminho.
        let reversed = copy_names("T02", 1, &globs(&["mobile/home.png", "desktop/home.png"]));
        assert_eq!(reversed, vec![Some(b.clone()), Some(a.clone())]);
        assert_eq!(path_suffix("desktop/home.png"), path_suffix("desktop/home.png"));
        // Com sufixo, o nome continua dentro do teto.
        let long = |d: &str| format!("{d}/{}.png", "y".repeat(300));
        let capped = copy_names("T02", 1, &[long("a"), long("b")]);
        for n in capped.iter().flatten() {
            assert!(n.len() <= MAX_COPY_NAME_CHARS, "{n}");
        }
        assert_ne!(capped[0], capped[1]);
    }

    #[test]
    fn copy_viewed_copies_and_respects_the_spec_cap() {
        let src = tmp("copy-src");
        let spec = tmp("copy-spec");
        write(&src, "s/a.png", PNG);
        write(&src, "s/b.jpg", JPEG);
        let copied = copy_viewed(&spec, "T02", 1, &src, &globs(&["s/a.png", "s/b.jpg", "s/a.png", "s/missing.png"]));
        assert_eq!(copied, vec!["t02-1-a.png".to_string(), "t02-1-b.jpg".to_string()]);
        assert_eq!(fs::read(spec.join("captures/t02-1-a.png")).expect("cópia"), PNG);
        // Acima do teto da spec, nada é copiado.
        let filler = fs::File::create(spec.join("captures/filler.bin")).expect("filler");
        filler.set_len(MAX_SPEC_BYTES).expect("tamanho");
        assert!(copy_viewed(&spec, "T02", 2, &src, &globs(&["s/a.png"])).is_empty());
        assert!(!spec.join("captures/t02-2-a.png").exists());
    }

    #[test]
    fn copy_viewed_keeps_both_colliding_captures() {
        let src = tmp("copy-clash");
        let spec = tmp("copy-clash-spec");
        write(&src, "desktop/home.png", PNG);
        write(&src, "mobile/home.png", PNG);
        let batch = globs(&["desktop/home.png", "mobile/home.png"]);
        let copied = copy_viewed(&spec, "T02", 1, &src, &globs(&["desktop/home.png"]));
        assert_eq!(copied, vec!["t02-1-home.png".to_string()]);
        let copied = copy_viewed(&spec, "T02", 1, &src, &batch);
        let expected: Vec<String> = copy_names("T02", 1, &batch).into_iter().flatten().collect();
        assert_eq!(copied, expected);
        assert_eq!(fs::read(spec.join("captures").join(&copied[0])).expect("cópia"), PNG);
    }

    #[test]
    fn resolve_name_accepts_a_unique_file_name() {
        // Spec 086 T01: nome curto único resolve; ambíguo ou inexistente, não.
        let available = globs(&["test-results/screens/home-desktop.png", "a/home.png", "b/home.png"]);
        assert_eq!(resolve_name(&available, "home-desktop.png"), Some("test-results/screens/home-desktop.png"));
        assert_eq!(resolve_name(&available, "a/home.png"), Some("a/home.png"));
        assert_eq!(resolve_name(&available, "home.png"), None, "ambíguo");
        assert_eq!(resolve_name(&available, "outra.png"), None);
        assert_eq!(resolve_name(&available, "screens/home-desktop.png"), None, "só o nome exato ou o do arquivo");
    }

    #[test]
    fn vanished_lists_previous_task_captures_not_generated_now() {
        // Spec 086 T01: só outras tarefas (sem `final-…` nem a própria), sem o sufixo de colisão.
        let spec = tmp("vanished");
        for f in [
            "t02-1-home.png",
            "t02-2-perfil-3fa2c1.png",
            "t02-1-mantida.png",
            "t03-1-propria.png",
            "final-1-final.png",
            "t01-1-antiga.jpg",
            "notas.txt",
        ] {
            write(&spec, &format!("captures/{f}"), PNG);
        }
        fs::create_dir_all(spec.join("captures/t02-1-dir.png")).expect("dir");
        let current = globs(&["shots/Mantida.png", "shots/x/novo.png"]);
        let got = vanished(&spec, "T03", &current);
        let expected: Vec<(String, String)> = [("antiga.jpg", "T01"), ("home.png", "T02"), ("perfil.png", "T02")]
            .iter()
            .map(|(b, t)| (b.to_string(), t.to_string()))
            .collect();
        assert_eq!(got, expected);
        assert_eq!(
            render_vanished(&got),
            format!("{VANISHED_LABEL}: antiga.jpg (T01), home.png (T02), perfil.png (T02)")
        );
        // Sem faltas: nada.
        let all = globs(&["s/home.png", "s/perfil.png", "s/antiga.jpg", "s/mantida.png"]);
        assert!(vanished(&spec, "T03", &all).is_empty());
        assert_eq!(render_vanished(&[]), "");
        // Sem diretório de cópias: nenhuma anterior.
        assert!(vanished(&tmp("vanished-none"), "T03", &current).is_empty());
    }

    #[test]
    fn vanished_line_is_capped_at_ten_names() {
        let list: Vec<(String, String)> = (0..12).map(|i| (format!("c{i:02}.png"), "T02".to_string())).collect();
        let line = render_vanished(&list);
        assert!(line.ends_with("c09.png (T02), …"), "{line}");
        assert_eq!(line.matches("(T02)").count(), MAX_VANISHED);
    }

    #[cfg(unix)]
    #[test]
    fn vanished_does_not_follow_symlinks() {
        let spec = tmp("vanished-link");
        let other = tmp("vanished-link-target");
        write(&other, "t02-1-fora.png", PNG);
        fs::create_dir_all(spec.join("captures")).expect("dir");
        std::os::unix::fs::symlink(other.join("t02-1-fora.png"), spec.join("captures/t02-1-link.png")).expect("link");
        assert!(vanished(&spec, "T03", &[]).is_empty());
        // O próprio diretório como symlink também é ignorado.
        let linked = tmp("vanished-linked-dir");
        std::os::unix::fs::symlink(&other, linked.join("captures")).expect("link dir");
        assert!(vanished(&linked, "T03", &[]).is_empty());
    }

    #[test]
    fn parse_rfc3339_roundtrips_the_server_format() {
        let t = parse_rfc3339("2026-10-18T12:34:56Z").expect("data");
        let secs = t.duration_since(UNIX_EPOCH).expect("época").as_secs();
        assert_eq!(crate::server::projects::format_utc(secs), "2026-10-18T12:34:56Z");
        assert_eq!(parse_rfc3339("2000-02-29T00:00:00Z").map(|t| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).ok()), Some(Some(951_782_400)));
        for bad in ["", "2026-10-18", "2026-13-01T00:00:00Z", "2026-10-18T12:34:56+00:00", "abcd-10-18T12:34:56Z"] {
            assert!(parse_rfc3339(bad).is_none(), "{bad}");
        }
    }
}
