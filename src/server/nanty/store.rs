//! Estado persistido do Nanty (spec 138 T01) em `<pasta de dados>/nanty/`:
//! - `state.json`: cartões abertos e silêncios (com validade), o gasto do dia, o último envio por
//!   grupo, o que foi feito por último e o que está previsto (gravação atômica, 0600);
//! - `conversation.jsonl`: a conversa única (web e Telegram), em JSONL com rotação;
//! - `journal.jsonl`: o registro, em JSONL com rotação (ver [`super::journal`]).
//!
//! Tudo o que vai para a tela leva `code` + `params` (C-08): a web traduz.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::jsonl::{read_private, write_atomic, JsonlError, JsonlLog};
use crate::config::Urgency;

/// Arquivo do estado.
pub const STATE_FILE: &str = "state.json";
/// Tamanho máximo lido do estado (1 MiB).
const MAX_STATE_BYTES: u64 = 1024 * 1024;
/// Itens "feito" guardados no estado (os mais recentes).
pub const MAX_DONE: usize = 50;
/// Itens guardados para o resumo (os mais recentes).
pub const MAX_DIGEST: usize = 100;
/// Cartões decididos guardados (os mais recentes), para o "já decidido".
pub const MAX_DECIDED: usize = 100;
/// Conversa: tamanho do arquivo atual antes de girar, e gerações antigas guardadas.
const CONVERSATION_MAX_BYTES: u64 = 256 * 1024;
const CONVERSATION_KEEP: usize = 1;
/// Mensagens devolvidas pela leitura da conversa.
pub const CONVERSATION_TAIL: usize = 200;
/// Dias anteriores de gasto guardados (T05), para a janela Custos.
pub const MAX_SPEND_HISTORY: usize = 31;

/// Cartão aberto: um aviso esperando a pessoa (ou uma ação a aprovar, T03), até `expires_at_ms`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Grupo do filtro (projeto e tipo), ex.: `p1:plan_waiting`.
    pub group: String,
    /// Código do porquê (texto do dicionário, C-06).
    pub code: String,
    #[serde(default)]
    pub params: Map<String, Value>,
    /// Ação sugerida do catálogo (T03).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Hash do plano no cartão de aprovação (C-03).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,
    /// Spec da ação (o plano a aprovar, a execução a retomar), T03.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    /// Token aleatório da decisão (T03): só quem tem o cartão decide; vale até `expires_at_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    /// Urgência do aviso, da tabela fixa (T02).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<Urgency>,
    /// Como o aviso sai (T02): o Telegram (T07) segue isto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    /// Confirmação de uma ação pedida na conversa (T08), com o canal onde a pessoa pediu: `web` fica
    /// só na janela; `telegram` vai ao chat logo, sem a espera da aba visível. O turno proativo não
    /// a vê (não é aviso de observador).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<Channel>,
}

/// Como um aviso sai, decidido pelo filtro (T02).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Delivery {
    /// Na hora, com som.
    #[default]
    Push,
    /// Na hora, sem som (urgente no horário de silêncio).
    Silent,
    /// Só no resumo (não urgente no horário de silêncio).
    Digest,
}

/// Condição que um observador está vendo agora (T02), por grupo (projeto e tipo). Só fatos estáveis
/// (contagens e códigos): o que muda a cada rodada fica fora, para a rodada sem novidade não gravar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    /// Tipo do sinal (`plan_waiting`, `disk_low`...).
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default)]
    pub facts: Map<String, Value>,
    pub first_seen_ms: u64,
    /// Hora local (`HH:MM`) em que o observador viu a condição pela primeira vez.
    pub since: String,
    /// Os fatos do último aviso enviado; `None` = ainda não avisado (espera do grupo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notified: Option<Map<String, Value>>,
    /// Calado agora: `inhibited` ou `silenced`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed: Option<String>,
}

/// Item guardado para o resumo (T05 monta o texto).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DigestItem {
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub group: String,
    pub code: String,
    #[serde(default)]
    pub params: Map<String, Value>,
}

/// Silêncio com validade: de um projeto, de um grupo ou de tudo (os dois `None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Silence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub until_ms: u64,
}

