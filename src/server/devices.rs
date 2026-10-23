//! Dispositivos autorizados na rede local (spec 112, T01).
//!
//! - **Por que token e não IP:** no WSL modo NAT todas as conexões externas chegam com o mesmo IP
//!   de origem (o do Windows). A autorização é por dispositivo: um token aleatório de 32 bytes, do
//!   gerador seguro do sistema, no cookie [`COOKIE_NAME`] (`HttpOnly`, `SameSite=Strict`, `Path=/`,
//!   um ano, renovado no uso no máximo uma vez por dia; sem `Secure`, porque é HTTP local).
//! - **Armazenamento:** `devices.json` na pasta de dados do Nants, 0600, escrita atômica. Cada
//!   dispositivo guarda id, nome, o SHA-256 do token (nunca o token), `created_at` e `last_seen`
//!   (gravado no máximo uma vez por minuto). A conferência compara o hash em tempo constante.
//! - **Camada [`require_device`]**, depois da guarda de `Host`/`Origin`: em modo rede (inclusive
//!   qualquer escuta fora do loopback, como `--host` com `--allow-remote`), toda
//!   requisição que não venha de loopback de verdade (127.0.0.0/8 ou ::1 vistos pelo Nants) precisa
//!   de um dispositivo válido. Sem ele, a API responde 401 JSON e as páginas redirecionam para a
//!   entrada ([`ENTRY_PATH`]). Fora do modo rede, nada muda.
//! - **Rotas abertas sem dispositivo** ([`is_open`]), e o motivo:
//!   - `GET /connect`: a página de entrada (a SPA desenha o pedido do código);
//!   - `GET /assets/*` e `GET /brand/*` e os ícones da raiz: o JS, o CSS, as fontes e os ícones do
//!     bundle que a página de entrada carrega (código aberto, sem dado do usuário);
//!   - `GET /pair?t=...`: o link de entrada de uso único impresso no terminal;
//!   - `POST /api/devices/pair`: a troca do código de 6 dígitos;
//!   - `GET /auth/callback`, só com um `state` pendente e válido do login pelo plano ChatGPT
//!     (spec 116): o retorno chega pelo navegador deste computador, e o handler consome o `state`
//!     e confere o dispositivo que iniciou a tentativa.
//! - **Link de entrada:** gerado ao subir em modo rede ([`DeviceStore::issue_entry_link`]), de uso
//!   único e válido por 10 minutos; cria o dispositivo "Este computador".
//! - **Código de 6 dígitos:** só com a janela de adicionar aberta (até 10 minutos), troca a cada
//!   60 s, uso único; 5 erros invalidam a janela (429 até abrir outra) e há o limite global de uma
//!   tentativa por segundo. O código e os tokens nunca vão para o log: só o id e o nome.
//! - **Revogar** derruba na hora os streams (SSE) abertos pelo dispositivo.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use super::error::{params, ApiError};
use super::events::{ApiEvent, EventType};
use super::state::AppState;
use crate::i18n::{text, tr, Lang};

/// Nome do cookie do dispositivo.
pub const COOKIE_NAME: &str = "nants_device";
/// Validade do cookie (um ano).
pub const COOKIE_MAX_AGE_SECS: u64 = 365 * 24 * 60 * 60;
/// O cookie é renovado no uso no máximo uma vez por este intervalo (um dia).
pub const COOKIE_RENEW_EVERY_MS: u64 = 24 * 60 * 60 * 1000;
/// `last_seen` é gravado no máximo uma vez por este intervalo (um minuto).
pub const LAST_SEEN_EVERY_SECS: u64 = 60;
/// Validade do link de entrada (10 minutos).
pub const ENTRY_LINK_TTL_MS: u64 = 10 * 60 * 1000;
/// Duração máxima da janela de adicionar (10 minutos).
pub const CODE_WINDOW_MS: u64 = 10 * 60 * 1000;
/// O código troca a cada 60 s.
pub const CODE_ROTATE_MS: u64 = 60 * 1000;
/// Erros por janela antes de invalidá-la.
pub const MAX_CODE_ERRORS: u32 = 5;
/// Intervalo mínimo entre tentativas de troca do código, para todos (o IP de origem não serve).
pub const PAIR_MIN_INTERVAL_MS: u64 = 1000;
/// Tamanho máximo do nome do dispositivo (em caracteres).
pub const MAX_NAME_CHARS: usize = 40;
/// Arquivo dos dispositivos, na pasta de dados do Nants.
pub const DEVICES_FILE: &str = "devices.json";
/// Página de entrada (servida pela SPA).
pub const ENTRY_PATH: &str = "/connect";
/// Link de entrada de uso único (`/pair?t=...`).
pub const PAIR_LINK_PATH: &str = "/pair";
/// Troca do código de 6 dígitos.
pub const PAIR_API_PATH: &str = "/api/devices/pair";
/// Prefixos dos recursos estáticos que a página de entrada carrega.
const OPEN_STATIC_PREFIXES: [&str; 2] = ["/assets/", "/brand/"];
/// Ícones e manifesto da raiz do bundle.
const OPEN_STATIC_FILES: [&str; 4] = ["/favicon.ico", "/favicon.svg", "/manifest.webmanifest", "/apple-touch-icon.png"];
/// Teto de leitura do arquivo de dispositivos.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Versão do formato do arquivo.
const FILE_VERSION: u32 = 1;

