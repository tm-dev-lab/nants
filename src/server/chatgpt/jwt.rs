//! Validação do ID token do login pelo plano ChatGPT (spec 116, T01).
//!
//! - Só RS256 (`alg` exato no cabeçalho): `none`, HS256 e qualquer outro são recusados.
//! - Assinatura RSA PKCS#1 v1.5 com SHA-256 pelo `ring` (que já vem com o rustls), com a chave
//!   pública montada do `n` e do `e` do JWKS. Nenhuma dependência nova de criptografia.
//! - Declarações: `iss` exato, `aud` igual ao `client_id` (texto ou lista; com mais de um valor,
//!   `azp` igual ao `client_id`), `exp` com tolerância
//!   de [`EXP_LEEWAY_SECS`] e, no login, o `nonce` da tentativa. `sub` não vazio.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;

/// Tolerância do `exp` (segundos).
pub const EXP_LEEWAY_SECS: u64 = 5;
/// Teto do tamanho do token aceito (bytes).
const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// Por que o ID token foi recusado. Só o motivo vai ao log, nunca o token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdTokenError {
    Malformed,
    Algorithm,
    UnknownKey,
    Signature,
    Issuer,
    Audience,
    Expired,
    Nonce,
    Subject,
}

impl IdTokenError {
    /// Código estável do motivo (log técnico).
    pub fn code(self) -> &'static str {
        match self {
            IdTokenError::Malformed => "malformed",
            IdTokenError::Algorithm => "algorithm",
            IdTokenError::UnknownKey => "unknown_key",
            IdTokenError::Signature => "signature",
            IdTokenError::Issuer => "issuer",
            IdTokenError::Audience => "audience",
            IdTokenError::Expired => "expired",
            IdTokenError::Nonce => "nonce",
            IdTokenError::Subject => "subject",
        }
    }
}

pub fn b64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Base64url sem preenchimento (o do JWT); aceita o `=` final por tolerância.
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text.trim_end_matches('=')).ok()
}

/// Chave RSA do JWKS. `n` e `e` em big-endian, sem zeros à esquerda.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jwk {
    pub kid: Option<String>,
    pub n: Vec<u8>,
    pub e: Vec<u8>,
}

#[derive(Deserialize)]
struct JwksDoc {
    #[serde(default)]
    keys: Vec<Value>,
}

fn strip_zeros(mut bytes: Vec<u8>) -> Vec<u8> {
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    bytes.drain(..first);
    bytes
}

/// Chaves RSA de assinatura do documento JWKS (`kty = RSA`; `alg`, se houver, `RS256`; `use`, se
/// houver, `sig`). As demais são ignoradas.
pub fn parse_jwks(body: &[u8]) -> Vec<Jwk> {
    let Ok(doc) = serde_json::from_slice::<JwksDoc>(body) else {
        return Vec::new();
    };
    doc.keys
        .iter()
        .filter_map(|k| {
            let field = |name: &str| k.get(name).and_then(Value::as_str);
            if field("kty") != Some("RSA") {
                return None;
            }
            if field("alg").is_some_and(|a| a != "RS256") || field("use").is_some_and(|u| u != "sig") {
                return None;
            }
            let n = strip_zeros(b64url_decode(field("n")?)?);
            let e = strip_zeros(b64url_decode(field("e")?)?);
            (!n.is_empty() && !e.is_empty()).then(|| Jwk { kid: field("kid").map(str::to_string), n, e })
        })
        .collect()
}

/// Token separado: o `kid` do cabeçalho, a parte assinada, a assinatura e as declarações cruas.
pub struct Parsed {
    pub kid: Option<String>,
    signing_input: Vec<u8>,
    signature: Vec<u8>,
    claims: Value,
}

/// Separa o JWT e confere o cabeçalho (`alg` exatamente `RS256`).
pub fn parse(token: &str) -> Result<Parsed, IdTokenError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(IdTokenError::Malformed);
    }
    let mut parts = token.split('.');
    let (Some(h), Some(c), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(IdTokenError::Malformed);
    };
    let header: Value = b64url_decode(h)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or(IdTokenError::Malformed)?;
    if header.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(IdTokenError::Algorithm);
    }
    let claims: Value = b64url_decode(c)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(Value::is_object)
        .ok_or(IdTokenError::Malformed)?;
    let signature = b64url_decode(s).filter(|s| !s.is_empty()).ok_or(IdTokenError::Malformed)?;
    Ok(Parsed {
        kid: header.get("kid").and_then(Value::as_str).map(str::to_string),
        signing_input: format!("{h}.{c}").into_bytes(),
        signature,
        claims,
    })
}

/// A chave do token entre as do JWKS: pelo `kid`; sem `kid`, só se houver uma chave.
pub fn find_key<'a>(keys: &'a [Jwk], kid: Option<&str>) -> Option<&'a Jwk> {
    match kid {
        Some(kid) => keys.iter().find(|k| k.kid.as_deref() == Some(kid)),
        None if keys.len() == 1 => keys.first(),
        None => None,
    }
}

/// Assinatura RS256 (RSA PKCS#1 v1.5, SHA-256, chave de 2048 a 8192 bits) pelo `ring`.
pub fn verify_signature(parsed: &Parsed, key: &Jwk) -> Result<(), IdTokenError> {
    let public = ring::signature::RsaPublicKeyComponents { n: key.n.as_slice(), e: key.e.as_slice() };
    public
        .verify(&ring::signature::RSA_PKCS1_2048_8192_SHA256, &parsed.signing_input, &parsed.signature)
        .map_err(|_| IdTokenError::Signature)
}

/// Declarações conferidas do ID token.
#[derive(Clone, PartialEq, Eq)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub exp: u64,
}

impl std::fmt::Debug for Claims {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Claims(exp={})", self.exp)
    }
}

/// Confere `iss`, `aud`, `exp` (com tolerância), `nonce` (quando pedido) e `sub`.
pub fn check_claims(
    parsed: &Parsed,
    issuer: &str,
    client_id: &str,
    nonce: Option<&str>,
    now_secs: u64,
) -> Result<Claims, IdTokenError> {
    let c = &parsed.claims;
    let text = |name: &str| c.get(name).and_then(Value::as_str);
    if text("iss") != Some(issuer) {
        return Err(IdTokenError::Issuer);
    }
    // OIDC Core 3.1.3.7: com mais de uma audiência, `azp` precisa ser o `client_id`.
    let aud_ok = match c.get("aud") {
        Some(Value::String(a)) => a == client_id,
        Some(Value::Array(list)) => {
            list.iter().any(|a| a.as_str() == Some(client_id)) && (list.len() == 1 || text("azp") == Some(client_id))
        }
        _ => false,
    };
    if !aud_ok {
        return Err(IdTokenError::Audience);
    }
    let exp = c.get("exp").and_then(Value::as_u64).ok_or(IdTokenError::Expired)?;
    if exp.saturating_add(EXP_LEEWAY_SECS) <= now_secs {
        return Err(IdTokenError::Expired);
    }
    if let Some(expected) = nonce {
        let got = text("nonce").unwrap_or("");
        if !super::ct_eq(got.as_bytes(), expected.as_bytes()) {
            return Err(IdTokenError::Nonce);
        }
    }
    let sub = text("sub").filter(|s| !s.trim().is_empty()).ok_or(IdTokenError::Subject)?;
    Ok(Claims { sub: sub.to_string(), email: text("email").unwrap_or("").to_string(), exp })
}