/// Gasto do dia local (`day` = `AAAA-MM-DD`): vira zero quando o dia muda.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spend {
    pub day: String,
    pub usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Turnos cujo modelo não tinha preço (o orçamento conta tokens, T05).
    pub unpriced_turns: u64,
    /// Chamadas de voz do dia (a fala e a transcrição, T09); os tokens e o US$ delas já estão nos
    /// totais acima.
    pub voice_calls: u64,
}

/// Quem decidiu uma ação.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecidedBy {
    Auto,
    Telegram,
    Web,
}

/// Algo que o Nanty fez (ou a pessoa decidiu), para o "feito" do feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoneItem {
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub code: String,
    #[serde(default)]
    pub params: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<DecidedBy>,
    /// Resultado em código (`ok`, `failed`, `stale`, `resolved`...).
    pub result: String,
    /// Cartão fechado por este item (o resolvido, T02).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_id: Option<String>,
    /// Desfazer disponível (T03): a troca de conexão, até `expires_at_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<UndoRef>,
}

/// Referência ao desfazer de um "feito" (T03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoRef {
    pub id: String,
    pub token: String,
    pub expires_at_ms: u64,
}

/// Uma conexão e um modelo gravados num papel (o antes e o depois da troca, T03).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteValue {
    pub connection_id: String,
    pub model: String,
}

/// Desfazer guardado (T03): a troca da conexão de um papel volta ao valor anterior só se o valor
/// atual ainda for o que o Nanty gravou (comparar e trocar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoEntry {
    pub id: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub action: String,
    pub role: String,
    pub previous: RouteValue,
    pub applied: RouteValue,
    pub at_ms: u64,
    pub expires_at_ms: u64,
}

/// Cartão já decidido (T03): o segundo clique, na web ou no Telegram, recebe "já decidido por".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecidedCard {
    pub card_id: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub decided_by: DecidedBy,
    /// `yes`, `no` ou `later`; `undo` para o desfazer.
    pub answer: String,
    pub at_ms: u64,
}

/// Oferta (ação do catálogo e os fatos que a motivaram, T03): depois de "não" ou de feita, a mesma
/// oferta só volta com um fato novo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub action: String,
    pub fingerprint: String,
}

/// Novas tentativas feitas numa parada (T03), por `<projeto>:<spec>:<código>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryCount {
    pub count: u32,
    pub last_ms: u64,
}

/// Ação prevista (o que o Nanty fará sozinho se nada mudar), para o "previsto" do feed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Grupo da condição que motivou a ação (T03).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub action: String,
    pub code: String,
    #[serde(default)]
    pub params: Map<String, Value>,
    pub at_ms: u64,
}

/// Cartões enviados ao Telegram guardados (os mais recentes).
pub const MAX_SENT: usize = 200;

/// Cartão enviado ao Telegram (T07): o `message_id` para a edição depois da decisão, do resolvido
/// ou do vencimento. `message_id` vazio = o cartão saiu junto num resumo da fila (sem botões) ou o
/// Telegram recusou o envio: não há o que editar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentCard {
    pub card_id: String,
    pub chat_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    /// O `created_at_ms` do cartão enviado: a renovação (repetição) manda uma mensagem nova.
    pub created_at_ms: u64,
    /// A mensagem ainda tem os botões.
    #[serde(default)]
    pub buttons: bool,
    /// O texto enviado (já redigido), base da edição.
    #[serde(default)]
    pub text: String,
    pub at_ms: u64,
}

