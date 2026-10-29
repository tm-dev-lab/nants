//! Base de conhecimento por projeto (spec 134/T01): armazenamento em `.sdd/kb/`.
//!
//! Estrutura, toda fora do Git do projeto (linha [`KB_EXCLUDE_LINE`] no `.git/info/exclude`):
//! - `sources/<id>.<ext>`: o original enviado, intocado;
//! - `notes/<id>.md`: o texto em markdown com frontmatter (`title`, `kind`, `source`,
//!   `source_url`, `added_at`, `fetched_at`, `sha256`, `pages`, `extractor`, `status`). As notas
//!   da 134 trazem também `role`, que a leitura ignora (só o corpo depois do frontmatter conta).
//!   Markdown e texto nascem prontos, com o texto normalizado; PDF e link nascem `pending` e a
//!   fila de extração ([`crate::server::kb_queue`], T02) passa por `extracting` até `ready`,
//!   `no_text` ou `error` (com o motivo codificado em `status_reason`);
//! - `manifest.json`: a lista dos documentos ([`Manifest`]), gravada de forma atômica (temporário
//!   e `rename`) sob [`KB_GUARD`], sem corrida entre envios simultâneos;
//! - `overview.md`: a "Visão geral da base", escrita pela pessoa (C-20), até
//!   [`MAX_OVERVIEW_BYTES`].
//!
//! Regras de segurança:
//! - nenhum componente do caminho pode ser symlink (todo I/O passa por `safe_fs`, com
//!   `O_NOFOLLOW`); um alvo que não é arquivo regular é recusado;
//! - nomes típicos de segredo (`.env`, chaves PEM e afins, [`is_secret_name`]) e conteúdo com
//!   chave privada PEM ou com um segredo configurado são recusados na entrada;
//! - o id é estável e seguro para caminho ([`valid_id`]): `slug-<hex do sha256>`;
//! - o texto gravado na nota já sai redigido pelo `SecretSet`, e a leitura redige de novo.
//!
//! Spec 136 T10, documentos escritos no Nants ([`add_written`] e [`edit_content`]): o documento
//! escrito é um markdown com `source: "written"`, gravado pelo mesmo caminho (original em
//! `sources/<id>.md`, nota, manifesto, a mesma recusa de segredo e a mesma redação). A edição vale
//! para ele e para os enviados em markdown ou texto, cujo original é o próprio texto; PDF e link
//! são extração de um original e continuam só leitura.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::server::events::SecretSet;
use crate::server::projects::scrub_text;
use crate::server::safe_fs::{self, SafeFsError, O_NOFOLLOW};

pub mod catalog;
pub mod cite;
pub mod extract;
pub mod index;
pub mod indicators;
pub mod mention;
#[cfg(test)]
pub(crate) mod test_pdf;

/// Pasta da base, relativa à raiz do projeto.
pub const KB_REL: &str = ".sdd/kb";
/// Originais enviados.
pub const SOURCES_REL: &str = ".sdd/kb/sources";
/// Notas em markdown.
pub const NOTES_REL: &str = ".sdd/kb/notes";
/// Manifesto dos documentos.
pub const MANIFEST_REL: &str = ".sdd/kb/manifest.json";
/// Visão geral da base, escrita pela pessoa.
pub const OVERVIEW_REL: &str = ".sdd/kb/overview.md";
/// Linha do exclude local que tira a base inteira do Git.
pub const KB_EXCLUDE_LINE: &str = ".sdd/kb/";

/// Teto técnico de um arquivo enviado (32 MiB): também o limite de corpo da rota de envio. Cabe
/// com folga um PDF de padrões de várias centenas de páginas; o corpo fica em memória durante o
/// envio, então o teto existe para proteger o servidor, não para limitar a base.
pub const MAX_UPLOAD_BYTES: usize = 32 * 1024 * 1024;
/// Teto de leitura de uma nota: o maior texto aceito mais folga para o frontmatter.
pub const MAX_NOTE_BYTES: u64 = MAX_UPLOAD_BYTES as u64 + 64 * 1024;
/// Teto de leitura do manifesto (8 MiB, milhares de documentos).
pub const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
/// Teto técnico da visão geral (32 KiB): é um texto curto que vai inteiro ao catálogo.
pub const MAX_OVERVIEW_BYTES: usize = 32 * 1024;
/// Tamanho máximo da descrição de uma linha (C-19).
pub const MAX_DESCRIPTION_CHARS: usize = 200;
/// Tamanho máximo do título derivado do nome do arquivo ou do link.
pub const MAX_TITLE_CHARS: usize = 200;
/// Tamanho máximo de um link.
pub const MAX_URL_BYTES: usize = 2048;
/// Tamanho máximo do nome do arquivo enviado.
pub const MAX_FILE_NAME_CHARS: usize = 255;
/// Tamanho máximo de um id.
pub const MAX_ID_CHARS: usize = 64;
/// Tamanho máximo do slug no início do id.
const SLUG_MAX_CHARS: usize = 40;
/// Teto do texto de um documento escrito ou editado no Nants (2 MiB): folgado para um documento
/// escrito à mão, e o corpo vai inteiro num JSON (o limite da rota é [`MAX_WRITTEN_BODY_BYTES`]).
pub const MAX_WRITTEN_BYTES: usize = 2 * 1024 * 1024;
/// Limite de corpo das rotas de escrita (`POST /kb/written` e `PUT /kb/docs/:doc/content`): o
/// dobro do texto, para o escape do JSON, mais folga para o título e a versão.
pub const MAX_WRITTEN_BODY_BYTES: usize = 2 * MAX_WRITTEN_BYTES + 64 * 1024;
/// Versão do formato do manifesto.
pub const MANIFEST_VERSION: u32 = 1;

/// `ELOOP` no Linux: o último componente é symlink e a abertura usou `O_NOFOLLOW`.
const ELOOP: i32 = 40;

/// Extensões possíveis do original em `sources/` (a T02 grava o HTML dos links).
const SOURCE_EXTS: [&str; 4] = ["md", "txt", "pdf", "html"];

/// Serializa as mutações da base (manifesto, notas e originais) entre pedidos simultâneos.
static KB_GUARD: Mutex<()> = Mutex::new(());

