//! Barramento de eventos em memória e stream SSE (`GET /api/events`).
//!
//! Envelope (BLUEPRINT §8): `{"event_type", "project_id", "payload"}`.
//! Chaves de API nunca entram em payloads: são removidas antes da publicação.
//! Assinantes lentos recebem um evento `lagged` e o stream continua.

use std::convert::Infallible;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use axum::http::StatusCode;
use serde_json::{json, Map, Value};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::watch;

use super::config_api::redact_urls;
use super::dialog::{deliver, DialogTarget};
use super::error::ApiError;
use super::state::AppState;
use crate::agents::activity::{ActivityEvent, ActivitySink, MAX_EVENT_TEXT_BYTES};
use crate::agents::runner::{EventSink, RunnerEvent};
use crate::config::Config;
use crate::sdd::TaskStatus;

/// Capacidade do canal de broadcast; assinantes além disso perdem eventos antigos.
pub const EVENT_CHANNEL_CAPACITY: usize = 1024;
/// Intervalo do comentário de keep-alive do SSE.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// Tamanho mínimo de um valor secreto para ser substituído (evita mascarar texto comum).
pub const MIN_SECRET_CHARS: usize = 4;
/// Substituto de todo valor secreto.
const SECRET_MASK: &str = "***";
/// Tamanho máximo do texto de `agent_message` (8 KiB).
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;
const TRUNCATED_SUFFIX: &str = "...[truncado]";
/// Nomes de chave que nunca podem aparecer em payloads (case-insensitive).
const SECRET_KEYS: [&str; 8] = [
    "api_key",
    "api-key",
    "apikey",
    "x-api-key",
    "authorization",
    "password",
    "secret",
    "token",
];

/// Tipos de evento publicados pela API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    ProjectStatus,
    AgentMessage,
    TaskUpdated,
    PermissionRequested,
    PermissionResolved,
    SddDraftUpdated,
    /// Projeto removido do Nants (os arquivos no disco permanecem).
    ProjectRemoved,
    /// Relatório final da spec gravado em `.sdd/specs/<id>/report.md`.
    ReportReady,
    /// Falha do relatório final (código fixo em `code`); a conclusão da spec não muda.
    ReportFailed,
    /// Fim da finalização pós-integração (relatório + ARCHITECTURE.md): a vaga foi liberada (`spec_id`).
    FinalizationDone,
    /// O Arquiteto começou a analisar uma tarefa bloqueada por qualidade (`spec_id`, `task_id`, `cycle`).
    RecoveryStarted,
    /// Decisão da análise de recuperação (`spec_id`, `task_id`, `cycle`, `decision`: RETRY ou ESCALATE).
    RecoveryDecided,
    /// App de teste iniciado (`spec_id`, `url`).
    AppStarted,
    /// App de teste encerrado (`code`, `reason`).
    AppExited,
    /// Atividade ao vivo de um agente (efêmera: só no barramento, nunca em arquivo).
    AgentActivity,
    /// Turno do Arquiteto começou (`turn_id`, `user_text` redigido se não interno, `started_at`).
    ChatTurnStarted,
    /// Turno do Arquiteto terminou (`turn_id`, `ok`).
    ChatTurnEnded,
    /// Lista de dispositivos autorizados mudou (`reason`: added, renamed ou revoked; spec 112).
    DevicesChanged,
    /// Janela de adicionar dispositivo invalidada por tentativas erradas (`reason`; spec 112).
    DeviceCodeLocked,
}

impl EventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProjectStatus => "project_status",
            Self::AgentMessage => "agent_message",
            Self::TaskUpdated => "task_updated",
            Self::PermissionRequested => "permission_requested",
            Self::PermissionResolved => "permission_resolved",
            Self::SddDraftUpdated => "sdd_draft_updated",
            Self::ProjectRemoved => "project_removed",
            Self::ReportReady => "report_ready",
            Self::ReportFailed => "report_failed",
            Self::FinalizationDone => "finalization_done",
            Self::RecoveryStarted => "recovery_started",
            Self::RecoveryDecided => "recovery_decided",
            Self::AppStarted => "app_started",
            Self::AppExited => "app_exited",
            Self::AgentActivity => "agent_activity",
            Self::ChatTurnStarted => "chat_turn_started",
            Self::ChatTurnEnded => "chat_turn_ended",
            Self::DevicesChanged => "devices_changed",
            Self::DeviceCodeLocked => "device_code_locked",
        }
    }
}

/// Evento no envelope da API. Eventos globais têm `project_id` nulo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiEvent {
    event_type: String,
    pub project_id: Option<String>,
    pub payload: Value,
}

impl ApiEvent {
    pub fn new(kind: EventType, project_id: Option<String>, payload: Value) -> Self {
        Self {
            event_type: kind.as_str().to_string(),
            project_id,
            payload,
        }
    }

    /// Nome do evento (sempre um `EventType::as_str`, seguro para o cabeçalho SSE).
    pub fn event_type(&self) -> &str {
        &self.event_type
    }
}

/// Barramento por processo: clones compartilham o mesmo canal.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ApiEvent>,
    live: super::live_activity::LiveActivityRegistry,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            tx,
            live: super::live_activity::LiveActivityRegistry::default(),
        }
    }

    /// Registro da atividade ao vivo dos agentes (compartilhado entre clones).
    pub fn live_activity(&self) -> &super::live_activity::LiveActivityRegistry {
        &self.live
    }

    /// Publica um evento após remover chaves de API do payload.
    /// Nunca bloqueia nem falha: sem assinantes o evento é descartado.
    pub fn publish(&self, mut event: ApiEvent) {
        strip_secrets(&mut event.payload);
        // Erro significa apenas "nenhum assinante": é esperado.
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApiEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Remove, em qualquer nível, chaves que possam carregar segredos.
fn strip_secrets(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !SECRET_KEYS.iter().any(|s| key.eq_ignore_ascii_case(s)));
            map.values_mut().for_each(strip_secrets);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_secrets),
        _ => {}
    }
}

/// Valida `project_id`: `[a-zA-Z0-9_-]{1,64}`.
pub fn validate_project_id(id: &str) -> Result<(), ApiError> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "project_id_invalid", Map::new()))
    }
}

/// Valores secretos conhecidos do processo: chaves literais, chaves resolvidas de
/// `env:NOME` e userinfo das `base_url`. Substituição exata (barreira principal).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretSet {
    /// Ordenados do maior para o menor, sem duplicatas (sobreposições são cobertas).
    values: Vec<String>,
}

impl SecretSet {
    /// Descarta valores com menos de `MIN_SECRET_CHARS` caracteres.
    pub fn new(values: impl IntoIterator<Item = String>) -> Self {
        let mut values: Vec<String> = values
            .into_iter()
            .filter(|v| v.chars().count() >= MIN_SECRET_CHARS)
            .collect();
        values.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        values.dedup();
        Self { values }
    }

    /// Reúne os segredos da configuração atual. Variáveis de ambiente ausentes são ignoradas.
    pub fn from_config(config: &Config) -> Self {
        let mut values = Vec::new();
        for conn in &config.connections {
            match conn.api_key.strip_prefix("env:") {
                Some(name) => {
                    if let Ok(value) = env::var(name) {
                        values.push(value);
                    }
                }
                None => values.push(conn.api_key.clone()),
            }
            values.extend(userinfo_candidates(&conn.base_url));
        }
        Self::new(values)
    }

