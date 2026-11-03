//! Memória de longo prazo do Nanty (spec 140 T07, G4, C-04 e C-21 a C-24).
//!
//! O que fica de uma conversa para a outra, por tema: como a pessoa e o Nanty trabalham juntos, como
//! operar o sistema, os runbooks, as decisões e a identidade que ele vai ganhando com ela. Sobrevive a
//! reinícios e atualizações: um arquivo JSON versionado por tema, na pasta do Nanty ([`store`]).
//!
//! - **Entrada com procedência (C-04):** cada [`Entry`] guarda de onde veio ([`Source`]): o que a
//!   pessoa disse ou editou na aba (`person_said`), o que o modelo gravou num turno limpo
//!   (`model_clean`), o que ela confirmou no cartão (`person_confirmed`) e o importado (`imported`, que
//!   ainda não existe e nasce não confiável).
//! - **Índice como dado (C-04):** depois do sistema e antes do resumo, rotulado como notas
//!   (`memory_notes`, [`index`]), só com as entradas ativas. A identidade é o único tema com o texto no
//!   índice, num teto menor; os outros vão só com o título, e o texto vem pela ferramenta `recall`.
//!   Nada do índice entra no prompt de sistema, e nenhuma nota muda uma regra.
//! - **Teto próprio do índice:** `[nanty.memory] index_max_bytes` (provisório: 2 KB, cerca de 500
//!   tokens pela estimativa do projeto), fora do teto de tokens do histórico. O índice só muda quando
//!   a memória muda, e a ordem dele é a dos temas e da criação: o começo do contexto continua estável
//!   para o cache do provedor, e o histórico não perde mensagens por causa dele.
//! - **Sem uso por muito tempo** (`stale_days`), a entrada sai do índice e continua na aba e no
//!   `recall`, que a traz de volta.
//! - **Turno contaminado (C-24):** gravar e apagar viram o cartão de confirmação ([`card`]), sem som,
//!   um por turno, reunindo os pendentes; o clique grava como `person_confirmed`.
//! - **Memória nunca vira ação, permissão, canal ou token:** o módulo só lê e grava as notas. As
//!   ações, o filtro e o silêncio não dependem dele (há teste estrutural), e o resultado das
//!   ferramentas daqui não muda configuração, cartão de ação nem destinatário.
//! - **Isolamento (C-16):** a entrada implementa [`Scoped`]: a limpeza de projeto e a varredura
//!   completa a tiram, e o contexto do modelo só vê a de projeto observado ([`visible`]).
//! - **Resumo nunca alimenta `remember` (C-03):** o texto que já está nas notas do resumo é recusado
//!   ([`from_summary`]).

pub mod api;
pub mod card;
pub mod ops;
pub mod store;
pub mod tools;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::journal::{JournalEntry, JournalKind};
use super::provenance::{Scoped, Taint, TaintKind};
use super::store::Channel;
use super::Nanty;
use crate::config::{Config, NantyMemory};

use self::store::{MemoryError, Snapshot};

/// Pasta da memória dentro da pasta do Nanty.
pub const MEMORY_DIR: &str = "memory";
/// Versão do formato gravado por este código.
pub const MEMORY_VERSION: u32 = 1;
/// Teto do título de uma entrada, em caracteres.
pub const TITLE_MAX_CHARS: usize = 80;
/// Entradas devolvidas pelo `recall`, no máximo.
pub const RECALL_MAX: usize = 5;
/// Teto da consulta do `recall`, em caracteres.
pub const QUERY_MAX_CHARS: usize = 200;

/// O tema de uma entrada: um arquivo por tema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Quem o Nanty é com esta pessoa: o jeito de falar e de ajudar que ela pediu.
    Identity,
    /// Como os dois trabalham juntos: preferências e combinados.
    WorkingTogether,
    /// Como operar o sistema e o que se sabe dele.
    System,
    /// Passos para uma tarefa que se repete.
    Runbooks,
    /// Decisões tomadas, com o porquê.
    Decisions,
}

