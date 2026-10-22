//! Imagens geradas pelo Desenvolvedor (spec 090 T02): cache e manifesto da spec.
//!
//! - Originais em `.sdd/specs/<spec>/images/<hash>.<ext>` (no repositório principal; as worktrees não
//!   têm `.sdd/specs`), com `hash` = SHA-256 de (guia + prompt + opções + modelo): a mesma combinação
//!   não gera de novo.
//! - Manifesto `.sdd/specs/<spec>/images.jsonl`, uma linha por imagem gravada numa worktree (nova ou
//!   reaproveitada), com o prompt já redigido e truncado. O limite de gerações novas por spec é
//!   contado por ele.
//! - [`SpecImages`] serve o Revisor: os FATOS listam as imagens da tarefa e `view_screenshot` abre o
//!   original do cache pelo caminho (ou só pelo nome do arquivo) do manifesto.
//!
//! Nada aqui segue symlinks: diretório da spec, pasta do cache, arquivos e manifesto que forem symlink
//! são recusados (ou ignorados na leitura).

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Subdiretório da spec com os originais.
pub const IMAGES_DIR: &str = "images";
/// Manifesto da spec.
pub const MANIFEST_FILE: &str = "images.jsonl";
/// Seção da spec anteposta ao prompt.
pub const GUIDE_SECTION: &str = "Guia de arte";
/// Teto (caracteres) do guia anteposto.
pub const GUIDE_MAX_CHARS: usize = 1500;
/// Máximo de gerações novas por spec (o cache não conta).
pub const MAX_NEW_PER_SPEC: usize = 12;
/// Tamanho máximo de uma imagem gerada.
pub const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;
/// Teto (caracteres) do prompt guardado no manifesto.
pub const MANIFEST_PROMPT_MAX_CHARS: usize = 300;
/// Teto (caracteres) do prompt nos FATOS da revisão.
pub const FACTS_PROMPT_MAX_CHARS: usize = 120;
/// Teto do manifesto: sem folga para mais uma linha ([`manifest_has_room`]), a geração é recusada.
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Folga exigida no manifesto antes de gerar (uma linha tem bem menos que isso).
pub const MANIFEST_LINE_RESERVE: u64 = 4 * 1024;
/// Máximo de imagens geradas abertas por sessão de Revisor (`view_screenshot`, à parte das capturas).
pub const MAX_VIEWS: usize = 8;
/// Máximo de linhas nos FATOS.
pub const MAX_FACTS: usize = 30;

/// Extensão aceita no `path` e o formato da API correspondente.
pub fn format_of_path(path: &str) -> Option<&'static str> {
    let (_, ext) = path.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "webp" => Some("webp"),
        "png" => Some("png"),
        "jpg" => Some("jpeg"),
        _ => None,
    }
}

/// Extensão do arquivo do cache para o formato da API.
pub fn ext_of_format(format: &str) -> &'static str {
    match format {
        "png" => "png",
        "jpeg" => "jpg",
        _ => "webp",
    }
}

/// Texto da seção "## Guia de arte" do spec.md (aparado, até [`GUIDE_MAX_CHARS`]); vazio sem ela.
pub fn art_guide(spec_md: &str) -> String {
    crate::agents::runner::section_text(spec_md, GUIDE_SECTION)
        .map(|s| s.trim().chars().take(GUIDE_MAX_CHARS).collect::<String>().trim_end().to_string())
        .unwrap_or_default()
}

/// Prompt enviado ao provedor: o guia (quando houver) antes do pedido.
pub fn compose_prompt(guide: &str, prompt: &str) -> String {
    if guide.is_empty() {
        prompt.to_string()
    } else {
        format!("Guia de arte (siga):\n{guide}\n\nImagem:\n{prompt}")
    }
}

/// Chave do cache: SHA-256 de (guia, prompt, tamanho, fundo, qualidade, formato, modelo), serializados
/// como lista JSON (sem ambiguidade de separador).
pub fn cache_key(guide: &str, prompt: &str, options: [&str; 4], model: &str) -> String {
    let [size, background, quality, format] = options;
    let parts = [guide, prompt, size, background, quality, format, model];
    let text = serde_json::to_string(&parts).unwrap_or_default();
    super::environment::sha256_hex(text.as_bytes())
}