/// Códigos estáveis (spec 102) das respostas desta área.
pub const CODE_DEVICE_REQUIRED: &str = "device_required";
pub const CODE_PAIR_RATE_LIMITED: &str = "device_pair_rate_limited";
pub const CODE_CODE_LOCKED: &str = "device_code_locked";
pub const CODE_CODE_INACTIVE: &str = "device_code_inactive";
pub const CODE_CODE_INVALID: &str = "device_code_invalid";
pub const CODE_NAME_INVALID: &str = "device_name_invalid";
pub const CODE_NOT_FOUND: &str = "device_not_found";
pub const CODE_STORE_FAILED: &str = "device_store_failed";

/// Relógio dos dispositivos em milissegundos desde a época Unix (injetável nos testes).
pub type DeviceClock = Arc<dyn Fn() -> u64 + Send + Sync>;

fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Dispositivo gravado. Sem `Debug` derivado: o hash não aparece em registros.
#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// SHA-256 do token, em hexadecimal.
    pub token_hash: String,
    /// Segundos desde a época Unix.
    pub created_at: u64,
    pub last_seen: u64,
    /// Criado pelo link de entrada do terminal ("Este computador").
    #[serde(default)]
    pub this_computer: bool,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Device({}, {:?})", self.id, self.name)
    }
}

impl Device {
    /// Forma da API (sem o hash).
    pub fn view(&self, current: Option<&str>) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "created_at": self.created_at,
            "last_seen": self.last_seen,
            "this_computer": self.this_computer,
            "current": current == Some(self.id.as_str()),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct DeviceFile {
    version: u32,
    devices: Vec<Device>,
}

/// Link de entrada pendente (só o hash do valor).
struct EntryLink {
    hash: String,
    expires_at: u64,
    lang: Lang,
}

/// Janela de adicionar dispositivo.
struct CodeWindow {
    expires_at: u64,
    code: String,
    code_since: u64,
    errors: u32,
}

/// Estado da janela de adicionar para a interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeState {
    Closed,
    /// Invalidada por erros: as tentativas recebem 429 até abrir outra.
    Locked,
    Open { code: String, seconds_to_next: u64, expires_in: u64 },
}

/// Falha na troca do código.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    RateLimited { retry_after_ms: u64 },
    Locked,
    Inactive,
    Invalid { attempts_left: u32 },
    Store,
}

/// Falha de gravação ou do gerador aleatório.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreError;

#[derive(Default)]
struct Inner {
    loaded: bool,
    devices: Vec<Device>,
    /// Última renovação do cookie por dispositivo (ms), só em memória.
    renewed: HashMap<String, u64>,
    entry: Option<EntryLink>,
    window: Option<CodeWindow>,
    locked: bool,
    last_attempt: Option<u64>,
}

/// Dispositivos autorizados, link de entrada e janela de adicionar. Clones de `AppState`
/// compartilham o mesmo (`Arc`).
pub struct DeviceStore {
    path: Option<PathBuf>,
    clock: DeviceClock,
    inner: Mutex<Inner>,
    /// Muda a cada revogação: os streams abertos conferem se o dispositivo deles ainda existe.
    revocations: watch::Sender<u64>,
}

impl std::fmt::Debug for DeviceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.inner.lock().unwrap_or_else(PoisonError::into_inner).devices.len();
        write!(f, "DeviceStore({n} dispositivo(s))")
    }
}