impl Topic {
    /// Na ordem do índice e da aba.
    pub const ALL: [Topic; 5] = [Topic::Identity, Topic::WorkingTogether, Topic::System, Topic::Runbooks, Topic::Decisions];

    pub fn as_str(self) -> &'static str {
        match self {
            Topic::Identity => "identity",
            Topic::WorkingTogether => "working_together",
            Topic::System => "system",
            Topic::Runbooks => "runbooks",
            Topic::Decisions => "decisions",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == raw.trim())
    }

    /// Teto do texto no tema: o runbook tem o dele, o resto é fato.
    pub fn max_chars(self, cfg: &NantyMemory) -> usize {
        match self {
            Topic::Runbooks => cfg.runbook_chars(),
            _ => cfg.fact_chars(),
        }
    }
}

/// De onde veio a entrada (C-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A pessoa escreveu (a edição na aba).
    PersonSaid,
    /// O modelo gravou num turno limpo.
    ModelClean,
    /// A pessoa confirmou no cartão do turno contaminado.
    PersonConfirmed,
    /// Veio de fora (a importação, que fica para depois): não confiável.
    Imported,
    /// Gravado por uma versão mais nova: tratado como importado.
    #[serde(other)]
    Unknown,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::PersonSaid => "person_said",
            SourceKind::ModelClean => "model_clean",
            SourceKind::PersonConfirmed => "person_confirmed",
            SourceKind::Imported => "imported",
            SourceKind::Unknown => "unknown",
        }
    }

    /// Escrito pela pessoa ou num turno limpo.
    pub fn trusted(self) -> bool {
        matches!(self, SourceKind::PersonSaid | SourceKind::ModelClean | SourceKind::PersonConfirmed)
    }
}

/// A procedência de uma entrada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    /// A mensagem da pessoa que deu origem à entrada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    /// O projeto da entrada; sem ele, é geral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub at_ms: u64,
}

/// Situação de uma entrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    Active,
    /// Apagada: ainda volta por um tempo (`deleted_keep_days`) e depois sai do arquivo.
    Deleted,
    /// Gravado por uma versão mais nova: fica fora do índice.
    #[serde(other)]
    Unknown,
}

/// Uma entrada da memória.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub topic: Topic,
    pub title: String,
    pub text: String,
    pub source: Source,
    pub created: u64,
    /// Também a versão da edição pela aba: a gravação só vale sobre o `updated` que ela leu.
    pub updated: u64,
    pub last_used: u64,
    pub state: EntryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<u64>,
}

impl Scoped for Entry {
    fn projects(&self) -> Vec<&str> {
        self.source.project_id.as_deref().into_iter().collect()
    }
}

impl Entry {
    pub fn active(&self) -> bool {
        self.state == EntryState::Active
    }

    /// Sem uso nem mudança por mais que o teto: fora do índice.
    pub fn stale(&self, now_ms: u64, cfg: &NantyMemory) -> bool {
        now_ms.saturating_sub(self.last_used.max(self.updated)) > cfg.stale_ms()
    }

    fn recency(&self) -> u64 {
        self.last_used.max(self.updated)
    }
}

/// O conteúdo de um arquivo de tema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFile {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

/// A entrada pode ir ao modelo: ativa e, se tem projeto, de um projeto cadastrado e observado.
pub fn visible(cfg: &Config, entry: &Entry) -> bool {
    entry.active() && entry.within(|id| cfg.projects.iter().any(|p| p.id == id) && cfg.nanty.observes(id))
}

/// Texto sem controle (quebras de linha ficam) e sem espaço nas pontas.
pub fn tidy(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || *c == '\n').collect::<String>().trim().to_string()
}

// ---- O índice ----

/// O índice como vai ao contexto e o que ele traz de não confiável.
#[derive(Debug, Clone, PartialEq)]
pub struct Index {
    pub block: String,
    pub taint: Taint,
}