/// Primeiros `max` caracteres, com `…` quando cortado.
pub fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Uma linha do manifesto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Caminho relativo gravado na worktree.
    pub path: String,
    pub hash: String,
    /// Prompt do agente (sem o guia), redigido e truncado em [`MANIFEST_PROMPT_MAX_CHARS`].
    pub prompt: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    pub bytes: u64,
    pub task: String,
    pub at: String,
    /// Copiada do cache, sem nova geração (não conta no limite).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reused: bool,
    /// `false` = geração cobrada que não chegou à worktree (recusada por tamanho/formato ou falha de
    /// gravação): conta no limite, mas não aparece nos FATOS nem em `view_screenshot`.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub stored: bool,
}

fn yes() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

impl ManifestEntry {
    /// Nome do original no cache (`<hash>.<ext>`).
    pub fn cache_name(&self) -> Option<String> {
        let format = format_of_path(&self.path)?;
        valid_hash(&self.hash).then(|| format!("{}.{}", self.hash, ext_of_format(format)))
    }

    /// "1024×1024" ou "?".
    pub fn dims(&self) -> String {
        dims_text(self.width.zip(self.height))
    }
}

/// "L×A" ou "?".
pub fn dims_text(dims: Option<(u32, u32)>) -> String {
    match dims {
        Some((w, h)) => format!("{w}×{h}"),
        None => "?".into(),
    }
}

/// Hash do cache: 64 hex minúsculos.
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Diretório real (não symlink).
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

fn nofollow(opts: &mut fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
}

/// Lê um arquivo regular sem seguir symlink, até `max` bytes (`None` se maior, ausente ou inseguro).
pub fn read_regular_nofollow(path: &Path, max: u64) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > max {
        return None;
    }
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    nofollow(&mut opts);
    let mut bytes = Vec::new();
    opts.open(path).ok()?.take(max + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= max).then_some(bytes)
}

/// Grava de forma atômica: temporário exclusivo na mesma pasta (`O_CREAT|O_EXCL|O_NOFOLLOW`),
/// `sync_all` e `rename` sobre o destino; em erro, o temporário é removido. Quem lê nunca vê o
/// arquivo pela metade, e um symlink no destino é substituído, nunca seguido.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "destino inválido");
    let parent = path.parent().ok_or_else(invalid)?;
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(invalid)?;
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = parent.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let staged = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        nofollow(&mut opts);
        let mut file = opts.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if staged.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    staged
}

/// A imagem está inteira: PNG termina no `IEND`, WebP tem o tamanho do RIFF igual ao do arquivo e
/// JPEG termina no `EOI`. Arquivo truncado no meio da escrita não passa.
pub fn complete(bytes: &[u8], format: &str) -> bool {
    match format {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.ends_with(b"IEND\xAE\x42\x60\x82"),
        "webp" => {
            let declared = bytes.get(4..8).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
            bytes.starts_with(b"RIFF")
                && bytes.get(8..12) == Some(b"WEBP".as_slice())
                && declared.is_some_and(|d| u64::from(d) + 8 == bytes.len() as u64)
        }
        "jpeg" => bytes.starts_with(&[0xFF, 0xD8]) && bytes.ends_with(&[0xFF, 0xD9]),
        _ => false,
    }
}

/// Linhas válidas do manifesto da spec (as ilegíveis são ignoradas). Manifesto ausente, symlink ou
/// acima de [`MAX_MANIFEST_BYTES`] = vazio.
pub fn read_manifest(spec_dir: &Path) -> Vec<ManifestEntry> {
    if !is_real_dir(spec_dir) {
        return Vec::new();
    }
    let Some(bytes) = read_regular_nofollow(&spec_dir.join(MANIFEST_FILE), MAX_MANIFEST_BYTES) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| serde_json::from_str::<ManifestEntry>(l).ok())
        .collect()
}

/// O manifesto aceita mais uma linha: a pasta da spec é real e o manifesto está ausente ou é regular
/// (não symlink) com folga de [`MANIFEST_LINE_RESERVE`] até [`MAX_MANIFEST_BYTES`]. Sem isso, nenhuma
/// geração é feita (o limite não poderia ser contado).
pub fn manifest_has_room(spec_dir: &Path) -> bool {
    if !is_real_dir(spec_dir) {
        return false;
    }
    match fs::symlink_metadata(spec_dir.join(MANIFEST_FILE)) {
        Ok(m) => m.is_file() && !m.file_type().is_symlink() && m.len() + MANIFEST_LINE_RESERVE <= MAX_MANIFEST_BYTES,
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    }
}

/// Gerações novas registradas no manifesto (gravadas ou não).
pub fn new_generations(entries: &[ManifestEntry]) -> usize {
    entries.iter().filter(|e| !e.reused).count()
}