/// O estado de `state.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NantyState {
    pub cards: Vec<Card>,
    pub silences: Vec<Silence>,
    pub spend: Spend,
    /// Último envio por grupo (ms), para segurar a repetição (T02/T07).
    pub last_sent: BTreeMap<String, u64>,
    pub done: Vec<DoneItem>,
    pub planned: Vec<PlannedItem>,
    /// Condições vistas agora, por grupo (T02).
    pub alerts: BTreeMap<String, Alert>,
    /// Itens à espera do resumo (T02 enfileira, T05 consome).
    pub digest: Vec<DigestItem>,
    /// Cartões decididos (T03), os mais recentes.
    pub decided: Vec<DecidedCard>,
    /// Ofertas recusadas por grupo (T03).
    pub declined: BTreeMap<String, Offer>,
    /// Ofertas já feitas por grupo (T03): a mesma não se repete sem fato novo.
    pub handled: BTreeMap<String, Offer>,
    /// Novas tentativas por parada (T03).
    pub retries: BTreeMap<String, RetryCount>,
    /// Desfazer disponível (T03).
    pub undo: Vec<UndoEntry>,
    /// Gasto dos dias anteriores (T05), do mais velho para o mais novo, até [`MAX_SPEND_HISTORY`].
    pub spend_history: Vec<Spend>,
    /// Cartões criados até este instante já passaram pelo turno proativo (T05).
    pub turned_until_ms: u64,
    /// Dia local (`AAAA-MM-DD`) do último resumo da manhã (T05): sai uma vez por dia.
    pub digest_day: String,
    /// Cartões enviados ao Telegram (T07), até [`MAX_SENT`].
    pub telegram_sent: Vec<SentCard>,
}

/// Prefixo do grupo de um projeto (`<id>:`): o grupo nasce do projeto e do tipo.
pub(super) fn group_of_project(group: &str, project_id: &str) -> bool {
    group.strip_prefix(project_id).is_some_and(|rest| rest.starts_with(':'))
}

impl NantyState {
    /// Tira o que venceu (cartões e silêncios) em `now_ms`. `true` se mudou.
    pub fn prune_expired(&mut self, now_ms: u64) -> bool {
        let before = (self.cards.len(), self.silences.len(), self.undo.len());
        self.cards.retain(|c| c.expires_at_ms > now_ms);
        self.silences.retain(|s| s.until_ms > now_ms);
        self.undo.retain(|u| u.expires_at_ms > now_ms);
        before != (self.cards.len(), self.silences.len(), self.undo.len())
    }

    /// Tira tudo o que é do projeto (cartões, silêncios, últimos envios, feitos e previstos).
    /// `true` se mudou.
    pub fn forget_project(&mut self, project_id: &str) -> bool {
        let before = self.clone();
        let mine = |p: &Option<String>| p.as_deref() == Some(project_id);
        self.cards.retain(|c| !mine(&c.project_id) && !group_of_project(&c.group, project_id));
        self.silences.retain(|s| {
            !mine(&s.project_id) && !s.group.as_deref().is_some_and(|g| group_of_project(g, project_id))
        });
        self.last_sent.retain(|g, _| !group_of_project(g, project_id));
        self.done.retain(|d| !mine(&d.project_id));
        self.planned.retain(|p| !mine(&p.project_id));
        self.alerts.retain(|g, a| !mine(&a.project_id) && !group_of_project(g, project_id));
        self.digest.retain(|d| !mine(&d.project_id) && !group_of_project(&d.group, project_id));
        self.decided.retain(|d| !mine(&d.project_id));
        self.declined.retain(|g, _| !group_of_project(g, project_id));
        self.handled.retain(|g, _| !group_of_project(g, project_id));
        self.retries.retain(|k, _| !group_of_project(k, project_id));
        self.undo.retain(|u| !mine(&u.project_id));
        *self != before
    }

