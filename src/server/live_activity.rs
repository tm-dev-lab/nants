//! Registro em memória da atividade ao vivo dos agentes, por projeto. Alimentado pelo mesmo
//! caminho que publica `agent_activity` (guarda exatamente o texto já redigido/limitado do
//! evento), e consultado por `GET /api/projects/:id/activity` para reidratar o cliente.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::json;

use super::error::ApiError;
use super::projects::find_project;
use super::state::AppState;
use crate::agents::activity::ActivityKind;
use crate::agents::runner::FINAL_VERIFICATION_STAGE;

/// Entradas sem atualização por mais que isto são descartadas na leitura.
pub const ACTIVITY_TTL: Duration = Duration::from_secs(120);
/// Milissegundos desde a época Unix.
pub fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}
/// Máximo de entradas por projeto.
pub const MAX_ENTRIES_PER_PROJECT: usize = 32;

#[derive(Debug, Clone)]
struct Entry {
    spec_id: Option<String>,
    activity_id: String,
    reasoning: Option<String>,
    text: Option<String>,
    tool: Option<String>,
    command: Option<String>,
    command_started_at: Option<u64>,
    /// Chamada ao modelo em curso (spec 114 T04): início (ms, época Unix) e bytes recebidos.
    model_started_at: Option<u64>,
    received_bytes: Option<u64>,
    stage: Option<String>,
    stage_code: Option<String>,
    /// `code`/`params` do último rótulo de ferramenta e do último texto (spec 102).
    tool_code: Option<Coded>,
    text_code: Option<Coded>,
    updated: Instant,
}

type Coded = (String, serde_json::Map<String, serde_json::Value>);

type Key = (String, Option<String>);

/// Evento já redigido/limitado, como publicado em `agent_activity`.
#[derive(Debug, Clone, Copy)]
pub struct Observed<'a> {
    pub agent: &'a str,
    pub task_id: Option<&'a str>,
    pub spec_id: Option<&'a str>,
    pub activity_id: &'a str,
    pub kind: ActivityKind,
    pub text: Option<&'a str>,
    /// Rótulo da etapa (relatório, arquitetura), quando houver.
    pub stage: Option<&'a str>,
    /// `code` estável da etapa (spec 102).
    pub stage_code: Option<&'a str>,
    /// `code` e `params` (já saneados) do texto do evento (spec 102).
    pub code: Option<(&'a str, &'a serde_json::Map<String, serde_json::Value>)>,
    /// Bytes recebidos da resposta do modelo em curso (heartbeat da chamada, spec 114 T04).
    pub received_bytes: Option<u64>,
}

/// Atividade ao vivo de um (agente, tarefa), como exposta pela API.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LiveActivity {
    pub agent: String,
    pub task_id: Option<String>,
    pub spec_id: Option<String>,
    pub activity_id: String,
    pub reasoning: Option<String>,
    pub text: Option<String>,
    pub tool: Option<String>,
    pub command: Option<String>,
    /// Início do comando em curso (ms desde a época Unix).
    pub command_started_at: Option<u64>,
    /// Chamada ao modelo em curso (spec 114 T04): início (ms desde a época Unix) e bytes recebidos.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_bytes: Option<u64>,
    pub stage: Option<String>,
    /// `code`/`params` neutros de idioma (spec 102): a interface traduz; o texto é a reserva.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_params: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_params: Option<serde_json::Map<String, serde_json::Value>>,
    pub age_ms: u64,
}

/// Registro compartilhado (clones apontam para o mesmo mapa).
#[derive(Debug, Clone)]
pub struct LiveActivityRegistry {
    inner: Arc<Mutex<HashMap<String, HashMap<Key, Entry>>>>,
    ttl: Duration,
}

impl Default for LiveActivityRegistry {
    fn default() -> Self {
        Self::with_ttl(ACTIVITY_TTL)
    }
}