/// Acrescenta uma linha ao manifesto (sem seguir symlink, até [`MAX_MANIFEST_BYTES`]).
pub fn append_manifest(spec_dir: &Path, entry: &ManifestEntry) -> io::Result<()> {
    if !is_real_dir(spec_dir) {
        return Err(io::Error::new(io::ErrorKind::NotFound, "pasta da spec ausente"));
    }
    let line = serde_json::to_string(entry).map_err(io::Error::other)?;
    let mut opts = fs::OpenOptions::new();
    opts.append(true).create(true);
    nofollow(&mut opts);
    let mut file = opts.open(spec_dir.join(MANIFEST_FILE))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "manifesto não é arquivo regular"));
    }
    if meta.len() + line.len() as u64 + 1 > MAX_MANIFEST_BYTES {
        return Err(io::Error::other("manifesto cheio"));
    }
    file.write_all(format!("{line}\n").as_bytes())
}

/// Pasta do cache da spec, criada se faltar; `None` se a spec não existir ou algo for symlink.
pub fn cache_dir(spec_dir: &Path) -> Option<PathBuf> {
    if !is_real_dir(spec_dir) {
        return None;
    }
    let dir = spec_dir.join(IMAGES_DIR);
    match fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Some(dir),
        Ok(_) => None,
        Err(_) => fs::create_dir(&dir).ok().map(|()| dir),
    }
}

/// Original do cache (`<hash>.<ext>`), se existir, for regular, couber no limite, tiver a assinatura
/// do formato e estiver inteiro ([`complete`]); senão, `None` (a chamada gera de novo).
pub fn read_cached(spec_dir: &Path, hash: &str, format: &str) -> Option<Vec<u8>> {
    if !valid_hash(hash) || !is_real_dir(spec_dir) || !is_real_dir(&spec_dir.join(IMAGES_DIR)) {
        return None;
    }
    let path = spec_dir.join(IMAGES_DIR).join(format!("{hash}.{}", ext_of_format(format)));
    let bytes = read_regular_nofollow(&path, MAX_IMAGE_BYTES)?;
    (super::references::sniff(&bytes) == Some(media_type(format)) && complete(&bytes, format)).then_some(bytes)
}

/// Grava o original no cache, de forma atômica.
pub fn store_cached(spec_dir: &Path, hash: &str, format: &str, bytes: &[u8]) -> io::Result<()> {
    let dir = cache_dir(spec_dir).ok_or_else(|| io::Error::other("cache indisponível"))?;
    write_atomic(&dir.join(format!("{hash}.{}", ext_of_format(format))), bytes)
}

/// Tipo MIME do formato da API.
pub fn media_type(format: &str) -> &'static str {
    match format {
        "png" => "image/png",
        "jpeg" => "image/jpeg",
        _ => "image/webp",
    }
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?])))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?])))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(at)?, *b.get(at + 1)?, *b.get(at + 2)?, 0]))
}

/// Largura × altura pelo cabeçalho PNG (IHDR), WebP (VP8, VP8L, VP8X) ou JPEG (SOFn); `None` se não
/// der para ler.
pub fn dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        if b.get(12..16)? != b"IHDR" {
            return None;
        }
        let w = u32::from_be_bytes(b.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(b.get(20..24)?.try_into().ok()?);
        return Some((w, h));
    }
    if b.len() >= 16 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        return match &b[12..16] {
            b"VP8 " => {
                if b.get(23..26)? != [0x9d, 0x01, 0x2a] {
                    return None;
                }
                Some((le16(b, 26)? & 0x3fff, le16(b, 28)? & 0x3fff))
            }
            b"VP8L" => {
                if *b.get(20)? != 0x2f {
                    return None;
                }
                let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => Some((le24(b, 24)? + 1, le24(b, 27)? + 1)),
            _ => None,
        };
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xFF {
                return None;
            }
            let marker = b[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
                i += 2;
                continue;
            }
            let len = be16(b, i + 2)? as usize;
            let sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
            if sof {
                return Some((be16(b, i + 7)?, be16(b, i + 5)?));
            }
            i += 2 + len;
        }
    }
    None
}

/// Imagens geradas de uma spec, para o Revisor: a pasta da spec e o manifesto lido. Vazio sem
/// manifesto.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecImages {
    pub spec_dir: PathBuf,
    pub entries: Vec<ManifestEntry>,
}