    /// Tira o que é de projetos fora de `known` (varredura completa). `true` se mudou.
    pub fn retain_projects(&mut self, known: &[String]) -> bool {
        let mut ids: Vec<String> = Vec::new();
        let mut note = |p: &Option<String>| {
            if let Some(id) = p {
                if !known.contains(id) && !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        };
        self.cards.iter().for_each(|c| note(&c.project_id));
        self.silences.iter().for_each(|s| note(&s.project_id));
        self.done.iter().for_each(|d| note(&d.project_id));
        self.planned.iter().for_each(|p| note(&p.project_id));
        self.alerts.values().for_each(|a| note(&a.project_id));
        self.digest.iter().for_each(|d| note(&d.project_id));
        self.decided.iter().for_each(|d| note(&d.project_id));
        self.undo.iter().for_each(|u| note(&u.project_id));
        let mut changed = false;
        for id in ids {
            changed |= self.forget_project(&id);
        }
        changed
    }

    /// Gasto do dia `day`: o guardado se for do mesmo dia, senão zero.
    pub fn spend_on(&self, day: &str) -> Spend {
        if self.spend.day == day {
            self.spend.clone()
        } else {
            Spend { day: day.to_string(), ..Spend::default() }
        }
    }

    /// Soma um turno do modelo ao gasto do dia `day` (T05): o dia anterior vai para o histórico.
    /// `usd = None` é o modelo sem preço (ou pelo plano): conta só os tokens.
    pub fn add_spend(&mut self, day: &str, input: u64, output: u64, usd: Option<f64>) {
        if self.spend.day != day {
            let previous = std::mem::replace(&mut self.spend, Spend { day: day.to_string(), ..Spend::default() });
            if !previous.day.is_empty() {
                self.spend_history.retain(|s| s.day != previous.day);
                self.spend_history.push(previous);
                let extra = self.spend_history.len().saturating_sub(MAX_SPEND_HISTORY);
                self.spend_history.drain(..extra);
            }
        }
        self.spend.input_tokens = self.spend.input_tokens.saturating_add(input);
        self.spend.output_tokens = self.spend.output_tokens.saturating_add(output);
        match usd.filter(|v| v.is_finite() && *v >= 0.0) {
            Some(v) => self.spend.usd += v,
            None => self.spend.unpriced_turns = self.spend.unpriced_turns.saturating_add(1),
        }
    }

    /// Soma uma chamada de voz (T09) ao gasto do dia: os tokens e o US$ entram no mesmo orçamento
    /// dos turnos, e a chamada conta em `voice_calls`.
    pub fn add_voice_spend(&mut self, day: &str, input: u64, output: u64, usd: Option<f64>) {
        self.add_spend(day, input, output, usd);
        self.spend.voice_calls = self.spend.voice_calls.saturating_add(1);
    }

    /// Gasto de um dia qualquer: o de hoje, o do histórico ou zero.
    pub fn spend_of_day(&self, day: &str) -> Spend {
        if self.spend.day == day {
            return self.spend.clone();
        }
        self.spend_history
            .iter()
            .find(|s| s.day == day)
            .cloned()
            .unwrap_or_else(|| Spend { day: day.to_string(), ..Spend::default() })
    }

    /// Guarda um "feito", mantendo só os [`MAX_DONE`] mais recentes.
    pub fn push_done(&mut self, item: DoneItem) {
        self.done.push(item);
        let extra = self.done.len().saturating_sub(MAX_DONE);
        self.done.drain(..extra);
    }

    /// Guarda um cartão decidido, mantendo só os [`MAX_DECIDED`] mais recentes.
    pub fn push_decided(&mut self, item: DecidedCard) {
        self.decided.push(item);
        let extra = self.decided.len().saturating_sub(MAX_DECIDED);
        self.decided.drain(..extra);
    }

    /// Guarda (ou troca) o envio de um cartão ao Telegram, mantendo só os [`MAX_SENT`] mais
    /// recentes.
    pub fn put_sent(&mut self, item: SentCard) {
        self.telegram_sent.retain(|s| s.card_id != item.card_id);
        self.telegram_sent.push(item);
        let extra = self.telegram_sent.len().saturating_sub(MAX_SENT);
        self.telegram_sent.drain(..extra);
    }

    /// Guarda um item do resumo, mantendo só os [`MAX_DIGEST`] mais recentes.
    pub fn push_digest(&mut self, item: DigestItem) {
        self.digest.push(item);
        let extra = self.digest.len().saturating_sub(MAX_DIGEST);
        self.digest.drain(..extra);
    }
}

/// Lado de uma mensagem da conversa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    Person,
    Nanty,
}

/// Canal por onde a mensagem passou.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Web,
    Telegram,
}