/// Bytes aleatórios do gerador seguro do sistema.
fn random_bytes<const N: usize>() -> Result<[u8; N], StoreError> {
    let mut buf = [0u8; N];
    getrandom::getrandom(&mut buf).map_err(|_| StoreError)?;
    Ok(buf)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// SHA-256 em hexadecimal.
pub fn hash_hex(value: &str) -> String {
    hex(&Sha256::digest(value.as_bytes()))
}

/// Comparação em tempo constante (para o mesmo tamanho).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// Token de dispositivo bem formado: 64 caracteres hexadecimais minúsculos.
fn token_well_formed(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Código de 6 dígitos uniforme (rejeição acima do maior múltiplo de 10^6).
fn new_code() -> Result<String, StoreError> {
    const LIMIT: u32 = 4_294_000_000;
    loop {
        let n = u32::from_le_bytes(random_bytes::<4>()?);
        if n < LIMIT {
            return Ok(format!("{:06}", n % 1_000_000));
        }
    }
}


/// Aviso do terminal para quem opera o servidor (spec 115 T04): autorização, revogação e problemas
/// da lista de dispositivos, no idioma do terminal (`LC_ALL` > `LC_MESSAGES` > `LANG`, como o resto
/// do `nants`).
fn terminal_notice(key: &str, params: &[(&str, &str)]) {
    eprintln!("{}", crate::i18n::tr(crate::i18n::Lang::from_env(), key, params));
}

impl DeviceStore {
    /// Dispositivos guardados em `path` (`None`: só em memória, sem pasta de dados).
    pub fn new(path: Option<PathBuf>) -> Self {
        Self::with_clock(path, Arc::new(system_now_ms))
    }

    pub fn with_clock(path: Option<PathBuf>, clock: DeviceClock) -> Self {
        Self { path, clock, inner: Mutex::new(Inner::default()), revocations: watch::channel(0).0 }
    }

    /// Caminho do arquivo dos dispositivos, se houver.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !inner.loaded {
            inner.loaded = true;
            inner.devices = self.load();
        }
        inner
    }

    fn load(&self) -> Vec<Device> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(_) => {
                terminal_notice("devices.log.list_unreadable", &[]);
                return Vec::new();
            }
        };
        if !meta.file_type().is_file() {
            terminal_notice("devices.log.list_not_file", &[]);
            return Vec::new();
        }
        let mut raw = String::new();
        let read = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .and_then(|f| f.take(MAX_FILE_BYTES).read_to_string(&mut raw));
        if read.is_err() {
            terminal_notice("devices.log.list_unreadable", &[]);
            return Vec::new();
        }
        match serde_json::from_str::<DeviceFile>(&raw) {
            Ok(file) => file.devices.into_iter().filter(|d| d.token_hash.len() == 64).collect(),
            Err(_) => {
                terminal_notice("devices.log.list_invalid", &[]);
                Vec::new()
            }
        }
    }

    /// Grava a lista (0600, atômica). Sem pasta de dados, fica só em memória.
    fn save(&self, devices: &[Device]) -> Result<(), StoreError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let body = serde_json::to_vec_pretty(&DeviceFile { version: FILE_VERSION, devices: devices.to_vec() })
            .map_err(|_| StoreError)?;
        let parent = path.parent().ok_or(StoreError)?;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(parent).map_err(|_| StoreError)?;
        let suffix = hex(&random_bytes::<6>()?);
        let tmp = parent.join(format!(".{DEVICES_FILE}.{suffix}.tmp"));
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .and_then(|mut f| {
                f.write_all(&body)?;
                f.sync_all()
            })
            .and_then(|()| fs::rename(&tmp, path));
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
            terminal_notice("devices.log.save_failed", &[]);
            return Err(StoreError);
        }
        Ok(())
    }

    /// Cria um dispositivo e devolve-o com o token (que só existe no cookie).
    fn create(&self, inner: &mut Inner, name: String, this_computer: bool) -> Result<(Device, String), StoreError> {
        let token = hex(&random_bytes::<32>()?);
        let id = hex(&random_bytes::<8>()?);
        let now_s = self.now() / 1000;
        let device = Device { id, name, token_hash: hash_hex(&token), created_at: now_s, last_seen: now_s, this_computer };
        inner.devices.push(device.clone());
        if let Err(e) = self.save(&inner.devices) {
            inner.devices.pop();
            return Err(e);
        }
        inner.renewed.insert(device.id.clone(), self.now());
        Ok((device, token))
    }

    /// Id do dispositivo do token, se válido. Atualiza `last_seen` (gravado no máximo uma vez por
    /// minuto). Confere o hash contra todos, em tempo constante.
    pub fn authenticate(&self, token: &str) -> Option<String> {
        if !token_well_formed(token) {
            return None;
        }
        let hash = hash_hex(token);
        let mut inner = self.lock();
        let mut found = None;
        for (i, d) in inner.devices.iter().enumerate() {
            if ct_eq(d.token_hash.as_bytes(), hash.as_bytes()) {
                found = Some(i);
            }
        }
        let i = found?;
        let now_s = self.now() / 1000;
        let id = inner.devices[i].id.clone();
        if now_s.saturating_sub(inner.devices[i].last_seen) >= LAST_SEEN_EVERY_SECS {
            inner.devices[i].last_seen = now_s;
            // Falha de gravação não impede o acesso: o último acesso fica em memória.
            let _ = self.save(&inner.devices);
        }
        Some(id)
    }

    /// O dispositivo existe e foi criado pelo link de entrada do terminal ("Este computador",
    /// spec 117)? A marcação vem só da origem do registro, nunca do nome (que é editável), e some
    /// junto com o dispositivo revogado.
    pub fn is_this_computer(&self, id: &str) -> bool {
        self.lock().devices.iter().any(|d| d.id == id && d.this_computer)
    }

    /// O dispositivo ainda existe?
    pub fn is_active(&self, id: &str) -> bool {
        self.lock().devices.iter().any(|d| d.id == id)
    }

    /// Receptor das revogações (cada uma muda o valor).
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.revocations.subscribe()
    }

    /// O cookie deste dispositivo deve ser renovado agora? Marca a renovação.
    pub fn renew_due(&self, id: &str) -> bool {
        let now = self.now();
        let mut inner = self.lock();
        let due = inner.renewed.get(id).is_none_or(|at| now.saturating_sub(*at) >= COOKIE_RENEW_EVERY_MS);
        if due {
            inner.renewed.insert(id.to_string(), now);
        }
        due
    }

    /// Gera o link de entrada (uso único, [`ENTRY_LINK_TTL_MS`]); substitui o anterior. Devolve o
    /// valor do parâmetro `t`.
    pub fn issue_entry_link(&self, lang: Lang) -> Result<String, StoreError> {
        let token = hex(&random_bytes::<16>()?);
        let expires_at = self.now().saturating_add(ENTRY_LINK_TTL_MS);
        self.lock().entry = Some(EntryLink { hash: hash_hex(&token), expires_at, lang });
        Ok(token)
    }

    /// Confere e consome o link (`None`: inválido ou expirado). Devolve o idioma do link.
    fn take_entry_link(inner: &mut Inner, token: &str, now: u64) -> Option<Lang> {
        let entry = inner.entry.as_ref()?;
        if now >= entry.expires_at {
            inner.entry = None;
            return None;
        }
        if !ct_eq(entry.hash.as_bytes(), hash_hex(token).as_bytes()) {
            return None;
        }
        let lang = entry.lang;
        inner.entry = None;
        Some(lang)
    }

    /// Consome o link sem criar dispositivo (navegador já autorizado).
    pub fn consume_entry_link(&self, token: &str) -> bool {
        let now = self.now();
        Self::take_entry_link(&mut self.lock(), token, now).is_some()
    }

    /// Troca o link de entrada pelo dispositivo "Este computador". `Ok(None)`: link inválido ou
    /// expirado.
    pub fn redeem_entry_link(&self, token: &str) -> Result<Option<(Device, String)>, StoreError> {
        let now = self.now();
        let mut inner = self.lock();
        let Some(lang) = Self::take_entry_link(&mut inner, token, now) else {
            return Ok(None);
        };
        self.create(&mut inner, text(lang, "devices.this_computer"), true).map(Some)
    }

    /// Abre (ou reabre) a janela de adicionar: código novo, erros zerados.
    pub fn open_window(&self) -> Result<CodeState, StoreError> {
        let now = self.now();
        let code = new_code()?;
        let mut inner = self.lock();
        inner.window = Some(CodeWindow { expires_at: now.saturating_add(CODE_WINDOW_MS), code, code_since: now, errors: 0 });
        inner.locked = false;
        Ok(Self::view_window(&mut inner, now))
    }

    /// Fecha a janela de adicionar (e a trava de erros).
    pub fn close_window(&self) {
        let mut inner = self.lock();
        inner.window = None;
        inner.locked = false;
    }

    /// Estado atual da janela (troca o código se passou o minuto).
    pub fn code_state(&self) -> CodeState {
        let now = self.now();
        Self::view_window(&mut self.lock(), now)
    }

    /// Troca o código vencido e fecha a janela vencida.
    fn rotate(inner: &mut Inner, now: u64) -> Result<(), StoreError> {
        let expired = inner.window.as_ref().is_some_and(|w| now >= w.expires_at);
        if expired {
            inner.window = None;
        }
        if let Some(w) = inner.window.as_mut() {
            if now >= w.code_since.saturating_add(CODE_ROTATE_MS) {
                let steps = (now - w.code_since) / CODE_ROTATE_MS;
                w.code_since += steps * CODE_ROTATE_MS;
                w.code = new_code()?;
            }
        }
        Ok(())
    }

    fn view_window(inner: &mut Inner, now: u64) -> CodeState {
        if Self::rotate(inner, now).is_err() {
            inner.window = None;
        }
        if inner.locked {
            return CodeState::Locked;
        }
        match &inner.window {
            None => CodeState::Closed,
            Some(w) => {
                let next = w.code_since.saturating_add(CODE_ROTATE_MS).min(w.expires_at);
                CodeState::Open {
                    code: w.code.clone(),
                    seconds_to_next: next.saturating_sub(now).div_ceil(1000),
                    expires_in: w.expires_at.saturating_sub(now).div_ceil(1000),
                }
            }
        }
    }

    /// Troca do código pelo dispositivo. O código nunca vai para o log.
    pub fn pair(&self, code: &str, name: String) -> Result<(Device, String), PairError> {
        let now = self.now();
        let mut inner = self.lock();
        if let Some(last) = inner.last_attempt {
            let since = now.saturating_sub(last);
            if since < PAIR_MIN_INTERVAL_MS {
                return Err(PairError::RateLimited { retry_after_ms: PAIR_MIN_INTERVAL_MS - since });
            }
        }
        inner.last_attempt = Some(now);
        if inner.locked {
            return Err(PairError::Locked);
        }
        if Self::rotate(&mut inner, now).is_err() {
            inner.window = None;
        }
        let Some(window) = inner.window.as_mut() else {
            return Err(PairError::Inactive);
        };
        let code = code.trim();
        if !ct_eq(window.code.as_bytes(), code.as_bytes()) {
            window.errors += 1;
            let errors = window.errors;
            terminal_notice("devices.log.wrong_code", &[("errors", &errors.to_string()), ("max", &MAX_CODE_ERRORS.to_string())]);
            if errors >= MAX_CODE_ERRORS {
                inner.window = None;
                inner.locked = true;
                terminal_notice("devices.log.code_invalidated", &[]);
            }
            return Err(PairError::Invalid { attempts_left: MAX_CODE_ERRORS.saturating_sub(errors) });
        }
        // Uso único: a janela fecha com o acerto.
        inner.window = None;
        inner.locked = false;
        self.create(&mut inner, name, false).map_err(|_| PairError::Store)
    }

    /// Lista dos dispositivos.
    pub fn list(&self) -> Vec<Device> {
        self.lock().devices.clone()
    }

    /// Renomeia. `Ok(None)`: id desconhecido.
    pub fn rename(&self, id: &str, name: String) -> Result<Option<Device>, StoreError> {
        let mut inner = self.lock();
        let Some(i) = inner.devices.iter().position(|d| d.id == id) else {
            return Ok(None);
        };
        let previous = std::mem::replace(&mut inner.devices[i].name, name);
        if let Err(e) = self.save(&inner.devices) {
            inner.devices[i].name = previous;
            return Err(e);
        }
        Ok(Some(inner.devices[i].clone()))
    }

    /// Revoga um dispositivo. `Ok(None)`: id desconhecido. Derruba os streams dele.
    pub fn revoke(&self, id: &str) -> Result<Option<Device>, StoreError> {
        let removed = {
            let mut inner = self.lock();
            let Some(i) = inner.devices.iter().position(|d| d.id == id) else {
                return Ok(None);
            };
            let removed = inner.devices.remove(i);
            inner.renewed.remove(id);
            // A revogação vale em memória mesmo se a gravação falhar; o erro é devolvido.
            let saved = self.save(&inner.devices);
            (removed, saved)
        };
        self.revocations.send_modify(|v| *v = v.wrapping_add(1));
        removed.1.map(|()| Some(removed.0))
    }

    /// Revoga todos; devolve quantos eram.
    pub fn revoke_all(&self) -> Result<usize, StoreError> {
        let (n, saved) = {
            let mut inner = self.lock();
            let n = inner.devices.len();
            inner.devices.clear();
            inner.renewed.clear();
            (n, self.save(&[]))
        };
        self.revocations.send_modify(|v| *v = v.wrapping_add(1));
        saved.map(|()| n)
    }
}