/// Tipo do documento.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Markdown,
    Text,
    Pdf,
    Link,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Text => "text",
            Self::Pdf => "pdf",
            Self::Link => "link",
        }
    }

    /// Extensão do original em `sources/` (`None` para link, gravado pela T02).
    fn source_ext(self) -> Option<&'static str> {
        match self {
            Self::Markdown => Some("md"),
            Self::Text => Some("txt"),
            Self::Pdf => Some("pdf"),
            Self::Link => None,
        }
    }

    /// Tipo pela extensão do nome enviado.
    fn from_file_name(name: &str) -> Option<Self> {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
        match ext.as_str() {
            "md" | "markdown" => Some(Self::Markdown),
            "txt" | "text" => Some(Self::Text),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

/// Estado da extração (T02): `pending` → `extracting` → `ready`, `no_text` ou `error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Extracting,
    Ready,
    /// Sem texto aproveitável (PDF escaneado, página que depende de JavaScript).
    NoText,
    #[serde(alias = "failed")]
    Error,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Extracting => "extracting",
            Self::Ready => "ready",
            Self::NoText => "no_text",
            Self::Error => "error",
        }
    }

    /// Ainda na fila (ou interrompido no meio por um reinício do servidor).
    pub fn is_queued(self) -> bool {
        matches!(self, Self::Pending | Self::Extracting)
    }
}

/// Origem do documento: arquivo enviado, link ou escrito no Nants (spec 136 T10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    File,
    Url,
    Written,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Url => "url",
            Self::Written => "written",
        }
    }
}

/// Um documento do manifesto.
///
/// Spec 136 T02 (P2): sem classificação manual. Os campos `role` e `applies_to` dos manifestos da
/// 134 não existem mais aqui: a leitura os ignora (a struct não recusa campo desconhecido) e a
/// próxima gravação do manifesto os descarta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    pub source: Origin,
    /// Nome original do arquivo enviado.
    #[serde(default)]
    pub file_name: Option<String>,
    /// Link, para os documentos de origem `url`.
    #[serde(default)]
    pub source_url: Option<String>,
    /// Tamanho do original em bytes (0 para link ainda não lido).
    #[serde(default)]
    pub size: u64,
    /// SHA-256 do original, em hexadecimal (vazio para link ainda não lido).
    #[serde(default)]
    pub sha256: String,
    pub added_at: String,
    /// Quando o link foi lido (T02).
    #[serde(default)]
    pub fetched_at: Option<String>,
    pub status: Status,
    /// Motivo codificado de `no_text` e `error` ([`extract::reason`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    /// Páginas do PDF (T02).
    #[serde(default)]
    pub pages: Option<u32>,
    /// Extrator que gerou a nota (`raw` para markdown e texto).
    #[serde(default)]
    pub extractor: Option<String>,
    /// Descrição de uma linha, escrita pela pessoa (C-19); a T02 preenche um rascunho extraído.
    #[serde(default)]
    pub description: String,
    /// Ids dos documentos da base citados por este (C-22), recalculados a cada mudança da base.
    #[serde(default)]
    pub links_out: Vec<String>,
    /// Alvos dos links do documento (`url:<endereço>` ou `file:<nome>`), a base de `links_out`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_targets: Vec<String>,
}

/// O manifesto da base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub docs: Vec<Entry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self { version: MANIFEST_VERSION, docs: Vec::new() }
    }
}

/// Falha de uma operação da base. Nunca carrega caminhos nem conteúdo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KbError {
    Fs(SafeFsError),
    /// Já existe documento com o mesmo conteúdo (ou o mesmo link): o id dele.
    Duplicate(String),
    /// Nome típico de arquivo de segredo.
    SecretName,
    /// Conteúdo com chave privada ou com um segredo configurado.
    SecretContent,
    /// Extensão fora de md, txt e pdf, ou conteúdo que não confere com ela.
    Unsupported,
    /// Markdown ou texto que não é UTF-8 (ou tem byte nulo).
    NotText,
    /// Arquivo vazio.
    Empty,
    /// Nome de arquivo ausente ou inválido.
    BadName,
    /// Link inválido (esquema, tamanho, sem host).
    BadUrl,
    /// Link com usuário ou senha.
    UrlCredentials,
    /// Documento inexistente.
    NotFound,
    /// Manifesto ilegível.
    Corrupt,
    /// Descrição com quebra de linha ou caractere de controle.
    BadDescription,
    /// Descrição acima de [`MAX_DESCRIPTION_CHARS`].
    DescriptionTooLong,
    /// Visão geral acima de [`MAX_OVERVIEW_BYTES`].
    OverviewTooLarge,
    /// Título do documento escrito vazio (spec 136 T10).
    TitleEmpty,
    /// Título acima de [`MAX_TITLE_CHARS`].
    TitleTooLong,
    /// Já existe documento com o mesmo título: o id dele.
    TitleTaken(String),
    /// Texto escrito acima de [`MAX_WRITTEN_BYTES`].
    WrittenTooLarge,
    /// A versão (`base_sha256`) enviada não é mais a atual: outra edição gravou antes.
    EditConflict,
    /// PDF e link: o texto é extração de um original e não se edita.
    NotEditable,
    /// O original de um editável mudaria com a redação (segredo configurado, credencial num
    /// endereço): não sai redigido, para o editor não gravar o texto redigido por cima do original.
    SourceRedacted,
}

impl From<SafeFsError> for KbError {
    fn from(e: SafeFsError) -> Self {
        Self::Fs(e)
    }
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    KB_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// --- ids, nomes e textos ---

/// Id seguro para caminho: `[a-z0-9-]`, começa com letra ou dígito, até [`MAX_ID_CHARS`].
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_CHARS
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && id.as_bytes().first().is_some_and(|b| *b != b'-')
}

/// Letra latina sem o acento (á → a, ç → c); outros caracteres ficam como estão.
pub fn fold_accent(ch: char) -> char {
    match ch {
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
        other => other,
    }
}

/// Slug `[a-z0-9-]` de até [`SLUG_MAX_CHARS`], com os acentos dobrados. Vazio vira `doc`.
fn slugify(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars().map(fold_accent).flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= SLUG_MAX_CHARS {
            break;
        }
    }
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        "doc".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Id estável: slug + `-` + 8 hex do hash; se já estiver em uso por outro documento, 16 hex.
/// Tamanhos possíveis do sufixo hexadecimal do id (`<slug>-<hex>`): 8 e, no desempate, 16 ou 32.
pub const ID_SUFFIX_LENS: [usize; 3] = [8, 16, 32];

