//! Registro persistente das falhas do provedor (spec 133 T01, C-12).
//!
//! Cada falha final de um papel vira uma linha JSON em `.sdd/specs/<spec>/provider-errors.jsonl`;
//! fora de uma spec (a conversa com o Arquiteto), em `.sdd/specs/.provider-errors.jsonl`. Os dois
//! ficam fora do Git do projeto (`.git/info/exclude`) e têm teto de tamanho: passado o teto, novas
//! linhas são descartadas.
//!
//! A linha leva sempre o que prova a origem sem conteúdo do pedido (papel, conexão, modelo,
//! tarefa, tentativa, status, `error.type`, `error.code`, `error.param`, `request-id`, a categoria
//! do Nants e as novas tentativas) e a mensagem do provedor, redigida pelos segredos da
//! configuração e com teto. A mensagem nunca vai para a tela nem para os `params`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::events::SecretSet;
use super::projects::{format_utc, scrub_text};
use super::safe_fs;
use super::spec_result::validate_spec_id;
use crate::agents::llm_client::{ConnectionRef, FailureEvidence, ProviderFailure};

/// Teto de cada arquivo (1 MiB).
pub const MAX_PROVIDER_ERRORS_BYTES: u64 = 1024 * 1024;
const SPECS_DIR: &str = ".sdd/specs";
const FILE: &str = "provider-errors.jsonl";
/// Arquivo das falhas fora de spec (a conversa com o Arquiteto).
const NO_SPEC_FILE: &str = ".sdd/specs/.provider-errors.jsonl";
/// Linhas do `.git/info/exclude` (por spec e o de reserva).
pub const PROVIDER_ERRORS_EXCLUDE_LINE: &str = ".sdd/specs/**/provider-errors.jsonl";
pub const PROVIDER_ERRORS_CHAT_EXCLUDE_LINE: &str = ".sdd/specs/.provider-errors.jsonl";
/// Teto, em bytes, do nome da conexão nos `params` e no registro.
const NAME_MAX_BYTES: usize = 120;

/// Uma linha de `provider-errors.jsonl`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderErrorRecord {
    pub at: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Etapa fora do laço da tarefa (`chat`, `plan_review`); ausente na execução da spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_kind: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Causa fixa do Nants (`provider_http_400`, `provider_spend_limit`...).
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Novas tentativas feitas antes de desistir.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retries: u32,
    /// Mensagem do provedor, redigida e com teto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Corta `text` em `max` bytes sem quebrar caractere.
fn cut(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Nome da conexão como vai ao registro e à tela: redigido, numa linha, com teto.
fn connection_name(secrets: &SecretSet, conn: &ConnectionRef) -> String {
    let clean = scrub_text(secrets, &conn.name).split_whitespace().collect::<Vec<_>>().join(" ");
    cut(&clean, NAME_MAX_BYTES)
}

/// Linha do registro de uma falha final.
pub fn record_of(
    project: &str,
    spec: Option<&str>,
    stage: Option<&str>,
    failure: &ProviderFailure,
    ev: &FailureEvidence,
    secrets: &SecretSet,
) -> ProviderErrorRecord {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let http = ev.http.as_ref();
    ProviderErrorRecord {
        at: format_utc(secs),
        project: project.to_string(),
        spec: spec.filter(|s| validate_spec_id(s).is_ok()).map(str::to_string),
        stage: stage.map(str::to_string),
        role: ev.role.clone(),
        connection_id: ev.connection.as_ref().map(|c| scrub_text(secrets, &c.id)),
        connection_name: ev.connection.as_ref().map(|c| connection_name(secrets, c)),
        connection_kind: ev.connection.as_ref().map(|c| c.kind.clone()),
        model: scrub_text(secrets, &ev.model),
        task: ev.task.clone(),
        attempt: ev.attempt,
        provider: failure.to_string(),
        category: ev.category.map(|c| c.as_str().to_string()),
        status: http.map(|h| h.status),
        error_type: http.and_then(|h| h.error_type.clone()),
        error_code: http.and_then(|h| h.error_code.clone()),
        error_param: http.and_then(|h| h.error_param.clone()),
        request_id: http.and_then(|h| h.request_id.clone()),
        retries: ev.retries,
        message: http.and_then(|h| h.message.as_deref()).map(|m| scrub_text(secrets, m)),
    }
}

/// Grava a linha no arquivo da spec (ou no de reserva), garantindo antes a exclusão do Git.
/// Falhas de I/O são ignoradas: o registro nunca derruba a execução.
pub fn append(root: &Path, record: &ProviderErrorRecord) {
    let _ = super::projects::ensure_local_excludes(root);
    let rel = match &record.spec {
        Some(spec) => format!("{SPECS_DIR}/{spec}/{FILE}"),
        None => NO_SPEC_FILE.to_string(),
    };
    if let Ok(line) = serde_json::to_string(record) {
        let _ = safe_fs::append_bounded(root, &rel, &line, MAX_PROVIDER_ERRORS_BYTES);
    }
}

/// Conexão de um papel como vai à evidência: id, nome e tipo.
pub fn connection_ref(conn: &crate::config::Connection) -> ConnectionRef {
    ConnectionRef { id: conn.id.clone(), name: conn.name.clone(), kind: conn.kind.as_str().to_string() }
}

/// Papel de uma etapa fora da execução da spec, capturado no disparo (com o cliente): o modelo e
/// a conexão de quando a chamada foi montada, não os da configuração no momento da falha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageSetup {
    pub role: &'static str,
    pub model: String,
    pub connection: Option<ConnectionRef>,
}