// ---------------------------------------------------------------------------------------------
// Nome sugerido pelo User-Agent.

/// Nome curto a partir do User-Agent, por exemplo "Safari no iPhone" ou "Chrome on Windows".
pub fn suggest_name(user_agent: Option<&str>, lang: Lang) -> String {
    let ua = user_agent.unwrap_or("");
    let os = if ua.contains("iPhone") {
        Some("iPhone")
    } else if ua.contains("iPad") {
        Some("iPad")
    } else if ua.contains("Android") {
        Some("Android")
    } else if ua.contains("CrOS") {
        Some("ChromeOS")
    } else if ua.contains("Windows") {
        Some("Windows")
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        Some("Mac")
    } else if ua.contains("Linux") {
        Some("Linux")
    } else {
        None
    };
    let browser = if ua.contains("Edg/") || ua.contains("EdgiOS") || ua.contains("EdgA/") {
        Some("Edge")
    } else if ua.contains("OPR/") || ua.contains("Opera") {
        Some("Opera")
    } else if ua.contains("SamsungBrowser") {
        Some("Samsung Internet")
    } else if ua.contains("Firefox/") || ua.contains("FxiOS") {
        Some("Firefox")
    } else if ua.contains("Chrome/") || ua.contains("CriOS") {
        Some("Chrome")
    } else if ua.contains("Safari/") {
        Some("Safari")
    } else {
        None
    };
    match (browser, os) {
        (Some(b), Some(o)) => tr(lang, "devices.name_browser_os", &[("browser", b), ("os", o)]),
        (None, Some(o)) => tr(lang, "devices.name_browser_os", &[("browser", &text(lang, "devices.browser")), ("os", o)]),
        (Some(b), None) => b.to_string(),
        (None, None) => text(lang, "devices.fallback_name"),
    }
}