    /// Substitui todas as ocorrências dos valores secretos por `***`. Sem regex.
    /// Os intervalos são colhidos sobre o texto original, unidos quando se sobrepõem
    /// ou são adjacentes, e cada união vira um único `***` (sem sufixos vazados).
    pub fn scrub_exact(&self, text: &str) -> String {
        // Custo O(n·k·m) no pior caso (n = texto, k = segredos, m = tamanho do segredo):
        // a varredura por início de char inclui ocorrências sobrepostas (ex.: período
        // interno). O texto é escrutinado antes do truncamento, que limita a saída.
        let mut spans: Vec<(usize, usize)> = self
            .values
            .iter()
            .flat_map(|secret| {
                let pat = secret.as_str();
                text.char_indices()
                    .filter(move |&(i, _)| text[i..].starts_with(pat))
                    .map(move |(i, _)| (i, i + pat.len()))
            })
            .collect();
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for (start, end) in spans {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        for (start, end) in merged {
            out.push_str(&text[pos..start]);
            out.push_str(SECRET_MASK);
            pos = end;
        }
        out.push_str(&text[pos..]);
        out
    }
}

/// Usuário, senha e `usuário:senha` de uma `base_url`, na forma original e decodificada.
fn userinfo_candidates(base_url: &str) -> Vec<String> {
    let Some(pos) = base_url.find("://") else {
        return Vec::new();
    };
    let tail = &base_url[pos + 3..];
    let end = tail
        .find(|c: char| matches!(c, '/' | '?' | '#'))
        .unwrap_or(tail.len());
    let authority = &tail[..end];
    let Some(at) = authority.rfind('@') else {
        return Vec::new();
    };
    let userinfo = &authority[..at];
    let mut parts = vec![userinfo];
    if let Some((user, pass)) = userinfo.split_once(':') {
        parts.push(user);
        parts.push(pass);
    }
    let mut out = Vec::new();
    for part in parts {
        out.push(part.to_string());
        out.extend(percent_decode(part));
    }
    out
}

/// Decodifica `%XX`; `None` se a sequência for inválida ou não resultar em UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Aplica a barreira de segredos exatos e depois a redação de URLs (texto livre).
fn clean(secrets: &SecretSet, text: &str) -> String {
    redact_urls(&secrets.scrub_exact(text))
}

/// Linhas de texto mantidas por mensagem; o excedente vira uma linha `…`.
pub const MAX_MESSAGE_LINES: usize = 400;
/// Marca do excedente de linhas.
const MORE_LINES_MARK: &str = "…";
/// Limite do texto já codificado em JSON (folga de 1 KiB para o resto da linha gravada).
const TEXT_JSON_LIMIT: usize = MAX_MESSAGE_BYTES - 1024;

/// Normalização única do texto de mensagem (evento ao vivo, gravação e GET usam esta função):
/// sequências de terminal (CSI e OSC, 7 e 8 bits, e escapes curtos) saem antes de tudo
/// ([`strip_escape_sequences`], spec 113); `\r\n` e `\r` viram `\n`; `\n` e `\t` são preservados;
/// demais controles (C0, DEL, C1) e formatação perigosa (bidi, largura zero, BOM) viram espaço; no
/// máximo `MAX_MESSAGE_LINES` linhas.
pub fn normalize_text(text: &str) -> String {
    let stripped = strip_escape_sequences(text);
    let unified = stripped.replace("\r\n", "\n").replace('\r', "\n");
    let flat: String = unified
        .chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_control() || is_dangerous_format(c) => ' ',
            c => c,
        })
        .collect();
    let mut lines = flat.split('\n');
    let kept: Vec<&str> = lines.by_ref().take(MAX_MESSAGE_LINES).collect();
    if lines.next().is_some() {
        format!("{}\n{MORE_LINES_MARK}", kept.join("\n"))
    } else {
        kept.join("\n")
    }
}

const ESC: char = '\u{1b}';
const CSI_8BIT: char = '\u{9b}';
const OSC_8BIT: char = '\u{9d}';
const ST_8BIT: char = '\u{9c}';
const BEL: char = '\u{7}';

/// Remove sequências de terminal de saídas de processo (cores do Vite, do npm, dos testes), numa
/// passada linear sem regex:
/// - CSI (`ESC [` ou `\u{9b}`): parâmetros `0x30..=0x3f`, intermediários `0x20..=0x2f` e o final
///   `0x40..=0x7e`. Sem final válido, só o introdutor e o que já foi lido saem;
/// - OSC (`ESC ]` ou `\u{9d}`): até BEL, `\u{9c}` ou `ESC \`; sem terminador, para antes da
///   quebra de linha (nunca engole o resto da mensagem);
/// - escapes curtos com intermediário (`ESC ( B`, `ESC # 8`) e os de cursor e teclado
///   (`ESC 7`, `ESC 8`, `ESC =`, `ESC >`).
///
/// Um ESC seguido de letra ou solto fica: o passo seguinte da normalização o troca por espaço
/// (a letra é texto, como em `h\u{1b}i`).
pub fn strip_escape_sequences(text: &str) -> String {
    if !text.contains([ESC, CSI_8BIT, OSC_8BIT]) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let kind = match c {
            CSI_8BIT => Some('['),
            OSC_8BIT => Some(']'),
            ESC => match chars.peek() {
                Some(&n @ ('[' | ']')) => {
                    chars.next();
                    Some(n)
                }
                Some(&n) if ('\u{20}'..='\u{2f}').contains(&n) || matches!(n, '7' | '8' | '=' | '>') => Some('e'),
                _ => None,
            },
            _ => None,
        };
        match kind {
            Some('[') => {
                while chars.next_if(|n| ('\u{30}'..='\u{3f}').contains(n)).is_some() {}
                while chars.next_if(|n| ('\u{20}'..='\u{2f}').contains(n)).is_some() {}
                chars.next_if(|n| ('\u{40}'..='\u{7e}').contains(n));
            }
            Some(']') => {
                while let Some(&n) = chars.peek() {
                    if n == '\n' || n == '\r' {
                        break;
                    }
                    chars.next();
                    if n == BEL || n == ST_8BIT {
                        break;
                    }
                    if n == ESC {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            Some(_) => {
                while chars.next_if(|n| ('\u{20}'..='\u{2f}').contains(n)).is_some() {}
                chars.next_if(|n| ('\u{30}'..='\u{7e}').contains(n));
            }
            None => out.push(c),
        }
    }
    out
}

/// Caracteres de formatação que reordenam ou escondem texto (bidi, largura zero, BOM).
fn is_dangerous_format(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}' | '\u{FEFF}'
    )
}

/// Bytes que o caractere ocupa dentro de uma string JSON (escapes do serde_json).
fn escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// Limita o texto normalizado ao limite de bytes codificados (sufixo `...[truncado]`).
pub fn truncate_text(text: String) -> String {
    let encoded: usize = text.chars().map(escaped_len).sum();
    if encoded <= TEXT_JSON_LIMIT {
        return text;
    }
    let budget = TEXT_JSON_LIMIT - TRUNCATED_SUFFIX.len();
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        let n = escaped_len(c);
        if used + n > budget {
            break;
        }
        used += n;
        out.push(c);
    }
    out.push_str(TRUNCATED_SUFFIX);
    out
}

/// Texto de mensagem como exibido e gravado: redação de segredos e URLs, normalização, limites.
pub fn message_text(secrets: &SecretSet, text: &str) -> String {
    truncate_text(normalize_text(&clean(secrets, text)))
}

/// Teto de bytes de cada parâmetro de texto de uma mensagem com `code` (spec 102).
pub const MAX_PARAM_BYTES: usize = 512;
/// Teto de parâmetros de uma mensagem com `code`.
const MAX_PARAMS: usize = 8;