/// O que o rastro da etapa provou: a evidência HTTP do último erro e as novas tentativas.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageTrace {
    pub http: Option<crate::agents::llm_client::HttpEvidence>,
    pub retries: u32,
}

impl StageTrace {
    /// Lido do escopo do rastro em curso (dentro de `with_trace_scope`).
    pub fn current() -> Self {
        use crate::agents::llm_client::evidence;
        Self { http: evidence::last_http_error(), retries: evidence::call_trace().map_or(0, |t| t.retries) }
    }
}

/// Falha fora da execução da spec (a conversa com o Arquiteto, a revisão do plano): monta a
/// evidência pelo papel capturado no disparo (`setup`) e pelo rastro da etapa, grava a linha
/// (na spec, quando houver; senão, no arquivo de reserva) e devolve a evidência, para os `params`
/// da mensagem (spec 133 T11).
#[allow(clippy::too_many_arguments)]
pub async fn record_stage_failure(
    secrets: &SecretSet,
    project_id: &str,
    root: &Path,
    spec: Option<&str>,
    stage: &str,
    setup: &StageSetup,
    failure: &ProviderFailure,
    trace: StageTrace,
) -> FailureEvidence {
    use crate::agents::llm_client::evidence;
    let StageTrace { http, retries } = trace;
    let http = http.filter(|h| match failure {
        ProviderFailure::Http(status, _) => h.status == *status,
        ProviderFailure::Plan(_) | ProviderFailure::Limit(_) => true,
        _ => false,
    });
    let ev = FailureEvidence {
        role: setup.role.to_string(),
        model: setup.model.clone(),
        task: None,
        attempt: None,
        category: evidence::classify(failure, http.as_ref(), false),
        retries,
        http,
        connection: setup.connection.clone(),
    };
    let record = record_of(project_id, spec, Some(stage), failure, &ev, secrets);
    let root = root.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || append(&root, &record)).await;
    ev
}

/// `params` da falha de uma etapa fora da execução (spec 133 T11): a causa fixa e o motivo neutro
/// do provedor (`provider`, `kind`, `provider_code`), somados à evidência (papel, conexão, modelo,
/// categoria, status e os códigos do provedor). Os mesmos campos do `run.provider_failed`.
pub fn stage_failure_params(secrets: &SecretSet, failure: &ProviderFailure, ev: &FailureEvidence) -> Map<String, Value> {
    let mut params = super::error::provider_failure_params(failure);
    params.extend(evidence_params(secrets, ev));
    params
}