/// Nome curto e limpo: sem caracteres de controle nem de direção do texto, espaços colapsados e no
/// máximo [`MAX_NAME_CHARS`] caracteres. `None` se ficar vazio.
pub fn sanitize_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() || is_invisible_format(c) { ' ' } else { c })
        .collect();
    let name: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_NAME_CHARS).collect();
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn is_invisible_format(c: char) -> bool {
    matches!(c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2069 | 0xFEFF)
}

/// Idioma do pedido: o cabeçalho da interface, senão o `Accept-Language`, senão pt-BR.
fn request_lang(headers: &HeaderMap) -> Lang {
    if let Some(lang) = super::lang::header_lang(headers) {
        return lang;
    }
    headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split([',', ';']).next())
        .map_or_else(Lang::default, Lang::from_locale)
}

// ---------------------------------------------------------------------------------------------
// Camada de autorização.

/// Quem fez a requisição, posto nas extensões por [`require_device`].
#[derive(Debug, Clone, Default)]
pub struct Access {
    /// Dispositivo autorizado pelo cookie, se houver.
    pub device: Option<String>,
    /// Conexão de loopback de verdade (127.0.0.0/8 ou ::1 vistos pelo Nants).
    pub loopback: bool,
}

/// A origem da conexão é loopback de verdade? Sem a informação da conexão, não é.
pub fn peer_is_loopback(req: &Request) -> bool {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(addr)| addr.ip().to_canonical().is_loopback())
}