/// `true` se `id` tem a forma de um id da base, como [`make_id`] o gera: um id válido
/// ([`valid_id`]) `<slug>-<sufixo>`, com slug não vazio e sufixo de [`ID_SUFFIX_LENS`] dígitos
/// hexadecimais minúsculos (spec 134 T05: `kb:session` ou `kb:user` não são citação).
pub fn is_doc_id(id: &str) -> bool {
    let Some((slug, suffix)) = id.rsplit_once('-') else {
        return false;
    };
    valid_id(id)
        && !slug.is_empty()
        && ID_SUFFIX_LENS.contains(&suffix.len())
        && suffix.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn make_id(slug: &str, hash: &str, manifest: &Manifest) -> Option<String> {
    ID_SUFFIX_LENS.iter().find_map(|&n| {
        let id = format!("{slug}-{}", hash.get(..n)?);
        (valid_id(&id) && !manifest.docs.iter().any(|d| d.id == id)).then_some(id)
    })
}

/// Último componente de um nome enviado (aceita `/` e `\`), sem espaços nas pontas.
fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name).trim()
}

/// Texto de uma linha: sem caracteres de controle, espaços colapsados, até `max` caracteres.
fn one_line(text: &str, max: usize) -> String {
    let joined = text.split(|c: char| c.is_whitespace() || c.is_control()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
    joined.chars().take(max).collect()
}

/// `true` para nomes típicos de arquivo de segredo: `.env` e variantes, chaves (PEM, `.key`,
/// PKCS#12, keystores, PuTTY), chaves SSH privadas, arquivos de credenciais de ferramentas e
/// stems como `secrets` ou `credentials`. Compara só o último componente, sem diferenciar caixa.
pub fn is_secret_name(name: &str) -> bool {
    let lower = base_name(name).to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    if lower == ".env" || lower.starts_with(".env.") || lower.ends_with(".env") || lower == "env" {
        return true;
    }
    const EXACT: &[&str] = &[
        ".netrc", "_netrc", ".npmrc", ".pypirc", ".pgpass", ".htpasswd", ".git-credentials", ".dockercfg",
        "credentials", "authorized_keys", "known_hosts",
    ];
    if EXACT.contains(&lower.as_str()) {
        return true;
    }
    let (stem, ext) = match lower.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (lower.trim_start_matches('.'), None),
    };
    const KEY_EXTS: &[&str] = &["pem", "key", "p12", "pfx", "jks", "keystore", "ppk", "kdbx", "p8", "asc", "gpg"];
    if ext.is_some_and(|e| KEY_EXTS.contains(&e)) {
        return true;
    }
    const SSH_KEYS: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "id_ecdsa_sk", "id_ed25519_sk"];
    if SSH_KEYS.contains(&lower.as_str()) || (SSH_KEYS.contains(&stem) && ext != Some("pub")) {
        return true;
    }
    const STEMS: &[&str] = &[
        "secret", "secrets", "credential", "credentials", "password", "passwords", "senha", "senhas", "tokens",
        "apikey", "apikeys", "api_key", "api_keys", "api-key", "api-keys", "private_key", "private-key", "privatekey",
    ];
    STEMS.contains(&stem)
}

/// `true` se `bytes` traz um bloco de chave privada PEM (`-----BEGIN ... PRIVATE KEY-----`).
pub fn has_private_key(bytes: &[u8]) -> bool {
    const BEGIN: &[u8] = b"-----BEGIN ";
    const TAIL: &[u8] = b"PRIVATE KEY-----";
    let mut rest = bytes;
    while let Some(pos) = find(rest, BEGIN) {
        let after = &rest[pos + BEGIN.len()..];
        let line_end = after.iter().position(|b| *b == b'\n' || *b == b'\r').unwrap_or(after.len()).min(64);
        if find(&after[..line_end], TAIL).is_some() {
            return true;
        }
        rest = after;
    }
    false
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `true` se o texto (ou os bytes, lidos como UTF-8 com perda) contém um segredo configurado.
fn has_configured_secret(secrets: &SecretSet, bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    secrets.scrub_exact(&text) != text
}

/// Descrição validada (C-19): uma linha, até [`MAX_DESCRIPTION_CHARS`]; espaços nas pontas saem.
pub fn clean_description(raw: &str) -> Result<String, KbError> {
    let text = raw.trim();
    if text.chars().any(char::is_control) {
        return Err(KbError::BadDescription);
    }
    if text.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(KbError::DescriptionTooLong);
    }
    Ok(text.to_string())
}