/// Linhas gravadas de uma spec (ou as de reserva, com `None`). Linhas inválidas são ignoradas.
pub fn load(root: &Path, spec: Option<&str>) -> Vec<ProviderErrorRecord> {
    let rel = match spec {
        Some(spec) => format!("{SPECS_DIR}/{spec}/{FILE}"),
        None => NO_SPEC_FILE.to_string(),
    };
    safe_fs::read_regular(root, &rel, MAX_PROVIDER_ERRORS_BYTES)
        .map(|raw| raw.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

/// `params` da evidência para a mensagem do sistema (spec 133 T01): papel, conexão, modelo,
/// tarefa, tentativa, categoria e os códigos do provedor. Nunca a mensagem do provedor.
pub fn evidence_params(secrets: &SecretSet, ev: &FailureEvidence) -> Map<String, Value> {
    let mut out = Map::new();
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            out.insert(k.to_string(), v);
        }
    };
    put("role", Some(Value::from(ev.role.clone())));
    put("model", Some(Value::from(scrub_text(secrets, &ev.model))));
    if let Some(conn) = &ev.connection {
        put("connection_id", Some(Value::from(scrub_text(secrets, &conn.id))));
        put("connection_name", Some(Value::from(connection_name(secrets, conn))));
        put("connection_kind", Some(Value::from(conn.kind.clone())));
    }
    put("task", ev.task.clone().map(Value::from));
    put("attempt", ev.attempt.map(Value::from));
    put("category", ev.category.map(|c| Value::from(c.as_str())));
    if let Some(http) = &ev.http {
        put("status", Some(Value::from(http.status)));
        put("error_type", http.error_type.clone().map(Value::from));
        put("error_code", http.error_code.clone().map(Value::from));
        put("error_param", http.error_param.clone().map(Value::from));
        put("request_id", http.request_id.clone().map(Value::from));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::llm_client::{FailureCategory, HttpEvidence};

    fn evidence(message: &str) -> FailureEvidence {
        FailureEvidence {
            role: "developer".into(),
            model: "modelo-x".into(),
            task: Some("T03".into()),
            attempt: Some(2),
            category: Some(FailureCategory::Billing),
            retries: 1,
            http: Some(HttpEvidence {
                status: 400,
                error_type: Some("invalid_request_error".into()),
                error_code: None,
                error_param: Some("max_tokens".into()),
                request_id: Some("req_123".into()),
                message: Some(message.into()),
            }),
            connection: Some(ConnectionRef { id: "anthropic-1".into(), name: "Minha Anthropic".into(), kind: "anthropic".into() }),
        }
    }

    fn tmp_git(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("nants-provider-errors-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git/info")).expect("git");
        std::fs::create_dir_all(root.join(".sdd/specs/s1")).expect("spec");
        root
    }

    #[test]
    fn record_is_redacted_excluded_from_git_and_reloadable() {
        let root = tmp_git("rec");
        let secret = "segredo-da-config-123";
        let secrets = SecretSet::new(vec![secret.to_string()]);
        let ev = evidence(&format!("token {secret} recusado"));
        let failure = ProviderFailure::Http(400, None);
        append(&root, &record_of("p1", Some("s1"), None, &failure, &ev, &secrets));
        append(&root, &record_of("p1", None, Some("chat"), &failure, &ev, &secrets));
        let raw = std::fs::read_to_string(root.join(".sdd/specs/s1/provider-errors.jsonl")).expect("arquivo");
        assert!(!raw.contains(secret), "{raw}");
        let rows = load(&root, Some("s1"));
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!((row.role.as_str(), row.model.as_str(), row.task.as_deref(), row.attempt), ("developer", "modelo-x", Some("T03"), Some(2)));
        assert_eq!((row.status, row.error_type.as_deref(), row.error_param.as_deref()), (Some(400), Some("invalid_request_error"), Some("max_tokens")));
        assert_eq!((row.request_id.as_deref(), row.category.as_deref()), (Some("req_123"), Some("billing")));
        assert_eq!((row.connection_id.as_deref(), row.connection_kind.as_deref()), (Some("anthropic-1"), Some("anthropic")));
        assert_eq!(row.provider, "provider_http_400");
        assert_eq!(row.retries, 1);
        assert_eq!(load(&root, None).first().and_then(|r| r.stage.clone()).as_deref(), Some("chat"));
        let exclude = std::fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
        assert!(exclude.lines().any(|l| l == PROVIDER_ERRORS_EXCLUDE_LINE), "{exclude}");
        assert!(exclude.lines().any(|l| l == PROVIDER_ERRORS_CHAT_EXCLUDE_LINE), "{exclude}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn params_never_carry_the_provider_message() {
        let secrets = SecretSet::new(Vec::new());
        let params = evidence_params(&secrets, &evidence("mensagem do provedor"));
        let text = serde_json::to_string(&params).expect("json");
        assert!(!text.contains("mensagem do provedor"));
        for key in ["role", "model", "connection_id", "connection_name", "connection_kind", "task", "attempt", "category", "status", "error_type", "error_param", "request_id"] {
            assert!(params.contains_key(key), "falta {key}: {text}");
        }
        assert!(!params.contains_key("error_code"));
    }
}