/// `code` estável de mensagem (spec 102): `[a-z0-9_.]`, de 1 a 64 caracteres, começando por letra.
pub fn valid_message_code(code: &str) -> bool {
    code.len() <= 64
        && code.starts_with(|c: char| c.is_ascii_lowercase())
        && code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

/// `params` de mensagem com `code` (spec 102), saneados: nomes `[a-z0-9_]` até 32, no máximo
/// [`MAX_PARAMS`]; textos com a mesma normalização e redação do texto e cortados em
/// [`MAX_PARAM_BYTES`]; números inteiros mantidos; o resto é descartado. Mesma função no evento,
/// na linha gravada e no GET.
pub fn message_params(secrets: &SecretSet, params: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    params
        .iter()
        .filter(|(k, _)| !k.is_empty() && k.len() <= 32 && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
        .filter_map(|(k, v)| {
            let v = match v {
                Value::String(s) => Value::from(cut_bytes(&normalize_text(&clean(secrets, s)), MAX_PARAM_BYTES)),
                Value::Number(n) if n.is_i64() || n.is_u64() => Value::Number(n.clone()),
                _ => return None,
            };
            Some((k.clone(), v))
        })
        .take(MAX_PARAMS)
        .collect()
}

/// Limite de bytes de `agent` e `task_id` em mensagens (evento, linha gravada e GET).
pub const MAX_ID_BYTES: usize = 64;

/// Normalização de identificadores (`agent`, `task_id`): normalização de texto e corte em
/// `MAX_ID_BYTES` em fronteira de caractere. Mesma função no evento, na gravação e no GET.
pub fn normalize_id_text(text: &str) -> String {
    cut_bytes(&normalize_text(text), MAX_ID_BYTES)
}

/// Identificador exibido: redação de segredos e URLs, depois `normalize_id_text`.
pub fn normalize_id_field(secrets: &SecretSet, raw: &str) -> String {
    normalize_id_text(&clean(secrets, raw))
}

/// Corta `text` em no máximo `max` bytes, em fronteira de caractere.
pub fn cut_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Converte um evento do runner no evento da API do projeto dado.
pub fn runner_event_to_api(project_id: &str, secrets: &SecretSet, ev: RunnerEvent) -> ApiEvent {
    let pid = Some(project_id.to_string());
    match ev {
        RunnerEvent::AgentMessage {
            agent,
            text,
            task_id,
            attempt,
        } => ApiEvent::new(
            EventType::AgentMessage,
            pid,
            json!({
                "agent": normalize_id_field(secrets, &agent),
                "text": message_text(secrets, &text),
                "task_id": task_id.map(|t| normalize_id_field(secrets, &t)),
                "attempt": attempt,
                // Preenchido só quando a mensagem é gravada em `dialog.jsonl` (ver `dialog::deliver`).
                "seq": Value::Null,
            }),
        ),
        // Mensagem do sistema com `code`/`params` (spec 102); código inválido fica só com o texto.
        RunnerEvent::SystemNotice { text, code, params, task_id } => {
            let mut payload = json!({
                "agent": "system",
                "text": message_text(secrets, &text),
                "task_id": task_id.map(|t| normalize_id_field(secrets, &t)),
                "attempt": Value::Null,
                "seq": Value::Null,
            });
            if let (true, Some(map)) = (valid_message_code(&code), payload.as_object_mut()) {
                map.insert("code".into(), Value::from(code));
                map.insert("params".into(), Value::Object(message_params(secrets, &params)));
            }
            ApiEvent::new(EventType::AgentMessage, pid, payload)
        }
        // Atividade ao vivo "Testes": reaproveita `agent_activity` (kind `command` no início,
        // `end` no fim, mesmo `activity_id`). Efêmera: não entra no registro nem no diálogo.
        RunnerEvent::CommandRun {
            run_id,
            started,
            kind,
            label,
            stage_code,
            stage_params,
            command,
            task_id,
            attempt,
        } => {
            let mut payload = json!({
                "agent": "tests",
                "task_id": normalize_id_field(secrets, &task_id),
                "spec_id": Value::Null,
                "activity_id": normalize_id_field(secrets, &run_id),
                "kind": if started { "command" } else { "end" },
                "stage": cut_bytes(&normalize_text(&clean(secrets, &label)), 120),
                "stage_code": stage_code,
                "stage_params": message_params(secrets, &stage_params),
                "run_kind": kind.as_str(),
                "attempt": attempt,
            });
            if started {
                payload["text"] = json!(cut_bytes(&normalize_text(&clean(secrets, &command)), 200));
                payload["command_started_at"] = json!(super::live_activity::epoch_ms());
            }
            ApiEvent::new(EventType::AgentActivity, pid, payload)
        }
        RunnerEvent::TaskUpdated {
            spec_id,
            task_id,
            status,
        } => ApiEvent::new(
            EventType::TaskUpdated,
            pid,
            json!({
                "spec_id": clean(secrets, &spec_id),
                "task_id": clean(secrets, &task_id),
                "status": status_name(status),
            }),
        ),
        RunnerEvent::Integrated { spec_id, task_id } => ApiEvent::new(
            EventType::TaskUpdated,
            pid,
            json!({
                "spec_id": clean(secrets, &spec_id),
                "task_id": clean(secrets, &task_id),
                "status": status_name(TaskStatus::Done),
                "integrated": true,
            }),
        ),
    }
}

fn status_name(status: TaskStatus) -> Value {
    serde_json::to_value(status).unwrap_or(Value::Null)
}

/// Adaptador que publica os eventos do runner no barramento, para um projeto fixo.
/// Com `with_dialog`, as mensagens de agente são gravadas em `dialog.jsonl` antes de publicadas.
#[derive(Debug, Clone)]
pub struct BusSink {
    bus: EventBus,
    project_id: String,
    secrets: Arc<SecretSet>,
    dialog: Option<DialogTarget>,
}

impl BusSink {
    pub fn new(bus: EventBus, project_id: impl Into<String>, secrets: Arc<SecretSet>) -> Self {
        Self {
            bus,
            project_id: project_id.into(),
            secrets,
            dialog: None,
        }
    }

    /// Grava as mensagens de agente do run no diálogo da spec.
    pub fn with_dialog(mut self, target: DialogTarget) -> Self {
        self.dialog = Some(target);
        self
    }
}

impl EventSink for BusSink {
    fn emit(&self, ev: RunnerEvent) {
        let event = runner_event_to_api(&self.project_id, &self.secrets, ev);
        deliver(&self.bus, self.dialog.as_ref(), event);
    }
}

/// Teto de `.sdd/specs/<spec>/tools.jsonl`; ao atingir, a trilha para de crescer sem erro.
pub const MAX_TOOLS_TRAIL_BYTES: u64 = 2 * 1024 * 1024;

/// Nome do arquivo da trilha de ferramentas dentro da pasta da spec.
pub const TOOLS_TRAIL_FILE: &str = "tools.jsonl";

/// Destino da trilha de ferramentas: raiz do projeto + spec (já validada).
#[derive(Debug, Clone)]
struct TrailTarget {
    root: PathBuf,
    rel: String,
}

/// Publica a atividade ao vivo dos agentes no barramento de um projeto. Redige segredos e URLs,
/// normaliza e corta o texto; nada vai para `dialog.jsonl` nem chat. Com `with_trail`, só os
/// eventos `tool` e `command` (já redigidos e cortados) viram linha em `tools.jsonl` da spec.
#[derive(Debug, Clone)]
pub struct BusActivitySink {
    bus: EventBus,
    project_id: String,
    secrets: Arc<SecretSet>,
    /// Etapa: `code` estável (spec 102) e o rótulo de reserva em pt-BR.
    stage: Option<(&'static str, String)>,
    trail: Option<TrailTarget>,
}

impl BusActivitySink {
    pub fn new(bus: EventBus, project_id: impl Into<String>, secrets: Arc<SecretSet>) -> Self {
        Self {
            bus,
            project_id: project_id.into(),
            secrets,
            stage: None,
            trail: None,
        }
    }

    /// Grava a trilha de ferramentas em `.sdd/specs/<spec_id>/tools.jsonl`. `spec_id` inválido
    /// desliga a trilha (o evento continua sendo publicado).
    pub fn with_trail(mut self, root: impl Into<PathBuf>, spec_id: &str) -> Self {
        self.trail = super::spec_result::validate_spec_id(spec_id).ok().map(|()| TrailTarget {
            root: root.into(),
            rel: format!(".sdd/specs/{spec_id}/{TOOLS_TRAIL_FILE}"),
        });
        self
    }

    /// Acrescenta uma linha à trilha com os valores já redigidos do payload. Síncrono e curto;
    /// falha (symlink, pasta ausente, teto) é ignorada e nunca impede a publicação.
    fn append_trail(&self, payload: &Value) {
        let Some(trail) = &self.trail else { return };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut line = json!({
            "ts": ts,
            "agent": payload["agent"],
            "task_id": payload["task_id"],
            "kind": payload["kind"],
            "text": payload["text"],
        });
        for key in ["stage", "stage_code", "code", "params"] {
            if let Some(value) = payload.get(key) {
                line[key] = value.clone();
            }
        }
        if let Ok(text) = serde_json::to_string(&line) {
            let _ = super::safe_fs::append_bounded(&trail.root, &trail.rel, &text, MAX_TOOLS_TRAIL_BYTES);
        }
    }

    /// Etapa exibida no cabeçalho da bolha: `code` estável (ex.: `architecture`), que a interface
    /// traduz, e o rótulo de reserva em pt-BR (ex.: "Atualizando ARCHITECTURE.md").
    pub fn with_stage(mut self, code: &'static str, stage: &str) -> Self {
        self.stage = Some((code, stage.to_string()));
        self
    }
}

impl ActivitySink for BusActivitySink {
    fn sanitize(&self, text: &str) -> String {
        normalize_text(&clean(&self.secrets, text))
    }

    fn emit(&self, ev: ActivityEvent) {
        let secrets = &self.secrets;
        let opt = |v: &Option<String>| v.as_deref().map(|t| normalize_id_field(secrets, t));
        let text = ev
            .text
            .as_deref()
            .map(|t| cut_bytes(&normalize_text(&clean(secrets, t)), MAX_EVENT_TEXT_BYTES));
        let payload = json!({
            "agent": normalize_id_field(secrets, &ev.agent),
            "task_id": opt(&ev.task_id),
            "spec_id": opt(&ev.spec_id),
            "activity_id": normalize_id_field(secrets, &ev.activity_id),
            "kind": ev.kind.as_str(),
            "text": text,
        });
        let mut payload = payload;
        if let Some((code, stage)) = &self.stage {
            payload["stage"] = json!(stage);
            payload["stage_code"] = json!(code);
        }
        // `code`/`params` do texto (rótulo de ferramenta, aviso do limitador), spec 102.
        let coded = ev
            .code
            .as_ref()
            .filter(|c| valid_message_code(&c.code))
            .map(|c| (c.code.clone(), message_params(secrets, &c.params)));
        if let Some((code, params)) = &coded {
            payload["code"] = json!(code);
            payload["params"] = Value::Object(params.clone());
        }
        if ev.kind == crate::agents::activity::ActivityKind::Command {
            payload["command_started_at"] = json!(super::live_activity::epoch_ms());
        }
        // Spec 114 T04: início da chamada ao modelo e o tamanho já recebido da resposta.
        if ev.kind == crate::agents::activity::ActivityKind::Model {
            payload["model_started_at"] = json!(super::live_activity::epoch_ms());
        }
        if let Some(bytes) = ev.received_bytes {
            payload["received_bytes"] = json!(bytes);
        }
        if matches!(
            ev.kind,
            crate::agents::activity::ActivityKind::Tool | crate::agents::activity::ActivityKind::Command
        ) {
            self.append_trail(&payload);
        }
        self.bus.live_activity().record(
            &self.project_id,
            super::live_activity::Observed {
                agent: payload["agent"].as_str().unwrap_or_default(),
                task_id: payload["task_id"].as_str(),
                spec_id: payload["spec_id"].as_str(),
                activity_id: payload["activity_id"].as_str().unwrap_or_default(),
                kind: ev.kind,
                text: text.as_deref(),
                stage: self.stage.as_ref().map(|(_, s)| s.as_str()),
                stage_code: self.stage.as_ref().map(|(c, _)| *c),
                code: coded.as_ref().map(|(c, p)| (c.as_str(), p)),
                received_bytes: ev.received_bytes,
            },
        );
        self.bus.publish(ApiEvent::new(
            EventType::AgentActivity,
            Some(self.project_id.clone()),
            payload,
        ));
    }
}

/// Query de `GET /api/events`.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub project_id: Option<String>,
}

/// Eventos globais (sem `project_id`) sempre passam; os demais só se forem do filtro.
pub fn matches_project(filter: Option<&str>, event: &ApiEvent) -> bool {
    match (filter, event.project_id.as_deref()) {
        (Some(wanted), Some(actual)) => wanted == actual,
        _ => true,
    }
}

/// `GET /api/events`: stream SSE com filtro opcional por projeto.
pub async fn stream_events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<Response, ApiError> {
    if let Some(id) = query.project_id.as_deref() {
        validate_project_id(id)?;
    }
    let rx = state.events().subscribe();
    let stream = event_stream(rx, query.project_id, state.shutdown_signal());
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE_INTERVAL))
        .into_response())
}

