//! Estado persistido do Nanty (spec 138 T01) em `<pasta de dados>/nanty/`:
//! - `state.json`: cartões abertos e silêncios (com validade), o gasto do dia, o último envio por
//!   grupo, o que foi feito por último e o que está previsto (gravação atômica, 0600);
//! - `conversation.jsonl`: a conversa única (web e Telegram), em JSONL com rotação ([`Conversation`]);
//! - `summary.json`: o resumo da conversa, em arquivo próprio (ver [`super::summary`]);
//! - `audio/`: os áudios da conversa (ver [`super::voice`]);
//! - `journal.jsonl`: o registro, em JSONL com rotação (ver [`super::journal`]).
//!
//! Tudo o que vai para a tela leva `code` + `params` (C-08): a web traduz.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::jsonl::{read_private, write_atomic, JsonlError, JsonlLog, Limits, Release};
use super::provenance::Taint;
use super::summary::{Summary, SummaryItem, SummarySource, SummaryStore};
use crate::config::{NantyStorage, Urgency};

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

impl Card {
    /// O cartão espera uma decisão da pessoa. O botão de desligar o silêncio (spec 139 T04) é um
    /// controle, não uma decisão pendente: fica fora de "Esperando você", do `/status`, do resumo, do
    /// selo e do avatar "esperando".
    pub fn awaits_person(&self) -> bool {
        // Spec 140 T07: o botão de apagar do rodapé "guardei isto" também é um controle.
        !matches!(self.action.as_deref(), Some("end_silence" | "memory_forget"))
    }
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
    /// O que as análises gastaram no dia (spec 140 T12, C-19), já somado nos totais acima: a fatia do
    /// cenário proativo é conferida por aqui.
    pub analysis_usd: f64,
    pub analysis_tokens: u64,
}

/// Um caso já analisado pelo cenário proativo (spec 140 T12, C-14), no modelo de `handled`: a chave
/// `projeto:spec:tarefa:sinal:impressão dos fatos`, o dia local e o instante em que a análise
/// começou. Vale para os tetos por dia e por spec, e sobrevive a reinício.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioMark {
    pub key: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_id: Option<String>,
    pub day: String,
    pub at_ms: u64,
}

impl super::provenance::Scoped for ScenarioMark {
    fn projects(&self) -> Vec<&str> {
        vec![self.project_id.as_str()]
    }
}

/// Casos analisados guardados (os mais recentes).
pub const MAX_SCENARIOS: usize = 200;

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

/// Como o cartão foi ao Telegram (spec 139 T08, C-14): a mensagem de texto é editada pelo
/// `editMessageText`, e a nota de voz, pelo `editMessageCaption`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SentKind {
    #[default]
    Text,
    Voice,
}

impl SentKind {
    fn is_text(&self) -> bool {
        *self == SentKind::Text
    }
}

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
    /// O texto enviado (já redigido), base da edição. Na nota que falou o texto do modelo (spec 140
    /// T02), esse texto e o porquê do observador.
    #[serde(default)]
    pub text: String,
    /// O porquê que fecha o `text` quando há texto do modelo antes dele: a edição corta antes dele.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,
    pub at_ms: u64,
    /// Texto ou nota de voz (spec 139 T08). Quem foi gravado antes dela é texto.
    #[serde(default, skip_serializing_if = "SentKind::is_text")]
    pub kind: SentKind,
    /// O botão do cartão foi junto da resposta que o cita (spec 140 T07: o "apagar" da memória), e não
    /// numa mensagem própria: ao sair o cartão, só o botão some, e o texto da resposta fica.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub embedded: bool,
    /// O link do Nants que a mesma resposta levou (spec 140 T07, revisão 1, F-02): tirar o botão de
    /// apagar mantém o dele.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// O estado de `state.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NantyState {
    /// O último `Host` aceito vindo de um aparelho autorizado (spec 140 T03, C-30): o endereço que o
    /// celular alcança, sem o esquema. Só um IP da rede ([`super::link::device_host`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_host: Option<String>,
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
    /// Cartões longos demais para o Telegram, que saíram sem o "sim" e apontando a janela (spec 140
    /// T17, da revisão 2 da T07): o "não" leva o mesmo token, então um "sim" que chegue pelo Telegram
    /// é recusado ([`Self::window_only`]). À parte de [`Self::telegram_sent`], que tem teto e é
    /// esquecido ao fim do cartão: a marca vale enquanto o cartão viver, e sai com ele.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub window_only_cards: Vec<String>,
    /// Último idioma em que a pessoa falou (`pt-BR` ou `en`; spec 139 T05): o que o texto dela
    /// mostrou ou, sem sinal no texto, o `language_code` do Telegram. Base do "seguir a conversa".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_lang: Option<String>,
    /// O outro idioma visto na última mensagem, com uma pista só (spec 139 T12): a próxima pista
    /// dele confirma a troca; uma mensagem que não a confirma zera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_lang_pending: Option<String>,
    /// Último `language_code` do dono no Telegram, já como `pt-BR` ou `en` (spec 139 T05).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telegram_lang: Option<String>,
    /// Casos já analisados pelo cenário proativo (spec 140 T12), até [`MAX_SCENARIOS`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<ScenarioMark>,
    /// Os rascunhos que o Nanty deixou na caixa do Arquiteto (spec 140 T15, C-15): um por projeto, até
    /// a pessoa enviar ou descartar.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub architect_drafts: Vec<super::draft::ArchitectDraft>,
}

