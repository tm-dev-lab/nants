//! Conexão pelo plano ChatGPT (spec 116, T01): login OAuth com PKCE, credenciais e sair.
//!
//! - **Início** (`POST /api/connections/chatgpt/start`): só loopback ou dispositivo autorizado
//!   (spec 112). Gera `state`, `nonce` e o verificador PKCE (S256), novos a cada tentativa, e
//!   devolve a URL de autorização. No primeiro login: `client_id=dynamic_agent_client`,
//!   `agent_name_hint=Nants` e o `ext_agent_host_id` da instalação. Na reautorização: o
//!   `client_id` emitido, `id_token_hint` e `login_hint`. O `redirect_uri` é sempre
//!   `http://127.0.0.1:<porta do Nants>/auth/callback`. A tentativa vale [`ATTEMPT_TTL_MS`], é de
//!   uso único e fica ligada ao dispositivo que a iniciou (quando não veio de loopback). `local` vale
//!   para o loopback e para o dispositivo do link de entrada do terminal (spec 117).
//! - **Retorno** (`GET /auth/callback`, spec 118): é uma página de passagem. A volta vem de outro
//!   site (`auth.openai.com`); com o cookie `SameSite=Strict` de antes da spec 128, essa navegação
//!   chegava sem ele, e com o `Lax` de hoje ele pode vir ou não (navegador, extensão, aba nova), por
//!   isso a passagem continua. Com um `state` pendente, a rota responde 200 com uma página mínima, sem
//!   consumir nada e sem exigir cookie, que navega na hora (meta refresh, sem script; com o link
//!   "Continuar" de reserva) para a conclusão na mesma origem. Uma navegação iniciada por uma
//!   página do próprio site leva o cookie. A página leva `Referrer-Policy: no-referrer`,
//!   `Cache-Control: no-store`, uma CSP sem script e nenhum recurso de fora.
//! - **Conclusão** (`GET /auth/finish`): confere quem pode concluir antes de consumir o `state`
//!   (o dispositivo que iniciou, pelo cookie, ou o loopback), troca o código (form-urlencoded, sem
//!   secret, com o `code_verifier`, o mesmo `redirect_uri` e o `resource`), valida o ID token
//!   ([`jwt`]: RS256 pelo JWKS com cache, `iss`, `aud`, `exp` e `nonce`), confere o escopo
//!   [`DIRECT_SCOPE`], recusa um `client_id` diferente do salvo, grava as credenciais
//!   ([`store`], 0600) e cria ou atualiza a conexão. Responde com um redirecionamento para a
//!   interface com o resultado (`/?chatgpt=connected&connection=<id>` ou
//!   `/?chatgpt=error&reason=<motivo>`). Em modo rede, só o retorno passa sem dispositivo, e só
//!   com um `state` pendente ([`ChatgptAuth::callback_pending`]); a conclusão segue a regra de
//!   sempre da camada de dispositivos.
//! - **Renovação** ([`ChatgptAuth::access_token`]): renova quando faltam menos de
//!   [`REFRESH_MARGIN_SECS`]; uma renovação por conexão de cada vez; erro terminal
//!   (`invalid_grant`, `refresh_token_*`) marca "precisa entrar de novo" e não repete.
//! - **Sair** (`POST /api/connections/:id/chatgpt/logout`): revoga o refresh token (falha de rede
//!   vira aviso), apaga as credenciais e a conexão fica desconectada.
//! - **Estado** (`GET /api/connections/:id/chatgpt`): conectada (com o e-mail), precisa entrar de
//!   novo ou desconectada, sem segredos.
//!
//! Tokens, código, verificador e `state` nunca vão para o log nem para a resposta.

pub mod jwt;
pub mod plan_client;
pub mod store;
#[cfg(test)]
mod plan_tests;
#[cfg(test)]
mod test_keys;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::devices::Access;
use super::error::ApiError;
use super::state::AppState;
use crate::config::Connection;
use store::Credentials;

/// Endpoints oficiais (developers.openai.com/siwc e `openid-configuration`).
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
pub const REVOKE_URL: &str = "https://auth.openai.com/api/accounts/oauth/revoke";
pub const JWKS_URL: &str = "https://auth.openai.com/.well-known/jwks.json";
/// `iss` exato do ID token.
pub const ISSUER: &str = "https://auth.openai.com";
/// `resource` do pedido de autorização, da troca e da renovação.
pub const RESOURCE: &str = "https://api.openai.com/v1";
/// Escopos pedidos.
pub const SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// Escopo sem o qual a inferência pelo plano não é feita.
pub const DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";
/// `client_id` do cadastro dinâmico (só no primeiro login).
pub const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
/// `agent_name_hint` (só no primeiro login).
pub const AGENT_NAME_HINT: &str = "Nants";
/// Rota do retorno do login (loopback).
pub const CALLBACK_PATH: &str = "/auth/callback";
/// Rota da conclusão do login (spec 118): a página de passagem do retorno navega para cá.
pub const FINISH_PATH: &str = "/auth/finish";
/// CSP da página de passagem: nada de script, estilo, imagem nem moldura; a navegação é pelo meta
/// refresh e pelo link.
const PASSAGE_CSP: &str = "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
/// Rota do início do login.
pub const START_PATH: &str = "/api/connections/chatgpt/start";
/// Validade da tentativa de login (10 minutos).
pub const ATTEMPT_TTL_MS: u64 = 10 * 60 * 1000;
/// Renova o access token quando faltam menos que isto (5 minutos).
pub const REFRESH_MARGIN_SECS: u64 = 5 * 60;
/// Validade do access token quando a resposta não diz (1 hora, pela documentação).
const DEFAULT_EXPIRES_IN: u64 = 60 * 60;
/// Tentativas pendentes guardadas ao mesmo tempo; além disso, a mais antiga sai.
const MAX_PENDING: usize = 16;
/// Tempo total de cada chamada ao servidor de autenticação.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Validade do JWKS em cache.
const JWKS_TTL_MS: u64 = 60 * 60 * 1000;
/// Teto do `client_id` emitido aceito.
const MAX_CLIENT_ID_CHARS: usize = 200;
/// Códigos estáveis (spec 115) dos erros desta área.
const ERR_NOT_PLAN: &str = "chatgpt_not_plan";
const ERR_STORE: &str = "chatgpt_store_failed";