/// Link validado e normalizado (sem o fragmento): http ou https, com host, sem usuário nem senha.
pub fn clean_url(raw: &str) -> Result<String, KbError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_BYTES || raw.chars().any(char::is_control) {
        return Err(KbError::BadUrl);
    }
    let mut url = reqwest::Url::parse(raw).map_err(|_| KbError::BadUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(KbError::BadUrl);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(KbError::UrlCredentials);
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

// --- arquivos ---

fn source_rel(id: &str, ext: &str) -> String {
    format!("{SOURCES_REL}/{id}.{ext}")
}

fn note_rel(id: &str) -> String {
    format!("{NOTES_REL}/{id}.md")
}

/// Cria `rel` com `data`, sem seguir symlink e sem sobrescrever. Em falha, apaga o que criou.
fn write_new_file(root: &Path, rel: &str, data: &[u8]) -> Result<(), KbError> {
    let path = safe_fs::check_chain(root, rel)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .open(&path)
        .map_err(|e| if e.raw_os_error() == Some(ELOOP) { SafeFsError::Unsafe } else { SafeFsError::Io })?;
    if file.write_all(data).and_then(|()| file.sync_all()).is_err() {
        drop(file);
        let _ = safe_fs::remove_regular(root, rel);
        return Err(KbError::Fs(SafeFsError::Io));
    }
    Ok(())
}

/// Grava `rel` de forma atômica: temporário exclusivo (`O_NOFOLLOW`) na mesma pasta, `fsync` e
/// `rename` por cima. Nenhum componente pode ser symlink; um alvo que não é arquivo regular é
/// recusado (nunca substituído).
fn write_atomic(root: &Path, rel: &str, data: &[u8]) -> Result<(), KbError> {
    let target = safe_fs::check_chain(root, rel)?;
    match fs::symlink_metadata(&target) {
        Ok(m) if m.file_type().is_file() => {}
        Ok(m) if m.file_type().is_symlink() => return Err(KbError::Fs(SafeFsError::Unsafe)),
        Ok(_) => return Err(KbError::Fs(SafeFsError::NotRegular)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(KbError::Fs(SafeFsError::Io)),
    }
    let (dir, file) = rel.rsplit_once('/').ok_or(KbError::Fs(SafeFsError::Unsafe))?;
    let suffix = crate::server::approvals::random_hex_id().map_err(|_| SafeFsError::Io)?;
    let tmp_rel = format!("{dir}/.{file}.{suffix}.tmp");
    write_new_file(root, &tmp_rel, data)?;
    let tmp = safe_fs::check_chain(root, &tmp_rel)?;
    // O caminho é revalidado logo antes do `rename`: um symlink trocado no meio é recusado.
    let renamed = safe_fs::check_chain(root, rel)
        .map_err(KbError::Fs)
        .and_then(|target| fs::rename(&tmp, &target).map_err(|_| KbError::Fs(SafeFsError::Io)));
    if renamed.is_err() {
        let _ = safe_fs::remove_regular(root, &tmp_rel);
    }
    renamed
}

/// Cria `.sdd/kb/`, `sources/` e `notes/` como diretórios reais.
fn ensure_layout(root: &Path) -> Result<(), KbError> {
    safe_fs::ensure_dirs(root, SOURCES_REL)?;
    safe_fs::ensure_dirs(root, NOTES_REL)?;
    Ok(())
}

/// Lê o manifesto. Ausente → base vazia; ilegível → [`KbError::Corrupt`].
pub fn load_manifest(root: &Path) -> Result<Manifest, KbError> {
    match safe_fs::read_regular(root, MANIFEST_REL, MAX_MANIFEST_BYTES) {
        Ok(text) => serde_json::from_str(&text).map_err(|_| KbError::Corrupt),
        Err(SafeFsError::Missing) => Ok(Manifest::default()),
        Err(SafeFsError::Invalid) => Err(KbError::Corrupt),
        Err(e) => Err(KbError::Fs(e)),
    }
}

fn save_manifest(root: &Path, manifest: &Manifest) -> Result<(), KbError> {
    let mut text = serde_json::to_string_pretty(manifest).map_err(|_| KbError::Fs(SafeFsError::Io))?;
    text.push('\n');
    write_atomic(root, MANIFEST_REL, text.as_bytes())
}

/// Valor de frontmatter: string entre aspas (escape JSON, que também é YAML válido) ou `null`.
fn fm_str(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_string(), |v| serde_json::Value::String(v.to_string()).to_string())
}

/// Nota completa: frontmatter da entrada + corpo.
fn note_text(entry: &Entry, body: &str) -> String {
    let pages = entry.pages.map_or_else(|| "null".to_string(), |p| p.to_string());
    let mut out = String::with_capacity(body.len() + 512);
    out.push_str("---\n");
    for (key, value) in [
        ("title", fm_str(Some(&entry.title))),
        ("kind", fm_str(Some(entry.kind.as_str()))),
        ("source", fm_str(Some(entry.source.as_str()))),
        ("source_url", fm_str(entry.source_url.as_deref())),
        ("added_at", fm_str(Some(&entry.added_at))),
        ("fetched_at", fm_str(entry.fetched_at.as_deref())),
        ("sha256", fm_str(Some(&entry.sha256))),
        ("pages", pages),
        ("extractor", fm_str(entry.extractor.as_deref())),
        ("status", fm_str(Some(entry.status.as_str()))),
    ] {
        out.push_str(key);
        out.push_str(": ");
        out.push_str(&value);
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(body);
    out
}

/// Corpo da nota, sem o frontmatter. Nota sem frontmatter devolve o texto inteiro.
pub fn note_body(note: &str) -> &str {
    let Some(rest) = note.strip_prefix("---\n") else {
        return note;
    };
    match rest.find("\n---\n") {
        Some(end) => &rest[end + 5..],
        None => rest.strip_suffix("\n---").map_or(note, |_| ""),
    }
}

/// Lê a nota bruta de `id`. Ausente → `None`.
fn read_note_file(root: &Path, id: &str) -> Result<Option<String>, KbError> {
    match safe_fs::read_regular(root, &note_rel(id), MAX_NOTE_BYTES) {
        Ok(text) => Ok(Some(text)),
        Err(SafeFsError::Missing) => Ok(None),
        Err(e) => Err(KbError::Fs(e)),
    }
}

/// Apaga o original e a nota de `id` (melhor esforço; symlinks nunca são seguidos).
fn remove_files(root: &Path, id: &str) {
    for ext in SOURCE_EXTS {
        let _ = safe_fs::remove_regular(root, &source_rel(id, ext));
    }
    let _ = safe_fs::remove_regular(root, &note_rel(id));
}

// --- operações ---

/// `true` se o alvo de link `target` (`url:<endereço>` ou `file:<nome em minúsculas>`) aponta para
/// `doc` (C-22): `url:` casa com o link do documento, `file:` com o nome do arquivo enviado (sem
/// caixa).
pub fn link_points_to(doc: &Entry, target: &str) -> bool {
    if let Some(url) = target.strip_prefix("url:") {
        return doc.source_url.as_deref() == Some(url);
    }
    match (target.strip_prefix("file:"), &doc.file_name) {
        (Some(name), Some(file)) => file.to_lowercase() == name,
        _ => false,
    }
}

/// Recalcula os `links_out` de todos os documentos a partir dos `link_targets` (C-22), pela regra
/// de [`link_points_to`].
fn relink(manifest: &mut Manifest) {
    let outs: Vec<Vec<String>> = manifest
        .docs
        .iter()
        .map(|doc| {
            let mut out: Vec<String> = Vec::new();
            for target in &doc.link_targets {
                for other in &manifest.docs {
                    if other.id != doc.id && link_points_to(other, target) && !out.contains(&other.id) {
                        out.push(other.id.clone());
                    }
                }
            }
            out
        })
        .collect();
    for (doc, out) in manifest.docs.iter_mut().zip(outs) {
        doc.links_out = out;
    }
}

/// Descrição extraída (C-19), redigida e numa linha, até [`MAX_DESCRIPTION_CHARS`].
fn extracted_description(secrets: &SecretSet, text: Option<&str>) -> String {
    text.map(|d| one_line(&scrub_text(secrets, d), MAX_DESCRIPTION_CHARS)).unwrap_or_default()
}

/// Adiciona um arquivo enviado. Markdown e texto nascem `ready`, com o texto normalizado e
/// redigido na nota, a descrição extraída e os links; PDF nasce `pending`, para a fila.
pub fn add_file(root: &Path, name: &str, data: &[u8], secrets: &SecretSet, now: &str) -> Result<Entry, KbError> {
    let name = base_name(name);
    if name.is_empty() || name.chars().count() > MAX_FILE_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(KbError::BadName);
    }
    if is_secret_name(name) {
        return Err(KbError::SecretName);
    }
    let kind = Kind::from_file_name(name).ok_or(KbError::Unsupported)?;
    if data.is_empty() {
        return Err(KbError::Empty);
    }
    let extracted = match kind {
        Kind::Pdf => {
            if !data.starts_with(b"%PDF-") {
                return Err(KbError::Unsupported);
            }
            None
        }
        _ => {
            let text = std::str::from_utf8(data).map_err(|_| KbError::NotText)?;
            if text.contains('\0') {
                return Err(KbError::NotText);
            }
            Some(extract::text(text, kind == Kind::Markdown))
        }
    };
    if has_private_key(data) || has_configured_secret(secrets, data) {
        return Err(KbError::SecretContent);
    }
    let sha256 = sha256_hex(data);
    // Título e nome passam pela redação antes de ir ao manifesto, à nota e ao catálogo.
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let stem = scrub_text(secrets, stem);
    let title = extracted
        .as_ref()
        .and_then(|e| e.title.as_deref())
        .map(|t| one_line(&scrub_text(secrets, t), MAX_TITLE_CHARS))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| one_line(&stem, MAX_TITLE_CHARS));
    let file_name = one_line(&scrub_text(secrets, name), MAX_FILE_NAME_CHARS);

    let _guard = lock();
    ensure_layout(root)?;
    let mut manifest = load_manifest(root)?;
    if let Some(dup) = manifest.docs.iter().find(|d| d.sha256 == sha256) {
        return Err(KbError::Duplicate(dup.id.clone()));
    }
    let id = make_id(&slugify(&stem), &sha256, &manifest).ok_or(KbError::Fs(SafeFsError::Io))?;
    let entry = Entry {
        title: if title.is_empty() { id.clone() } else { title },
        id: id.clone(),
        kind,
        source: Origin::File,
        file_name: Some(file_name),
        source_url: None,
        size: data.len() as u64,
        sha256,
        added_at: now.to_string(),
        fetched_at: None,
        status: if extracted.is_some() { Status::Ready } else { Status::Pending },
        status_reason: None,
        pages: None,
        extractor: extracted.as_ref().map(|e| e.extractor.to_string()),
        description: extracted_description(secrets, extracted.as_ref().and_then(|e| e.description.as_deref())),
        links_out: Vec::new(),
        link_targets: extracted.as_ref().map(|e| e.link_targets.clone()).unwrap_or_default(),
    };
    // Sobras de um documento removido com o mesmo id saem antes.
    remove_files(root, &id);
    let ext = kind.source_ext().unwrap_or("bin");
    let mut saved = entry.clone();
    let stored = write_new_file(root, &source_rel(&id, ext), data)
        .and_then(|()| {
            let scrubbed = extracted.as_ref().map(|e| scrub_text(secrets, &e.body)).unwrap_or_default();
            write_atomic(root, &note_rel(&id), note_text(&entry, &scrubbed).as_bytes())
        })
        .and_then(|()| {
            manifest.docs.push(entry.clone());
            relink(&mut manifest);
            if let Some(found) = manifest.docs.iter().find(|d| d.id == id) {
                saved = found.clone();
            }
            save_manifest(root, &manifest)
        });
    if let Err(e) = stored {
        remove_files(root, &id);
        return Err(e);
    }
    Ok(saved)
}