/// Prefixo do grupo de um projeto (`<id>:`): o grupo nasce do projeto e do tipo.
pub(super) fn group_of_project(group: &str, project_id: &str) -> bool {
    group.strip_prefix(project_id).is_some_and(|rest| rest.starts_with(':'))
}

/// Tira `attempts: 0` dos fatos (spec 139 T02: zero tentativas não é fato). `true` se mudou.
fn drop_zero_attempts(facts: &mut Map<String, Value>) -> bool {
    let zero = facts.get("attempts").is_some_and(|v| v.as_u64() == Some(0));
    if zero {
        facts.remove("attempts");
    }
    zero
}

impl NantyState {
    /// Os avisos gravados antes da spec 139 T02 levam `attempts: 0` nos fatos e no que foi avisado;
    /// o observador de agora não emite mais esse fato. Sem normalizar, a primeira varredura depois de
    /// atualizar veria fatos novos e reenviaria o mesmo aviso como atualização. `true` se mudou.
    pub fn normalize_legacy_facts(&mut self) -> bool {
        let mut changed = false;
        for alert in self.alerts.values_mut() {
            changed |= drop_zero_attempts(&mut alert.facts);
            if let Some(notified) = alert.notified.as_mut() {
                changed |= drop_zero_attempts(notified);
            }
        }
        changed
    }

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
        // Spec 140 T12: os casos analisados do projeto saem com ele.
        self.scenarios.retain(|m| !super::provenance::Scoped::touches(m, project_id));
        // Spec 140 T15: o rascunho do Arquiteto, por todos os projetos dele (o destino e a fonte).
        self.architect_drafts.retain(|d| !super::provenance::Scoped::touches(d, project_id));
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
        self.scenarios.iter().for_each(|m| note(&Some(m.project_id.clone())));
        for d in &self.architect_drafts {
            super::provenance::Scoped::projects(d).into_iter().for_each(|id| note(&Some(id.to_string())));
        }
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

    /// Anota a parte das análises no gasto do dia (spec 140 T12, C-19): os mesmos tokens e US$ que
    /// [`Self::add_spend`] já somou, para a fatia do cenário proativo.
    pub fn add_analysis_spend(&mut self, day: &str, tokens: u64, usd: Option<f64>) {
        if self.spend.day != day {
            return;
        }
        self.spend.analysis_tokens = self.spend.analysis_tokens.saturating_add(tokens);
        if let Some(v) = usd.filter(|v| v.is_finite() && *v >= 0.0) {
            self.spend.analysis_usd += v;
        }
    }

