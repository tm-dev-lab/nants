//! Registro do Nanty (spec 138 T01): `journal.jsonl` na pasta do Nanty, com rotação.
//!
//! Uma linha por evento: o sinal, a decisão do filtro, o turno do modelo com o custo, a ação
//! sugerida, quem decidiu (automático, Telegram ou web), o resultado e as ações previstas. Só
//! códigos, ids e números estruturados: nada de texto livre de projeto, log ou segredo (C-10).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::jsonl::{JsonlError, JsonlLog};
use super::store::DecidedBy;

/// Tamanho do arquivo atual antes de girar (1 MiB) e gerações antigas guardadas.
const JOURNAL_MAX_BYTES: u64 = 1024 * 1024;
const JOURNAL_KEEP: usize = 3;

/// Tipo da linha do registro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalKind {
    /// Um observador viu algo (T02).
    Signal,
    /// O filtro decidiu: avisar, esperar, segurar a repetição, calar (T02).
    Filter,
    /// Um turno do modelo, com tokens e custo (T05).
    ModelTurn,
    /// Uma ação do catálogo sugerida (T03).
    Suggestion,
    /// Alguém decidiu uma ação (T03).
    Decision,
    /// O resultado de uma ação (T03).
    Result,
    /// Uma ação prevista (T03).
    Planned,
    /// O canal do Telegram (T06): pareamento, estranho ignorado, saída de grupo, token recusado e
    /// mensagem acumulada. Só códigos e números: nunca o token, o código de pareamento ou o texto.
    Telegram,
    /// A conversa (T08): o que a pessoa mandou (comando, texto livre) e o código da resposta, com o
    /// canal e os cartões pedidos. Nunca o texto da pessoa nem o do modelo.
    Conversation,
    /// A voz (T09): a fala (cada passo da ordem, com o motivo em código quando cai para o próximo)
    /// e a transcrição da nota de voz, com tokens e custo. Nunca o texto nem o áudio.
    Voice,
}

/// Uma linha do registro.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub at_ms: u64,
    pub kind: JournalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Código estável do que aconteceu (sinal, decisão do filtro, resultado).
    pub code: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<DecidedBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

impl JournalEntry {
    /// Linha mínima: instante, tipo e código.
    pub fn new(at_ms: u64, kind: JournalKind, code: impl Into<String>) -> Self {
        Self {
            at_ms,
            kind,
            project_id: None,
            code: code.into(),
            params: Map::new(),
            action: None,
            decided_by: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
        }
    }
}

/// O registro numa pasta do Nanty.
#[derive(Debug, Clone)]
pub struct Journal {
    log: JsonlLog,
}

impl Journal {
    pub fn new(dir: PathBuf) -> Self {
        Self { log: JsonlLog::new(dir, "journal", JOURNAL_MAX_BYTES, JOURNAL_KEEP) }
    }

    #[cfg(test)]
    fn with_limits(dir: PathBuf, max_bytes: u64, keep: usize) -> Self {
        Self { log: JsonlLog::new(dir, "journal", max_bytes, keep) }
    }

    pub fn append(&self, entry: &JournalEntry) -> Result<(), JsonlError> {
        self.log.append(entry)
    }

    /// As últimas `limit` linhas, da mais velha para a mais nova.
    pub fn tail(&self, limit: usize) -> Vec<JournalEntry> {
        self.log.tail(limit)
    }

    /// Mantém só as linhas sem projeto ou de projeto conhecido (varredura completa).
    pub fn retain_known(&self, known: &impl Fn(&Option<String>) -> bool) -> Result<(), JsonlError> {
        self.log.retain::<JournalEntry>(|e| known(&e.project_id))
    }

    /// Tira as linhas de um projeto removido.
    pub fn forget_project(&self, project_id: &str) -> Result<(), JsonlError> {
        self.log.retain::<JournalEntry>(|e| e.project_id.as_deref() != Some(project_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nants-nanty-journal-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn every_kind_is_one_line_and_rotation_keeps_the_newest() {
        let d = dir("kinds");
        let journal = Journal::with_limits(d.clone(), 600, 2);
        let kinds = [
            JournalKind::Signal,
            JournalKind::Filter,
            JournalKind::ModelTurn,
            JournalKind::Suggestion,
            JournalKind::Decision,
            JournalKind::Result,
            JournalKind::Planned,
            JournalKind::Conversation,
        ];
        for (n, kind) in kinds.into_iter().cycle().take(40).enumerate() {
            let mut e = JournalEntry::new(n as u64, kind, "nanty.code");
            e.project_id = Some(if n % 2 == 0 { "p1" } else { "p2" }.into());
            if kind == JournalKind::ModelTurn {
                (e.cost_usd, e.input_tokens, e.output_tokens) = (Some(0.001), Some(100), Some(20));
            }
            if kind == JournalKind::Decision {
                (e.action, e.decided_by) = (Some("retry_provider".into()), Some(DecidedBy::Telegram));
            }
            journal.append(&e).expect("append");
        }
        let text = std::fs::read_to_string(d.join("journal.jsonl")).expect("atual");
        assert!(text.lines().all(|l| serde_json::from_str::<JournalEntry>(l).is_ok()));
        assert!(d.join("journal.2.jsonl").is_file() && !d.join("journal.3.jsonl").exists());
        let tail = journal.tail(1000);
        assert_eq!(tail.last().map(|e| e.at_ms), Some(39), "a mais nova fica");
        assert!(tail.first().is_some_and(|e| e.at_ms > 0), "as mais velhas saíram");
        let decision = tail.iter().find(|e| e.kind == JournalKind::Decision).expect("decisão");
        assert_eq!(decision.decided_by, Some(DecidedBy::Telegram));
        journal.forget_project("p1").expect("forget");
        assert!(journal.tail(1000).iter().all(|e| e.project_id.as_deref() == Some("p2")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