/// Endereços do servidor de autenticação (testes apontam para um servidor local simulado).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatgptEndpoints {
    pub authorize: String,
    pub token: String,
    pub revoke: String,
    pub jwks: String,
}

impl Default for ChatgptEndpoints {
    fn default() -> Self {
        Self {
            authorize: AUTHORIZE_URL.to_string(),
            token: TOKEN_URL.to_string(),
            revoke: REVOKE_URL.to_string(),
            jwks: JWKS_URL.to_string(),
        }
    }
}

/// Relógio em milissegundos desde a época Unix (injetável nos testes).
pub type AuthClock = Arc<dyn Fn() -> u64 + Send + Sync>;

fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    super::devices::ct_eq(a, b)
}

fn random_token() -> Option<String> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).ok()?;
    Some(jwt::b64url_encode(&buf))
}

/// Desafio PKCE S256 do verificador.
pub fn pkce_challenge(verifier: &str) -> String {
    jwt::b64url_encode(&Sha256::digest(verifier.as_bytes()))
}

/// Tentativa de login pendente. Sem `Debug` derivado: o manual não mostra segredos.
#[derive(Clone)]
struct Attempt {
    verifier: String,
    nonce: String,
    created_ms: u64,
    redirect_uri: String,
    /// Conexão a atualizar (reautorização ou reconectar); `None` cria uma nova.
    connection_id: Option<String>,
    /// Nome pedido no início (opcional).
    name: Option<String>,
    /// `client_id` emitido já salvo (reautorização).
    saved_client_id: Option<String>,
    /// Dispositivo que iniciou, quando não veio de loopback em modo rede.
    initiator: Option<String>,
}

impl std::fmt::Debug for Attempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Attempt({:?})", self.connection_id)
    }
}

/// JWKS em cache, com o instante da busca.
struct JwksCache {
    fetched_ms: u64,
    keys: Vec<jwt::Jwk>,
}

/// Estado do login pelo plano ChatGPT, compartilhado pelo servidor.
pub struct ChatgptAuth {
    endpoints: ChatgptEndpoints,
    http: reqwest::Client,
    clock: AuthClock,
    /// Tentativas pendentes, pelo SHA-256 do `state`.
    attempts: Mutex<HashMap<String, Attempt>>,
    jwks: tokio::sync::Mutex<Option<JwksCache>>,
    /// Uma renovação (ou gravação) por conexão de cada vez.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Serializa a criação do identificador da instalação.
    host_id: Mutex<()>,
}

impl std::fmt::Debug for ChatgptAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChatgptAuth")
    }
}

impl Default for ChatgptAuth {
    fn default() -> Self {
        Self::new(ChatgptEndpoints::default(), Arc::new(system_now_ms))
    }
}

/// Estado de uma conexão pelo plano, para a API (sem segredos).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PlanStatus {
    Connected { email: String },
    NeedsLogin { email: String },
    Disconnected,
}

/// Por que não há access token utilizável.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// Sem credenciais (nunca entrou ou saiu).
    NotConnected,
    /// Precisa entrar de novo (renovação recusada de vez ou escopo faltando).
    NeedsLogin,
    /// Servidor de autenticação indisponível ou resposta ruim: tentar depois.
    Unavailable,
    /// Pasta de dados indisponível ou falha ao gravar.
    Store,
}

/// Motivo de um login recusado no retorno (vai à interface pelo redirecionamento).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFailure {
    StateInvalid,
    AccessDenied,
    AuthorizeFailed,
    CodeMissing,
    ClientIdMissing,
    ClientIdMismatch,
    TokenExchangeFailed,
    TokenResponseInvalid,
    IdTokenInvalid,
    ScopeMissing,
    StoreFailed,
}