    /// Guarda um caso analisado, mantendo só os [`MAX_SCENARIOS`] mais recentes.
    pub fn push_scenario(&mut self, mark: ScenarioMark) {
        self.scenarios.push(mark);
        let extra = self.scenarios.len().saturating_sub(MAX_SCENARIOS);
        self.scenarios.drain(..extra);
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
    /// Marca o cartão como decidido só na janela, tirando as marcas de cartões que já saíram.
    pub fn mark_window_only(&mut self, card_id: &str) {
        let cards = &self.cards;
        self.window_only_cards.retain(|id| id != card_id && cards.iter().any(|c| c.id == *id));
        self.window_only_cards.push(card_id.to_string());
    }

    /// O "sim" deste cartão só vale na janela: ele saiu ao Telegram sem o "sim" e ainda vive.
    pub fn window_only(&self, card_id: &str) -> bool {
        self.window_only_cards.iter().any(|id| id == card_id) && self.cards.iter().any(|c| c.id == card_id)
    }

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
    /// A mensagem da pessoa traz texto de terceiro (spec 139 T04, C-07): encaminhada ou com a
    /// citação de outra pessoa. Vai ao modelo como dado não confiável e contamina o turno.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relayed: Option<Relayed>,
    /// Todos os projetos ligados à mensagem (spec 140 T04, C-16): a limpeza de projeto e a exclusão de
    /// observação valem para todos ([`super::provenance::Scoped`]). O `project_id` segue só para o
    /// caso de um projeto, e nas linhas antigas ele é a única marca.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_ids: Vec<String>,
    /// Procedência da mensagem do Nanty (spec 140 T04, C-01). Toda mensagem nova do Nanty a grava,
    /// mesmo limpa; `None` nas do Nanty é linha antiga, que conta como contaminada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taint: Option<Taint>,
    /// Número de sequência da linha (spec 140 T05, revisão 1, F-03): crescente, dado por
    /// [`Conversation::append`] sob o mutex do arquivo, e a base da marca do resumo. Não depende do
    /// relógio, e a hora mostrada continua a de `at_ms`. A segunda linha de um id (o áudio) leva o
    /// número da primeira. `None` = linha de antes da T05 ainda sem número: nunca coberta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Texto de terceiro numa mensagem da pessoa (spec 139 T04, C-07).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relayed {
    /// A mensagem inteira foi encaminhada (`forward_origin`): o texto dela é de terceiro.
    #[serde(default, skip_serializing_if = "is_false")]
    pub forwarded: bool,
    /// A pessoa citou ou respondeu a mensagem de outra pessoa (`quote`, `reply_to_message` de
    /// terceiro ou `external_reply`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub third_party: bool,
    /// O trecho citado do terceiro, já redigido, quando o Telegram o manda.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
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

/// Funde as linhas da conversa com o mesmo id (spec 139 T08): o arquivo só recebe acréscimos, então a
/// mensagem que ganha o áudio depois volta como uma linha nova. A última linha vence, e a posição
/// continua a da primeira.
pub fn merged_lines(lines: Vec<ConversationMessage>) -> Vec<ConversationMessage> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out: Vec<ConversationMessage> = Vec::with_capacity(lines.len());
    for line in lines {
        match seen.get(&line.id).and_then(|at| out.get_mut(*at)) {
            Some(first) => *first = line,
            None => {
                seen.insert(line.id.clone(), out.len());
                out.push(line);
            }
        }
    }
    out
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
            relayed: None,
            project_ids: Vec::new(),
            taint: Some(Taint::default()),
            seq: None,
        }
    }
}

/// Quem resume a conversa (a T06 liga e desliga): decide se a rotação pode tirar uma linha que o
/// resumo ainda não cobriu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Summarizer {
    /// O resumo roda: a linha ainda não resumida fica no arquivo até entrar nele.
    Active,
    /// O resumo está desligado por falta de orçamento (ou sem modelo): a linha sai na rotação, e no
    /// lugar dela entra a linha do servidor no resumo, só com campos estruturados.
    Off,
}