fn index_item(entry: &Entry, with_text: bool) -> Value {
    let title = if entry.source.kind.trusted() { json!(entry.title) } else { json!(super::agent::envelope(&entry.title)) };
    let mut item = json!({ "id": entry.id, "title": title });
    if with_text {
        item["text"] = if entry.source.kind.trusted() { json!(entry.text) } else { json!(super::agent::envelope(&entry.text)) };
    }
    if let Some(p) = &entry.source.project_id {
        item["project_id"] = json!(p);
    }
    item
}

fn index_json(chosen: &[(&Entry, bool)]) -> String {
    let mut sorted: Vec<&(&Entry, bool)> = chosen.iter().collect();
    sorted.sort_by(|a, b| (a.0.topic, a.0.created, &a.0.id).cmp(&(b.0.topic, b.0.created, &b.0.id)));
    let mut notes = Map::new();
    for topic in Topic::ALL {
        let items: Vec<Value> = sorted.iter().filter(|(e, _)| e.topic == topic).map(|(e, text)| index_item(e, *text)).collect();
        if !items.is_empty() {
            notes.insert(topic.as_str().to_string(), Value::Array(items));
        }
    }
    super::tools::seal(&json!({ "memory_notes": notes }).to_string())
}

/// O índice das entradas que cabem no teto: a identidade (com o texto) no teto dela, e as outras (só o
/// título) no resto, as usadas mais recentemente primeiro. A ordem do bloco é a dos temas e da
/// criação, para ele só mudar quando a memória muda. `None` sem entrada.
pub fn index(cfg: &Config, snapshot: &Snapshot, now_ms: u64) -> Option<Index> {
    let memory = &cfg.nanty.memory;
    let mut candidates: Vec<&Entry> = snapshot.entries().filter(|e| visible(cfg, e) && !e.stale(now_ms, memory)).collect();
    candidates.sort_by(|a, b| b.recency().cmp(&a.recency()).then_with(|| a.id.cmp(&b.id)));
    let mut chosen: Vec<(&Entry, bool)> = Vec::new();
    let mut identity_bytes = 0usize;
    for entry in candidates.iter().filter(|e| e.topic == Topic::Identity) {
        let size = index_item(entry, true).to_string().len();
        if identity_bytes + size > memory.identity_bytes() {
            continue;
        }
        chosen.push((entry, true));
        if index_json(&chosen).len() > memory.index_bytes() {
            chosen.pop();
            continue;
        }
        identity_bytes += size;
    }
    for entry in candidates.iter().filter(|e| e.topic != Topic::Identity) {
        chosen.push((entry, false));
        if index_json(&chosen).len() > memory.index_bytes() {
            chosen.pop();
        }
    }
    if chosen.is_empty() {
        return None;
    }
    let mut taint = Taint::default();
    for (entry, _) in &chosen {
        if !entry.source.kind.trusted() {
            taint.add(TaintKind::ImportedMemory, entry.source.project_id.as_deref());
        }
    }
    Some(Index { block: index_json(&chosen), taint })
}

// ---- A busca ----

fn terms(text: &str) -> Vec<String> {
    crate::kb::index::tokens(text).into_iter().map(|(_, t)| t).collect()
}

