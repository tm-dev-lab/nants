//! Arquivos do login pelo plano ChatGPT na pasta de dados do Nants (spec 116, T01).
//!
//! - [`HOST_ID_FILE`]: o `ext_agent_host_id` da instalação (`urn:uuid:<v4>`), criado antes do
//!   primeiro login e reutilizado depois.
//! - `chatgpt-conn-<id>.json` ([`credentials_path`]): as credenciais de uma conexão.
//!
//! Os dois são gravados com permissão 0600, de forma atômica (temporário no mesmo diretório e
//! `rename`), sem seguir link simbólico, e nunca vão para o log: [`Credentials`] tem `Debug` que
//! redige os segredos.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Arquivo do identificador da instalação, na pasta de dados.
pub const HOST_ID_FILE: &str = "chatgpt-host-id.json";
/// Prefixo dos arquivos de credenciais (`chatgpt-conn-<id da conexão>.json`): nunca colide com
/// [`HOST_ID_FILE`], qualquer que seja o id.
const CREDENTIALS_PREFIX: &str = "chatgpt-conn-";
/// Teto de leitura dos arquivos (as credenciais têm poucos KB).
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Credenciais de uma conexão pelo plano ChatGPT. Sem `Debug` derivado: o manual redige.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Credentials {
    /// `client_id` emitido no primeiro login (`oaiapp_...`), reutilizado depois.
    pub client_id: String,
    pub access_token: String,
    /// Rotativo: cada renovação grava o novo.
    pub refresh_token: String,
    pub id_token: String,
    /// Validade do `access_token`, em segundos desde a época Unix.
    pub expires_at: u64,
    pub scopes: Vec<String>,
    pub email: String,
    pub sub: String,
    /// Segundos desde a época Unix.
    pub saved_at: u64,
    /// A renovação falhou de vez (`invalid_grant`, `refresh_token_*`): precisa entrar de novo.
    /// Os tokens de acesso e de renovação já foram apagados; ficam o `client_id`, o `id_token` e o
    /// e-mail para a reautorização.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_login: bool,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client_id", &self.client_id)
            .field("access_token", &"***")
            .field("refresh_token", &"***")
            .field("id_token", &"***")
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("email", &"***")
            .field("sub", &"***")
            .field("saved_at", &self.saved_at)
            .field("needs_login", &self.needs_login)
            .finish()
    }
}

/// Arquivo do identificador da instalação.
#[derive(Serialize, Deserialize)]
struct HostIdFile {
    ext_agent_host_id: String,
}

/// Id de conexão seguro para nome de arquivo: `[a-zA-Z0-9_-]{1,64}` (a mesma regra da API).
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Caminho das credenciais da conexão; `None` para um id fora da regra.
pub fn credentials_path(dir: &Path, connection_id: &str) -> Option<PathBuf> {
    safe_id(connection_id).then(|| dir.join(format!("{CREDENTIALS_PREFIX}{connection_id}.json")))
}

/// Grava `body` em `path` com 0600, de forma atômica e sem seguir link simbólico. A pasta é criada
/// com 0700 se faltar.
pub fn write_private(path: &Path, body: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let name = path.file_name().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    let mut suffix = [0u8; 6];
    getrandom::getrandom(&mut suffix).map_err(|_| io::Error::from(io::ErrorKind::Other))?;
    let tmp = parent.join(format!(".{}.{}.tmp", name.to_string_lossy(), super::hex(&suffix)));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(body)?;
            f.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Lê um arquivo regular (sem seguir link simbólico), até [`MAX_FILE_BYTES`]. Ausente: `None`.
fn read_private(path: &Path) -> io::Result<Option<String>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_file() {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut raw = String::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .and_then(|f| f.take(MAX_FILE_BYTES).read_to_string(&mut raw))?;
    Ok(Some(raw))
}

/// Credenciais da conexão. Ausentes: `Ok(None)`. Ilegíveis ou inválidas: erro (nunca o conteúdo).
pub fn load_credentials(dir: &Path, connection_id: &str) -> io::Result<Option<Credentials>> {
    let path = credentials_path(dir, connection_id).ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    match read_private(&path)? {
        None => Ok(None),
        Some(raw) => serde_json::from_str(&raw).map(Some).map_err(|_| io::Error::from(io::ErrorKind::InvalidData)),
    }
}

/// Grava as credenciais da conexão (0600, atômica).
pub fn save_credentials(dir: &Path, connection_id: &str, creds: &Credentials) -> io::Result<()> {
    let path = credentials_path(dir, connection_id).ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let body = serde_json::to_vec_pretty(creds).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    write_private(&path, &body)
}

/// Apaga as credenciais da conexão. Já ausentes: ok.
pub fn delete_credentials(dir: &Path, connection_id: &str) -> io::Result<()> {
    let path = credentials_path(dir, connection_id).ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    match fs::remove_file(&path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// `urn:uuid:` seguido de um UUID na forma canônica (minúsculas).
pub fn valid_host_id(value: &str) -> bool {
    let Some(uuid) = value.strip_prefix("urn:uuid:") else {
        return false;
    };
    uuid.len() == 36
        && uuid.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

/// Novo `urn:uuid:<v4>` do gerador seguro do sistema.
fn new_host_id() -> io::Result<String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|_| io::Error::from(io::ErrorKind::Other))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = super::hex(&b);
    Ok(format!("urn:uuid:{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]))
}

/// Identificador da instalação: o gravado, ou um novo, gravado antes de ser devolvido. Um arquivo
/// inválido é trocado por um novo.
pub fn load_or_create_host_id(dir: &Path) -> io::Result<String> {
    let path = dir.join(HOST_ID_FILE);
    if let Some(raw) = read_private(&path)? {
        if let Ok(file) = serde_json::from_str::<HostIdFile>(&raw) {
            if valid_host_id(&file.ext_agent_host_id) {
                return Ok(file.ext_agent_host_id);
            }
        }
    }
    let id = new_host_id()?;
    let body = serde_json::to_vec_pretty(&HostIdFile { ext_agent_host_id: id.clone() })
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    write_private(&path, &body)?;
    Ok(id)
}