impl SpecImages {
    pub fn load(spec_dir: &Path) -> Self {
        Self { spec_dir: spec_dir.to_path_buf(), entries: read_manifest(spec_dir) }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Imagens gravadas por caminho (a última linha de cada caminho vence), na ordem da primeira
    /// aparição. Com `task`, só as gravadas nela. Gerações não gravadas ficam de fora.
    pub fn latest(&self, task: Option<&str>) -> Vec<&ManifestEntry> {
        let mut out: Vec<&ManifestEntry> = Vec::new();
        for e in self.entries.iter().filter(|e| e.stored && task.is_none_or(|t| e.task == t)) {
            match out.iter_mut().find(|o| o.path == e.path) {
                Some(slot) => *slot = e,
                None => out.push(e),
            }
        }
        out
    }

    /// Caminhos que `view_screenshot` aceita (todos os da spec).
    pub fn view_names(&self) -> Vec<String> {
        self.latest(None).iter().map(|e| e.path.clone()).collect()
    }

    /// Resolve o nome pedido: o caminho do manifesto ou, se único, só o nome do arquivo.
    pub fn resolve(&self, name: &str) -> Option<&ManifestEntry> {
        let all = self.latest(None);
        if let Some(exact) = all.iter().find(|e| e.path == name) {
            return Some(exact);
        }
        let mut matches = all.iter().filter(|e| e.path.rsplit('/').next() == Some(name));
        match (matches.next(), matches.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }

    /// Lê o original do cache de uma entrada: `(mime, bytes)`.
    pub fn read(&self, entry: &ManifestEntry) -> Result<(&'static str, Vec<u8>), String> {
        let format = format_of_path(&entry.path).ok_or("extensão de imagem inválida")?;
        read_cached(&self.spec_dir, &entry.hash, format)
            .map(|bytes| (media_type(format), bytes))
            .ok_or_else(|| "imagem gerada ausente ou inválida no cache da spec".to_string())
    }

    /// Seção dos FATOS: as imagens da tarefa (`task`) ou da spec inteira (`None`, verificação final),
    /// com caminho, dimensões e prompt curto. Vazia sem imagens.
    pub fn facts(&self, task: Option<&str>) -> String {
        let list = self.latest(task);
        if list.is_empty() {
            return String::new();
        }
        let title = match task {
            Some(_) => "Imagens geradas nesta tarefa",
            None => "Imagens geradas nesta spec",
        };
        let mut out = format!(
            "## {title} (DADO; abra com view_screenshot pelo caminho; até {MAX_VIEWS})\n"
        );
        for e in list.iter().take(MAX_FACTS) {
            out.push_str(&format!(
                "- {} ({}): \"{}\"\n",
                e.path,
                e.dims(),
                clip(&e.prompt, FACTS_PROMPT_MAX_CHARS)
            ));
        }
        if list.len() > MAX_FACTS {
            out.push_str(&format!("- … mais {}\n", list.len() - MAX_FACTS));
        }
        out
    }
}

/// Extensões de imagem que o plano pode citar como imagem a gerar (spec 092 T03).
const PLANNED_EXTS: [&str; 4] = ["png", "webp", "jpg", "jpeg"];
/// Trechos de caminho de capturas e relatórios de teste (não são imagens planejadas).
/// Também as referências do usuário (`referencia/<anexo>` de `view_screenshot`, `references/`).
const NOT_PLANNED_MARKERS: [&str; 8] = [
    "test-results/",
    "playwright-report/",
    "screenshot",
    "captura",
    "capture",
    "referencia/",
    "referência/",
    "references/",
];

/// Motivos fixos das recusas de `generate_image` que não são falha do provedor (spec 092 T03).
pub const REASON_LIMIT: &str = "limite de imagens da spec";
pub const REASON_REGISTRY_FULL: &str = "registro de imagens cheio";
pub const REASON_BLOCKED: &str = "geração suspensa na sessão";
pub const REASON_FORMAT: &str = "formato divergente do provedor";

/// Recusa determinística da spec, vista do disco (sobrevive ao reinício): limite de gerações novas
/// atingido ou manifesto sem folga. `None` = a spec ainda aceita gerações.
pub fn spec_refusal(spec_dir: &Path) -> Option<&'static str> {
    if !manifest_has_room(spec_dir) {
        Some(REASON_REGISTRY_FULL)
    } else if new_generations(&read_manifest(spec_dir)) >= MAX_NEW_PER_SPEC {
        Some(REASON_LIMIT)
    } else {
        None
    }
}

/// Caminhos de imagem planejados citados num texto (objetivo + critérios da tarefa), na ordem da
/// primeira aparição e sem repetição: relativos com pasta (sem `/` inicial, URL, `..` nem nome
/// solto), com extensão `.png`/`.webp`/`.jpg`/`.jpeg`, sem glob e fora de capturas, relatórios de
/// teste e referências do usuário. `./` inicial é removido.
pub fn planned_paths(text: &str) -> Vec<String> {
    let is_path_char = |c: char| c.is_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | '@' | '+');
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !is_path_char(c)) {
        let token = raw.trim_end_matches('.');
        let token = token.strip_prefix("./").unwrap_or(token);
        if token.is_empty() || token.starts_with('/') || token.starts_with('.') {
            continue;
        }
        let Some((stem, ext)) = token.rsplit_once('.') else { continue };
        let file_stem = stem.rsplit('/').next().unwrap_or_default();
        if file_stem.is_empty() || !PLANNED_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            continue;
        }
        if !token.contains('/') || token.split('/').any(|seg| seg.is_empty() || seg == "..") {
            continue;
        }
        let lower = token.to_lowercase();
        if NOT_PLANNED_MARKERS.iter().any(|m| lower.contains(m)) {
            continue;
        }
        if !out.iter().any(|p| p == token) {
            out.push(token.to_string());
        }
    }
    out
}

