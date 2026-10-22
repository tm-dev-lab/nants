//! Imagens de referência do usuário (spec 084 T02): anexos do chat do Arquiteto que definem o visual
//! esperado, listados em `references` no frontmatter da spec.
//!
//! - [`attachment_mime`] é a regra única de nome dos anexos do chat (também usada pela rota de anexos).
//! - [`copy_to_spec`] copia, na aprovação, os anexos listados para `.sdd/specs/<spec>/references/`
//!   (o Git dos projetos não versiona `.sdd/specs`, e a pasta de anexos tem cota). Melhor esforço,
//!   idempotente, sem seguir symlinks.
//! - [`References::load`] e [`read`] servem os briefings e o `view_screenshot` (nomes
//!   `referencia/<arquivo>`), com limite próprio de [`MAX_VIEWS`] por sessão.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Diretório dos anexos do chat, relativo à raiz do projeto.
pub const CHAT_ATTACHMENTS_REL: &str = ".sdd/specs/.chat-attachments";
/// Máximo de referências por spec.
pub const MAX_REFERENCES: usize = 4;
/// Tamanho máximo de cada referência (o mesmo limite do upload de anexos).
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Subdiretório da spec com as cópias das referências.
pub const REFERENCES_DIR: &str = "references";
/// Prefixo dos nomes aceitos por `view_screenshot` para as referências.
pub const VIEW_PREFIX: &str = "referencia/";
/// Máximo de referências anexadas por sessão de agente (à parte das capturas).
pub const MAX_VIEWS: usize = 4;
/// Título da seção dos briefings (Desenvolvedor, Revisor e verificação final).
pub const SECTION_TITLE: &str = "Imagens de referência do usuário (use view_screenshot; até 4)";

const ID_LEN: usize = 32;