/// Mensagem da conversa única. A da pessoa traz `text`; a do Nanty traz `code` + `params` (o texto
/// fixo, traduzido pela web) e, quando o modelo respondeu, `text` como veio, com o idioma em `lang`
/// (T05, C-08).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: String,
    pub at_ms: u64,
    pub from: Speaker,
    pub channel: Channel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    /// Idioma do `text` do modelo (`pt-BR` ou `en`), T05.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Cartões a que a mensagem se refere (o turno proativo, T05).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card_ids: Vec<String>,
    /// Ids do catálogo que o modelo sugeriu, já validados no servidor (T05).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    /// Resposta à mensagem da pessoa com este id (T08): sai pelo canal onde ela escreveu (a da
    /// janela não vai ao Telegram).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// A pessoa falou por nota de voz (T09): o `text` é a transcrição, já redigida.
    #[serde(default, skip_serializing_if = "is_false")]
    pub voice: bool,
    /// O áudio da mensagem (T09): a nota de voz da pessoa ou a fala do Nanty, servido à janela por
    /// `GET /api/nanty/audio/:id` enquanto a mensagem estiver na conversa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioRef>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Um áudio da conversa (T09), em `<pasta do Nanty>/audio/<id>.ogg` (Opus em OGG).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioRef {
    /// 16 hex (o `new_id`): o nome do arquivo.
    pub id: String,
    /// Duração em segundos (arredondada para cima).
    pub duration_secs: u32,
    pub bytes: u64,
}

impl ConversationMessage {
    /// Mensagem do Nanty com o texto fixo do dicionário (`code` + `params`).
    pub fn from_nanty(id: String, at_ms: u64, channel: Channel, code: &str, params: Map<String, Value>) -> Self {
        Self {
            id,
            at_ms,
            from: Speaker::Nanty,
            channel,
            project_id: None,
            text: None,
            code: Some(code.to_string()),
            params,
            lang: None,
            card_ids: Vec::new(),
            actions: Vec::new(),
            reply_to: None,
            voice: false,
            audio: None,
        }
    }
}