/// Registra um link com `status: pending`; a fila de extração o lê uma vez (decisão 4).
pub fn add_link(root: &Path, raw_url: &str, secrets: &SecretSet, now: &str) -> Result<Entry, KbError> {
    let url = clean_url(raw_url)?;
    if has_configured_secret(secrets, url.as_bytes()) {
        return Err(KbError::SecretContent);
    }
    let parsed = reqwest::Url::parse(&url).map_err(|_| KbError::BadUrl)?;
    let slug = slugify(&format!("{}{}", parsed.host_str().unwrap_or_default(), parsed.path()));
    let hash = sha256_hex(url.as_bytes());

    let _guard = lock();
    ensure_layout(root)?;
    let mut manifest = load_manifest(root)?;
    if let Some(dup) = manifest.docs.iter().find(|d| d.source_url.as_deref() == Some(url.as_str())) {
        return Err(KbError::Duplicate(dup.id.clone()));
    }
    let id = make_id(&slug, &hash, &manifest).ok_or(KbError::Fs(SafeFsError::Io))?;
    let entry = Entry {
        id: id.clone(),
        title: one_line(&scrub_text(secrets, &url), MAX_TITLE_CHARS),
        kind: Kind::Link,
        source: Origin::Url,
        file_name: None,
        source_url: Some(url),
        size: 0,
        sha256: String::new(),
        added_at: now.to_string(),
        fetched_at: None,
        status: Status::Pending,
        status_reason: None,
        pages: None,
        extractor: None,
        description: String::new(),
        links_out: Vec::new(),
        link_targets: Vec::new(),
    };
    remove_files(root, &id);
    let stored = write_atomic(root, &note_rel(&id), note_text(&entry, "").as_bytes()).and_then(|()| {
        manifest.docs.push(entry.clone());
        relink(&mut manifest);
        save_manifest(root, &manifest)
    });
    if let Err(e) = stored {
        remove_files(root, &id);
        return Err(e);
    }
    Ok(entry)
}

// --- documentos escritos no Nants (spec 136 T10) ---

/// Chave de comparação de títulos: sem acento, sem caixa e com os espaços colapsados
/// ("Padrões  da API" e "padroes da api" são o mesmo título).
fn title_key(title: &str) -> String {
    one_line(title, usize::MAX).chars().map(fold_accent).flat_map(char::to_lowercase).collect()
}