/// As entradas ativas e visíveis que mais casam com a consulta, pelo BM25 da base de conhecimento
/// (a mesma tokenização e os mesmos parâmetros, sem o armazenamento dela). O título conta em dobro.
pub fn recall<'a>(cfg: &Config, snapshot: &'a Snapshot, query: &str, topic: Option<Topic>, max: usize) -> Vec<&'a Entry> {
    let mut wanted = terms(query);
    wanted.sort();
    wanted.dedup();
    let docs: Vec<(&Entry, Vec<String>)> = snapshot
        .entries()
        .filter(|e| visible(cfg, e) && topic.is_none_or(|t| e.topic == t))
        .map(|e| {
            let title = terms(&e.title);
            let mut all = title.clone();
            all.extend(title);
            all.extend(terms(&e.text));
            (e, all)
        })
        .collect();
    if wanted.is_empty() || docs.is_empty() {
        return Vec::new();
    }
    let n = docs.len() as f32;
    let avg = docs.iter().map(|(_, t)| t.len() as f32).sum::<f32>() / n;
    let avg = avg.max(1.0);
    let (k1, b) = (crate::kb::index::K1, crate::kb::index::B);
    let mut scored: Vec<(&Entry, f32)> = docs
        .iter()
        .map(|(entry, words)| {
            let len = words.len() as f32;
            let score = wanted
                .iter()
                .map(|term| {
                    let f = words.iter().filter(|w| *w == term).count() as f32;
                    if f == 0.0 {
                        return 0.0;
                    }
                    let df = docs.iter().filter(|(_, w)| w.contains(term)).count() as f32;
                    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                    idf * f * (k1 + 1.0) / (f + k1 * (1.0 - b + b * len / avg))
                })
                .sum::<f32>();
            (*entry, score)
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.recency().cmp(&a.0.recency())));
    scored.into_iter().take(max.max(1)).map(|(e, _)| e).collect()
}

// ---- O resumo nunca alimenta a memória ----

/// Termos seguidos do texto que, achados juntos numa nota do resumo, mostram que o texto veio dela.
const SUMMARY_RUN: usize = 8;
/// Texto mais curto que isto (em termos) não é comparado: uma frase curta coincide por acaso.
const SUMMARY_MIN: usize = 4;

/// O texto repete uma nota do resumo da conversa: um trecho de [`SUMMARY_RUN`] termos seguidos (ou o
/// texto inteiro, se for mais curto) aparece igual numa das notas e não está na mensagem atual da
/// pessoa (`said`). O que ela repete agora é dela, mesmo que o resumo já diga.
pub fn from_summary(text: &str, notes: &[&str], said: &str) -> bool {
    let words = terms(text);
    if words.len() < SUMMARY_MIN {
        return false;
    }
    let run = words.len().min(SUMMARY_RUN);
    let spoken = terms(said);
    let in_note = |w: &[String]| notes.iter().any(|note| terms(note).windows(run).any(|h| h == w));
    words.windows(run).any(|w| in_note(w) && !spoken.windows(run).any(|h| h == w))
}

// ---- O que um turno fez na memória ----

/// Uma entrada gravada num turno limpo, com o cartão do botão de apagar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub id: String,
    pub title: String,
    pub card: Option<String>,
}

/// O que as ferramentas da memória fizeram na mensagem.
#[derive(Debug, Clone, Default)]
pub struct Effects {
    pub saved: Vec<Saved>,
    /// Títulos das entradas apagadas.
    pub forgotten: Vec<String>,
    /// O que o turno contaminado propôs: vira o cartão de confirmação no fim do turno.
    pub proposals: Vec<card::Proposal>,
    /// Os cartões da memória citados pela resposta: o de confirmação e os de apagar.
    pub cards: Vec<String>,
    /// Gravações e apagamentos pedidos na mensagem (o teto [`tools::MAX_WRITES`]).
    pub writes: usize,
}

impl Effects {
    pub fn is_empty(&self) -> bool {
        self.saved.is_empty() && self.forgotten.is_empty() && self.proposals.is_empty() && self.cards.is_empty()
    }

    /// As frases do servidor da resposta, `(código, parâmetros)`: "guardei isto", "apaguei isto" e o
    /// pedido de confirmação no cartão.
    pub fn lines(&self) -> Vec<(&'static str, Map<String, Value>)> {
        let title = |t: &str| {
            let mut p = Map::new();
            p.insert("title".into(), json!(t));
            p
        };
        let mut out = Vec::new();
        for saved in &self.saved {
            out.push(("nanty.chat.memory_saved", title(&saved.title)));
        }
        for gone in &self.forgotten {
            out.push(("nanty.chat.memory_forgotten", title(gone)));
        }
        if !self.proposals.is_empty() {
            out.push(("nanty.chat.memory_proposed", Map::new()));
        }
        out
    }
}