/// Disponibilidade da geração de imagens na execução do Desenvolvedor (spec 092 T03).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GenerationStatus {
    Available,
    #[default]
    Unavailable,
    /// Falha final registrada (motivo fixo, já saneado).
    Failed(String),
}

impl GenerationStatus {
    pub fn text(&self) -> String {
        match self {
            GenerationStatus::Available => "disponível".into(),
            GenerationStatus::Unavailable => "indisponível".into(),
            GenerationStatus::Failed(reason) => format!("falhou ({reason})"),
        }
    }
}

/// Imagens planejadas da tarefa e a disponibilidade da geração (FATOS da revisão).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlannedImages {
    pub paths: Vec<String>,
    pub status: GenerationStatus,
}

/// Lista curta de caminhos (até [`MAX_FACTS`]).
fn path_list(paths: &[&str]) -> String {
    let mut text = paths.iter().take(MAX_FACTS).copied().collect::<Vec<_>>().join(", ");
    if paths.len() > MAX_FACTS {
        text.push_str(&format!(", … mais {}", paths.len() - MAX_FACTS));
    }
    text
}

impl SpecImages {
    /// Seção dos FATOS com as imagens planejadas × geradas na tarefa `task` e a disponibilidade da
    /// geração. Vazia sem imagens planejadas. Planejada já gerada em outra tarefa da spec não falta.
    pub fn planned_facts(&self, task: &str, planned: &PlannedImages) -> String {
        if planned.paths.is_empty() {
            return String::new();
        }
        let here: Vec<&str> = self.latest(Some(task)).iter().map(|e| e.path.as_str()).collect();
        let spec: Vec<&str> = self.latest(None).iter().map(|e| e.path.as_str()).collect();
        let generated = planned.paths.iter().filter(|p| here.contains(&p.as_str())).count();
        let elsewhere: Vec<&str> = planned
            .paths
            .iter()
            .map(String::as_str)
            .filter(|p| !here.contains(p) && spec.contains(p))
            .collect();
        let missing: Vec<&str> = planned
            .paths
            .iter()
            .map(String::as_str)
            .filter(|p| !here.contains(p) && !spec.contains(p))
            .collect();
        let missing_text = if missing.is_empty() { "nenhuma".to_string() } else { path_list(&missing) };
        let mut out = format!(
            "## Imagens planejadas nesta tarefa (DADO)\n- Imagens planejadas: {}; geradas nesta tarefa: {generated}; faltando: {missing_text}\n",
            planned.paths.len()
        );
        if !elsewhere.is_empty() {
            out.push_str(&format!("- Já geradas em outra tarefa desta spec: {}\n", path_list(&elsewhere)));
        }
        out.push_str(&format!("- Geração de imagens nesta execução: {}\n", planned.status.text()));
        out
    }
}