impl LoginFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            LoginFailure::StateInvalid => "state_invalid",
            LoginFailure::AccessDenied => "access_denied",
            LoginFailure::AuthorizeFailed => "authorize_failed",
            LoginFailure::CodeMissing => "code_missing",
            LoginFailure::ClientIdMissing => "client_id_missing",
            LoginFailure::ClientIdMismatch => "client_id_mismatch",
            LoginFailure::TokenExchangeFailed => "token_exchange_failed",
            LoginFailure::TokenResponseInvalid => "token_response_invalid",
            LoginFailure::IdTokenInvalid => "id_token_invalid",
            LoginFailure::ScopeMissing => "scope_missing",
            LoginFailure::StoreFailed => "store_failed",
        }
    }
}

/// Resposta do endpoint de token. Sem `Debug`: só tokens.
#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

/// Resultado de uma chamada ao endpoint de token.
enum TokenCall {
    Ok(TokenResponse),
    /// Erro OAuth com o código (`error` texto, ou `error.code`/`error.type`).
    Rejected(Option<String>),
    /// Rede, tempo esgotado ou resposta ilegível.
    Unavailable,
}

/// Código do erro OAuth do corpo: `{"error": "x"}` ou `{"error": {"code"|"type": "x"}}`.
fn oauth_error_code(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    match value.get("error")? {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("code").or_else(|| o.get("type")).and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// Erro de renovação terminal: não adianta repetir, só entrar de novo.
pub fn is_terminal_refresh_error(code: &str) -> bool {
    code == "invalid_grant" || code.starts_with("refresh_token_")
}

fn scopes_of(scope: Option<&str>) -> Vec<String> {
    match scope {
        Some(s) => s.split_whitespace().map(str::to_string).collect(),
        // RFC 6749 §5.1: sem `scope`, valem os pedidos.
        None => SCOPES.split_whitespace().map(str::to_string).collect(),
    }
}

/// `client_id` emitido aceitável: curto e só `[A-Za-z0-9_.-]`.
fn valid_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MAX_CLIENT_ID_CHARS
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn terminal_notice(key: &str, params: &[(&str, &str)]) {
    eprintln!("{}", crate::i18n::tr(crate::i18n::Lang::from_env(), key, params));
}

impl ChatgptAuth {
    pub fn new(endpoints: ChatgptEndpoints, clock: AuthClock) -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            endpoints,
            http,
            clock,
            attempts: Mutex::new(HashMap::new()),
            jwks: tokio::sync::Mutex::new(None),
            locks: Mutex::new(HashMap::new()),
            host_id: Mutex::new(()),
        }
    }

    fn now_ms(&self) -> u64 {
        (self.clock)()
    }

    fn now_secs(&self) -> u64 {
        self.now_ms() / 1000
    }

    fn lock_for(&self, connection_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(map.entry(connection_id.to_string()).or_default())
    }

    /// Identificador da instalação (criado e gravado na primeira vez).
    pub fn host_id(&self, dir: &Path) -> std::io::Result<String> {
        let _guard = self.host_id.lock().unwrap_or_else(PoisonError::into_inner);
        store::load_or_create_host_id(dir)
    }

    fn state_key(state: &str) -> String {
        hex(&Sha256::digest(state.as_bytes()))
    }

    /// Guarda a tentativa (tira as vencidas e, no teto, a mais antiga).
    fn remember(&self, state: &str, attempt: Attempt) {
        let now = self.now_ms();
        let mut map = self.attempts.lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, a| now.saturating_sub(a.created_ms) < ATTEMPT_TTL_MS);
        while map.len() >= MAX_PENDING {
            let oldest = map.iter().min_by_key(|(_, a)| a.created_ms).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => map.remove(&k),
                None => break,
            };
        }
        map.insert(Self::state_key(state), attempt);
    }

    /// Consome a tentativa do `state` (uso único). Vencida ou desconhecida: `None`.
    fn take(&self, state: &str) -> Option<Attempt> {
        let now = self.now_ms();
        let mut map = self.attempts.lock().unwrap_or_else(PoisonError::into_inner);
        let attempt = map.remove(&Self::state_key(state))?;
        (now.saturating_sub(attempt.created_ms) < ATTEMPT_TTL_MS).then_some(attempt)
    }

    /// A tentativa pendente e válida do `state`, sem consumir (conferência de quem pode usá-la).
    fn peek(&self, state: &str) -> Option<Attempt> {
        let now = self.now_ms();
        let map = self.attempts.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(&Self::state_key(state)).filter(|a| now.saturating_sub(a.created_ms) < ATTEMPT_TTL_MS).cloned()
    }

    /// Há uma tentativa pendente e válida com este `state`? Não consome (camada de dispositivos).
    pub fn callback_pending(&self, state: &str) -> bool {
        let now = self.now_ms();
        let map = self.attempts.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(&Self::state_key(state)).is_some_and(|a| now.saturating_sub(a.created_ms) < ATTEMPT_TTL_MS)
    }

    async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, Bytes), ()> {
        let resp = self
            .http
            .post(url)
            .header(header::ACCEPT, "application/json")
            .form(form)
            .send()
            .await
            .map_err(|_| ())?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(|_| ())?;
        Ok((status, body))
    }

    async fn token_call(&self, form: &[(&str, &str)]) -> TokenCall {
        let Ok((status, body)) = self.post_form(&self.endpoints.token, form).await else {
            return TokenCall::Unavailable;
        };
        if (200..300).contains(&status) {
            return match serde_json::from_slice::<TokenResponse>(&body) {
                Ok(r) => TokenCall::Ok(r),
                Err(_) => TokenCall::Unavailable,
            };
        }
        match oauth_error_code(&body) {
            Some(code) => TokenCall::Rejected(Some(code)),
            None if (400..500).contains(&status) => TokenCall::Rejected(None),
            None => TokenCall::Unavailable,
        }
    }

    /// Chaves do JWKS: do cache válido ou buscadas de novo (`force`, ou cache vencido).
    async fn jwks_keys(&self, force: bool) -> Option<Vec<jwt::Jwk>> {
        let mut cache = self.jwks.lock().await;
        let now = self.now_ms();
        if !force {
            if let Some(c) = cache.as_ref() {
                if now.saturating_sub(c.fetched_ms) < JWKS_TTL_MS {
                    return Some(c.keys.clone());
                }
            }
        }
        let fetched = async {
            let resp = self.http.get(&self.endpoints.jwks).send().await.ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let body = resp.bytes().await.ok()?;
            Some(jwt::parse_jwks(&body))
        }
        .await;
        match fetched {
            Some(keys) if !keys.is_empty() => {
                *cache = Some(JwksCache { fetched_ms: now, keys: keys.clone() });
                Some(keys)
            }
            // Falhou: usa o cache que houver, mesmo vencido.
            _ => cache.as_ref().map(|c| c.keys.clone()),
        }
    }

    /// Valida o ID token: assinatura RS256 pelo JWKS (nova busca quando o `kid` é desconhecido),
    /// `iss`, `aud` = `client_id`, `exp` e, no login, o `nonce`.
    pub async fn validate_id_token(
        &self,
        token: &str,
        client_id: &str,
        nonce: Option<&str>,
    ) -> Result<jwt::Claims, jwt::IdTokenError> {
        let parsed = jwt::parse(token)?;
        let mut keys = self.jwks_keys(false).await.unwrap_or_default();
        if jwt::find_key(&keys, parsed.kid.as_deref()).is_none() {
            keys = self.jwks_keys(true).await.unwrap_or_default();
        }
        let key = jwt::find_key(&keys, parsed.kid.as_deref()).ok_or(jwt::IdTokenError::UnknownKey)?;
        jwt::verify_signature(&parsed, key)?;
        jwt::check_claims(&parsed, ISSUER, client_id, nonce, self.now_secs())
    }

    /// URL de autorização da tentativa.
    fn authorize_url(
        &self,
        state: &str,
        nonce: &str,
        challenge: &str,
        redirect_uri: &str,
        host_id: &str,
        reauth: Option<&Credentials>,
    ) -> Option<String> {
        let mut url = reqwest::Url::parse(&self.endpoints.authorize).ok()?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("response_type", "code");
            match reauth {
                Some(creds) => {
                    q.append_pair("client_id", &creds.client_id);
                }
                None => {
                    q.append_pair("client_id", DYNAMIC_CLIENT_ID);
                }
            }
            q.append_pair("redirect_uri", redirect_uri);
            q.append_pair("scope", SCOPES);
            q.append_pair("state", state);
            q.append_pair("nonce", nonce);
            q.append_pair("code_challenge", challenge);
            q.append_pair("code_challenge_method", "S256");
            q.append_pair("resource", RESOURCE);
            q.append_pair("ext_agent_host_id", host_id);
            match reauth {
                Some(creds) => {
                    if !creds.id_token.is_empty() {
                        q.append_pair("id_token_hint", &creds.id_token);
                    }
                    if !creds.email.is_empty() {
                        q.append_pair("login_hint", &creds.email);
                    }
                }
                None => {
                    q.append_pair("agent_name_hint", AGENT_NAME_HINT);
                }
            }
        }
        Some(url.into())
    }

    /// Estado da conexão pelo plano, lido das credenciais (sem segredos).
    pub fn status(&self, dir: Option<&Path>, connection_id: &str) -> PlanStatus {
        let Some(dir) = dir else {
            return PlanStatus::Disconnected;
        };
        match store::load_credentials(dir, connection_id) {
            Ok(None) => PlanStatus::Disconnected,
            Ok(Some(c)) if c.needs_login || c.refresh_token.is_empty() => PlanStatus::NeedsLogin { email: c.email },
            Ok(Some(c)) => PlanStatus::Connected { email: c.email },
            Err(_) => PlanStatus::NeedsLogin { email: String::new() },
        }
    }

    /// Marca a conexão como "precisa entrar de novo": apaga os tokens de acesso e de renovação e
    /// mantém o `client_id`, o `id_token` e o e-mail para a reautorização.
    fn mark_needs_login(&self, dir: &Path, connection_id: &str, mut creds: Credentials) -> TokenError {
        creds.needs_login = true;
        creds.access_token.clear();
        creds.refresh_token.clear();
        creds.saved_at = self.now_secs();
        if store::save_credentials(dir, connection_id, &creds).is_err() {
            terminal_notice("chatgpt.log.save_failed", &[("id", connection_id)]);
            return TokenError::Store;
        }
        terminal_notice("chatgpt.log.needs_login", &[("id", connection_id)]);
        TokenError::NeedsLogin
    }

    /// Access token utilizável da conexão: renova antes se faltar menos de
    /// [`REFRESH_MARGIN_SECS`]. Uma renovação por conexão de cada vez: quem esperar recebe o token
    /// já renovado. Erro terminal marca "precisa entrar de novo" e não repete.
    pub async fn access_token(&self, dir: &Path, connection_id: &str) -> Result<String, TokenError> {
        let lock = self.lock_for(connection_id);
        let _guard = lock.lock().await;
        let creds = store::load_credentials(dir, connection_id)
            .map_err(|_| TokenError::NeedsLogin)?
            .ok_or(TokenError::NotConnected)?;
        if creds.needs_login || creds.refresh_token.is_empty() {
            return Err(TokenError::NeedsLogin);
        }
        if !creds.scopes.iter().any(|s| s == DIRECT_SCOPE) {
            return Err(self.mark_needs_login(dir, connection_id, creds));
        }
        let now = self.now_secs();
        if !creds.access_token.is_empty() && creds.expires_at > now.saturating_add(REFRESH_MARGIN_SECS) {
            return Ok(creds.access_token);
        }
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", creds.refresh_token.as_str()),
            ("client_id", creds.client_id.as_str()),
            ("resource", RESOURCE),
        ];
        let reply = match self.token_call(&form).await {
            TokenCall::Ok(r) => r,
            TokenCall::Rejected(Some(code)) if is_terminal_refresh_error(&code) => {
                return Err(self.mark_needs_login(dir, connection_id, creds));
            }
            TokenCall::Rejected(_) | TokenCall::Unavailable => return Err(TokenError::Unavailable),
        };
        let Some(access) = reply.access_token.filter(|t| !t.is_empty()) else {
            return Err(TokenError::Unavailable);
        };
        let mut next = creds.clone();
        next.access_token = access.clone();
        // Rotativo: grava o novo; sem novo na resposta, segue o atual.
        if let Some(refresh) = reply.refresh_token.filter(|t| !t.is_empty()) {
            next.refresh_token = refresh;
        }
        // ID token novo só substitui o salvo se for válido (sem `nonce` na renovação). Outra conta
        // (`sub` diferente do salvo) é erro terminal: precisa entrar de novo.
        if let Some(id_token) = reply.id_token.filter(|t| !t.is_empty()) {
            if let Ok(claims) = self.validate_id_token(&id_token, &creds.client_id, None).await {
                if !ct_eq(claims.sub.as_bytes(), creds.sub.as_bytes()) {
                    return Err(self.mark_needs_login(dir, connection_id, creds));
                }
                next.id_token = id_token;
            }
        }
        if reply.scope.is_some() {
            next.scopes = scopes_of(reply.scope.as_deref());
        }
        next.expires_at = now.saturating_add(reply.expires_in.unwrap_or(DEFAULT_EXPIRES_IN));
        next.saved_at = now;
        if store::save_credentials(dir, connection_id, &next).is_err() {
            terminal_notice("chatgpt.log.save_failed", &[("id", connection_id)]);
            return Err(TokenError::Store);
        }
        if !next.scopes.iter().any(|s| s == DIRECT_SCOPE) {
            return Err(self.mark_needs_login(dir, connection_id, next));
        }
        Ok(access)
    }

    /// O provedor recusou o access token atual (spec 116 T02): apaga-o para a próxima chamada a
    /// [`Self::access_token`] renovar. O refresh token fica.
    pub async fn expire_access_token(&self, dir: &Path, connection_id: &str) {
        let lock = self.lock_for(connection_id);
        let _guard = lock.lock().await;
        let Ok(Some(mut creds)) = store::load_credentials(dir, connection_id) else {
            return;
        };
        if creds.access_token.is_empty() {
            return;
        }
        creds.access_token.clear();
        creds.expires_at = 0;
        if store::save_credentials(dir, connection_id, &creds).is_err() {
            terminal_notice("chatgpt.log.save_failed", &[("id", connection_id)]);
        }
    }

    /// A conta foi recusada de vez na inferência (spec 116 T02: usuário inválido ou 401 depois da
    /// renovação): a conexão passa a "precisa entrar de novo".
    pub async fn require_login(&self, dir: &Path, connection_id: &str) {
        let lock = self.lock_for(connection_id);
        let _guard = lock.lock().await;
        if let Ok(Some(creds)) = store::load_credentials(dir, connection_id) {
            if !creds.needs_login {
                self.mark_needs_login(dir, connection_id, creds);
            }
        }
    }

    /// Revoga o refresh token no servidor. `true` se o servidor confirmou.
    async fn revoke(&self, creds: &Credentials) -> bool {
        if creds.refresh_token.is_empty() {
            return true;
        }
        let form = [
            ("token", creds.refresh_token.as_str()),
            ("token_type_hint", "refresh_token"),
            ("client_id", creds.client_id.as_str()),
        ];
        matches!(self.post_form(&self.endpoints.revoke, &form).await, Ok((status, _)) if (200..300).contains(&status))
    }
}