/// Arquivos do Nanty numa pasta (`<pasta de dados>/nanty`).
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
    /// A conversa: uma única instância, com o mutex do arquivo.
    conversation: JsonlLog,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        let conversation = JsonlLog::new(dir.clone(), "conversation", CONVERSATION_MAX_BYTES, CONVERSATION_KEEP);
        Self { dir, conversation }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Lê o estado; ausente ou inválido = vazio.
    pub fn load(&self) -> NantyState {
        read_private(&self.dir.join(STATE_FILE), MAX_STATE_BYTES)
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Grava o estado de forma atômica.
    pub fn save(&self, state: &NantyState) -> Result<(), JsonlError> {
        let body = serde_json::to_vec_pretty(state).map_err(|_| JsonlError)?;
        write_atomic(&self.dir.join(STATE_FILE), &body)
    }

    /// A conversa (JSONL com rotação).
    pub fn conversation(&self) -> &JsonlLog {
        &self.conversation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(id: &str, project: Option<&str>, group: &str, expires: u64) -> Card {
        Card {
            id: id.into(),
            project_id: project.map(Into::into),
            group: group.into(),
            code: "nanty.why.plan_waiting".into(),
            params: Map::new(),
            created_at_ms: 0,
            expires_at_ms: expires,
            ..Card::default()
        }
    }

    fn done(project: Option<&str>, at: u64) -> DoneItem {
        DoneItem {
            at_ms: at,
            project_id: project.map(Into::into),
            code: "c".into(),
            params: Map::new(),
            action: Some("retry_provider".into()),
            decided_by: Some(DecidedBy::Auto),
            result: "ok".into(),
            card_id: None,
            undo: None,
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nants-nanty-store-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn sample() -> NantyState {
        let mut st = NantyState::default();
        st.cards.push(card("a", Some("p1"), "p1:plan_waiting", 100));
        st.cards.push(card("b", Some("p2"), "p2:blocked", 200));
        st.cards.push(card("c", None, "system:disk", 300));
        st.silences.push(Silence { project_id: Some("p1".into()), group: None, until_ms: 500 });
        st.silences.push(Silence { project_id: None, group: Some("p1:cost".into()), until_ms: 500 });
        st.silences.push(Silence { project_id: None, group: None, until_ms: 50 });
        st.last_sent.insert("p1:plan_waiting".into(), 10);
        st.last_sent.insert("p10:plan_waiting".into(), 10);
        st.done.push(done(Some("p1"), 1));
        st.done.push(done(Some("p2"), 2));
        st.planned.push(PlannedItem { project_id: Some("p1".into()), group: None, action: "a".into(), code: "c".into(), params: Map::new(), at_ms: 9 });
        st.spend = Spend { day: "2026-10-30".into(), usd: 0.12, input_tokens: 10, output_tokens: 5, unpriced_turns: 0, voice_calls: 0 };
        st
    }

    #[test]
    fn state_round_trips_on_disk_with_private_mode() {
        let d = dir("roundtrip");
        let store = Store::new(d.clone());
        assert_eq!(store.load(), NantyState::default(), "ausente = vazio");
        let st = sample();
        store.save(&st).expect("save");
        assert_eq!(store.load(), st);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.join(STATE_FILE)).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::write(d.join(STATE_FILE), "{quebrado").expect("write");
        assert_eq!(store.load(), NantyState::default(), "inválido = vazio");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn expired_cards_and_silences_leave() {
        let mut st = sample();
        assert!(st.prune_expired(150));
        assert_eq!(st.cards.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert_eq!(st.silences.len(), 2);
        assert!(!st.prune_expired(150), "nada mais a tirar");
    }

    #[test]
    fn removed_project_leaves_everything_it_owned() {
        let mut st = sample();
        assert!(st.forget_project("p1"));
        assert_eq!(st.cards.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert_eq!(st.silences, [Silence { project_id: None, group: None, until_ms: 50 }]);
        assert_eq!(st.last_sent.keys().collect::<Vec<_>>(), ["p10:plan_waiting"], "p10 não é p1");
        assert_eq!(st.done.len(), 1);
        assert!(st.planned.is_empty());
        assert!(!st.forget_project("p1"));
        let mut full = sample();
        assert!(full.retain_projects(&["p1".into()]));
        assert!(full.cards.iter().all(|c| c.project_id.as_deref() != Some("p2")));
    }

    #[test]
    fn spend_resets_on_a_new_day_and_done_is_bounded() {
        let st = sample();
        assert!((st.spend_on("2026-10-30").usd - 0.12).abs() < 1e-9);
        assert_eq!(st.spend_on("2026-10-31"), Spend { day: "2026-10-31".into(), ..Spend::default() });
        let mut st = NantyState::default();
        for n in 0..(MAX_DONE as u64 + 5) {
            st.push_done(done(None, n));
        }
        assert_eq!(st.done.len(), MAX_DONE);
        assert_eq!(st.done[0].at_ms, 5);
    }

    #[test]
    fn spend_moves_to_history_on_a_new_day_and_unpriced_counts_tokens() {
        let mut st = NantyState::default();
        st.add_spend("2026-10-29", 100, 20, Some(0.01));
        st.add_spend("2026-10-29", 50, 10, None);
        assert_eq!((st.spend.input_tokens, st.spend.output_tokens, st.spend.unpriced_turns), (150, 30, 1));
        assert!((st.spend.usd - 0.01).abs() < 1e-9);
        st.add_spend("2026-10-30", 1, 1, Some(0.002));
        assert_eq!(st.spend.day, "2026-10-30");
        assert_eq!(st.spend_history.len(), 1);
        assert_eq!(st.spend_of_day("2026-10-29").input_tokens, 150);
        assert_eq!(st.spend_of_day("2026-10-28"), Spend { day: "2026-10-28".into(), ..Spend::default() });
        for d in 1..=40u32 {
            st.add_spend(&format!("2027-01-{d:02}"), 1, 0, None);
        }
        assert_eq!(st.spend_history.len(), MAX_SPEND_HISTORY);
    }

    #[test]
    fn conversation_is_jsonl_and_keeps_code_and_params() {
        let d = dir("conversation");
        let store = Store::new(d.clone());
        let log = store.conversation();
        let msg = ConversationMessage {
            id: "m1".into(),
            at_ms: 1,
            from: Speaker::Nanty,
            channel: Channel::Telegram,
            project_id: Some("p1".into()),
            text: None,
            code: Some("nanty.why.plan_waiting".into()),
            params: json!({"count": 2}).as_object().cloned().unwrap_or_default(),
            lang: None,
            card_ids: Vec::new(),
            actions: Vec::new(),
            reply_to: None,
            voice: false,
            audio: None,
        };
        log.append(&msg).expect("append");
        let back: Vec<ConversationMessage> = log.tail(CONVERSATION_TAIL);
        assert_eq!(back, [msg]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