impl LiveActivityRegistry {
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::default(),
            ttl,
        }
    }

    pub fn record(&self, project_id: &str, ev: Observed<'_>) {
        self.record_at(Instant::now(), project_id, ev);
    }

    pub fn record_at(&self, now: Instant, project_id: &str, ev: Observed<'_>) {
        let key: Key = (ev.agent.to_string(), ev.task_id.map(String::from));
        let mut all = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let map = all.entry(project_id.to_string()).or_default();
        if ev.kind == ActivityKind::End {
            if map.get(&key).is_some_and(|e| e.activity_id == ev.activity_id) {
                map.remove(&key);
            }
            if map.is_empty() {
                all.remove(project_id);
            }
            return;
        }
        let fresh = || Entry {
            spec_id: ev.spec_id.map(String::from),
            activity_id: ev.activity_id.to_string(),
            reasoning: None,
            text: None,
            tool: None,
            command: None,
            command_started_at: None,
            model_started_at: None,
            received_bytes: None,
            stage: ev.stage.map(String::from),
            stage_code: ev.stage_code.map(String::from),
            tool_code: None,
            text_code: None,
            updated: now,
        };
        let entry = map.entry(key).or_insert_with(fresh);
        if ev.kind == ActivityKind::Start || entry.activity_id != ev.activity_id {
            *entry = fresh();
        }
        entry.updated = now;
        let value = ev.text.map(String::from);
        let coded = ev.code.map(|(code, params)| (code.to_string(), params.clone()));
        match ev.kind {
            ActivityKind::Reasoning => entry.reasoning = value,
            ActivityKind::Text => {
                entry.text = value;
                entry.text_code = coded;
            }
            ActivityKind::Tool => {
                entry.tool = value;
                entry.tool_code = coded;
            }
            ActivityKind::Command => {
                entry.command = value;
                entry.command_started_at = Some(epoch_ms());
            }
            ActivityKind::Heartbeat => {}
            ActivityKind::ModelBeat => {
                if entry.model_started_at.is_some() && ev.received_bytes.is_some() {
                    entry.received_bytes = ev.received_bytes;
                }
            }
            ActivityKind::CommandEnd => {
                entry.command = None;
                entry.command_started_at = None;
            }
            ActivityKind::Model => {
                entry.model_started_at = Some(epoch_ms());
                entry.received_bytes = None;
            }
            ActivityKind::ModelEnd => {
                entry.model_started_at = None;
                entry.received_bytes = None;
            }
            ActivityKind::Start | ActivityKind::End => {}
        }
        while map.len() > MAX_ENTRIES_PER_PROJECT {
            let oldest = map.iter().min_by_key(|(_, e)| e.updated).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => map.remove(&k),
                None => break,
            };
        }
    }

    pub fn snapshot(&self, project_id: &str) -> Vec<LiveActivity> {
        self.snapshot_at(Instant::now(), project_id)
    }

    /// Entradas vivas (descarta as expiradas), da mais recente para a mais antiga.
    pub fn snapshot_at(&self, now: Instant, project_id: &str) -> Vec<LiveActivity> {
        let mut all = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(map) = all.get_mut(project_id) else {
            return Vec::new();
        };
        // A verificação final não emite deltas enquanto o Revisor trabalha: vale até o `end`.
        map.retain(|_, e| {
            e.stage.as_deref() == Some(FINAL_VERIFICATION_STAGE)
                || now.saturating_duration_since(e.updated) <= self.ttl
        });
        let mut out: Vec<LiveActivity> = map
            .iter()
            .map(|((agent, task_id), e)| LiveActivity {
                agent: agent.clone(),
                task_id: task_id.clone(),
                spec_id: e.spec_id.clone(),
                activity_id: e.activity_id.clone(),
                reasoning: e.reasoning.clone(),
                text: e.text.clone(),
                tool: e.tool.clone(),
                command: e.command.clone(),
                command_started_at: e.command_started_at,
                model_started_at: e.model_started_at,
                received_bytes: e.received_bytes,
                stage: e.stage.clone(),
                stage_code: e.stage_code.clone(),
                tool_code: e.tool_code.as_ref().map(|(c, _)| c.clone()),
                tool_params: e.tool_code.as_ref().map(|(_, p)| p.clone()),
                text_code: e.text_code.as_ref().map(|(c, _)| c.clone()),
                text_params: e.text_code.as_ref().map(|(_, p)| p.clone()),
                age_ms: u64::try_from(now.saturating_duration_since(e.updated).as_millis())
                    .unwrap_or(u64::MAX),
            })
            .collect();
        if map.is_empty() {
            all.remove(project_id);
        }
        out.sort_by(|a, b| a.age_ms.cmp(&b.age_ms).then_with(|| a.agent.cmp(&b.agent)));
        out
    }

    pub fn clear_project(&self, project_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(project_id);
    }
}