/// Valores do cookie do dispositivo nos cabeçalhos `Cookie`.
fn cookie_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(name, _)| *name == COOKIE_NAME)
        .map(|(_, value)| value.trim().trim_matches('"').to_string())
        .collect()
}

/// Cabeçalho `Set-Cookie` do dispositivo.
pub fn device_cookie(token: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={token}; Max-Age={COOKIE_MAX_AGE_SECS}; Path=/; HttpOnly; SameSite=Strict"
    ))
    .ok()
}

/// Cabeçalho `Set-Cookie` que apaga o cookie do dispositivo.
pub fn clear_cookie() -> HeaderValue {
    HeaderValue::from_static("nants_device=; Max-Age=0; Path=/; HttpOnly; SameSite=Strict")
}

/// Rota aberta sem dispositivo (ver o comentário do módulo).
pub fn is_open(method: &Method, path: &str) -> bool {
    if *method == Method::POST {
        return path == PAIR_API_PATH;
    }
    if *method != Method::GET && *method != Method::HEAD {
        return false;
    }
    if path == ENTRY_PATH || path == PAIR_LINK_PATH {
        return true;
    }
    if path.contains("..") || path.contains('%') || path.contains('\\') {
        return false;
    }
    OPEN_STATIC_PREFIXES.iter().any(|p| path.len() > p.len() && path.starts_with(p)) || OPEN_STATIC_FILES.contains(&path)
}

fn is_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