/// Stream de eventos SSE. Termina quando o sinal de shutdown é acionado
/// (ou o sinal é descartado) ou quando todos os emissores forem fechados.
fn event_stream(
    rx: broadcast::Receiver<ApiEvent>,
    filter: Option<String>,
    shutdown: watch::Receiver<bool>,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    event_stream_with_ping(rx, filter, shutdown, KEEP_ALIVE_INTERVAL)
}

/// Como `event_stream`, emitindo também o evento nomeado `ping` (data `{}`, sem `project_id`)
/// a cada `ping_every`. Diferente do keep-alive em comentário, o `ping` chega ao JavaScript e
/// permite ao cliente detectar conexão zumbi (spec 080 T01).
fn event_stream_with_ping(
    rx: broadcast::Receiver<ApiEvent>,
    filter: Option<String>,
    shutdown: watch::Receiver<bool>,
    ping_every: Duration,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + ping_every, ping_every);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let init = (rx, filter, shutdown, ping);
    stream::unfold(init, |(mut rx, filter, mut shutdown, mut ping)| async move {
        loop {
            let item = tokio::select! {
                biased;
                () = wait_shutdown(&mut shutdown) => return None,
                _ = ping.tick() => ping_event(),
                received = rx.recv() => match received {
                    Ok(ev) => {
                        if !matches_project(filter.as_deref(), &ev) {
                            continue;
                        }
                        match sse_event(&ev) {
                            Some(item) => item,
                            None => continue,
                        }
                    }
                    Err(RecvError::Lagged(skipped)) => lagged_event(skipped),
                    Err(RecvError::Closed) => return None,
                },
            };
            return Some((Ok(item), (rx, filter, shutdown, ping)));
        }
    })
}

/// Completa quando o shutdown estiver acionado (ou o emissor sumir).
async fn wait_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow_and_update() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

fn sse_event(ev: &ApiEvent) -> Option<Event> {
    let data = serde_json::to_string(ev).ok()?;
    Some(Event::default().event(ev.event_type.as_str()).data(data))
}

/// Evento `ping` visível ao cliente: dado mínimo, sem dados de projeto.
fn ping_event() -> Event {
    Event::default().event("ping").data("{}")
}