/// `GET /api/projects/:id/activity`.
pub async fn get_activity(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    super::events::validate_project_id(&id)?;
    find_project(&state, &id).await?;
    let activities = state.events().live_activity().snapshot(&id);
    Ok(Json(json!({ "activities": activities })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(r: &LiveActivityRegistry, now: Instant, id: &str, kind: ActivityKind, text: Option<&str>) {
        let ev = Observed {
            agent: "developer",
            task_id: Some("T01"),
            spec_id: Some("026"),
            activity_id: id,
            kind,
            text,
            stage: None,
            stage_code: None,
            code: None,
            received_bytes: None,
        };
        r.record_at(now, "p", ev);
    }

    #[test]
    fn command_is_in_snapshot_and_heartbeat_keeps_entry_alive() {
        let r = LiveActivityRegistry::with_ttl(Duration::from_secs(120));
        let t = Instant::now();
        rec(&r, t, "a1", ActivityKind::Start, None);
        rec(&r, t, "a1", ActivityKind::Command, Some("npm test"));
        let t2 = t + Duration::from_secs(100);
        rec(&r, t2, "a1", ActivityKind::Heartbeat, None);
        let snap = r.snapshot_at(t2 + Duration::from_secs(100), "p");
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].command.as_deref(), Some("npm test"));
        assert!(snap[0].command_started_at.is_some());
        rec(&r, t2, "a1", ActivityKind::CommandEnd, None);
        let snap = r.snapshot_at(t2, "p");
        assert!(snap[0].command.is_none() && snap[0].command_started_at.is_none());
    }

    /// Spec 114 T04: a chamada ao modelo entra no instantâneo (início e bytes do heartbeat) e o
    /// heartbeat mantém a entrada viva além do TTL; `model_end` limpa.
    #[test]
    fn model_call_is_in_snapshot_with_received_bytes() {
        let r = LiveActivityRegistry::with_ttl(Duration::from_secs(120));
        let t = Instant::now();
        let ev = |kind, received_bytes| Observed {
            agent: "developer",
            task_id: Some("T02"),
            spec_id: Some("003"),
            activity_id: "a1",
            kind,
            text: None,
            stage: None,
            stage_code: None,
            code: None,
            received_bytes,
        };
        r.record_at(t, "p", ev(ActivityKind::Start, None));
        r.record_at(t, "p", ev(ActivityKind::Model, None));
        let s = r.snapshot_at(t, "p");
        assert!(s[0].model_started_at.is_some() && s[0].received_bytes.is_none());
        let mut at = t;
        for i in 1..=5u64 {
            at += Duration::from_secs(100);
            r.record_at(at, "p", ev(ActivityKind::ModelBeat, Some(i * 1000)));
        }
        let s = r.snapshot_at(at + Duration::from_secs(60), "p");
        assert_eq!(s.len(), 1, "o heartbeat da chamada mantém a entrada viva");
        assert_eq!(s[0].received_bytes, Some(5000));
        let json = serde_json::to_value(&s[0]).expect("json");
        assert_eq!(json["received_bytes"], 5000);
        r.record_at(at, "p", ev(ActivityKind::ModelEnd, None));
        let s = r.snapshot_at(at, "p");
        assert!(s[0].model_started_at.is_none() && s[0].received_bytes.is_none());
        let json = serde_json::to_value(&s[0]).expect("json");
        assert!(json.get("model_started_at").is_none());
    }

    #[test]
    fn final_verification_stage_survives_ttl_until_end() {
        let r = LiveActivityRegistry::with_ttl(Duration::from_secs(120));
        let t = Instant::now();
        let ev = |kind| Observed {
            agent: "reviewer",
            task_id: None,
            spec_id: Some("074"),
            activity_id: "f1",
            kind,
            text: None,
            stage: Some(FINAL_VERIFICATION_STAGE),
            stage_code: Some(FINAL_VERIFICATION_STAGE),
            code: None,
            received_bytes: None,
        };
        r.record_at(t, "p", ev(ActivityKind::Start));
        let late = t + Duration::from_secs(600);
        let snap = r.snapshot_at(late, "p");
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].stage.as_deref(), Some("final_verification"));
        assert_eq!(snap[0].stage_code.as_deref(), Some("final_verification"));
        assert!(snap[0].task_id.is_none());
        r.record_at(late, "p", ev(ActivityKind::End));
        assert!(r.snapshot_at(late, "p").is_empty());
    }

    #[test]
    fn start_resets_end_removes_only_current() {
        let r = LiveActivityRegistry::default();
        let t = Instant::now();
        rec(&r, t, "a1", ActivityKind::Start, None);
        rec(&r, t, "a1", ActivityKind::Reasoning, Some("pensando"));
        rec(&r, t, "a1", ActivityKind::Tool, Some("ls"));
        let s = r.snapshot_at(t, "p");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].reasoning.as_deref(), Some("pensando"));
        assert_eq!(s[0].tool.as_deref(), Some("ls"));
        rec(&r, t, "a2", ActivityKind::Start, None);
        let s = r.snapshot_at(t, "p");
        assert_eq!((s[0].activity_id.as_str(), s[0].reasoning.clone()), ("a2", None));
        rec(&r, t, "a1", ActivityKind::End, None);
        assert_eq!(r.snapshot_at(t, "p").len(), 1, "end antigo não remove");
        rec(&r, t, "a2", ActivityKind::End, None);
        assert!(r.snapshot_at(t, "p").is_empty());
    }

    #[test]
    fn expires_by_age_and_caps_entries() {
        let r = LiveActivityRegistry::with_ttl(Duration::from_secs(120));
        let t = Instant::now();
        rec(&r, t, "a1", ActivityKind::Start, None);
        assert_eq!(r.snapshot_at(t + Duration::from_secs(119), "p").len(), 1);
        assert!(r.snapshot_at(t + Duration::from_secs(121), "p").is_empty());
        for i in 0..40 {
            let agent = format!("ag{i}");
            let ev = Observed {
                agent: &agent,
                task_id: None,
                spec_id: None,
                activity_id: "x",
                kind: ActivityKind::Start,
                text: None,
                stage: None,
                stage_code: None,
                code: None,
                received_bytes: None,
            };
            r.record_at(t, "p", ev);
        }
        assert_eq!(r.snapshot_at(t, "p").len(), MAX_ENTRIES_PER_PROJECT);
        r.clear_project("p");
        assert!(r.snapshot_at(t, "p").is_empty());
    }

    /// Spec 102: o instantâneo leva o `code`/`params` do último rótulo e do último texto; um
    /// evento sem `code` limpa o anterior do mesmo tipo.
    #[test]
    fn snapshot_keeps_codes_of_tool_and_text() {
        let r = LiveActivityRegistry::default();
        let t = Instant::now();
        let params: serde_json::Map<String, serde_json::Value> =
            [("path".to_string(), serde_json::Value::from("web/a.png"))].into_iter().collect();
        let empty = serde_json::Map::new();
        let ev = |kind, text, code| Observed {
            agent: "developer",
            task_id: Some("T01"),
            spec_id: Some("102"),
            activity_id: "a1",
            kind,
            text,
            stage: Some("Escrevendo relatório final"),
            stage_code: Some("report"),
            code,
            received_bytes: None,
        };
        r.record_at(t, "p", ev(ActivityKind::Start, None, None));
        r.record_at(t, "p", ev(ActivityKind::Tool, Some("gerando imagem web/a.png"), Some(("tool.generate_image", &params))));
        r.record_at(t, "p", ev(ActivityKind::Text, Some("aguardando o ritmo"), Some(("pace.queued", &empty))));
        let s = r.snapshot_at(t, "p");
        assert_eq!(s[0].stage_code.as_deref(), Some("report"));
        assert_eq!(s[0].tool_code.as_deref(), Some("tool.generate_image"));
        assert_eq!(s[0].tool_params.as_ref().and_then(|p| p.get("path")), Some(&serde_json::Value::from("web/a.png")));
        assert_eq!(s[0].text_code.as_deref(), Some("pace.queued"));
        r.record_at(t, "p", ev(ActivityKind::Text, Some("texto do agente"), None));
        let s = r.snapshot_at(t, "p");
        assert!(s[0].text_code.is_none() && s[0].text_params.is_none());
        assert_eq!(s[0].tool_code.as_deref(), Some("tool.generate_image"));
    }
}