/// Título do documento escrito: uma linha, sem caracteres de controle, até [`MAX_TITLE_CHARS`];
/// vazio é recusado, e o título com segredo também (ele vai ao manifesto, à nota e ao catálogo).
fn clean_title(raw: &str, secrets: &SecretSet) -> Result<String, KbError> {
    let title = one_line(raw, usize::MAX);
    if title.is_empty() {
        return Err(KbError::TitleEmpty);
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(KbError::TitleTooLong);
    }
    if has_private_key(title.as_bytes()) || has_configured_secret(secrets, title.as_bytes()) {
        return Err(KbError::SecretContent);
    }
    Ok(title)
}

/// Texto escrito ou editado: até [`MAX_WRITTEN_BYTES`], sem byte nulo, sem chave privada nem
/// segredo configurado (a mesma recusa do envio de arquivo).
fn check_written(content: &str, secrets: &SecretSet) -> Result<(), KbError> {
    if content.len() > MAX_WRITTEN_BYTES {
        return Err(KbError::WrittenTooLarge);
    }
    if content.contains('\0') {
        return Err(KbError::NotText);
    }
    if has_private_key(content.as_bytes()) || has_configured_secret(secrets, content.as_bytes()) {
        return Err(KbError::SecretContent);
    }
    Ok(())
}

/// Cria um documento escrito no Nants: markdown, `source: "written"`, pronto na hora.
///
/// - **Id:** estável a partir do título ([`make_id`] com o slug e o hash da chave do título): o
///   mesmo título dá o mesmo id, também depois de remover e escrever de novo.
/// - **Título:** o da pessoa (o `title` de um frontmatter no texto não o troca); repetido (a mesma
///   chave de [`title_key`] de outro documento da base) é recusado com o id do outro.
/// - **Nome:** `<slug do título>.md`, para que um link `[texto](<slug>.md)` de outro documento
///   chegue a ele pela regra de sempre ([`link_points_to`]).
/// - **Duplicata:** o mesmo conteúdo de outro documento não é recusado; vira o indicador
///   `duplicate` ([`indicators`]).
pub fn add_written(root: &Path, raw_title: &str, content: &str, secrets: &SecretSet, now: &str) -> Result<Entry, KbError> {
    let title = clean_title(raw_title, secrets)?;
    check_written(content, secrets)?;
    let extracted = extract::text(content, true);
    let sha256 = sha256_hex(content.as_bytes());
    let key = title_key(&title);
    let slug = slugify(&title);

    let _guard = lock();
    ensure_layout(root)?;
    let mut manifest = load_manifest(root)?;
    if let Some(taken) = manifest.docs.iter().find(|d| title_key(&d.title) == key) {
        return Err(KbError::TitleTaken(taken.id.clone()));
    }
    let id = make_id(&slug, &sha256_hex(key.as_bytes()), &manifest).ok_or(KbError::Fs(SafeFsError::Io))?;
    let entry = Entry {
        id: id.clone(),
        title,
        kind: Kind::Markdown,
        source: Origin::Written,
        file_name: Some(format!("{slug}.md")),
        source_url: None,
        size: content.len() as u64,
        sha256,
        added_at: now.to_string(),
        fetched_at: None,
        status: Status::Ready,
        status_reason: None,
        pages: None,
        extractor: Some(extracted.extractor.to_string()),
        description: extracted_description(secrets, extracted.description.as_deref()),
        links_out: Vec::new(),
        link_targets: extracted.link_targets.clone(),
    };
    remove_files(root, &id);
    let mut saved = entry.clone();
    let stored = write_new_file(root, &source_rel(&id, "md"), content.as_bytes())
        .and_then(|()| write_atomic(root, &note_rel(&id), note_text(&entry, &scrub_text(secrets, &extracted.body)).as_bytes()))
        .and_then(|()| {
            manifest.docs.push(entry.clone());
            relink(&mut manifest);
            if let Some(found) = manifest.docs.iter().find(|d| d.id == id) {
                saved = found.clone();
            }
            save_manifest(root, &manifest)
        });
    if let Err(e) = stored {
        remove_files(root, &id);
        return Err(e);
    }
    Ok(saved)
}

/// `true` se o texto do documento é editável no Nants: o escrito e o enviado em markdown ou
/// texto (o original é o próprio texto). PDF e link são extração de um original.
pub fn is_editable(entry: &Entry) -> bool {
    matches!(entry.kind, Kind::Markdown | Kind::Text) && matches!(entry.source, Origin::File | Origin::Written)
}

/// Troca o texto de `id` (spec 136 T10): regrava o original e a nota e recalcula o `sha256`, o
/// tamanho, os alvos de link e os `links_out` de toda a base. O índice e os indicadores seguem o
/// manifesto, que muda junto (a impressão do `sha256` do manifesto invalida o índice).
///
/// - **Versão:** `base_sha256` é o `sha256` que a pessoa abriu; outro → [`KbError::EditConflict`]
///   e nada é gravado.
/// - **Título:** fica o do manifesto e do frontmatter da nota (não é derivado do texto de novo).
/// - **Descrição:** a escrita pela pessoa fica; vazia, ou igual ao rascunho extraído do texto
///   antigo, passa a ser a extraída do texto novo.
/// - **Ordem:** original, nota e por fim o manifesto; o índice nunca vê o manifesto novo com a
///   nota antiga. Falhou no meio, o original e a nota antigos voltam (melhor esforço).
pub fn edit_content(root: &Path, id: &str, content: &str, base_sha256: &str, secrets: &SecretSet) -> Result<Entry, KbError> {
    if !valid_id(id) {
        return Err(KbError::NotFound);
    }
    check_written(content, secrets)?;

    let _guard = lock();
    let mut manifest = load_manifest(root)?;
    let entry = manifest.docs.iter_mut().find(|d| d.id == id).ok_or(KbError::NotFound)?;
    if !is_editable(entry) {
        return Err(KbError::NotEditable);
    }
    if entry.sha256 != base_sha256 {
        return Err(KbError::EditConflict);
    }
    let ext = entry.kind.source_ext().ok_or(KbError::NotEditable)?;
    let markdown = entry.kind == Kind::Markdown;
    let old_note = read_note_file(root, id)?;
    // A descrição que é o rascunho extraído do texto antigo acompanha o texto; a escrita pela
    // pessoa (diferente do rascunho) fica.
    let old_draft = old_note
        .as_deref()
        .map(|note| extracted_description(secrets, extract::text(note_body(note), markdown).description.as_deref()));
    let extracted = extract::text(content, markdown);
    entry.sha256 = sha256_hex(content.as_bytes());
    entry.size = content.len() as u64;
    entry.status = Status::Ready;
    entry.status_reason = None;
    entry.extractor = Some(extracted.extractor.to_string());
    entry.link_targets = extracted.link_targets.clone();
    if entry.description.is_empty() || old_draft.as_deref() == Some(entry.description.as_str()) {
        entry.description = extracted_description(secrets, extracted.description.as_deref());
    }
    relink(&mut manifest);
    let updated = manifest.docs.iter().find(|d| d.id == id).cloned().ok_or(KbError::NotFound)?;

    let source = source_rel(id, ext);
    let old_source = safe_fs::read_bounded(root, &source, MAX_UPLOAD_BYTES as u64).ok().and_then(|r| r.data);
    let stored = write_atomic(root, &source, content.as_bytes())
        .and_then(|()| write_atomic(root, &note_rel(id), note_text(&updated, &scrub_text(secrets, &extracted.body)).as_bytes()))
        .and_then(|()| save_manifest(root, &manifest));
    if let Err(e) = stored {
        if let Some(bytes) = old_source {
            let _ = write_atomic(root, &source, &bytes);
        }
        if let Some(note) = old_note {
            let _ = write_atomic(root, &note_rel(id), note.as_bytes());
        }
        return Err(e);
    }
    Ok(updated)
}