fn lagged_event(skipped: u64) -> Event {
    Event::default()
        .event("lagged")
        .data(json!({ "skipped": skipped }).to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use axum::body::Body;
    use axum::http::{header, Method, Request, StatusCode};
    use futures_util::StreamExt;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::config::{Config, Connection};
    use crate::server::{router, AppState};

    const PORT: u16 = 3000;
    const HOST: &str = "127.0.0.1:3000";

    fn state() -> AppState {
        state_with(Config::default())
    }

    fn state_with(cfg: Config) -> AppState {
        AppState::new(PathBuf::from("config.toml"), cfg, PORT)
    }

    fn msg(project: Option<&str>, marker: &str) -> ApiEvent {
        ApiEvent::new(
            EventType::AgentMessage,
            project.map(String::from),
            json!({ "agent": "architect", "text": marker }),
        )
    }

    fn conn(id: &str, api_key: &str, base_url: &str) -> Connection {
        Connection {
            id: id.into(),
            name: id.into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            wire_api: crate::config::WireApi::default(),
            kind: Default::default(),
        }
    }

    #[test]
    fn secret_set_replaces_literal_key_everywhere() {
        let set = SecretSet::new(vec!["sk-LITERAL-SEGREDO".to_string()]);
        let out = set.scrub_exact("erro: sk-LITERAL-SEGREDO e {\"k\":\"sk-LITERAL-SEGREDO\"}");
        assert_eq!(out, "erro: *** e {\"k\":\"***\"}");
    }

    #[test]
    fn secret_set_from_config_resolves_env_and_userinfo() {
        let var = "NANTS_T02_SECRET_SET_RESOLVE_TEST";
        std::env::set_var(var, "env-valor-SEGREDO");
        let mut cfg = Config::default();
        cfg.connections.push(conn("lit", "sk-lit-SEGREDO", "https://api.example.com/v1"));
        cfg.connections.push(conn(
            "env",
            &format!("env:{var}"),
            "https://usu%40rio:se%3Anha-SEG@api.example.com/v1",
        ));
        let set = SecretSet::from_config(&cfg);
        std::env::remove_var(var);
        let text = "a sk-lit-SEGREDO b env-valor-SEGREDO c usu@rio d se:nha-SEG e usu%40rio f se%3Anha-SEG";
        let out = set.scrub_exact(text);
        assert!(!out.contains("SEG"), "{out}");
        assert!(!out.contains("rio"), "{out}");
        assert!(!out.contains("nha"), "{out}");
        assert_eq!(out, "a *** b *** c *** d *** e *** f ***");
    }

    #[test]
    fn secret_set_ignores_unset_env_and_short_values() {
        let mut cfg = Config::default();
        cfg.connections.push(conn("e", "env:NANTS_T02_SURELY_UNSET_VAR_XYZ", "https://h/v1"));
        cfg.connections.push(conn("s", "abc", "https://h/v1"));
        let set = SecretSet::from_config(&cfg);
        assert_eq!(set, SecretSet::default());
        assert_eq!(set.scrub_exact("abc xyz"), "abc xyz");
    }

    #[test]
    fn secret_set_periodic_secret_overlaps_are_masked() {
        let set = SecretSet::new(vec!["abcdabcd".into()]);
        assert_eq!(set.scrub_exact("abcdabcdabcd"), "***");
        let digits = SecretSet::new(vec!["11111111".into()]);
        assert_eq!(digits.scrub_exact("x 111111111111 y"), "x *** y");
    }

    #[test]
    fn secret_set_multibyte_periodic_secret_does_not_panic() {
        let secret = "ñoñoñoño";
        let set = SecretSet::new(vec![secret.to_string()]);
        let text = format!("pre {} meio {secret}{secret} fim ñ", "ñoñoñoño".repeat(3));
        let out = set.scrub_exact(&text);
        assert!(!out.contains("ñoñ"), "{out}");
        assert!(out.starts_with("pre ***"), "{out}");
        assert!(out.ends_with("fim ñ"), "{out}");
    }

    #[test]
    fn secret_set_partial_overlap_is_fully_masked() {
        let set = SecretSet::new(vec!["abcdef".into(), "cdefgh".into()]);
        assert_eq!(set.scrub_exact("abcdefgh"), "***");
        let reversed = SecretSet::new(vec!["cdefgh".into(), "abcdef".into()]);
        assert_eq!(reversed.scrub_exact("abcdefgh"), "***");
        let key_and_password = SecretSet::new(vec!["sk-ABCD1234".into(), "1234XYZW".into()]);
        assert_eq!(key_and_password.scrub_exact("sk-ABCD1234XYZW"), "***");
    }

    #[test]
    fn secret_set_chained_secrets_collapse_into_one_mask() {
        let set = SecretSet::new(vec!["abcd".into(), "cdef".into(), "efgh".into()]);
        assert_eq!(set.scrub_exact("abcdefgh"), "***");
    }

    #[test]
    fn secret_set_adjacent_secrets_are_masked() {
        let set = SecretSet::new(vec!["abcd".into(), "efgh".into()]);
        assert_eq!(set.scrub_exact("abcdefgh"), "***");
        assert_eq!(set.scrub_exact("x abcd-efgh y"), "x ***-*** y");
    }

    #[test]
    fn secret_set_repeated_occurrences_are_each_masked() {
        let set = SecretSet::new(vec!["sk-123456".into()]);
        assert_eq!(
            set.scrub_exact("x sk-123456 y sk-123456 z"),
            "x *** y *** z"
        );
    }

    #[test]
    fn secret_set_keeps_text_without_secrets_intact() {
        let set = SecretSet::new(vec!["segredo-123".into()]);
        let text = "texto comum, acentuação é ✓ sem nada";
        assert_eq!(set.scrub_exact(text), text);
        assert_eq!(SecretSet::default().scrub_exact(text), text);
    }

    #[test]
    fn secret_set_covers_overlapping_values() {
        let set = SecretSet::new(vec![
            "abc".into(),
            "segredo".into(),
            "segredo-longo-123".into(),
        ]);
        assert_eq!(set.scrub_exact("abc e segredo-longo-123"), "abc e ***");
    }

    #[tokio::test]
    async fn command_run_becomes_scrubbed_agent_activity_start_and_end() {
        let key = "sk-CHAVE-DO-SERVIDOR-123";
        let mut cfg = Config::default();
        cfg.connections.push(conn("c", key, "https://api.example.com/v1"));
        let st = state_with(cfg);
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let sink = BusSink::new(bus.clone(), "proj", st.secret_set().await);
        let run = |started: bool| RunnerEvent::CommandRun {
            run_id: "T01-1-0".into(),
            started,
            kind: crate::agents::runner::CommandKind::Check,
            label: "Checagem build · tentativa 1".into(),
            stage_code: "check",
            stage_params: [("attempt".to_string(), json!(1)), ("check".to_string(), json!("build"))].into_iter().collect(),
            command: format!("TOKEN={key} npm test"),
            task_id: "T01".into(),
            attempt: 1,
        };
        sink.emit(run(true));
        sink.emit(run(false));
        let start = rx.try_recv().expect("início");
        let end = rx.try_recv().expect("fim");
        assert_eq!(start.event_type(), "agent_activity");
        let text = serde_json::to_string(&start).expect("json");
        assert!(!text.contains(key), "{text}");
        assert_eq!(start.payload["agent"], "tests");
        assert_eq!(start.payload["kind"], "command");
        assert_eq!(start.payload["text"], "TOKEN=*** npm test");
        assert_eq!(start.payload["task_id"], "T01");
        assert_eq!(start.payload["run_kind"], "check");
        assert_eq!(start.payload["attempt"], 1);
        assert_eq!(start.payload["stage"], "Checagem build · tentativa 1");
        assert_eq!(start.payload["stage_code"], "check");
        assert_eq!(start.payload["stage_params"], json!({"attempt": 1, "check": "build"}));
        assert!(start.payload["command_started_at"].as_u64().is_some());
        assert_eq!(end.payload["kind"], "end");
        assert_eq!(end.payload["activity_id"], start.payload["activity_id"]);
        // Efêmero: não entra no registro de atividade (reidratação só do histórico persistido).
        assert!(bus.live_activity().snapshot("proj").is_empty());
    }

    #[tokio::test]
    async fn bus_sink_publishes_agent_message_without_configured_key() {
        let key = "sk-CHAVE-DO-SERVIDOR-123";
        let mut cfg = Config::default();
        cfg.connections.push(conn("c", key, "https://api.example.com/v1"));
        let st = state_with(cfg);
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let sink = BusSink::new(bus.clone(), "proj", st.secret_set().await);
        sink.emit(RunnerEvent::AgentMessage {
            agent: "architect".into(),
            text: format!("erro com chave {key} aqui"),
            task_id: Some("T01".into()),
            attempt: Some(1),
        });
        let got = rx.try_recv().expect("evento");
        let text = serde_json::to_string(&got).expect("json");
        assert!(!text.contains(key), "{text}");
        assert_eq!(got.payload["text"], "erro com chave *** aqui");
    }

    fn sse_get(uri: &str, host: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(header::HOST, host)
            .body(Body::empty())
            .expect("requisição")
    }

    /// Lê o próximo bloco SSE completo (terminado em linha vazia), com timeout curto.
    async fn next_block(body: &mut Body, buf: &mut String) -> String {
        loop {
            if let Some(pos) = buf.find("\n\n") {
                let block = buf[..pos].to_string();
                buf.drain(..pos + 2);
                return block;
            }
            let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
                .await
                .expect("timeout esperando evento SSE")
                .expect("stream encerrou")
                .expect("frame válido");
            if let Ok(data) = frame.into_data() {
                buf.push_str(std::str::from_utf8(&data).expect("utf8"));
            }
        }
    }

    /// Separa um bloco SSE em (nome do evento, JSON do data).
    fn parse_block(block: &str) -> (String, Value) {
        let mut name = String::new();
        let mut data = String::new();
        for line in block.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                name = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                data.push_str(v.trim());
            }
        }
        (name, serde_json::from_str(&data).expect("json do data"))
    }

    #[test]
    fn event_type_names_match_blueprint() {
        assert_eq!(EventType::ProjectStatus.as_str(), "project_status");
        assert_eq!(EventType::AgentMessage.as_str(), "agent_message");
        assert_eq!(EventType::TaskUpdated.as_str(), "task_updated");
        assert_eq!(EventType::PermissionRequested.as_str(), "permission_requested");
        assert_eq!(EventType::PermissionResolved.as_str(), "permission_resolved");
        assert_eq!(EventType::SddDraftUpdated.as_str(), "sdd_draft_updated");
    }

    #[test]
    fn envelope_serializes_with_three_fields() {
        let ev = ApiEvent::new(EventType::TaskUpdated, None, json!({"a": 1}));
        let v = serde_json::to_value(&ev).expect("json");
        assert_eq!(
            v,
            json!({"event_type": "task_updated", "project_id": null, "payload": {"a": 1}})
        );
    }

    #[tokio::test]
    async fn subscriber_receives_published_events() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        bus.publish(msg(Some("p1"), "oi"));
        let got = rx.recv().await.expect("evento");
        assert_eq!(got.event_type, "agent_message");
        assert_eq!(got.project_id.as_deref(), Some("p1"));
        assert_eq!(got.payload["text"], "oi");
    }

    #[test]
    fn publish_without_subscribers_does_not_fail() {
        let bus = EventBus::new();
        bus.publish(msg(None, "ninguém ouve"));
        let mut rx = bus.subscribe();
        bus.publish(msg(None, "agora sim"));
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn sse_stream_emits_named_ping_without_project_data() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        let (_keep, shutdown) = tokio::sync::watch::channel(false);
        let stream =
            event_stream_with_ping(rx, Some("p1".into()), shutdown, Duration::from_millis(50));
        let mut body = Sse::new(stream).into_response().into_body();
        let mut buf = String::new();
        let block = next_block(&mut body, &mut buf).await;
        assert!(block.contains("event: ping"), "bloco: {block}");
        let (name, data) = parse_block(&block);
        assert_eq!(name, "ping");
        assert_eq!(data, json!({}));
        assert!(!block.contains("project_id"));
        // O ping se repete e os eventos normais continuam passando.
        bus.publish(msg(Some("p1"), "oi"));
        let mut names = Vec::new();
        for _ in 0..3 {
            names.push(parse_block(&next_block(&mut body, &mut buf).await).0);
        }
        assert!(names.contains(&"agent_message".to_string()), "{names:?}");
        assert!(names.contains(&"ping".to_string()), "{names:?}");
        drop(bus);
    }

    #[tokio::test(start_paused = true)]
    async fn api_events_route_emits_ping_at_keep_alive_interval() {
        let st = state();
        let resp = router(st.clone())
            .oneshot(sse_get("/api/events?project_id=p1", HOST))
            .await
            .expect("resposta");
        let mut body = resp.into_body();
        let mut buf = String::new();
        // Com o tempo pausado, o runtime avança até o próximo timer (o ping de 15 s).
        let block = loop {
            let frame = body.frame().await.expect("stream ativo").expect("frame");
            if let Ok(data) = frame.into_data() {
                buf.push_str(std::str::from_utf8(&data).expect("utf8"));
            }
            if let Some(pos) = buf.find("event: ping") {
                break buf[pos..].to_string();
            }
        };
        assert!(block.starts_with("event: ping\ndata: {}"), "{block}");
        st.trigger_shutdown();
    }

    #[tokio::test]
    async fn stream_ends_when_all_senders_are_dropped() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        let (_keep, shutdown) = tokio::sync::watch::channel(false);
        let mut stream = Box::pin(event_stream(rx, None, shutdown));
        drop(bus);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn stream_ends_promptly_on_shutdown_signal() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        let (tx, shutdown) = tokio::sync::watch::channel(false);
        let mut stream = Box::pin(event_stream(rx, None, shutdown));
        let trigger = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send_replace(true);
        });
        let next = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("stream não terminou em 1 s após shutdown");
        assert!(next.is_none());
        trigger.await.expect("task de disparo");
        drop(bus);
    }

    #[tokio::test]
    async fn sse_body_ends_after_app_shutdown() {
        let st = state();
        let resp = router(st.clone())
            .oneshot(sse_get("/api/events", HOST))
            .await
            .expect("resposta");
        let mut body = resp.into_body();
        st.trigger_shutdown();
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("body não terminou em 1 s");
        assert!(frame.is_none());
    }

    #[tokio::test]
    async fn activity_sink_redacts_cuts_and_filters_by_project() {
        use crate::agents::activity::{ActivityKind, ActivityScope};
        use crate::agents::llm_client::StreamDelta;
        let bus = EventBus::new();
        let rx = bus.subscribe();
        let secrets = Arc::new(SecretSet::new(["chave-super-secreta".to_string()]));
        let sink = Arc::new(BusActivitySink::new(bus.clone(), "proj-a", secrets));
        let other = Arc::new(BusActivitySink::new(bus.clone(), "proj-b", Arc::new(SecretSet::default())));
        let scope = ActivityScope::new(sink, "developer", Some("024".into()), Some("T02".into()));
        let run = scope.begin();
        run.delta(StreamDelta::Reasoning("usar chave-super-secreta agora ".into()));
        run.tool("run_command", r#"{"command":"curl -H chave-super-secreta x"}"#);
        run.delta(StreamDelta::Text("x".repeat(10_000)));
        run.end();
        ActivityScope::new(other, "reviewer", None, None).begin().end();

        let (_keep, shutdown) = tokio::sync::watch::channel(false);
        let mut stream = Box::pin(event_stream(rx, Some("proj-a".into()), shutdown));
        let mut seen = Vec::new();
        while seen.len() < 5 {
            let item = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .expect("evento")
                .expect("stream aberto")
                .expect("infalível");
            seen.push(format!("{item:?}"));
        }
        let all = seen.join("\n");
        assert!(!all.contains("chave-super-secreta"), "{all}");
        assert!(all.contains("agent_activity"));
        assert!(all.contains("developer"));
        assert!(!all.contains("reviewer"), "evento de outro projeto vazou");

        let mut rx = bus.subscribe();
        let sink = BusActivitySink::new(bus.clone(), "p", Arc::new(SecretSet::default()));
        use crate::agents::activity::{ActivityEvent, ActivitySink as _};
        sink.emit(ActivityEvent {
            agent: "architect".into(),
            spec_id: None,
            task_id: None,
            activity_id: "a1".into(),
            kind: ActivityKind::Text,
            text: Some("y".repeat(10_000)),
            code: None,
            received_bytes: None,
        });
        let ev = rx.try_recv().expect("evento");
        assert_eq!(ev.event_type(), "agent_activity");
        assert_eq!(ev.payload["kind"], "text");
        assert!(ev.payload["task_id"].is_null());
        assert!(ev.payload["text"].as_str().expect("texto").len() <= 2048);
    }

    #[test]
    fn activity_is_not_persisted_in_dialog() {
        let dir = std::env::temp_dir().join(format!("nants-act-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let bus = EventBus::new();
        let sink = BusActivitySink::new(bus, "p", Arc::new(SecretSet::default()));
        use crate::agents::activity::{ActivityEvent, ActivityKind, ActivitySink as _};
        sink.emit(ActivityEvent {
            agent: "architect".into(),
            spec_id: Some("024".into()),
            task_id: None,
            activity_id: "a1".into(),
            kind: ActivityKind::Start,
            text: None,
            code: None,
            received_bytes: None,
        });
        let entries = std::fs::read_dir(&dir).expect("ler").count();
        assert_eq!(entries, 0, "nada pode ser gravado em disco");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Spec 114 T04: `model` leva o início da chamada e o heartbeat dela, os bytes recebidos.
    #[test]
    fn model_call_payload_has_start_and_received_bytes() {
        use crate::agents::activity::{ActivityEvent, ActivityKind, ActivitySink as _};
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let sink = BusActivitySink::new(bus, "p", Arc::new(SecretSet::default()));
        let ev = |kind, received_bytes| ActivityEvent {
            agent: "developer".into(),
            spec_id: Some("003".into()),
            task_id: Some("T02".into()),
            activity_id: "a1".into(),
            kind,
            text: None,
            code: None,
            received_bytes,
        };
        sink.emit(ev(ActivityKind::Model, None));
        sink.emit(ev(ActivityKind::ModelBeat, Some(48_000)));
        sink.emit(ev(ActivityKind::ModelEnd, None));
        let model = rx.try_recv().expect("model");
        assert_eq!(model.payload["kind"], "model");
        assert!(model.payload["model_started_at"].as_u64().is_some_and(|ms| ms > 0));
        assert!(model.payload.get("received_bytes").is_none());
        let beat = rx.try_recv().expect("heartbeat");
        assert_eq!((beat.payload["kind"].as_str(), beat.payload["received_bytes"].as_u64()), (Some("heartbeat"), Some(48_000)));
        assert_eq!(rx.try_recv().expect("fim").payload["kind"], "model_end");
    }

    #[test]
    fn bus_sink_maps_task_updated_and_integrated() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let sink = BusSink::new(bus.clone(), "proj", Arc::new(SecretSet::default()));
        sink.emit(RunnerEvent::TaskUpdated {
            spec_id: "004".into(),
            task_id: "T02".into(),
            status: TaskStatus::InReview,
        });
        sink.emit(RunnerEvent::Integrated {
            spec_id: "004".into(),
            task_id: "T01".into(),
        });
        let a = rx.try_recv().expect("a");
        assert_eq!(a.event_type, "task_updated");
        assert_eq!(a.project_id.as_deref(), Some("proj"));
        assert_eq!(a.payload["status"], "IN_REVIEW");
        assert_eq!(a.payload["task_id"], "T02");
        let b = rx.try_recv().expect("b");
        assert_eq!(b.event_type, "task_updated");
        assert_eq!(b.payload["status"], "DONE");
        assert_eq!(b.payload["integrated"], true);
    }

    #[test]
    fn bus_sink_scrubs_userinfo_and_truncates_long_text() {
        let msg = format!(
            "erro em https://user:SEGREDO@host/v1 {}",
            "a".repeat(MAX_MESSAGE_BYTES * 2)
        );
        let ev = runner_event_to_api(
            "p",
            &SecretSet::default(),
            RunnerEvent::AgentMessage {
                agent: "architect".into(),
                text: msg,
                task_id: None,
                attempt: None,
            },
        );
        let text = ev.payload["text"].as_str().expect("texto");
        assert!(!text.contains("SEGREDO"), "{text}");
        assert!(text.len() <= MAX_MESSAGE_BYTES, "{}", text.len());
        assert!(text.ends_with(TRUNCATED_SUFFIX));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let text = "é".repeat(MAX_MESSAGE_BYTES);
        let out = clean(&SecretSet::default(), &text);
        let out = truncate_text(out);
        assert!(out.len() <= MAX_MESSAGE_BYTES);
        assert!(out.ends_with(TRUNCATED_SUFFIX));
    }

    #[test]
    fn publish_strips_extended_secret_keys() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        bus.publish(ApiEvent::new(
            EventType::ProjectStatus,
            None,
            json!({
                "api-key": "SEGREDO-1",
                "x-api-key": "SEGREDO-2",
                "apiKey": "SEGREDO-3",
                "authorization": "SEGREDO-4",
                "Password": "SEGREDO-5",
                "secret": "SEGREDO-6",
                "token": "SEGREDO-7",
                "ok": 1
            }),
        ));
        let got = rx.try_recv().expect("evento");
        let text = serde_json::to_string(&got).expect("json");
        assert!(!text.contains("SEGREDO"), "{text}");
        assert_eq!(got.payload, json!({"ok": 1}));
    }

    #[test]
    fn publish_strips_api_keys_at_any_level() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        bus.publish(ApiEvent::new(
            EventType::ProjectStatus,
            None,
            json!({
                "api_key": "sk-SEGREDO-1",
                "ok": 1,
                "nested": { "API_KEY": "sk-SEGREDO-2", "keep": "x" },
                "list": [{ "apikey": "sk-SEGREDO-3" }, { "v": 2 }]
            }),
        ));
        let got = rx.try_recv().expect("evento");
        let text = serde_json::to_string(&got).expect("json");
        assert!(!text.contains("SEGREDO"), "{text}");
        assert!(!text.contains("api_key"), "{text}");
        assert_eq!(got.payload["ok"], 1);
        assert_eq!(got.payload["nested"]["keep"], "x");
        assert_eq!(got.payload["list"][1]["v"], 2);
    }

    #[tokio::test]
    async fn sse_delivers_agent_message_and_task_updated_in_envelope() {
        let st = state();
        let resp = router(st.clone())
            .oneshot(sse_get("/api/events", HOST))
            .await
            .expect("resposta");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers()[header::CONTENT_TYPE]
            .to_str()
            .expect("ct")
            .starts_with("text/event-stream"));
        let mut body = resp.into_body();
        st.events().publish(msg(Some("p1"), "olá"));
        st.events().publish(ApiEvent::new(
            EventType::TaskUpdated,
            Some("p1".into()),
            json!({"spec_id": "004", "task_id": "T02", "status": "DONE"}),
        ));
        let mut buf = String::new();
        let (name, data) = parse_block(&next_block(&mut body, &mut buf).await);
        assert_eq!(name, "agent_message");
        assert_eq!(data["event_type"], "agent_message");
        assert_eq!(data["project_id"], "p1");
        assert_eq!(data["payload"]["text"], "olá");
        let (name, data) = parse_block(&next_block(&mut body, &mut buf).await);
        assert_eq!(name, "task_updated");
        assert_eq!(data["payload"]["task_id"], "T02");
    }

    #[tokio::test]
    async fn sse_filter_hides_other_projects_but_keeps_global_events() {
        let st = state();
        let resp = router(st.clone())
            .oneshot(sse_get("/api/events?project_id=p1", HOST))
            .await
            .expect("resposta");
        let mut body = resp.into_body();
        st.events().publish(msg(Some("p2"), "de outro projeto"));
        st.events().publish(msg(None, "global"));
        st.events().publish(msg(Some("p1"), "meu"));
        let mut buf = String::new();
        let (_, first) = parse_block(&next_block(&mut body, &mut buf).await);
        assert_eq!(first["project_id"], Value::Null);
        assert_eq!(first["payload"]["text"], "global");
        let (_, second) = parse_block(&next_block(&mut body, &mut buf).await);
        assert_eq!(second["project_id"], "p1");
        assert_eq!(second["payload"]["text"], "meu");
    }

    #[tokio::test]
    async fn sse_lagged_subscriber_gets_lagged_event_and_stream_continues() {
        let st = state();
        let resp = router(st.clone())
            .oneshot(sse_get("/api/events", HOST))
            .await
            .expect("resposta");
        let mut body = resp.into_body();
        for i in 0..(EVENT_CHANNEL_CAPACITY + 100) {
            st.events().publish(msg(None, &format!("n{i}")));
        }
        let mut buf = String::new();
        let (name, data) = parse_block(&next_block(&mut body, &mut buf).await);
        assert_eq!(name, "lagged");
        assert!(data["skipped"].as_u64().is_some_and(|n| n > 0), "{data}");
        st.events().publish(msg(None, "marcador"));
        // O marcador pode provocar novo `lagged` (o assinante ainda não leu o backlog).
        loop {
            let (name, data) = parse_block(&next_block(&mut body, &mut buf).await);
            if name == "lagged" {
                continue;
            }
            assert_eq!(name, "agent_message");
            if data["payload"]["text"] == "marcador" {
                break;
            }
        }
    }

    #[tokio::test]
    async fn invalid_project_id_is_422() {
        let st = state();
        for uri in ["/api/events?project_id=a.b", "/api/events?project_id="] {
            let resp = router(st.clone())
                .oneshot(sse_get(uri, HOST))
                .await
                .expect("resposta");
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{uri}");
        }
        let long = "x".repeat(65);
        let uri = format!("/api/events?project_id={long}");
        let resp = router(st.clone())
            .oneshot(sse_get(&uri, HOST))
            .await
            .expect("resposta");
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn foreign_host_is_forbidden_on_events() {
        let resp = router(state())
            .oneshot(sse_get("/api/events", "evil.com:3000"))
            .await
            .expect("resposta");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn activity_sink_stage_reaches_event_and_snapshot() {
        use crate::agents::activity::ActivityScope;
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let sink = Arc::new(BusActivitySink::new(bus.clone(), "p", Arc::new(SecretSet::default())).with_stage("architecture", "Atualizando ARCHITECTURE.md"));
        let run = ActivityScope::new(sink, "architect", None, None).begin();
        let ev = rx.recv().await.expect("evento");
        assert_eq!(ev.payload["stage"], "Atualizando ARCHITECTURE.md");
        assert_eq!(ev.payload["stage_code"], "architecture");
        assert_eq!(bus.live_activity().snapshot("p")[0].stage.as_deref(), Some("Atualizando ARCHITECTURE.md"));
        drop(run);
    }
    /// Raiz temporária com `.sdd/specs/085/` criada.
    fn trail_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("nants-trail-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".sdd/specs/085")).expect("mkdir");
        root
    }

    fn act(kind: crate::agents::activity::ActivityKind, task: Option<&str>, text: &str) -> crate::agents::activity::ActivityEvent {
        crate::agents::activity::ActivityEvent {
            agent: "developer".into(),
            spec_id: Some("085".into()),
            task_id: task.map(String::from),
            activity_id: "a1".into(),
            kind,
            text: Some(text.into()),
            code: None,
            received_bytes: None,
        }
    }

    fn trail_lines(root: &std::path::Path) -> Vec<Value> {
        std::fs::read_to_string(root.join(".sdd/specs/085/tools.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).expect("json da linha"))
            .collect()
    }

    #[test]
    fn trail_records_tool_and_command_with_published_values() {
        use crate::agents::activity::{ActivityKind, ActivitySink as _};
        let root = trail_root("basic");
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let sink = BusActivitySink::new(bus, "p", Arc::new(SecretSet::default()))
            .with_stage("final_verification", "Verificação final")
            .with_trail(&root, "085");
        sink.emit(act(ActivityKind::Tool, Some("T01"), "vendo a referência tela.png"));
        sink.emit(act(ActivityKind::Command, None, "npm test"));
        let tool_ev = rx.try_recv().expect("evento tool");
        let lines = trail_lines(&root);
        assert_eq!(lines.len(), 2);
        assert!(lines[0]["ts"].as_u64().is_some_and(|t| t > 0));
        assert_eq!(lines[0]["agent"], "developer");
        assert_eq!(lines[0]["task_id"], "T01");
        assert_eq!(lines[0]["kind"], "tool");
        assert_eq!(lines[0]["text"], tool_ev.payload["text"]);
        assert_eq!(lines[0]["stage"], "Verificação final");
        assert!(lines[1]["task_id"].is_null());
        assert_eq!(lines[1]["kind"], "command");
        assert_eq!(lines[1]["text"], "npm test");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Spec 102: `code`/`params` do rótulo e `code` da etapa chegam ao evento, ao instantâneo e à
    /// trilha, com os `params` saneados; `code` fora do formato fica de fora.
    #[test]
    fn activity_codes_reach_event_snapshot_and_trail() {
        use crate::agents::activity::{ActivityCode, ActivityKind, ActivitySink as _};
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = trail_root("codes");
        let _cleanup = Cleanup(root.clone());
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let secrets = Arc::new(SecretSet::new(["chave-super-secreta".to_string()]));
        let sink = BusActivitySink::new(bus.clone(), "p", secrets)
            .with_stage("report", "Escrevendo relatório final")
            .with_trail(&root, "085");
        let mut ev = act(ActivityKind::Tool, Some("T01"), "rodando `curl chave-super-secreta`");
        ev.code = Some(ActivityCode {
            code: "tool.run_command".into(),
            params: [("command".to_string(), json!("curl chave-super-secreta")), ("obj".to_string(), json!({"x": 1}))]
                .into_iter()
                .collect(),
        });
        sink.emit(ev);
        let mut bad = act(ActivityKind::Tool, Some("T01"), "lendo x");
        bad.code = Some(ActivityCode { code: "Tool!".into(), params: serde_json::Map::new() });
        sink.emit(bad);
        let published = rx.try_recv().expect("evento");
        assert_eq!(published.payload["code"], "tool.run_command");
        assert_eq!(published.payload["params"], json!({"command": "curl ***"}));
        assert_eq!(published.payload["stage_code"], "report");
        let snap = bus.live_activity().snapshot("p");
        assert!(snap[0].tool_code.is_none(), "o último rótulo (código inválido) limpa o anterior");
        let lines = trail_lines(&root);
        assert_eq!(lines[0]["code"], "tool.run_command");
        assert_eq!(lines[0]["params"], json!({"command": "curl ***"}));
        assert_eq!(lines[0]["stage_code"], "report");
        assert!(lines[1].get("code").is_none() && lines[1].get("params").is_none());
        assert!(!std::fs::read_to_string(root.join(".sdd/specs/085/tools.jsonl")).unwrap_or_default().contains("chave-super-secreta"));
    }

    #[test]
    fn trail_omits_stage_when_absent_and_skips_other_kinds() {
        use crate::agents::activity::{ActivityKind, ActivitySink as _};
        let root = trail_root("kinds");
        let sink = BusActivitySink::new(EventBus::new(), "p", Arc::new(SecretSet::default())).with_trail(&root, "085");
        for kind in [
            ActivityKind::Start,
            ActivityKind::Reasoning,
            ActivityKind::Text,
            ActivityKind::Heartbeat,
            ActivityKind::CommandEnd,
            ActivityKind::End,
        ] {
            sink.emit(act(kind, Some("T01"), "pensamento do modelo"));
        }
        assert!(trail_lines(&root).is_empty());
        sink.emit(act(ActivityKind::Tool, Some("T01"), "lendo src/x"));
        let lines = trail_lines(&root);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].get("stage").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trail_redacts_secret_in_label() {
        use crate::agents::activity::{ActivityKind, ActivitySink as _};
        let root = trail_root("secret");
        let secrets = Arc::new(SecretSet::new(["sk-SEGREDO-TRILHA-123".to_string()]));
        let sink = BusActivitySink::new(EventBus::new(), "p", secrets).with_trail(&root, "085");
        sink.emit(act(ActivityKind::Command, Some("T01"), "TOKEN=sk-SEGREDO-TRILHA-123 npm test"));
        let raw = std::fs::read_to_string(root.join(".sdd/specs/085/tools.jsonl")).expect("trilha");
        assert!(!raw.contains("SEGREDO"), "{raw}");
        assert_eq!(trail_lines(&root)[0]["text"], "TOKEN=*** npm test");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trail_does_not_follow_symlink() {
        use crate::agents::activity::{ActivityKind, ActivitySink as _};
        let root = trail_root("symlink");
        let outside = root.join("fora.txt");
        std::fs::write(&outside, "").expect("alvo");
        std::os::unix::fs::symlink(&outside, root.join(".sdd/specs/085/tools.jsonl")).expect("ln");
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let sink = BusActivitySink::new(bus, "p", Arc::new(SecretSet::default())).with_trail(&root, "085");
        sink.emit(act(ActivityKind::Tool, Some("T01"), "lendo src/x"));
        assert_eq!(std::fs::read_to_string(&outside).expect("ler"), "");
        assert!(rx.try_recv().is_ok(), "falha da trilha não impede a publicação");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trail_respects_cap() {
        use crate::agents::activity::{ActivityKind, ActivitySink as _};
        let root = trail_root("cap");
        let path = root.join(".sdd/specs/085/tools.jsonl");
        let filler = "x".repeat(usize::try_from(MAX_TOOLS_TRAIL_BYTES).expect("cabe") - 10);
        std::fs::write(&path, &filler).expect("preencher");
        let sink = BusActivitySink::new(EventBus::new(), "p", Arc::new(SecretSet::default())).with_trail(&root, "085");
        sink.emit(act(ActivityKind::Tool, Some("T01"), "lendo src/x"));
        let len = std::fs::metadata(&path).expect("meta").len();
        assert_eq!(len, filler.len() as u64);
        assert!(len <= MAX_TOOLS_TRAIL_BYTES);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trail_off_without_builder_or_with_invalid_spec() {
        use crate::agents::activity::{ActivityKind, ActivitySink as _};
        let root = trail_root("off");
        BusActivitySink::new(EventBus::new(), "p", Arc::new(SecretSet::default()))
            .emit(act(ActivityKind::Tool, Some("T01"), "lendo src/x"));
        assert!(!root.join(".sdd/specs/085/tools.jsonl").exists());
        std::fs::create_dir_all(root.join(".sdd/x")).expect("mkdir");
        BusActivitySink::new(EventBus::new(), "p", Arc::new(SecretSet::default()))
            .with_trail(&root, "../x")
            .emit(act(ActivityKind::Tool, Some("T01"), "lendo src/x"));
        assert!(!root.join(".sdd/x/tools.jsonl").exists());
        assert!(!root.join(".sdd/specs/085/tools.jsonl").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Spec 102: `SystemNotice` vira `agent_message` do sistema com `code` e `params` saneados.
    #[test]
    fn system_notice_becomes_agent_message_with_code() {
        let secrets = SecretSet::new(vec!["sk-SEGREDO".to_string()]);
        let mut params = serde_json::Map::new();
        params.insert("path".into(), json!("web/a.webp sk-SEGREDO"));
        params.insert("count".into(), json!(3));
        params.insert("Bad-Name".into(), json!("x"));
        params.insert("obj".into(), json!({"a": 1}));
        let ev = runner_event_to_api(
            "p1",
            &secrets,
            RunnerEvent::SystemNotice { text: "texto sk-SEGREDO".into(), code: "image.failed".into(), params, task_id: None },
        );
        assert_eq!(ev.event_type(), "agent_message");
        assert_eq!(ev.payload["agent"], "system");
        assert_eq!(ev.payload["code"], "image.failed");
        assert_eq!(ev.payload["params"]["count"], 3);
        let path = ev.payload["params"]["path"].as_str().unwrap_or_default();
        assert!(path.starts_with("web/a.webp") && !path.contains("sk-SEGREDO"), "{path}");
        assert!(ev.payload["params"].get("Bad-Name").is_none() && ev.payload["params"].get("obj").is_none());
        assert!(!ev.payload["text"].as_str().unwrap_or_default().contains("sk-SEGREDO"));
        let invalid = runner_event_to_api(
            "p1",
            &secrets,
            RunnerEvent::SystemNotice { text: "t".into(), code: "Inválido!".into(), params: serde_json::Map::new(), task_id: None },
        );
        assert!(invalid.payload.get("code").is_none() && invalid.payload.get("params").is_none());
        assert!(ev.payload["task_id"].is_null(), "sem tarefa");
        let with_task = runner_event_to_api(
            "p1",
            &secrets,
            RunnerEvent::SystemNotice {
                text: "Tarefa T02 bloqueada".into(),
                code: "task.worktree_dirty".into(),
                params: serde_json::Map::new(),
                task_id: Some("T02".into()),
            },
        );
        assert_eq!(with_task.payload["task_id"], "T02");
        assert_eq!(with_task.payload["code"], "task.worktree_dirty");
        assert!(valid_message_code("run.stopped") && !valid_message_code("1run") && !valid_message_code(""));
    }

    #[test]
    fn normalize_text_strips_terminal_sequences_before_flattening_controls() {
        let vite = "\u{1b}[32m\u{1b}[1mVITE\u{1b}[22m v6.2.2\u{1b}[39m  \u{1b}[2mready in \u{1b}[0m\u{1b}[1m412\u{1b}[22m ms";
        assert_eq!(normalize_text(vite), "VITE v6.2.2  ready in 412 ms");
        assert_eq!(normalize_text("\u{1b}[38;5;208mwarn\u{1b}[0m \u{1b}[48;2;255;100;0mx\u{1b}[m"), "warn x");
        assert_eq!(normalize_text("\u{9b}31mrojo\u{9b}0m"), "rojo");
        assert_eq!(normalize_text("\u{1b}[?25l\u{1b}[2K\u{1b}[1Gok\u{1b}[?25h"), "ok");
        assert_eq!(normalize_text("a\u{1b}]0;titulo\u{7}b\u{1b}]8;;http://x\u{1b}\\c\u{9d}t\u{9c}d"), "abcd");
        assert_eq!(normalize_text("\u{1b}(Bx\u{1b}7y\u{1b}=z"), "xyz");
        // OSC sem terminador para na quebra de linha; ESC solto vira espaço; CSI incompleto some.
        assert_eq!(normalize_text("a\u{1b}]0;sem fim\nlinha"), "a\nlinha");
        assert_eq!(normalize_text("fim\u{1b}"), "fim ");
        assert_eq!(normalize_text("x\u{1b}["), "x");
        assert_eq!(normalize_text("Ø café ➜"), "Ø café ➜");
        let out = normalize_text("\u{1b}[31m\u{1b}[1mFAIL\u{1b}[22m\u{1b}[39m src/a.test.ts\r\n");
        assert_eq!(out, "FAIL src/a.test.ts\n");
        assert!(!out.contains('[') && !out.contains('\u{1b}'));
    }
}