/// Nome de anexo aceito: 32 hex minúsculos + `.png`, `.jpg`, `.jpeg` ou `.webp`, nada além disso.
/// Devolve o MIME.
pub fn attachment_mime(name: &str) -> Option<&'static str> {
    let (id, ext) = name.split_once('.')?;
    if id.len() != ID_LEN || !id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// MIME pela assinatura do conteúdo: PNG, JPEG ou WebP (`RIFF....WEBP`).
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// `references` da spec: nomes válidos (ver [`attachment_mime`]), sem repetição, até
/// [`MAX_REFERENCES`]; os inválidos são ignorados.
pub fn validate(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names.iter().map(|n| n.trim()) {
        if out.len() >= MAX_REFERENCES {
            break;
        }
        if attachment_mime(name).is_some() && !out.iter().any(|o| o == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// `base/<parts...>` só com diretórios reais (sem symlink); `None` se algum faltar ou for inseguro.
fn real_dir(base: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    for part in parts {
        path.push(part);
        let meta = fs::symlink_metadata(&path).ok()?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return None;
        }
    }
    Some(path)
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

/// Lê a imagem `dir/name`: nome válido, arquivo regular (sem symlink), até [`MAX_FILE_BYTES`] e
/// assinatura do formato da extensão. Devolve `(mime, bytes)`.
fn read_image(dir: &Path, name: &str) -> Result<(&'static str, Vec<u8>), String> {
    let mime = attachment_mime(name).ok_or("nome de referência inválido")?;
    let path = dir.join(name);
    let meta = fs::symlink_metadata(&path).map_err(|_| "referência não encontrada".to_string())?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("referência inválida (symlink ou tipo inesperado)".into());
    }
    let file = open_nofollow(&path).map_err(|_| "referência ilegível".to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "referência ilegível".to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("referência maior que 5 MiB".into());
    }
    if sniff(&bytes) != Some(mime) {
        return Err("o conteúdo não é PNG, JPEG nem WebP".into());
    }
    Ok((mime, bytes))
}

/// `spec_id` é um único componente normal (sem separador nem `..`).
fn is_plain_id(spec_id: &str) -> bool {
    let mut parts = Path::new(spec_id).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none() && !spec_id.contains('\\')
}

/// Copia os anexos `names` (de [`CHAT_ATTACHMENTS_REL`]) para `.sdd/specs/<spec_id>/references/`.
/// Melhor esforço: nome inválido, anexo ausente, inseguro (symlink), grande demais ou com conteúdo
/// que não confere fica de fora, sem erro. Idempotente: cópia idêntica já presente não é regravada.
/// Devolve os nomes presentes no destino depois da cópia, na ordem de `names`.
pub fn copy_to_spec(root: &Path, spec_id: &str, names: &[String]) -> Vec<String> {
    let mut done = Vec::new();
    let names = validate(names);
    if names.is_empty() || !is_plain_id(spec_id) {
        return done;
    }
    let Some(spec_dir) = real_dir(root, &[".sdd", "specs", spec_id]) else { return done };
    let Some(src_dir) = real_dir(root, &[".sdd", "specs", ".chat-attachments"]) else { return done };
    let dest_dir = spec_dir.join(REFERENCES_DIR);
    match fs::symlink_metadata(&dest_dir) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
        Ok(_) => return done,
        Err(_) => {
            if fs::create_dir(&dest_dir).is_err() {
                return done;
            }
        }
    }
    for name in names {
        let Ok((_, bytes)) = read_image(&src_dir, &name) else { continue };
        let dest = dest_dir.join(&name);
        match fs::symlink_metadata(&dest) {
            // Já copiada com o mesmo conteúdo: nada a fazer.
            Ok(m) if m.is_file() && read_image(&dest_dir, &name).is_ok_and(|(_, b)| b == bytes) => {
                done.push(name);
                continue;
            }
            Ok(m) if !m.is_file() => continue,
            _ => {}
        }
        if write_nofollow(&dest, &bytes).is_ok() {
            done.push(name);
        }
    }
    done
}

/// Tamanho máximo de um `spec.md` lido para coletar as referências.
const MAX_SPEC_BYTES: u64 = 1024 * 1024;

/// Nomes de anexo no `references` de todas as `.sdd/specs/*/spec.md` (spec 085 T02), lidos com o
/// parser de specs. Diretório de spec ou `spec.md` symlink, ausente, grande demais ou ilegível é
/// ignorado, sem erro.
pub fn spec_references(root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(specs) = real_dir(root, &[".sdd", "specs"]) else { return out };
    let Ok(entries) = fs::read_dir(&specs) else { return out };
    for entry in entries.flatten() {
        // `DirEntry::metadata` não segue symlinks: diretório de spec que é symlink fica de fora.
        if !entry.metadata().is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let path = entry.path().join("spec.md");
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let Ok(file) = open_nofollow(&path) else { continue };
        let mut bytes = Vec::new();
        if file.take(MAX_SPEC_BYTES + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > MAX_SPEC_BYTES {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else { continue };
        if let Ok(spec) = super::Spec::parse(&text) {
            out.extend(spec.frontmatter.references.unwrap_or_default());
        }
    }
    out
}

/// Referências de uma spec: o diretório (`.sdd/specs/<spec>/references`, no repositório principal) e
/// os arquivos válidos dele. Vazio sem referências.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct References {
    pub dir: PathBuf,
    pub files: Vec<String>,
}

impl References {
    /// Arquivos regulares de `<spec_dir>/references/` com nome de anexo válido, em ordem alfabética,
    /// até [`MAX_REFERENCES`]. Diretório ausente, symlink ou ilegível = sem referências.
    pub fn load(spec_dir: &Path) -> Self {
        let dir = spec_dir.join(REFERENCES_DIR);
        let mut files: Vec<String> = match fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => fs::read_dir(&dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|e| fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_file()))
                        .filter_map(|e| e.file_name().into_string().ok())
                        .filter(|n| attachment_mime(n).is_some())
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        files.sort();
        files.truncate(MAX_REFERENCES);
        Self { dir, files }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Nomes para `view_screenshot`: `referencia/<arquivo>`.
    pub fn view_names(&self) -> Vec<String> {
        self.files.iter().map(|f| format!("{VIEW_PREFIX}{f}")).collect()
    }

    /// Lê a referência pelo nome de `view_screenshot` (`referencia/<arquivo>`), só da lista.
    pub fn read(&self, view_name: &str) -> Result<(&'static str, Vec<u8>), String> {
        let file = view_name.strip_prefix(VIEW_PREFIX).unwrap_or(view_name);
        if !self.files.iter().any(|f| f == file) {
            return Err("referência fora da lista".into());
        }
        read_image(&self.dir, file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nresto";
    const WEBP: &[u8] = b"RIFF\x10\x00\x00\x00WEBPVP8 ";
    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-refs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".sdd/specs/.chat-attachments")).expect("anexos");
        fs::create_dir_all(dir.join(".sdd/specs/s1")).expect("spec");
        dir
    }

    fn attach(root: &Path, name: &str, bytes: &[u8]) {
        fs::write(root.join(CHAT_ATTACHMENTS_REL).join(name), bytes).expect("anexo");
    }

    #[test]
    fn names_follow_the_attachment_rule_and_the_limit() {
        assert_eq!(attachment_mime(&format!("{ID}.jpeg")), Some("image/jpeg"));
        assert_eq!(attachment_mime(&format!("{ID}.webp")), Some("image/webp"));
        for bad in [format!("{ID}.gif"), format!("{ID}.PNG"), format!("../{ID}.png"), format!("{}.png", &ID[1..])] {
            assert_eq!(attachment_mime(&bad), None, "{bad}");
        }
        let many: Vec<String> = (0..6).map(|i| format!("{}{i:x}.png", &ID[..31])).collect();
        let mut input = vec!["ruim.png".to_string(), many[0].clone()];
        input.extend(many.iter().cloned());
        assert_eq!(validate(&input), many[..MAX_REFERENCES].to_vec());
        assert_eq!(sniff(WEBP), Some("image/webp"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVE"), None);
    }

    #[test]
    fn copy_is_idempotent_and_ignores_missing_and_invalid() {
        let root = tmp("copy");
        let png = format!("{ID}.png");
        let webp = format!("{}.webp", "f".repeat(32));
        let fake = format!("{}.png", "a".repeat(32));
        attach(&root, &png, PNG);
        attach(&root, &webp, WEBP);
        attach(&root, &fake, b"nao e png");
        let missing = format!("{}.jpg", "b".repeat(32));
        let names = vec![png.clone(), missing, "x.png".into(), fake, webp.clone()];
        assert_eq!(copy_to_spec(&root, "s1", &names), vec![png.clone(), webp.clone()]);
        let refs = References::load(&root.join(".sdd/specs/s1"));
        assert_eq!(refs.files.len(), 2);
        assert_eq!(copy_to_spec(&root, "s1", &names), vec![png.clone(), webp.clone()]);
        let count = fs::read_dir(root.join(".sdd/specs/s1/references")).expect("dir").count();
        assert_eq!(count, 2, "aprovar de novo não duplica");
        assert_eq!(refs.read(&format!("referencia/{png}")).expect("lê").1, PNG);
        assert!(refs.read(&format!("referencia/{}.png", "c".repeat(32))).is_err());
        assert!(copy_to_spec(&root, "../s1", &names).is_empty());
        assert!(copy_to_spec(&root, "nao-existe", &names).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_refuses_symlinks_at_source_and_destination() {
        let root = tmp("link");
        let outside = root.join("fora.png");
        fs::write(&outside, PNG).expect("fora");
        let linked = format!("{ID}.png");
        symlink(&outside, root.join(CHAT_ATTACHMENTS_REL).join(&linked)).expect("link origem");
        assert!(copy_to_spec(&root, "s1", std::slice::from_ref(&linked)).is_empty());
        // Destino: um symlink no lugar da cópia não é seguido nem sobrescrito.
        let real = format!("{}.png", "d".repeat(32));
        attach(&root, &real, PNG);
        let target = root.join("alvo.png");
        fs::write(&target, b"intacto").expect("alvo");
        fs::create_dir_all(root.join(".sdd/specs/s1/references")).expect("refs");
        symlink(&target, root.join(".sdd/specs/s1/references").join(&real)).expect("link destino");
        assert!(copy_to_spec(&root, "s1", std::slice::from_ref(&real)).is_empty());
        assert_eq!(fs::read(&target).expect("alvo"), b"intacto");
        // Pasta de referências como symlink: nada é copiado nem listado.
        let other = root.join(".sdd/specs/s2");
        fs::create_dir_all(&other).expect("s2");
        symlink(root.join("fora-dir"), other.join(REFERENCES_DIR)).expect("link dir");
        assert!(copy_to_spec(&root, "s2", std::slice::from_ref(&real)).is_empty());
        assert!(References::load(&other).is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