// ---------------------------------------------------------------------------------------------
// Rotas.

/// Início e sair: só loopback ou dispositivo autorizado (fora do modo rede, como hoje).
fn require_local_or_device(state: &AppState, access: &Access) -> Result<(), ApiError> {
    if !state.lan_mode() || access.loopback || access.device.is_some() {
        Ok(())
    } else {
        Err(ApiError::coded(StatusCode::UNAUTHORIZED, super::devices::CODE_DEVICE_REQUIRED, Map::new()))
    }
}

fn store_error() -> ApiError {
    ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, ERR_STORE, Map::new())
}

fn not_found() -> ApiError {
    ApiError::coded(StatusCode::NOT_FOUND, "connection_not_found", Map::new())
}

/// A conexão existe e é pelo plano ChatGPT.
async fn plan_connection(state: &AppState, id: &str) -> Result<Connection, ApiError> {
    super::config_api::validate_connection_id(id).map_err(|_| not_found())?;
    let cfg = state.config().read().await;
    let conn = cfg.connections.iter().find(|c| c.id == id).cloned().ok_or_else(not_found)?;
    if !conn.is_chatgpt_plan() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, ERR_NOT_PLAN, Map::new()));
    }
    Ok(conn)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct StartInput {
    #[serde(default)]
    name: Option<String>,
    /// Conexão existente a reconectar.
    #[serde(default)]
    connection_id: Option<String>,
}