#[cfg(test)]
pub(crate) mod testing {
    /// PNG mínimo de `w`×`h`: assinatura, IHDR e IEND (inteiro para [`super::complete`]).
    pub fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b.extend_from_slice(b"\0\0\0\0IEND\xAE\x42\x60\x82");
        b
    }

    /// WebP lossy (VP8) mínimo de `w`×`h`, com o tamanho do RIFF correto.
    pub fn webp(w: u16, h: u16) -> Vec<u8> {
        let mut b = b"RIFF\0\0\0\0WEBPVP8 \x0a\0\0\0".to_vec();
        b.extend_from_slice(&[0, 0, 0, 0x9d, 0x01, 0x2a]);
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        let size = u32::try_from(b.len() - 8).unwrap_or(0);
        b[4..8].copy_from_slice(&size.to_le_bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{png, webp};
    use super::*;
    use std::os::unix::fs::symlink;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-images-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("spec dir");
        dir
    }

    fn entry(path: &str, task: &str, reused: bool) -> ManifestEntry {
        ManifestEntry {
            path: path.into(),
            hash: "a".repeat(64),
            prompt: "um gato astronauta".into(),
            width: Some(1024),
            height: Some(1536),
            bytes: 10,
            task: task.into(),
            at: "2026-10-19T00:00:00Z".into(),
            reused,
            stored: true,
        }
    }

    #[test]
    fn planned_paths_keeps_relative_image_paths_only() {
        let text = "## Objetivo\nGere `web/img/hero.webp` e ./web/img/planetas/terra.png.\n\
            ## Critérios de Aceitação\n- web/img/hero.webp existe (repetido).\n\
            - foto em assets/Foto.JPG e assets/b.jpeg; ícone web/icon.svg não conta.\n\
            - captura test-results/screens/home.png e e2e/screenshots/a.png e docs/captura-1.png não contam.\n\
            - absoluto /tmp/x.png, URL https://cdn.exemplo.com/y.png, ../fora.png, glob web/**/*.png, .png só.\n\
            - pageerror/console.error não é imagem; nome solto hero.png não conta.\n\
            - igual à referencia/mockup.png e docs/references/tela.png do usuário.";
        assert_eq!(
            planned_paths(text),
            vec!["web/img/hero.webp", "web/img/planetas/terra.png", "assets/Foto.JPG", "assets/b.jpeg"]
        );
        assert!(planned_paths("Sem imagens; só texto e logo.svg.").is_empty());
    }

    #[test]
    fn spec_refusal_sees_the_limit_and_a_full_registry() {
        let dir = tmp("refusal");
        assert_eq!(spec_refusal(&dir), None);
        for i in 0..MAX_NEW_PER_SPEC {
            append_manifest(&dir, &entry(&format!("web/{i}.png"), "T01", false)).expect("linha");
        }
        assert_eq!(spec_refusal(&dir), Some(REASON_LIMIT));
        fs::write(dir.join(MANIFEST_FILE), vec![b'\n'; (MAX_MANIFEST_BYTES - 100) as usize]).expect("cheio");
        assert_eq!(spec_refusal(&dir), Some(REASON_REGISTRY_FULL));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn planned_facts_count_missing_and_status() {
        let dir = tmp("planned");
        append_manifest(&dir, &entry("web/img/hero.webp", "T03", false)).expect("1");
        append_manifest(&dir, &entry("web/img/logo.webp", "T01", false)).expect("2");
        let imgs = SpecImages::load(&dir);
        let planned = PlannedImages {
            paths: vec!["web/img/hero.webp".into(), "web/img/terra.webp".into(), "web/img/logo.webp".into(), "web/img/lua.webp".into()],
            status: GenerationStatus::Available,
        };
        let facts = imgs.planned_facts("T03", &planned);
        assert!(facts.contains("Imagens planejadas: 4; geradas nesta tarefa: 1; faltando: web/img/terra.webp, web/img/lua.webp\n"), "{facts}");
        assert!(facts.contains("- Já geradas em outra tarefa desta spec: web/img/logo.webp\n"), "{facts}");
        assert!(facts.contains("- Geração de imagens nesta execução: disponível\n"), "{facts}");
        let failed = PlannedImages { status: GenerationStatus::Failed("tempo esgotado".into()), ..planned.clone() };
        assert!(imgs.planned_facts("T03", &failed).contains("Geração de imagens nesta execução: falhou (tempo esgotado)"));
        let off = PlannedImages { paths: vec!["web/img/hero.webp".into()], status: GenerationStatus::Unavailable };
        let facts = imgs.planned_facts("T03", &off);
        assert!(facts.contains("geradas nesta tarefa: 1; faltando: nenhuma") && facts.contains("nesta execução: indisponível"), "{facts}");
        // Sem imagens planejadas, nada muda.
        assert_eq!(imgs.planned_facts("T03", &PlannedImages::default()), "");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dimensions_of_png_webp_and_jpeg() {
        assert_eq!(dimensions(&png(1536, 1024)), Some((1536, 1024)));
        assert_eq!(dimensions(&webp(1024, 1536)), Some((1024, 1536)));
        // VP8L: largura-1 nos 14 bits baixos, altura-1 nos 14 seguintes.
        let bits: u32 = (99) | (49 << 14);
        let mut lossless = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
        lossless.extend_from_slice(&bits.to_le_bytes());
        assert_eq!(dimensions(&lossless), Some((100, 50)));
        let mut ext = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        ext.extend_from_slice(&[0xff, 0x03, 0x00, 0xff, 0x05, 0x00]);
        assert_eq!(dimensions(&ext), Some((1024, 1536)));
        // JPEG: APP0 e depois SOF0 (altura 600, largura 800).
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x02, 0x58, 0x03, 0x20,
        ];
        assert_eq!(dimensions(&jpeg), Some((800, 600)));
        assert_eq!(dimensions(b"\x89PNG\r\n\x1a\nxx"), None);
        assert_eq!(dimensions(b"nada"), None);
        assert_eq!(dims_text(None), "?");
    }

    #[test]
    fn guide_is_the_section_text_capped() {
        let spec = "## Contexto\nc\n## Guia de arte\nAquarela, traço fino.\n\n## Tarefas\n- T01\n";
        assert_eq!(art_guide(spec), "Aquarela, traço fino.");
        assert_eq!(art_guide("## Contexto\nc\n"), "");
        let long = format!("## Guia de arte\n{}\n", "é".repeat(2000));
        assert_eq!(art_guide(&long).chars().count(), GUIDE_MAX_CHARS);
        assert_eq!(compose_prompt("", "um gato"), "um gato");
        let full = compose_prompt("aquarela", "um gato");
        assert!(full.starts_with("Guia de arte") && full.find("aquarela") < full.find("um gato"), "{full}");
    }

    #[test]
    fn cache_key_changes_with_every_input() {
        let base = cache_key("g", "p", ["auto", "auto", "medium", "webp"], "m");
        assert_eq!(base.len(), 64);
        assert_eq!(base, cache_key("g", "p", ["auto", "auto", "medium", "webp"], "m"));
        for other in [
            cache_key("g2", "p", ["auto", "auto", "medium", "webp"], "m"),
            cache_key("g", "p2", ["auto", "auto", "medium", "webp"], "m"),
            cache_key("g", "p", ["1024x1024", "auto", "medium", "webp"], "m"),
            cache_key("g", "p", ["auto", "auto", "high", "webp"], "m"),
            cache_key("g", "p", ["auto", "auto", "medium", "png"], "m"),
            cache_key("g", "p", ["auto", "auto", "medium", "webp"], "m2"),
            // Sem ambiguidade de separador.
            cache_key("gp", "", ["auto", "auto", "medium", "webp"], "m"),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn manifest_roundtrip_counts_and_resolves() {
        let dir = tmp("manifest");
        append_manifest(&dir, &entry("web/img/gato.webp", "T01", false)).expect("1");
        append_manifest(&dir, &entry("web/img/gato.webp", "T02", true)).expect("2");
        append_manifest(&dir, &entry("web/fundo.png", "T02", false)).expect("3");
        fs::write(dir.join("lixo"), "x").expect("lixo");
        let mut text = fs::read_to_string(dir.join(MANIFEST_FILE)).expect("ler");
        assert!(!text.lines().next().unwrap_or_default().contains("reused"), "{text}");
        text.push_str("linha ilegível\n");
        fs::write(dir.join(MANIFEST_FILE), text).expect("gravar");
        let imgs = SpecImages::load(&dir);
        assert_eq!(imgs.entries.len(), 3);
        assert_eq!(new_generations(&imgs.entries), 2);
        assert_eq!(imgs.latest(Some("T02")).len(), 2);
        assert_eq!(imgs.latest(None).len(), 2, "um por caminho");
        assert_eq!(imgs.resolve("gato.webp").map(|e| e.task.as_str()), Some("T02"));
        assert!(imgs.resolve("web/fundo.png").is_some());
        assert!(imgs.resolve("outra.png").is_none());
        let facts = imgs.facts(Some("T01"));
        assert!(facts.contains("- web/img/gato.webp (1024×1536): \"um gato astronauta\""), "{facts}");
        assert!(!facts.contains("fundo"));
        assert!(imgs.facts(Some("T09")).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unstored_generations_count_but_are_not_listed() {
        let dir = tmp("unstored");
        let mut charged = entry("web/grande.png", "T01", false);
        charged.stored = false;
        append_manifest(&dir, &charged).expect("cobrada");
        append_manifest(&dir, &entry("web/ok.png", "T01", false)).expect("gravada");
        let raw = fs::read_to_string(dir.join(MANIFEST_FILE)).expect("ler");
        assert_eq!(raw.matches("\"stored\":false").count(), 1, "{raw}");
        let imgs = SpecImages::load(&dir);
        assert_eq!(new_generations(&imgs.entries), 2);
        assert_eq!(imgs.view_names(), vec!["web/ok.png".to_string()]);
        assert!(imgs.resolve("grande.png").is_none() && !imgs.facts(None).contains("grande"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_room_requires_a_real_dir_and_spare_bytes() {
        let dir = tmp("room");
        assert!(manifest_has_room(&dir), "manifesto ausente");
        fs::write(dir.join(MANIFEST_FILE), vec![b'x'; (MAX_MANIFEST_BYTES - MANIFEST_LINE_RESERVE) as usize]).expect("cheio");
        assert!(manifest_has_room(&dir), "no limite da folga");
        fs::write(dir.join(MANIFEST_FILE), vec![b'x'; (MAX_MANIFEST_BYTES - 100) as usize]).expect("cheio");
        assert!(!manifest_has_room(&dir));
        fs::remove_file(dir.join(MANIFEST_FILE)).expect("rm");
        fs::create_dir(dir.join(MANIFEST_FILE)).expect("dir no lugar");
        assert!(!manifest_has_room(&dir));
        assert!(!manifest_has_room(&dir.join("nao-existe")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_writes_are_atomic_and_truncated_files_are_not_served() {
        let dir = tmp("atomic");
        let hash = "d".repeat(64);
        for (format, bytes) in [("png", png(4, 4)), ("webp", webp(4, 4))] {
            assert!(complete(&bytes, format), "{format}");
            store_cached(&dir, &hash, format, &bytes).expect("grava");
            assert_eq!(read_cached(&dir, &hash, format), Some(bytes.clone()));
            // Sobrescrita também atômica; nenhum temporário sobra.
            store_cached(&dir, &hash, format, &bytes).expect("regrava");
            let names: Vec<String> = fs::read_dir(dir.join(IMAGES_DIR))
                .expect("dir")
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
            // Truncado (escrita interrompida): não é servido.
            let path = dir.join(IMAGES_DIR).join(format!("{hash}.{}", ext_of_format(format)));
            fs::write(&path, &bytes[..bytes.len() - 3]).expect("trunca");
            assert!(read_cached(&dir, &hash, format).is_none(), "{format}");
        }
        assert!(complete(&[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xD9], "jpeg"));
        assert!(!complete(&[0xFF, 0xD8, 0xFF, 0xE0], "jpeg"));
        // Destino symlink: substituído pelo arquivo, o alvo fica intacto.
        let outside = tmp("atomic-out");
        fs::write(outside.join("alvo"), b"intacto").expect("alvo");
        let link = dir.join("link.png");
        symlink(outside.join("alvo"), &link).expect("link");
        write_atomic(&link, b"novo").expect("grava");
        assert_eq!(fs::read(outside.join("alvo")).expect("alvo"), b"intacto");
        assert!(!fs::symlink_metadata(&link).expect("meta").file_type().is_symlink());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn cache_and_manifest_refuse_symlinks() {
        let dir = tmp("links");
        let outside = tmp("links-out");
        let hash = "b".repeat(64);
        // Pasta do cache como symlink: nada é lido nem gravado.
        symlink(&outside, dir.join(IMAGES_DIR)).expect("link dir");
        assert!(store_cached(&dir, &hash, "png", &png(1, 1)).is_err());
        fs::write(outside.join(format!("{hash}.png")), png(1, 1)).expect("fora");
        assert!(read_cached(&dir, &hash, "png").is_none());
        fs::remove_file(dir.join(IMAGES_DIR)).expect("rm link");
        // Original como symlink: ignorado.
        fs::create_dir(dir.join(IMAGES_DIR)).expect("dir");
        symlink(outside.join(format!("{hash}.png")), dir.join(IMAGES_DIR).join(format!("{hash}.png"))).expect("link");
        assert!(read_cached(&dir, &hash, "png").is_none());
        // Manifesto como symlink: não lido nem acrescentado.
        fs::write(outside.join("m.jsonl"), "").expect("m");
        symlink(outside.join("m.jsonl"), dir.join(MANIFEST_FILE)).expect("link m");
        assert!(append_manifest(&dir, &entry("a.png", "T01", false)).is_err());
        assert!(read_manifest(&dir).is_empty());
        assert_eq!(fs::read_to_string(outside.join("m.jsonl")).expect("m"), "");
        // Conteúdo que não confere com o formato: ignorado.
        let other = "c".repeat(64);
        fs::write(dir.join(IMAGES_DIR).join(format!("{other}.webp")), png(1, 1)).expect("png como webp");
        assert!(read_cached(&dir, &other, "webp").is_none());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }
}