fn redirect(location: &str) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::SEE_OTHER;
    if let Ok(v) = HeaderValue::from_str(location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

fn device_required() -> ApiError {
    ApiError::coded(StatusCode::UNAUTHORIZED, CODE_DEVICE_REQUIRED, Map::new())
}

/// Resposta sem dispositivo: 401 JSON na API e redirecionamento para a entrada nas páginas.
fn deny(method: &Method, path: &str) -> Response {
    if is_api(path) || (*method != Method::GET && *method != Method::HEAD) {
        return device_required().into_response();
    }
    redirect(ENTRY_PATH)
}

/// Corta o corpo (streams SSE) quando o dispositivo for revogado.
fn cut_on_revoke(resp: Response, store: Arc<DeviceStore>, id: String) -> Response {
    let (parts, body) = resp.into_parts();
    let mut rx = store.subscribe();
    let stop = async move {
        loop {
            if !store.is_active(&id) {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    };
    let stream = body.into_data_stream().take_until(Box::pin(stop));
    Response::from_parts(parts, Body::from_stream(stream))
}

fn is_event_stream(resp: &Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
}

/// Retorno do login pelo plano ChatGPT (spec 116) com um `state` pendente e válido: passa sem
/// dispositivo, e o handler consome o `state` e confere o dispositivo que iniciou a tentativa.
/// Sem `state` válido, a rota segue a regra de sempre.
fn chatgpt_callback_pending(state: &AppState, req: &Request) -> bool {
    req.method() == Method::GET
        && req.uri().path() == super::chatgpt::CALLBACK_PATH
        && super::chatgpt::callback_state(req.uri()).is_some_and(|s| state.chatgpt().callback_pending(&s))
}

/// Camada depois da guarda de `Host`/`Origin`: em modo rede, exige um dispositivo autorizado em
/// tudo o que não venha de loopback de verdade, fora das rotas abertas.
pub async fn require_device(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let loopback = peer_is_loopback(&req);
    let store = Arc::clone(state.devices());
    let mut device = None;
    for token in cookie_tokens(req.headers()) {
        if let Some(id) = store.authenticate(&token) {
            device = Some((id, token));
            break;
        }
    }
    let lan = state.lan_mode();
    if lan && !loopback && device.is_none() && !is_open(req.method(), req.uri().path()) && !chatgpt_callback_pending(&state, &req) {
        return deny(req.method(), req.uri().path());
    }
    req.extensions_mut().insert(Access { device: device.as_ref().map(|(id, _)| id.clone()), loopback });
    let mut resp = next.run(req).await;
    let Some((id, token)) = device else {
        return resp;
    };
    // Todo stream do dispositivo passa pelo corte, sem conferência antes: o corte assina as
    // revogações e confere `is_active` no primeiro poll, então uma revogação feita durante o
    // handler (ou a qualquer momento depois) encerra o stream.
    if lan && !loopback && is_event_stream(&resp) {
        resp = cut_on_revoke(resp, Arc::clone(&store), id.clone());
    }
    // Revogado: só não renova o cookie.
    if store.is_active(&id) && store.renew_due(&id) && !resp.headers().contains_key(header::SET_COOKIE) {
        if let Some(cookie) = device_cookie(&token) {
            resp.headers_mut().append(header::SET_COOKIE, cookie);
        }
    }
    resp
}

// ---------------------------------------------------------------------------------------------
// Rotas.

/// Gestão só de dispositivo autorizado ou loopback (fora do modo rede, como hoje).
fn require_manager(state: &AppState, access: &Access) -> Result<(), ApiError> {
    if !state.lan_mode() || access.loopback || access.device.is_some() {
        Ok(())
    } else {
        Err(device_required())
    }
}

fn store_failed() -> ApiError {
    ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, CODE_STORE_FAILED, Map::new())
}

fn publish_changed(state: &AppState, reason: &str) {
    state.events().publish(ApiEvent::new(EventType::DevicesChanged, None, json!({"reason": reason})));
}

#[derive(Deserialize)]
pub struct PairLinkQuery {
    t: Option<String>,
}

/// `GET /pair?t=...`: link de entrada de uso único. Válido: cria "Este computador", grava o cookie
/// e volta para `/`. Inválido ou expirado: a página de entrada com o motivo.
pub async fn get_pair_link(
    State(state): State<AppState>,
    method: Method,
    Extension(access): Extension<Access>,
    Query(query): Query<PairLinkQuery>,
) -> Response {
    if !state.lan_mode() {
        return redirect("/");
    }
    // HEAD (pré-visualização de link) nunca consome o link.
    if method == Method::HEAD {
        return redirect(ENTRY_PATH);
    }
    let token = query.t.unwrap_or_default();
    let store = state.devices();
    if access.device.is_some() {
        store.consume_entry_link(&token);
        return redirect("/");
    }
    match store.redeem_entry_link(&token) {
        Ok(Some((device, token))) => {
            terminal_notice("devices.log.authorized_link", &[("id", &device.id), ("name", &device.name)]);
            publish_changed(&state, "added");
            let mut resp = redirect("/");
            if let Some(cookie) = device_cookie(&token) {
                resp.headers_mut().append(header::SET_COOKIE, cookie);
            }
            resp
        }
        Ok(None) => redirect(&format!("{ENTRY_PATH}?reason=link_invalid")),
        Err(_) => store_failed().into_response(),
    }
}

/// Endereços de entrada para o outro aparelho: o `Host` da página (se for um IP da rede) e os IPv4
/// da rede (fora do WSL, os locais; no WSL, os do Windows), todos na porta da rede.
async fn entry_urls(state: &AppState, headers: &HeaderMap) -> Vec<String> {
    let mut urls = Vec::new();
    let port = super::lan::lan_port(state.network_run(), state.bound_port());
    if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        let name = super::app::host_name(host);
        let ip = name.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>();
        if ip.is_ok_and(|ip| !ip.is_loopback() && !ip.is_unspecified()) {
            urls.push(format!("http://{host}{ENTRY_PATH}"));
        }
    }
    let probe = state.net_probe();
    // No WSL, os IPs da rede são os do Windows (conferidos só lendo); sem conferência, só o Host.
    let ips = if probe.wsl { state.windows_ips().await.unwrap_or_default() } else { probe.ips() };
    for ip in ips {
        if ip.is_loopback() || ip.is_unspecified() {
            continue;
        }
        let url = format!("http://{ip}:{port}{ENTRY_PATH}");
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    urls
}

async fn code_view(state: &AppState, headers: &HeaderMap, code: CodeState) -> Value {
    let base = json!({
        "entry_path": ENTRY_PATH,
        "entry_urls": entry_urls(state, headers).await,
        "max_errors": MAX_CODE_ERRORS,
    });
    let mut out = match code {
        CodeState::Closed => json!({"state": "closed"}),
        CodeState::Locked => json!({"state": "locked"}),
        CodeState::Open { code, seconds_to_next, expires_in } => json!({
            "state": "open",
            "code": code,
            "seconds_to_next": seconds_to_next,
            "expires_in": expires_in,
        }),
    };
    if let (Some(out), Value::Object(base)) = (out.as_object_mut(), base) {
        out.extend(base);
    }
    out
}

/// `POST /api/devices/code`: abre (ou reabre) a janela de adicionar e devolve o código atual.
pub async fn post_code(
    State(state): State<AppState>,
    Extension(access): Extension<Access>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_manager(&state, &access)?;
    let code = state.devices().open_window().map_err(|_| store_failed())?;
    Ok(Json(code_view(&state, &headers, code).await).into_response())
}

/// `GET /api/devices/code`: estado da janela (o código atual, se aberta).
pub async fn get_code(
    State(state): State<AppState>,
    Extension(access): Extension<Access>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_manager(&state, &access)?;
    let code = state.devices().code_state();
    Ok(Json(code_view(&state, &headers, code).await).into_response())
}

/// `DELETE /api/devices/code`: fecha a janela.
pub async fn delete_code(State(state): State<AppState>, Extension(access): Extension<Access>) -> Result<Response, ApiError> {
    require_manager(&state, &access)?;
    state.devices().close_window();
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairInput {
    code: String,
    #[serde(default)]
    name: Option<String>,
}

/// `POST /api/devices/pair {code, name?}`: sem autenticação. Acertou: cria o dispositivo, grava o
/// cookie e fecha a janela.
pub async fn post_pair(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Result<Response, ApiError> {
    // Corpo inválido também conta no limite por segundo (a conferência vem antes de ler o corpo).
    let input: Result<PairInput, ApiError> = super::config_api::parse_body(&body);
    let (code, name) = match input {
        Ok(i) => (i.code, i.name),
        Err(_) => (String::new(), None),
    };
    let lang = request_lang(&headers);
    let name = name
        .as_deref()
        .and_then(sanitize_name)
        .unwrap_or_else(|| suggest_name(headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()), lang));
    match state.devices().pair(&code, name) {
        Ok((device, token)) => {
            terminal_notice("devices.log.authorized_code", &[("id", &device.id), ("name", &device.name)]);
            publish_changed(&state, "added");
            let mut resp = Json(json!({"device": device.view(Some(&device.id))})).into_response();
            if let Some(cookie) = device_cookie(&token) {
                resp.headers_mut().append(header::SET_COOKIE, cookie);
            }
            Ok(resp)
        }
        Err(PairError::RateLimited { retry_after_ms }) => {
            let err = ApiError::coded(StatusCode::TOO_MANY_REQUESTS, CODE_PAIR_RATE_LIMITED, params([("retry_after_ms", json!(retry_after_ms))]));
            let mut resp = err.into_response();
            resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            Ok(resp)
        }
        Err(PairError::Locked) => Err(ApiError::coded(StatusCode::TOO_MANY_REQUESTS, CODE_CODE_LOCKED, Map::new())),
        Err(PairError::Inactive) => Err(ApiError::coded(StatusCode::BAD_REQUEST, CODE_CODE_INACTIVE, Map::new())),
        Err(PairError::Invalid { attempts_left }) => {
            if attempts_left == 0 {
                state.events().publish(ApiEvent::new(
                    EventType::DeviceCodeLocked,
                    None,
                    json!({"reason": "too_many_errors"}),
                ));
            }
            Err(ApiError::coded(StatusCode::BAD_REQUEST, CODE_CODE_INVALID, params([("attempts_left", json!(attempts_left))])))
        }
        Err(PairError::Store) => Err(store_failed()),
    }
}

/// `GET /api/devices`: lista sem tokens, marcando o atual.
pub async fn get_devices(State(state): State<AppState>, Extension(access): Extension<Access>) -> Result<Response, ApiError> {
    require_manager(&state, &access)?;
    let current = access.device.as_deref();
    let devices: Vec<Value> = state.devices().list().iter().map(|d| d.view(current)).collect();
    Ok(Json(json!({"devices": devices, "current": current})).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameInput {
    name: String,
}

fn not_found() -> ApiError {
    ApiError::coded(StatusCode::NOT_FOUND, CODE_NOT_FOUND, Map::new())
}

fn valid_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `PATCH /api/devices/:id {name}`: renomeia.
pub async fn patch_device(
    State(state): State<AppState>,
    Extension(access): Extension<Access>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    require_manager(&state, &access)?;
    if !valid_id(&id) {
        return Err(not_found());
    }
    let input: RenameInput = super::config_api::parse_body(&body)?;
    let name = sanitize_name(&input.name).ok_or_else(|| {
        ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, CODE_NAME_INVALID, params([("max", json!(MAX_NAME_CHARS))]))
    })?;
    match state.devices().rename(&id, name) {
        Ok(Some(device)) => {
            publish_changed(&state, "renamed");
            Ok(Json(json!({"device": device.view(access.device.as_deref())})).into_response())
        }
        Ok(None) => Err(not_found()),
        Err(_) => Err(store_failed()),
    }
}

/// `DELETE /api/devices/:id`: revoga e derruba as conexões abertas do dispositivo.
pub async fn delete_device(
    State(state): State<AppState>,
    Extension(access): Extension<Access>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, ApiError> {
    require_manager(&state, &access)?;
    if !valid_id(&id) {
        return Err(not_found());
    }
    let revoked = state.devices().revoke(&id);
    match revoked {
        Ok(Some(device)) => terminal_notice("devices.log.revoked", &[("id", &device.id), ("name", &device.name)]),
        Ok(None) => return Err(not_found()),
        Err(_) => {
            publish_changed(&state, "revoked");
            return Err(store_failed());
        }
    }
    publish_changed(&state, "revoked");
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if access.device.as_deref() == Some(id.as_str()) {
        resp.headers_mut().append(header::SET_COOKIE, clear_cookie());
    }
    Ok(resp)
}

/// `DELETE /api/devices`: revoga todos.
pub async fn delete_devices(State(state): State<AppState>, Extension(access): Extension<Access>) -> Result<Response, ApiError> {
    require_manager(&state, &access)?;
    let result = state.devices().revoke_all();
    publish_changed(&state, "revoked");
    let n = result.map_err(|_| store_failed())?;
    terminal_notice("devices.log.all_revoked", &[("count", &n.to_string())]);
    let mut resp = Json(json!({"revoked": n})).into_response();
    if access.device.is_some() {
        resp.headers_mut().append(header::SET_COOKIE, clear_cookie());
    }
    Ok(resp)
}

#[cfg(test)]
#[path = "devices_tests.rs"]
mod tests;