/// `POST /api/connections/chatgpt/start`: devolve `{url, local}`. `local` diz se a chamada veio
/// deste computador (loopback ou o dispositivo autorizado pelo link do terminal); num aparelho da
/// rede, a interface explica que o login é feito no computador onde o Nants roda (o retorno é
/// loopback).
pub async fn post_start(
    State(state): State<AppState>,
    Extension(access): Extension<Access>,
    body: Bytes,
) -> Result<Response, ApiError> {
    require_local_or_device(&state, &access)?;
    let input: StartInput = if body.iter().all(u8::is_ascii_whitespace) {
        StartInput::default()
    } else {
        super::config_api::parse_body(&body)?
    };
    let name = input.name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if let Some(name) = &name {
        super::config_api::validate_text_field("name", name)?;
    }
    let dir = state
        .data_dir()
        .map(Path::to_path_buf)
        .ok_or_else(|| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "data_dir_unavailable", Map::new()))?;
    let auth = state.chatgpt();
    let reauth = match &input.connection_id {
        Some(id) => {
            plan_connection(&state, id).await?;
            store::load_credentials(&dir, id).ok().flatten().filter(|c| valid_client_id(&c.client_id))
        }
        None => None,
    };
    let host_id = auth.host_id(&dir).map_err(|_| {
        terminal_notice("chatgpt.log.host_id_failed", &[]);
        store_error()
    })?;
    let (Some(st), Some(nonce), Some(verifier)) = (random_token(), random_token(), random_token()) else {
        return Err(store_error());
    };
    let redirect_uri = format!("http://127.0.0.1:{}{CALLBACK_PATH}", state.bound_port());
    let url = auth
        .authorize_url(&st, &nonce, &pkce_challenge(&verifier), &redirect_uri, &host_id, reauth.as_ref())
        .ok_or_else(store_error)?;
    // Deste computador: loopback ou o dispositivo do link de entrada do terminal (spec 117). No
    // WSL em NAT, o navegador do Windows chega pelo `portproxy` com o IP do Windows, e o retorno
    // em 127.0.0.1 funciona nele. Fora do loopback, a tentativa fica ligada ao dispositivo.
    let this_computer = access.device.as_deref().is_some_and(|id| state.devices().is_this_computer(id));
    let local = !state.lan_mode() || access.loopback || this_computer;
    let bound = state.lan_mode() && !access.loopback;
    auth.remember(
        &st,
        Attempt {
            verifier,
            nonce,
            created_ms: auth.now_ms(),
            redirect_uri,
            connection_id: input.connection_id,
            name,
            saved_client_id: reauth.map(|c| c.client_id),
            initiator: if bound { access.device.clone() } else { None },
        },
    );
    Ok(Json(json!({ "url": url, "local": local })).into_response())
}