// --- extração (T02) ---

/// Ids dos documentos ainda na fila (`pending`, ou `extracting` interrompido por um reinício).
pub fn queued_ids(root: &Path) -> Result<Vec<String>, KbError> {
    Ok(load_manifest(root)?.docs.into_iter().filter(|d| d.status.is_queued()).map(|d| d.id).collect())
}

/// Passa `id` para `extracting` e devolve a entrada. `None` se o documento saiu da base ou já
/// não está na fila.
pub fn begin_extraction(root: &Path, id: &str) -> Result<Option<Entry>, KbError> {
    if !valid_id(id) {
        return Ok(None);
    }
    let _guard = lock();
    let mut manifest = load_manifest(root)?;
    let Some(entry) = manifest.docs.iter_mut().find(|d| d.id == id && d.status.is_queued()) else {
        return Ok(None);
    };
    entry.status = Status::Extracting;
    entry.status_reason = None;
    let updated = entry.clone();
    save_manifest(root, &manifest)?;
    let note = read_note_file(root, id)?.unwrap_or_default();
    write_atomic(root, &note_rel(id), note_text(&updated, note_body(&note)).as_bytes())?;
    Ok(Some(updated))
}

/// Bytes do original de um PDF enviado, até [`MAX_UPLOAD_BYTES`].
pub fn source_bytes(root: &Path, entry: &Entry) -> Result<Vec<u8>, KbError> {
    let ext = entry.kind.source_ext().ok_or(KbError::Unsupported)?;
    let read = safe_fs::read_bounded(root, &source_rel(&entry.id, ext), MAX_UPLOAD_BYTES as u64)?;
    read.data.ok_or(KbError::Fs(SafeFsError::TooLarge))
}

/// Resultado de uma extração, para [`finish_extraction`]: o texto e, nos links, o original lido;
/// ou o motivo codificado do erro.
pub type Outcome = Result<(extract::Extracted, Option<extract::LinkSource>), &'static str>;

/// Grava o resultado da extração de `id`. Só vale se o documento ainda é o mesmo (`added_at`)
/// e está em `extracting`: um documento removido no meio fica removido. Texto extraído (ou original
/// do link) com chave privada ou com um segredo configurado vira `error` (`kb_secret_content`), o
/// original sai e nada dele é gravado.
pub fn finish_extraction(
    root: &Path,
    id: &str,
    added_at: &str,
    outcome: Outcome,
    secrets: &SecretSet,
    now: &str,
) -> Result<Option<Entry>, KbError> {
    if !valid_id(id) {
        return Ok(None);
    }
    let _guard = lock();
    let mut manifest = load_manifest(root)?;
    let Some(entry) = manifest
        .docs
        .iter_mut()
        .find(|d| d.id == id && d.added_at == added_at && d.status == Status::Extracting)
    else {
        return Ok(None);
    };
    // O texto extraído e o original do link (gravado em `sources/`) passam pela mesma conferência
    // do envio de arquivo: um segredo no HTML fora do texto principal também barra o documento.
    let outcome = outcome.and_then(|(ex, source)| {
        let leaks = |bytes: &[u8]| has_private_key(bytes) || has_configured_secret(secrets, bytes);
        let leaked = leaks(ex.body.as_bytes()) || source.as_ref().is_some_and(|s| leaks(&s.bytes));
        if leaked {
            Err(extract::reason::SECRET)
        } else {
            Ok((ex, source))
        }
    });
    let body = match outcome {
        Err(reason) => {
            entry.status = Status::Error;
            entry.status_reason = Some(reason.to_string());
            if reason == extract::reason::SECRET {
                for ext in SOURCE_EXTS {
                    let _ = safe_fs::remove_regular(root, &source_rel(id, ext));
                }
            }
            String::new()
        }
        Ok((ex, source)) => {
            if let Some(source) = source {
                write_atomic(root, &source_rel(id, source.ext), &source.bytes)?;
                entry.size = source.bytes.len() as u64;
                entry.sha256 = sha256_hex(&source.bytes);
                entry.fetched_at = Some(now.to_string());
            }
            if entry.kind == Kind::Link {
                if let Some(title) = ex.title.as_deref() {
                    let title = one_line(&scrub_text(secrets, title), MAX_TITLE_CHARS);
                    if !title.is_empty() {
                        entry.title = title;
                    }
                }
            }
            if entry.description.is_empty() {
                entry.description = extracted_description(secrets, ex.description.as_deref());
            }
            entry.status = ex.status;
            entry.status_reason = ex.reason.map(str::to_string);
            entry.pages = ex.pages;
            entry.extractor = Some(ex.extractor.to_string());
            // Alvo com um segredo configurado (token na query de um link) não vai ao manifesto.
            entry.link_targets = ex.link_targets.into_iter().filter(|t| scrub_text(secrets, t) == *t).collect();
            scrub_text(secrets, &ex.body)
        }
    };
    let updated = entry.clone();
    relink(&mut manifest);
    let updated = manifest.docs.iter().find(|d| d.id == id).cloned().unwrap_or(updated);
    write_atomic(root, &note_rel(id), note_text(&updated, &body).as_bytes())?;
    save_manifest(root, &manifest)?;
    Ok(Some(updated))
}