/// O que a rotação da conversa fez fora do comum, para o registro (só códigos e números).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationNote {
    /// Mensagens que saíram sem resumo, com a linha do servidor (`reason`: `summary_off` ou
    /// `ceiling`, o limite fixo de gerações).
    Unsummarized { messages: usize, reason: &'static str },
    /// A geração mais velha ficou além dos limites: o resumo ainda não a cobriu.
    Held,
    /// O resumo não pôde ser gravado (o código do erro): a geração ficou.
    SummaryFailed(&'static str),
    /// A geração mais velha existe, mas não deu para ler: ela ficou (só o limite fixo a tira).
    Unreadable,
}

/// Índice dos áudios citados pela conversa (C-20): a leitura de um áudio não relê a conversa toda.
/// Vale enquanto nenhuma linha sair do arquivo ([`JsonlLog::removals`]); o acréscimo entra nele.
#[derive(Debug)]
struct AudioIndex {
    removals: u64,
    ids: BTreeSet<String>,
}

/// A conversa (JSONL com rotação), o resumo que segura a rotação e o índice dos áudios. Uma única
/// instância por pasta (os clones dividem tudo).
///
/// **Rotação (spec 140 T05, C-20):** o arquivo atual gira em `[nanty.storage]
/// conversation_max_bytes` e guarda `conversation_generations` gerações antigas, com o teto
/// `conversation_total_bytes`. A geração mais velha só sai quando todas as linhas dela estão
/// cobertas pela marca do resumo ([`Summary::covers`]). Com o resumo desligado ([`Summarizer::Off`]),
/// ela sai, e as linhas não resumidas viram a linha do servidor no resumo. Com o resumo ligado, ela
/// fica, mesmo acima dos limites, até o fixo [`super::jsonl::HARD_MAX_GENERATIONS`].
#[derive(Debug, Clone)]
pub struct Conversation {
    log: JsonlLog,
    summary: SummaryStore,
    summarizer: Arc<Mutex<Summarizer>>,
    audio: Arc<Mutex<Option<AudioIndex>>>,
    /// Quantas vezes o índice dos áudios foi montado lendo a conversa.
    index_builds: Arc<AtomicU64>,
    /// A sequência das linhas; o mutex também põe os acréscimos em fila.
    seq: Arc<Mutex<SeqState>>,
}

/// Ids recentes guardados com o número, para a segunda linha de um id (o áudio) levar o da primeira.
const RECENT_IDS: usize = 1024;

/// Espera entre duas tentativas de numerar a conversa com uma geração ilegível (spec 140 T17).
const SEQ_RETRY: Duration = Duration::from_secs(60);

/// A sequência das linhas da conversa: `next == 0` = ainda não lida do arquivo.
#[derive(Debug)]
struct SeqState {
    next: u64,
    recent: VecDeque<(String, u64)>,
    /// A última numeração que falhou por uma geração ilegível: o instante e o `removals` do arquivo
    /// naquela hora. A próxima só tenta depois de [`Self::retry`] ou de alguma linha sair do arquivo.
    failed: Option<(Instant, u64)>,
    /// A espera entre as tentativas ([`SEQ_RETRY`]; os testes baixam).
    retry: Duration,
    /// Quantas vezes a conversa foi lida para numerar (testes).
    attempts: u64,
}

impl Default for SeqState {
    fn default() -> Self {
        Self { next: 0, recent: VecDeque::new(), failed: None, retry: SEQ_RETRY, attempts: 0 }
    }
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Os limites da rotação na configuração.
pub fn conversation_limits(storage: &NantyStorage) -> Limits {
    Limits { max_bytes: storage.conversation_max(), keep: storage.generations(), total_bytes: storage.conversation_total() }
}

impl Conversation {
    fn new(dir: PathBuf, summary: SummaryStore) -> Self {
        Self {
            log: JsonlLog::with_limits(dir, "conversation", conversation_limits(&NantyStorage::default())),
            summary,
            // Até a T06 ligar o resumo, ele está desligado: a rotação deixa a linha do servidor.
            summarizer: Arc::new(Mutex::new(Summarizer::Off)),
            audio: Arc::new(Mutex::new(None)),
            index_builds: Arc::new(AtomicU64::new(0)),
            seq: Arc::new(Mutex::new(SeqState::default())),
        }
    }

    /// Baixa o teto de leitura das gerações (testes).
    #[cfg(test)]
    pub fn set_read_cap(&self, cap: u64) {
        self.log.set_read_cap(cap);
    }

    /// Troca a espera entre as tentativas de numerar (testes).
    #[cfg(test)]
    pub fn set_seq_retry(&self, retry: Duration) {
        locked(&self.seq).retry = retry;
    }

    /// Quantas vezes a conversa foi lida para numerar as linhas (testes).
    #[cfg(test)]
    pub fn seq_attempts(&self) -> u64 {
        locked(&self.seq).attempts
    }

    /// Caminho da geração `n` (0 = o atual).
    pub fn path(&self, generation: usize) -> PathBuf {
        self.log.path(generation)
    }

    pub fn set_limits(&self, limits: Limits) {
        self.log.set_limits(limits);
    }

    pub fn set_summarizer(&self, summarizer: Summarizer) {
        *locked(&self.summarizer) = summarizer;
    }

    /// Acrescenta uma linha, com o número de sequência dela ([`ConversationMessage::seq`]). Devolve o
    /// que a rotação fez fora do comum, para o registro.
    pub fn append(&self, message: &ConversationMessage) -> Result<Vec<RotationNote>, JsonlError> {
        let mut seq = locked(&self.seq);
        let mut line = message.clone();
        line.seq = self.next_seq(&mut seq, message);
        let summarizer = *locked(&self.summarizer);
        let mut notes = Vec::new();
        self.log.append_with(&line, |text, forced| self.release(text, forced, summarizer, &mut notes))?;
        drop(seq);
        if let Some(audio) = &message.audio {
            if let Some(index) = locked(&self.audio).as_mut() {
                index.ids.insert(audio.id.clone());
            }
        }
        Ok(notes)
    }

    /// O número da linha: o da primeira linha do mesmo id, ou o próximo. Na primeira vez, o próximo
    /// sai do maior número gravado e da marca do resumo, e as linhas de antes da T05 ganham número,
    /// na ordem do arquivo ([`Self::number_old_lines`]). Com uma geração ilegível, o maior número
    /// gravado não é conhecido (spec 140 T06, revisão 1, F-01): a linha fica sem número (nunca
    /// coberta, então nunca sai sem resumo).
    ///
    /// Spec 140 T17: a nova tentativa não relê a conversa a cada gravação (até cerca de 33 MiB com as
    /// gerações seguradas). Ela espera [`SEQ_RETRY`], ou que alguma linha saia do arquivo (uma rotação
    /// que tirou a geração, ou a limpeza de projeto); até lá, a linha segue sem número, como antes.
    fn next_seq(&self, state: &mut SeqState, message: &ConversationMessage) -> Option<u64> {
        if state.next == 0 {
            let removals = self.log.removals();
            let waiting = state.failed.is_some_and(|(at, seen)| seen == removals && at.elapsed() < state.retry);
            if waiting {
                return None;
            }
            state.attempts = state.attempts.saturating_add(1);
            if !self.number_old_lines(state) {
                state.failed = Some((Instant::now(), removals));
                return None;
            }
            state.failed = None;
        }
        Some(self.known_or_next(state, message))
    }

    fn known_or_next(&self, state: &mut SeqState, message: &ConversationMessage) -> u64 {
        if let Some(known) = state.recent.iter().rev().find(|(id, _)| *id == message.id).map(|(_, s)| *s) {
            return known;
        }
        // A mensagem lida da conversa e gravada de novo (o mesmo id) já traz o número dela.
        if let Some(seq) = message.seq.filter(|s| *s < state.next) {
            return seq;
        }
        let seq = state.next;
        state.next = state.next.saturating_add(1);
        state.recent.push_back((message.id.clone(), seq));
        if state.recent.len() > RECENT_IDS {
            state.recent.pop_front();
        }
        seq
    }

    /// Dá número às linhas que não têm (as de antes da T05, ou as gravadas por uma versão anterior
    /// depois de voltar a ela), depois de todos os já dados: elas nunca nascem cobertas por uma
    /// marca. A segunda linha de um id leva o número da primeira. Se uma geração não puder ser
    /// regravada, as linhas dela seguem sem número, e sem número nunca são cobertas. Com uma geração
    /// ilegível, nada é numerado e devolve `false`: os números dela não entram no maior conhecido.
    fn number_old_lines(&self, state: &mut SeqState) -> bool {
        let Ok(lines) = self.log.all_strict::<ConversationMessage>() else { return false };
        let mark = self.summary.load().summary().covered_through_seq.unwrap_or(0);
        let top = lines.iter().filter_map(|m| m.seq).max().unwrap_or(0);
        let mut next = top.max(mark).saturating_add(1);
        let mut by_id: BTreeMap<String, u64> = BTreeMap::new();
        for m in &lines {
            if let Some(s) = m.seq {
                by_id.entry(m.id.clone()).or_insert(s);
            }
        }
        let _ = self.log.map_all::<ConversationMessage>(|m| {
            if m.seq.is_some() {
                return false;
            }
            let seq = *by_id.entry(m.id.clone()).or_insert_with(|| {
                let s = next;
                next = next.saturating_add(1);
                s
            });
            m.seq = Some(seq);
            true
        });
        for m in lines.iter().rev().take(RECENT_IDS).rev() {
            if let Some(s) = by_id.get(&m.id) {
                if !state.recent.iter().any(|(id, _)| *id == m.id) {
                    state.recent.push_back((m.id.clone(), *s));
                }
            }
        }
        state.next = next;
        true
    }

    /// A geração mais velha (`text`) pode sair? Sim, se o resumo cobre todas as linhas dela; se não,
    /// só com o resumo desligado (ou no limite fixo), deixando a linha do servidor. Ilegível
    /// (`None`), ela fica: só o limite fixo a tira.
    fn release(&self, text: Option<&str>, forced: bool, summarizer: Summarizer, notes: &mut Vec<RotationNote>) -> Release {
        let Some(text) = text else {
            notes.push(RotationNote::Unreadable);
            return Release::Hold;
        };
        let summary = self.summary.load().summary();
        let pending: Vec<ConversationMessage> =
            text.lines().filter_map(|l| serde_json::from_str::<ConversationMessage>(l).ok()).filter(|m| !summary.covers(m)).collect();
        if pending.is_empty() {
            return Release::Drop;
        }
        let reason = match (forced, summarizer) {
            (true, _) => "ceiling",
            (false, Summarizer::Off) => "summary_off",
            (false, Summarizer::Active) => {
                notes.push(RotationNote::Held);
                return Release::Hold;
            }
        };
        let Some(item) = SummaryItem::server(super::new_id(), &pending, reason) else { return Release::Drop };
        let messages = item.messages as usize;
        match self.summary.update(|s| {
            add_server_item(s, item);
            true
        }) {
            Ok(_) => {
                notes.push(RotationNote::Unsummarized { messages, reason });
                Release::Drop
            }
            Err(e) => {
                notes.push(RotationNote::SummaryFailed(e.code()));
                Release::Hold
            }
        }
    }

    /// Todas as linhas, ou erro se alguma geração existe e não dá para ler ([`JsonlLog::all_strict`]).
    pub fn all_strict(&self) -> Result<Vec<ConversationMessage>, JsonlError> {
        self.log.all_strict()
    }

    /// As últimas `limit` linhas, da mais velha para a mais nova.
    pub fn tail(&self, limit: usize) -> Vec<ConversationMessage> {
        self.log.tail(limit)
    }

    /// Mantém só as linhas que passam em `keep`, em todas as gerações.
    pub fn retain(&self, keep: impl Fn(&ConversationMessage) -> bool) -> Result<(), JsonlError> {
        self.log.retain::<ConversationMessage>(keep)
    }

    /// Alguma linha ainda na conversa cita o áudio `id`. Lê a conversa só quando alguma linha saiu
    /// desde a última montagem do índice.
    pub fn cites_audio(&self, id: &str) -> bool {
        self.with_audio_index(|ids| ids.contains(id))
    }

    /// Os áudios citados pela conversa.
    pub fn audio_ids(&self) -> BTreeSet<String> {
        self.with_audio_index(Clone::clone)
    }

    fn with_audio_index<R>(&self, f: impl FnOnce(&BTreeSet<String>) -> R) -> R {
        let mut index = locked(&self.audio);
        let removals = self.log.removals();
        if let Some(current) = index.as_ref().filter(|i| i.removals == removals) {
            return f(&current.ids);
        }
        let ids: BTreeSet<String> = self.log.tail::<ConversationMessage>(usize::MAX).into_iter().filter_map(|m| m.audio.map(|a| a.id)).collect();
        self.index_builds.fetch_add(1, Ordering::Relaxed);
        let out = f(&ids);
        *index = Some(AudioIndex { removals, ids });
        out
    }

    /// Quantas vezes o índice dos áudios leu a conversa.
    pub fn index_builds(&self) -> u64 {
        self.index_builds.load(Ordering::Relaxed)
    }
}

/// Junta a linha do servidor nova à anterior, quando a última do resumo também é do servidor pelo
/// mesmo motivo: o resumo não cresce uma linha por rotação.
pub(super) fn add_server_item(summary: &mut Summary, item: SummaryItem) {
    if let Some(last) = summary.items.last_mut().filter(|l| l.source == SummarySource::Server && l.reason == item.reason) {
        last.from_ms = last.from_ms.min(item.from_ms);
        last.to_ms = last.to_ms.max(item.to_ms);
        last.messages = last.messages.saturating_add(item.messages);
        for id in item.project_ids {
            if !last.project_ids.contains(&id) {
                last.project_ids.push(id);
            }
        }
        for source in item.taint.sources {
            last.taint.add(source.kind, source.project_id.as_deref());
        }
        return;
    }
    summary.items.push(item);
}

/// Arquivos do Nanty numa pasta (`<pasta de dados>/nanty`).
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
    /// A conversa: uma única instância, com o mutex do arquivo.
    conversation: Conversation,
    /// O resumo da conversa (spec 140 T05), em arquivo próprio.
    summary: SummaryStore,
    /// A guarda em disco da configuração (`[nanty.storage]`), aplicada pelo laço.
    storage: Arc<Mutex<NantyStorage>>,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        let summary = SummaryStore::new(dir.clone());
        let conversation = Conversation::new(dir.clone(), summary.clone());
        Self { dir, conversation, summary, storage: Arc::new(Mutex::new(NantyStorage::default())) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Aplica a guarda em disco da configuração: a rotação e os tetos dos áudios.
    pub fn apply_storage(&self, storage: &NantyStorage) {
        let mut current = locked(&self.storage);
        if *current != *storage {
            self.conversation.set_limits(conversation_limits(storage));
            *current = storage.clone();
        }
    }

    /// A guarda em disco em vigor.
    pub fn storage(&self) -> NantyStorage {
        locked(&self.storage).clone()
    }

    /// O resumo da conversa.
    pub fn summary(&self) -> &SummaryStore {
        &self.summary
    }

    /// Lê o estado; ausente ou inválido = vazio. Normaliza o que versões anteriores gravaram
    /// ([`NantyState::normalize_legacy_facts`]).
    pub fn load(&self) -> NantyState {
        let mut state: NantyState = read_private(&self.dir.join(STATE_FILE), MAX_STATE_BYTES)
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        state.normalize_legacy_facts();
        state
    }

    /// Grava o estado de forma atômica.
    pub fn save(&self, state: &NantyState) -> Result<(), JsonlError> {
        let body = serde_json::to_vec_pretty(state).map_err(|_| JsonlError)?;
        write_atomic(&self.dir.join(STATE_FILE), &body)
    }

    /// A conversa (JSONL com rotação).
    pub fn conversation(&self) -> &Conversation {
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
        st.spend = Spend { day: "2026-10-30".into(), usd: 0.12, input_tokens: 10, output_tokens: 5, unpriced_turns: 0, voice_calls: 0, ..Spend::default() };
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
            relayed: None,
            project_ids: Vec::new(),
            taint: None,
            seq: None,
        };
        log.append(&msg).expect("append");
        let back: Vec<ConversationMessage> = log.tail(CONVERSATION_TAIL);
        assert_eq!(back, [ConversationMessage { seq: Some(1), ..msg }], "a linha ganha o número de sequência");
        let _ = std::fs::remove_dir_all(&d);
    }
}