#[derive(Deserialize, Default)]
pub struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
}

/// Valor do `state` na query do retorno (para a camada de dispositivos).
pub fn callback_state(uri: &axum::http::Uri) -> Option<String> {
    Query::<CallbackQuery>::try_from_uri(uri).ok().and_then(|Query(q)| q.state)
}

fn redirect(location: &str) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::SEE_OTHER;
    if let Ok(v) = HeaderValue::from_str(location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

fn fail(reason: LoginFailure) -> Response {
    terminal_notice("chatgpt.log.login_failed", &[("reason", reason.as_str())]);
    redirect(&format!("/?chatgpt=error&reason={}", reason.as_str()))
}

/// Nome padrão da conexão: "ChatGPT (e-mail)", no limite dos campos de texto.
fn default_name(email: &str) -> String {
    if email.is_empty() {
        return "ChatGPT".to_string();
    }
    let room = super::config_api::MAX_TEXT_CHARS.saturating_sub("ChatGPT ()".chars().count());
    let email: String = email.chars().filter(|c| !c.is_control()).take(room).collect();
    format!("ChatGPT ({email})")
}

/// Id novo de conexão (`chatgpt-<8 hex>`), diferente dos existentes.
fn new_connection_id(existing: &[Connection]) -> Option<String> {
    for _ in 0..8 {
        let mut b = [0u8; 4];
        getrandom::getrandom(&mut b).ok()?;
        let id = format!("chatgpt-{}", hex(&b));
        if !existing.iter().any(|c| c.id == id) {
            return Some(id);
        }
    }
    None
}

/// Escapa texto para HTML (conteúdo e valor de atributo).
fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Endereço relativo da conclusão com os parâmetros do retorno (`code`, `state`, `error` e
/// `client_id`), codificados de novo.
fn finish_location(q: &CallbackQuery) -> Option<String> {
    let mut url = reqwest::Url::parse(&format!("http://127.0.0.1{FINISH_PATH}")).ok()?;
    {
        let mut pairs = url.query_pairs_mut();
        let fields = [("code", &q.code), ("state", &q.state), ("error", &q.error), ("client_id", &q.client_id)];
        for (name, value) in fields {
            if let Some(value) = value {
                pairs.append_pair(name, value);
            }
        }
    }
    Some(format!("{}?{}", url.path(), url.query().unwrap_or("")))
}

/// Página de passagem do retorno (spec 118): navega na hora para a conclusão, da própria origem.
fn passage_page(q: &CallbackQuery, lang: crate::i18n::Lang) -> Response {
    let Some(target) = finish_location(q) else {
        return fail(LoginFailure::StateInvalid);
    };
    let target = html_escape(&target);
    let html = format!(
        "<!doctype html>\n<html lang=\"{lang}\"><head><meta charset=\"utf-8\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta http-equiv=\"refresh\" content=\"0; url={target}\">\
         <title>{title}</title></head>\
         <body><p>{body}</p><p><a href=\"{target}\" rel=\"noreferrer\">{cont}</a></p></body></html>\n",
        lang = lang.tag(),
        title = html_escape(&crate::i18n::text(lang, "chatgpt.passage.title")),
        body = html_escape(&crate::i18n::text(lang, "chatgpt.passage.body")),
        cont = html_escape(&crate::i18n::text(lang, "chatgpt.passage.continue")),
    );
    let mut resp = Response::new(Body::from(html));
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(PASSAGE_CSP));
    resp
}

/// `GET /auth/callback`: página de passagem (spec 118). Não exige cookie, não consome o `state` e
/// não troca o código: a volta vem de outro site e pode chegar sem o cookie do dispositivo. Com um
/// `state` pendente, devolve a página que segue para [`FINISH_PATH`] a partir da própria origem;
/// sem ele, volta para a interface com o erro.
pub async fn get_callback(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    query: Result<Query<CallbackQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let q = query.map(|Query(q)| q).unwrap_or_default();
    if !q.state.as_deref().is_some_and(|st| state.chatgpt().callback_pending(st)) {
        return fail(LoginFailure::StateInvalid);
    }
    passage_page(&q, super::devices::request_lang(&headers))
}

/// `GET /auth/finish`: termina o login e volta para a interface com o resultado. Só o dispositivo
/// que iniciou (cookie) ou o loopback; fora do modo rede, como antes.
pub async fn get_finish(
    State(state): State<AppState>,
    Extension(access): Extension<Access>,
    query: Result<Query<CallbackQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let q = query.map(|Query(q)| q).unwrap_or_default();
    let auth = state.chatgpt();
    let Some(st) = q.state.as_deref() else {
        return fail(LoginFailure::StateInvalid);
    };
    // Ligada ao dispositivo que iniciou; sem dispositivo, só deste computador. A conferência é
    // feita antes de consumir: quem não pode usar o `state` recebe a recusa e não cancela o login
    // de quem iniciou.
    let allowed = auth.peek(st).is_some_and(|attempt| match &attempt.initiator {
        Some(dev) => access.loopback || access.device.as_deref() == Some(dev.as_str()),
        None => !state.lan_mode() || access.loopback,
    });
    if !allowed {
        return fail(LoginFailure::StateInvalid);
    }
    let Some(attempt) = auth.take(st) else {
        return fail(LoginFailure::StateInvalid);
    };
    if let Some(err) = q.error.as_deref() {
        return fail(if err == "access_denied" { LoginFailure::AccessDenied } else { LoginFailure::AuthorizeFailed });
    }
    let Some(code) = q.code.filter(|c| !c.is_empty()) else {
        return fail(LoginFailure::CodeMissing);
    };
    let returned = q.client_id.filter(|c| !c.is_empty());
    if returned.as_deref().is_some_and(|c| !valid_client_id(c)) {
        return fail(LoginFailure::ClientIdMissing);
    }
    let client_id = match (returned, attempt.saved_client_id.clone()) {
        (Some(got), Some(saved)) if got != saved => return fail(LoginFailure::ClientIdMismatch),
        (Some(got), _) => got,
        (None, Some(saved)) => saved,
        (None, None) => return fail(LoginFailure::ClientIdMissing),
    };
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", attempt.redirect_uri.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", attempt.verifier.as_str()),
        ("resource", RESOURCE),
    ];
    let reply = match auth.token_call(&form).await {
        TokenCall::Ok(r) => r,
        TokenCall::Rejected(_) | TokenCall::Unavailable => return fail(LoginFailure::TokenExchangeFailed),
    };
    let (Some(access_token), Some(refresh_token), Some(id_token)) = (
        reply.access_token.filter(|t| !t.is_empty()),
        reply.refresh_token.filter(|t| !t.is_empty()),
        reply.id_token.filter(|t| !t.is_empty()),
    ) else {
        return fail(LoginFailure::TokenResponseInvalid);
    };
    let claims = match auth.validate_id_token(&id_token, &client_id, Some(&attempt.nonce)).await {
        Ok(c) => c,
        Err(e) => {
            terminal_notice("chatgpt.log.id_token_rejected", &[("reason", e.code())]);
            return fail(LoginFailure::IdTokenInvalid);
        }
    };
    let scopes = scopes_of(reply.scope.as_deref());
    if !scopes.iter().any(|s| s == DIRECT_SCOPE) {
        return fail(LoginFailure::ScopeMissing);
    }
    let Some(dir) = state.data_dir().map(Path::to_path_buf) else {
        return fail(LoginFailure::StoreFailed);
    };
    let now = auth.now_secs();
    let creds = Credentials {
        client_id,
        access_token,
        refresh_token,
        id_token,
        expires_at: now.saturating_add(reply.expires_in.unwrap_or(DEFAULT_EXPIRES_IN)),
        scopes,
        email: claims.email.clone(),
        sub: claims.sub.clone(),
        saved_at: now,
        needs_login: false,
    };
    match save_login(&state, &dir, &attempt, &creds).await {
        Ok(id) => {
            terminal_notice("chatgpt.log.connected", &[("id", &id)]);
            redirect(&format!("/?chatgpt=connected&connection={id}"))
        }
        Err(()) => fail(LoginFailure::StoreFailed),
    }
}