// ---- A memória do Nanty ----

impl Nanty {
    /// O que está gravado agora (vazio sem pasta de dados).
    pub fn memory(&self) -> Snapshot {
        self.memory.as_ref().map(store::MemoryStore::load).unwrap_or_default()
    }

    /// Muda a memória com `f` (só o dono grava). Antes, as entradas apagadas há mais que o teto saem
    /// do arquivo. A recusa por arquivo inválido ou de versão mais nova vai ao registro, com o tema.
    pub fn memory_write<T>(&self, cfg: &NantyMemory, f: impl FnOnce(&mut Snapshot) -> T) -> Result<T, MemoryError> {
        self.memory_change(Some(cfg.deleted_keep_ms()), f)
    }

    /// Como [`Self::memory_write`]; sem `keep`, as apagadas ficam como estão (a limpeza de projeto, que
    /// não tem a configuração à mão).
    fn memory_change<T>(&self, keep: Option<u64>, f: impl FnOnce(&mut Snapshot) -> T) -> Result<T, MemoryError> {
        if !self.owns() {
            return Err(MemoryError::NotOwner);
        }
        let Some(store) = &self.memory else { return Err(MemoryError::NotOwner) };
        let now = self.now().epoch_ms;
        let result = store.transact(|snap| {
            let out = f(snap);
            if let Some(keep) = keep {
                snap.purge_deleted(now, keep);
            }
            out
        });
        match result {
            Ok((out, changed)) => {
                if changed {
                    self.changed(super::Part::Conversation);
                }
                Ok(out)
            }
            Err(e) => {
                let mut entry = JournalEntry::new(now, JournalKind::Conversation, "memory.refused");
                entry.params.insert("reason".into(), json!(e.code()));
                if let Some(topic) = e.topic() {
                    entry.params.insert("topic".into(), json!(topic.as_str()));
                }
                self.record(&entry);
                Err(e)
            }
        }
    }

    /// Tira as entradas do projeto, do arquivo e do `.bak`. Sem pasta ou sem ser o dono, nada.
    pub(super) fn forget_project_memory(&self, project_id: &str) {
        self.retain_memory(|e| !e.touches(project_id));
    }

    /// Mantém só as entradas de projetos que ainda existem.
    pub(super) fn retain_known_memory(&self, known: &[String]) {
        self.retain_memory(|e| e.within(|id| known.iter().any(|k| k == id)));
    }

    /// A limpeza tema a tema (revisão 1 da T07, F-03): cada tema gravável fica só com o que passa em
    /// `keep`; o tema que não pode ser gravado (inválido ou de versão mais nova) fica intocado e vai ao
    /// registro, sem segurar os outros. O `.bak` também.
    fn retain_memory(&self, keep: impl Fn(&Entry) -> bool + Copy) {
        let Some(store) = self.memory.as_ref().filter(|_| self.owns()) else { return };
        let errors = store.retain_writable(keep);
        let now = self.now().epoch_ms;
        for e in errors {
            let mut entry = JournalEntry::new(now, JournalKind::Conversation, "memory.refused");
            entry.params.insert("reason".into(), json!(e.code()));
            if let Some(topic) = e.topic() {
                entry.params.insert("topic".into(), json!(topic.as_str()));
            }
            self.record(&entry);
        }
        self.scrub_memory_backups(keep);
        self.changed(super::Part::Conversation);
    }

    /// O `.bak` guarda a versão anterior: o que saiu por projeto sai dele também.
    fn scrub_memory_backups(&self, keep: impl Fn(&Entry) -> bool) {
        if let Some(store) = self.memory.as_ref().filter(|_| self.owns()) {
            store.scrub_backups(keep);
        }
    }

    /// O índice para o contexto da conversa.
    pub fn memory_index(&self, cfg: &Config) -> Option<Index> {
        index(cfg, &self.memory(), self.now().epoch_ms)
    }
}

#[cfg(test)]
mod tests;