/// Documentos da base, na ordem em que entraram.
pub fn list(root: &Path) -> Result<Vec<Entry>, KbError> {
    Ok(load_manifest(root)?.docs)
}

/// Entrada de `id` no manifesto, sem ler a nota (spec 136 T03, a leitura de uma seção).
pub fn read_entry(root: &Path, id: &str) -> Result<Entry, KbError> {
    if !valid_id(id) {
        return Err(KbError::NotFound);
    }
    load_manifest(root)?.docs.into_iter().find(|d| d.id == id).ok_or(KbError::NotFound)
}

/// Entrada e corpo da nota de `id`, redigido pelo `SecretSet`. Nota ainda ausente → corpo vazio.
pub fn read_note(root: &Path, id: &str, secrets: &SecretSet) -> Result<(Entry, String), KbError> {
    if !valid_id(id) {
        return Err(KbError::NotFound);
    }
    let entry = load_manifest(root)?.docs.into_iter().find(|d| d.id == id).ok_or(KbError::NotFound)?;
    let note = read_note_file(root, id)?.unwrap_or_default();
    Ok((entry, crate::server::projects::scrub_text(secrets, note_body(&note))))
}

/// Texto-fonte de um documento editável (spec 136 T11), para o editor: a entrada e o original,
/// sem redação. O `sha256` da entrada é a versão que o `PUT` espera em `base_sha256`.
///
/// - PDF e link → [`KbError::NotEditable`];
/// - original acima de [`MAX_WRITTEN_BYTES`] → [`KbError::WrittenTooLarge`] (a gravação recusaria);
/// - original que a redação mudaria → [`KbError::SourceRedacted`]: o envio e a escrita já recusam
///   segredo configurado, então isso só acontece com um segredo configurado depois do envio ou com
///   uma credencial num endereço; devolver o texto redigido levaria o marcador ao original.
pub fn read_source(root: &Path, id: &str, secrets: &SecretSet) -> Result<(Entry, String), KbError> {
    if !valid_id(id) {
        return Err(KbError::NotFound);
    }
    let entry = load_manifest(root)?.docs.into_iter().find(|d| d.id == id).ok_or(KbError::NotFound)?;
    if !is_editable(&entry) {
        return Err(KbError::NotEditable);
    }
    let ext = entry.kind.source_ext().ok_or(KbError::NotEditable)?;
    let text = match safe_fs::read_regular(root, &source_rel(id, ext), MAX_WRITTEN_BYTES as u64) {
        Ok(text) => text,
        Err(SafeFsError::TooLarge) => return Err(KbError::WrittenTooLarge),
        Err(e) => return Err(KbError::Fs(e)),
    };
    if crate::server::projects::scrub_text(secrets, &text) != text {
        return Err(KbError::SourceRedacted);
    }
    Ok((entry, text))
}

/// Alteração de um documento pela pessoa: a descrição.
///
/// Spec 136 T02 (C-06): o `PATCH` da 134 mandava também `role` e `applies_to`. Os dois continuam
/// aceitos, com qualquer valor, e são ignorados: um cliente antigo não recebe erro, e nenhum campo
/// desconhecido além deles passa.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub description: Option<String>,
    /// `role` da 134, ignorado.
    #[serde(default, rename = "role")]
    pub legacy_role: Option<serde::de::IgnoredAny>,
    /// `applies_to` da 134, ignorado.
    #[serde(default, rename = "applies_to")]
    pub legacy_applies_to: Option<serde::de::IgnoredAny>,
}

/// Altera a `description`. Os campos antigos do [`Patch`] não mudam nada.
pub fn update(root: &Path, id: &str, patch: &Patch) -> Result<Entry, KbError> {
    if !valid_id(id) {
        return Err(KbError::NotFound);
    }
    let description = patch.description.as_deref().map(clean_description).transpose()?;

    let _guard = lock();
    let mut manifest = load_manifest(root)?;
    let entry = manifest.docs.iter_mut().find(|d| d.id == id).ok_or(KbError::NotFound)?;
    if let Some(description) = description {
        entry.description = description;
    }
    let updated = entry.clone();
    save_manifest(root, &manifest)?;
    Ok(updated)
}

/// Remove `id`: primeiro do manifesto, depois o original e a nota.
pub fn remove(root: &Path, id: &str) -> Result<(), KbError> {
    if !valid_id(id) {
        return Err(KbError::NotFound);
    }
    let _guard = lock();
    let mut manifest = load_manifest(root)?;
    let before = manifest.docs.len();
    manifest.docs.retain(|d| d.id != id);
    if manifest.docs.len() == before {
        return Err(KbError::NotFound);
    }
    relink(&mut manifest);
    save_manifest(root, &manifest)?;
    remove_files(root, id);
    Ok(())
}

/// Visão geral da base, redigida. Ausente → vazio.
pub fn read_overview(root: &Path, secrets: &SecretSet) -> Result<String, KbError> {
    match safe_fs::read_regular(root, OVERVIEW_REL, MAX_OVERVIEW_BYTES as u64) {
        Ok(text) => Ok(crate::server::projects::scrub_text(secrets, &text)),
        Err(SafeFsError::Missing) => Ok(String::new()),
        Err(e) => Err(KbError::Fs(e)),
    }
}

/// Grava a visão geral (texto vazio apaga o arquivo). Acima de [`MAX_OVERVIEW_BYTES`] é recusada.
pub fn write_overview(root: &Path, text: &str) -> Result<(), KbError> {
    if text.len() > MAX_OVERVIEW_BYTES {
        return Err(KbError::OverviewTooLarge);
    }
    let _guard = lock();
    if text.trim().is_empty() {
        return safe_fs::remove_regular(root, OVERVIEW_REL).map_err(KbError::Fs);
    }
    safe_fs::ensure_dirs(root, KB_REL)?;
    write_atomic(root, OVERVIEW_REL, text.as_bytes())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod eval_tests;
#[cfg(test)]
mod written_tests;