/// Grava as credenciais e cria ou atualiza a conexão. Devolve o id da conexão.
async fn save_login(state: &AppState, dir: &Path, attempt: &Attempt, creds: &Credentials) -> Result<String, ()> {
    let id = match &attempt.connection_id {
        Some(id) => id.clone(),
        None => new_connection_id(&state.config().read().await.connections).ok_or(())?,
    };
    // Primeiro a vez da conexão (uma renovação em andamento termina antes), depois a configuração:
    // a configuração não fica presa enquanto a renovação espera a rede.
    let lock = state.chatgpt().lock_for(&id);
    let _serial = lock.lock().await;
    let mut guard = state.config().write().await;
    let mut next = guard.clone();
    match next.connections.iter_mut().find(|c| c.id == id) {
        // Id novo que outra conexão tomou nesse meio tempo.
        Some(_) if attempt.connection_id.is_none() => return Err(()),
        Some(conn) => {
            // A conexão por chave com o mesmo id não vira plano por aqui.
            if !conn.is_chatgpt_plan() {
                return Err(());
            }
            if let Some(name) = &attempt.name {
                conn.name = name.clone();
            }
        }
        None => {
            let name = attempt.name.clone().unwrap_or_else(|| default_name(&creds.email));
            next.connections.push(Connection::chatgpt_plan(id.clone(), name));
        }
    }
    let previous = store::load_credentials(dir, &id).ok().flatten();
    if store::save_credentials(dir, &id, creds).is_err() {
        terminal_notice("chatgpt.log.save_failed", &[("id", &id)]);
        return Err(());
    }
    if super::config_api::persist(state, &next).await.is_err() {
        // Volta as credenciais ao que eram (a conexão não foi gravada).
        let _ = match &previous {
            Some(p) => store::save_credentials(dir, &id, p),
            None => store::delete_credentials(dir, &id),
        };
        return Err(());
    }
    *guard = next;
    drop(guard);
    state.models_cache().invalidate_connection(&id);
    Ok(id)
}

/// `GET /api/connections/:id/chatgpt`: estado da conexão pelo plano (sem segredos).
pub async fn get_status(State(state): State<AppState>, UrlPath(id): UrlPath<String>) -> Result<Response, ApiError> {
    plan_connection(&state, &id).await?;
    let status = state.chatgpt().status(state.data_dir(), &id);
    Ok(Json(status).into_response())
}

/// `POST /api/connections/:id/chatgpt/logout`: revoga o refresh token (falha vira aviso),
/// apaga as credenciais locais e a conexão fica desconectada. Responde `{state, revoked}`.
pub async fn post_logout(
    State(state): State<AppState>,
    Extension(access): Extension<Access>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, ApiError> {
    require_local_or_device(&state, &access)?;
    plan_connection(&state, &id).await?;
    let dir = state
        .data_dir()
        .map(Path::to_path_buf)
        .ok_or_else(|| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "data_dir_unavailable", Map::new()))?;
    let auth = state.chatgpt();
    let lock = auth.lock_for(&id);
    let _serial = lock.lock().await;
    let creds = store::load_credentials(&dir, &id).ok().flatten();
    let revoked = match &creds {
        Some(c) => auth.revoke(c).await,
        None => true,
    };
    if !revoked {
        terminal_notice("chatgpt.log.revoke_failed", &[("id", &id)]);
    }
    store::delete_credentials(&dir, &id).map_err(|_| store_error())?;
    state.models_cache().invalidate_connection(&id);
    Ok(Json(json!({ "state": "disconnected", "revoked": revoked })).into_response())
}
